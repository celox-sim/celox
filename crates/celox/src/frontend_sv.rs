//! SystemVerilog integration adapter.
//!
//! SystemVerilog syntax and semantic analysis belongs in the
//! `celox-sv-analyzer` crate. The current symbolic assembly pipeline still
//! uses Veryl-owned module identities and metadata, so the adapter that joins
//! those two frontends belongs at the top-level `celox` integration boundary,
//! not in a purportedly independent SystemVerilog frontend crate.

use std::path::{Path, PathBuf};

use celox_design::{
    BinaryOp, BitAccess, DomainKind, ModuleId, PortTypeKind, RegionedVarAddrBase, RuntimeErrorInfo,
    STABLE_REGION, TriggerSet, UnaryOp, VarAtomBase, WORKING_REGION,
};
use celox_frontend_veryl::{
    BuildConfig, ExternalHierarchy, ExternalModule, FrontendTrace, FrontendTraceOptions, GlueAddr,
    LoweringPhase, ParserError, ScheduledRtlOutput, SimModule, SymbolicRtl,
    logic_tree::coerce_node_width,
};
use celox_sir::{
    BlockId, ExecutionUnit, RegisterType, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator,
    SIRValue,
};
use celox_slt::{
    CombObserver, GlueBlockBase, LogicPath, LogicPathTarget, SLTNode, SLTNodeArena, SymbolicStore,
};
use celox_sv_analyzer as sv;
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use num_bigint::BigUint;
use veryl_analyzer::{
    ir::{Module, Shape, Type, TypeKind, VarId, VarKind, VarPath, Variable},
    symbol::Affiliation,
};
use veryl_parser::{resource_table, token_range::TokenRange};

type RegionedVarAddr = RegionedVarAddrBase<VarId>;
type GlueBlock = GlueBlockBase<VarId>;

