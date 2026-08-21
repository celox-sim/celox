//! SystemVerilog lowering adapter for the shared Celox frontend.
//!
//! SystemVerilog syntax and semantic analysis belongs in the
//! `celox-sv-analyzer` crate. This module converts analyzed SV into the shared
//! symbolic assembly model. It intentionally lives beside that assembly rather
//! than in the public `celox` facade or in a misleading frontend-to-frontend
//! dependency.

use std::path::{Path, PathBuf};

use celox_design::{
    BinaryOp, BitAccess, DomainKind, InitialStateData, InitialStateValue, ModuleId, PortTypeKind,
    RegionedVarAddrBase, RuntimeErrorInfo, STABLE_REGION, TriggerSet, UnaryOp, VarAtomBase,
    WORKING_REGION,
};
use celox_frontend_core::symbolic::artifact::{
    ExternalHierarchy, ExternalModule, SimModule, SymbolicGlueAddr as GlueAddr, SymbolicRtl,
    SymbolicVariable,
};
use celox_frontend_core::{
    FrontendTrace, FrontendTraceOptions, LoweringPhase, ParserError, ScheduledRtlOutput,
    SourceLocation, SourceVarId, VariableKind, symbolic::width::coerce_node_width,
};
use celox_sir::{
    BlockId, ExecutionUnit, RegisterType, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator,
    SIRValue, merge_sir_eus,
};
use celox_slt::{
    CombObserver, GlueBlockBase, LogicPath, LogicPathTarget, NodeId, SLTIndex, SLTIndexKind,
    SLTNode, SLTNodeArena,
};
use celox_sv_analyzer as sv;
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use num_bigint::BigUint;

type RegionedVarAddr = RegionedVarAddrBase<SourceVarId>;
type GlueBlock = GlueBlockBase<SourceVarId>;
const MAX_SV_SPECIALIZATIONS_PER_MODULE: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum FrontendError {
    #[error(transparent)]
    Analyzer(#[from] sv::AnalyzerError),
    #[error(transparent)]
    Lowering(#[from] ParserError),
}

#[derive(Clone)]
struct SvVariable {
    path: Vec<String>,
    width: usize,
    signed: bool,
    is_4state: bool,
    is_net: bool,
    packed_ranges: Vec<(i128, i128)>,
    array_dims: Vec<usize>,
    domain_kind: DomainKind,
    kind: VariableKind,
    type_kind: PortTypeKind,
    source: Option<SourceLocation>,
}

impl SvVariable {
    fn to_symbolic_variable(&self) -> SymbolicVariable {
        SymbolicVariable {
            path: self.path.clone(),
            kind: self.kind,
            signed: self.signed,
            metadata: celox_design::VariableMetadata {
                width: self.width,
                is_4state: self.is_4state,
                kind: self.domain_kind,
                type_kind: self.type_kind,
                array_dims: self.array_dims.clone(),
            },
            packed_dims: self
                .packed_ranges
                .iter()
                .map(|(left, right)| left.abs_diff(*right) as usize + 1)
                .collect(),
            source: self.source.clone(),
            module_affiliated: true,
        }
    }
}

#[derive(Clone)]
pub(crate) struct LoweredSvModule {
    source: sv::ir::Module,
    implicit_nets_allowed: bool,
    pub sim_module: SimModule,
    variables: HashMap<SourceVarId, SvVariable>,
    pub port_order: Vec<SourceVarId>,
    pub signal_names: HashMap<String, SourceVarId>,
    constants: HashMap<String, i128>,
    parameter_types: HashMap<String, (usize, bool)>,
    pub instances: Vec<LoweredSvInstance>,
}

#[derive(Clone)]
struct AnalyzedSvModule {
    name: String,
    source_code: String,
    source_path: PathBuf,
    implicit_nets_allowed: bool,
}

#[derive(Clone)]
pub(crate) struct LoweredSvInstance {
    pub module_name: String,
    pub instance_name: String,
    pub parameter_overrides: Vec<LoweredSvParameterOverride>,
    pub port_connections: Vec<LoweredSvPortConnection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LoweredSvParameterOverride {
    pub name: String,
    pub value: Option<sv::ir::ConstExpr>,
}

#[derive(Clone)]
pub(crate) struct LoweredSvPortConnection {
    pub formal: String,
    pub actual: String,
    pub actual_expr: Option<sv::ir::Expr>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LoweredSvModuleKey {
    pub name: String,
    pub parameter_overrides: Vec<LoweredSvParameterOverride>,
}

impl LoweredSvModuleKey {
    pub fn base(name: String) -> Self {
        Self {
            name,
            parameter_overrides: Vec::new(),
        }
    }

    pub fn instance_key(instance: &LoweredSvInstance) -> Self {
        let mut parameter_overrides = instance.parameter_overrides.clone();
        parameter_overrides.sort_by(|left, right| left.name.cmp(&right.name));
        Self {
            name: instance.module_name.clone(),
            parameter_overrides,
        }
    }
}

fn analyze_sources(
    sources: &[(&str, &Path)],
) -> Result<HashMap<String, AnalyzedSvModule>, sv::AnalyzerError> {
    let mut modules = HashMap::default();
    for (code, path) in sources {
        let implicit_net_permissions = sv::source_module_implicit_net_permissions(code, path)?;
        for module_name in sv::source_module_names(code, path)? {
            let name = module_name.clone();
            if modules.contains_key(&name) {
                return Err(sv::AnalyzerError::DuplicateModule { name: module_name });
            }
            modules.insert(
                name,
                AnalyzedSvModule {
                    implicit_nets_allowed: implicit_net_permissions
                        .iter()
                        .find_map(|(name, allowed)| (name == &module_name).then_some(*allowed))
                        .unwrap_or(true),
                    name: module_name,
                    source_code: (*code).to_string(),
                    source_path: (*path).to_path_buf(),
                },
            );
        }
    }
    Ok(modules)
}

fn validate_specialized_instance_net_drivers(
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    modules: &HashMap<ModuleId, LoweredSvModule>,
) -> Result<(), sv::AnalyzerError> {
    for module in modules.values() {
        for port in module
            .source
            .ports()
            .iter()
            .filter(|port| port.direction() == sv::ir::PortDirection::Input)
        {
            if !child_output_driver_ranges(module, port.name(), module_ids, modules).is_empty() {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "write to input port `{}`",
                    port.name()
                )));
            }
        }

        let net_names = module
            .source
            .signals()
            .iter()
            .filter(|signal| signal.is_net())
            .map(|signal| (signal.name(), true))
            .chain(
                module
                    .source
                    .ports()
                    .iter()
                    .filter(|port| port.is_net())
                    .map(|port| (port.name(), false)),
            );
        for (signal_name, require_driver) in net_names {
            let child_driver_ranges =
                child_output_driver_ranges(module, signal_name, module_ids, modules);
            validate_net_driver_ranges(module, signal_name, &child_driver_ranges, require_driver)?;
        }

        let variable_names = module
            .source
            .signals()
            .iter()
            .filter(|signal| !signal.is_net())
            .map(|signal| signal.name())
            .chain(
                module
                    .source
                    .ports()
                    .iter()
                    .filter(|port| !port.is_net())
                    .map(|port| port.name()),
            );
        for signal_name in variable_names {
            let child_driver_ranges =
                child_output_driver_ranges(module, signal_name, module_ids, modules);
            let local_drivers = local_driver_ranges(
                &module.source,
                signal_name,
                &module.constants,
                &module.parameter_types,
            );
            let child_overlaps = driver_ranges_overlap(&child_driver_ranges);
            let child_local_overlap = child_driver_ranges.iter().any(|(_, child_range)| {
                local_drivers
                    .iter()
                    .any(|(_, local_range)| net_driver_ranges_overlap(*child_range, *local_range))
            });
            if child_overlaps || child_local_overlap {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "multiple variable drivers for `{signal_name}`"
                )));
            }
        }
    }
    Ok(())
}

fn child_output_driver_ranges(
    module: &LoweredSvModule,
    signal_name: &str,
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    modules: &HashMap<ModuleId, LoweredSvModule>,
) -> Vec<(usize, Option<(i128, i128)>)> {
    let Some(signal_id) = module.signal_names.get(signal_name).copied() else {
        return Vec::new();
    };
    let mut drivers = Vec::new();
    for instance in &module.instances {
        let key = LoweredSvModuleKey::instance_key(instance);
        let Some(child_id) = module_ids.get(&key).copied() else {
            continue;
        };
        let Some(child) = modules.get(&child_id) else {
            continue;
        };
        for connection in &instance.port_connections {
            if !child.source.ports().iter().any(|port| {
                port.name() == connection.formal
                    && matches!(
                        port.direction(),
                        sv::ir::PortDirection::Output | sv::ir::PortDirection::Inout
                    )
            }) {
                continue;
            }
            let Some(actual_expr) = connection.actual_expr.as_ref() else {
                continue;
            };
            let Some((actual_id, access)) = output_lvalue_access(
                actual_expr,
                &module.variables,
                &module.signal_names,
                &module.constants,
                &module.parameter_types,
            ) else {
                if output_connection_targets_signal(actual_expr, signal_name) {
                    drivers.push((drivers.len(), None));
                }
                continue;
            };
            if actual_id == signal_id {
                drivers.push((
                    drivers.len(),
                    Some((access.lsb as i128, access.msb as i128)),
                ));
            }
        }
    }
    drivers
}

fn output_connection_targets_signal(expr: &sv::ir::Expr, signal_name: &str) -> bool {
    match expr {
        sv::ir::Expr::Ident(name) => name == signal_name,
        sv::ir::Expr::Select { expr, .. } | sv::ir::Expr::Resize { expr, .. } => {
            output_connection_targets_signal(expr, signal_name)
        }
        sv::ir::Expr::Concat(parts) => parts
            .iter()
            .any(|part| output_connection_targets_signal(part, signal_name)),
        _ => false,
    }
}

fn validate_net_driver_ranges(
    module: &LoweredSvModule,
    signal_name: &str,
    child_driver_ranges: &[(usize, Option<(i128, i128)>)],
    require_driver: bool,
) -> Result<(), sv::AnalyzerError> {
    let local_drivers = local_driver_ranges(
        &module.source,
        signal_name,
        &module.constants,
        &module.parameter_types,
    );
    let overlapping_local_drivers = local_drivers.iter().enumerate().any(|(index, left)| {
        local_drivers[index + 1..]
            .iter()
            .any(|right| left.0 != right.0 && net_driver_ranges_overlap(left.1, right.1))
    });
    let child_local_overlap = child_driver_ranges.iter().any(|(_, child_range)| {
        local_drivers
            .iter()
            .any(|(_, local_range)| net_driver_ranges_overlap(*child_range, *local_range))
    });
    if driver_ranges_overlap(child_driver_ranges)
        || child_local_overlap
        || overlapping_local_drivers
    {
        return Err(sv::AnalyzerError::Unsupported(format!(
            "multiple net drivers for `{signal_name}`"
        )));
    }
    if require_driver && child_driver_ranges.is_empty() && local_drivers.is_empty() {
        return Err(sv::AnalyzerError::Unsupported(format!(
            "undriven net declaration `{signal_name}`"
        )));
    }
    Ok(())
}

fn driver_ranges_overlap(drivers: &[(usize, Option<(i128, i128)>)]) -> bool {
    drivers.iter().enumerate().any(|(index, left)| {
        drivers[index + 1..]
            .iter()
            .any(|right| net_driver_ranges_overlap(left.1, right.1))
    })
}

fn validate_variable_driver_ranges(
    module: &sv::ir::Module,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Result<(), sv::AnalyzerError> {
    for port in module
        .ports()
        .iter()
        .filter(|port| port.direction() == sv::ir::PortDirection::Input)
    {
        if !local_driver_ranges(module, port.name(), constants, parameter_types).is_empty() {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "write to input port `{}`",
                port.name()
            )));
        }
    }

    let variable_names = module
        .signals()
        .iter()
        .filter(|signal| !signal.is_net())
        .map(|signal| signal.name())
        .chain(
            module
                .ports()
                .iter()
                .filter(|port| !port.is_net())
                .map(|port| port.name()),
        );
    for signal_name in variable_names {
        let drivers = local_driver_ranges(module, signal_name, constants, parameter_types);
        let has_overlap = drivers.iter().enumerate().any(|(index, left)| {
            drivers[index + 1..]
                .iter()
                .any(|right| left.0 != right.0 && net_driver_ranges_overlap(left.1, right.1))
        });
        if has_overlap {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "multiple variable drivers for `{signal_name}`"
            )));
        }
    }
    Ok(())
}

fn local_driver_ranges(
    module: &sv::ir::Module,
    signal_name: &str,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Vec<(usize, Option<(i128, i128)>)> {
    let mut drivers = Vec::new();
    let mut driver_id = 0;
    for process in module.comb_processes() {
        let active = process.condition().is_none_or(|condition| {
            sv::typecheck::eval_const_expr_with_types(condition, constants, parameter_types)
                .is_none_or(|value| value != 0)
        });
        if active {
            for assignment in process.assignments() {
                if assignment.lhs() == signal_name {
                    drivers.push((
                        driver_id,
                        net_lvalue_range(assignment.lhs_value(), constants, parameter_types),
                    ));
                }
                if process.kind() == sv::ir::CombProcessKind::ContinuousAssign {
                    driver_id += 1;
                }
            }
            if process.kind() == sv::ir::CombProcessKind::AlwaysComb {
                driver_id += 1;
            }
        } else {
            driver_id += 1;
        }
    }
    for process in module.ff_processes() {
        drivers.extend(
            process
                .assignments()
                .iter()
                .map(|assignment| assignment.assignment())
                .filter(|assignment| assignment.lhs() == signal_name)
                .map(|assignment| {
                    (
                        driver_id,
                        net_lvalue_range(assignment.lhs_value(), constants, parameter_types),
                    )
                }),
        );
        driver_id += 1;
    }
    drivers
}

fn net_lvalue_range(
    lvalue: &sv::ir::LValue,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<(i128, i128)> {
    let sv::ir::LValue::Select { msb, lsb, .. } = lvalue else {
        return None;
    };
    let msb = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
    let lsb = sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
    Some((msb.min(lsb), msb.max(lsb)))
}

fn net_driver_ranges_overlap(left: Option<(i128, i128)>, right: Option<(i128, i128)>) -> bool {
    match (left, right) {
        (Some((left_start, left_end)), Some((right_start, right_end))) => {
            left_start <= right_end && right_start <= left_end
        }
        _ => true,
    }
}

/// Lower the requested SystemVerilog roots and their reachable children into
/// an embeddable hierarchy. The module IDs in the returned graph are local and
/// are remapped during mixed-language symbolic hierarchy assembly.
pub fn prepare_external_hierarchy(
    sources: &[(&str, &Path)],
    root_names: &HashSet<String>,
    four_state: bool,
) -> Result<ExternalHierarchy, FrontendError> {
    let analyzed = analyze_sources(sources)?;
    let mut names = root_names
        .iter()
        .filter(|&name| analyzed.contains_key(name))
        .cloned()
        .collect::<Vec<_>>();
    names.sort();

    let mut module_ids = HashMap::default();
    let mut module_specialization_counts = HashMap::default();
    let mut queue = Vec::new();
    for name in names {
        let key = LoweredSvModuleKey::base(name.clone());
        let module_id = ModuleId(module_ids.len());
        module_ids.insert(key.clone(), module_id);
        module_specialization_counts.insert(name.clone(), 1usize);
        queue.push(key);
    }

    let mut index = 0;
    while index < queue.len() {
        let key = queue[index].clone();
        index += 1;
        let base = analyzed
            .get(&key.name)
            .ok_or_else(|| unsupported_sv_instance(key.name.clone()))?;
        let lowered = specialize_module(base, &key, four_state)?;
        for instance in &lowered.instances {
            let child_key = LoweredSvModuleKey::instance_key(instance);
            if !analyzed.contains_key(&child_key.name) {
                continue;
            }
            if !module_ids.contains_key(&child_key) {
                let specialization_count = module_specialization_counts
                    .entry(child_key.name.clone())
                    .or_insert(0);
                if *specialization_count >= MAX_SV_SPECIALIZATIONS_PER_MODULE {
                    return Err(sv_specialization_limit_error(child_key.name.clone()).into());
                }
                *specialization_count += 1;
                let child_id = ModuleId(module_ids.len());
                module_ids.insert(child_key.clone(), child_id);
                queue.push(child_key);
            }
        }
    }

    let lowered_modules = module_ids
        .iter()
        .map(|(key, &module_id)| {
            let base = analyzed
                .get(&key.name)
                .ok_or_else(|| unsupported_sv_instance(key.name.clone()))?;
            Ok((module_id, specialize_module(base, key, four_state)?))
        })
        .collect::<Result<HashMap<_, _>, FrontendError>>()?;
    validate_specialized_instance_net_drivers(&module_ids, &lowered_modules)?;
    let mut modules = HashMap::default();
    for (key, &module_id) in &module_ids {
        let lowered = &lowered_modules[&module_id];
        let mut sim_module = lowered.sim_module.clone();
        let unresolved_instances: Vec<String> = lowered
            .instances
            .iter()
            .filter_map(|instance| {
                (!module_ids.contains_key(&LoweredSvModuleKey::instance_key(instance)))
                    .then_some(instance.module_name.clone())
            })
            .collect();
        let mut resolved = lowered.clone();
        resolved.instances.retain(|instance| {
            module_ids.contains_key(&LoweredSvModuleKey::instance_key(instance))
        });
        attach_instance_glue(
            &mut sim_module,
            &resolved,
            key,
            &module_ids,
            &lowered_modules,
            four_state,
        )?;
        modules.insert(
            module_id,
            ExternalModule {
                sim_module,
                port_order: lowered.port_order.clone(),
                unresolved_instances,
            },
        );
    }
    let roots = module_ids
        .iter()
        .filter(|(key, _)| key.parameter_overrides.is_empty())
        .map(|(key, &module_id)| (key.name.clone(), module_id))
        .collect();
    Ok(ExternalHierarchy { modules, roots })
}

/// Analyze SystemVerilog sources and lower the selected top through Celox's
/// shared symbolic scheduling pipeline.
pub fn schedule_sources(
    sources: &[(&str, &Path)],
    top: &str,
    parameter_overrides: &[(String, u64)],
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    trace_options: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
) -> Result<ScheduledRtlOutput, FrontendError> {
    let analyzed = analyze_sources(sources)?;
    let top = top.to_string();
    let root_key = LoweredSvModuleKey {
        name: top.clone(),
        parameter_overrides: parameter_overrides
            .iter()
            .map(|(name, value)| LoweredSvParameterOverride {
                name: name.clone(),
                value: Some(sv::ir::ConstExpr::Literal(value.to_string())),
            })
            .collect(),
    };
    if !analyzed.contains_key(&top) {
        return Err(sv_top_not_found(top).into());
    }

    let root_id = ModuleId(0);
    let mut module_ids = HashMap::default();
    module_ids.insert(root_key.clone(), root_id);
    let mut module_specialization_counts = HashMap::default();
    module_specialization_counts.insert(root_key.name.clone(), 1usize);
    let mut queue = vec![root_key.clone()];
    let mut index = 0;
    while index < queue.len() {
        let key = queue[index].clone();
        index += 1;
        let base = analyzed
            .get(&key.name)
            .ok_or_else(|| unsupported_sv_instance(key.name.clone()))?;
        let lowered = specialize_module(base, &key, four_state)?;
        for instance in &lowered.instances {
            let child_key = LoweredSvModuleKey::instance_key(instance);
            if !analyzed.contains_key(&child_key.name) {
                return Err(unsupported_sv_instance(child_key.name.clone()).into());
            }
            if !module_ids.contains_key(&child_key) {
                let specialization_count = module_specialization_counts
                    .entry(child_key.name.clone())
                    .or_insert(0);
                if *specialization_count >= MAX_SV_SPECIALIZATIONS_PER_MODULE {
                    return Err(sv_specialization_limit_error(child_key.name.clone()).into());
                }
                *specialization_count += 1;
                let child_id = ModuleId(module_ids.len());
                module_ids.insert(child_key.clone(), child_id);
                queue.push(child_key);
            }
        }
    }

    let lowered_modules = module_ids
        .iter()
        .map(|(key, &module_id)| {
            let base = analyzed
                .get(&key.name)
                .ok_or_else(|| unsupported_sv_instance(key.name.clone()))?;
            let lowered = specialize_module(base, key, four_state).map_err(FrontendError::from)?;
            Ok((module_id, lowered))
        })
        .collect::<Result<HashMap<_, _>, FrontendError>>()?;
    validate_specialized_instance_net_drivers(&module_ids, &lowered_modules)?;
    let root = &lowered_modules[&root_id];
    if let Some(port) = root
        .port_order
        .iter()
        .map(|port_id| &root.variables[port_id])
        .find(|port| port.kind == VariableKind::Inout)
    {
        return Err(unsupported_sv_inout(port.path.join(".")).into());
    }
    validate_sv_module_graph(
        &root_key,
        &module_ids,
        &lowered_modules,
        &mut HashSet::default(),
        &mut HashSet::default(),
    )?;

    let mut modules = HashMap::default();
    let mut module_names = HashMap::default();
    for (key, &module_id) in &module_ids {
        let lowered = &lowered_modules[&module_id];
        let mut sim_module = lowered.sim_module.clone();
        attach_instance_glue(
            &mut sim_module,
            lowered,
            key,
            &module_ids,
            &lowered_modules,
            four_state,
        )?;
        module_names.insert(module_id, key.name.clone());
        modules.insert(module_id, sim_module);
    }

    let symbolic = SymbolicRtl {
        modules,
        module_names,
        root_id,
    };
    celox_frontend_core::symbolic::assembly::schedule_symbolic_rtl(
        symbolic,
        None,
        ignored_loops,
        true_loops,
        four_state,
        trace_options,
        trace,
    )
    .map_err(FrontendError::from)
}

fn sv_specialization_limit_error(name: String) -> ParserError {
    ParserError::unsupported(
        64,
        LoweringPhase::SimulatorParser,
        "systemverilog module specialization limit exceeded (possible recursive instantiation)",
        name,
        None,
    )
}

fn validate_sv_module_graph(
    key: &LoweredSvModuleKey,
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    lowered_modules: &HashMap<ModuleId, LoweredSvModule>,
    active: &mut HashSet<LoweredSvModuleKey>,
    complete: &mut HashSet<LoweredSvModuleKey>,
) -> Result<(), ParserError> {
    if complete.contains(key) {
        return Ok(());
    }
    if !active.insert(key.clone()) {
        return Err(ParserError::unsupported(
            64,
            LoweringPhase::SimulatorParser,
            "recursive systemverilog module instantiation",
            key.name.clone(),
            None,
        ));
    }
    let module_id = module_ids
        .get(key)
        .copied()
        .ok_or_else(|| unsupported_sv_instance(key.name.clone()))?;
    let module = lowered_modules
        .get(&module_id)
        .ok_or_else(|| unsupported_sv_instance(key.name.clone()))?;
    for instance in &module.instances {
        validate_sv_module_graph(
            &LoweredSvModuleKey::instance_key(instance),
            module_ids,
            lowered_modules,
            active,
            complete,
        )?;
    }
    active.remove(key);
    complete.insert(key.clone());
    Ok(())
}

fn specialize_module(
    module: &AnalyzedSvModule,
    key: &LoweredSvModuleKey,
    four_state: bool,
) -> Result<LoweredSvModule, sv::AnalyzerError> {
    let overrides = evaluated_parameter_overrides(&key.parameter_overrides)?;
    let ir = sv::analyze_source_module_with_parameter_expr_overrides(
        &module.source_code,
        &module.source_path,
        &module.name,
        &overrides,
    )?;
    let specialized = ir
        .modules()
        .iter()
        .find(|candidate| candidate.name() == module.name)
        .ok_or_else(|| sv::AnalyzerError::Unsupported(format!("module `{}`", module.name)))?;
    lower_module(specialized, four_state, module.implicit_nets_allowed)
}

fn lower_module(
    module: &sv::ir::Module,
    four_state: bool,
    implicit_nets_allowed: bool,
) -> Result<LoweredSvModule, sv::AnalyzerError> {
    lower_module_with_overrides(module, &[], four_state, implicit_nets_allowed)
}

fn lower_module_with_overrides(
    module: &sv::ir::Module,
    parameter_overrides: &[LoweredSvParameterOverride],
    four_state: bool,
    implicit_nets_allowed: bool,
) -> Result<LoweredSvModule, sv::AnalyzerError> {
    let name = module.name().to_string();
    let mut next_id = SourceVarId::default();
    let mut variables = HashMap::default();
    let mut name_to_id = HashMap::default();
    let mut port_order = Vec::new();
    let mut initial_memory_values = Vec::new();
    let parameter_types = module
        .parameters()
        .iter()
        .filter_map(|parameter| {
            Some((
                parameter.name().to_string(),
                (
                    parameter.resolved_width()?,
                    parameter.resolved_signed().unwrap_or(false),
                ),
            ))
        })
        .collect();
    let constants = module_constants_with_overrides(module, parameter_overrides);
    validate_variable_driver_ranges(module, &constants, &parameter_types)?;

    for port in module.ports() {
        if name_to_id.contains_key(port.name()) {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "duplicate port name `{}`",
                port.name()
            )));
        }
        let id = next_var_id(&mut next_id);
        let type_info = signal_type_from_sv(port.r#type(), &constants, &parameter_types)?;
        let path = vec![port.name().to_string()];
        let kind = signal_kind_from_port_direction(port.direction())?;
        let variable = SvVariable {
            path,
            width: type_info.width,
            signed: type_info.signed,
            is_4state: type_info.is_4state,
            is_net: port.is_net(),
            packed_ranges: type_info.packed_ranges,
            array_dims: type_info.array_dims,
            domain_kind: DomainKind::Other,
            kind,
            type_kind: type_info.type_kind,
            source: None,
        };
        name_to_id.insert(port.name().to_string(), id);
        port_order.push(id);
        if port.is_net() || type_info.is_4state {
            let written_mask = (BigUint::from(1u8) << type_info.width) - BigUint::from(1u8);
            let value = if port.is_net() {
                BigUint::default()
            } else {
                written_mask.clone()
            };
            initial_memory_values.push(InitialStateValue {
                address: id,
                data: InitialStateData::Packed {
                    value,
                    mask: written_mask.clone(),
                    written_mask,
                },
            });
        }
        variables.insert(id, variable);
    }

    for signal in module.signals() {
        if name_to_id.contains_key(signal.name()) {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "duplicate port or signal name `{}`",
                signal.name()
            )));
        }
        let id = next_var_id(&mut next_id);
        let type_info = signal_type_from_sv(signal.r#type(), &constants, &parameter_types)?;
        let path = vec![signal.name().to_string()];
        let variable = SvVariable {
            path,
            width: type_info.width,
            signed: type_info.signed,
            is_4state: type_info.is_4state,
            is_net: signal.is_net(),
            packed_ranges: type_info.packed_ranges,
            array_dims: type_info.array_dims,
            domain_kind: DomainKind::Other,
            kind: VariableKind::Variable,
            type_kind: type_info.type_kind,
            source: None,
        };
        name_to_id.insert(signal.name().to_string(), id);
        if signal.is_net() || type_info.is_4state {
            let written_mask = (BigUint::from(1u8) << type_info.width) - BigUint::from(1u8);
            let value = if signal.is_net() {
                BigUint::default()
            } else {
                written_mask.clone()
            };
            initial_memory_values.push(InitialStateValue {
                address: id,
                data: InitialStateData::Packed {
                    value,
                    mask: written_mask.clone(),
                    written_mask,
                },
            });
        }
        variables.insert(id, variable);
    }

    let (eval_only_ff_blocks, apply_ff_blocks, eval_apply_ff_blocks, reset_clock_map) =
        lower_ff_processes(
            module,
            &variables,
            &name_to_id,
            &constants,
            &parameter_types,
            four_state,
        )?;
    mark_ff_event_domains(module, &mut variables, &name_to_id);

    let shared_variables = variables
        .iter()
        .map(|(&id, variable)| (id, variable.to_symbolic_variable()))
        .collect();
    let mut instances = Vec::new();
    for instance in module.instances() {
        if let Some(condition) = instance.condition() {
            let condition =
                sv::typecheck::eval_const_expr_with_types(condition, &constants, &parameter_types)
                    .ok_or_else(|| {
                        sv::AnalyzerError::Unsupported(
                            "unknown conditional-generate condition".to_string(),
                        )
                    })?;
            if condition == 0 {
                continue;
            }
        }
        instances.push(LoweredSvInstance {
            module_name: instance.module_name().to_string(),
            instance_name: instance.name().to_string(),
            parameter_overrides: lower_parameter_overrides(instance, &constants, &parameter_types),
            port_connections: instance
                .port_connections()
                .iter()
                .map(|connection| LoweredSvPortConnection {
                    formal: connection.formal().to_string(),
                    actual: connection.actual().to_string(),
                    actual_expr: connection.actual_expr().cloned(),
                })
                .collect(),
        });
    }

    Ok(LoweredSvModule {
        source: module.clone(),
        implicit_nets_allowed,
        sim_module: SimModule {
            name,
            variables: shared_variables,
            ff_access_summaries: HashMap::default(),
            eval_only_ff_blocks,
            apply_ff_blocks,
            eval_apply_ff_blocks,
            glue_blocks: HashMap::default(),
            indexed_instance_names: HashSet::default(),
            comb_blocks: Vec::new(),
            comb_observers: Vec::<CombObserver<SourceVarId>>::new(),
            runtime_errors: HashMap::<i64, RuntimeErrorInfo<SourceVarId>>::default(),
            runtime_event_sites: Vec::new(),
            initial_memory_values,
            comb_boundaries: HashMap::default(),
            arena: SLTNodeArena::new(),
            reset_clock_map,
        },
        variables,
        port_order,
        signal_names: name_to_id,
        constants: constants.clone(),
        parameter_types,
        instances,
    })
}

