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
    RegionedVarAddrBase, RuntimeErrorInfo, RuntimeEventKind, RuntimeEventSite, STABLE_REGION,
    TriggerSet, UnaryOp, VarAtomBase, WORKING_REGION,
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
    BlockId, ExecutionUnit, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator, SIRValue,
    merge_sir_eus,
};
use celox_slt::{
    CombObserver, GlueBlockBase, LogicPath, LogicPathTarget, NodeId, SLTIndex, SLTIndexKind,
    SLTNode, SLTNodeArena,
};
use celox_sv_analyzer as sv;
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use num_bigint::BigUint;

mod comb;
mod ff;
mod procedural;

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
    packed_ranges: Vec<(i128, i128)>,
    array_dims: Vec<usize>,
    domain_kind: DomainKind,
    kind: VariableKind,
    type_kind: PortTypeKind,
    source: Option<SourceLocation>,
    /// A procedural local or a lowering temporary: not addressable by name.
    hidden: bool,
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
            module_affiliated: !self.hidden,
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
    /// The positional interface of every module in all sources, used to bind
    /// positional port and parameter connections.
    interfaces: std::sync::Arc<sv::ModuleInterfaces>,
    /// The packages declared in all sources, inlined into the modules that use them.
    packages: std::sync::Arc<HashMap<String, sv::PackageSource>>,
}