#[derive(Debug, thiserror::Error)]
pub enum FrontendError {
    #[error(transparent)]
    Analyzer(#[from] sv::AnalyzerError),
    #[error(transparent)]
    Lowering(#[from] ParserError),
}

#[derive(Clone)]
struct SvVariable {
    id: VarId,
    path: VarPath,
    width: usize,
    signed: bool,
    is_4state: bool,
    domain_kind: DomainKind,
    kind: VarKind,
    type_kind: PortTypeKind,
    token: TokenRange,
}

impl SvVariable {
    fn to_shared_variable(&self) -> Variable {
        let kind = match self.domain_kind {
            DomainKind::ClockPosedge => TypeKind::ClockPosedge,
            DomainKind::ClockNegedge => TypeKind::ClockNegedge,
            DomainKind::ResetAsyncHigh => TypeKind::ResetAsyncHigh,
            DomainKind::ResetAsyncLow => TypeKind::ResetAsyncLow,
            DomainKind::Other => match self.type_kind {
                PortTypeKind::Bit => TypeKind::Bit,
                _ => TypeKind::Logic,
            },
        };
        let mut r#type = Type::new(kind);
        r#type.signed = self.signed;
        r#type.set_concrete_width(Shape::new(vec![Some(self.width)]));
        Variable {
            id: self.id,
            path: self.path.clone(),
            kind: self.kind,
            r#type,
            value: Vec::new(),
            assigned: Vec::new(),
            affiliation: Affiliation::Module,
            token: self.token,
        }
    }
}

#[derive(Clone)]
pub(crate) struct LoweredSvModule {
    source: sv::ir::Module,
    source_code: String,
    source_path: PathBuf,
    pub sim_module: SimModule,
    variables: HashMap<VarId, SvVariable>,
    pub port_order: Vec<VarId>,
    pub signal_names: HashMap<String, VarId>,
    pub instances: Vec<LoweredSvInstance>,
}

#[derive(Clone)]
pub(crate) struct LoweredSvInstance {
    pub module_name: resource_table::StrId,
    pub instance_name: resource_table::StrId,
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
    pub name: resource_table::StrId,
    pub parameter_overrides: Vec<LoweredSvParameterOverride>,
}

impl LoweredSvModuleKey {
    pub fn base(name: resource_table::StrId) -> Self {
        Self {
            name,
            parameter_overrides: Vec::new(),
        }
    }

    pub fn instance_key(instance: &LoweredSvInstance) -> Self {
        let mut parameter_overrides = instance.parameter_overrides.clone();
        parameter_overrides.sort_by(|left, right| left.name.cmp(&right.name));
        Self {
            name: instance.module_name,
            parameter_overrides,
        }
    }
}

pub(crate) fn analyze_sources(
    sources: &[(&str, &Path)],
) -> Result<HashMap<resource_table::StrId, LoweredSvModule>, sv::AnalyzerError> {
    let mut modules = HashMap::default();
    for (code, path) in sources {
        let ir = sv::analyze_source(code, path)?;
        for module in ir.modules() {
            let name = resource_table::insert_str(module.name());
            modules.insert(name, lower_module(module, code, path));
        }
    }
    Ok(modules)
}

/// Lower every SystemVerilog module into an embeddable hierarchy. The module
/// IDs in the returned graph are local and are remapped by the Veryl frontend.
pub fn prepare_external_hierarchy(
    sources: &[(&str, &Path)],
) -> Result<ExternalHierarchy, FrontendError> {
    let analyzed = analyze_sources(sources)?;
    let mut names = analyzed.keys().copied().collect::<Vec<_>>();
    names.sort_by_key(|name| resource_table::get_str_value(*name).unwrap_or_default());

    let mut module_ids = HashMap::default();
    let mut queue = Vec::new();
    for name in names {
        let key = LoweredSvModuleKey::base(name);
        let module_id = ModuleId(module_ids.len());
        module_ids.insert(key.clone(), module_id);
        queue.push(key);
    }

    let mut index = 0;
    while index < queue.len() {
        let key = queue[index].clone();
        index += 1;
        let base = analyzed
            .get(&key.name)
            .ok_or_else(|| unsupported_sv_instance(key.name))?;
        let lowered = specialize_module(base, &key)?;
        for instance in &lowered.instances {
            let child_key = LoweredSvModuleKey::instance_key(instance);
            if !analyzed.contains_key(&child_key.name) {
                return Err(unsupported_sv_instance(child_key.name).into());
            }
            if !module_ids.contains_key(&child_key) {
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
                .ok_or_else(|| unsupported_sv_instance(key.name))?;
            Ok((module_id, specialize_module(base, key)?))
        })
        .collect::<Result<HashMap<_, _>, FrontendError>>()?;

    let mut modules = HashMap::default();
    for (key, &module_id) in &module_ids {
        let lowered = &lowered_modules[&module_id];
        let mut sim_module = lowered.sim_module.clone();
        attach_instance_glue(&mut sim_module, lowered, key, &module_ids, &lowered_modules)?;
        modules.insert(
            module_id,
            ExternalModule {
                metadata: metadata_module(&sim_module),
                sim_module,
                port_order: lowered.port_order.clone(),
            },
        );
    }
    let roots = module_ids
        .iter()
        .filter(|(key, _)| key.parameter_overrides.is_empty())
        .map(|(key, &module_id)| (key.name, module_id))
        .collect();
    Ok(ExternalHierarchy { modules, roots })
}

fn metadata_module(module: &SimModule) -> Module {
    Module {
        name: module.name,
        token: TokenRange::default(),
        ports: HashMap::default(),
        port_types: HashMap::default(),
        variables: module.variables.clone(),
        functions: HashMap::default(),
        declarations: Vec::new(),
        suppress_unassigned: true,
        per_decl_refs: HashMap::default(),
        assign_tokens: HashMap::default(),
        ff_table: Default::default(),
    }
}

/// Analyze SystemVerilog sources and lower the selected top through Celox's
/// shared symbolic scheduling pipeline.
pub fn schedule_sources(
    sources: &[(&str, &Path)],
    top: &str,
    config: &BuildConfig,
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
    let top = resource_table::insert_str(top);
    let root_key = LoweredSvModuleKey::base(top);
    if !analyzed.contains_key(&top) {
        return Err(sv_top_not_found(top).into());
    }

    let root_id = ModuleId(0);
    let mut module_ids = HashMap::default();
    module_ids.insert(root_key.clone(), root_id);
    let mut queue = vec![root_key];
    let mut index = 0;
    while index < queue.len() {
        let key = queue[index].clone();
        index += 1;
        let base = analyzed
            .get(&key.name)
            .ok_or_else(|| unsupported_sv_instance(key.name))?;
        let lowered = specialize_module(base, &key)?;
        for instance in &lowered.instances {
            let child_key = LoweredSvModuleKey::instance_key(instance);
            if !analyzed.contains_key(&child_key.name) {
                return Err(unsupported_sv_instance(child_key.name).into());
            }
            if !module_ids.contains_key(&child_key) {
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
                .ok_or_else(|| unsupported_sv_instance(key.name))?;
            let lowered = specialize_module(base, key).map_err(FrontendError::from)?;
            Ok((module_id, lowered))
        })
        .collect::<Result<HashMap<_, _>, FrontendError>>()?;

    let mut modules = HashMap::default();
    let mut module_names = HashMap::default();
    for (key, &module_id) in &module_ids {
        let lowered = &lowered_modules[&module_id];
        let mut sim_module = lowered.sim_module.clone();
        attach_instance_glue(&mut sim_module, lowered, key, &module_ids, &lowered_modules)?;
        module_names.insert(module_id, key.name);
        modules.insert(module_id, sim_module);
    }

    // The shared scheduler still accepts a Veryl module metadata view. SV
    // supplies variables only; declarations and functions remain empty.
    let metadata_modules = modules
        .iter()
        .map(|(&module_id, module)| (module_id, metadata_module(module)))
        .collect::<HashMap<_, _>>();
    let module_ir = metadata_modules
        .iter()
        .map(|(&module_id, module)| (module_id, module))
        .collect();
    let symbolic = SymbolicRtl {
        modules,
        module_ir,
        module_names,
        root_id,
    };
    celox_frontend_veryl::schedule_symbolic_rtl(
        symbolic,
        config,
        ignored_loops,
        true_loops,
        four_state,
        trace_options,
        trace,
    )
    .map_err(FrontendError::from)
}

pub(crate) fn specialize_module(
    module: &LoweredSvModule,
    key: &LoweredSvModuleKey,
) -> Result<LoweredSvModule, sv::AnalyzerError> {
    if key.parameter_overrides.is_empty() {
        return Ok(module.clone());
    }
    let overrides = evaluated_parameter_overrides(&key.parameter_overrides);
    let ir = sv::analyze_source_with_module_parameter_overrides(
        &module.source_code,
        &module.source_path,
        module.source.name(),
        &overrides,
    )?;
    let specialized = ir
        .modules()
        .iter()
        .find(|candidate| candidate.name() == module.source.name())
        .unwrap_or(&module.source);
    Ok(lower_module_with_overrides(
        specialized,
        &[],
        &module.source_code,
        &module.source_path,
    ))
}

fn lower_module(module: &sv::ir::Module, source_code: &str, source_path: &Path) -> LoweredSvModule {
    lower_module_with_overrides(module, &[], source_code, source_path)
}

fn lower_module_with_overrides(
    module: &sv::ir::Module,
    parameter_overrides: &[LoweredSvParameterOverride],
    source_code: &str,
    source_path: &Path,
) -> LoweredSvModule {
    let token = TokenRange::default();
    let name = resource_table::insert_str(module.name());
    let mut next_id = VarId::default();
    let mut variables = HashMap::default();
    let mut name_to_id = HashMap::default();
    let mut port_order = Vec::new();
    let constants = module_constants_with_overrides(module, parameter_overrides);

    for port in module.ports() {
        let id = next_var_id(&mut next_id);
        let type_info = signal_type_from_sv(port.r#type(), &constants);
        let path = VarPath::new(resource_table::insert_str(port.name()));
        let kind = signal_kind_from_port_direction(port.direction());
        let variable = SvVariable {
            id,
            path,
            width: type_info.width,
            signed: type_info.signed,
            is_4state: type_info.is_4state,
            domain_kind: DomainKind::Other,
            kind,
            type_kind: type_info.type_kind,
            token,
        };
        name_to_id.insert(port.name().to_string(), id);
        port_order.push(id);
        variables.insert(id, variable);
    }

    for signal in module.signals() {
        let id = next_var_id(&mut next_id);
        let type_info = signal_type_from_sv(signal.r#type(), &constants);
        let path = VarPath::new(resource_table::insert_str(signal.name()));
        let variable = SvVariable {
            id,
            path,
            width: type_info.width,
            signed: type_info.signed,
            is_4state: type_info.is_4state,
            domain_kind: DomainKind::Other,
            kind: VarKind::Variable,
            type_kind: type_info.type_kind,
            token,
        };
        name_to_id.insert(signal.name().to_string(), id);
        variables.insert(id, variable);
    }

    let mut arena = SLTNodeArena::new();
    let mut comb_blocks = Vec::new();
    for process in module.comb_processes() {
        if process
            .condition()
            .and_then(|condition| sv::typecheck::eval_const_expr(condition, &constants))
            .is_some_and(|condition| condition == 0)
        {
            continue;
        }
        comb_blocks.extend(lower_comb_process(
            process,
            &variables,
            &name_to_id,
            &constants,
            &mut arena,
        ));
    }
    let (eval_only_ff_blocks, apply_ff_blocks, eval_apply_ff_blocks, reset_clock_map) =
        lower_ff_processes(module, &variables, &name_to_id, &constants);
    mark_ff_event_domains(module, &mut variables, &name_to_id);

    let shared_variables = variables
        .iter()
        .map(|(&id, variable)| (id, variable.to_shared_variable()))
        .collect();

    LoweredSvModule {
        source: module.clone(),
        source_code: source_code.to_string(),
        source_path: source_path.to_path_buf(),
        sim_module: SimModule {
            name,
            variables: shared_variables,
            ff_access_summaries: HashMap::default(),
            eval_only_ff_blocks,
            apply_ff_blocks,
            eval_apply_ff_blocks,
            glue_blocks: HashMap::default(),
            comb_blocks,
            comb_observers: Vec::<CombObserver<VarId>>::new(),
            runtime_errors: HashMap::<i64, RuntimeErrorInfo<VarId>>::default(),
            runtime_event_sites: Vec::new(),
            initial_memory_values: Vec::new(),
            comb_boundaries: HashMap::default(),
            arena,
            store: SymbolicStore::default(),
            reset_clock_map,
        },
        variables,
        port_order,
        signal_names: name_to_id,
        instances: module
            .instances()
            .iter()
            .filter(|instance| {
                instance
                    .condition()
                    .and_then(|condition| sv::typecheck::eval_const_expr(condition, &constants))
                    .is_none_or(|condition| condition != 0)
            })
            .map(|instance| LoweredSvInstance {
                module_name: resource_table::insert_str(instance.module_name()),
                instance_name: resource_table::insert_str(instance.name()),
                parameter_overrides: lower_parameter_overrides(instance, &constants),
                port_connections: instance
                    .port_connections()
                    .iter()
                    .map(|connection| LoweredSvPortConnection {
                        formal: connection.formal().to_string(),
                        actual: connection.actual().to_string(),
                        actual_expr: connection.actual_expr().cloned(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn mark_ff_event_domains(
    module: &sv::ir::Module,
    variables: &mut HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
) {
    for process in module.ff_processes() {
        let Some(clock) = process
            .events()
            .iter()
            .find(|event| event.edge() == sv::ir::FfEdge::Pos)
            .or_else(|| process.events().first())
        else {
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
) -> std::collections::HashMap<String, i128> {
    let constants = std::collections::HashMap::new();
    parameter_overrides
        .iter()
        .filter_map(|parameter| {
            parameter
                .value
                .as_ref()
                .and_then(|value| sv::typecheck::eval_const_expr(value, &constants))
                .map(|value| (parameter.name.clone(), value))
        })
        .collect()
}

fn lower_parameter_overrides(
    instance: &sv::ir::Instance,
    constants: &std::collections::HashMap<String, i128>,
) -> Vec<LoweredSvParameterOverride> {
    instance
        .parameter_overrides()
        .iter()
        .map(|parameter| {
            let value = parameter
                .value()
                .and_then(|value| sv::typecheck::eval_const_expr(value, constants))
                .map(|value| sv::ir::ConstExpr::Literal(value.to_string()))
                .or_else(|| parameter.value().cloned());
            LoweredSvParameterOverride {
                name: parameter.name().to_string(),
                value,
            }
        })
        .collect()
}

pub(crate) fn attach_instance_glue(
    module: &mut SimModule,
    lowered: &LoweredSvModule,
    current_key: &LoweredSvModuleKey,
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    lowered_modules: &HashMap<ModuleId, LoweredSvModule>,
) -> Result<(), ParserError> {
    let mut signal_names = lowered.signal_names.clone();
    let mut parent_variables = lowered.variables.clone();
    for instance in &lowered.instances {
        let child_key = LoweredSvModuleKey::instance_key(instance);
        let Some(child_id) = module_ids.get(&child_key).copied() else {
            return Err(unsupported_sv_instance(instance.module_name));
        };
        if &child_key == current_key {
            return Err(ParserError::unsupported(
                64,
                LoweringPhase::SimulatorParser,
                "recursive systemverilog module instantiation",
                resource_table::get_str_value(instance.module_name).unwrap_or_default(),
                None,
            ));
        }
        let Some(child) = lowered_modules.get(&child_id) else {
            return Err(unsupported_sv_instance(instance.module_name));
        };
        ensure_parent_output_signals(
            module,
            &mut parent_variables,
            &mut signal_names,
            child,
            &instance.port_connections,
        );
        let glue = build_instance_glue(
            &parent_variables,
            &signal_names,
            child,
            &instance.port_connections,
        )?;
        module
            .glue_blocks
            .entry(instance.instance_name)
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

fn ensure_parent_output_signals(
    parent: &mut SimModule,
    parent_variables: &mut HashMap<VarId, SvVariable>,
    parent_signal_names: &mut HashMap<String, VarId>,
    child: &LoweredSvModule,
    connections: &[LoweredSvPortConnection],
) {
    for child_port_id in &child.port_order {
        let child_var = &child.variables[child_port_id];
        if child_var.kind != VarKind::Output {
            continue;
        }
        let formal = child_var.path.to_string();
        let actual = connections
            .iter()
            .find(|connection| connection.formal == formal)
            .map(|connection| connection.actual.as_str())
            .unwrap_or(formal.as_str());
        if parent_signal_names.contains_key(actual) {
            continue;
        }
        let mut next_id = VarId::default();
        while parent.variables.contains_key(&next_id) {
            next_id.inc();
        }
        parent_signal_names.insert(actual.to_string(), next_id);
        let variable = SvVariable {
            id: next_id,
            path: VarPath::new(resource_table::insert_str(actual)),
            width: child_var.width,
            signed: child_var.signed,
            is_4state: child_var.is_4state,
            domain_kind: DomainKind::Other,
            kind: VarKind::Variable,
            type_kind: child_var.type_kind,
            token: child_var.token,
        };
        parent
            .variables
            .insert(next_id, variable.to_shared_variable());
        parent_variables.insert(next_id, variable);
    }
}

type SvGlue = (
    Vec<(Vec<VarId>, LogicPath<GlueAddr>)>,
    Vec<(Vec<VarId>, LogicPath<GlueAddr>)>,
    SLTNodeArena<GlueAddr>,
);

fn build_instance_glue(
    parent_variables: &HashMap<VarId, SvVariable>,
    parent_signal_names: &HashMap<String, VarId>,
    child: &LoweredSvModule,
    connections: &[LoweredSvPortConnection],
) -> Result<SvGlue, ParserError> {
    let mut input_ports = Vec::new();
    let mut output_ports = Vec::new();
    let mut arena = SLTNodeArena::<GlueAddr>::new();

    for child_port_id in &child.port_order {
        let child_var = &child.variables[child_port_id];
        let formal = child_var.path.to_string();
        let connection = connections
            .iter()
            .find(|connection| connection.formal == formal);
        let actual = connection
            .map(|connection| connection.actual.as_str())
            .unwrap_or(formal.as_str());
        let width = child_var.width;
        match child_var.kind {
            VarKind::Input => {
                let actual_expr = connection
                    .and_then(|connection| connection.actual_expr.as_ref())
                    .cloned()
                    .unwrap_or_else(|| sv::ir::Expr::Ident(actual.to_string()));
                let (expr, sources, source_ids) = lower_glue_parent_expr(
                    &actual_expr,
                    parent_variables,
                    parent_signal_names,
                    &std::collections::HashMap::new(),
                    &mut arena,
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
                let expr_width = celox_slt::get_width(expr, &arena);
                let expr = if width == expr_width {
                    expr
                } else {
                    arena.alloc(SLTNode::Slice {
                        expr,
                        access: BitAccess::new(0, width - 1),
                    })?
                };
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
                        pre_lower_nodes: Vec::new(),
                    },
                ));
            }
            VarKind::Output => {
                let Some(parent_signal_id) = parent_signal_names.get(actual).copied() else {
                    continue;
                };
                let parent_var = &parent_variables[&parent_signal_id];
                let child_node = arena.alloc(SLTNode::Input {
                    variable: GlueAddr::Child(*child_port_id),
                    signed: child_var.signed,
                    index: Vec::new(),
                    access: BitAccess::new(0, width - 1),
                })?;
                let expr = if width == parent_var.width {
                    child_node
                } else {
                    arena.alloc(SLTNode::Slice {
                        expr: child_node,
                        access: BitAccess::new(0, parent_var.width - 1),
                    })?
                };
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
                            0,
                            parent_var.width - 1,
                        )),
                        expr,
                        sources,
                        address_sources: HashSet::default(),
                        previous_sources: HashSet::default(),
                        local_inputs: Vec::new(),
                        order_before: HashSet::default(),
                        comb_capture_enable_sites: Vec::new(),
                        pre_lower_nodes: Vec::new(),
                    },
                ));
            }
            VarKind::Inout => {
                return Err(unsupported_sv_inout(
                    child_var.path.to_string(),
                    &child_var.token,
                ));
            }
            _ => {}
        }
    }

    Ok((input_ports, output_ports, arena))
}

fn lower_glue_parent_expr(
    expr: &sv::ir::Expr,
    variables: &HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
    constants: &std::collections::HashMap<String, i128>,
    arena: &mut SLTNodeArena<GlueAddr>,
) -> Option<(
    celox_slt::NodeId,
    HashSet<VarAtomBase<GlueAddr>>,
    Vec<VarId>,
)> {
    match expr {
        sv::ir::Expr::Ident(name) => {
            let Some(id) = name_to_id.get(name).copied() else {
                let value = constants.get(name)?;
                return Some((
                    arena
                        .alloc(SLTNode::Constant(
                            BigUint::from(*value as u128),
                            BigUint::from(0u32),
                            32,
                            false,
                        ))
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
            Some((node, sources, vec![id]))
        }
        sv::ir::Expr::Select { expr, msb, lsb } => {
            let (inner, sources, source_ids) =
                lower_glue_parent_expr(expr, variables, name_to_id, constants, arena)?;
            let msb_value = sv::typecheck::eval_const_expr(msb, constants)?;
            let lsb_value = sv::typecheck::eval_const_expr(lsb, constants)?;
            let msb = usize::try_from(msb_value.max(lsb_value)).ok()?;
            let lsb = usize::try_from(msb_value.min(lsb_value)).ok()?;
            let access = BitAccess::new(lsb, msb);
            let node = arena
                .alloc(SLTNode::Slice {
                    expr: inner,
                    access,
                })
                .ok()?;
            let sources = sources
                .into_iter()
                .map(|source| VarAtomBase::new(source.id, access.lsb, access.msb))
                .collect();
            Some((node, sources, source_ids))
        }
        sv::ir::Expr::Concat(parts) => {
            let mut nodes = Vec::new();
            let mut sources = HashSet::default();
            let mut source_ids = Vec::new();
            for part in parts {
                let (node, part_sources, part_source_ids) =
                    lower_glue_parent_expr(part, variables, name_to_id, constants, arena)?;
                let width = celox_slt::get_width(node, arena);
                nodes.push((node, width));
                sources.extend(part_sources);
                source_ids.extend(part_source_ids);
            }
            source_ids.sort();
            source_ids.dedup();
            Some((
                arena.alloc(SLTNode::Concat(nodes)).ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::RepeatConcat { count, parts } => {
            let count = sv::typecheck::eval_const_expr(count, constants)?;
            let count = usize::try_from(count).ok()?;
            let mut nodes = Vec::new();
            let mut sources = HashSet::default();
            let mut source_ids = Vec::new();
            for _ in 0..count {
                for part in parts {
                    let (node, part_sources, part_source_ids) =
                        lower_glue_parent_expr(part, variables, name_to_id, constants, arena)?;
                    let width = celox_slt::get_width(node, arena);
                    nodes.push((node, width));
                    sources.extend(part_sources);
                    source_ids.extend(part_source_ids);
                }
            }
            source_ids.sort();
            source_ids.dedup();
            Some((
                arena.alloc(SLTNode::Concat(nodes)).ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Resize {
            expr,
            width,
            signed,
        } => {
            let (inner, sources, source_ids) =
                lower_glue_parent_expr(expr, variables, name_to_id, constants, arena)?;
            let resized = coerce_node_width(arena, inner, Some(*width), *signed).ok()?;
            Some((resized, sources, source_ids))
        }
        sv::ir::Expr::Literal(literal) => {
            let literal = sv::typecheck::parse_integral_literal(literal)?;
            Some((
                arena
                    .alloc(SLTNode::Constant(
                        literal.value,
                        literal.mask,
                        literal.width,
                        literal.signed,
                    ))
                    .ok()?,
                HashSet::default(),
                Vec::new(),
            ))
        }
        sv::ir::Expr::Unary { op, expr } => {
            let (inner, sources, source_ids) =
                lower_glue_parent_expr(expr, variables, name_to_id, constants, arena)?;
            Some((
                arena
                    .alloc(SLTNode::Unary(unary_op_from_sv(*op)?, inner))
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Binary { left, op, right } => {
            let operands_signed = sv_expr_is_signed(left, variables, name_to_id)
                && sv_expr_is_signed(right, variables, name_to_id);
            let context_sized_comparison = matches!(
                op,
                sv::ir::BinaryOp::EqCase
                    | sv::ir::BinaryOp::NeCase
                    | sv::ir::BinaryOp::EqWildcard
                    | sv::ir::BinaryOp::NeWildcard
            );
            let left_fill = context_sized_comparison
                .then(|| expr_unbased_fill_literal(left))
                .flatten();
            let right_fill = context_sized_comparison
                .then(|| expr_unbased_fill_literal(right))
                .flatten();
            let (
                (mut left, mut sources, mut source_ids),
                (mut right, right_sources, right_source_ids),
            ) = match (left_fill, right_fill) {
                (Some(left_fill), Some(right_fill)) => (
                    (
                        lower_unbased_fill_literal_slt(arena, left_fill, 1)?,
                        HashSet::default(),
                        Vec::new(),
                    ),
                    (
                        lower_unbased_fill_literal_slt(arena, right_fill, 1)?,
                        HashSet::default(),
                        Vec::new(),
                    ),
                ),
                (Some(fill), None) => {
                    let right =
                        lower_glue_parent_expr(right, variables, name_to_id, constants, arena)?;
                    let width = celox_slt::get_width(right.0, arena);
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
                    let left =
                        lower_glue_parent_expr(left, variables, name_to_id, constants, arena)?;
                    let width = celox_slt::get_width(left.0, arena);
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
                    lower_glue_parent_expr(left, variables, name_to_id, constants, arena)?,
                    lower_glue_parent_expr(right, variables, name_to_id, constants, arena)?,
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
                    .alloc(SLTNode::Binary(left, binary_op_from_sv(*op), right))
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Mux { .. } | sv::ir::Expr::Call { .. } => None,
    }
}

fn next_var_id(next_id: &mut VarId) -> VarId {
    let id = *next_id;
    next_id.inc();
    id
}

fn signal_kind_from_port_direction(direction: sv::ir::PortDirection) -> VarKind {
    match direction {
        sv::ir::PortDirection::Input => VarKind::Input,
        sv::ir::PortDirection::Output => VarKind::Output,
        sv::ir::PortDirection::Inout => VarKind::Inout,
        sv::ir::PortDirection::Ref | sv::ir::PortDirection::Unspecified => VarKind::Variable,
    }
}

struct SvSignalType {
    width: usize,
    signed: bool,
    is_4state: bool,
    type_kind: PortTypeKind,
}

fn signal_type_from_sv(
    typ: &sv::ir::Type,
    constants: &std::collections::HashMap<String, i128>,
) -> SvSignalType {
    let width = sv::typecheck::resolve_packed_width_with_env(typ.packed_ranges(), constants)
        .or_else(|| typ.resolved_width())
        .unwrap_or(1)
        .max(1);
    let signed = typ.is_signed();
    let is_4state = !matches!(typ.kind(), sv::ir::TypeKind::Bit);
    let type_kind = match typ.kind() {
        sv::ir::TypeKind::Bit => PortTypeKind::Bit,
        sv::ir::TypeKind::Logic | sv::ir::TypeKind::Reg | sv::ir::TypeKind::Implicit => {
            PortTypeKind::Logic
        }
    };
    SvSignalType {
        width,
        signed,
        is_4state,
        type_kind,
    }
}

fn lower_comb_process(
    process: &sv::ir::CombProcess,
    variables: &HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
    constants: &std::collections::HashMap<String, i128>,
    arena: &mut SLTNodeArena<VarId>,
) -> Vec<LogicPath<VarId>> {
    process
        .assignments()
        .iter()
        .filter_map(|assignment| {
            lower_assignment(assignment, variables, name_to_id, constants, arena)
        })
        .collect()
}

fn lower_assignment(
    assignment: &sv::ir::Assignment,
    variables: &HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
    constants: &std::collections::HashMap<String, i128>,
    arena: &mut SLTNodeArena<VarId>,
) -> Option<LogicPath<VarId>> {
    let target = lower_lvalue_target(assignment.lhs_value(), variables, name_to_id, constants)?;
    let (expr, sources) = lower_expr(assignment.rhs(), variables, name_to_id, constants, arena)?;
    let target_width = target
        .var()
        .map(|target| target.access.msb - target.access.lsb + 1)?;
    let expr = coerce_node_width(arena, expr, Some(target_width), false).ok()?;
    Some(LogicPath {
        target,
        expr,
        sources,
        address_sources: HashSet::default(),
        previous_sources: HashSet::default(),
        local_inputs: Vec::new(),
        order_before: HashSet::default(),
        comb_capture_enable_sites: Vec::new(),
        pre_lower_nodes: Vec::new(),
    })
}

fn lower_lvalue_target(
    lvalue: &sv::ir::LValue,
    variables: &HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
    constants: &std::collections::HashMap<String, i128>,
) -> Option<LogicPathTarget<VarId>> {
    let target_id = *name_to_id.get(lvalue.name())?;
    let target_width = variables.get(&target_id)?.width;
    let (lsb, msb) = match lvalue {
        sv::ir::LValue::Ident(_) => (0, target_width.checked_sub(1)?),
        sv::ir::LValue::Select { msb, lsb, .. } => {
            let msb = sv::typecheck::eval_const_expr(msb, constants)?;
            let lsb = sv::typecheck::eval_const_expr(lsb, constants)?;
            let msb = usize::try_from(msb).ok()?;
            let lsb = usize::try_from(lsb).ok()?;
            (lsb, msb)
        }
    };
    (lsb <= msb && msb < target_width)
        .then(|| LogicPathTarget::Var(VarAtomBase::new(target_id, lsb, msb)))
}

fn lower_expr(
    expr: &sv::ir::Expr,
    variables: &HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
    constants: &std::collections::HashMap<String, i128>,
    arena: &mut SLTNodeArena<VarId>,
) -> Option<(celox_slt::NodeId, HashSet<VarAtomBase<VarId>>)> {
    match expr {
        sv::ir::Expr::Ident(name) => {
            let Some(id) = name_to_id.get(name).copied() else {
                let value = constants.get(name)?;
                return Some((
                    arena
                        .alloc(SLTNode::Constant(
                            BigUint::from(*value as u128),
                            BigUint::from(0u32),
                            32,
                            false,
                        ))
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
            Some((node, sources))
        }
        sv::ir::Expr::Select { expr, msb, lsb } => {
            let (inner, mut sources) = lower_expr(expr, variables, name_to_id, constants, arena)?;
            let msb_value = sv::typecheck::eval_const_expr(msb, constants)?;
            let lsb_value = sv::typecheck::eval_const_expr(lsb, constants)?;
            let msb = usize::try_from(msb_value.max(lsb_value)).ok()?;
            let lsb = usize::try_from(msb_value.min(lsb_value)).ok()?;
            let access = BitAccess::new(lsb, msb);
            let node = arena
                .alloc(SLTNode::Slice {
                    expr: inner,
                    access,
                })
                .ok()?;
            sources = sources
                .into_iter()
                .map(|source| VarAtomBase::new(source.id, access.lsb, access.msb))
                .collect();
            Some((node, sources))
        }
        sv::ir::Expr::Concat(parts) => {
            let mut nodes = Vec::new();
            let mut sources = HashSet::default();
            for part in parts {
                let (node, part_sources) =
                    lower_expr(part, variables, name_to_id, constants, arena)?;
                let width = celox_slt::get_width(node, arena);
                nodes.push((node, width));
                sources.extend(part_sources);
            }
            Some((arena.alloc(SLTNode::Concat(nodes)).ok()?, sources))
        }
        sv::ir::Expr::RepeatConcat { count, parts } => {
            let count = sv::typecheck::eval_const_expr(count, constants)?;
            let count = usize::try_from(count).ok()?;
            let mut repeated = Vec::new();
            let mut sources = HashSet::default();
            for _ in 0..count {
                for part in parts {
                    let (node, part_sources) =
                        lower_expr(part, variables, name_to_id, constants, arena)?;
                    let width = celox_slt::get_width(node, arena);
                    repeated.push((node, width));
                    sources.extend(part_sources);
                }
            }
            Some((arena.alloc(SLTNode::Concat(repeated)).ok()?, sources))
        }
        sv::ir::Expr::Literal(literal) => {
            let literal = sv::typecheck::parse_integral_literal(literal)?;
            Some((
                arena
                    .alloc(SLTNode::Constant(
                        literal.value,
                        literal.mask,
                        literal.width,
                        literal.signed,
                    ))
                    .ok()?,
                HashSet::default(),
            ))
        }
        sv::ir::Expr::Unary { op, expr } => {
            let (inner, sources) = lower_expr(expr, variables, name_to_id, constants, arena)?;
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
            let (inner, sources) = lower_expr(expr, variables, name_to_id, constants, arena)?;
            let resized = coerce_node_width(arena, inner, Some(*width), *signed).ok()?;
            Some((resized, sources))
        }
        sv::ir::Expr::Binary { left, op, right } => {
            let operands_signed = sv_expr_is_signed(left, variables, name_to_id)
                && sv_expr_is_signed(right, variables, name_to_id);
            let context_sized_comparison = matches!(
                op,
                sv::ir::BinaryOp::EqCase
                    | sv::ir::BinaryOp::NeCase
                    | sv::ir::BinaryOp::EqWildcard
                    | sv::ir::BinaryOp::NeWildcard
            );
            let left_fill = context_sized_comparison
                .then(|| expr_unbased_fill_literal(left))
                .flatten();
            let right_fill = context_sized_comparison
                .then(|| expr_unbased_fill_literal(right))
                .flatten();
            let ((mut left, mut sources), (mut right, right_sources)) =
                match (left_fill, right_fill) {
                    (Some(left_fill), Some(right_fill)) => (
                        (
                            lower_unbased_fill_literal_slt(arena, left_fill, 1)?,
                            HashSet::default(),
                        ),
                        (
                            lower_unbased_fill_literal_slt(arena, right_fill, 1)?,
                            HashSet::default(),
                        ),
                    ),
                    (Some(fill), None) => {
                        let right = lower_expr(right, variables, name_to_id, constants, arena)?;
                        let width = celox_slt::get_width(right.0, arena);
                        (
                            (
                                lower_unbased_fill_literal_slt(arena, fill, width)?,
                                HashSet::default(),
                            ),
                            right,
                        )
                    }
                    (None, Some(fill)) => {
                        let left = lower_expr(left, variables, name_to_id, constants, arena)?;
                        let width = celox_slt::get_width(left.0, arena);
                        (
                            left,
                            (
                                lower_unbased_fill_literal_slt(arena, fill, width)?,
                                HashSet::default(),
                            ),
                        )
                    }
                    (None, None) => (
                        lower_expr(left, variables, name_to_id, constants, arena)?,
                        lower_expr(right, variables, name_to_id, constants, arena)?,
                    ),
                };
            sources.extend(right_sources);
            if context_sized_comparison {
                let common_width =
                    celox_slt::get_width(left, arena).max(celox_slt::get_width(right, arena));
                left = coerce_node_width(arena, left, Some(common_width), operands_signed).ok()?;
                right =
                    coerce_node_width(arena, right, Some(common_width), operands_signed).ok()?;
            }
            Some((
                arena
                    .alloc(SLTNode::Binary(left, binary_op_from_sv(*op), right))
                    .ok()?,
                sources,
            ))
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let (condition, mut sources) =
                lower_expr(condition, variables, name_to_id, constants, arena)?;
            let (then_expr, then_sources) =
                lower_expr(then_expr, variables, name_to_id, constants, arena)?;
            let (else_expr, else_sources) =
                lower_expr(else_expr, variables, name_to_id, constants, arena)?;
            sources.extend(then_sources);
            sources.extend(else_sources);
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

type SvFfBlocks = (
    HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    HashMap<VarId, VarId>,
);

fn lower_ff_processes(
    module: &sv::ir::Module,
    variables: &HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
    constants: &std::collections::HashMap<String, i128>,
) -> SvFfBlocks {
    let mut eval_only_ff_blocks = HashMap::default();
    let mut apply_ff_blocks = HashMap::default();
    let mut eval_apply_ff_blocks = HashMap::default();
    let mut reset_clock_map = HashMap::default();

    for process in module.ff_processes() {
        let Some(trigger_set) = trigger_set_from_ff_events(process.events(), name_to_id) else {
            continue;
        };
        for reset in &trigger_set.resets {
            reset_clock_map.insert(*reset, trigger_set.clock);
        }
        let Some((eval_only, apply, eval_apply)) =
            lower_ff_process(process, &trigger_set, variables, name_to_id, constants)
        else {
            continue;
        };
        eval_only_ff_blocks.insert(trigger_set.clone(), eval_only);
        apply_ff_blocks.insert(trigger_set.clone(), apply);
        eval_apply_ff_blocks.insert(trigger_set, eval_apply);
    }

    (
        eval_only_ff_blocks,
        apply_ff_blocks,
        eval_apply_ff_blocks,
        reset_clock_map,
    )
}

fn trigger_set_from_ff_events(
    events: &[sv::ir::FfEvent],
    name_to_id: &HashMap<String, VarId>,
) -> Option<TriggerSet<VarId>> {
    let clock = events
        .iter()
        .find(|event| event.edge() == sv::ir::FfEdge::Pos)
        .or_else(|| events.first())?;
    let clock_id = *name_to_id.get(clock.signal())?;
    let resets = events
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
    trigger_set: &TriggerSet<VarId>,
    variables: &HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
    constants: &std::collections::HashMap<String, i128>,
) -> Option<(
    ExecutionUnit<RegionedVarAddr>,
    ExecutionUnit<RegionedVarAddr>,
    ExecutionUnit<RegionedVarAddr>,
)> {
    let targets = ff_targets(process, variables, name_to_id, constants)?;
    let mut eval_builder = SIRBuilder::new();
    emit_ff_seeds(&mut eval_builder, &targets);
    emit_ff_assignment_stores(
        &mut eval_builder,
        process,
        &targets,
        variables,
        name_to_id,
        constants,
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
    variables: &HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
    constants: &std::collections::HashMap<String, i128>,
) -> Option<Vec<VarAtomBase<VarId>>> {
    let mut targets = Vec::new();
    for assignment in process.assignments() {
        let target = lvalue_atom(
            assignment.assignment().lhs_value(),
            variables,
            name_to_id,
            constants,
        )?;
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    Some(targets)
}

fn emit_ff_seeds(builder: &mut SIRBuilder<RegionedVarAddr>, targets: &[VarAtomBase<VarId>]) {
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

fn emit_ff_commits(builder: &mut SIRBuilder<RegionedVarAddr>, targets: &[VarAtomBase<VarId>]) {
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
    targets: &[VarAtomBase<VarId>],
    variables: &HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
    constants: &std::collections::HashMap<String, i128>,
) -> Option<()> {
    let mut target_ids = Vec::new();
    for target in targets {
        if !target_ids.contains(&target.id) {
            target_ids.push(target.id);
        }
    }

    for target_id in target_ids {
        let width = variables.get(&target_id)?.width;
        let mut value = builder.alloc_logic(width);
        builder.emit(SIRInstruction::Load(
            value,
            RegionedVarAddrBase {
                region: STABLE_REGION,
                var_id: target_id,
            },
            SIROffset::Static(0),
            width,
        ));
        for assignment in process.assignments() {
            let target = lvalue_atom(
                assignment.assignment().lhs_value(),
                variables,
                name_to_id,
                constants,
            )?;
            if target.id != target_id {
                continue;
            }
            let target_width = target.access.msb - target.access.lsb + 1;
            let rhs_expr = assignment.assignment().rhs();
            let rhs = match rhs_expr {
                sv::ir::Expr::Literal(literal) => match unbased_fill_literal(literal) {
                    Some(fill) => lower_unbased_fill_literal(builder, fill, target_width)?,
                    None => {
                        let rhs =
                            lower_expr_to_sir(builder, rhs_expr, variables, name_to_id, constants)?;
                        resize_sir_register(
                            builder,
                            rhs,
                            target_width,
                            sv_expr_is_signed(rhs_expr, variables, name_to_id),
                        )?
                    }
                },
                _ => {
                    let rhs =
                        lower_expr_to_sir(builder, rhs_expr, variables, name_to_id, constants)?;
                    resize_sir_register(
                        builder,
                        rhs,
                        target_width,
                        sv_expr_is_signed(rhs_expr, variables, name_to_id),
                    )?
                }
            };
            let assigned =
                replace_sir_slice(builder, value, rhs, target.access.lsb, target_width, width)?;
            value = match assignment.condition() {
                Some(condition) => {
                    let condition =
                        lower_expr_to_sir(builder, condition, variables, name_to_id, constants)?;
                    let mux = builder.alloc_logic(width);
                    builder.emit(SIRInstruction::Mux(mux, condition, assigned, value));
                    mux
                }
                None => assigned,
            };
        }
        builder.emit(SIRInstruction::Store(
            RegionedVarAddrBase {
                region: WORKING_REGION,
                var_id: target_id,
            },
            SIROffset::Static(0),
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }
    Some(())
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
    variables: &HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
    constants: &std::collections::HashMap<String, i128>,
) -> Option<VarAtomBase<VarId>> {
    let id = *name_to_id.get(lvalue.name())?;
    let width = variables.get(&id)?.width;
    match lvalue {
        sv::ir::LValue::Ident(_) => Some(VarAtomBase::new(id, 0, width.checked_sub(1)?)),
        sv::ir::LValue::Select { msb, lsb, .. } => {
            let msb = sv::typecheck::eval_const_expr(msb, constants)?;
            let lsb = sv::typecheck::eval_const_expr(lsb, constants)?;
            let high = usize::try_from(msb.max(lsb)).ok()?;
            let low = usize::try_from(msb.min(lsb)).ok()?;
            (low <= high && high < width).then(|| VarAtomBase::new(id, low, high))
        }
    }
}

fn sv_expr_is_signed(
    expr: &sv::ir::Expr,
    variables: &HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
) -> bool {
    match expr {
        sv::ir::Expr::Ident(name) => name_to_id
            .get(name)
            .and_then(|id| variables.get(id))
            .is_some_and(|variable| variable.signed),
        sv::ir::Expr::Literal(literal) => {
            sv::typecheck::parse_integral_literal(literal).is_some_and(|literal| literal.signed)
        }
        sv::ir::Expr::Resize { signed, .. } => *signed,
        sv::ir::Expr::Select { .. }
        | sv::ir::Expr::Concat(_)
        | sv::ir::Expr::RepeatConcat { .. }
        | sv::ir::Expr::Call { .. } => false,
        sv::ir::Expr::Unary { op, expr } => {
            matches!(
                op,
                sv::ir::UnaryOp::Plus | sv::ir::UnaryOp::Minus | sv::ir::UnaryOp::BitNot
            ) && sv_expr_is_signed(expr, variables, name_to_id)
        }
        sv::ir::Expr::Binary { left, op, right } => match op {
            sv::ir::BinaryOp::Shl | sv::ir::BinaryOp::Shr => {
                sv_expr_is_signed(left, variables, name_to_id)
            }
            sv::ir::BinaryOp::Add
            | sv::ir::BinaryOp::Sub
            | sv::ir::BinaryOp::Mul
            | sv::ir::BinaryOp::Div
            | sv::ir::BinaryOp::Mod
            | sv::ir::BinaryOp::BitAnd
            | sv::ir::BinaryOp::BitOr
            | sv::ir::BinaryOp::BitXor => {
                sv_expr_is_signed(left, variables, name_to_id)
                    && sv_expr_is_signed(right, variables, name_to_id)
            }
            _ => false,
        },
        sv::ir::Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => {
            sv_expr_is_signed(then_expr, variables, name_to_id)
                && sv_expr_is_signed(else_expr, variables, name_to_id)
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
    variables: &HashMap<VarId, SvVariable>,
    name_to_id: &HashMap<String, VarId>,
    constants: &std::collections::HashMap<String, i128>,
) -> Option<celox_sir::RegisterId> {
    match expr {
        sv::ir::Expr::Ident(name) => {
            let Some(id) = name_to_id.get(name).copied() else {
                let value = constants.get(name)?;
                let reg = builder.alloc_logic(32);
                builder.emit(SIRInstruction::Imm(
                    reg,
                    SIRValue::new_four_state(*value as u128, 0u32),
                ));
                return Some(reg);
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
                SIROffset::Static(0),
                var.width,
            ));
            Some(reg)
        }
        sv::ir::Expr::Literal(literal) => {
            let literal = sv::typecheck::parse_integral_literal(literal)?;
            let reg = builder.alloc_logic(literal.width);
            builder.emit(SIRInstruction::Imm(
                reg,
                SIRValue::new_four_state(literal.value, literal.mask),
            ));
            Some(reg)
        }
        sv::ir::Expr::Select { expr, msb, lsb } => {
            let inner = lower_expr_to_sir(builder, expr, variables, name_to_id, constants)?;
            let msb = sv::typecheck::eval_const_expr(msb, constants)?;
            let lsb = sv::typecheck::eval_const_expr(lsb, constants)?;
            let high = usize::try_from(msb.max(lsb)).ok()?;
            let low = usize::try_from(msb.min(lsb)).ok()?;
            let width = high - low + 1;
            let reg = builder.alloc_logic(width);
            builder.emit(SIRInstruction::Slice(reg, inner, low, width));
            Some(reg)
        }
        sv::ir::Expr::Resize {
            expr,
            width,
            signed,
        } => {
            let inner = lower_expr_to_sir(builder, expr, variables, name_to_id, constants)?;
            resize_sir_register(builder, inner, *width, *signed)
        }
        sv::ir::Expr::Unary { op, expr } => {
            let inner = lower_expr_to_sir(builder, expr, variables, name_to_id, constants)?;
            let width = builder.register(&inner).width();
            let reg = if matches!(op, sv::ir::UnaryOp::ToTwoState) {
                builder.alloc_bit(width, false)
            } else {
                builder.alloc_logic(width)
            };
            builder.emit(SIRInstruction::Unary(reg, unary_op_from_sv(*op)?, inner));
            Some(reg)
        }
        sv::ir::Expr::Binary { left, op, right } => {
            let operands_signed = sv_expr_is_signed(left, variables, name_to_id)
                && sv_expr_is_signed(right, variables, name_to_id);
            let right_fill = match &**right {
                sv::ir::Expr::Literal(literal) => unbased_fill_literal(literal),
                _ => None,
            };
            let left_fill = match &**left {
                sv::ir::Expr::Literal(literal) => unbased_fill_literal(literal),
                _ => None,
            };
            let (mut left, mut right) = if let Some(fill) = right_fill {
                let left = lower_expr_to_sir(builder, left, variables, name_to_id, constants)?;
                let width = builder.register(&left).width();
                (left, lower_unbased_fill_literal(builder, fill, width)?)
            } else if let Some(fill) = left_fill {
                let right = lower_expr_to_sir(builder, right, variables, name_to_id, constants)?;
                let width = builder.register(&right).width();
                (lower_unbased_fill_literal(builder, fill, width)?, right)
            } else {
                (
                    lower_expr_to_sir(builder, left, variables, name_to_id, constants)?,
                    lower_expr_to_sir(builder, right, variables, name_to_id, constants)?,
                )
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
                binary_op_from_sv(*op),
                right,
            ));
            Some(reg)
        }
        sv::ir::Expr::Concat(parts) => {
            let mut regs = Vec::new();
            for part in parts {
                regs.push(lower_expr_to_sir(
                    builder, part, variables, name_to_id, constants,
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
            let count = sv::typecheck::eval_const_expr(count, constants)?;
            let count = usize::try_from(count).ok()?;
            let mut regs = Vec::new();
            for _ in 0..count {
                for part in parts {
                    regs.push(lower_expr_to_sir(
                        builder, part, variables, name_to_id, constants,
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
            let condition =
                lower_expr_to_sir(builder, condition, variables, name_to_id, constants)?;
            let then_expr =
                lower_expr_to_sir(builder, then_expr, variables, name_to_id, constants)?;
            let else_expr =
                lower_expr_to_sir(builder, else_expr, variables, name_to_id, constants)?;
            let width = builder
                .register(&then_expr)
                .width()
                .max(builder.register(&else_expr).width());
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
) -> std::collections::HashMap<String, i128> {
    let override_values: std::collections::HashMap<&str, &sv::ir::ConstExpr> = parameter_overrides
        .iter()
        .filter_map(|parameter| {
            parameter
                .value
                .as_ref()
                .map(|value| (parameter.name.as_str(), value))
        })
        .collect();
    let mut constants = std::collections::HashMap::new();
    for parameter in module.parameters() {
        let value = override_values
            .get(parameter.name())
            .copied()
            .or_else(|| parameter.value())
            .and_then(|expr| sv::typecheck::eval_const_expr(expr, &constants))
            .or_else(|| parameter.resolved_value());
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

fn binary_op_from_sv(op: sv::ir::BinaryOp) -> BinaryOp {
    match op {
        sv::ir::BinaryOp::Add => BinaryOp::Add,
        sv::ir::BinaryOp::Sub => BinaryOp::Sub,
        sv::ir::BinaryOp::Mul => BinaryOp::Mul,
        sv::ir::BinaryOp::Div => BinaryOp::DivU,
        sv::ir::BinaryOp::Mod => BinaryOp::RemU,
        sv::ir::BinaryOp::Shl => BinaryOp::Shl,
        sv::ir::BinaryOp::Shr => BinaryOp::Shr,
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
        sv::ir::BinaryOp::Lt => BinaryOp::LtU,
        sv::ir::BinaryOp::Le => BinaryOp::LeU,
        sv::ir::BinaryOp::Gt => BinaryOp::GtU,
        sv::ir::BinaryOp::Ge => BinaryOp::GeU,
    }
}

pub(crate) fn sv_top_not_found(name: resource_table::StrId) -> ParserError {
    ParserError::TopNotFound {
        name: resource_table::get_str_value(name).unwrap_or_default(),
    }
}

pub(crate) fn unsupported_sv_instance(name: resource_table::StrId) -> ParserError {
    ParserError::unsupported(
        64,
        LoweringPhase::SimulatorParser,
        "systemverilog module instantiation",
        format!("name: \"{}\"", name),
        None,
    )
}

pub(crate) fn unsupported_sv_inout(path: String, token: &TokenRange) -> ParserError {
    ParserError::unsupported(
        64,
        LoweringPhase::SimulatorParser,
        "systemverilog inout port",
        path,
        Some(token),
    )
}