fn mark_ff_event_domains(
    module: &sv::ir::Module,
    variables: &mut HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
) {
    for process in module.ff_processes() {
        let Some(clock) = clock_event_from_ff_process(process) else {
            continue;
        };
        if let Some(id) = name_to_id.get(clock.signal()).copied()
            && let Some(variable) = variables.get_mut(&id)
        {
            variable.domain_kind = match clock.edge() {
                sv::ir::FfEdge::Pos => DomainKind::ClockPosedge,
                sv::ir::FfEdge::Neg => DomainKind::ClockNegedge,
            };
            variable.type_kind = PortTypeKind::Clock;
        }
        for event in process
            .events()
            .iter()
            .filter(|event| event.signal() != clock.signal())
        {
            if let Some(id) = name_to_id.get(event.signal()).copied()
                && let Some(variable) = variables.get_mut(&id)
            {
                variable.domain_kind = match event.edge() {
                    sv::ir::FfEdge::Pos => DomainKind::ResetAsyncHigh,
                    sv::ir::FfEdge::Neg => DomainKind::ResetAsyncLow,
                };
                variable.type_kind = match event.edge() {
                    sv::ir::FfEdge::Pos => PortTypeKind::ResetAsyncHigh,
                    sv::ir::FfEdge::Neg => PortTypeKind::ResetAsyncLow,
                };
            }
        }
    }
}

fn evaluated_parameter_overrides(
    parameter_overrides: &[LoweredSvParameterOverride],
) -> Result<HashMap<String, sv::ir::ConstExpr>, sv::AnalyzerError> {
    let constants = HashMap::default();
    let mut evaluated = HashMap::default();
    for parameter in parameter_overrides {
        let Some(value) = parameter.value.as_ref() else {
            continue;
        };
        sv::typecheck::eval_const_expr(value, &constants).ok_or_else(|| {
            sv::AnalyzerError::Unsupported(format!(
                "non-integer module parameter override `{}`",
                parameter.name
            ))
        })?;
        evaluated.insert(parameter.name.clone(), value.clone());
    }
    Ok(evaluated)
}

fn lower_parameter_overrides(
    instance: &sv::ir::Instance,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Vec<LoweredSvParameterOverride> {
    instance
        .parameter_overrides()
        .iter()
        .map(|parameter| {
            let value = parameter.value().cloned().map(|value| {
                let value =
                    sv::typecheck::substitute_typed_constants(value, constants, parameter_types);
                if const_expr_references_identifier(&value) {
                    sv::typecheck::eval_const_expr_with_types(&value, constants, parameter_types)
                        .map(const_expr_from_i128)
                        .unwrap_or(value)
                } else {
                    value
                }
            });
            LoweredSvParameterOverride {
                name: parameter.name().to_string(),
                value,
            }
        })
        .collect()
}

fn const_expr_references_identifier(expr: &sv::ir::ConstExpr) -> bool {
    match expr {
        sv::ir::ConstExpr::Ident(_) => true,
        sv::ir::ConstExpr::Literal(_) => false,
        sv::ir::ConstExpr::Select { expr, bit } => {
            const_expr_references_identifier(expr) || const_expr_references_identifier(bit)
        }
        sv::ir::ConstExpr::Function { args, .. } => {
            args.iter().any(const_expr_references_identifier)
        }
        sv::ir::ConstExpr::Unary { expr, .. } => const_expr_references_identifier(expr),
        sv::ir::ConstExpr::Binary { left, right, .. } => {
            const_expr_references_identifier(left) || const_expr_references_identifier(right)
        }
        sv::ir::ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            const_expr_references_identifier(condition)
                || const_expr_references_identifier(then_expr)
                || const_expr_references_identifier(else_expr)
        }
    }
}

fn const_expr_from_i128(value: i128) -> sv::ir::ConstExpr {
    if value < 0 {
        sv::ir::ConstExpr::Unary {
            op: sv::ir::UnaryOp::Minus,
            expr: Box::new(sv::ir::ConstExpr::Literal(value.unsigned_abs().to_string())),
        }
    } else {
        sv::ir::ConstExpr::Literal(value.to_string())
    }
}

fn parameter_value_bits(value: i128, width: usize) -> BigUint {
    let modulus = BigUint::from(1u8) << width;
    if value >= 0 {
        BigUint::from(value as u128) % modulus
    } else {
        let remainder = BigUint::from(value.unsigned_abs()) % &modulus;
        if remainder == BigUint::default() {
            remainder
        } else {
            modulus - remainder
        }
    }
}

pub(crate) fn attach_instance_glue(
    module: &mut SimModule,
    lowered: &LoweredSvModule,
    current_key: &LoweredSvModuleKey,
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    lowered_modules: &HashMap<ModuleId, LoweredSvModule>,
    four_state: bool,
) -> Result<(), ParserError> {
    let mut signal_names = lowered.signal_names.clone();
    let mut parent_variables = lowered.variables.clone();
    let mut implicit_output_signals = HashSet::default();
    let mut resolved_instances = Vec::new();
    for instance in &lowered.instances {
        let child_key = LoweredSvModuleKey::instance_key(instance);
        let Some(child_id) = module_ids.get(&child_key).copied() else {
            return Err(unsupported_sv_instance(instance.module_name.clone()));
        };
        if &child_key == current_key {
            return Err(ParserError::unsupported(
                64,
                LoweringPhase::SimulatorParser,
                "recursive systemverilog module instantiation",
                instance.module_name.clone(),
                None,
            ));
        }
        let Some(child) = lowered_modules.get(&child_id) else {
            return Err(unsupported_sv_instance(instance.module_name.clone()));
        };
        ensure_parent_output_signals(
            module,
            &mut parent_variables,
            &mut signal_names,
            &mut implicit_output_signals,
            lowered.implicit_nets_allowed,
            &lowered.source,
            &lowered.constants,
            &lowered.parameter_types,
            child,
            &instance.port_connections,
        )?;
        resolved_instances.push((instance, child_id, child));
    }
    let (comb_blocks, arena) = lower_comb_processes(
        &lowered.source,
        &parent_variables,
        &signal_names,
        &lowered.constants,
        &lowered.parameter_types,
        four_state,
    )
    .map_err(|error| {
        ParserError::unsupported(
            64,
            LoweringPhase::SimulatorParser,
            "systemverilog combinational process lowering",
            error.to_string(),
            None,
        )
    })?;
    module.comb_blocks = comb_blocks;
    module.arena = arena;
    for (instance, child_id, child) in resolved_instances {
        let glue = build_instance_glue(
            &parent_variables,
            &signal_names,
            &lowered.constants,
            &lowered.parameter_types,
            child,
            &instance.port_connections,
            four_state,
        )?;
        module
            .glue_blocks
            .entry(instance.instance_name.clone())
            .or_default()
            .push(GlueBlock {
                module_id: child_id,
                input_ports: glue.0,
                output_ports: glue.1,
                arena: glue.2,
            });
    }
    Ok(())
}

fn expr_for_state_mode(expr: &sv::ir::Expr, four_state: bool) -> sv::ir::Expr {
    match expr {
        sv::ir::Expr::Mux {
            then_expr,
            else_expr,
            ..
        } if matches!(
            &**then_expr,
            sv::ir::Expr::Literal(literal)
                if literal == sv::DIV_ZERO_UNKNOWN_LITERAL
        ) =>
        {
            if four_state {
                let sv::ir::Expr::Mux {
                    condition,
                    else_expr,
                    ..
                } = expr
                else {
                    unreachable!()
                };
                sv::ir::Expr::Mux {
                    condition: Box::new(expr_for_state_mode(condition, four_state)),
                    then_expr: Box::new(sv::ir::Expr::Literal("'x".to_string())),
                    else_expr: Box::new(expr_for_state_mode(else_expr, four_state)),
                }
            } else {
                expr_for_state_mode(else_expr, four_state)
            }
        }
        sv::ir::Expr::Literal(literal) if !four_state && expr_is_unknown_literal(expr) => {
            if unbased_fill_literal(literal).is_some() {
                sv::ir::Expr::Literal("'0".to_string())
            } else {
                sv::ir::Expr::Unary {
                    op: sv::ir::UnaryOp::ToTwoState,
                    expr: Box::new(expr.clone()),
                }
            }
        }
        sv::ir::Expr::Ident(_) | sv::ir::Expr::Literal(_) => expr.clone(),
        sv::ir::Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => sv::ir::Expr::Select {
            expr: Box::new(expr_for_state_mode(expr, four_state)),
            msb: msb.clone(),
            lsb: lsb.clone(),
            signed: *signed,
        },
        sv::ir::Expr::Concat(parts) => sv::ir::Expr::Concat(
            parts
                .iter()
                .map(|part| expr_for_state_mode(part, four_state))
                .collect(),
        ),
        sv::ir::Expr::RepeatConcat { count, parts } => sv::ir::Expr::RepeatConcat {
            count: count.clone(),
            parts: parts
                .iter()
                .map(|part| expr_for_state_mode(part, four_state))
                .collect(),
        },
        sv::ir::Expr::Resize {
            expr,
            width,
            signed,
        } => sv::ir::Expr::Resize {
            expr: Box::new(expr_for_state_mode(expr, four_state)),
            width: *width,
            signed: *signed,
        },
        sv::ir::Expr::Unary { op, expr } => sv::ir::Expr::Unary {
            op: *op,
            expr: Box::new(expr_for_state_mode(expr, four_state)),
        },
        sv::ir::Expr::Binary { left, op, right } => sv::ir::Expr::Binary {
            left: Box::new(expr_for_state_mode(left, four_state)),
            op: *op,
            right: Box::new(expr_for_state_mode(right, four_state)),
        },
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => sv::ir::Expr::Mux {
            condition: Box::new(expr_for_state_mode(condition, four_state)),
            then_expr: Box::new(expr_for_state_mode(then_expr, four_state)),
            else_expr: Box::new(expr_for_state_mode(else_expr, four_state)),
        },
        sv::ir::Expr::Call { name, args } => sv::ir::Expr::Call {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| expr_for_state_mode(arg, four_state))
                .collect(),
        },
    }
}

fn lower_comb_processes(
    module: &sv::ir::Module,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    four_state: bool,
) -> Result<(Vec<LogicPath<SourceVarId>>, SLTNodeArena<SourceVarId>), sv::AnalyzerError> {
    let mut arena = SLTNodeArena::new();
    let mut comb_blocks = Vec::new();
    for process in module.comb_processes() {
        if let Some(condition) = process.condition() {
            let condition =
                sv::typecheck::eval_const_expr_with_types(condition, constants, parameter_types)
                    .ok_or_else(|| {
                        sv::AnalyzerError::Unsupported(
                            "unknown conditional-generate condition".to_string(),
                        )
                    })?;
            if condition == 0 {
                continue;
            }
        }
        comb_blocks.extend(lower_comb_process(
            process,
            variables,
            name_to_id,
            constants,
            parameter_types,
            &mut arena,
            four_state,
        )?);
    }
    Ok((comb_blocks, arena))
}

fn ensure_parent_output_signals(
    parent: &mut SimModule,
    parent_variables: &mut HashMap<SourceVarId, SvVariable>,
    parent_signal_names: &mut HashMap<String, SourceVarId>,
    implicit_output_signals: &mut HashSet<String>,
    implicit_nets_allowed: bool,
    parent_source: &sv::ir::Module,
    parent_constants: &HashMap<String, i128>,
    parent_parameter_types: &HashMap<String, (usize, bool)>,
    child: &LoweredSvModule,
    connections: &[LoweredSvPortConnection],
) -> Result<(), ParserError> {
    for child_port_id in &child.port_order {
        let child_var = &child.variables[child_port_id];
        if child_var.kind != VariableKind::Output {
            continue;
        }
        let formal = child_var.path.join(".");
        let Some(connection) = connections
            .iter()
            .find(|connection| connection.formal == formal)
        else {
            continue;
        };
        let Some(actual) = connection
            .actual_expr
            .as_ref()
            .and_then(simple_output_lvalue_ident)
        else {
            continue;
        };
        if parent_signal_names.contains_key(actual) {
            if implicit_output_signals.contains(actual) {
                return Err(ParserError::illegal_context(
                    "systemverilog output port connection",
                    format!("multiple child outputs drive implicit net `{actual}`"),
                    None,
                ));
            }
            continue;
        }
        if parent_constants.contains_key(actual) {
            return Err(ParserError::illegal_context(
                "systemverilog output port connection",
                format!("cannot drive parameter `{actual}`"),
                None,
            ));
        }
        if !implicit_nets_allowed {
            return Err(ParserError::illegal_context(
                "systemverilog output port connection",
                format!("implicit net `{actual}` disabled by `default_nettype none"),
                None,
            ));
        }
        if !local_driver_ranges(
            parent_source,
            actual,
            parent_constants,
            parent_parameter_types,
        )
        .is_empty()
        {
            return Err(ParserError::illegal_context(
                "systemverilog output port connection",
                format!("multiple net drivers for `{actual}`"),
                None,
            ));
        }
        let mut next_id = SourceVarId::default();
        while parent.variables.contains_key(&next_id) {
            next_id.0 += 1;
        }
        parent_signal_names.insert(actual.to_string(), next_id);
        implicit_output_signals.insert(actual.to_string());
        let variable = SvVariable {
            path: vec![actual.to_string()],
            width: 1,
            signed: false,
            is_4state: true,
            is_net: true,
            packed_ranges: Vec::new(),
            array_dims: Vec::new(),
            domain_kind: DomainKind::Other,
            kind: VariableKind::Variable,
            type_kind: PortTypeKind::Logic,
            source: None,
        };
        parent
            .variables
            .insert(next_id, variable.to_symbolic_variable());
        parent_variables.insert(next_id, variable);
    }
    Ok(())
}