#[derive(Clone)]
pub(crate) struct LoweredSvInstance {
    pub module_name: String,
    pub instance_name: String,
    pub parameter_overrides: Vec<LoweredSvParameterOverride>,
    pub port_connections: Vec<LoweredSvPortConnection>,
    /// For an element of an instance array: its position in declaration order
    /// and the number of elements.
    pub array_element: Option<(usize, usize)>,
    /// The lower bound of the instance array, which numbers its elements.
    pub array_index_base: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LoweredSvParameterOverride {
    pub name: String,
    pub value: Option<sv::ir::ConstExpr>,
    /// The data type bound to a `parameter type`, as source text.
    pub type_text: Option<String>,
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
    let mut interfaces = sv::ModuleInterfaces::default();
    for (code, path) in sources {
        interfaces.extend(sv::source_module_interfaces(code, path)?);
    }
    let interfaces = std::sync::Arc::new(interfaces);
    let mut packages = HashMap::default();
    for (code, path) in sources {
        for package in sv::source_packages(code, path)? {
            packages.insert(package.name.clone(), package);
        }
    }
    let packages = std::sync::Arc::new(packages);
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
                    interfaces: interfaces.clone(),
                    packages: packages.clone(),
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
        let element_connections = match instance.array_element {
            Some((position, count)) => match array_element_connections(
                &instance.port_connections,
                child,
                position,
                count,
                &module.variables,
                &module.signal_names,
                &module.constants,
                &module.parameter_types,
            ) {
                Ok(connections) => Some(connections),
                // Building the instance reports the invalid connection.
                Err(_) => continue,
            },
            None => None,
        };
        let connections = element_connections
            .as_deref()
            .unwrap_or(&instance.port_connections);
        for connection in connections {
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
            let Some(accesses) = output_lvalue_accesses(
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
            for OutputLvalueAccess { signal, access, .. } in accesses {
                if signal == signal_id {
                    drivers.push((
                        drivers.len(),
                        Some((access.lsb as i128, access.msb as i128)),
                    ));
                }
            }
        }
    }
    drivers
}

fn output_connection_targets_signal(expr: &sv::ir::Expr, signal_name: &str) -> bool {
    match expr {
        sv::ir::Expr::Ident(name) => name == signal_name,
        sv::ir::Expr::Select { expr, .. }
        | sv::ir::Expr::Resize { expr, .. }
        | sv::ir::Expr::Unary {
            op: sv::ir::UnaryOp::ToTwoState,
            expr,
        } => output_connection_targets_signal(expr, signal_name),
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
    let body_drivers = |drivers: &mut Vec<_>, body: &[sv::ir::Stmt], driver_id: usize| {
        for stmt in body {
            stmt.walk(&mut |stmt| {
                let lvalues: Vec<&sv::ir::LValue> = match stmt {
                    sv::ir::Stmt::Assign { lhs, .. } => vec![lhs],
                    sv::ir::Stmt::AssignConcat { parts, .. } => parts.iter().collect(),
                    _ => Vec::new(),
                };
                for lvalue in lvalues {
                    if lvalue.name() == signal_name {
                        drivers.push((
                            driver_id,
                            net_lvalue_range(lvalue, constants, parameter_types),
                        ));
                    }
                }
            });
        }
    };
    for process in module.comb_processes() {
        let active = process.condition().is_none_or(|condition| {
            sv::typecheck::eval_const_expr_with_types(condition, constants, parameter_types)
                .is_none_or(|value| value != 0)
        });
        if active {
            body_drivers(&mut drivers, process.body(), driver_id);
        }
        driver_id += 1;
    }
    for process in module.ff_processes() {
        body_drivers(&mut drivers, process.body(), driver_id);
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
        let lowered = specialize_module(base, &key, four_state, false)?;
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
            Ok((module_id, specialize_module(base, key, four_state, false)?))
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
    parallel: &celox_frontend_core::ParallelScheduleOptions,
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
                type_text: None,
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
        let lowered = specialize_module(base, &key, four_state, false)?;
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
            let lowered = specialize_module(base, key, four_state, parallel.enabled())
                .map_err(FrontendError::from)?;
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
        parallel,
        trace_options,
        trace,
    )
    .map_err(FrontendError::from)
}

fn sv_specialization_limit_error(name: String) -> ParserError {
    ParserError::unsupported(
        sv::SV_FRONTEND_TRACKING_ISSUE,
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
            sv::SV_FRONTEND_TRACKING_ISSUE,
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
    ff_parts: bool,
) -> Result<LoweredSvModule, sv::AnalyzerError> {
    let overrides = evaluated_parameter_overrides(&key.parameter_overrides)?;
    // A `parameter type` is bound by rewriting its default in the module source.
    let type_overrides: Vec<(String, String)> = key
        .parameter_overrides
        .iter()
        .filter_map(|parameter| Some((parameter.name.clone(), parameter.type_text.clone()?)))
        .collect();
    let typed = sv::apply_module_type_parameters(
        &module.source_code,
        &module.source_path,
        &module.name,
        &type_overrides,
    )?;
    let typed_code = typed.as_deref().unwrap_or(&module.source_code);
    // A module that uses packages is analyzed with their items inlined.
    let inlined = sv::inline_module_packages(
        typed_code,
        &module.source_path,
        &module.name,
        &module.packages,
    )?;
    let ir = sv::analyze_source_module_with_parameter_expr_overrides(
        inlined.as_deref().unwrap_or(typed_code),
        &module.source_path,
        &module.name,
        &overrides,
        &module.interfaces,
    )?;
    let specialized = ir
        .modules()
        .iter()
        .find(|candidate| candidate.name() == module.name)
        .ok_or_else(|| sv::AnalyzerError::Unsupported(format!("module `{}`", module.name)))?;
    lower_module(
        specialized,
        four_state,
        module.implicit_nets_allowed,
        ff_parts,
    )
}

fn lower_module(
    module: &sv::ir::Module,
    four_state: bool,
    implicit_nets_allowed: bool,
    ff_parts: bool,
) -> Result<LoweredSvModule, sv::AnalyzerError> {
    lower_module_with_overrides(module, &[], four_state, implicit_nets_allowed, ff_parts)
}

/// `ff_parts` additionally keeps every `always_ff` process as an
/// independently evaluated part, for lane-partitioned builds.
fn lower_module_with_overrides(
    module: &sv::ir::Module,
    parameter_overrides: &[LoweredSvParameterOverride],
    four_state: bool,
    implicit_nets_allowed: bool,
    ff_parts: bool,
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
            packed_ranges: type_info.packed_ranges,
            array_dims: type_info.array_dims,
            domain_kind: DomainKind::Other,
            kind,
            type_kind: type_info.type_kind,
            source: None,
            hidden: false,
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
            packed_ranges: type_info.packed_ranges,
            array_dims: type_info.array_dims,
            domain_kind: DomainKind::Other,
            kind: VariableKind::Variable,
            type_kind: type_info.type_kind,
            source: None,
            hidden: false,
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

    procedural::register_locals(
        module,
        &mut variables,
        &mut name_to_id,
        &mut next_id,
        &constants,
        &parameter_types,
    )?;
    let (
        (
            eval_only_ff_blocks,
            apply_ff_blocks,
            eval_apply_ff_blocks,
            reset_clock_map,
            parallel_ff_parts,
        ),
        runtime_event_sites,
        runtime_errors,
    ) = {
        let mut pm = procedural::ProcModule::new(
            module,
            &mut variables,
            &mut name_to_id,
            &constants,
            &parameter_types,
            four_state,
        );
        let blocks = lower_ff_processes(module, &mut pm, ff_parts)?;
        (
            blocks,
            std::mem::take(&mut pm.runtime_event_sites),
            std::mem::take(&mut pm.runtime_errors),
        )
    };
    mark_ff_event_domains(module, &mut variables, &name_to_id);
    initial_memory_values.extend(lower_initial_processes(
        module,
        &mut variables,
        &mut name_to_id,
        &constants,
        &parameter_types,
        four_state,
    )?);

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
        // An instance array is one instance per element, all under one name.
        // Elements are listed from the lowest index up and numbered from the
        // lower bound, so the hierarchy uses the declared indices; each keeps
        // its position in declaration order, which decides its connections.
        let array_index_base = match instance.array_range() {
            Some((left, right)) => usize::try_from(left.min(right)).map_err(|_| {
                sv::AnalyzerError::Unsupported(format!(
                    "module instance array `{}` with a negative index",
                    instance.name()
                ))
            })?,
            None => 0,
        };
        let elements = instance.array_len().map_or(vec![None], |len| {
            let descending = instance
                .array_range()
                .is_some_and(|(left, right)| left >= right);
            (0..len)
                .map(|offset| {
                    let position = if descending { len - 1 - offset } else { offset };
                    Some((position, len))
                })
                .collect()
        });
        for array_element in elements {
            instances.push(LoweredSvInstance {
                module_name: instance.module_name().to_string(),
                instance_name: instance.name().to_string(),
                parameter_overrides: lower_parameter_overrides(
                    instance,
                    &constants,
                    &parameter_types,
                ),
                port_connections: instance
                    .port_connections()
                    .iter()
                    .map(|connection| LoweredSvPortConnection {
                        formal: connection.formal().to_string(),
                        actual: connection.actual().to_string(),
                        actual_expr: connection.actual_expr().cloned(),
                    })
                    .collect(),
                array_element,
                array_index_base,
            });
        }
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
            parallel_ff_parts,
            glue_blocks: HashMap::default(),
            indexed_instance_names: HashSet::default(),
            instance_index_bases: HashMap::default(),
            comb_blocks: Vec::new(),
            comb_observers: Vec::<CombObserver<SourceVarId>>::new(),
            runtime_errors,
            runtime_event_sites,
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
                type_text: parameter.type_text().map(str::to_string),
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

/// The port connections of element `position` of an array of `count`
/// instances. A connection as wide as the port is shared by every element; one
/// `count` times as wide is divided between them, the first element taking the
/// most significant slice.
fn array_element_connections(
    connections: &[LoweredSvPortConnection],
    child: &LoweredSvModule,
    position: usize,
    count: usize,
    parent_variables: &HashMap<SourceVarId, SvVariable>,
    parent_signal_names: &HashMap<String, SourceVarId>,
    parent_constants: &HashMap<String, i128>,
    parent_parameter_types: &HashMap<String, (usize, bool)>,
) -> Result<Vec<LoweredSvPortConnection>, ParserError> {
    let unsupported = |detail: String| {
        ParserError::unsupported(
            sv::SV_FRONTEND_TRACKING_ISSUE,
            LoweringPhase::SimulatorParser,
            "systemverilog module instance array",
            detail,
            None,
        )
    };
    connections
        .iter()
        .map(|connection| {
            let Some(actual_expr) = connection.actual_expr.as_ref() else {
                return Ok(connection.clone());
            };
            let Some(port_width) = child
                .signal_names
                .get(&connection.formal)
                .and_then(|id| child.variables.get(id))
                .map(|variable| variable.width)
            else {
                return Ok(connection.clone());
            };
            let Some(actual_width) = sv_expr_natural_width(
                actual_expr,
                parent_variables,
                parent_signal_names,
                parent_constants,
                parent_parameter_types,
            ) else {
                return Ok(connection.clone());
            };
            if actual_width == port_width {
                return Ok(connection.clone());
            }
            // Any other width is an error (IEEE 1800-2023 23.3.3.5), including
            // a fill literal ('0 is one bit) or an unsized constant (32 bits).
            let mismatch = || {
                ParserError::illegal_context(
                    "systemverilog module instance array connection",
                    format!(
                        "`{}` is {actual_width} bits wide; a {port_width}-bit port of {count} elements needs {port_width} or {} bits",
                        connection.formal,
                        port_width * count
                    ),
                    None,
                )
            };
            if actual_width != port_width * count {
                return Err(mismatch());
            }
            let sv::ir::Expr::Ident(name) = actual_expr else {
                return Err(unsupported(format!(
                    "`{}` is split between the elements only from a named vector or array",
                    connection.formal
                )));
            };
            let unpacked = parent_signal_names
                .get(name)
                .and_then(|id| parent_variables.get(id))
                .is_some_and(|variable| variable.array_dims == [count]);
            if unpacked && actual_width == port_width * count {
                // An unpacked array of `count` elements: element `i` of the
                // instance array takes array element `i`.
                let lsb = position * port_width;
                return Ok(LoweredSvPortConnection {
                    formal: connection.formal.clone(),
                    actual: connection.actual.clone(),
                    actual_expr: Some(sv::ir::Expr::Select {
                        expr: Box::new(actual_expr.clone()),
                        msb: sv::ir::ConstExpr::Literal((lsb + port_width - 1).to_string()),
                        lsb: sv::ir::ConstExpr::Literal(lsb.to_string()),
                        signed: false,
                    }),
                });
            }
            let zero_based = parent_signal_names
                .get(name)
                .and_then(|id| parent_variables.get(id))
                .is_some_and(|variable| {
                    variable.array_dims.is_empty()
                        && matches!(
                            variable.packed_ranges.as_slice(),
                            [] | [(_, 0)]
                        )
                });
            if !zero_based {
                return Err(unsupported(format!(
                    "`{}` is split between the elements only from a vector declared [N-1:0]",
                    connection.formal
                )));
            }
            let lsb = (count - 1 - position) * port_width;
            let slice = sv::ir::Expr::Select {
                expr: Box::new(actual_expr.clone()),
                msb: sv::ir::ConstExpr::Literal((lsb + port_width - 1).to_string()),
                lsb: sv::ir::ConstExpr::Literal(lsb.to_string()),
                signed: false,
            };
            Ok(LoweredSvPortConnection {
                formal: connection.formal.clone(),
                actual: connection.actual.clone(),
                actual_expr: Some(slice),
            })
        })
        .collect()
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
                sv::SV_FRONTEND_TRACKING_ISSUE,
                LoweringPhase::SimulatorParser,
                "recursive systemverilog module instantiation",
                instance.module_name.clone(),
                None,
            ));
        }
        let Some(child) = lowered_modules.get(&child_id) else {
            return Err(unsupported_sv_instance(instance.module_name.clone()));
        };
        let connections = match instance.array_element {
            Some((position, count)) => {
                module
                    .indexed_instance_names
                    .insert(instance.instance_name.clone());
                if instance.array_index_base != 0 {
                    module
                        .instance_index_bases
                        .insert(instance.instance_name.clone(), instance.array_index_base);
                }
                array_element_connections(
                    &instance.port_connections,
                    child,
                    position,
                    count,
                    &parent_variables,
                    &signal_names,
                    &lowered.constants,
                    &lowered.parameter_types,
                )?
            }
            None => instance.port_connections.clone(),
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
            &connections,
        )?;
        resolved_instances.push((instance, child_id, child, connections));
    }
    let (comb_blocks, arena, created, comb_observers, comb_sites) = lower_comb_processes(
        &lowered.source,
        &mut parent_variables,
        &mut signal_names,
        &lowered.constants,
        &lowered.parameter_types,
        four_state,
    )
    .map_err(|error| match error {
        sv::AnalyzerError::MemoryFile(detail) => ParserError::MemoryFile {
            detail,
            source_location: None,
        },
        error => ParserError::unsupported(
            error.tracking_issue(),
            LoweringPhase::SimulatorParser,
            "systemverilog combinational process lowering",
            error.to_string(),
            None,
        ),
    })?;
    module.comb_blocks = comb_blocks;
    module.arena = arena;
    // Combinational event sites follow the flip-flop ones.
    let site_base = module.runtime_event_sites.len() as u32;
    for mut observer in comb_observers {
        observer.site_id += site_base;
        observer.activation_group += site_base;
        module.comb_observers.push(observer);
    }
    module.runtime_event_sites.extend(comb_sites);
    for id in created {
        module
            .variables
            .insert(id, parent_variables[&id].to_symbolic_variable());
    }
    for (instance, child_id, child, connections) in resolved_instances {
        let glue = build_instance_glue(
            &parent_variables,
            &signal_names,
            &lowered.constants,
            &lowered.parameter_types,
            child,
            &connections,
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

/// `expr inside { items }` as the comparisons the language defines: a value
/// matches by wildcard equality, a range by an inclusive bounds check.
fn inside_as_comparisons(expr: &sv::ir::Expr, items: &[sv::ir::InsideItem]) -> sv::ir::Expr {
    let compare =
        |left: sv::ir::Expr, op: sv::ir::BinaryOp, right: sv::ir::Expr| sv::ir::Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
    items
        .iter()
        .map(|item| match item {
            sv::ir::InsideItem::Value(value) => {
                compare(expr.clone(), sv::ir::BinaryOp::EqWildcard, value.clone())
            }
            sv::ir::InsideItem::Range { low, high } => compare(
                compare(expr.clone(), sv::ir::BinaryOp::Ge, low.clone()),
                sv::ir::BinaryOp::LogicAnd,
                compare(expr.clone(), sv::ir::BinaryOp::Le, high.clone()),
            ),
        })
        .reduce(|left, right| compare(left, sv::ir::BinaryOp::LogicOr, right))
        .unwrap_or_else(|| sv::ir::Expr::Literal("1'b0".to_string()))
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
                let else_expr = expr_for_state_mode(else_expr, four_state);
                // The unknown result has the width and signedness of the
                // division: an X operand makes the whole sum X (IEEE 1800-2023
                // 11.4.3), and `'x` alone would make the result unsigned.
                let unknown = match &else_expr {
                    sv::ir::Expr::Binary { left, right, .. } => sv::ir::Expr::Binary {
                        left: Box::new(sv::ir::Expr::Binary {
                            left: left.clone(),
                            op: sv::ir::BinaryOp::Add,
                            right: right.clone(),
                        }),
                        op: sv::ir::BinaryOp::Add,
                        right: Box::new(sv::ir::Expr::Literal("1'sbx".to_string())),
                    },
                    _ => sv::ir::Expr::Literal("'x".to_string()),
                };
                sv::ir::Expr::Mux {
                    condition: Box::new(expr_for_state_mode(condition, four_state)),
                    then_expr: Box::new(unknown),
                    else_expr: Box::new(else_expr),
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
        sv::ir::Expr::Inside { expr, items } => sv::ir::Expr::Inside {
            expr: Box::new(expr_for_state_mode(expr, four_state)),
            items: items
                .iter()
                .map(|item| item.map(&mut |operand| expr_for_state_mode(operand, four_state)))
                .collect(),
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

/// The initial state `initial` blocks define (IEEE 1800-2023 9.2.1): their
/// bodies run once, before any other process, so each must compute constant
/// values. The hidden variables used while executing them are discarded.
fn lower_initial_processes(
    module: &sv::ir::Module,
    variables: &mut HashMap<SourceVarId, SvVariable>,
    name_to_id: &mut HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    four_state: bool,
) -> Result<Vec<InitialStateValue<SourceVarId>>, sv::AnalyzerError> {
    if module.initial_processes().is_empty() {
        return Ok(Vec::new());
    }
    let mut values = Vec::new();
    let mut pm = procedural::ProcModule::new(
        module,
        variables,
        name_to_id,
        constants,
        parameter_types,
        four_state,
    );
    for process in module.initial_processes() {
        if let Some(condition) = process.condition() {
            let condition =
                sv::typecheck::eval_const_expr_with_types(condition, constants, parameter_types)
                    .ok_or_else(|| {
                        procedural::unsupported("unknown conditional-generate condition")
                    })?;
            if condition == 0 {
                continue;
            }
        }
        let mut arena = SLTNodeArena::new();
        let mut comb = comb::Comb::new(&mut pm, &mut arena);
        values.extend(comb.lower_initial(process.body())?);
    }
    let created = std::mem::take(&mut pm.created);
    for id in created {
        if let Some(variable) = variables.remove(&id) {
            name_to_id.remove(&variable.path.join("."));
        }
    }
    Ok(values)
}

fn lower_comb_processes(
    module: &sv::ir::Module,
    variables: &mut HashMap<SourceVarId, SvVariable>,
    name_to_id: &mut HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    four_state: bool,
) -> Result<
    (
        Vec<LogicPath<SourceVarId>>,
        SLTNodeArena<SourceVarId>,
        Vec<SourceVarId>,
        Vec<CombObserver<SourceVarId>>,
        Vec<RuntimeEventSite>,
    ),
    sv::AnalyzerError,
> {
    let mut arena = SLTNodeArena::new();
    let mut comb_blocks = Vec::new();
    let mut observers = Vec::new();
    let mut sites = Vec::new();
    let mut pm = procedural::ProcModule::new(
        module,
        variables,
        name_to_id,
        constants,
        parameter_types,
        four_state,
    );
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
        let mut comb = comb::Comb::new(&mut pm, &mut arena);
        comb.continuous = process.kind() == sv::ir::CombProcessKind::ContinuousAssign;
        comb_blocks.extend(comb.lower_process(process.body())?);
        // The sites of one process activate together.
        let base = sites.len() as u32;
        for mut observer in std::mem::take(&mut comb.observers) {
            observer.site_id += base;
            observer.activation_group = base;
            observers.push(observer);
        }
        sites.append(&mut comb.sites);
    }
    let created = std::mem::take(&mut pm.created);
    Ok((comb_blocks, arena, created, observers, sites))
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
            packed_ranges: Vec::new(),
            array_dims: Vec::new(),
            domain_kind: DomainKind::Other,
            kind: VariableKind::Variable,
            type_kind: PortTypeKind::Logic,
            source: None,
            hidden: false,
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
                sv::SV_FRONTEND_TRACKING_ISSUE,
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
                            sv::SV_FRONTEND_TRACKING_ISSUE,
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
                let Some(accesses) = output_lvalue_accesses(
                    actual_expr,
                    parent_variables,
                    parent_signal_names,
                    parent_constants,
                    parent_parameter_types,
                ) else {
                    return Err(ParserError::unsupported(
                        sv::SV_FRONTEND_TRACKING_ISSUE,
                        LoweringPhase::SimulatorParser,
                        "systemverilog output port lvalue connection",
                        format!("{formal} -> {actual}: {actual_expr:?}"),
                        None,
                    ));
                };
                let target_width = accesses.iter().try_fold(0usize, |width, target| {
                    let access = &target.access;
                    width.checked_add(access.msb - access.lsb + 1)
                });
                let Some(target_width) = target_width.filter(|target_width| *target_width != 0)
                else {
                    return Err(ParserError::unsupported(
                        sv::SV_FRONTEND_TRACKING_ISSUE,
                        LoweringPhase::SimulatorParser,
                        "systemverilog output port lvalue connection",
                        format!("{formal} -> {actual}: {actual_expr:?}"),
                        None,
                    ));
                };
                let child_input = arena.alloc(SLTNode::Input {
                    variable: GlueAddr::Child(*child_port_id),
                    signed: child_var.signed,
                    index: Vec::new(),
                    access: BitAccess::new(0, width - 1),
                })?;
                let child_node = coerce_node_width(
                    &mut arena,
                    child_input,
                    Some(target_width),
                    child_var.signed,
                )?;
                let mut child_lsb = target_width;
                for OutputLvalueAccess {
                    signal: parent_signal_id,
                    access,
                    is_2state,
                } in accesses
                {
                    let parent_var = &parent_variables[&parent_signal_id];
                    let part_width = access.msb - access.lsb + 1;
                    child_lsb -= part_width;
                    let child_expr = if child_lsb == 0 && part_width == target_width {
                        child_node
                    } else {
                        arena.alloc(SLTNode::Slice {
                            expr: child_node,
                            access: BitAccess::new(child_lsb, child_lsb + part_width - 1),
                        })?
                    };
                    let mut expr = coerce_node_width(
                        &mut arena,
                        child_expr,
                        Some(part_width),
                        child_var.signed,
                    )?;
                    if is_2state || !parent_var.is_4state {
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
            }
            VariableKind::Inout => {
                return Err(unsupported_sv_inout(child_var.path.join(".")));
            }
            _ => {}
        }
    }

    Ok((input_ports, output_ports, arena))
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
            let msb = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
            let lsb = sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
            let (msb, lsb) = packed_expr_select_offsets(expr, msb, lsb, variables, name_to_id)?;
            let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
            (access.msb < variable.width).then_some((id, access))
        }
        _ => None,
    }
}

struct OutputLvalueAccess {
    signal: SourceVarId,
    access: BitAccess,
    is_2state: bool,
}

fn output_lvalue_accesses(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<Vec<OutputLvalueAccess>> {
    match expr {
        sv::ir::Expr::Concat(parts) if !parts.is_empty() => {
            let mut accesses = Vec::new();
            for part in parts {
                accesses.extend(output_lvalue_accesses(
                    part,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                )?);
            }
            Some(accesses)
        }
        sv::ir::Expr::Resize { expr, .. } => {
            output_lvalue_accesses(expr, variables, name_to_id, constants, parameter_types)
        }
        sv::ir::Expr::Unary {
            op: sv::ir::UnaryOp::ToTwoState,
            expr,
        } => {
            // A bit member carries a read conversion in the analyzed expression.
            // For an output connection, retain its target and apply that conversion
            // to the incoming value, even when the enclosing struct is four-state.
            let mut accesses =
                output_lvalue_accesses(expr, variables, name_to_id, constants, parameter_types)?;
            for access in &mut accesses {
                access.is_2state = true;
            }
            Some(accesses)
        }
        _ => output_lvalue_access(expr, variables, name_to_id, constants, parameter_types).map(
            |(signal, access)| {
                vec![OutputLvalueAccess {
                    signal,
                    access,
                    is_2state: false,
                }]
            },
        ),
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
            if let Some(rewritten) = runtime_select_as_shift(
                expr,
                msb,
                lsb,
                *signed,
                variables,
                name_to_id,
                constants,
                parameter_types,
            ) {
                return lower_glue_parent_expr(
                    &rewritten,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    context_width,
                    context_signed,
                );
            }
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
                let node = lower_dynamic_array_selection_slt(
                    arena,
                    GlueAddr::Parent(id),
                    *signed,
                    offset,
                    access,
                    element_width,
                    variable,
                )?;
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
                        binary_op_from_sv(*op, operator_signed)?,
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
            let arms_signed = sv_glue_expr_is_signed(then_expr, variables, name_to_id, parameter_types)
                    && sv_glue_expr_is_signed(else_expr, variables, name_to_id, parameter_types)
                    // An unsigned context makes the whole expression unsigned.
                    && context_signed != Some(false);
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
        sv::ir::Expr::Inside { expr, items } => lower_glue_parent_expr(
            &inside_as_comparisons(expr, items),
            variables,
            name_to_id,
            constants,
            parameter_types,
            arena,
            context_width,
            context_signed,
        ),
        sv::ir::Expr::Call { name, args }
            if sv::typecheck::bit_vector_function_return_type(name, args.len()).is_some() =>
        {
            let arg = &args[0];
            let width =
                sv_expr_natural_width(arg, variables, name_to_id, constants, parameter_types)?;
            let (inner, sources, source_ids) = lower_glue_parent_expr(
                arg,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                Some(width),
                None,
            )?;
            Some((
                lower_bit_vector_function_slt(arena, name, inner, context_width, context_signed)?,
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
        sv::ir::Expr::Inside { expr, items } => {
            expr_references_ident(expr, name)
                || items
                    .iter()
                    .flat_map(sv::ir::InsideItem::exprs)
                    .any(|operand| expr_references_ident(operand, name))
        }
        sv::ir::Expr::Call { args, .. } => args.iter().any(|arg| expr_references_ident(arg, name)),
    }
}

/// A write to a packed vector whose selected position is a runtime value.
struct DynamicPackedWrite {
    /// Number of bits written.
    select_width: usize,
    /// Where the selected bits sit; see [`RuntimePosition`].
    up: sv::ir::Expr,
    down: sv::ir::Expr,
}

fn dynamic_packed_write(
    lvalue: &sv::ir::LValue,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<DynamicPackedWrite> {
    let sv::ir::LValue::Select {
        name,
        msb,
        lsb,
        array_slice_width: None,
        ..
    } = lvalue
    else {
        return None;
    };
    let id = *name_to_id.get(name)?;
    let variable = variables.get(&id)?;
    let position = runtime_select_position(
        name,
        msb,
        lsb,
        variables,
        name_to_id,
        constants,
        parameter_types,
    )?;
    (variable.array_dims.is_empty() && position.width <= variable.width).then_some(
        DynamicPackedWrite {
            select_width: position.width,
            up: position.up,
            down: position.down,
        },
    )
}

fn permute_reversed_lvalue_rhs_slt(
    lvalue: &sv::ir::LValue,
    expr: NodeId,
    target_width: usize,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<SourceVarId>,
) -> Option<NodeId> {
    let sv::ir::LValue::Select {
        array_slice_width: Some(array_slice_width),
        array_slice_reversed: true,
        ..
    } = lvalue
    else {
        return Some(expr);
    };
    let element_width = usize::try_from(sv::typecheck::eval_const_expr_with_types(
        array_slice_width,
        constants,
        parameter_types,
    )?)
    .ok()
    .filter(|width| *width != 0)?;
    if !target_width.is_multiple_of(element_width) {
        return None;
    }
    let element_count = target_width / element_width;
    if element_count <= 1 {
        return Some(expr);
    }
    let mut parts = Vec::with_capacity(element_count);
    for lsb in (0..target_width).step_by(element_width) {
        let msb = lsb.checked_add(element_width)?.checked_sub(1)?;
        let part = arena
            .alloc(SLTNode::Slice {
                expr,
                access: BitAccess::new(lsb, msb),
            })
            .ok()?;
        parts.push((part, element_width));
    }
    arena.alloc(SLTNode::Concat(parts)).ok()
}

// IEEE 1800-2023 20.9: count only known ones; predicates return a two-state bit.
fn lower_bit_vector_function_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    name: &str,
    inner: NodeId,
    context_width: Option<usize>,
    context_signed: Option<bool>,
) -> Option<NodeId> {
    let known = arena
        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, inner))
        .ok()?;
    let (width, signed) = sv::typecheck::bit_vector_function_return_type(name, 1)?;
    let result = if name == "$clog2" {
        // ceil(log2(x)): the bit length of x - 1, and 0 for x <= 1
        // (IEEE 1800-2023 20.8.1).
        let operand_width = celox_slt::get_width(known, arena);
        let constant = |arena: &mut SLTNodeArena<A>, value: usize, width: usize| {
            arena
                .alloc(SLTNode::Constant(
                    BigUint::from(value),
                    BigUint::default(),
                    width,
                    false,
                ))
                .ok()
        };
        let one = constant(arena, 1, operand_width)?;
        let decremented = arena
            .alloc(SLTNode::Binary(known, BinaryOp::Sub, one))
            .ok()?;
        let zeros = arena
            .alloc(SLTNode::Unary(UnaryOp::CountLeadingZeros, decremented))
            .ok()?;
        let zeros = coerce_node_width(arena, zeros, Some(32), false).ok()?;
        let total = constant(arena, operand_width, 32)?;
        let bits = arena
            .alloc(SLTNode::Binary(total, BinaryOp::Sub, zeros))
            .ok()?;
        let small = arena
            .alloc(SLTNode::Binary(known, BinaryOp::LeU, one))
            .ok()?;
        let zero = constant(arena, 0, 32)?;
        arena
            .alloc(SLTNode::Mux {
                cond: small,
                then_expr: zero,
                else_expr: bits,
            })
            .ok()?
    } else if name == "$isunknown" {
        // Case inequality detects either X or Z, including unknown bits whose
        // value plane is zero and would disappear during two-state conversion.
        arena
            .alloc(SLTNode::Binary(inner, BinaryOp::NeCase, known))
            .ok()?
    } else {
        let count = arena.alloc(SLTNode::Unary(UnaryOp::PopCount, known)).ok()?;
        // PopCount has a minimal unsigned width; $countones returns signed int.
        let count = coerce_node_width(arena, count, Some(32), false).ok()?;
        if name == "$countones" {
            count
        } else {
            let one = arena
                .alloc(SLTNode::Constant(
                    BigUint::from(1u8),
                    BigUint::default(),
                    32,
                    false,
                ))
                .ok()?;
            let op = if name == "$onehot" {
                BinaryOp::Eq
            } else {
                BinaryOp::LeU
            };
            arena.alloc(SLTNode::Binary(count, op, one)).ok()?
        }
    };
    coerce_node_width(
        arena,
        result,
        Some(context_width.unwrap_or(width)),
        context_signed.unwrap_or(signed),
    )
    .ok()
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
            if let Some(rewritten) = runtime_select_as_shift(
                expr,
                msb,
                lsb,
                *signed,
                variables,
                name_to_id,
                constants,
                parameter_types,
            ) {
                return lower_expr_with_context(
                    &rewritten,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    context_width,
                    context_signed,
                );
            }
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
                let node = lower_dynamic_array_selection_slt(
                    arena,
                    id,
                    *signed,
                    offset,
                    access,
                    element_width,
                    variable,
                )?;
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
            if let Some(fill) = unbased_fill_literal(literal) {
                // In a self-determined context an unbased unsized literal is
                // one bit wide (IEEE 1800-2023 5.7.1).
                return Some((
                    lower_unbased_fill_literal_slt(arena, fill, context_width.unwrap_or(1))?,
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
            // A self-determined operand (such as a concatenation) is extended
            // to the context width before the operator applies.
            let inner = coerce_node_width(
                arena,
                inner,
                operand_context,
                context_signed.unwrap_or(false),
            )
            .ok()?;
            if *op == sv::ir::UnaryOp::Plus {
                // Unary plus is arithmetic: an unknown operand bit makes the
                // whole result unknown (IEEE 1800-2023 11.4.3).
                let width = celox_slt::get_width(inner, arena);
                let zero = arena
                    .alloc(SLTNode::Constant(
                        BigUint::default(),
                        BigUint::default(),
                        width,
                        procedural::node_is_signed(arena, inner),
                    ))
                    .ok()?;
                return Some((
                    arena
                        .alloc(SLTNode::Binary(inner, BinaryOp::Add, zero))
                        .ok()?,
                    sources,
                ));
            }
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
            // An unsigned context makes the operands of a context-determined
            // operator unsigned, and `>>>` a logical shift (IEEE 1800-2023
            // 11.8.2).
            let unsigned_context = context_determined && context_signed == Some(false);
            let left_signed = !unsigned_context
                && sv_expr_is_signed_with_parameters(left, variables, name_to_id, parameter_types);
            let operands_signed = left_signed
                && sv_expr_is_signed_with_parameters(right, variables, name_to_id, parameter_types);
            let operator_signed = if matches!(op, sv::ir::BinaryOp::Sar) {
                left_signed
            } else {
                operands_signed
            };
            let operation_context = context_width.map(|context_width| {
                context_width.max(
                    sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)
                        .unwrap_or(context_width),
                )
            });
            if matches!(op, sv::ir::BinaryOp::Pow) {
                // The base is context-determined and gives the result its
                // type; the exponent is self-determined (IEEE 1800-2023 11.6.1).
                let (base, mut sources) = lower_expr_with_context(
                    left,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    operation_context,
                    Some(left_signed),
                )?;
                let exponent_signed = sv_expr_is_signed_with_parameters(
                    right,
                    variables,
                    name_to_id,
                    parameter_types,
                );
                let (exponent, exponent_sources) = lower_expr_with_context(
                    right,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    None,
                    Some(exponent_signed),
                )?;
                sources.extend(exponent_sources);
                let width = celox_slt::get_width(base, arena);
                let node = celox_frontend_core::symbolic::pow::lower_runtime_pow(
                    arena,
                    base,
                    exponent,
                    width,
                    exponent_signed,
                    left_signed,
                )
                .ok()?;
                return Some((node, sources));
            }
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
                        binary_op_from_sv(*op, operator_signed)?,
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
            ) && context_signed != Some(false);
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
        sv::ir::Expr::Inside { expr, items } => lower_expr_with_context(
            &inside_as_comparisons(expr, items),
            variables,
            name_to_id,
            constants,
            parameter_types,
            arena,
            context_width,
            context_signed,
        ),
        sv::ir::Expr::Call { name, args }
            if sv::typecheck::bit_vector_function_return_type(name, args.len()).is_some() =>
        {
            let arg = &args[0];
            let width =
                sv_expr_natural_width(arg, variables, name_to_id, constants, parameter_types)?;
            let (inner, sources) = lower_expr_with_context(
                arg,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                Some(width),
                None,
            )?;
            Some((
                lower_bit_vector_function_slt(arena, name, inner, context_width, context_signed)?,
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

fn dynamic_array_selection_width(
    variable: &SvVariable,
    access: BitAccess,
    packed_element_width: usize,
) -> Option<usize> {
    let access_width = access.msb.checked_sub(access.lsb)?.checked_add(1)?;
    let element_width = packed_element_width.max(access_width);
    (element_width.is_multiple_of(packed_element_width)
        && variable.width.is_multiple_of(element_width)
        && access.msb < element_width)
        .then_some(element_width)
}

fn dynamic_array_index_kind(variable: &SvVariable, element_width: usize) -> SLTIndexKind {
    if unpacked_element_width(variable) == Some(element_width) {
        SLTIndexKind::Unpacked { element_width }
    } else {
        SLTIndexKind::Packed
    }
}

fn lower_dynamic_array_selection_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    variable: A,
    signed: bool,
    index: NodeId,
    access: BitAccess,
    element_width: usize,
    variable_info: &SvVariable,
) -> Option<NodeId> {
    let packed_element_width = unpacked_element_width(variable_info)?;
    if element_width == packed_element_width {
        return arena
            .alloc(SLTNode::Input {
                variable,
                signed,
                index: vec![SLTIndex {
                    node: index,
                    stride: element_width,
                    kind: dynamic_array_index_kind(variable_info, element_width),
                }],
                access,
            })
            .ok();
    }
    if access.lsb != 0 || access.msb.checked_add(1)? != element_width {
        return None;
    }
    let inner_count = element_width.checked_div(packed_element_width)?;
    let inner_count_literal = arena
        .alloc(SLTNode::Constant(
            BigUint::from(inner_count),
            BigUint::default(),
            64,
            false,
        ))
        .ok()?;
    let scaled_index = arena
        .alloc(SLTNode::Binary(index, BinaryOp::Mul, inner_count_literal))
        .ok()?;
    let mut nodes = Vec::with_capacity(inner_count);
    for inner_index in (0..inner_count).rev() {
        let node = if inner_index == 0 {
            scaled_index
        } else {
            let inner_index_literal = arena
                .alloc(SLTNode::Constant(
                    BigUint::from(inner_index),
                    BigUint::default(),
                    64,
                    false,
                ))
                .ok()?;
            arena
                .alloc(SLTNode::Binary(
                    scaled_index,
                    BinaryOp::Add,
                    inner_index_literal,
                ))
                .ok()?
        };
        let node = arena
            .alloc(SLTNode::Input {
                variable: variable.clone(),
                signed,
                index: vec![SLTIndex {
                    node,
                    stride: packed_element_width,
                    kind: SLTIndexKind::Unpacked {
                        element_width: packed_element_width,
                    },
                }],
                access: BitAccess::new(0, packed_element_width - 1),
            })
            .ok()?;
        nodes.push((node, packed_element_width));
    }
    arena.alloc(SLTNode::Concat(nodes)).ok()
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

/// Width of a packed select whose bounds depend on a runtime value. `None`
/// when both bounds are constants.
fn runtime_select_width(
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<usize> {
    let constant = |bound: &sv::ir::ConstExpr| {
        sv::typecheck::eval_const_expr_with_types(bound, constants, parameter_types)
    };
    if constant(msb).is_some() && constant(lsb).is_some() {
        return None;
    }
    // The width is the same for every value of the runtime operands.
    let mut sample = constants.clone();
    for name in name_to_id.keys() {
        sample.entry(name.clone()).or_insert(0);
    }
    let sampled = |bound: &sv::ir::ConstExpr| {
        sv::typecheck::eval_const_expr_with_types(bound, &sample, parameter_types)
    };
    usize::try_from(sampled(msb)?.abs_diff(sampled(lsb)?))
        .ok()?
        .checked_add(1)
}

/// How far a runtime-positioned select sits from bit 0 of its vector.
///
/// The lowest selected bit lies at position `low` (it is negative when the
/// select hangs over the bottom of the vector). `up` is `low` when it is
/// non-negative and zero otherwise; `down` is `-low` when it is negative and
/// zero otherwise. A write places a value with `(value << up) >> down`; a read
/// brings the selected bits back to bit 0 with `(value << down) >> up`.
struct RuntimePosition {
    width: usize,
    vector_width: usize,
    up: sv::ir::Expr,
    down: sv::ir::Expr,
}

/// The select width and runtime position of the `lsb` index for a packed
/// select of `name` whose bounds depend on a runtime value. A variable keeps
/// its declared range; a parameter is a zero-based vector.
fn runtime_select_position(
    name: &str,
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<RuntimePosition> {
    let (left, right) = match name_to_id.get(name).and_then(|id| variables.get(id)) {
        Some(variable) => {
            if !variable.array_dims.is_empty() {
                return None;
            }
            match variable.packed_ranges.as_slice() {
                [range] => *range,
                // The analyzer flattens the selects of several packed
                // dimensions into bit offsets from bit 0.
                [_, _, ..] => (i128::try_from(variable.width).ok()?.checked_sub(1)?, 0),
                [] => return None,
            }
        }
        None => {
            constants.get(name)?;
            let (width, _) = parameter_types.get(name).copied()?;
            (i128::try_from(width).ok()?.checked_sub(1)?, 0)
        }
    };
    if right < 0 {
        return None;
    }
    let width = runtime_select_width(msb, lsb, name_to_id, constants, parameter_types)?;
    let index = expr_from_const_expr(lsb)?;
    // An index such as `i - 1` wraps when it should be negative. Widen it with
    // its sign so that "hangs over the bottom" can be tested as a comparison.
    let index_is_wide =
        sv_expr_natural_width(&index, variables, name_to_id, constants, parameter_types)
            .is_some_and(|width| width >= 32);
    let index = sv::ir::Expr::Resize {
        expr: Box::new(index),
        width: 64,
        signed: index_is_wide,
    };
    let descending = left >= right;
    let boundary = || sv::ir::Expr::Literal(right.to_string());
    let zero = || sv::ir::Expr::Literal("0".to_string());
    let binary = |left, op, right| sv::ir::Expr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    };
    let select = |condition, then_expr, else_expr| sv::ir::Expr::Mux {
        condition: Box::new(condition),
        then_expr: Box::new(then_expr),
        else_expr: Box::new(else_expr),
    };
    // `above` is the index's distance past the declared bit 0; it is
    // negative when the select hangs over the bottom of the vector.
    let (hangs_over, above, below) = if descending {
        (
            binary(index.clone(), sv::ir::BinaryOp::Lt, boundary()),
            binary(index.clone(), sv::ir::BinaryOp::Sub, boundary()),
            binary(boundary(), sv::ir::BinaryOp::Sub, index),
        )
    } else {
        (
            binary(index.clone(), sv::ir::BinaryOp::Gt, boundary()),
            binary(boundary(), sv::ir::BinaryOp::Sub, index.clone()),
            binary(index, sv::ir::BinaryOp::Sub, boundary()),
        )
    };
    Some(RuntimePosition {
        width,
        vector_width: usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?,
        up: select(hangs_over.clone(), zero(), above),
        down: select(hangs_over, below, zero()),
    })
}

/// Rewrite `v[msb:lsb]` of a packed vector, whose bounds depend on a runtime
/// value, as `(v >> low)[width-1:0]`, where `low` is the bit position of the
/// `lsb` index. Positions outside a four-state vector read as X (IEEE
/// 1800-2023 11.5.1); a two-state vector, or simulation, reads zero.
fn runtime_select_as_shift(
    expr: &sv::ir::Expr,
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    signed: bool,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<sv::ir::Expr> {
    let sv::ir::Expr::Ident(name) = expr else {
        return None;
    };
    let position = runtime_select_position(
        name,
        msb,
        lsb,
        variables,
        name_to_id,
        constants,
        parameter_types,
    )?;
    let shift = |value: sv::ir::Expr, op, amount: sv::ir::Expr| sv::ir::Expr::Binary {
        left: Box::new(value),
        op,
        right: Box::new(amount),
    };
    // Bring the selection down to bit 0: right by `up`, or left by `down`
    // when it hangs over the bottom.
    let move_down = |value: sv::ir::Expr| sv::ir::Expr::Select {
        expr: Box::new(shift(
            shift(value, sv::ir::BinaryOp::Shl, position.down.clone()),
            sv::ir::BinaryOp::Shr,
            position.up.clone(),
        )),
        msb: sv::ir::ConstExpr::Literal((position.width - 1).to_string()),
        lsb: sv::ir::ConstExpr::Literal("0".to_string()),
        signed: false,
    };
    let moved = move_down(expr.clone());
    // Shifting fills the missing bits with 0, which is what a two-state
    // vector (or a parameter) reads.
    let four_state = name_to_id
        .get(name)
        .and_then(|id| variables.get(id))
        .is_some_and(|variable| variable.is_4state);
    if !four_state {
        return Some(sv::ir::Expr::Select {
            expr: Box::new(moved),
            msb: sv::ir::ConstExpr::Literal((position.width - 1).to_string()),
            lsb: sv::ir::ConstExpr::Literal("0".to_string()),
            signed,
        });
    }
    // Moving an all-ones vector the same way marks the selected bits that
    // exist; the others read X.
    let literal = |digit: &str, width: usize| {
        sv::ir::Expr::Literal(format!("{width}'b{}", digit.repeat(width)))
    };
    let in_vector = move_down(literal("1", position.vector_width));
    let binary = |left, op, right| sv::ir::Expr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    };
    let outside = sv::ir::Expr::Unary {
        op: sv::ir::UnaryOp::BitNot,
        expr: Box::new(in_vector.clone()),
    };
    let selected = binary(
        binary(moved, sv::ir::BinaryOp::BitAnd, in_vector),
        sv::ir::BinaryOp::BitOr,
        binary(
            literal("x", position.width),
            sv::ir::BinaryOp::BitAnd,
            outside,
        ),
    );
    Some(sv::ir::Expr::Select {
        expr: Box::new(selected),
        msb: sv::ir::ConstExpr::Literal((position.width - 1).to_string()),
        lsb: sv::ir::ConstExpr::Literal("0".to_string()),
        signed,
    })
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
    let packed_element_width = unpacked_element_width(variable).filter(|width| *width != 0)?;
    let is_dynamic = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)
        .is_none()
        || sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types).is_none();
    if !is_dynamic {
        return None;
    }
    let (msb_base, msb_offset) = split_dynamic_array_offset(msb, constants, parameter_types)?;
    let (lsb_base, lsb_offset) = split_dynamic_array_offset(lsb, constants, parameter_types)?;
    if msb_base != lsb_base {
        return None;
    }
    let msb = usize::try_from(msb_offset).ok()?;
    let lsb = usize::try_from(lsb_offset).ok()?;
    let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
    let element_width = dynamic_array_selection_width(variable, access, packed_element_width)?;
    if element_width > 1
        && !dynamic_array_base_has_stride(
            msb_base,
            i128::try_from(element_width).ok()?,
            constants,
            parameter_types,
        )
    {
        return None;
    }
    Some((id, element_width, access))
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
    let packed_element_width = unpacked_element_width(variable).filter(|width| *width != 0)?;
    let is_dynamic = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)
        .is_none()
        || sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types).is_none();
    if !is_dynamic {
        return None;
    }
    let (msb_base, msb_offset) = split_dynamic_array_offset(msb, constants, parameter_types)?;
    let (lsb_base, lsb_offset) = split_dynamic_array_offset(lsb, constants, parameter_types)?;
    if msb_base != lsb_base {
        return None;
    }
    let offset = lsb.clone();
    let msb = usize::try_from(msb_offset).ok()?;
    let lsb = usize::try_from(lsb_offset).ok()?;
    let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
    let element_width = dynamic_array_selection_width(variable, access, packed_element_width)?;
    if element_width > 1
        && !dynamic_array_base_has_stride(
            msb_base,
            i128::try_from(element_width).ok()?,
            constants,
            parameter_types,
        )
    {
        return None;
    }
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
    // An invalid index reads X: both the value and the mask bit are set
    // (IEEE 1800-2023 7.4.6).
    let unknown = arena
        .alloc(SLTNode::Constant(
            unknown_mask.clone(),
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

type SvFfBlocks = (
    HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    HashMap<SourceVarId, SourceVarId>,
    HashMap<
        TriggerSet<SourceVarId>,
        Vec<celox_frontend_core::symbolic::artifact::FfPart<RegionedVarAddr>>,
    >,
);

fn lower_ff_processes(
    module: &sv::ir::Module,
    pm: &mut procedural::ProcModule<'_>,
    ff_parts: bool,
) -> Result<SvFfBlocks, sv::AnalyzerError> {
    let mut parallel_ff_parts = HashMap::<_, Vec<_>>::default();
    let mut eval_only_ff_blocks = HashMap::default();
    let mut apply_ff_blocks = HashMap::default();
    let mut eval_apply_ff_blocks = HashMap::default();
    let mut reset_clock_map = HashMap::default();
    let mut clock_edges = HashMap::default();
    let mut reset_edges = HashMap::default();

    for process in module.ff_processes() {
        let clock = clock_event_from_ff_process(process)
            .ok_or_else(|| sv::AnalyzerError::Unsupported("always_ff event control".to_string()))?;
        let clock_id = pm
            .id(clock.signal())
            .ok_or_else(|| sv::AnalyzerError::Unsupported("always_ff event control".to_string()))?;
        if pm
            .variables
            .get(&clock_id)
            .is_some_and(|variable| variable.width != 1)
        {
            return Err(sv::AnalyzerError::Unsupported(
                "multi-bit always_ff event signal".to_string(),
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
            let reset_id = pm.id(reset.signal()).ok_or_else(|| {
                sv::AnalyzerError::Unsupported("always_ff event control".to_string())
            })?;
            if pm
                .variables
                .get(&reset_id)
                .is_some_and(|variable| variable.width != 1)
            {
                return Err(sv::AnalyzerError::Unsupported(
                    "multi-bit always_ff event signal".to_string(),
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
        let trigger_set = trigger_set_from_ff_process(process, pm.name_to_id)
            .ok_or_else(|| sv::AnalyzerError::Unsupported("always_ff event control".to_string()))?;
        // A reset shared by several clock domains is associated with the
        // first; the association only names a clock for the reset signal.
        for reset in &trigger_set.resets {
            reset_clock_map.entry(*reset).or_insert(trigger_set.clock);
        }
        let sites_before = pm.runtime_event_sites.len();
        let (eval_only, apply, targets) = ff::Ff::new(pm).lower_process(process.body())?;
        // A process without writes still runs for its runtime events.
        let has_effects = pm.runtime_event_sites.len() > sites_before;
        if trigger_set.resets.is_empty() && targets.is_empty() && !has_effects {
            continue;
        }
        if ff_parts {
            parallel_ff_parts
                .entry(trigger_set.clone())
                .or_default()
                .push(celox_frontend_core::symbolic::artifact::FfPart {
                    evaluate: eval_only.clone(),
                    apply: apply.clone(),
                });
        }
        insert_or_merge_ff_unit(&mut eval_only_ff_blocks, trigger_set.clone(), eval_only);
        insert_or_merge_ff_unit(&mut apply_ff_blocks, trigger_set, apply);
    }
    // Every process of a trigger evaluates against the pre-edge state before
    // any of them commits (IEEE 1800-2023 10.4.2), so the combined unit runs
    // all evaluations first and all commits afterwards.
    for (trigger_set, eval_only) in &eval_only_ff_blocks {
        let apply = &apply_ff_blocks[trigger_set];
        let mut eval_apply = merge_sir_eus(&[eval_only.clone(), apply.clone()]).0;
        ff::prune_unreachable_blocks(&mut eval_apply);
        eval_apply_ff_blocks.insert(trigger_set.clone(), eval_apply);
    }
    // One process is the whole trigger group.
    parallel_ff_parts.retain(|_, parts| parts.len() > 1);

    Ok((
        eval_only_ff_blocks,
        apply_ff_blocks,
        eval_apply_ff_blocks,
        reset_clock_map,
        parallel_ff_parts,
    ))
}

fn insert_or_merge_ff_unit(
    blocks: &mut HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    trigger_set: TriggerSet<SourceVarId>,
    unit: ExecutionUnit<RegionedVarAddr>,
) {
    if let Some(existing) = blocks.remove(&trigger_set) {
        let mut merged = merge_sir_eus(&[existing, unit]).0;
        ff::prune_unreachable_blocks(&mut merged);
        blocks.insert(trigger_set, merged);
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
    let mut used = false;
    for stmt in process.body() {
        stmt.walk(&mut |stmt| match stmt {
            sv::ir::Stmt::If { condition, .. } => {
                used |= expr_references_ident(condition, event.signal());
            }
            sv::ir::Stmt::Case { selector, .. } => {
                used |= expr_references_ident(selector, event.signal());
            }
            sv::ir::Stmt::Assign { rhs, .. } | sv::ir::Stmt::AssignConcat { rhs, .. } => {
                used |= expr_uses_ident_as_condition(rhs, event.signal());
            }
            _ => {}
        });
    }
    used
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
        sv::ir::Expr::Inside { expr, items } => {
            expr_uses_ident_as_condition(expr, name)
                || items
                    .iter()
                    .flat_map(sv::ir::InsideItem::exprs)
                    .any(|operand| expr_uses_ident_as_condition(operand, name))
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

fn seal_builder(mut builder: SIRBuilder<RegionedVarAddr>) -> ExecutionUnit<RegionedVarAddr> {
    builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = builder.drain();
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
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
        sv::ir::Expr::Inside { .. } => false,
        sv::ir::Expr::Call { name, args } => {
            sv::typecheck::bit_vector_function_return_type(name, args.len())
                .is_some_and(|(_, signed)| signed)
        }
        sv::ir::Expr::Concat(_) | sv::ir::Expr::RepeatConcat { .. } => false,
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
        sv::ir::Expr::Inside { .. } => false,
        sv::ir::Expr::Call { name, args } => {
            sv::typecheck::bit_vector_function_return_type(name, args.len())
                .is_some_and(|(_, signed)| signed)
        }
        sv::ir::Expr::Concat(_) | sv::ir::Expr::RepeatConcat { .. } => false,
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
        // The division-by-zero guard takes the type of the division it
        // guards; its unknown arm is internal.
        sv::ir::Expr::Mux {
            then_expr,
            else_expr,
            ..
        } if matches!(
            &**then_expr,
            sv::ir::Expr::Literal(literal) if literal == sv::DIV_ZERO_UNKNOWN_LITERAL
        ) =>
        {
            sv_expr_is_signed_with_parameters(else_expr, variables, name_to_id, parameter_types)
        }
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
            if let Some(width) =
                runtime_select_width(msb, lsb, name_to_id, constants, parameter_types)
            {
                return Some(width);
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
        sv::ir::Expr::Inside { .. } => Some(1),
        sv::ir::Expr::Call { name, args } => {
            sv::typecheck::bit_vector_function_return_type(name, args.len()).map(|(width, _)| width)
        }
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

fn binary_op_from_sv(op: sv::ir::BinaryOp, operands_signed: bool) -> Option<BinaryOp> {
    Some(match op {
        // Exponentiation exists only in constant expressions; hardware for a
        // run-time exponent is not lowered.
        sv::ir::BinaryOp::Pow => return None,
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
    })
}

pub(crate) fn sv_top_not_found(name: String) -> ParserError {
    ParserError::TopNotFound { name }
}

pub(crate) fn unsupported_sv_instance(name: String) -> ParserError {
    ParserError::unsupported(
        sv::SV_FRONTEND_TRACKING_ISSUE,
        LoweringPhase::SimulatorParser,
        "systemverilog module instantiation",
        format!("name: \"{}\"", name),
        None,
    )
}

pub(crate) fn unsupported_sv_inout(path: String) -> ParserError {
    ParserError::unsupported(
        sv::SV_FRONTEND_TRACKING_ISSUE,
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