type SvGlue = (
    Vec<(Vec<SourceVarId>, LogicPath<GlueAddr>)>,
    Vec<(Vec<SourceVarId>, LogicPath<GlueAddr>)>,
    SLTNodeArena<GlueAddr>,
);

fn build_instance_glue(
    parent_variables: &HashMap<SourceVarId, SvVariable>,
    parent_signal_names: &HashMap<String, SourceVarId>,
    parent_constants: &HashMap<String, i128>,
    parent_parameter_types: &HashMap<String, (usize, bool)>,
    child: &LoweredSvModule,
    connections: &[LoweredSvPortConnection],
    four_state: bool,
) -> Result<SvGlue, ParserError> {
    let mut input_ports = Vec::new();
    let mut output_ports = Vec::new();
    let mut arena = SLTNodeArena::<GlueAddr>::new();

    let mut connected_formals = HashSet::default();
    for connection in connections {
        let matches = child
            .port_order
            .iter()
            .filter(|port_id| child.variables[port_id].path.join(".") == connection.formal)
            .count();
        if matches != 1 || !connected_formals.insert(connection.formal.clone()) {
            return Err(ParserError::unsupported(
                64,
                LoweringPhase::SimulatorParser,
                "unknown or duplicate systemverilog child port connection",
                connection.formal.clone(),
                None,
            ));
        }
    }

    for child_port_id in &child.port_order {
        let child_var = &child.variables[child_port_id];
        let formal = child_var.path.join(".");
        let connection = connections
            .iter()
            .find(|connection| connection.formal == formal);
        let width = child_var.width;
        match child_var.kind {
            VariableKind::Input => {
                let collapse_unknown_literal = !four_state
                    && connection
                        .and_then(|item| item.actual_expr.as_ref())
                        .is_some_and(expr_is_unknown_literal);
                let (mut expr, sources, source_ids) = if let Some(actual_expr) =
                    connection.and_then(|item| item.actual_expr.as_ref())
                {
                    let actual_expr = expr_for_state_mode(actual_expr, four_state);
                    let actual = connection.map_or("", |item| item.actual.as_str());
                    let (expr, sources, source_ids) = lower_glue_parent_expr(
                        &actual_expr,
                        parent_variables,
                        parent_signal_names,
                        parent_constants,
                        parent_parameter_types,
                        &mut arena,
                        Some(width),
                        Some(sv_glue_expr_is_signed(
                            &actual_expr,
                            parent_variables,
                            parent_signal_names,
                            parent_parameter_types,
                        )),
                    )
                    .ok_or_else(|| {
                        ParserError::unsupported(
                            64,
                            LoweringPhase::SimulatorParser,
                            "systemverilog input port connection",
                            format!("{formal} -> {actual}"),
                            None,
                        )
                    })?;
                    let expr = coerce_node_width(
                        &mut arena,
                        expr,
                        Some(width),
                        sv_glue_expr_is_signed(
                            &actual_expr,
                            parent_variables,
                            parent_signal_names,
                            parent_parameter_types,
                        ),
                    )?;
                    (expr, sources, source_ids)
                } else {
                    let unknown_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
                    (
                        arena.alloc(SLTNode::Constant(
                            BigUint::default(),
                            unknown_mask,
                            width,
                            false,
                        ))?,
                        HashSet::default(),
                        Vec::new(),
                    )
                };
                if !child_var.is_4state || collapse_unknown_literal {
                    expr = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, expr))?;
                }
                input_ports.push((
                    source_ids,
                    LogicPath {
                        target: LogicPathTarget::Var(VarAtomBase::new(
                            GlueAddr::Child(*child_port_id),
                            0,
                            width - 1,
                        )),
                        expr,
                        sources,
                        address_sources: HashSet::default(),
                        previous_sources: HashSet::default(),
                        local_inputs: Vec::new(),
                        order_before: HashSet::default(),
                        comb_capture_enable_sites: Vec::new(),
                        comb_capture_enable_always: false,
                        pre_lower_nodes: Vec::new(),
                    },
                ));
            }
            VariableKind::Output => {
                let Some(connection) = connection else {
                    continue;
                };
                let actual = connection.actual.as_str();
                let Some(actual_expr) = connection.actual_expr.as_ref() else {
                    continue;
                };
                if let Some(dynamic_output) = lower_dynamic_output_glue(
                    actual_expr,
                    parent_variables,
                    parent_signal_names,
                    parent_constants,
                    parent_parameter_types,
                    *child_port_id,
                    child_var,
                    &mut arena,
                    &formal,
                    actual,
                )? {
                    output_ports.push(dynamic_output);
                    continue;
                }
                let Some((parent_signal_id, access)) = output_lvalue_access(
                    actual_expr,
                    parent_variables,
                    parent_signal_names,
                    parent_constants,
                    parent_parameter_types,
                ) else {
                    return Err(ParserError::unsupported(
                        64,
                        LoweringPhase::SimulatorParser,
                        "systemverilog output port lvalue connection",
                        format!("{formal} -> {actual}: {actual_expr:?}"),
                        None,
                    ));
                };
                let parent_var = &parent_variables[&parent_signal_id];
                let target_width = access.msb - access.lsb + 1;
                let child_node = arena.alloc(SLTNode::Input {
                    variable: GlueAddr::Child(*child_port_id),
                    signed: child_var.signed,
                    index: Vec::new(),
                    access: BitAccess::new(0, width - 1),
                })?;
                let mut expr = coerce_node_width(
                    &mut arena,
                    child_node,
                    Some(target_width),
                    child_var.signed,
                )?;
                if !parent_var.is_4state {
                    expr = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, expr))?;
                }
                let mut sources = HashSet::default();
                sources.insert(VarAtomBase::new(
                    GlueAddr::Child(*child_port_id),
                    0,
                    width - 1,
                ));
                output_ports.push((
                    vec![parent_signal_id],
                    LogicPath {
                        target: LogicPathTarget::Var(VarAtomBase::new(
                            GlueAddr::Parent(parent_signal_id),
                            access.lsb,
                            access.msb,
                        )),
                        expr,
                        sources,
                        address_sources: HashSet::default(),
                        previous_sources: HashSet::default(),
                        local_inputs: Vec::new(),
                        order_before: HashSet::default(),
                        comb_capture_enable_sites: Vec::new(),
                        comb_capture_enable_always: false,
                        pre_lower_nodes: Vec::new(),
                    },
                ));
            }
            VariableKind::Inout => {
                return Err(unsupported_sv_inout(child_var.path.join(".")));
            }
            _ => {}
        }
    }

    Ok((input_ports, output_ports, arena))
}

fn lower_dynamic_output_glue(
    actual_expr: &sv::ir::Expr,
    parent_variables: &HashMap<SourceVarId, SvVariable>,
    parent_signal_names: &HashMap<String, SourceVarId>,
    parent_constants: &HashMap<String, i128>,
    parent_parameter_types: &HashMap<String, (usize, bool)>,
    child_port_id: SourceVarId,
    child_var: &SvVariable,
    arena: &mut SLTNodeArena<GlueAddr>,
    formal: &str,
    actual: &str,
) -> Result<Option<(Vec<SourceVarId>, LogicPath<GlueAddr>)>, ParserError> {
    let sv::ir::Expr::Select { expr, msb, lsb, .. } = actual_expr else {
        return Ok(None);
    };
    let Some((parent_signal_id, element_width, access)) = dynamic_array_element_subselection(
        expr,
        msb,
        lsb,
        parent_variables,
        parent_signal_names,
        parent_constants,
        parent_parameter_types,
    ) else {
        return Ok(None);
    };
    let parent_var = &parent_variables[&parent_signal_id];
    if parent_var.is_net {
        return Err(ParserError::unsupported(
            64,
            LoweringPhase::SimulatorParser,
            "dynamic child output connection to a net",
            format!("{formal} -> {actual}: {actual_expr:?}"),
            None,
        ));
    }
    let (offset, index_sources, index_source_ids) = lower_dynamic_array_element_index_glue(
        lsb,
        parent_variables,
        parent_signal_names,
        parent_constants,
        parent_parameter_types,
        arena,
        element_width,
    )
    .ok_or_else(|| {
        ParserError::unsupported(
            64,
            LoweringPhase::SimulatorParser,
            "systemverilog output port lvalue connection",
            format!("{formal} -> {actual}: {actual_expr:?}"),
            None,
        )
    })?;
    let element_count = parent_var.width / element_width;
    let child_node = arena.alloc(SLTNode::Input {
        variable: GlueAddr::Child(child_port_id),
        signed: child_var.signed,
        index: Vec::new(),
        access: BitAccess::new(0, child_var.width - 1),
    })?;
    let target_width = access.msb - access.lsb + 1;
    let child_expr = coerce_node_width(arena, child_node, Some(target_width), child_var.signed)?;
    let old = arena.alloc(SLTNode::Input {
        variable: GlueAddr::Parent(parent_signal_id),
        signed: parent_var.signed,
        index: Vec::new(),
        access: BitAccess::new(0, parent_var.width - 1),
    })?;
    let mut parts = Vec::with_capacity(element_count);
    for element in (0..element_count).rev() {
        let lsb = element * element_width;
        let old_element = arena.alloc(SLTNode::Slice {
            expr: old,
            access: BitAccess::new(lsb, lsb + element_width - 1),
        })?;
        let element_literal = arena.alloc(SLTNode::Constant(
            BigUint::from(element),
            BigUint::default(),
            64,
            false,
        ))?;
        let condition = arena.alloc(SLTNode::Binary(offset, BinaryOp::EqCase, element_literal))?;
        let Some(updated_element) = replace_slt_slice(
            arena,
            old_element,
            child_expr,
            access.lsb,
            target_width,
            element_width,
        ) else {
            return Ok(None);
        };
        let updated = arena.alloc(SLTNode::Mux {
            cond: condition,
            then_expr: updated_element,
            else_expr: old_element,
        })?;
        parts.push((updated, element_width));
    }
    let mut expr = if parts.len() == 1 {
        parts[0].0
    } else {
        arena.alloc(SLTNode::Concat(parts))?
    };
    if !parent_var.is_4state {
        expr = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, expr))?;
    }
    let mut sources = index_sources.clone();
    sources.insert(VarAtomBase::new(
        GlueAddr::Child(child_port_id),
        0,
        child_var.width - 1,
    ));
    let previous_sources = [VarAtomBase::new(
        GlueAddr::Parent(parent_signal_id),
        0,
        parent_var.width - 1,
    )]
    .into_iter()
    .collect();
    let mut source_ids = index_source_ids;
    source_ids.push(parent_signal_id);
    source_ids.sort();
    source_ids.dedup();
    Ok(Some((
        source_ids,
        LogicPath {
            target: LogicPathTarget::Var(VarAtomBase::new(
                GlueAddr::Parent(parent_signal_id),
                0,
                parent_var.width - 1,
            )),
            expr,
            sources,
            address_sources: index_sources,
            previous_sources,
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
        },
    )))
}

fn simple_output_lvalue_ident(expr: &sv::ir::Expr) -> Option<&str> {
    match expr {
        sv::ir::Expr::Ident(name) => Some(name),
        sv::ir::Expr::Resize { expr, .. } => simple_output_lvalue_ident(expr),
        _ => None,
    }
}

fn output_lvalue_access(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<(SourceVarId, BitAccess)> {
    match expr {
        sv::ir::Expr::Ident(name) => {
            let id = *name_to_id.get(name)?;
            let variable = variables.get(&id)?;
            Some((id, BitAccess::new(0, variable.width.checked_sub(1)?)))
        }
        sv::ir::Expr::Resize { expr, .. } => {
            output_lvalue_access(expr, variables, name_to_id, constants, parameter_types)
        }
        sv::ir::Expr::Select { expr, msb, lsb, .. } => {
            let sv::ir::Expr::Ident(name) = &**expr else {
                return None;
            };
            let id = *name_to_id.get(name)?;
            let variable = variables.get(&id)?;
            if variable.array_dims.is_empty() {
                return None;
            }
            let msb = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
            let lsb = sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
            let (msb, lsb) = packed_expr_select_offsets(expr, msb, lsb, variables, name_to_id)?;
            let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
            (access.msb < variable.width).then_some((id, access))
        }
        _ => None,
    }
}

fn lower_glue_parent_expr(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<GlueAddr>,
    context_width: Option<usize>,
    context_signed: Option<bool>,
) -> Option<(
    celox_slt::NodeId,
    HashSet<VarAtomBase<GlueAddr>>,
    Vec<SourceVarId>,
)> {
    match expr {
        sv::ir::Expr::Ident(name) => {
            let Some(id) = name_to_id.get(name).copied() else {
                let value = constants.get(name)?;
                let (width, signed) = parameter_types.get(name).copied().unwrap_or((32, false));
                let node = arena
                    .alloc(SLTNode::Constant(
                        parameter_value_bits(*value, width),
                        BigUint::from(0u32),
                        width,
                        signed,
                    ))
                    .ok()?;
                return Some((
                    coerce_node_width(arena, node, context_width, context_signed.unwrap_or(signed))
                        .ok()?,
                    HashSet::default(),
                    Vec::new(),
                ));
            };
            let var = variables.get(&id)?;
            let width = var.width;
            let node = arena
                .alloc(SLTNode::Input {
                    variable: GlueAddr::Parent(id),
                    signed: var.signed,
                    index: Vec::new(),
                    access: BitAccess::new(0, width - 1),
                })
                .ok()?;
            let mut sources = HashSet::default();
            sources.insert(VarAtomBase::new(GlueAddr::Parent(id), 0, width - 1));
            Some((
                coerce_node_width(
                    arena,
                    node,
                    context_width,
                    context_signed.unwrap_or(var.signed),
                )
                .ok()?,
                sources,
                vec![id],
            ))
        }
        sv::ir::Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => {
            if let Some((id, element_width, access)) = dynamic_array_element_subselection(
                expr,
                msb,
                lsb,
                variables,
                name_to_id,
                constants,
                parameter_types,
            ) {
                let (offset, mut sources, mut source_ids) = lower_dynamic_array_element_index_glue(
                    lsb,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    element_width,
                )?;
                let variable = variables.get(&id)?;
                let element_count = variable.width.checked_div(element_width)?;
                let (offset, valid) = dynamic_array_index_guard_slt(arena, offset, element_count)?;
                let node = arena
                    .alloc(SLTNode::Input {
                        variable: GlueAddr::Parent(id),
                        signed: *signed,
                        index: vec![SLTIndex {
                            node: offset,
                            stride: element_width,
                            kind: SLTIndexKind::Unpacked { element_width },
                        }],
                        access,
                    })
                    .ok()?;
                let node = guard_dynamic_array_read_slt(
                    arena,
                    valid,
                    node,
                    access.msb - access.lsb + 1,
                    variable.is_4state,
                )?;
                sources.insert(VarAtomBase::new(
                    GlueAddr::Parent(id),
                    0,
                    variable.width.checked_sub(1)?,
                ));
                source_ids.push(id);
                source_ids.sort();
                source_ids.dedup();
                return Some((
                    coerce_node_width(
                        arena,
                        node,
                        context_width,
                        context_signed.unwrap_or(*signed),
                    )
                    .ok()?,
                    sources,
                    source_ids,
                ));
            }
            let (inner, sources, source_ids) = lower_glue_parent_expr(
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                None,
                None,
            )?;
            let msb_value =
                sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
            let lsb_value =
                sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
            let (msb, lsb) =
                packed_expr_select_offsets(expr, msb_value, lsb_value, variables, name_to_id)?;
            let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
            let node = arena
                .alloc(SLTNode::Slice {
                    expr: inner,
                    access,
                })
                .ok()?;
            let sources = select_sources(expr, sources, access)?;
            Some((
                coerce_node_width(
                    arena,
                    node,
                    context_width,
                    context_signed.unwrap_or(*signed),
                )
                .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Concat(parts) => {
            let mut nodes = Vec::new();
            let mut sources = HashSet::default();
            let mut source_ids = Vec::new();
            for part in parts {
                let (node, part_sources, part_source_ids) =
                    if let Some(fill) = expr_unbased_fill_literal(part) {
                        (
                            lower_unbased_fill_literal_slt(arena, fill, 1)?,
                            HashSet::default(),
                            Vec::new(),
                        )
                    } else {
                        lower_glue_parent_expr(
                            part,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                            arena,
                            None,
                            None,
                        )?
                    };
                let width = celox_slt::get_width(node, arena);
                nodes.push((node, width));
                sources.extend(part_sources);
                source_ids.extend(part_source_ids);
            }
            source_ids.sort();
            source_ids.dedup();
            let node = arena.alloc(SLTNode::Concat(nodes)).ok()?;
            Some((
                coerce_node_width(arena, node, context_width, context_signed.unwrap_or(false))
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::RepeatConcat { count, parts } => {
            let count =
                sv::typecheck::eval_const_expr_with_types(count, constants, parameter_types)?;
            let count = usize::try_from(count).ok()?;
            let mut nodes = Vec::new();
            let mut sources = HashSet::default();
            let mut source_ids = Vec::new();
            for _ in 0..count {
                for part in parts {
                    let (node, part_sources, part_source_ids) =
                        if let Some(fill) = expr_unbased_fill_literal(part) {
                            (
                                lower_unbased_fill_literal_slt(arena, fill, 1)?,
                                HashSet::default(),
                                Vec::new(),
                            )
                        } else {
                            lower_glue_parent_expr(
                                part,
                                variables,
                                name_to_id,
                                constants,
                                parameter_types,
                                arena,
                                None,
                                None,
                            )?
                        };
                    let width = celox_slt::get_width(node, arena);
                    nodes.push((node, width));
                    sources.extend(part_sources);
                    source_ids.extend(part_source_ids);
                }
            }
            source_ids.sort();
            source_ids.dedup();
            let node = arena.alloc(SLTNode::Concat(nodes)).ok()?;
            Some((
                coerce_node_width(arena, node, context_width, context_signed.unwrap_or(false))
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Resize {
            expr,
            width,
            signed,
        } => {
            let (inner, sources, source_ids) = lower_glue_parent_expr(
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                Some(*width),
                Some(*signed),
            )?;
            let resized = coerce_node_width(arena, inner, Some(*width), *signed).ok()?;
            Some((
                coerce_node_width(
                    arena,
                    resized,
                    context_width,
                    context_signed.unwrap_or(*signed),
                )
                .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Literal(literal) => {
            if let Some(width) = context_width
                && let Some(fill) = unbased_fill_literal(literal)
            {
                return Some((
                    lower_unbased_fill_literal_slt(arena, fill, width)?,
                    HashSet::default(),
                    Vec::new(),
                ));
            }
            let literal = sv::typecheck::parse_integral_literal(literal)?;
            let signed = literal.signed;
            let node = arena
                .alloc(SLTNode::Constant(
                    literal.value,
                    literal.mask,
                    literal.width,
                    signed,
                ))
                .ok()?;
            Some((
                coerce_node_width(arena, node, context_width, context_signed.unwrap_or(signed))
                    .ok()?,
                HashSet::default(),
                Vec::new(),
            ))
        }
        sv::ir::Expr::Unary { op, expr } => {
            let one_bit_result = matches!(
                op,
                sv::ir::UnaryOp::LogicNot
                    | sv::ir::UnaryOp::RedAnd
                    | sv::ir::UnaryOp::RedOr
                    | sv::ir::UnaryOp::RedXor
            );
            let operand_context = (!one_bit_result).then_some(context_width).flatten();
            let operand_signed = context_signed.or_else(|| {
                Some(sv_glue_expr_is_signed(
                    expr,
                    variables,
                    name_to_id,
                    parameter_types,
                ))
            });
            let (inner, sources, source_ids) = lower_glue_parent_expr(
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                operand_context,
                operand_signed,
            )?;
            Some((
                arena
                    .alloc(SLTNode::Unary(unary_op_from_sv(*op)?, inner))
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Binary { left, op, right } => {
            let left_signed = sv_glue_expr_is_signed(left, variables, name_to_id, parameter_types);
            let operands_signed = left_signed
                && sv_glue_expr_is_signed(right, variables, name_to_id, parameter_types);
            let operator_signed = if matches!(op, sv::ir::BinaryOp::Sar) {
                left_signed
            } else {
                operands_signed
            };
            let comparison = matches!(
                op,
                sv::ir::BinaryOp::Eq
                    | sv::ir::BinaryOp::Ne
                    | sv::ir::BinaryOp::EqCase
                    | sv::ir::BinaryOp::NeCase
                    | sv::ir::BinaryOp::EqWildcard
                    | sv::ir::BinaryOp::NeWildcard
                    | sv::ir::BinaryOp::Lt
                    | sv::ir::BinaryOp::Le
                    | sv::ir::BinaryOp::Gt
                    | sv::ir::BinaryOp::Ge
            );
            let shift = matches!(
                op,
                sv::ir::BinaryOp::Shl | sv::ir::BinaryOp::Shr | sv::ir::BinaryOp::Sar
            );
            let context_determined = !comparison
                && !matches!(op, sv::ir::BinaryOp::LogicAnd | sv::ir::BinaryOp::LogicOr);
            let operation_context = context_width.map(|context_width| {
                context_width.max(
                    sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)
                        .unwrap_or(context_width),
                )
            });
            let comparison_context = comparison
                .then(|| {
                    sv_comparison_operand_width(
                        left,
                        right,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                    )
                })
                .flatten();
            let left_context = if comparison {
                comparison_context
            } else {
                context_determined.then_some(operation_context).flatten()
            };
            let right_context = if comparison {
                comparison_context
            } else {
                (context_determined && !shift)
                    .then_some(operation_context)
                    .flatten()
            };
            let left_context_signed = Some(if shift { left_signed } else { operands_signed });
            let right_context_signed = Some(operands_signed);
            let context_sized_comparison = comparison;
            let left_fill = (context_sized_comparison || shift)
                .then(|| expr_unbased_fill_literal(left))
                .flatten();
            let right_fill = (context_sized_comparison || shift)
                .then(|| expr_unbased_fill_literal(right))
                .flatten();
            let (
                (mut left, mut sources, mut source_ids),
                (mut right, right_sources, right_source_ids),
            ) = match (left_fill, right_fill) {
                (Some(left_fill), Some(right_fill)) => {
                    let left_width = if shift { left_context.unwrap_or(1) } else { 1 };
                    (
                        (
                            lower_unbased_fill_literal_slt(arena, left_fill, left_width)?,
                            HashSet::default(),
                            Vec::new(),
                        ),
                        (
                            lower_unbased_fill_literal_slt(arena, right_fill, 1)?,
                            HashSet::default(),
                            Vec::new(),
                        ),
                    )
                }
                (Some(fill), None) => {
                    let right = lower_glue_parent_expr(
                        right,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        right_context,
                        right_context_signed,
                    )?;
                    let width = if shift {
                        left_context.unwrap_or(1)
                    } else {
                        celox_slt::get_width(right.0, arena)
                    };
                    (
                        (
                            lower_unbased_fill_literal_slt(arena, fill, width)?,
                            HashSet::default(),
                            Vec::new(),
                        ),
                        right,
                    )
                }
                (None, Some(fill)) => {
                    let left = lower_glue_parent_expr(
                        left,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        left_context,
                        left_context_signed,
                    )?;
                    let width = if shift {
                        1
                    } else {
                        celox_slt::get_width(left.0, arena)
                    };
                    (
                        left,
                        (
                            lower_unbased_fill_literal_slt(arena, fill, width)?,
                            HashSet::default(),
                            Vec::new(),
                        ),
                    )
                }
                (None, None) => (
                    lower_glue_parent_expr(
                        left,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        left_context,
                        left_context_signed,
                    )?,
                    lower_glue_parent_expr(
                        right,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        right_context,
                        right_context_signed,
                    )?,
                ),
            };
            sources.extend(right_sources);
            source_ids.extend(right_source_ids);
            source_ids.sort();
            source_ids.dedup();
            if context_sized_comparison {
                let common_width =
                    celox_slt::get_width(left, arena).max(celox_slt::get_width(right, arena));
                left = coerce_node_width(arena, left, Some(common_width), operands_signed).ok()?;
                right =
                    coerce_node_width(arena, right, Some(common_width), operands_signed).ok()?;
            }
            Some((
                arena
                    .alloc(SLTNode::Binary(
                        left,
                        binary_op_from_sv(*op, operator_signed),
                        right,
                    ))
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let arms_signed =
                sv_glue_expr_is_signed(then_expr, variables, name_to_id, parameter_types)
                    && sv_glue_expr_is_signed(else_expr, variables, name_to_id, parameter_types);
            let arm_context =
                sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)
                    .map(|natural_width| {
                        context_width.map_or(natural_width, |width| width.max(natural_width))
                    })
                    .or(context_width);
            let (condition, mut sources, mut source_ids) = lower_glue_parent_expr(
                condition,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                None,
                None,
            )?;
            let (mut then_expr, then_sources, then_source_ids) = lower_glue_parent_expr(
                then_expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                arm_context,
                Some(arms_signed),
            )?;
            let (mut else_expr, else_sources, else_source_ids) = lower_glue_parent_expr(
                else_expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                arm_context,
                Some(arms_signed),
            )?;
            sources.extend(then_sources);
            sources.extend(else_sources);
            source_ids.extend(then_source_ids);
            source_ids.extend(else_source_ids);
            source_ids.sort();
            source_ids.dedup();
            let width =
                celox_slt::get_width(then_expr, arena).max(celox_slt::get_width(else_expr, arena));
            then_expr = coerce_node_width(arena, then_expr, Some(width), arms_signed).ok()?;
            else_expr = coerce_node_width(arena, else_expr, Some(width), arms_signed).ok()?;
            Some((
                arena
                    .alloc(SLTNode::Mux {
                        cond: condition,
                        then_expr,
                        else_expr,
                    })
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Call { .. } => None,
    }
}

fn lower_dynamic_array_element_index_glue(
    offset: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<GlueAddr>,
    element_width: usize,
) -> Option<(NodeId, HashSet<VarAtomBase<GlueAddr>>, Vec<SourceVarId>)> {
    let offset_expr = expr_from_const_expr(offset)?;
    let (offset, sources, source_ids) = lower_glue_parent_expr(
        &offset_expr,
        variables,
        name_to_id,
        constants,
        parameter_types,
        arena,
        None,
        None,
    )?;
    let element_index = if element_width == 1 {
        offset
    } else {
        let divisor = arena
            .alloc(SLTNode::Constant(
                BigUint::from(element_width),
                BigUint::default(),
                64,
                false,
            ))
            .ok()?;
        arena
            .alloc(SLTNode::Binary(offset, BinaryOp::DivU, divisor))
            .ok()?
    };
    Some((element_index, sources, source_ids))
}

fn next_var_id(next_id: &mut SourceVarId) -> SourceVarId {
    let id = *next_id;
    next_id.0 += 1;
    id
}

fn signal_kind_from_port_direction(
    direction: sv::ir::PortDirection,
) -> Result<VariableKind, sv::AnalyzerError> {
    Ok(match direction {
        sv::ir::PortDirection::Input => VariableKind::Input,
        sv::ir::PortDirection::Output => VariableKind::Output,
        sv::ir::PortDirection::Inout => VariableKind::Inout,
        sv::ir::PortDirection::Ref => {
            return Err(sv::AnalyzerError::Unsupported(
                "ref port direction".to_string(),
            ));
        }
        sv::ir::PortDirection::Unspecified => VariableKind::Variable,
    })
}

struct SvSignalType {
    width: usize,
    signed: bool,
    is_4state: bool,
    packed_ranges: Vec<(i128, i128)>,
    array_dims: Vec<usize>,
    type_kind: PortTypeKind,
}

fn signal_type_from_sv(
    typ: &sv::ir::Type,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Result<SvSignalType, sv::AnalyzerError> {
    let packed_width = if typ.packed_ranges().is_empty() {
        1
    } else {
        typ.packed_ranges()
            .iter()
            .try_fold(1usize, |acc, range| {
                let left = sv::typecheck::eval_const_expr_with_types(
                    range.left(),
                    constants,
                    parameter_types,
                )?;
                let right = sv::typecheck::eval_const_expr_with_types(
                    range.right(),
                    constants,
                    parameter_types,
                )?;
                let width = usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?;
                acc.checked_mul(width)
            })
            .or_else(|| typ.resolved_width())
            .ok_or_else(|| {
                sv::AnalyzerError::Unsupported("unresolved explicit packed width".to_string())
            })?
            .max(1)
    };
    let array_dims = typ
        .unpacked_ranges()
        .iter()
        .map(|range| {
            let left = sv::typecheck::eval_const_expr_with_types(
                range.left(),
                constants,
                parameter_types,
            )?;
            let right = sv::typecheck::eval_const_expr_with_types(
                range.right(),
                constants,
                parameter_types,
            )?;
            usize::try_from(left.abs_diff(right))
                .ok()
                .and_then(|width| width.checked_add(1))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            sv::AnalyzerError::Unsupported("unresolved unpacked array dimension".to_string())
        })?;
    let element_count = array_dims
        .iter()
        .copied()
        .try_fold(1usize, usize::checked_mul)
        .ok_or_else(|| sv::AnalyzerError::Unsupported("signal width overflow".to_string()))?;
    let width = packed_width
        .checked_mul(element_count)
        .ok_or_else(|| sv::AnalyzerError::Unsupported("signal width overflow".to_string()))?;
    let signed = typ.is_signed();
    let is_4state = !matches!(typ.kind(), sv::ir::TypeKind::Bit);
    let packed_ranges = typ
        .packed_ranges()
        .iter()
        .filter_map(|range| {
            let left = sv::typecheck::eval_const_expr_with_types(
                range.left(),
                constants,
                parameter_types,
            )?;
            let right = sv::typecheck::eval_const_expr_with_types(
                range.right(),
                constants,
                parameter_types,
            )?;
            Some((left, right))
        })
        .collect();
    let type_kind = match typ.kind() {
        sv::ir::TypeKind::Bit => PortTypeKind::Bit,
        sv::ir::TypeKind::Logic | sv::ir::TypeKind::Reg | sv::ir::TypeKind::Implicit => {
            PortTypeKind::Logic
        }
    };
    Ok(SvSignalType {
        width,
        signed,
        is_4state,
        packed_ranges,
        array_dims,
        type_kind,
    })
}

struct PreviousArrayValue {
    expr: NodeId,
    sources: HashSet<VarAtomBase<SourceVarId>>,
    previous_sources: HashSet<VarAtomBase<SourceVarId>>,
    address_sources: HashSet<VarAtomBase<SourceVarId>>,
}

fn lower_previous_array_value(
    id: SourceVarId,
    width: usize,
    paths: &[LogicPath<SourceVarId>],
) -> Result<Option<PreviousArrayValue>, sv::AnalyzerError> {
    let mut matching = paths
        .iter()
        .filter(|path| path.target.var().is_some_and(|target| target.id == id));
    let Some(path) = matching.next() else {
        return Ok(None);
    };
    let Some(target) = path.target.var() else {
        unreachable!("matching path must have a variable target");
    };
    if target.access
        != BitAccess::new(
            0,
            width.checked_sub(1).ok_or_else(|| {
                sv::AnalyzerError::Unsupported("zero-width unpacked array".to_string())
            })?,
        )
    {
        return Err(sv::AnalyzerError::Unsupported(
            "dynamic unpacked-array assignment after an earlier partial assignment to the same array is unsupported"
                .to_string(),
        ));
    }
    Ok(Some(PreviousArrayValue {
        expr: path.expr,
        sources: path.sources.clone(),
        previous_sources: path.previous_sources.clone(),
        address_sources: path.address_sources.clone(),
    }))
}

fn lower_comb_process(
    process: &sv::ir::CombProcess,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<SourceVarId>,
    four_state: bool,
) -> Result<Vec<LogicPath<SourceVarId>>, sv::AnalyzerError> {
    let assignments = process.assignments();
    if process.kind() == sv::ir::CombProcessKind::AlwaysComb {
        for (index, assignment) in assignments.iter().enumerate() {
            if assignments[index + 1..].iter().any(|later| {
                later.lhs_value() != assignment.lhs_value()
                    && expr_references_ident(assignment.rhs(), later.lhs())
            }) {
                return Err(sv::AnalyzerError::Unsupported(
                    "read-before-write dependency inside always_comb".to_string(),
                ));
            }
            for later_index in index + 1..assignments.len() {
                if assignments[later_index].lhs() != assignment.lhs() {
                    continue;
                }
                if assignments[index + 1..=later_index]
                    .iter()
                    .any(|later| expr_references_ident(later.rhs(), assignment.lhs()))
                {
                    return Err(sv::AnalyzerError::Unsupported(
                        "dependent repeated assignment inside always_comb".to_string(),
                    ));
                }
            }
        }
    }
    let mut paths = Vec::new();
    for (index, assignment) in assignments.iter().enumerate() {
        if process.kind() == sv::ir::CombProcessKind::AlwaysComb
            && assignments[index + 1..]
                .iter()
                .any(|later| later.lhs_value() == assignment.lhs_value())
        {
            continue;
        }
        let allow_dynamic_array_write = process.kind() == sv::ir::CombProcessKind::AlwaysComb;
        let previous_array = if allow_dynamic_array_write {
            if let Some((id, _, _, _)) = dynamic_array_element_lvalue(
                assignment.lhs_value(),
                variables,
                name_to_id,
                constants,
                parameter_types,
            ) {
                let width = variables
                    .get(&id)
                    .map(|variable| variable.width)
                    .ok_or_else(|| {
                        sv::AnalyzerError::Unsupported(
                            "dynamic unpacked-array assignment target".to_string(),
                        )
                    })?;
                lower_previous_array_value(id, width, &paths)?
            } else {
                None
            }
        } else {
            None
        };
        let path = lower_assignment(
            assignment,
            variables,
            name_to_id,
            constants,
            parameter_types,
            arena,
            four_state,
            allow_dynamic_array_write,
            previous_array.as_ref(),
        )?;
        merge_overlapping_comb_path(&mut paths, path, arena)?;
    }
    Ok(paths)
}

fn merge_overlapping_comb_path(
    paths: &mut Vec<LogicPath<SourceVarId>>,
    mut later: LogicPath<SourceVarId>,
    arena: &mut SLTNodeArena<SourceVarId>,
) -> Result<(), sv::AnalyzerError> {
    let mut index = 0;
    while index < paths.len() {
        let Some(previous_target) = paths[index].target.var() else {
            index += 1;
            continue;
        };
        let Some(later_target) = later.target.var() else {
            break;
        };
        if previous_target.id != later_target.id
            || !previous_target.access.overlaps(&later_target.access)
        {
            index += 1;
            continue;
        }
        let previous = paths.remove(index);
        later = overlay_comb_paths(previous, later, arena)?;
    }
    paths.push(later);
    Ok(())
}

fn overlay_comb_paths(
    previous: LogicPath<SourceVarId>,
    later: LogicPath<SourceVarId>,
    arena: &mut SLTNodeArena<SourceVarId>,
) -> Result<LogicPath<SourceVarId>, sv::AnalyzerError> {
    let previous_target = previous.target.var().expect("variable path target");
    let later_target = later.target.var().expect("variable path target");
    debug_assert_eq!(previous_target.id, later_target.id);
    debug_assert!(previous_target.access.overlaps(&later_target.access));

    let access = BitAccess::new(
        previous_target.access.lsb.min(later_target.access.lsb),
        previous_target.access.msb.max(later_target.access.msb),
    );
    let end = access.msb.checked_add(1).ok_or_else(|| {
        sv::AnalyzerError::Unsupported("overlapping always_comb assignment width".to_string())
    })?;
    let previous_end = previous_target.access.msb.checked_add(1).ok_or_else(|| {
        sv::AnalyzerError::Unsupported("overlapping always_comb assignment width".to_string())
    })?;
    let later_end = later_target.access.msb.checked_add(1).ok_or_else(|| {
        sv::AnalyzerError::Unsupported("overlapping always_comb assignment width".to_string())
    })?;
    let mut boundaries = vec![
        access.lsb,
        end,
        previous_target.access.lsb,
        previous_end,
        later_target.access.lsb,
        later_end,
    ];
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut nodes = Vec::new();
    let mut uses_previous = false;
    let mut uses_later = false;
    for bounds in boundaries.windows(2).rev() {
        let segment = BitAccess::new(bounds[0], bounds[1] - 1);
        let (path, target) =
            if later_target.access.lsb <= segment.lsb && segment.msb <= later_target.access.msb {
                uses_later = true;
                (&later, later_target)
            } else {
                uses_previous = true;
                (&previous, previous_target)
            };
        let relative = BitAccess::new(
            segment.lsb - target.access.lsb,
            segment.msb - target.access.lsb,
        );
        let node = if relative == BitAccess::new(0, target.access.msb - target.access.lsb) {
            path.expr
        } else {
            arena
                .alloc(SLTNode::Slice {
                    expr: path.expr,
                    access: relative,
                })
                .map_err(|error| {
                    sv::AnalyzerError::Unsupported(format!(
                        "overlapping always_comb assignment: {error}"
                    ))
                })?
        };
        nodes.push((node, segment.msb - segment.lsb + 1));
    }
    let expr = if nodes.len() == 1 {
        nodes[0].0
    } else {
        arena.alloc(SLTNode::Concat(nodes)).map_err(|error| {
            sv::AnalyzerError::Unsupported(format!("overlapping always_comb assignment: {error}"))
        })?
    };
    let mut sources = HashSet::default();
    if uses_previous {
        sources.extend(previous.sources);
    }
    if uses_later {
        sources.extend(later.sources);
    }
    let mut previous_sources = HashSet::default();
    if uses_previous {
        previous_sources.extend(previous.previous_sources);
    }
    if uses_later {
        previous_sources.extend(later.previous_sources);
    }
    let mut address_sources = HashSet::default();
    if uses_previous {
        address_sources.extend(previous.address_sources);
    }
    if uses_later {
        address_sources.extend(later.address_sources);
    }
    Ok(LogicPath {
        target: LogicPathTarget::Var(VarAtomBase::new(previous_target.id, access.lsb, access.msb)),
        expr,
        sources,
        address_sources,
        previous_sources,
        local_inputs: Vec::new(),
        order_before: HashSet::default(),
        comb_capture_enable_sites: Vec::new(),
        comb_capture_enable_always: false,
        pre_lower_nodes: Vec::new(),
    })
}

fn expr_references_ident(expr: &sv::ir::Expr, name: &str) -> bool {
    match expr {
        sv::ir::Expr::Ident(ident) => ident == name,
        sv::ir::Expr::Literal(_) => false,
        sv::ir::Expr::Select { expr, .. }
        | sv::ir::Expr::Resize { expr, .. }
        | sv::ir::Expr::Unary { expr, .. } => expr_references_ident(expr, name),
        sv::ir::Expr::Concat(parts) | sv::ir::Expr::RepeatConcat { parts, .. } => {
            parts.iter().any(|part| expr_references_ident(part, name))
        }
        sv::ir::Expr::Binary { left, right, .. } => {
            expr_references_ident(left, name) || expr_references_ident(right, name)
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_references_ident(condition, name)
                || expr_references_ident(then_expr, name)
                || expr_references_ident(else_expr, name)
        }
        sv::ir::Expr::Call { args, .. } => args.iter().any(|arg| expr_references_ident(arg, name)),
    }
}

fn lower_dynamic_array_write_expr(
    lvalue: &sv::ir::LValue,
    rhs: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<SourceVarId>,
    previous_array: Option<&PreviousArrayValue>,
) -> Option<(
    LogicPathTarget<SourceVarId>,
    celox_slt::NodeId,
    HashSet<VarAtomBase<SourceVarId>>,
    HashSet<VarAtomBase<SourceVarId>>,
)> {
    let (id, element_width, offset, access) =
        dynamic_array_element_lvalue(lvalue, variables, name_to_id, constants, parameter_types)?;
    let variable = variables.get(&id)?;
    let element_count = variable.width.checked_div(element_width)?;
    if element_count == 0 {
        return None;
    }
    let array_width = variable.width;
    let target_width = access.msb - access.lsb + 1;
    let (rhs_node, mut sources) = if let sv::ir::Expr::Literal(literal) = rhs
        && let Some(fill) = unbased_fill_literal(literal)
    {
        (
            lower_unbased_fill_literal_slt(arena, fill, target_width)?,
            HashSet::default(),
        )
    } else {
        lower_expr_with_context(
            rhs,
            variables,
            name_to_id,
            constants,
            parameter_types,
            arena,
            Some(target_width),
            Some(sv_expr_is_signed_with_parameters(
                rhs,
                variables,
                name_to_id,
                parameter_types,
            )),
        )?
    };
    let rhs_node = coerce_node_width(
        arena,
        rhs_node,
        Some(target_width),
        sv_expr_is_signed_with_parameters(rhs, variables, name_to_id, parameter_types),
    )
    .ok()?;
    let (element_index, index_sources) = lower_dynamic_array_element_index_slt(
        &offset,
        variables,
        name_to_id,
        constants,
        parameter_types,
        arena,
        element_width,
    )?;
    sources.extend(index_sources);
    let (old, previous_sources) = if let Some(previous_array) = previous_array {
        sources.extend(previous_array.sources.iter().copied());
        sources.extend(previous_array.address_sources.iter().copied());
        (previous_array.expr, previous_array.previous_sources.clone())
    } else {
        let previous_sources = [VarAtomBase::new(id, 0, array_width.checked_sub(1)?)]
            .into_iter()
            .collect();
        let old = arena
            .alloc(SLTNode::Input {
                variable: id,
                signed: variable.signed,
                index: Vec::new(),
                access: BitAccess::new(0, array_width - 1),
            })
            .ok()?;
        (old, previous_sources)
    };
    let mut parts = Vec::with_capacity(element_count);
    for element in (0..element_count).rev() {
        let lsb = element.checked_mul(element_width)?;
        let old_element = arena
            .alloc(SLTNode::Slice {
                expr: old,
                access: BitAccess::new(lsb, lsb + element_width - 1),
            })
            .ok()?;
        let element_literal = arena
            .alloc(SLTNode::Constant(
                BigUint::from(element),
                BigUint::default(),
                64,
                false,
            ))
            .ok()?;
        let condition = arena
            .alloc(SLTNode::Binary(
                element_index,
                BinaryOp::EqCase,
                element_literal,
            ))
            .ok()?;
        let updated_element = replace_slt_slice(
            arena,
            old_element,
            rhs_node,
            access.lsb,
            target_width,
            element_width,
        )?;
        let updated = arena
            .alloc(SLTNode::Mux {
                cond: condition,
                then_expr: updated_element,
                else_expr: old_element,
            })
            .ok()?;
        parts.push((updated, element_width));
    }
    let expr = if parts.len() == 1 {
        parts[0].0
    } else {
        arena.alloc(SLTNode::Concat(parts)).ok()?
    };
    Some((
        LogicPathTarget::Var(VarAtomBase::new(id, 0, array_width - 1)),
        expr,
        sources,
        previous_sources,
    ))
}

fn replace_slt_slice<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    current: NodeId,
    replacement: NodeId,
    lsb: usize,
    replacement_width: usize,
    total_width: usize,
) -> Option<NodeId> {
    if lsb == 0 && replacement_width == total_width {
        return Some(replacement);
    }
    let end = lsb.checked_add(replacement_width)?;
    if end > total_width {
        return None;
    }

    let mut parts = Vec::with_capacity(3);
    if end < total_width {
        let upper_width = total_width - end;
        let upper = arena
            .alloc(SLTNode::Slice {
                expr: current,
                access: BitAccess::new(end, total_width - 1),
            })
            .ok()?;
        parts.push((upper, upper_width));
    }
    parts.push((replacement, replacement_width));
    if lsb != 0 {
        let lower = arena
            .alloc(SLTNode::Slice {
                expr: current,
                access: BitAccess::new(0, lsb - 1),
            })
            .ok()?;
        parts.push((lower, lsb));
    }
    arena.alloc(SLTNode::Concat(parts)).ok()
}

fn lower_assignment(
    assignment: &sv::ir::Assignment,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<SourceVarId>,
    four_state: bool,
    allow_dynamic_array_write: bool,
    previous_array: Option<&PreviousArrayValue>,
) -> Result<LogicPath<SourceVarId>, sv::AnalyzerError> {
    let rhs = expr_for_state_mode(assignment.rhs(), four_state);
    if allow_dynamic_array_write
        && let Some((target, expr, sources, previous_sources)) = lower_dynamic_array_write_expr(
            assignment.lhs_value(),
            &rhs,
            variables,
            name_to_id,
            constants,
            parameter_types,
            arena,
            previous_array,
        )
    {
        let target_width = target
            .var()
            .map(|target| target.access.msb - target.access.lsb + 1)
            .ok_or_else(|| {
                sv::AnalyzerError::Unsupported(format!(
                    "combinational assignment target `{}`",
                    assignment.lhs()
                ))
            })?;
        let mut expr = coerce_node_width(
            arena,
            expr,
            Some(target_width),
            sv_expr_is_signed_with_parameters(&rhs, variables, name_to_id, parameter_types),
        )
        .map_err(|error| {
            sv::AnalyzerError::Unsupported(format!(
                "combinational assignment width coercion for `{}`: {error}",
                assignment.lhs()
            ))
        })?;
        let target_is_two_state = target
            .var()
            .and_then(|target| variables.get(&target.id))
            .is_some_and(|variable| !variable.is_4state);
        if target_is_two_state || (!four_state && expr_is_unknown_literal(&rhs)) {
            expr = arena
                .alloc(SLTNode::Unary(UnaryOp::ToTwoState, expr))
                .map_err(|error| {
                    sv::AnalyzerError::Unsupported(format!(
                        "two-state conversion for `{}`: {error}",
                        assignment.lhs()
                    ))
                })?;
        }
        return Ok(LogicPath {
            target,
            expr,
            sources,
            address_sources: HashSet::default(),
            previous_sources,
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
        });
    }
    let target = lower_lvalue_target(
        assignment.lhs_value(),
        variables,
        name_to_id,
        constants,
        parameter_types,
    )
    .ok_or_else(|| {
        sv::AnalyzerError::Unsupported(format!(
            "combinational assignment target `{}`",
            assignment.lhs()
        ))
    })?;
    let target_width = target
        .var()
        .map(|target| target.access.msb - target.access.lsb + 1)
        .ok_or_else(|| {
            sv::AnalyzerError::Unsupported(format!(
                "combinational assignment target `{}`",
                assignment.lhs()
            ))
        })?;
    let (expr, sources) = if let sv::ir::Expr::Literal(literal) = &rhs
        && let Some(fill) = unbased_fill_literal(literal)
    {
        (
            lower_unbased_fill_literal_slt(arena, fill, target_width).ok_or_else(|| {
                sv::AnalyzerError::Unsupported(format!("combinational expression `{literal}`"))
            })?,
            HashSet::default(),
        )
    } else {
        lower_expr_with_context(
            &rhs,
            variables,
            name_to_id,
            constants,
            parameter_types,
            arena,
            Some(target_width),
            Some(sv_expr_is_signed_with_parameters(
                &rhs,
                variables,
                name_to_id,
                parameter_types,
            )),
        )
        .ok_or_else(|| {
            sv::AnalyzerError::Unsupported(format!(
                "combinational expression assigned to `{}`",
                assignment.lhs()
            ))
        })?
    };
    let mut expr = coerce_node_width(
        arena,
        expr,
        Some(target_width),
        sv_expr_is_signed_with_parameters(&rhs, variables, name_to_id, parameter_types),
    )
    .map_err(|error| {
        sv::AnalyzerError::Unsupported(format!(
            "combinational assignment width coercion for `{}`: {error}",
            assignment.lhs()
        ))
    })?;
    let target_is_two_state = target
        .var()
        .and_then(|target| variables.get(&target.id))
        .is_some_and(|variable| !variable.is_4state);
    if target_is_two_state || (!four_state && expr_is_unknown_literal(&rhs)) {
        expr = arena
            .alloc(SLTNode::Unary(UnaryOp::ToTwoState, expr))
            .map_err(|error| {
                sv::AnalyzerError::Unsupported(format!(
                    "two-state conversion for `{}`: {error}",
                    assignment.lhs()
                ))
            })?;
    }
    Ok(LogicPath {
        target,
        expr,
        sources,
        address_sources: HashSet::default(),
        previous_sources: HashSet::default(),
        local_inputs: Vec::new(),
        order_before: HashSet::default(),
        comb_capture_enable_sites: Vec::new(),
        comb_capture_enable_always: false,
        pre_lower_nodes: Vec::new(),
    })
}

fn lower_lvalue_target(
    lvalue: &sv::ir::LValue,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<LogicPathTarget<SourceVarId>> {
    let target_id = *name_to_id.get(lvalue.name())?;
    let target_width = variables.get(&target_id)?.width;
    let (lsb, msb) = match lvalue {
        sv::ir::LValue::Ident(_) => (0, target_width.checked_sub(1)?),
        sv::ir::LValue::Select { msb, lsb, .. } => {
            let msb = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
            let lsb = sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
            let variable = variables.get(&target_id)?;
            let msb = packed_index_offset(variable, msb)?;
            let lsb = packed_index_offset(variable, lsb)?;
            (lsb.min(msb), lsb.max(msb))
        }
    };
    (lsb <= msb && msb < target_width)
        .then(|| LogicPathTarget::Var(VarAtomBase::new(target_id, lsb, msb)))
}

fn lower_expr(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<SourceVarId>,
) -> Option<(celox_slt::NodeId, HashSet<VarAtomBase<SourceVarId>>)> {
    lower_expr_with_context(
        expr,
        variables,
        name_to_id,
        constants,
        parameter_types,
        arena,
        None,
        None,
    )
}

fn lower_expr_with_context(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<SourceVarId>,
    context_width: Option<usize>,
    context_signed: Option<bool>,
) -> Option<(celox_slt::NodeId, HashSet<VarAtomBase<SourceVarId>>)> {
    match expr {
        sv::ir::Expr::Ident(name) => {
            let Some(id) = name_to_id.get(name).copied() else {
                let value = constants.get(name)?;
                let (width, signed) = parameter_types.get(name).copied().unwrap_or((32, false));
                let node = arena
                    .alloc(SLTNode::Constant(
                        parameter_value_bits(*value, width),
                        BigUint::from(0u32),
                        width,
                        signed,
                    ))
                    .ok()?;
                return Some((
                    coerce_node_width(arena, node, context_width, context_signed.unwrap_or(signed))
                        .ok()?,
                    HashSet::default(),
                ));
            };
            let var = variables.get(&id)?;
            let width = var.width;
            let node = arena
                .alloc(SLTNode::Input {
                    variable: id,
                    signed: var.signed,
                    index: Vec::new(),
                    access: BitAccess::new(0, width - 1),
                })
                .ok()?;
            let mut sources = HashSet::default();
            sources.insert(VarAtomBase::new(id, 0, width - 1));
            Some((
                coerce_node_width(
                    arena,
                    node,
                    context_width,
                    context_signed.unwrap_or(var.signed),
                )
                .ok()?,
                sources,
            ))
        }
        sv::ir::Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => {
            if let Some((id, element_width, access)) = dynamic_array_element_subselection(
                expr,
                msb,
                lsb,
                variables,
                name_to_id,
                constants,
                parameter_types,
            ) {
                let (offset, mut sources) = lower_dynamic_array_element_index_slt(
                    lsb,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    element_width,
                )?;
                let variable = variables.get(&id)?;
                let element_count = variable.width.checked_div(element_width)?;
                let (offset, valid) = dynamic_array_index_guard_slt(arena, offset, element_count)?;
                let node = arena
                    .alloc(SLTNode::Input {
                        variable: id,
                        signed: *signed,
                        index: vec![SLTIndex {
                            node: offset,
                            stride: element_width,
                            kind: SLTIndexKind::Unpacked { element_width },
                        }],
                        access,
                    })
                    .ok()?;
                let node = guard_dynamic_array_read_slt(
                    arena,
                    valid,
                    node,
                    access.msb - access.lsb + 1,
                    variable.is_4state,
                )?;
                sources.insert(VarAtomBase::new(id, 0, variable.width.checked_sub(1)?));
                return Some((
                    coerce_node_width(
                        arena,
                        node,
                        context_width,
                        context_signed.unwrap_or(*signed),
                    )
                    .ok()?,
                    sources,
                ));
            }
            let (inner, mut sources) = lower_expr(
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
            )?;
            let msb_value =
                sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
            let lsb_value =
                sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
            let (msb, lsb) = if let sv::ir::Expr::Ident(name) = &**expr {
                let variable = name_to_id.get(name).and_then(|id| variables.get(id))?;
                (
                    packed_index_offset(variable, msb_value)?,
                    packed_index_offset(variable, lsb_value)?,
                )
            } else {
                (
                    usize::try_from(msb_value).ok()?,
                    usize::try_from(lsb_value).ok()?,
                )
            };
            let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
            let node = arena
                .alloc(SLTNode::Slice {
                    expr: inner,
                    access,
                })
                .ok()?;
            sources = select_sources(expr, sources, access)?;
            Some((
                coerce_node_width(
                    arena,
                    node,
                    context_width,
                    context_signed.unwrap_or(*signed),
                )
                .ok()?,
                sources,
            ))
        }
        sv::ir::Expr::Concat(parts) => {
            let mut nodes = Vec::new();
            let mut sources = HashSet::default();
            for part in parts {
                let (node, part_sources) = lower_expr_with_context(
                    part,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    expr_unbased_fill_literal(part).map(|_| 1),
                    None,
                )?;
                let width = celox_slt::get_width(node, arena);
                nodes.push((node, width));
                sources.extend(part_sources);
            }
            Some((arena.alloc(SLTNode::Concat(nodes)).ok()?, sources))
        }
        sv::ir::Expr::RepeatConcat { count, parts } => {
            let count =
                sv::typecheck::eval_const_expr_with_types(count, constants, parameter_types)?;
            let count = usize::try_from(count).ok()?;
            let mut repeated = Vec::new();
            let mut sources = HashSet::default();
            for _ in 0..count {
                for part in parts {
                    let (node, part_sources) = lower_expr_with_context(
                        part,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        expr_unbased_fill_literal(part).map(|_| 1),
                        None,
                    )?;
                    let width = celox_slt::get_width(node, arena);
                    repeated.push((node, width));
                    sources.extend(part_sources);
                }
            }
            Some((arena.alloc(SLTNode::Concat(repeated)).ok()?, sources))
        }
        sv::ir::Expr::Literal(literal) => {
            if let Some(width) = context_width
                && let Some(fill) = unbased_fill_literal(literal)
            {
                return Some((
                    lower_unbased_fill_literal_slt(arena, fill, width)?,
                    HashSet::default(),
                ));
            }
            let literal = sv::typecheck::parse_integral_literal(literal)?;
            let signed = literal.signed;
            let node = arena
                .alloc(SLTNode::Constant(
                    literal.value,
                    literal.mask,
                    literal.width,
                    signed,
                ))
                .ok()?;
            Some((
                coerce_node_width(arena, node, context_width, context_signed.unwrap_or(signed))
                    .ok()?,
                HashSet::default(),
            ))
        }
        sv::ir::Expr::Unary { op, expr } => {
            let one_bit_result = matches!(
                op,
                sv::ir::UnaryOp::LogicNot
                    | sv::ir::UnaryOp::RedAnd
                    | sv::ir::UnaryOp::RedOr
                    | sv::ir::UnaryOp::RedXor
            );
            let operand_context = (!one_bit_result).then_some(context_width).flatten();
            let (inner, sources) = lower_expr_with_context(
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                operand_context,
                context_signed,
            )?;
            Some((
                arena
                    .alloc(SLTNode::Unary(unary_op_from_sv(*op)?, inner))
                    .ok()?,
                sources,
            ))
        }
        sv::ir::Expr::Resize {
            expr,
            width,
            signed,
        } => {
            let (inner, sources) = lower_expr_with_context(
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                Some(*width),
                Some(*signed),
            )?;
            let resized = coerce_node_width(arena, inner, Some(*width), *signed).ok()?;
            Some((
                coerce_node_width(
                    arena,
                    resized,
                    context_width,
                    context_signed.unwrap_or(*signed),
                )
                .ok()?,
                sources,
            ))
        }
        sv::ir::Expr::Binary { left, op, right } => {
            let left_signed =
                sv_expr_is_signed_with_parameters(left, variables, name_to_id, parameter_types);
            let operands_signed = left_signed
                && sv_expr_is_signed_with_parameters(right, variables, name_to_id, parameter_types);
            let operator_signed = if matches!(op, sv::ir::BinaryOp::Sar) {
                left_signed
            } else {
                operands_signed
            };
            let comparison = matches!(
                op,
                sv::ir::BinaryOp::Eq
                    | sv::ir::BinaryOp::Ne
                    | sv::ir::BinaryOp::EqCase
                    | sv::ir::BinaryOp::NeCase
                    | sv::ir::BinaryOp::EqWildcard
                    | sv::ir::BinaryOp::NeWildcard
                    | sv::ir::BinaryOp::Lt
                    | sv::ir::BinaryOp::Le
                    | sv::ir::BinaryOp::Gt
                    | sv::ir::BinaryOp::Ge
            );
            let shift = matches!(
                op,
                sv::ir::BinaryOp::Shl | sv::ir::BinaryOp::Shr | sv::ir::BinaryOp::Sar
            );
            let context_determined = !comparison
                && !matches!(op, sv::ir::BinaryOp::LogicAnd | sv::ir::BinaryOp::LogicOr);
            let operation_context = context_width.map(|context_width| {
                context_width.max(
                    sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)
                        .unwrap_or(context_width),
                )
            });
            let comparison_context = comparison
                .then(|| {
                    sv_comparison_operand_width(
                        left,
                        right,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                    )
                })
                .flatten();
            let left_context = if comparison {
                comparison_context
            } else {
                context_determined.then_some(operation_context).flatten()
            };
            let right_context = if comparison {
                comparison_context
            } else {
                (context_determined && !shift)
                    .then_some(operation_context)
                    .flatten()
            };
            let left_fill = (comparison || shift)
                .then(|| expr_unbased_fill_literal(left))
                .flatten();
            let right_fill = (comparison || shift)
                .then(|| expr_unbased_fill_literal(right))
                .flatten();
            let ((mut left, mut sources), (mut right, right_sources)) =
                match (left_fill, right_fill) {
                    (Some(left_fill), Some(right_fill)) => {
                        let left_width = if shift { left_context.unwrap_or(1) } else { 1 };
                        (
                            (
                                lower_unbased_fill_literal_slt(arena, left_fill, left_width)?,
                                HashSet::default(),
                            ),
                            (
                                lower_unbased_fill_literal_slt(arena, right_fill, 1)?,
                                HashSet::default(),
                            ),
                        )
                    }
                    (Some(fill), None) => {
                        let right = lower_expr_with_context(
                            right,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                            arena,
                            right_context,
                            Some(operands_signed),
                        )?;
                        let width = if shift {
                            left_context.unwrap_or(1)
                        } else {
                            celox_slt::get_width(right.0, arena)
                        };
                        (
                            (
                                lower_unbased_fill_literal_slt(arena, fill, width)?,
                                HashSet::default(),
                            ),
                            right,
                        )
                    }
                    (None, Some(fill)) => {
                        let left = lower_expr_with_context(
                            left,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                            arena,
                            left_context,
                            Some(if shift { left_signed } else { operands_signed }),
                        )?;
                        let width = if shift {
                            1
                        } else {
                            celox_slt::get_width(left.0, arena)
                        };
                        (
                            left,
                            (
                                lower_unbased_fill_literal_slt(arena, fill, width)?,
                                HashSet::default(),
                            ),
                        )
                    }
                    (None, None) => (
                        lower_expr_with_context(
                            left,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                            arena,
                            left_context,
                            Some(if shift { left_signed } else { operands_signed }),
                        )?,
                        lower_expr_with_context(
                            right,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                            arena,
                            right_context,
                            Some(operands_signed),
                        )?,
                    ),
                };
            sources.extend(right_sources);
            if comparison {
                let common_width =
                    celox_slt::get_width(left, arena).max(celox_slt::get_width(right, arena));
                left = coerce_node_width(arena, left, Some(common_width), operands_signed).ok()?;
                right =
                    coerce_node_width(arena, right, Some(common_width), operands_signed).ok()?;
            }
            Some((
                arena
                    .alloc(SLTNode::Binary(
                        left,
                        binary_op_from_sv(*op, operator_signed),
                        right,
                    ))
                    .ok()?,
                sources,
            ))
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let arms_signed = sv_expr_is_signed_with_parameters(
                then_expr,
                variables,
                name_to_id,
                parameter_types,
            ) && sv_expr_is_signed_with_parameters(
                else_expr,
                variables,
                name_to_id,
                parameter_types,
            );
            let arm_context =
                sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)
                    .map(|natural_width| {
                        context_width.map_or(natural_width, |width| width.max(natural_width))
                    })
                    .or(context_width);
            let (condition, mut sources) = lower_expr(
                condition,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
            )?;
            let (mut then_expr, then_sources) = lower_expr_with_context(
                then_expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                arm_context,
                Some(arms_signed),
            )?;
            let (mut else_expr, else_sources) = lower_expr_with_context(
                else_expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                arm_context,
                Some(arms_signed),
            )?;
            sources.extend(then_sources);
            sources.extend(else_sources);
            let width =
                celox_slt::get_width(then_expr, arena).max(celox_slt::get_width(else_expr, arena));
            then_expr = coerce_node_width(arena, then_expr, Some(width), arms_signed).ok()?;
            else_expr = coerce_node_width(arena, else_expr, Some(width), arms_signed).ok()?;
            Some((
                arena
                    .alloc(SLTNode::Mux {
                        cond: condition,
                        then_expr,
                        else_expr,
                    })
                    .ok()?,
                sources,
            ))
        }
        sv::ir::Expr::Call { .. } => None,
    }
}

fn select_can_narrow_source_ranges(expr: &sv::ir::Expr) -> bool {
    match expr {
        sv::ir::Expr::Ident(_) => true,
        sv::ir::Expr::Select { expr, .. } => select_can_narrow_source_ranges(expr),
        _ => false,
    }
}

fn select_sources<A: std::hash::Hash + Eq + Clone>(
    expr: &sv::ir::Expr,
    sources: HashSet<VarAtomBase<A>>,
    access: BitAccess,
) -> Option<HashSet<VarAtomBase<A>>> {
    if !select_can_narrow_source_ranges(expr) {
        return Some(sources);
    }
    sources
        .into_iter()
        .map(|source| {
            Some(VarAtomBase::new(
                source.id,
                source.access.lsb.checked_add(access.lsb)?,
                source.access.lsb.checked_add(access.msb)?,
            ))
        })
        .collect()
}

fn packed_index_offset(variable: &SvVariable, index: i128) -> Option<usize> {
    if !variable.array_dims.is_empty() {
        return usize::try_from(index)
            .ok()
            .filter(|offset| *offset < variable.width);
    }
    let offset = match variable.packed_ranges.as_slice() {
        [(left, right)] if left >= right => index.checked_sub(*right)?,
        [(_, right)] => right.checked_sub(index)?,
        _ => index,
    };
    usize::try_from(offset)
        .ok()
        .filter(|offset| *offset < variable.width)
}

fn unpacked_element_width(variable: &SvVariable) -> Option<usize> {
    if variable.array_dims.is_empty() {
        return None;
    }
    let element_count = variable
        .array_dims
        .iter()
        .copied()
        .try_fold(1usize, usize::checked_mul)?;
    (element_count != 0).then(|| variable.width.checked_div(element_count))?
}

fn sv_memory_offset(variable: &SvVariable, bit_offset: usize, width: usize) -> SIROffset {
    match unpacked_element_width(variable) {
        Some(element_width)
            if element_width != 0
                && width > element_width
                && bit_offset.is_multiple_of(element_width)
                && width.is_multiple_of(element_width) =>
        {
            SIROffset::PackedElements {
                bit_offset,
                element_width,
            }
        }
        _ => SIROffset::Static(bit_offset),
    }
}

fn packed_expr_select_offsets(
    expr: &sv::ir::Expr,
    msb: i128,
    lsb: i128,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
) -> Option<(usize, usize)> {
    if let sv::ir::Expr::Ident(name) = expr {
        if let Some(variable) = name_to_id.get(name).and_then(|id| variables.get(id)) {
            return Some((
                packed_index_offset(variable, msb)?,
                packed_index_offset(variable, lsb)?,
            ));
        }
    }
    Some((usize::try_from(msb).ok()?, usize::try_from(lsb).ok()?))
}

fn expr_from_const_expr(expr: &sv::ir::ConstExpr) -> Option<sv::ir::Expr> {
    Some(match expr {
        sv::ir::ConstExpr::Literal(value) => sv::ir::Expr::Literal(value.clone()),
        sv::ir::ConstExpr::Ident(name) => sv::ir::Expr::Ident(name.clone()),
        sv::ir::ConstExpr::Select { expr, bit } => sv::ir::Expr::Select {
            expr: Box::new(expr_from_const_expr(expr)?),
            msb: (**bit).clone(),
            lsb: (**bit).clone(),
            signed: false,
        },
        sv::ir::ConstExpr::Function { .. } => return None,
        sv::ir::ConstExpr::Unary { op, expr } => sv::ir::Expr::Unary {
            op: *op,
            expr: Box::new(expr_from_const_expr(expr)?),
        },
        sv::ir::ConstExpr::Binary { left, op, right } => sv::ir::Expr::Binary {
            left: Box::new(expr_from_const_expr(left)?),
            op: *op,
            right: Box::new(expr_from_const_expr(right)?),
        },
        sv::ir::ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => sv::ir::Expr::Mux {
            condition: Box::new(expr_from_const_expr(condition)?),
            then_expr: Box::new(expr_from_const_expr(then_expr)?),
            else_expr: Box::new(expr_from_const_expr(else_expr)?),
        },
    })
}

fn dynamic_array_element_subselection(
    expr: &sv::ir::Expr,
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<(SourceVarId, usize, BitAccess)> {
    let sv::ir::Expr::Ident(name) = expr else {
        return None;
    };
    let id = *name_to_id.get(name)?;
    let variable = variables.get(&id)?;
    let element_width = unpacked_element_width(variable).filter(|width| *width != 0)?;
    let is_dynamic = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)
        .is_none()
        || sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types).is_none();
    if !is_dynamic {
        return None;
    }
    let element_width_value = i128::try_from(element_width).ok()?;
    let (msb_base, msb_offset) = split_dynamic_array_offset(msb, constants, parameter_types)?;
    let (lsb_base, lsb_offset) = split_dynamic_array_offset(lsb, constants, parameter_types)?;
    if msb_base != lsb_base
        || (element_width > 1
            && !dynamic_array_base_has_stride(
                msb_base,
                element_width_value,
                constants,
                parameter_types,
            ))
    {
        return None;
    }
    let msb = usize::try_from(msb_offset).ok()?;
    let lsb = usize::try_from(lsb_offset).ok()?;
    let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
    (access.msb < element_width).then_some((id, element_width, access))
}

fn split_dynamic_array_offset<'a>(
    expr: &'a sv::ir::ConstExpr,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<(&'a sv::ir::ConstExpr, i128)> {
    if sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types).is_some() {
        return None;
    }
    if let sv::ir::ConstExpr::Binary { left, op, right } = expr
        && *op == sv::ir::BinaryOp::Add
    {
        if let Some(offset) =
            sv::typecheck::eval_const_expr_with_types(right, constants, parameter_types)
            && sv::typecheck::eval_const_expr_with_types(left, constants, parameter_types).is_none()
        {
            return Some((left, offset));
        }
        if let Some(offset) =
            sv::typecheck::eval_const_expr_with_types(left, constants, parameter_types)
            && sv::typecheck::eval_const_expr_with_types(right, constants, parameter_types)
                .is_none()
        {
            return Some((right, offset));
        }
    }
    Some((expr, 0))
}

fn dynamic_array_base_has_stride(
    expr: &sv::ir::ConstExpr,
    element_width: i128,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> bool {
    match expr {
        sv::ir::ConstExpr::Binary { left, op, right } => {
            if *op == sv::ir::BinaryOp::Mul {
                let left_value =
                    sv::typecheck::eval_const_expr_with_types(left, constants, parameter_types);
                let right_value =
                    sv::typecheck::eval_const_expr_with_types(right, constants, parameter_types);
                if left_value.is_some_and(|value| value > 0 && value % element_width == 0)
                    && right_value.is_none()
                {
                    return true;
                }
                if right_value.is_some_and(|value| value > 0 && value % element_width == 0)
                    && left_value.is_none()
                {
                    return true;
                }
            }
            dynamic_array_base_has_stride(left, element_width, constants, parameter_types)
                || dynamic_array_base_has_stride(right, element_width, constants, parameter_types)
        }
        sv::ir::ConstExpr::Mux {
            then_expr,
            else_expr,
            ..
        } => {
            dynamic_array_base_has_stride(then_expr, element_width, constants, parameter_types)
                || dynamic_array_base_has_stride(
                    else_expr,
                    element_width,
                    constants,
                    parameter_types,
                )
        }
        _ => false,
    }
}

fn dynamic_array_element_lvalue(
    lvalue: &sv::ir::LValue,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<(SourceVarId, usize, sv::ir::ConstExpr, BitAccess)> {
    let sv::ir::LValue::Select { name, msb, lsb, .. } = lvalue else {
        return None;
    };
    let id = *name_to_id.get(name)?;
    let variable = variables.get(&id)?;
    let element_width = unpacked_element_width(variable).filter(|width| *width != 0)?;
    let is_dynamic = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)
        .is_none()
        || sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types).is_none();
    if !is_dynamic {
        return None;
    }
    let (msb_base, msb_offset) = split_dynamic_array_offset(msb, constants, parameter_types)?;
    let (lsb_base, lsb_offset) = split_dynamic_array_offset(lsb, constants, parameter_types)?;
    if msb_base != lsb_base
        || (element_width > 1
            && !dynamic_array_base_has_stride(
                msb_base,
                i128::try_from(element_width).ok()?,
                constants,
                parameter_types,
            ))
    {
        return None;
    }
    let offset = lsb.clone();
    let msb = usize::try_from(msb_offset).ok()?;
    let lsb = usize::try_from(lsb_offset).ok()?;
    let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
    (access.msb < element_width).then_some((id, element_width, offset, access))
}

fn lower_dynamic_array_element_index_slt(
    offset: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<SourceVarId>,
    element_width: usize,
) -> Option<(celox_slt::NodeId, HashSet<VarAtomBase<SourceVarId>>)> {
    let offset_expr = expr_from_const_expr(offset)?;
    let (offset, sources) = lower_expr_with_context(
        &offset_expr,
        variables,
        name_to_id,
        constants,
        parameter_types,
        arena,
        None,
        None,
    )?;
    let element_index = if element_width == 1 {
        offset
    } else {
        let divisor = arena
            .alloc(SLTNode::Constant(
                BigUint::from(element_width),
                BigUint::default(),
                64,
                false,
            ))
            .ok()?;
        arena
            .alloc(SLTNode::Binary(offset, BinaryOp::DivU, divisor))
            .ok()?
    };
    Some((element_index, sources))
}

fn dynamic_array_index_guard_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    index: NodeId,
    element_count: usize,
) -> Option<(NodeId, NodeId)> {
    let element_count = BigUint::from(element_count);
    let two_state_index = arena
        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, index))
        .ok()?;
    let known = arena
        .alloc(SLTNode::Binary(index, BinaryOp::EqCase, two_state_index))
        .ok()?;
    let bound = arena
        .alloc(SLTNode::Constant(
            element_count,
            BigUint::default(),
            64,
            false,
        ))
        .ok()?;
    let in_range = arena
        .alloc(SLTNode::Binary(index, BinaryOp::LtU, bound))
        .ok()?;
    let valid = arena
        .alloc(SLTNode::Binary(known, BinaryOp::LogicAnd, in_range))
        .ok()?;
    let zero = arena
        .alloc(SLTNode::Constant(
            BigUint::default(),
            BigUint::default(),
            64,
            false,
        ))
        .ok()?;
    let safe_index = arena
        .alloc(SLTNode::Mux {
            cond: valid,
            then_expr: index,
            else_expr: zero,
        })
        .ok()?;
    Some((safe_index, valid))
}

fn guard_dynamic_array_read_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    valid: NodeId,
    value: NodeId,
    value_width: usize,
    is_4state: bool,
) -> Option<NodeId> {
    let unknown_mask = if is_4state {
        (BigUint::from(1u8) << value_width) - BigUint::from(1u8)
    } else {
        BigUint::default()
    };
    let unknown = arena
        .alloc(SLTNode::Constant(
            BigUint::default(),
            unknown_mask,
            value_width,
            false,
        ))
        .ok()?;
    arena
        .alloc(SLTNode::Mux {
            cond: valid,
            then_expr: value,
            else_expr: unknown,
        })
        .ok()
}

fn lower_dynamic_array_element_index(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    offset: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    element_width: usize,
) -> Option<celox_sir::RegisterId> {
    let offset_expr = expr_from_const_expr(offset)?;
    let offset = lower_expr_to_sir_with_context(
        builder,
        &offset_expr,
        variables,
        name_to_id,
        constants,
        parameter_types,
        None,
        None,
    )?;
    let offset = resize_sir_register(builder, offset, 64, false)?;
    if element_width == 1 {
        return Some(offset);
    }
    let divisor = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(
        divisor,
        SIRValue::new(element_width as u64),
    ));
    let index = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Binary(
        index,
        offset,
        BinaryOp::DivU,
        divisor,
    ));
    Some(index)
}

fn dynamic_array_index_guard_sir(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    index: celox_sir::RegisterId,
    element_count: usize,
) -> Option<(celox_sir::RegisterId, celox_sir::RegisterId)> {
    let element_count = u64::try_from(element_count).ok()?;
    let two_state_index = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Unary(
        two_state_index,
        UnaryOp::ToTwoState,
        index,
    ));
    let known = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Binary(
        known,
        index,
        BinaryOp::EqCase,
        two_state_index,
    ));
    let bound = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(bound, SIRValue::new(element_count)));
    let in_range = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Binary(
        in_range,
        index,
        BinaryOp::LtU,
        bound,
    ));
    let valid = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Binary(
        valid,
        known,
        BinaryOp::LogicAnd,
        in_range,
    ));
    let zero = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
    let safe_index = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Mux(safe_index, valid, index, zero));
    Some((safe_index, valid))
}

fn guard_dynamic_array_read_sir(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    valid: celox_sir::RegisterId,
    value: celox_sir::RegisterId,
    value_width: usize,
    is_4state: bool,
) -> celox_sir::RegisterId {
    let unknown = builder.alloc_logic(value_width);
    if is_4state {
        let unknown_mask = (BigUint::from(1u8) << value_width) - BigUint::from(1u8);
        builder.emit(SIRInstruction::Imm(
            unknown,
            SIRValue::new_four_state(BigUint::default(), unknown_mask),
        ));
    } else {
        builder.emit(SIRInstruction::Imm(unknown, SIRValue::new(0u8)));
    }
    let guarded = builder.alloc_logic(value_width);
    builder.emit(SIRInstruction::Mux(guarded, valid, value, unknown));
    guarded
}

type SvFfBlocks = (
    HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    HashMap<SourceVarId, SourceVarId>,
);

fn lower_ff_processes(
    module: &sv::ir::Module,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    four_state: bool,
) -> Result<SvFfBlocks, sv::AnalyzerError> {
    let mut eval_only_ff_blocks = HashMap::default();
    let mut apply_ff_blocks = HashMap::default();
    let mut eval_apply_ff_blocks = HashMap::default();
    let mut reset_clock_map = HashMap::default();
    let mut clock_edges = HashMap::default();
    let mut reset_edges = HashMap::default();

    for process in module.ff_processes() {
        let clock = clock_event_from_ff_process(process)
            .ok_or_else(|| sv::AnalyzerError::Unsupported("always_ff event control".to_string()))?;
        let clock_id = *name_to_id
            .get(clock.signal())
            .ok_or_else(|| sv::AnalyzerError::Unsupported("always_ff event control".to_string()))?;
        if variables
            .get(&clock_id)
            .is_some_and(|variable| variable.width != 1)
        {
            return Err(sv::AnalyzerError::Unsupported(
                "multi-bit always_ff event signal".to_string(),
            ));
        }
        if four_state
            && variables
                .get(&clock_id)
                .is_some_and(|variable| variable.is_4state)
        {
            return Err(sv::AnalyzerError::Unsupported(
                "four-state always_ff event signal".to_string(),
            ));
        }
        if reset_edges.contains_key(&clock_id) {
            return Err(sv::AnalyzerError::Unsupported(
                "mixed clock/reset-edge polarities for one signal".to_string(),
            ));
        }
        if clock_edges
            .insert(clock_id, clock.edge())
            .is_some_and(|edge| edge != clock.edge())
        {
            return Err(sv::AnalyzerError::Unsupported(
                "mixed clock-edge polarities for one signal".to_string(),
            ));
        }
        for reset in process
            .events()
            .iter()
            .filter(|event| event.signal() != clock.signal())
        {
            let reset_id = *name_to_id.get(reset.signal()).ok_or_else(|| {
                sv::AnalyzerError::Unsupported("always_ff event control".to_string())
            })?;
            if variables
                .get(&reset_id)
                .is_some_and(|variable| variable.width != 1)
            {
                return Err(sv::AnalyzerError::Unsupported(
                    "multi-bit always_ff event signal".to_string(),
                ));
            }
            if four_state
                && variables
                    .get(&reset_id)
                    .is_some_and(|variable| variable.is_4state)
            {
                return Err(sv::AnalyzerError::Unsupported(
                    "four-state always_ff event signal".to_string(),
                ));
            }
            if clock_edges.contains_key(&reset_id) {
                return Err(sv::AnalyzerError::Unsupported(
                    "mixed clock/reset-edge polarities for one signal".to_string(),
                ));
            }
            if reset_edges
                .insert(reset_id, reset.edge())
                .is_some_and(|edge| edge != reset.edge())
            {
                return Err(sv::AnalyzerError::Unsupported(
                    "mixed reset-edge polarities for one signal".to_string(),
                ));
            }
        }
        let trigger_set = trigger_set_from_ff_process(process, name_to_id)
            .ok_or_else(|| sv::AnalyzerError::Unsupported("always_ff event control".to_string()))?;
        for reset in &trigger_set.resets {
            if reset_clock_map
                .get(reset)
                .is_some_and(|clock| *clock != trigger_set.clock)
            {
                return Err(sv::AnalyzerError::Unsupported(
                    "shared reset associated with multiple clocks".to_string(),
                ));
            }
            reset_clock_map.insert(*reset, trigger_set.clock);
        }
        let (eval_only, apply, eval_apply) = lower_ff_process(
            process,
            &trigger_set,
            variables,
            name_to_id,
            constants,
            parameter_types,
            four_state,
        )
        .ok_or_else(|| {
            sv::AnalyzerError::Unsupported("always_ff assignment lowering".to_string())
        })?;
        insert_or_merge_ff_unit(&mut eval_only_ff_blocks, trigger_set.clone(), eval_only);
        insert_or_merge_ff_unit(&mut apply_ff_blocks, trigger_set.clone(), apply);
        insert_or_merge_ff_unit(&mut eval_apply_ff_blocks, trigger_set, eval_apply);
    }

    Ok((
        eval_only_ff_blocks,
        apply_ff_blocks,
        eval_apply_ff_blocks,
        reset_clock_map,
    ))
}

fn insert_or_merge_ff_unit(
    blocks: &mut HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    trigger_set: TriggerSet<SourceVarId>,
    unit: ExecutionUnit<RegionedVarAddr>,
) {
    if let Some(existing) = blocks.remove(&trigger_set) {
        blocks.insert(trigger_set, merge_sir_eus(&[existing, unit]).0);
    } else {
        blocks.insert(trigger_set, unit);
    }
}

fn clock_event_from_ff_process(process: &sv::ir::FfProcess) -> Option<&sv::ir::FfEvent> {
    let clock = process.events().first()?;
    if process.events().len() == 1 {
        return Some(clock);
    }

    (!ff_event_used_as_condition(process, clock)
        && process.events()[1..]
            .iter()
            .all(|event| ff_event_used_as_condition(process, event)))
    .then_some(clock)
}

fn ff_event_used_as_condition(process: &sv::ir::FfProcess, event: &sv::ir::FfEvent) -> bool {
    process.assignments().iter().any(|assignment| {
        assignment
            .condition()
            .is_some_and(|condition| expr_references_ident(condition, event.signal()))
            || expr_uses_ident_as_condition(assignment.assignment().rhs(), event.signal())
    })
}

fn expr_uses_ident_as_condition(expr: &sv::ir::Expr, name: &str) -> bool {
    match expr {
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_references_ident(condition, name)
                || expr_uses_ident_as_condition(then_expr, name)
                || expr_uses_ident_as_condition(else_expr, name)
        }
        sv::ir::Expr::Select { expr, .. }
        | sv::ir::Expr::Resize { expr, .. }
        | sv::ir::Expr::Unary { expr, .. } => expr_uses_ident_as_condition(expr, name),
        sv::ir::Expr::Concat(parts) | sv::ir::Expr::RepeatConcat { parts, .. } => parts
            .iter()
            .any(|part| expr_uses_ident_as_condition(part, name)),
        sv::ir::Expr::Binary { left, right, .. } => {
            expr_uses_ident_as_condition(left, name) || expr_uses_ident_as_condition(right, name)
        }
        sv::ir::Expr::Call { args, .. } => args
            .iter()
            .any(|arg| expr_uses_ident_as_condition(arg, name)),
        sv::ir::Expr::Ident(_) | sv::ir::Expr::Literal(_) => false,
    }
}

fn trigger_set_from_ff_process(
    process: &sv::ir::FfProcess,
    name_to_id: &HashMap<String, SourceVarId>,
) -> Option<TriggerSet<SourceVarId>> {
    let clock = clock_event_from_ff_process(process)?;
    let clock_id = *name_to_id.get(clock.signal())?;
    let resets = process
        .events()
        .iter()
        .filter(|event| event.signal() != clock.signal())
        .filter_map(|event| name_to_id.get(event.signal()).copied())
        .collect();
    Some(TriggerSet {
        clock: clock_id,
        resets,
    })
}

fn lower_ff_process(
    process: &sv::ir::FfProcess,
    trigger_set: &TriggerSet<SourceVarId>,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    four_state: bool,
) -> Option<(
    ExecutionUnit<RegionedVarAddr>,
    ExecutionUnit<RegionedVarAddr>,
    ExecutionUnit<RegionedVarAddr>,
)> {
    let targets = ff_targets(process, variables, name_to_id, constants, parameter_types)?;
    let mut eval_builder = SIRBuilder::new();
    emit_ff_seeds(&mut eval_builder, &targets);
    emit_ff_assignment_stores(
        &mut eval_builder,
        process,
        &targets,
        variables,
        name_to_id,
        constants,
        parameter_types,
        four_state,
    )?;
    let eval_only = seal_builder(eval_builder);

    let mut apply_builder = SIRBuilder::new();
    emit_ff_commits(&mut apply_builder, &targets);
    let apply = seal_builder(apply_builder);

    let mut eval_apply_builder = SIRBuilder::new();
    emit_ff_seeds(&mut eval_apply_builder, &targets);
    emit_ff_assignment_stores(
        &mut eval_apply_builder,
        process,
        &targets,
        variables,
        name_to_id,
        constants,
        parameter_types,
        four_state,
    )?;
    emit_ff_commits(&mut eval_apply_builder, &targets);
    let eval_apply = seal_builder(eval_apply_builder);

    if trigger_set.resets.is_empty() && targets.is_empty() {
        return None;
    }
    Some((eval_only, apply, eval_apply))
}

fn seal_builder(mut builder: SIRBuilder<RegionedVarAddr>) -> ExecutionUnit<RegionedVarAddr> {
    builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = builder.drain();
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

fn ff_targets(
    process: &sv::ir::FfProcess,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<Vec<VarAtomBase<SourceVarId>>> {
    let mut targets = Vec::new();
    for assignment in process.assignments() {
        let lvalue = assignment.assignment().lhs_value();
        let dynamic =
            dynamic_array_element_lvalue(lvalue, variables, name_to_id, constants, parameter_types);
        let target = lvalue_atom(lvalue, variables, name_to_id, constants, parameter_types)
            .or_else(|| {
                dynamic.as_ref().and_then(|(id, _, _, _)| {
                    variables
                        .get(id)
                        .and_then(|variable| variable.width.checked_sub(1))
                        .map(|msb| VarAtomBase::new(*id, 0, msb))
                })
            })?;
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    Some(targets)
}

fn emit_ff_seeds(builder: &mut SIRBuilder<RegionedVarAddr>, targets: &[VarAtomBase<SourceVarId>]) {
    for target in targets {
        builder.emit(SIRInstruction::Commit(
            RegionedVarAddrBase {
                region: STABLE_REGION,
                var_id: target.id,
            },
            RegionedVarAddrBase {
                region: WORKING_REGION,
                var_id: target.id,
            },
            SIROffset::Static(target.access.lsb),
            target.access.msb - target.access.lsb + 1,
            Vec::new(),
        ));
    }
}

fn emit_ff_commits(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    targets: &[VarAtomBase<SourceVarId>],
) {
    for target in targets {
        builder.emit(SIRInstruction::Commit(
            RegionedVarAddrBase {
                region: WORKING_REGION,
                var_id: target.id,
            },
            RegionedVarAddrBase {
                region: STABLE_REGION,
                var_id: target.id,
            },
            SIROffset::Static(target.access.lsb),
            target.access.msb - target.access.lsb + 1,
            Vec::new(),
        ));
    }
}

fn emit_ff_assignment_stores(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    process: &sv::ir::FfProcess,
    targets: &[VarAtomBase<SourceVarId>],
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    four_state: bool,
) -> Option<()> {
    let mut target_ids = Vec::new();
    for target in targets {
        if !target_ids.contains(&target.id) {
            target_ids.push(target.id);
        }
    }

    for target_id in target_ids {
        let variable = variables.get(&target_id)?;
        let width = variable.width;
        let mut value = builder.alloc_logic(width);
        builder.emit(SIRInstruction::Load(
            value,
            RegionedVarAddrBase {
                // Each process seeds the slices it owns before evaluating.  Read
                // the shared working value here so a later merged always_ff
                // process preserves disjoint slices written by an earlier one.
                region: WORKING_REGION,
                var_id: target_id,
            },
            sv_memory_offset(variable, 0, width),
            width,
        ));
        let mut value_dirty = false;
        for assignment in process.assignments() {
            let lvalue = assignment.assignment().lhs_value();
            let dynamic = dynamic_array_element_lvalue(
                lvalue,
                variables,
                name_to_id,
                constants,
                parameter_types,
            );
            let target = lvalue_atom(lvalue, variables, name_to_id, constants, parameter_types)
                .or_else(|| {
                    dynamic.as_ref().and_then(|(id, _, _, _)| {
                        variables
                            .get(id)
                            .and_then(|variable| variable.width.checked_sub(1))
                            .map(|msb| VarAtomBase::new(*id, 0, msb))
                    })
                })?;
            if target.id != target_id {
                continue;
            }
            let target_width = dynamic.as_ref().map_or_else(
                || target.access.msb - target.access.lsb + 1,
                |(_, _, _, access)| access.msb - access.lsb + 1,
            );
            let rhs_expr = expr_for_state_mode(assignment.assignment().rhs(), four_state);
            let rhs = match &rhs_expr {
                sv::ir::Expr::Literal(literal) => match unbased_fill_literal(literal) {
                    Some(fill) => lower_unbased_fill_literal(builder, fill, target_width)?,
                    None => {
                        let rhs = lower_expr_to_sir_with_context(
                            builder,
                            &rhs_expr,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                            Some(target_width),
                            Some(sv_expr_is_signed_with_parameters(
                                &rhs_expr,
                                variables,
                                name_to_id,
                                parameter_types,
                            )),
                        )?;
                        resize_sir_register(
                            builder,
                            rhs,
                            target_width,
                            sv_expr_is_signed_with_parameters(
                                &rhs_expr,
                                variables,
                                name_to_id,
                                parameter_types,
                            ),
                        )?
                    }
                },
                _ => {
                    let rhs = lower_expr_to_sir_with_context(
                        builder,
                        &rhs_expr,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        Some(target_width),
                        Some(sv_expr_is_signed_with_parameters(
                            &rhs_expr,
                            variables,
                            name_to_id,
                            parameter_types,
                        )),
                    )?;
                    resize_sir_register(
                        builder,
                        rhs,
                        target_width,
                        sv_expr_is_signed_with_parameters(
                            &rhs_expr,
                            variables,
                            name_to_id,
                            parameter_types,
                        ),
                    )?
                }
            };
            let rhs = if variables.get(&target.id)?.is_4state
                && (four_state || !expr_is_unknown_literal(&rhs_expr))
            {
                rhs
            } else {
                let two_state = builder.alloc_bit(target_width, false);
                builder.emit(SIRInstruction::Unary(two_state, UnaryOp::ToTwoState, rhs));
                two_state
            };
            if let Some((_, element_width, offset, access)) = dynamic {
                // Flush preceding static assignments before a dynamic store so
                // the direct element write observes the current working value.
                if value_dirty {
                    builder.emit(SIRInstruction::Store(
                        RegionedVarAddrBase {
                            region: WORKING_REGION,
                            var_id: target_id,
                        },
                        sv_memory_offset(variable, 0, width),
                        width,
                        value,
                        Vec::new(),
                        Vec::new(),
                    ));
                    value_dirty = false;
                }
                let index = lower_dynamic_array_element_index(
                    builder,
                    &offset,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    element_width,
                )?;
                let element_count = variable.width.checked_div(element_width)?;
                let (index, valid) = dynamic_array_index_guard_sir(builder, index, element_count)?;
                let old = builder.alloc_logic(target_width);
                builder.emit(SIRInstruction::Load(
                    old,
                    RegionedVarAddrBase {
                        region: WORKING_REGION,
                        var_id: target_id,
                    },
                    SIROffset::Element {
                        index,
                        element_width,
                        bit_offset: access.lsb,
                        dynamic_bit_offset: None,
                    },
                    target_width,
                ));
                let selected_value = match assignment.condition() {
                    Some(condition) => {
                        let condition = lower_procedural_condition(
                            builder,
                            condition,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                        )?;
                        let mux = builder.alloc_logic(target_width);
                        builder.emit(SIRInstruction::Mux(mux, condition, rhs, old));
                        mux
                    }
                    None => rhs,
                };
                let store_value = builder.alloc_logic(target_width);
                builder.emit(SIRInstruction::Mux(store_value, valid, selected_value, old));
                builder.emit(SIRInstruction::Store(
                    RegionedVarAddrBase {
                        region: WORKING_REGION,
                        var_id: target_id,
                    },
                    SIROffset::Element {
                        index,
                        element_width,
                        bit_offset: access.lsb,
                        dynamic_bit_offset: None,
                    },
                    target_width,
                    store_value,
                    Vec::new(),
                    Vec::new(),
                ));
                value = builder.alloc_logic(width);
                builder.emit(SIRInstruction::Load(
                    value,
                    RegionedVarAddrBase {
                        region: WORKING_REGION,
                        var_id: target_id,
                    },
                    sv_memory_offset(variable, 0, width),
                    width,
                ));
                continue;
            }
            let assigned =
                replace_sir_slice(builder, value, rhs, target.access.lsb, target_width, width)?;
            value = match assignment.condition() {
                Some(condition) => {
                    let condition = lower_procedural_condition(
                        builder,
                        condition,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                    )?;
                    let mux = builder.alloc_logic(width);
                    builder.emit(SIRInstruction::Mux(mux, condition, assigned, value));
                    mux
                }
                None => assigned,
            };
            value_dirty = true;
        }
        if !value_dirty {
            continue;
        }
        for target in targets.iter().filter(|target| target.id == target_id) {
            let target_width = target.access.msb - target.access.lsb + 1;
            let store_value = if target.access.lsb == 0 && target_width == width {
                value
            } else {
                let slice = builder.alloc_logic(target_width);
                builder.emit(SIRInstruction::Slice(
                    slice,
                    value,
                    target.access.lsb,
                    target_width,
                ));
                slice
            };
            builder.emit(SIRInstruction::Store(
                RegionedVarAddrBase {
                    region: WORKING_REGION,
                    var_id: target_id,
                },
                sv_memory_offset(variable, target.access.lsb, target_width),
                target_width,
                store_value,
                Vec::new(),
                Vec::new(),
            ));
        }
    }
    Some(())
}

fn lower_procedural_condition(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    condition: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<celox_sir::RegisterId> {
    let condition = lower_expr_to_sir(
        builder,
        condition,
        variables,
        name_to_id,
        constants,
        parameter_types,
    )?;
    let width = builder.register(&condition).width();
    let two_state = builder.alloc_bit(width, false);
    builder.emit(SIRInstruction::Unary(
        two_state,
        UnaryOp::ToTwoState,
        condition,
    ));
    if width == 1 {
        return Some(two_state);
    }
    let truth = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Unary(truth, UnaryOp::Or, two_state));
    Some(truth)
}

fn replace_sir_slice(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    current: celox_sir::RegisterId,
    replacement: celox_sir::RegisterId,
    lsb: usize,
    replacement_width: usize,
    total_width: usize,
) -> Option<celox_sir::RegisterId> {
    if lsb == 0 && replacement_width == total_width {
        return Some(replacement);
    }
    let end = lsb.checked_add(replacement_width)?;
    if end > total_width {
        return None;
    }

    let mut parts = Vec::with_capacity(3);
    if end < total_width {
        let upper_width = total_width - end;
        let upper = builder.alloc_logic(upper_width);
        builder.emit(SIRInstruction::Slice(upper, current, end, upper_width));
        parts.push(upper);
    }
    parts.push(replacement);
    if lsb != 0 {
        let lower = builder.alloc_logic(lsb);
        builder.emit(SIRInstruction::Slice(lower, current, 0, lsb));
        parts.push(lower);
    }

    let result = builder.alloc_logic(total_width);
    builder.emit(SIRInstruction::Concat(result, parts));
    Some(result)
}

fn lvalue_atom(
    lvalue: &sv::ir::LValue,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<VarAtomBase<SourceVarId>> {
    let id = *name_to_id.get(lvalue.name())?;
    let width = variables.get(&id)?.width;
    match lvalue {
        sv::ir::LValue::Ident(_) => Some(VarAtomBase::new(id, 0, width.checked_sub(1)?)),
        sv::ir::LValue::Select { msb, lsb, .. } => {
            let msb = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
            let lsb = sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
            let variable = variables.get(&id)?;
            let msb = packed_index_offset(variable, msb)?;
            let lsb = packed_index_offset(variable, lsb)?;
            let high = msb.max(lsb);
            let low = msb.min(lsb);
            (low <= high && high < width).then(|| VarAtomBase::new(id, low, high))
        }
    }
}

fn sv_glue_expr_is_signed(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> bool {
    match expr {
        sv::ir::Expr::Ident(name) => name_to_id
            .get(name)
            .and_then(|id| variables.get(id))
            .map(|variable| variable.signed)
            .or_else(|| parameter_types.get(name).map(|(_, signed)| *signed))
            .unwrap_or(false),
        sv::ir::Expr::Literal(literal) => {
            sv::typecheck::parse_integral_literal(literal).is_some_and(|literal| literal.signed)
        }
        sv::ir::Expr::Resize { signed, .. } => *signed,
        sv::ir::Expr::Select { signed, .. } => *signed,
        sv::ir::Expr::Concat(_) | sv::ir::Expr::RepeatConcat { .. } | sv::ir::Expr::Call { .. } => {
            false
        }
        sv::ir::Expr::Unary { op, expr } => {
            matches!(
                op,
                sv::ir::UnaryOp::Plus | sv::ir::UnaryOp::Minus | sv::ir::UnaryOp::BitNot
            ) && sv_glue_expr_is_signed(expr, variables, name_to_id, parameter_types)
        }
        sv::ir::Expr::Binary { left, op, right } => match op {
            sv::ir::BinaryOp::Shl | sv::ir::BinaryOp::Shr | sv::ir::BinaryOp::Sar => {
                sv_glue_expr_is_signed(left, variables, name_to_id, parameter_types)
            }
            sv::ir::BinaryOp::Add
            | sv::ir::BinaryOp::Sub
            | sv::ir::BinaryOp::Mul
            | sv::ir::BinaryOp::Div
            | sv::ir::BinaryOp::Mod
            | sv::ir::BinaryOp::BitAnd
            | sv::ir::BinaryOp::BitOr
            | sv::ir::BinaryOp::BitXor => {
                sv_glue_expr_is_signed(left, variables, name_to_id, parameter_types)
                    && sv_glue_expr_is_signed(right, variables, name_to_id, parameter_types)
            }
            _ => false,
        },
        sv::ir::Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => {
            sv_glue_expr_is_signed(then_expr, variables, name_to_id, parameter_types)
                && sv_glue_expr_is_signed(else_expr, variables, name_to_id, parameter_types)
        }
    }
}

fn sv_expr_is_signed_with_parameters(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> bool {
    match expr {
        sv::ir::Expr::Ident(name) => name_to_id
            .get(name)
            .and_then(|id| variables.get(id))
            .map_or_else(
                || parameter_types.get(name).is_some_and(|(_, signed)| *signed),
                |variable| variable.signed,
            ),
        sv::ir::Expr::Literal(literal) => {
            sv::typecheck::parse_integral_literal(literal).is_some_and(|literal| literal.signed)
        }
        sv::ir::Expr::Resize { signed, .. } => *signed,
        sv::ir::Expr::Select { signed, .. } => *signed,
        sv::ir::Expr::Concat(_) | sv::ir::Expr::RepeatConcat { .. } | sv::ir::Expr::Call { .. } => {
            false
        }
        sv::ir::Expr::Unary { op, expr } => {
            matches!(
                op,
                sv::ir::UnaryOp::Plus | sv::ir::UnaryOp::Minus | sv::ir::UnaryOp::BitNot
            ) && sv_expr_is_signed_with_parameters(expr, variables, name_to_id, parameter_types)
        }
        sv::ir::Expr::Binary { left, op, right } => match op {
            sv::ir::BinaryOp::Shl | sv::ir::BinaryOp::Shr | sv::ir::BinaryOp::Sar => {
                sv_expr_is_signed_with_parameters(left, variables, name_to_id, parameter_types)
            }
            sv::ir::BinaryOp::Add
            | sv::ir::BinaryOp::Sub
            | sv::ir::BinaryOp::Mul
            | sv::ir::BinaryOp::Div
            | sv::ir::BinaryOp::Mod
            | sv::ir::BinaryOp::BitAnd
            | sv::ir::BinaryOp::BitOr
            | sv::ir::BinaryOp::BitXor => {
                sv_expr_is_signed_with_parameters(left, variables, name_to_id, parameter_types)
                    && sv_expr_is_signed_with_parameters(
                        right,
                        variables,
                        name_to_id,
                        parameter_types,
                    )
            }
            _ => false,
        },
        sv::ir::Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => {
            sv_expr_is_signed_with_parameters(then_expr, variables, name_to_id, parameter_types)
                && sv_expr_is_signed_with_parameters(
                    else_expr,
                    variables,
                    name_to_id,
                    parameter_types,
                )
        }
    }
}

fn resize_sir_register(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    source: celox_sir::RegisterId,
    target_width: usize,
    sign_extend: bool,
) -> Option<celox_sir::RegisterId> {
    let source_type = builder.register(&source).clone();
    let source_width = source_type.width();
    if source_width == target_width {
        return Some(source);
    }

    let alloc_like = |builder: &mut SIRBuilder<RegionedVarAddr>, width| match &source_type {
        RegisterType::Logic { .. } => builder.alloc_logic(width),
        RegisterType::Bit { signed, .. } => builder.alloc_bit(width, *signed && sign_extend),
    };

    if source_width > target_width {
        let resized = alloc_like(builder, target_width);
        builder.emit(SIRInstruction::Slice(resized, source, 0, target_width));
        return Some(resized);
    }

    let extension_width = target_width - source_width;
    let mut parts = Vec::with_capacity(extension_width.saturating_add(1));
    if sign_extend {
        let sign = alloc_like(builder, 1);
        builder.emit(SIRInstruction::Slice(
            sign,
            source,
            source_width.checked_sub(1)?,
            1,
        ));
        parts.extend(std::iter::repeat_n(sign, extension_width));
    } else {
        let zero = alloc_like(builder, extension_width);
        builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        parts.push(zero);
    }
    parts.push(source);
    let resized = alloc_like(builder, target_width);
    builder.emit(SIRInstruction::Concat(resized, parts));
    Some(resized)
}

fn unbased_fill_literal(literal: &str) -> Option<char> {
    let normalized = literal.trim().to_ascii_lowercase();
    let mut chars = normalized.chars();
    (chars.next()? == '\'' && chars.clone().count() == 1).then_some(chars.next()?)
}

fn expr_unbased_fill_literal(expr: &sv::ir::Expr) -> Option<char> {
    match expr {
        sv::ir::Expr::Literal(literal) => unbased_fill_literal(literal),
        _ => None,
    }
}

fn expr_is_unknown_literal(expr: &sv::ir::Expr) -> bool {
    let sv::ir::Expr::Literal(literal) = expr else {
        return false;
    };
    sv::typecheck::parse_integral_literal(literal)
        .is_some_and(|literal| literal.mask != BigUint::default())
}

fn unbased_fill_value(fill: char, width: usize) -> Option<(BigUint, BigUint)> {
    let all_ones = if width == 0 {
        BigUint::default()
    } else {
        (BigUint::from(1u8) << width) - BigUint::from(1u8)
    };
    match fill {
        '0' => Some((BigUint::default(), BigUint::default())),
        '1' => Some((all_ones, BigUint::default())),
        'x' => Some((all_ones.clone(), all_ones)),
        'z' | '?' => Some((BigUint::default(), all_ones)),
        _ => None,
    }
}

fn lower_unbased_fill_literal_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    fill: char,
    width: usize,
) -> Option<celox_slt::NodeId> {
    let (value, mask) = unbased_fill_value(fill, width)?;
    arena
        .alloc(SLTNode::Constant(value, mask, width, false))
        .ok()
}

fn lower_unbased_fill_literal(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    fill: char,
    width: usize,
) -> Option<celox_sir::RegisterId> {
    let (value, mask) = unbased_fill_value(fill, width)?;
    let register = builder.alloc_logic(width);
    builder.emit(SIRInstruction::Imm(
        register,
        SIRValue::new_four_state(value, mask),
    ));
    Some(register)
}

fn lower_expr_to_sir(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<celox_sir::RegisterId> {
    lower_expr_to_sir_with_context(
        builder,
        expr,
        variables,
        name_to_id,
        constants,
        parameter_types,
        None,
        None,
    )
}

fn sv_expr_natural_width(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<usize> {
    match expr {
        sv::ir::Expr::Ident(name) => name_to_id
            .get(name)
            .and_then(|id| variables.get(id))
            .map_or_else(
                || {
                    constants
                        .contains_key(name)
                        .then(|| parameter_types.get(name).map_or(32, |(width, _)| *width))
                },
                |var| Some(var.width),
            ),
        sv::ir::Expr::Literal(literal) => Some(
            unbased_fill_literal(literal)
                .map(|_| 1)
                .unwrap_or(sv::typecheck::parse_integral_literal(literal)?.width),
        ),
        sv::ir::Expr::Select { expr, msb, lsb, .. } => {
            if let Some((_, _, access)) = dynamic_array_element_subselection(
                expr,
                msb,
                lsb,
                variables,
                name_to_id,
                constants,
                parameter_types,
            ) {
                return Some(access.msb - access.lsb + 1);
            }
            let msb = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
            let lsb = sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
            usize::try_from(msb.abs_diff(lsb)).ok()?.checked_add(1)
        }
        sv::ir::Expr::Resize { width, .. } => Some(*width),
        sv::ir::Expr::Unary { op, expr } => matches!(
            op,
            sv::ir::UnaryOp::LogicNot
                | sv::ir::UnaryOp::RedAnd
                | sv::ir::UnaryOp::RedOr
                | sv::ir::UnaryOp::RedXor
        )
        .then_some(1)
        .or_else(|| sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)),
        sv::ir::Expr::Binary { left, op, right } => {
            if matches!(
                op,
                sv::ir::BinaryOp::LogicAnd
                    | sv::ir::BinaryOp::LogicOr
                    | sv::ir::BinaryOp::Eq
                    | sv::ir::BinaryOp::Ne
                    | sv::ir::BinaryOp::EqCase
                    | sv::ir::BinaryOp::NeCase
                    | sv::ir::BinaryOp::EqWildcard
                    | sv::ir::BinaryOp::NeWildcard
                    | sv::ir::BinaryOp::Lt
                    | sv::ir::BinaryOp::Le
                    | sv::ir::BinaryOp::Gt
                    | sv::ir::BinaryOp::Ge
            ) {
                Some(1)
            } else if matches!(
                op,
                sv::ir::BinaryOp::Shl | sv::ir::BinaryOp::Shr | sv::ir::BinaryOp::Sar
            ) {
                sv_expr_natural_width(left, variables, name_to_id, constants, parameter_types)
            } else {
                Some(
                    sv_expr_natural_width(left, variables, name_to_id, constants, parameter_types)?
                        .max(sv_expr_natural_width(
                            right,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                        )?),
                )
            }
        }
        sv::ir::Expr::Concat(parts) => parts.iter().try_fold(0usize, |width, part| {
            width.checked_add(sv_expr_natural_width(
                part,
                variables,
                name_to_id,
                constants,
                parameter_types,
            )?)
        }),
        sv::ir::Expr::RepeatConcat { count, parts } => {
            let count = usize::try_from(sv::typecheck::eval_const_expr_with_types(
                count,
                constants,
                parameter_types,
            )?)
            .ok()?;
            let parts_width = parts.iter().try_fold(0usize, |width, part| {
                width.checked_add(sv_expr_natural_width(
                    part,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                )?)
            })?;
            count.checked_mul(parts_width)
        }
        sv::ir::Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => Some(
            sv_expr_natural_width(then_expr, variables, name_to_id, constants, parameter_types)?
                .max(sv_expr_natural_width(
                    else_expr,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                )?),
        ),
        sv::ir::Expr::Call { .. } => None,
    }
}

fn sv_comparison_operand_width(
    left: &sv::ir::Expr,
    right: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<usize> {
    Some(
        sv_expr_natural_width(left, variables, name_to_id, constants, parameter_types)?.max(
            sv_expr_natural_width(right, variables, name_to_id, constants, parameter_types)?,
        ),
    )
}

fn lower_expr_to_sir_with_context(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    context_width: Option<usize>,
    context_signed: Option<bool>,
) -> Option<celox_sir::RegisterId> {
    match expr {
        sv::ir::Expr::Ident(name) => {
            let Some(id) = name_to_id.get(name).copied() else {
                let value = constants.get(name)?;
                let (width, signed) = parameter_types.get(name).copied().unwrap_or((32, false));
                let reg = builder.alloc_logic(width);
                builder.emit(SIRInstruction::Imm(
                    reg,
                    SIRValue::new_four_state(parameter_value_bits(*value, width), 0u32),
                ));
                return resize_sir_register(
                    builder,
                    reg,
                    context_width.unwrap_or(width),
                    context_signed.unwrap_or(signed),
                );
            };
            let var = variables.get(&id)?;
            let reg = if var.is_4state {
                builder.alloc_logic(var.width)
            } else {
                builder.alloc_bit(var.width, var.signed)
            };
            builder.emit(SIRInstruction::Load(
                reg,
                RegionedVarAddrBase {
                    region: STABLE_REGION,
                    var_id: id,
                },
                sv_memory_offset(var, 0, var.width),
                var.width,
            ));
            resize_sir_register(
                builder,
                reg,
                context_width.unwrap_or(var.width),
                context_signed.unwrap_or(var.signed),
            )
        }
        sv::ir::Expr::Literal(literal) => {
            if let Some(width) = context_width
                && let Some(fill) = unbased_fill_literal(literal)
            {
                return lower_unbased_fill_literal(builder, fill, width);
            }
            let literal = sv::typecheck::parse_integral_literal(literal)?;
            let width = literal.width;
            let signed = literal.signed;
            let reg = builder.alloc_logic(literal.width);
            builder.emit(SIRInstruction::Imm(
                reg,
                SIRValue::new_four_state(literal.value, literal.mask),
            ));
            resize_sir_register(
                builder,
                reg,
                context_width.unwrap_or(width),
                context_signed.unwrap_or(signed),
            )
        }
        sv::ir::Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => {
            if let Some((id, element_width, access)) = dynamic_array_element_subselection(
                expr,
                msb,
                lsb,
                variables,
                name_to_id,
                constants,
                parameter_types,
            ) {
                let index = lower_dynamic_array_element_index(
                    builder,
                    lsb,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    element_width,
                )?;
                let width = access.msb - access.lsb + 1;
                let variable = variables.get(&id)?;
                let element_count = variable.width.checked_div(element_width)?;
                let (index, valid) = dynamic_array_index_guard_sir(builder, index, element_count)?;
                let reg = builder.alloc_logic(width);
                builder.emit(SIRInstruction::Load(
                    reg,
                    RegionedVarAddrBase {
                        region: STABLE_REGION,
                        var_id: id,
                    },
                    SIROffset::Element {
                        index,
                        element_width,
                        bit_offset: access.lsb,
                        dynamic_bit_offset: None,
                    },
                    width,
                ));
                let reg =
                    guard_dynamic_array_read_sir(builder, valid, reg, width, variable.is_4state);
                return resize_sir_register(
                    builder,
                    reg,
                    context_width.unwrap_or(width),
                    context_signed.unwrap_or(*signed),
                );
            }
            let msb = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
            let lsb = sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
            let (msb, lsb) = packed_expr_select_offsets(expr, msb, lsb, variables, name_to_id)?;
            let high = msb.max(lsb);
            let low = msb.min(lsb);
            let width = high - low + 1;
            if let sv::ir::Expr::Ident(name) = &**expr
                && let Some(var) = name_to_id.get(name).and_then(|id| variables.get(id))
                && !var.array_dims.is_empty()
            {
                let reg = builder.alloc_logic(width);
                builder.emit(SIRInstruction::Load(
                    reg,
                    RegionedVarAddrBase {
                        region: STABLE_REGION,
                        var_id: *name_to_id.get(name)?,
                    },
                    sv_memory_offset(var, low, width),
                    width,
                ));
                return resize_sir_register(
                    builder,
                    reg,
                    context_width.unwrap_or(width),
                    context_signed.unwrap_or(*signed),
                );
            }
            let inner = lower_expr_to_sir_with_context(
                builder,
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                None,
                None,
            )?;
            let reg = builder.alloc_logic(width);
            builder.emit(SIRInstruction::Slice(reg, inner, low, width));
            resize_sir_register(
                builder,
                reg,
                context_width.unwrap_or(width),
                context_signed.unwrap_or(*signed),
            )
        }
        sv::ir::Expr::Resize {
            expr,
            width,
            signed,
        } => {
            let inner = lower_expr_to_sir_with_context(
                builder,
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                Some(*width),
                Some(*signed),
            )?;
            let resized = resize_sir_register(builder, inner, *width, *signed)?;
            resize_sir_register(
                builder,
                resized,
                context_width.unwrap_or(*width),
                context_signed.unwrap_or(*signed),
            )
        }
        sv::ir::Expr::Unary { op, expr } => {
            let one_bit_result = matches!(
                op,
                sv::ir::UnaryOp::LogicNot
                    | sv::ir::UnaryOp::RedAnd
                    | sv::ir::UnaryOp::RedOr
                    | sv::ir::UnaryOp::RedXor
            );
            let inner = lower_expr_to_sir_with_context(
                builder,
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                (!one_bit_result).then_some(context_width).flatten(),
                context_signed,
            )?;
            let width = if one_bit_result {
                1
            } else {
                builder.register(&inner).width()
            };
            let reg = if matches!(op, sv::ir::UnaryOp::ToTwoState) {
                builder.alloc_bit(width, false)
            } else {
                builder.alloc_logic(width)
            };
            builder.emit(SIRInstruction::Unary(reg, unary_op_from_sv(*op)?, inner));
            Some(reg)
        }
        sv::ir::Expr::Binary { left, op, right } => {
            let left_signed =
                sv_expr_is_signed_with_parameters(left, variables, name_to_id, parameter_types);
            let operands_signed = left_signed
                && sv_expr_is_signed_with_parameters(right, variables, name_to_id, parameter_types);
            let operator_signed = if matches!(op, sv::ir::BinaryOp::Sar) {
                left_signed
            } else {
                operands_signed
            };
            let comparison = matches!(
                op,
                sv::ir::BinaryOp::Eq
                    | sv::ir::BinaryOp::Ne
                    | sv::ir::BinaryOp::EqCase
                    | sv::ir::BinaryOp::NeCase
                    | sv::ir::BinaryOp::EqWildcard
                    | sv::ir::BinaryOp::NeWildcard
                    | sv::ir::BinaryOp::Lt
                    | sv::ir::BinaryOp::Le
                    | sv::ir::BinaryOp::Gt
                    | sv::ir::BinaryOp::Ge
            );
            let shift = matches!(
                op,
                sv::ir::BinaryOp::Shl | sv::ir::BinaryOp::Shr | sv::ir::BinaryOp::Sar
            );
            let context_determined = !comparison
                && !matches!(op, sv::ir::BinaryOp::LogicAnd | sv::ir::BinaryOp::LogicOr);
            let operation_context = context_width.map(|context_width| {
                context_width.max(
                    sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)
                        .unwrap_or(context_width),
                )
            });
            let comparison_context = comparison
                .then(|| {
                    sv_comparison_operand_width(
                        left,
                        right,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                    )
                })
                .flatten();
            let left_context = if comparison {
                comparison_context
            } else {
                context_determined.then_some(operation_context).flatten()
            };
            let right_context = if comparison {
                comparison_context
            } else {
                (context_determined && !shift)
                    .then_some(operation_context)
                    .flatten()
            };
            let right_fill = match &**right {
                sv::ir::Expr::Literal(literal) => unbased_fill_literal(literal),
                _ => None,
            };
            let left_fill = match &**left {
                sv::ir::Expr::Literal(literal) => unbased_fill_literal(literal),
                _ => None,
            };
            let (mut left, mut right) = if let Some(fill) = right_fill {
                let left = lower_expr_to_sir_with_context(
                    builder,
                    left,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    left_context,
                    Some(if shift { left_signed } else { operands_signed }),
                )?;
                let width = if shift {
                    1
                } else {
                    builder.register(&left).width()
                };
                (left, lower_unbased_fill_literal(builder, fill, width)?)
            } else if let Some(fill) = left_fill {
                let right = lower_expr_to_sir_with_context(
                    builder,
                    right,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    right_context,
                    Some(operands_signed),
                )?;
                let width = left_context.unwrap_or_else(|| builder.register(&right).width());
                (lower_unbased_fill_literal(builder, fill, width)?, right)
            } else {
                (
                    lower_expr_to_sir_with_context(
                        builder,
                        left,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        left_context,
                        Some(if shift { left_signed } else { operands_signed }),
                    )?,
                    lower_expr_to_sir_with_context(
                        builder,
                        right,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        right_context,
                        Some(operands_signed),
                    )?,
                )
            };
            if comparison {
                let common_width = builder
                    .register(&left)
                    .width()
                    .max(builder.register(&right).width());
                left = resize_sir_register(builder, left, common_width, operands_signed)?;
                right = resize_sir_register(builder, right, common_width, operands_signed)?;
            }
            let width = match op {
                sv::ir::BinaryOp::LogicAnd
                | sv::ir::BinaryOp::LogicOr
                | sv::ir::BinaryOp::Eq
                | sv::ir::BinaryOp::Ne
                | sv::ir::BinaryOp::EqCase
                | sv::ir::BinaryOp::NeCase
                | sv::ir::BinaryOp::EqWildcard
                | sv::ir::BinaryOp::NeWildcard
                | sv::ir::BinaryOp::Lt
                | sv::ir::BinaryOp::Le
                | sv::ir::BinaryOp::Gt
                | sv::ir::BinaryOp::Ge => 1,
                sv::ir::BinaryOp::Shl | sv::ir::BinaryOp::Shr | sv::ir::BinaryOp::Sar => {
                    builder.register(&left).width()
                }
                _ => builder
                    .register(&left)
                    .width()
                    .max(builder.register(&right).width()),
            };
            let reg = if matches!(op, sv::ir::BinaryOp::EqCase | sv::ir::BinaryOp::NeCase) {
                builder.alloc_bit(width, false)
            } else {
                builder.alloc_logic(width)
            };
            builder.emit(SIRInstruction::Binary(
                reg,
                left,
                binary_op_from_sv(*op, operator_signed),
                right,
            ));
            Some(reg)
        }
        sv::ir::Expr::Concat(parts) => {
            let mut regs = Vec::new();
            for part in parts {
                regs.push(lower_expr_to_sir_with_context(
                    builder,
                    part,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    expr_unbased_fill_literal(part).map(|_| 1),
                    None,
                )?);
            }
            let width = regs
                .iter()
                .map(|reg| builder.register(reg).width())
                .sum::<usize>();
            let reg = builder.alloc_logic(width);
            builder.emit(SIRInstruction::Concat(reg, regs));
            Some(reg)
        }
        sv::ir::Expr::RepeatConcat { count, parts } => {
            let count =
                sv::typecheck::eval_const_expr_with_types(count, constants, parameter_types)?;
            let count = usize::try_from(count).ok()?;
            let mut regs = Vec::new();
            for _ in 0..count {
                for part in parts {
                    regs.push(lower_expr_to_sir_with_context(
                        builder,
                        part,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        expr_unbased_fill_literal(part).map(|_| 1),
                        None,
                    )?);
                }
            }
            let width = regs
                .iter()
                .map(|reg| builder.register(reg).width())
                .sum::<usize>();
            let reg = builder.alloc_logic(width);
            builder.emit(SIRInstruction::Concat(reg, regs));
            Some(reg)
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let arms_signed = sv_expr_is_signed_with_parameters(
                then_expr,
                variables,
                name_to_id,
                parameter_types,
            ) && sv_expr_is_signed_with_parameters(
                else_expr,
                variables,
                name_to_id,
                parameter_types,
            );
            let arm_context =
                sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)
                    .map(|natural_width| {
                        context_width.map_or(natural_width, |width| width.max(natural_width))
                    })
                    .or(context_width);
            let condition = lower_expr_to_sir_with_context(
                builder,
                condition,
                variables,
                name_to_id,
                constants,
                parameter_types,
                None,
                None,
            )?;
            let mut then_expr = lower_expr_to_sir_with_context(
                builder,
                then_expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arm_context,
                Some(arms_signed),
            )?;
            let mut else_expr = lower_expr_to_sir_with_context(
                builder,
                else_expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arm_context,
                Some(arms_signed),
            )?;
            let width = builder
                .register(&then_expr)
                .width()
                .max(builder.register(&else_expr).width());
            then_expr = resize_sir_register(builder, then_expr, width, arms_signed)?;
            else_expr = resize_sir_register(builder, else_expr, width, arms_signed)?;
            let reg = builder.alloc_logic(width);
            builder.emit(SIRInstruction::Mux(reg, condition, then_expr, else_expr));
            Some(reg)
        }
        sv::ir::Expr::Call { .. } => None,
    }
}

fn module_constants_with_overrides(
    module: &sv::ir::Module,
    parameter_overrides: &[LoweredSvParameterOverride],
) -> HashMap<String, i128> {
    let override_values: HashMap<&str, &sv::ir::ConstExpr> = parameter_overrides
        .iter()
        .filter_map(|parameter| {
            parameter
                .value
                .as_ref()
                .map(|value| (parameter.name.as_str(), value))
        })
        .collect();
    let mut constants = HashMap::default();
    for parameter in module.parameters() {
        let value = if let Some(override_value) = override_values.get(parameter.name()) {
            sv::typecheck::eval_const_expr(override_value, &constants)
        } else {
            parameter.resolved_value().or_else(|| {
                parameter
                    .value()
                    .and_then(|expr| sv::typecheck::eval_const_expr(expr, &constants))
            })
        };
        if let Some(value) = value {
            constants.insert(parameter.name().to_string(), value);
        }
    }

    constants
}

fn unary_op_from_sv(op: sv::ir::UnaryOp) -> Option<UnaryOp> {
    match op {
        sv::ir::UnaryOp::Plus => Some(UnaryOp::Ident),
        sv::ir::UnaryOp::Minus => Some(UnaryOp::Minus),
        sv::ir::UnaryOp::BitNot => Some(UnaryOp::BitNot),
        sv::ir::UnaryOp::LogicNot => Some(UnaryOp::LogicNot),
        sv::ir::UnaryOp::ToTwoState => Some(UnaryOp::ToTwoState),
        sv::ir::UnaryOp::RedAnd => Some(UnaryOp::And),
        sv::ir::UnaryOp::RedOr => Some(UnaryOp::Or),
        sv::ir::UnaryOp::RedXor => Some(UnaryOp::Xor),
    }
}

fn binary_op_from_sv(op: sv::ir::BinaryOp, operands_signed: bool) -> BinaryOp {
    match op {
        sv::ir::BinaryOp::Add => BinaryOp::Add,
        sv::ir::BinaryOp::Sub => BinaryOp::Sub,
        sv::ir::BinaryOp::Mul => BinaryOp::Mul,
        sv::ir::BinaryOp::Div if operands_signed => BinaryOp::DivS,
        sv::ir::BinaryOp::Div => BinaryOp::DivU,
        sv::ir::BinaryOp::Mod if operands_signed => BinaryOp::RemS,
        sv::ir::BinaryOp::Mod => BinaryOp::RemU,
        sv::ir::BinaryOp::Shl => BinaryOp::Shl,
        sv::ir::BinaryOp::Shr => BinaryOp::Shr,
        sv::ir::BinaryOp::Sar if operands_signed => BinaryOp::Sar,
        sv::ir::BinaryOp::Sar => BinaryOp::Shr,
        sv::ir::BinaryOp::BitAnd => BinaryOp::And,
        sv::ir::BinaryOp::BitOr => BinaryOp::Or,
        sv::ir::BinaryOp::BitXor => BinaryOp::Xor,
        sv::ir::BinaryOp::LogicAnd => BinaryOp::LogicAnd,
        sv::ir::BinaryOp::LogicOr => BinaryOp::LogicOr,
        sv::ir::BinaryOp::Eq => BinaryOp::Eq,
        sv::ir::BinaryOp::Ne => BinaryOp::Ne,
        sv::ir::BinaryOp::EqCase => BinaryOp::EqCase,
        sv::ir::BinaryOp::NeCase => BinaryOp::NeCase,
        sv::ir::BinaryOp::EqWildcard => BinaryOp::EqWildcard,
        sv::ir::BinaryOp::NeWildcard => BinaryOp::NeWildcard,
        sv::ir::BinaryOp::Lt if operands_signed => BinaryOp::LtS,
        sv::ir::BinaryOp::Lt => BinaryOp::LtU,
        sv::ir::BinaryOp::Le if operands_signed => BinaryOp::LeS,
        sv::ir::BinaryOp::Le => BinaryOp::LeU,
        sv::ir::BinaryOp::Gt if operands_signed => BinaryOp::GtS,
        sv::ir::BinaryOp::Gt => BinaryOp::GtU,
        sv::ir::BinaryOp::Ge if operands_signed => BinaryOp::GeS,
        sv::ir::BinaryOp::Ge => BinaryOp::GeU,
    }
}

pub(crate) fn sv_top_not_found(name: String) -> ParserError {
    ParserError::TopNotFound { name }
}

pub(crate) fn unsupported_sv_instance(name: String) -> ParserError {
    ParserError::unsupported(
        64,
        LoweringPhase::SimulatorParser,
        "systemverilog module instantiation",
        format!("name: \"{}\"", name),
        None,
    )
}

pub(crate) fn unsupported_sv_inout(path: String) -> ParserError {
    ParserError::unsupported(
        64,
        LoweringPhase::SimulatorParser,
        "systemverilog inout port",
        path,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_sources_adds_nested_offsets_and_keeps_computed_dependencies() {
        let nested = sv::ir::Expr::Select {
            expr: Box::new(sv::ir::Expr::Ident("a".to_string())),
            msb: sv::ir::ConstExpr::Literal("15".to_string()),
            lsb: sv::ir::ConstExpr::Literal("8".to_string()),
            signed: false,
        };
        let nested_sources = HashSet::from_iter([VarAtomBase::new(1u8, 8, 15)]);
        let narrowed = select_sources(&nested, nested_sources, BitAccess::new(0, 0)).unwrap();
        assert_eq!(narrowed, HashSet::from_iter([VarAtomBase::new(1u8, 8, 8)]));

        let computed = sv::ir::Expr::Binary {
            left: Box::new(sv::ir::Expr::Ident("a".to_string())),
            op: sv::ir::BinaryOp::Add,
            right: Box::new(sv::ir::Expr::Ident("b".to_string())),
        };
        let computed_sources =
            HashSet::from_iter([VarAtomBase::new(1u8, 0, 7), VarAtomBase::new(2u8, 0, 7)]);
        assert_eq!(
            select_sources(&computed, computed_sources.clone(), BitAccess::new(7, 7)).unwrap(),
            computed_sources
        );
    }
}
