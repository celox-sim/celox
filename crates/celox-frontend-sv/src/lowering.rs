//! SystemVerilog lowering adapter for the shared Celox frontend.
//!
//! SystemVerilog syntax and semantic analysis belongs in the
//! `celox-sv-analyzer` crate. This module converts analyzed SV into the shared
//! symbolic assembly model. It intentionally lives beside that assembly rather
//! than in the public `celox` facade or in a misleading frontend-to-frontend
//! dependency.

use std::collections::BTreeSet;
use std::path::Path;

use celox_design::{
    BinaryOp, BitAccess, DisplaySizing, DomainKind, ExternFunction, ExternSignature, ExternType,
    InitialStateData, InitialStateValue, ModuleId, NbaSite, PROCESS_DELAY_WIDTH,
    PROCESS_STATUS_WIDTH, PortTypeKind, ProcessSlots, RegionedVarAddrBase, ReportLocation,
    RuntimeErrorInfo, RuntimeEventKind, RuntimeEventSite, STABLE_REGION, Severity, TIME_SLOT_WIDTH,
    TriggerSet, UnaryOp, VarAtomBase, WORKING_REGION, WaitSite,
};
use celox_frontend_core::process::PROCESS_RESUME_WIDTH;
use celox_frontend_core::symbolic::artifact::{
    CombAlternative, ExternalHierarchy, ExternalModule, NbaSwitch, PackageBinding, SimModule,
    SymbolicGlueAddr as GlueAddr, SymbolicProcess, SymbolicRtl, SymbolicVariable,
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

mod case_equality;
mod comb;
mod comb_retention;
mod driver_validation;
mod drivers;
mod external_hierarchy;
mod ff;
mod floating;
mod hierarchy;
mod interfaces;
mod masked;
mod out_of_line;
mod packages;
mod procedural;
mod recursion;
mod schedule;
mod shared_variables;
mod sources;
mod specialize;
mod virtual_interfaces;

// The entry points the crate root exports.
pub use external_hierarchy::{
    ForeignModuleResolver, prepare_external_hierarchy, prepare_external_hierarchy_with_foreign,
    prepare_external_hierarchy_with_foreign_resolver,
    prepare_external_hierarchy_with_foreign_resolver_and_latch_policy,
    prepare_external_hierarchy_with_foreign_resolver_with_options,
    prepare_external_hierarchy_with_foreign_resolver_with_options_and_latch_policy,
};
pub use interfaces::{
    InterfaceSignature, interface_signatures, interface_signatures_for,
    interface_signatures_for_with_options,
};
pub use schedule::{
    schedule_sources, schedule_sources_with_force_signals,
    schedule_sources_with_force_signals_and_latch_policy, schedule_sources_with_latch_policy,
    schedule_sources_with_options, schedule_sources_with_parameter_expressions,
    schedule_sources_with_parameter_expressions_and_latch_policy,
};

// Every submodule reaches the items of these through `use super::*`.
use driver_validation::*;
use drivers::*;
use interfaces::*;
use packages::*;
use shared_variables::*;
use sources::*;
use specialize::*;

type RegionedVarAddr = RegionedVarAddrBase<SourceVarId>;
type GlueBlock = GlueBlockBase<SourceVarId>;
const MAX_SV_RECURSIVE_SPECIALIZATIONS: usize = 64;
/// The name of a module's [`SimModule::time_slot`].
const TIME_SLOT: &str = "$time";

#[derive(Debug, thiserror::Error)]
pub enum FrontendError {
    #[error(transparent)]
    Analyzer(#[from] sv::AnalyzerError),
    #[error(transparent)]
    Lowering(#[from] ParserError),
}

// These names cannot be authored as unescaped SV identifiers and are hidden
// from the source-facing hierarchy. The runtime image records their addresses.
const FORCE_ENABLE: &str = "@force-enable:";
const FORCE_VALUE: &str = "@force-value:";
/// The prefix of the variables `$event[k]` that hold the value of an event
/// expression, and of their copies of process-local operands.
const EVENT_PREFIX: &str = "$event[";

#[derive(Clone)]
struct SvVariable {
    float: Option<celox_sir::FloatPrecision>,
    /// What each element holds: packed bits, or a heap handle of a string
    /// or a class object.
    value_kind: celox_design::ValueKind,
    path: Vec<String>,
    width: usize,
    signed: bool,
    is_4state: bool,
    /// Flat structure storage is selected only through analyzer-checked member windows.
    is_unpacked_struct: bool,
    packed_ranges: Vec<(i128, i128)>,
    array_dims: Vec<usize>,
    unpacked_ranges: Vec<(i128, i128)>,
    domain_kind: DomainKind,
    kind: VariableKind,
    type_kind: PortTypeKind,
    source: Option<SourceLocation>,
    /// A procedural local or a lowering temporary: not addressable by name.
    hidden: bool,
    /// Compiler-generated external override slot: (target, is_enable).
    force_control: Option<(SourceVarId, bool)>,
    /// The variable whose sampled value this hidden copy holds
    /// (`SimModule::sample_shadows`).
    sampled_from: Option<SourceVarId>,
    /// The variable whose past sampled values this hidden register holds;
    /// it starts with the variable's value (`SimModule::initial_copies`).
    history_of: Option<SourceVarId>,
    /// A variable a process's lowering made to hold a value of one
    /// activation, such as an operand kept across a suspension. In a process
    /// with a frame (`celox_design::ProcessFrame::private`) each activation
    /// has its own; every other variable is the module's.
    activation_scratch: bool,
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
                value_kind: self.value_kind,
            },
            unpacked_ranges: self.unpacked_ranges.clone(),
            packed_dims: self
                .packed_ranges
                .iter()
                .map(|(left, right)| left.abs_diff(*right) as usize + 1)
                .collect(),
            source: self.source.clone(),
            module_affiliated: !self.hidden,
        }
    }

    /// A hidden variable other than the time slot, whose value the lowered
    /// code computes. The time slot and the sampled copies are state the
    /// scheduler writes, `$past` histories are state their process keeps,
    /// and an event expression's variables are state its process and its
    /// continuous assignment share.
    fn is_scratch(&self) -> bool {
        self.hidden
            && self.path != [TIME_SLOT]
            && self.force_control.is_none()
            && self.sampled_from.is_none()
            && self.history_of.is_none()
            && !self.path[0].starts_with(EVENT_PREFIX)
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
    /// Variables that denote a package variable, with its package and name.
    package_variables: Vec<(SourceVarId, String, String)>,
    /// The hidden `(copy, signal)` event copies of [`FfEventVars`].
    event_copies: Vec<(SourceVarId, SourceVarId)>,
    /// The continuous assignments of the event expressions the processes
    /// wait on, by their hidden target `$event[k]`.
    event_assignments: Vec<(SourceVarId, Vec<sv::ir::Stmt>)>,
    /// The write streams one flip-flop evaluation may write twice.
    repeated_ff_writes: Vec<FfWriteStream>,
    /// The `inout` ports whose net has its driver in this module or below it,
    /// filled in by [`resolve_inout_drivers`].
    inout_driven_inside: HashSet<String>,
    /// The calls the lowering inlined, when the recursion report is written.
    calls: Option<recursion::CallGraph>,
}

#[derive(Clone)]
struct AnalyzedSvModule {
    name: String,
    interface_member_paths: HashMap<String, sv::InterfaceMemberPath>,
    source: std::rc::Rc<sv::ParsedSource>,
    implicit_nets_allowed: bool,
    /// The positional interface of every module in all sources, used to bind
    /// positional port and parameter connections.
    interfaces: std::sync::Arc<sv::ModuleInterfaces>,
    /// The packages declared in all sources, analyzed once.
    packages: std::rc::Rc<sv::Packages>,
}

#[derive(Clone)]
pub(crate) struct LoweredSvInstance {
    pub module_name: String,
    pub instance_name: String,
    pub force_signals: Vec<String>,
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
    /// The data type resolved in the instantiating scope.
    pub bound_type: Option<sv::ast::Type>,
    pub bound_pattern: Option<sv::ast::BoundPattern>,
}

#[derive(Clone)]
pub(crate) struct LoweredSvPortConnection {
    pub formal: String,
    pub actual: String,
    pub actual_expr: Option<sv::ir::Expr>,
    pub bound_pattern: Option<sv::ast::BoundPattern>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LoweredSvModuleKey {
    pub name: String,
    pub force_signals: Vec<String>,
    pub parameter_overrides: Vec<LoweredSvParameterOverride>,
}

impl LoweredSvModuleKey {
    pub fn base(name: String) -> Self {
        Self {
            name,
            force_signals: Vec::new(),
            parameter_overrides: Vec::new(),
        }
    }

    pub fn instance_key(instance: &LoweredSvInstance) -> Self {
        let mut parameter_overrides = instance.parameter_overrides.clone();
        parameter_overrides.sort_by(|left, right| left.name.cmp(&right.name));
        Self {
            name: instance.module_name.clone(),
            force_signals: instance.force_signals.clone(),
            parameter_overrides,
        }
    }
}

fn append_error_warnings<T>(
    result: &Result<T, FrontendError>,
    warnings: &mut Vec<crate::warning::PendingWarning>,
) {
    if let Err(FrontendError::Analyzer(error)) = result {
        warnings.extend(crate::warning::analyzer_warnings(error.warnings()));
    }
}

/// A numeric override retains its integral type across frontend boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SvParameterValue {
    pub bits: u128,
    pub width: usize,
    pub signed: bool,
}

impl SvParameterValue {
    fn expression(self) -> sv::ir::ConstExpr {
        sv::ir::ConstExpr::Literal(format!(
            "{}'{}h{:x}",
            self.width.max(1),
            if self.signed { "s" } else { "" },
            self.bits
        ))
    }

    fn from_expression(expr: &sv::ir::ConstExpr) -> Option<Self> {
        let literal = sv::typecheck::eval_const_integral_literal_with_types(
            expr,
            &HashMap::default(),
            &HashMap::default(),
        )?;
        if literal.width > 128 || literal.mask != BigUint::default() {
            return None;
        }
        let digits = literal.value.to_u64_digits();
        let bits = u128::from(digits.first().copied().unwrap_or(0))
            | (u128::from(digits.get(1).copied().unwrap_or(0)) << 64);
        Some(Self {
            bits,
            width: literal.width,
            signed: literal.signed,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SvModuleRequest {
    pub alias: String,
    pub module: String,
    pub parameters: Vec<(String, SvParameterValue)>,
    pub interfaces: Vec<(String, String)>,
}

/// How `always @*` blocks that write a common variable run.
#[derive(Clone, Copy)]
struct SharedAlways<'a> {
    /// Make a variable a temporary of each block when that is unobservable
    /// within the module (the `shared_always_temporary` diagnostic; see
    /// [`share_always_variables`]).
    temporaries: bool,
    /// The module's signals that hierarchical names in other modules denote.
    foreign_references: &'a HashSet<String>,
}

impl Default for SharedAlways<'_> {
    fn default() -> Self {
        static EMPTY: std::sync::LazyLock<HashSet<String>> =
            std::sync::LazyLock::new(HashSet::default);
        Self {
            temporaries: false,
            foreign_references: &EMPTY,
        }
    }
}

/// What hierarchical names (IEEE 1800-2023 23.6) contribute to the
/// lowering of a specialization.
#[derive(Clone, Copy)]
struct HierarchicalNames<'a> {
    /// The signals of other instances that the module's names denote.
    signals: &'a [sv::ast::HierarchicalSignal],
    /// The module's signals that names in other modules write.
    foreign_writes: &'a HashMap<String, hierarchy::ForeignWrites>,
    /// How `always @*` blocks that write a common variable run.
    shared: SharedAlways<'a>,
    /// The analysis of the module with `signals`, if it is done already.
    analyzed: Option<&'a sv::Ir>,
    /// Where to append the module's recursive call cycles, if anywhere.
    recursion_report: Option<&'a std::path::Path>,
    /// Whether to lower each subroutine that can be out of line (see
    /// `AnalysisOptions::force_out_of_line_calls`).
    force_out_of_line: bool,
    /// The keys of the class properties that level waits of the design
    /// read, whose stores the module counts (see `ff::property_changes`).
    /// A wait that reads any other property is rejected.
    properties: &'a BTreeSet<u32>,
    /// The ids of the instances of each interface of the elaborated design,
    /// which the module's virtual interface accesses are repeated for (see
    /// `sv::ir::Module::expand_virtual_interface_instances`); none before
    /// the instances are known.
    virtual_ids: Option<&'a HashMap<String, Vec<u32>>>,
    /// A lowering that only discovers the hierarchy, before `properties`
    /// is known: the module counts the stores of the properties its own
    /// waits read, and the result is not simulated.
    discovery: bool,
}

impl Default for HierarchicalNames<'_> {
    fn default() -> Self {
        static EMPTY: std::sync::LazyLock<HashMap<String, hierarchy::ForeignWrites>> =
            std::sync::LazyLock::new(HashMap::default);
        static NO_PROPERTIES: BTreeSet<u32> = BTreeSet::new();
        Self {
            signals: &[],
            foreign_writes: &EMPTY,
            shared: SharedAlways::default(),
            analyzed: None,
            recursion_report: None,
            force_out_of_line: false,
            properties: &NO_PROPERTIES,
            virtual_ids: None,
            discovery: false,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_module(
    module: std::borrow::Cow<'_, sv::ir::Module>,
    four_state: bool,
    implicit_nets_allowed: bool,
    ff_parts: bool,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    force_signals: &[String],
    level_lists: LevelLists<'_>,
    foreign_writes: &HashMap<String, hierarchy::ForeignWrites>,
    reflected_paths: &HashMap<String, sv::InterfaceMemberPath>,
    shared: SharedAlways<'_>,
    calls: Option<&mut recursion::CallGraph>,
) -> Result<LoweredSvModule, sv::AnalyzerError> {
    lower_module_with_overrides(
        module,
        &[],
        four_state,
        implicit_nets_allowed,
        ff_parts,
        warnings,
        force_signals,
        level_lists,
        foreign_writes,
        reflected_paths,
        shared,
        calls,
    )
}

/// Which `always @(list)` processes are lowered like `always @*`.
#[derive(Clone, Copy)]
enum LevelLists<'a> {
    /// All of them stay processes.
    Processes,
    /// Every one whose list is its implicit event expression, except the
    /// demoted ones, by creation order, which take part in a combinational
    /// loop as `always @*`.
    Lowered { demoted: &'a HashSet<usize> },
}

fn needs_host_request(
    module: &sv::ir::Module,
    body: &[sv::ir::Stmt],
    visited: &mut HashSet<String>,
) -> bool {
    let mut found = false;
    let mut calls = Vec::new();
    for stmt in body {
        stmt.walk(&mut |stmt| {
            if let sv::ir::Stmt::SystemTask { task, .. } = stmt {
                found |= task.is_host_request();
            }
            procedural::stmt_calls(stmt, &mut calls);
        });
    }
    if found {
        return true;
    }
    let mut seen_defaults = procedural::SeenDefaults::default();
    while let Some((callee, args)) = calls.pop() {
        match callee {
            sv::ir::Callee::System(function) if function.is_host_request() => return true,
            sv::ir::Callee::String(sv::ir::StringOp::Format { .. }) => return true,
            sv::ir::Callee::Subroutine(name) => {
                let Some(subroutine) = module.subroutines().iter().find(|sub| sub.name == name)
                else {
                    continue;
                };
                // Different calls can select different defaults, even when the
                // subroutine body has already been inspected.
                let omitted: Vec<_> = (0..subroutine.params.len())
                    .map(|position| matches!(args.get(position), None | Some(None)))
                    .collect();
                if visited.insert(format!("defaults:{name}:{omitted:?}")) {
                    procedural::default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
                }
                if visited.insert(format!("body:{name}"))
                    && needs_host_request(module, &subroutine.body, visited)
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// An edge signal's role and the clock of the declaring process. Keep these
/// together when host requests move a process out of the retained FF list.
struct FfEventRole {
    signal: String,
    kind: DomainKind,
    clock: String,
    clock_kind: DomainKind,
}

/// `ff_parts` additionally keeps every `always_ff` process as an
/// independently evaluated part, for lane-partitioned builds.
#[allow(clippy::too_many_arguments)]
fn lower_module_with_overrides(
    module: std::borrow::Cow<'_, sv::ir::Module>,
    parameter_overrides: &[LoweredSvParameterOverride],
    four_state: bool,
    implicit_nets_allowed: bool,
    ff_parts: bool,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    force_signals: &[String],
    level_lists: LevelLists<'_>,
    foreign_writes: &HashMap<String, hierarchy::ForeignWrites>,
    reflected_paths: &HashMap<String, sv::InterfaceMemberPath>,
    shared: SharedAlways<'_>,
    mut calls: Option<&mut recursion::CallGraph>,
) -> Result<LoweredSvModule, sv::AnalyzerError> {
    let parameter_types = module_parameter_types(&module);
    let constants = module_constants_with_overrides(&module, parameter_overrides);
    // The module's processes are rewritten in place when the lowering owns
    // the module; a shared module is copied for the first rewrite.
    let mut module = module;
    if let LevelLists::Lowered { demoted } = level_lists
        && module.comb_processes().iter().any(|process| {
            process.kind() == sv::ir::CombProcessKind::AlwaysStar
                && demoted.contains(&process.creation_order())
        })
    {
        module = std::borrow::Cow::Owned(module.into_owned().run_comb_processes_as_processes(
            |_, process| demoted.contains(&process.creation_order()),
        ));
    }
    let sharing = share_always_variables(&module, &constants, &parameter_types, shared, warnings);
    let shared_process_variables = match sharing {
        Some((shared_module, names)) => {
            module = std::borrow::Cow::Owned(shared_module);
            Some(names)
        }
        None => None,
    };
    let shared_process_variables = shared_process_variables.as_ref();
    // The rewrite visits the edge processes in order, once each.
    let fallbacks = module
        .ff_processes()
        .iter()
        .map(|process| runs_as_process(&module, process, &constants, &parameter_types))
        .collect::<Vec<_>>();
    if fallbacks.contains(&true) {
        let mut fallbacks = fallbacks.into_iter();
        module = std::borrow::Cow::Owned(
            module
                .into_owned()
                .run_edge_processes_as_processes(|_| fallbacks.next().unwrap_or(false)),
        );
    }
    let module: &sv::ir::Module = &module;
    let mut ff_event_roles = Vec::new();
    for process in module.ff_processes() {
        let clock = clock_event_from_ff_process(process)
            .ok_or_else(|| sv::AnalyzerError::Unsupported("always_ff event control".into()))?;
        for event in process.events() {
            let kind = match (event.signal() == clock.signal(), event.edge()) {
                (true, sv::ir::FfEdge::Pos) => DomainKind::ClockPosedge,
                (true, sv::ir::FfEdge::Neg) => DomainKind::ClockNegedge,
                (false, sv::ir::FfEdge::Pos) => DomainKind::ResetAsyncHigh,
                (false, sv::ir::FfEdge::Neg) => DomainKind::ResetAsyncLow,
            };
            ff_event_roles.push(FfEventRole {
                signal: event.signal().to_string(),
                kind,
                clock: clock.signal().to_string(),
                clock_kind: match clock.edge() {
                    sv::ir::FfEdge::Pos => DomainKind::ClockPosedge,
                    sv::ir::FfEdge::Neg => DomainKind::ClockNegedge,
                },
            });
        }
    }
    let drivers = ModuleDrivers::new(module, &constants, &parameter_types);
    let shared_memories = shared_edge_memories(module, &drivers, foreign_writes, warnings);
    let shared_variables = shared_edge_variables(module, &drivers);
    let shared_all = shared_memories
        .union(&shared_variables)
        .chain(shared_process_variables.into_iter().flatten())
        .cloned()
        .collect();
    validate_variable_driver_ranges(module, &drivers, &shared_all)?;
    validate_foreign_drivers(module, &drivers, &shared_memories, foreign_writes)?;
    // The drivers above were scanned before the scheduled edges move, as
    // the roles were: the rare rewrite works on a copy.
    let scheduled = module
        .ff_processes()
        .iter()
        .any(|process| needs_host_request(module, process.body(), &mut HashSet::default()))
        .then(|| {
            module.clone().schedule_edge_processes(|process| {
                needs_host_request(module, process.body(), &mut HashSet::default())
            })
        });
    let module = scheduled.as_ref().unwrap_or(module);
    let mut package_variables = Vec::new();
    let name = module.name().to_string();
    let mut next_id = SourceVarId::default();
    let mut variables = HashMap::default();
    let mut name_to_id = HashMap::default();
    let mut port_order = Vec::new();
    let mut initial_memory_values = Vec::new();

    // Array-valued subroutine slots need to preserve their array shape in
    // captured arguments and results; keep their existing unsupported boundary.
    let string_array = |ty: &sv::ir::Type| {
        ty.kind() == sv::ir::TypeKind::String && !ty.unpacked_ranges().is_empty()
    };
    if module.subroutines().iter().any(|routine| {
        routine.return_type.as_ref().is_some_and(string_array)
            || routine
                .params
                .iter()
                .any(|param| string_array(&param.r#type))
    }) {
        return Err(sv::AnalyzerError::Unsupported(
            "fixed string arrays in subroutine interfaces".into(),
        ));
    }

    for port in module.ports() {
        if port.r#type().structure_identity().is_some() {
            continue;
        }
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
            float: type_info.float,

            value_kind: type_info.value_kind,
            path,
            width: type_info.width,
            signed: type_info.signed,
            is_4state: type_info.is_4state,
            is_unpacked_struct: type_info.is_unpacked_struct,
            packed_ranges: type_info.packed_ranges,
            array_dims: type_info.array_dims,
            unpacked_ranges: type_info.unpacked_ranges,
            domain_kind: DomainKind::Other,
            kind,
            type_kind: type_info.type_kind,
            source: None,
            hidden: false,
            force_control: None,
            sampled_from: None,
            history_of: None,
            activation_scratch: false,
        };
        name_to_id.insert(port.name().to_string(), id);
        port_order.push(id);
        if let Some(default) = port.default() {
            if !variable.array_dims.is_empty() {
                warnings.push((
                    crate::FrontendWarningKind::UnpackedPortDefault {
                        module: name.clone(),
                        port: port.name().to_string(),
                    },
                    None,
                ));
            }
            let literal = sv::typecheck::context_size_const_integral_literal(
                default,
                &constants,
                &parameter_types,
                type_info.width,
                type_info.signed,
            )
            .ok_or_else(|| {
                sv::AnalyzerError::Unsupported(format!(
                    "nonconstant input port default `{}`",
                    port.name()
                ))
            })?;
            initial_memory_values.push(InitialStateValue {
                address: id,
                data: InitialStateData::Packed {
                    value: literal.value,
                    mask: literal.mask,
                    written_mask: (BigUint::from(1u8) << type_info.width) - BigUint::from(1u8),
                },
            });
        } else if port.is_net() || type_info.is_4state {
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
        if signal.r#type().structure_identity().is_some()
            && signal.r#type().unpacked_ranges().is_empty()
        {
            continue;
        }
        if name_to_id.contains_key(signal.name()) {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "duplicate port or signal name `{}`",
                signal.name()
            )));
        }
        let id = next_var_id(&mut next_id);
        let type_info = signal_type_from_sv(signal.r#type(), &constants, &parameter_types)?;
        let path = vec![signal.name().to_string()];
        // A package variable is the package's object, not the module's.
        if let Some((package, variable)) = signal.package_variable() {
            package_variables.push((id, package.to_string(), variable.to_string()));
        }
        let variable = SvVariable {
            float: type_info.float,

            value_kind: type_info.value_kind,
            path,
            width: type_info.width,
            signed: type_info.signed,
            is_4state: type_info.is_4state,
            is_unpacked_struct: type_info.is_unpacked_struct,
            packed_ranges: type_info.packed_ranges,
            array_dims: type_info.array_dims,
            unpacked_ranges: type_info.unpacked_ranges,
            domain_kind: DomainKind::Other,
            kind: VariableKind::Variable,
            type_kind: type_info.type_kind,
            source: None,
            hidden: false,
            force_control: None,
            sampled_from: None,
            history_of: None,
            activation_scratch: false,
        };
        name_to_id.insert(signal.name().to_string(), id);
        // The package's object, or the other instance's, has its own
        // default value.
        if (signal.is_net() || type_info.is_4state)
            && signal.package_variable().is_none()
            && signal.hierarchical().is_none()
        {
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

    // Retain integral parameters for external observation, without adding them
    // to name_to_id: HDL expressions must still fold parameter references as
    // constants rather than introduce runtime loads.
    // Interface declarations are flattened into imported symbols, rather
    // than authored module parameters. Retain their instance-local values as
    // well; package imports remain owned by their package (IEEE 37.10).
    let interface_parameters = module
        .imported_parameters()
        .iter()
        .filter(|parameter| restore_interface_path(parameter.name(), reflected_paths).is_some());
    for parameter in module.parameters().iter().chain(interface_parameters) {
        let Some(width) = parameter.resolved_width().filter(|width| *width > 0) else {
            continue;
        };
        let signed = parameter.resolved_signed().unwrap_or(false);
        let expr = parameter.value().cloned().or_else(|| {
            constants
                .get(parameter.name())
                .copied()
                .map(const_expr_from_i128)
        });
        let Some(literal) = expr.and_then(|expr| {
            sv::typecheck::context_size_const_integral_literal(
                &expr,
                &constants,
                &parameter_types,
                width,
                signed,
            )
        }) else {
            continue;
        };
        let id = next_var_id(&mut next_id);
        let written_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        initial_memory_values.push(InitialStateValue {
            address: id,
            data: InitialStateData::Packed {
                value: literal.value,
                mask: literal.mask,
                written_mask,
            },
        });
        variables.insert(
            id,
            SvVariable {
                float: None,

                value_kind: celox_design::ValueKind::Bits,
                path: vec![parameter.name().to_string()],
                width,
                signed,
                is_4state: true,
                is_unpacked_struct: false,
                packed_ranges: vec![(width as i128 - 1, 0)],
                array_dims: Vec::new(),
                unpacked_ranges: Vec::new(),
                domain_kind: DomainKind::Other,
                kind: VariableKind::Parameter,
                type_kind: PortTypeKind::Logic,
                source: None,
                hidden: false,
                force_control: None,
                sampled_from: None,
                history_of: None,
                activation_scratch: false,
            },
        );
    }

    let forceable = variables
        .iter()
        .filter_map(|(&id, var)| {
            if var.kind == VariableKind::Parameter {
                return None;
            }
            let name = &var.path[0];
            force_signals.iter().any(|path| name == path).then_some(id)
        })
        .collect::<Vec<_>>();
    for id in forceable {
        let target = variables[&id].clone();
        // Whole packed variables only. Array elements require a separately
        // specified addressing contract rather than a guessed flat offset.
        if !target.array_dims.is_empty() {
            continue;
        }
        for (prefix, width, is_4state) in [
            (FORCE_ENABLE, 1, false),
            (FORCE_VALUE, target.width, target.is_4state),
        ] {
            let control_name = format!("{prefix}{}", target.path[0]);
            if name_to_id.contains_key(&control_name) {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "signal `{control_name}` conflicts with a compiler force control"
                )));
            }
            let mut control = target.clone();
            control.force_control = Some((id, prefix == FORCE_ENABLE));
            control.path = vec![control_name.clone()];
            control.width = width;
            control.signed = false;
            control.is_4state = is_4state;
            control.kind = VariableKind::Variable;
            control.hidden = true;
            control.packed_ranges = vec![(width as i128 - 1, 0)];
            control.type_kind = if is_4state {
                PortTypeKind::Logic
            } else {
                PortTypeKind::Bit
            };
            let control_id = next_var_id(&mut next_id);
            variables.insert(control_id, control);
            name_to_id.insert(control_name, control_id);
        }
    }

    // The functions Celox provides need no C library.
    if !cfg!(feature = "dpi")
        && module
            .dpi_imports()
            .iter()
            .any(|import| import.builtin().is_none())
    {
        return Err(sv::AnalyzerError::Unsupported(
            "DPI-C import (enable the `sv-dpi` feature of `celox`)".to_string(),
        ));
    }
    procedural::register_locals(
        module,
        &mut variables,
        &mut name_to_id,
        &mut next_id,
        &constants,
        &parameter_types,
    )?;
    // `always @(list)` naming exactly the reads of its statement is
    // `always @*` (IEEE 1800-2023 9.4.2.2), which needs no process.
    let level_sensitive = match level_lists {
        LevelLists::Processes => None,
        LevelLists::Lowered { demoted } => Some(demoted),
    }
    .filter(|_| {
        module
            .initial_processes()
            .iter()
            .any(|process| process.level_sensitive().is_some())
    })
    .map(|demoted| {
        let pm = procedural::ProcModule::new(
            module,
            &mut variables,
            &mut name_to_id,
            &constants,
            &parameter_types,
            four_state,
            warnings,
        );
        module.clone().lower_level_sensitive_processes(|process| {
            !demoted.contains(&process.creation_order())
                && process
                    .level_sensitive()
                    .is_some_and(|(items, body)| pm.is_implicit_event_list(items, body))
        })
    });
    let module = level_sensitive.as_ref().unwrap_or(module);
    let mut repeated_ff_writes = Vec::new();
    let (
        (
            eval_only_ff_blocks,
            apply_ff_blocks,
            eval_apply_ff_blocks,
            split_commit_ff_blocks,
            reset_clock_map,
            parallel_ff_parts,
            event_vars,
            delayed_sites,
        ),
        mut runtime_event_sites,
        mut runtime_errors,
        mut extern_functions,
    ) = {
        let mut pm = procedural::ProcModule::new(
            module,
            &mut variables,
            &mut name_to_id,
            &constants,
            &parameter_types,
            four_state,
            warnings,
        );
        pm.calls = calls.as_deref_mut();
        let foreign = pm
            .out_of_line()
            .map(|functions| std::mem::take(&mut functions.foreign))
            .unwrap_or_default();
        pm.mark_recursive(module);
        pm.observe_storage(module, &foreign);
        let blocks = lower_ff_processes(
            module,
            &mut pm,
            ff_parts,
            &ff_event_roles,
            &shared_memories,
            foreign_writes,
            &shared_variables,
            &mut repeated_ff_writes,
        )?;
        (
            blocks,
            std::mem::take(&mut pm.runtime_event_sites),
            std::mem::take(&mut pm.runtime_errors),
            std::mem::take(&mut pm.extern_functions),
        )
    };
    mark_ff_event_domains(&event_vars, &mut variables);
    let (
        initial_values,
        declaration_values,
        folded_initial_memory_values,
        mut processes,
        wait_sites,
        mut nba_sites,
        nba_switches,
        host_request_sites,
        event_assignments,
    ) = lower_initial_processes(
        module,
        &mut variables,
        &mut name_to_id,
        &constants,
        &parameter_types,
        four_state,
        &mut runtime_event_sites,
        &mut runtime_errors,
        &mut extern_functions,
        warnings,
        calls,
    )?;
    // The delayed flip-flop updates are published by their domains' commit
    // units through timed nonblocking sites; one synthetic process groups
    // the module's sites for publication. Its kernel never runs.
    if !delayed_sites.is_empty() {
        let index = processes.len();
        processes.push(delayed_publication_process(&mut variables, index));
        nba_sites.extend(delayed_sites.into_iter().map(|mut site| {
            site.process = index;
            site
        }));
    }
    let mut declaration_initial_memory_values = initial_memory_values.clone();
    declaration_initial_memory_values.extend(declaration_values);
    initial_memory_values.extend(initial_values);

    let shared_variables = variables
        .iter()
        .map(|(&id, variable)| (id, variable.to_symbolic_variable()))
        .collect();
    let instances = lower_instances(module, &constants, &parameter_types)?;

    Ok(LoweredSvModule {
        source: module.clone(),
        implicit_nets_allowed,
        sim_module: SimModule {
            time_precision: None,
            time_scale: celox_design::TimeScale::default(),
            name,
            variables: shared_variables,
            ff_access_summaries: HashMap::default(),
            eval_only_ff_blocks,
            apply_ff_blocks,
            eval_apply_ff_blocks,
            split_commit_ff_blocks,
            parallel_ff_parts,
            glue_blocks: HashMap::default(),
            indexed_instance_names: HashSet::default(),
            instance_index_bases: HashMap::default(),
            comb_blocks: Vec::new(),
            comb_observers: Vec::<CombObserver<SourceVarId>>::new(),
            runtime_errors,
            runtime_event_sites,
            extern_functions,
            initial_memory_values,
            declaration_initial_memory_values,
            folded_initial_memory_values,
            comb_boundaries: HashMap::default(),
            arena: SLTNodeArena::new(),
            reset_clock_map,
            processes,
            functions: Vec::new(),
            function_frames: Vec::new(),
            package_bindings: HashMap::default(),
            hierarchical_bindings: HashMap::default(),
            counter_slots: Vec::new(),
            pending_slots: Vec::new(),
            comb_alternatives: Vec::new(),
            wait_sites,
            nba_sites,
            nba_switches,
            host_request_sites,
            time_slot: name_to_id.get(TIME_SLOT).copied(),
            sample_shadows: Vec::new(),
            initial_copies: Vec::new(),
        },
        variables,
        port_order,
        signal_names: name_to_id,
        constants: constants.clone(),
        parameter_types,
        instances,
        package_variables,
        event_copies: event_vars.copies,
        event_assignments,
        repeated_ff_writes,
        inout_driven_inside: HashSet::default(),
        calls: None,
    })
}

fn mark_ff_event_domains(
    event_vars: &FfEventVars,
    variables: &mut HashMap<SourceVarId, SvVariable>,
) {
    for (id, kind) in event_vars.kinds() {
        if let Some(variable) = variables.get_mut(&id) {
            variable.domain_kind = kind;
            variable.type_kind = match kind {
                DomainKind::ResetAsyncHigh => PortTypeKind::ResetAsyncHigh,
                DomainKind::ResetAsyncLow => PortTypeKind::ResetAsyncLow,
                _ => PortTypeKind::Clock,
            };
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
        if !matches!(value, sv::ir::ConstExpr::Literal(value) if value == "$") {
            let integral = sv::typecheck::eval_const_integral_literal_with_types(
                value,
                &constants,
                &HashMap::default(),
            );
            if integral.is_none()
                && sv::typecheck::eval_const_real_expr(value, &constants).is_none()
            {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "nonconstant module parameter override `{}`",
                    parameter.name
                )));
            }
        }
        evaluated.insert(parameter.name.clone(), value.clone());
    }
    Ok(evaluated)
}

/// The instances of a specialized module, one per element of an instance
/// array, without those of false conditional-generate branches.
fn lower_instances(
    module: &sv::ir::Module,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Result<Vec<LoweredSvInstance>, sv::AnalyzerError> {
    let mut instances = Vec::new();
    for instance in module.instances() {
        if let Some(condition) = instance.condition() {
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
                force_signals: Vec::new(),
                parameter_overrides: lower_parameter_overrides(
                    instance,
                    constants,
                    parameter_types,
                ),
                port_connections: instance
                    .port_connections()
                    .iter()
                    .map(|connection| LoweredSvPortConnection {
                        formal: connection.formal().to_string(),
                        actual: connection.actual().to_string(),
                        actual_expr: connection.actual_expr().cloned(),
                        bound_pattern: connection.bound_pattern().cloned(),
                    })
                    .collect(),
                array_element,
                array_index_base,
            });
        }
    }
    Ok(instances)
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
                bound_type: parameter.bound_type().cloned(),
                bound_pattern: parameter.bound_pattern().cloned(),
            }
        })
        .collect()
}

fn const_expr_references_identifier(expr: &sv::ir::ConstExpr) -> bool {
    match expr {
        sv::ir::ConstExpr::Ident(_) => true,
        sv::ir::ConstExpr::Literal(_) => false,
        sv::ir::ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            const_expr_references_identifier(expr)
                || const_expr_references_identifier(msb)
                || const_expr_references_identifier(lsb)
        }
        sv::ir::ConstExpr::Select { expr, bit } => {
            const_expr_references_identifier(expr) || const_expr_references_identifier(bit)
        }
        sv::ir::ConstExpr::Function { args, .. } => {
            args.iter().any(const_expr_references_identifier)
        }
        sv::ir::ConstExpr::Resize { expr, .. } | sv::ir::ConstExpr::Unary { expr, .. } => {
            const_expr_references_identifier(expr)
        }
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
                    bound_pattern: None,
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
                bound_pattern: None,
            })
        })
        .collect()
}

/// Tagged unions cannot acquire an arbitrary tag through an integral port connection.
fn check_tagged_port_connections(
    parent: &sv::ir::Module,
    child: &sv::ir::Module,
    connections: &[LoweredSvPortConnection],
) -> Result<(), ParserError> {
    let type_of = |name: &str| {
        parent
            .ports()
            .iter()
            .find(|port| port.name() == name)
            .map(|port| port.r#type())
            .or_else(|| {
                parent
                    .signals()
                    .iter()
                    .find(|signal| signal.name() == name)
                    .map(|signal| signal.r#type())
            })
    };
    for connection in connections {
        let Some(port) = child
            .ports()
            .iter()
            .find(|port| port.name() == connection.formal)
        else {
            continue;
        };
        let Some(expr) = &connection.actual_expr else {
            continue;
        };
        let actual = if let sv::ir::Expr::Ident(name) = expr {
            type_of(name)
        } else {
            None
        };
        let writes_actual = matches!(
            port.direction(),
            sv::ir::PortDirection::Output
                | sv::ir::PortDirection::Inout
                | sv::ir::PortDirection::Ref
        );
        let mut writes_tagged = false;
        if writes_actual {
            let mut pending = vec![expr];
            while let Some(part) = pending.pop() {
                match part {
                    sv::ir::Expr::Ident(name) => {
                        writes_tagged |=
                            type_of(name).is_some_and(|ty| ty.tagged_identity().is_some());
                    }
                    sv::ir::Expr::Concat(parts) => pending.extend(parts),
                    sv::ir::Expr::Select { expr, .. } | sv::ir::Expr::Resize { expr, .. } => {
                        pending.push(expr)
                    }
                    _ => {}
                }
            }
        }
        let target = port.r#type();
        if target.tagged_identity().is_none() && !writes_tagged {
            continue;
        }
        if !actual.is_some_and(|ty| {
            ty.tagged_identity() == target.tagged_identity()
                && ty.resolved_width() == target.resolved_width()
                && ty.unpacked_ranges() == target.unpacked_ranges()
        }) {
            return Err(ParserError::illegal_context(
                "systemverilog tagged union port",
                format!(
                    "port `{}` requires a signal of the same tagged union type",
                    connection.formal
                ),
                None,
            ));
        }
    }
    Ok(())
}

/// Connect unpacked structures through their independently typed leaf ports.
/// Aggregate declarations remain in source metadata for nominal type checking.
fn expand_structure_connections<'a>(
    connections: &'a [LoweredSvPortConnection],
    parent: &sv::ir::Module,
    child: &sv::ir::Module,
) -> Result<std::borrow::Cow<'a, [LoweredSvPortConnection]>, ParserError> {
    if !child
        .ports()
        .iter()
        .any(|port| port.r#type().structure_identity().is_some())
    {
        return Ok(std::borrow::Cow::Borrowed(connections));
    }
    let mut expanded = Vec::new();
    let mut connected_structures = HashSet::default();
    for connection in connections {
        let Some(port) = child.ports().iter().find(|port| {
            port.name() == connection.formal && port.r#type().structure_identity().is_some()
        }) else {
            expanded.push(connection.clone());
            continue;
        };
        let invalid = |message: String| {
            ParserError::illegal_context(
                "systemverilog unpacked structure port connection",
                message,
                None,
            )
        };
        if !connected_structures.insert(&connection.formal) {
            return Err(invalid(format!(
                "duplicate connection of port `{}`",
                connection.formal
            )));
        }
        if let Some(pattern) = &connection.bound_pattern {
            if port.direction() != sv::ir::PortDirection::Input {
                return Err(invalid(
                    "an output structure port requires a variable".into(),
                ));
            }
            let parts = port
                .bind_structure_pattern(pattern)
                .map_err(|error| invalid(error.to_string()))?;
            for ((member, _), value) in port.r#type().structure_leaves().iter().zip(parts) {
                expanded.push(LoweredSvPortConnection {
                    formal: format!("{}.{member}", connection.formal),
                    actual: connection.actual.clone(),
                    actual_expr: Some(value),
                    bound_pattern: None,
                });
            }
            continue;
        }
        if connection.actual_expr.is_none() {
            // An explicitly open port leaves all of its leaf ports open.
            continue;
        }
        let actual = parent
            .ports()
            .iter()
            .find(|p| p.name() == connection.actual)
            .map(|p| p.r#type())
            .or_else(|| {
                parent
                    .signals()
                    .iter()
                    .find(|s| s.name() == connection.actual)
                    .map(|s| s.r#type())
            });
        if actual.and_then(|ty| ty.structure_identity()) != port.r#type().structure_identity() {
            return Err(invalid(format!(
                "port `{}` requires a variable of the same unpacked structure type",
                connection.formal
            )));
        }
        let expected = sv::ir::Expr::Concat(
            port.r#type()
                .structure_leaves()
                .iter()
                .map(|(member, _)| sv::ir::Expr::Ident(format!("{}.{member}", connection.actual)))
                .collect(),
        );
        if connection.actual_expr.as_ref() != Some(&expected)
            && connection.actual_expr.as_ref()
                != Some(&sv::ir::Expr::Ident(connection.actual.clone()))
        {
            return Err(invalid("an unpacked structure port currently requires a named variable or an input assignment pattern".into()));
        }
        for (member, _) in port.r#type().structure_leaves() {
            let actual = format!("{}.{member}", connection.actual);
            expanded.push(LoweredSvPortConnection {
                formal: format!("{}.{member}", connection.formal),
                actual: actual.clone(),
                actual_expr: Some(sv::ir::Expr::Ident(actual)),
                bound_pattern: None,
            });
        }
    }
    Ok(std::borrow::Cow::Owned(expanded))
}

fn bind_port_patterns(
    connections: &mut [LoweredSvPortConnection],
    child: &LoweredSvModule,
) -> Result<(), ParserError> {
    for connection in connections {
        let Some(pattern) = connection.bound_pattern.take() else {
            continue;
        };
        let port = child
            .source
            .ports()
            .iter()
            .find(|port| port.name() == connection.formal)
            .ok_or_else(|| {
                ParserError::illegal_context(
                    "systemverilog port assignment pattern",
                    format!("unknown port `{}`", connection.formal),
                    None,
                )
            })?;
        if port.direction() != sv::ir::PortDirection::Input {
            return Err(ParserError::illegal_context(
                "systemverilog port assignment pattern",
                "only input port assignment patterns are supported",
                None,
            ));
        }
        connection.actual_expr = Some(port.bind_pattern(&pattern).map_err(|error| {
            ParserError::illegal_context(
                "systemverilog port assignment pattern",
                error.to_string(),
                None,
            )
        })?);
    }
    Ok(())
}

pub(crate) fn attach_instance_glue(
    module: &mut SimModule,
    lowered: &LoweredSvModule,
    current_key: &LoweredSvModuleKey,
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    lowered_modules: &HashMap<ModuleId, LoweredSvModule>,
    four_state: bool,
    allow_inferred_latches: bool,
    foreign: bool,
    hierarchical_targets: &HashSet<String>,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    calls: Option<&mut recursion::CallGraph>,
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
        let mut authored_connections = expand_structure_connections(
            &instance.port_connections,
            &lowered.source,
            &child.source,
        )?
        .into_owned();
        bind_port_patterns(&mut authored_connections, child)?;
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
                    &authored_connections,
                    child,
                    position,
                    count,
                    &parent_variables,
                    &signal_names,
                    &lowered.constants,
                    &lowered.parameter_types,
                )?
            }
            None => authored_connections,
        };
        check_tagged_port_connections(&lowered.source, &child.source, &connections)?;
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
    // Materialize state-reading input actuals as continuous assignments in
    // the parent, including reads through callees and omitted defaults.
    // Stateful lowering guards activation, so time alone (or an unrelated
    // input) cannot reevaluate a time-only actual.
    let mut port_assignments = Vec::new();
    {
        let mut pm = procedural::ProcModule::new(
            &lowered.source,
            &mut parent_variables,
            &mut signal_names,
            &lowered.constants,
            &lowered.parameter_types,
            four_state,
            warnings,
        );
        for (_, _, child, connections) in &mut resolved_instances {
            for connection in connections {
                let Some(actual) = &connection.actual_expr else {
                    continue;
                };
                if !pm.reads_simulation_state(&[sv::ir::Stmt::Eval(actual.clone())]) {
                    continue;
                }
                let Some(port) =
                    child
                        .port_order
                        .iter()
                        .map(|id| &child.variables[id])
                        .find(|var| {
                            var.path.join(".") == connection.formal
                                && var.kind == VariableKind::Input
                        })
                else {
                    continue;
                };
                let (id, name) = pm.temp(
                    "port_time",
                    port.width,
                    port.signed,
                    four_state && port.is_4state,
                );
                port_assignments.push((
                    id,
                    vec![sv::ir::Stmt::Assign {
                        lhs: sv::ir::LValue::Ident(name.clone()),
                        rhs: actual.clone(),
                        nonblocking: false,
                    }],
                ));
                connection.actual_expr = Some(sv::ir::Expr::Ident(name));
            }
        }
        for id in &pm.created {
            module
                .variables
                .insert(*id, pm.variables[id].to_symbolic_variable());
        }
    }
    // An event expression is one more continuous assignment to a hidden
    // buffer the processes watch.
    port_assignments.extend(lowered.event_assignments.iter().cloned());
    // The repeated writes of flip-flops and combinational processes are
    // counted only in a design with a wait, so a design without one, such
    // as pure RTL, lowers exactly as before.
    let waits = lowered_modules
        .values()
        .any(|module| !module.sim_module.wait_sites.is_empty());
    // An `always_comb` alternative is kept only if a wait or a flip-flop
    // domain counts one of its targets. Within the module, they name its
    // wait terms, its triggers and whole-variable copies of them; beyond
    // it, ports, instance connections, package variables and the signals
    // hierarchical names reach, in either module. Every counter
    // a design declares also keeps nonblocking writes queued, so a design
    // with a nonblocking switch lowers every alternative, and so does a
    // module of a design with foreign modules, which this module does not see.
    let watch_seeds = (waits
        && !foreign
        && !lowered_modules
            .values()
            .any(|module| !module.sim_module.nba_switches.is_empty()))
    .then(|| {
        // A gate is read at each write of its term's signal too.
        let mut seeds: HashSet<SourceVarId> = module
            .wait_sites
            .iter()
            .flat_map(|site| {
                site.terms
                    .iter()
                    .map(|&(signal, _)| signal)
                    .chain(site.gates.iter().flatten().map(|gate| gate.signal))
            })
            .collect();
        let triggers = module
            .eval_only_ff_blocks
            .keys()
            .chain(module.apply_ff_blocks.keys())
            .chain(module.eval_apply_ff_blocks.keys())
            .chain(module.split_commit_ff_blocks.keys())
            .chain(module.parallel_ff_parts.keys());
        for trigger in triggers {
            seeds.insert(trigger.clock);
            seeds.extend(trigger.resets.iter().copied());
        }
        for &(copy, signal) in &lowered.event_copies {
            seeds.extend([copy, signal]);
        }
        seeds.extend(lowered.package_variables.iter().map(|(id, _, _)| *id));
        let instantiated = lowered_modules.values().any(|other| {
            other
                .instances
                .iter()
                .any(|instance| &LoweredSvModuleKey::instance_key(instance) == current_key)
        });
        if instantiated {
            seeds.extend(
                parent_variables
                    .iter()
                    .filter(|(_, variable)| variable.kind.is_port())
                    .map(|(id, _)| *id),
            );
        }
        let mut names = HashSet::default();
        for (_, _, _, connections) in &resolved_instances {
            for actual in connections.iter().filter_map(|c| c.actual_expr.as_ref()) {
                procedural::expr_idents(actual, &mut names);
            }
        }
        seeds.extend(
            names
                .iter()
                .filter_map(|name| signal_names.get(name).copied()),
        );
        seeds.extend(port_assignments.iter().map(|(target, _)| *target));
        seeds.extend(module.hierarchical_bindings.keys().copied());
        seeds.extend(
            hierarchical_targets
                .iter()
                .filter_map(|name| signal_names.get(name).copied()),
        );
        seeds
    });
    let gated_waits = lowered_modules.values().any(|module| {
        module
            .sim_module
            .wait_sites
            .iter()
            .any(|site| site.gates.iter().any(Option::is_some))
    });
    let (comb_blocks, arena, created, comb_observers, comb_sites, comb_alternatives) =
        lower_comb_processes(
            &lowered.source,
            &port_assignments,
            &mut parent_variables,
            &mut signal_names,
            &lowered.constants,
            &lowered.parameter_types,
            four_state,
            allow_inferred_latches,
            &mut module.runtime_errors,
            &mut module.extern_functions,
            warnings,
            waits,
            watch_seeds.as_ref(),
            gated_waits,
            calls,
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
    let (mut comb_blocks, mut arena) = (comb_blocks, arena);
    push_event_copies(&lowered.event_copies, &mut comb_blocks, &mut arena)?;
    module.comb_boundaries = comb_boundaries(&comb_blocks);
    module.comb_blocks = comb_blocks;
    // Signals a kernel or an imperative combinational unit may store twice
    // between two runtime checks get counters; assembly uses them only for
    // the ones a flip-flop domain or wait watches.
    let mut candidates = Vec::new();
    for process in &module.processes {
        candidates.extend(celox_frontend_core::watch::multi_store_candidates(
            &process.kernel,
            |address| (address.region == STABLE_REGION).then_some(address.var_id),
        ));
    }
    for path in &module.comb_blocks {
        if let LogicPathTarget::Process { unit, .. } = &path.target {
            candidates.extend(celox_frontend_core::watch::multi_store_candidates(
                unit,
                |address| Some(*address),
            ));
        }
    }
    // A flip-flop evaluation stores each update in the working state; two
    // of one execution are two update events (IEEE 1800-2023 4.3, 10.4.2).
    for unit in module.eval_only_ff_blocks.values() {
        candidates.extend(celox_frontend_core::watch::multi_store_only_candidates(
            unit,
            |address| (address.region == WORKING_REGION).then_some(address.var_id),
        ));
    }
    module.arena = arena;
    // Combinational event sites follow the flip-flop ones, including the
    // events run-time loops emit.
    let site_base = module.runtime_event_sites.len() as u32;
    if site_base != 0 {
        let len = module.arena.len();
        module
            .arena
            .remap_for_fold_effect_sites(0..len, |site_id, fatal_error_code| {
                site_id
                    .checked_add(site_base)
                    .map(|site_id| Some((site_id, fatal_error_code.map(|_| i64::from(site_id)))))
                    .ok_or(celox_slt::SLTNodeArenaEditError::SiteIdOverflow {
                        site_id,
                        offset: site_base,
                    })
            })
            .map_err(|error| {
                ParserError::illegal_context(
                    "systemverilog loop runtime-event remap",
                    error.to_string(),
                    None,
                )
            })?;
    }
    for path in &mut module.comb_blocks {
        if let LogicPathTarget::Process { unit, .. } = &mut path.target {
            for block in unit.blocks.values_mut() {
                for inst in &mut block.instructions {
                    if let SIRInstruction::RuntimeEvent { site_id, .. } = inst {
                        *site_id += site_base;
                    }
                }
            }
        }
    }
    for mut observer in comb_observers {
        observer.site_id += site_base;
        observer.activation_group += site_base;
        module.comb_observers.push(observer);
    }
    module
        .runtime_event_sites
        .extend(comb_sites.into_iter().map(|mut site| {
            if let Some(celox_design::DeferredSite::Action { queue, .. }) = &mut site.deferred {
                *queue += site_base;
            }
            site
        }));
    for id in created {
        module
            .variables
            .insert(id, parent_variables[&id].to_symbolic_variable());
    }
    // Combinational processes may read the time too.
    module.time_slot = signal_names.get(TIME_SLOT).copied();
    let mut sample_shadows: Vec<_> = parent_variables
        .iter()
        .filter_map(|(&copy, variable)| variable.sampled_from.map(|variable| (variable, copy)))
        .collect();
    sample_shadows.sort_by_key(|&(variable, copy)| (variable.0, copy.0));
    module.sample_shadows = sample_shadows;
    let mut initial_copies: Vec<_> = parent_variables
        .iter()
        .filter_map(|(&copy, variable)| variable.history_of.map(|variable| (variable, copy)))
        .collect();
    initial_copies.sort_by_key(|&(variable, copy)| (variable.0, copy.0));
    module.initial_copies = initial_copies;
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
    if waits {
        candidates.extend(lowered.repeated_ff_writes.iter().map(|&(signal, _)| signal));
        for alternative in &comb_alternatives {
            candidates.extend(alternative.candidates.iter().copied());
        }
    }
    module.comb_alternatives = comb_alternatives;
    // Probe results are activation-local values, never global occurrences.
    let probe_results: HashSet<_> = module
        .processes
        .iter()
        .filter_map(|process| process.frame.as_ref())
        .flat_map(|frame| {
            frame
                .event_probes
                .iter()
                .flat_map(|probe| probe.scratch.iter().copied())
        })
        .collect();
    candidates.retain(|signal| !probe_results.contains(signal));
    // Monitors observe the entire time slot, not just the interval between
    // process suspensions. Even single writes therefore require counters.
    candidates.extend(
        module
            .host_request_sites
            .iter()
            .filter(|site| {
                site.kind == celox_design::HostRequestKind::FileIo
                    && matches!(site.operation, 17 | 18)
            })
            .flat_map(|site| {
                site.args
                    .iter()
                    .skip(usize::from(site.operation == 18))
                    .map(|arg| arg.id)
            }),
    );
    module.counter_slots =
        celox_frontend_core::watch::declare_counter_slots(&mut module.variables, candidates);
    if waits {
        module.pending_slots = celox_frontend_core::watch::declare_pending_slots(
            &mut module.variables,
            lowered.repeated_ff_writes.iter().copied(),
        );
    }
    Ok(())
}

/// A [`sv::ir::Callee::Stream`] call as slices and concatenations of its
/// arguments, each lowered once. `lower` lowers an operand with a context
/// width and signedness.
fn lower_stream<A: std::hash::Hash + Eq + Clone>(
    args: &[Option<sv::ir::Expr>],
    stream: &sv::ir::Stream,
    arena: &mut SLTNodeArena<A>,
    lower: &mut dyn FnMut(
        &sv::ir::Expr,
        &mut SLTNodeArena<A>,
        Option<usize>,
        Option<bool>,
    ) -> Option<NodeId>,
) -> Option<NodeId> {
    fn slice<A: std::hash::Hash + Eq + Clone>(
        arena: &mut SLTNodeArena<A>,
        expr: NodeId,
        (lsb, width): (usize, usize),
    ) -> Option<NodeId> {
        if lsb == 0 && width == celox_slt::get_width(expr, arena) {
            return Some(expr);
        }
        arena
            .alloc(SLTNode::Slice {
                expr,
                access: BitAccess::new(lsb, lsb + width - 1),
            })
            .ok()
    }
    // The ranges of `expr`, the first in the most significant bits.
    fn gather<A: std::hash::Hash + Eq + Clone>(
        arena: &mut SLTNodeArena<A>,
        expr: NodeId,
        ranges: Vec<(usize, usize)>,
    ) -> Option<NodeId> {
        if let [range] = ranges.as_slice() {
            return slice(arena, expr, *range);
        }
        let parts = ranges
            .into_iter()
            .map(|range| Some((slice(arena, expr, range)?, range.1)))
            .collect::<Option<Vec<_>>>()?;
        arena.alloc(SLTNode::Concat(parts)).ok()
    }
    let mut parts = Vec::with_capacity(args.len());
    for (arg, layout) in args.iter().zip(&stream.args) {
        // An operand is evaluated in its own self-determined type (11.4.14.1).
        let node = lower(arg.as_ref()?, arena, None, None)?;
        let node = coerce_node_width(arena, node, Some(layout.width), false).ok()?;
        let bits = layout.bits.unwrap_or((0, layout.width));
        let node = slice(arena, node, bits)?;
        parts.push((gather(arena, node, layout.elements(bits.1))?, bits.1));
    }
    let generic = match parts.as_slice() {
        [(node, _)] => *node,
        _ => arena.alloc(SLTNode::Concat(parts)).ok()?,
    };
    let (width, stream_width) = (stream.width(), stream.stream_width());
    let ordered = if stream.unpack {
        let source = stream.args.first()?.width;
        let packed = slice(
            arena,
            generic,
            (source.checked_sub(stream_width)?, stream_width),
        )?;
        // The packed stream holds the blocks most significant first: put
        // each back at its place in the stream.
        let mut lsb = stream_width;
        let mut blocks = stream
            .blocks(stream_width)
            .into_iter()
            .map(|(block, size)| {
                lsb -= size;
                (block, (lsb, size))
            })
            .collect::<Vec<_>>();
        blocks.sort_by_key(|block| std::cmp::Reverse(block.0));
        gather(
            arena,
            packed,
            blocks.into_iter().map(|(_, range)| range).collect(),
        )?
    } else {
        let ordered = gather(arena, generic, stream.blocks(stream_width))?;
        if width > stream_width {
            let fill = width - stream_width;
            let zeros = arena
                .alloc(SLTNode::Constant(
                    BigUint::default(),
                    BigUint::default(),
                    fill,
                    false,
                ))
                .ok()?;
            arena
                .alloc(SLTNode::Concat(vec![
                    (ordered, stream_width),
                    (zeros, fill),
                ]))
                .ok()?
        } else {
            ordered
        }
    };
    if stream.targets.iter().all(|target| target.element.is_none()) {
        return Some(ordered);
    }
    let mut lsb = width;
    let mut parts = Vec::with_capacity(stream.targets.len());
    for target in &stream.targets {
        lsb -= target.width;
        let part = slice(arena, ordered, (lsb, target.width))?;
        parts.push((
            gather(arena, part, target.elements(target.width))?,
            target.width,
        ));
    }
    arena.alloc(SLTNode::Concat(parts)).ok()
}

/// A [`sv::ir::Callee::Lookup`] call as nested multiplexers of case
/// equalities. The selector is lowered once and its node shared by every
/// comparison, so nested lookups lower in linear time. `lower` lowers an
/// operand with a context width and signedness.
fn lower_lookup<A: std::hash::Hash + Eq + Clone>(
    args: &[Option<sv::ir::Expr>],
    width: usize,
    signed: bool,
    arena: &mut SLTNodeArena<A>,
    lower: &mut dyn FnMut(
        &sv::ir::Expr,
        &mut SLTNodeArena<A>,
        Option<usize>,
        Option<bool>,
    ) -> Option<NodeId>,
) -> Option<NodeId> {
    let (selector, rest) = args.split_first()?;
    let (default, pairs) = rest.split_last()?;
    let (pairs, []) = pairs.as_chunks::<2>() else {
        return None;
    };
    let selector = lower(selector.as_ref()?, arena, None, None)?;
    let resized = |expr: &Option<sv::ir::Expr>| {
        Some(sv::ir::Expr::Resize {
            expr: Box::new(expr.clone()?),
            width,
            signed,
        })
    };
    let mut result = lower(&resized(default)?, arena, Some(width), Some(signed))?;
    for [key, value] in pairs.iter().rev() {
        let key = lower(key.as_ref()?, arena, None, None)?;
        let compared = celox_slt::get_width(selector, arena).max(celox_slt::get_width(key, arena));
        let left = coerce_node_width(arena, selector, Some(compared), false).ok()?;
        let right = coerce_node_width(arena, key, Some(compared), false).ok()?;
        let cond = arena
            .alloc(SLTNode::Binary(left, BinaryOp::EqCase, right))
            .ok()?;
        let then_expr = lower(&resized(value)?, arena, Some(width), Some(signed))?;
        result = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr,
                else_expr: result,
            })
            .ok()?;
    }
    Some(result)
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

/// Borrow expressions that need no state-mode conversion. Select bounds and
/// repeat counts are not rewritten by this pass, so do not copy their trees
/// merely to inspect the expression that contains them.
fn expr_for_state_mode(
    expr: &sv::ir::Expr,
    four_state: bool,
) -> std::borrow::Cow<'_, sv::ir::Expr> {
    if needs_state_mode_rewrite(expr, four_state) {
        std::borrow::Cow::Owned(rewrite_expr_for_state_mode(expr, four_state))
    } else {
        std::borrow::Cow::Borrowed(expr)
    }
}

fn needs_state_mode_rewrite(expr: &sv::ir::Expr, four_state: bool) -> bool {
    use sv::ir::Expr;
    let needs = |expr| needs_state_mode_rewrite(expr, four_state);
    match expr {
        Expr::Literal(_) => !four_state && expr_is_unknown_literal(expr),
        Expr::Ident(_) => false,
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            needs(expr)
        }
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => parts.iter().any(needs),
        Expr::Binary { left, op, right } => {
            let keep_unknown =
                four_state || matches!(op, sv::ir::BinaryOp::EqCase | sv::ir::BinaryOp::NeCase);
            needs_state_mode_rewrite(left, keep_unknown)
                || needs_state_mode_rewrite(right, keep_unknown)
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            matches!(&**then_expr, Expr::Literal(literal) if literal == sv::DIV_ZERO_UNKNOWN_LITERAL)
                || needs(condition)
                || needs(then_expr)
                || needs(else_expr)
        }
        Expr::Inside { expr, items } => {
            needs(expr) || items.iter().any(|item| item.exprs().into_iter().any(needs))
        }
        Expr::Call { args, .. } => args.iter().flatten().any(needs),
    }
}

fn rewrite_expr_for_state_mode(expr: &sv::ir::Expr, four_state: bool) -> sv::ir::Expr {
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
                let else_expr = rewrite_expr_for_state_mode(else_expr, four_state);
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
                    condition: Box::new(rewrite_expr_for_state_mode(condition, four_state)),
                    then_expr: Box::new(unknown),
                    else_expr: Box::new(else_expr),
                }
            } else {
                rewrite_expr_for_state_mode(else_expr, four_state)
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
            expr: Box::new(rewrite_expr_for_state_mode(expr, four_state)),
            msb: msb.clone(),
            lsb: lsb.clone(),
            signed: *signed,
        },
        sv::ir::Expr::Concat(parts) => sv::ir::Expr::Concat(
            parts
                .iter()
                .map(|part| rewrite_expr_for_state_mode(part, four_state))
                .collect(),
        ),
        sv::ir::Expr::RepeatConcat { count, parts } => sv::ir::Expr::RepeatConcat {
            count: count.clone(),
            parts: parts
                .iter()
                .map(|part| rewrite_expr_for_state_mode(part, four_state))
                .collect(),
        },
        sv::ir::Expr::Resize {
            expr,
            width,
            signed,
        } => sv::ir::Expr::Resize {
            expr: Box::new(rewrite_expr_for_state_mode(expr, four_state)),
            width: *width,
            signed: *signed,
        },
        sv::ir::Expr::Unary { op, expr } => sv::ir::Expr::Unary {
            op: *op,
            expr: Box::new(rewrite_expr_for_state_mode(expr, four_state)),
        },
        sv::ir::Expr::Binary { left, op, right }
            if !four_state && matches!(op, sv::ir::BinaryOp::EqCase | sv::ir::BinaryOp::NeCase) =>
        {
            // Storage remains two-state; exact comparison operands retain
            // literal X/Z until value and mask are compared (11.4.5).
            sv::ir::Expr::Binary {
                left: Box::new(rewrite_expr_for_state_mode(left, true)),
                op: *op,
                right: Box::new(rewrite_expr_for_state_mode(right, true)),
            }
        }
        sv::ir::Expr::Binary { left, op, right } => sv::ir::Expr::Binary {
            left: Box::new(rewrite_expr_for_state_mode(left, four_state)),
            op: *op,
            right: Box::new(rewrite_expr_for_state_mode(right, four_state)),
        },
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => sv::ir::Expr::Mux {
            condition: Box::new(rewrite_expr_for_state_mode(condition, four_state)),
            then_expr: Box::new(rewrite_expr_for_state_mode(then_expr, four_state)),
            else_expr: Box::new(rewrite_expr_for_state_mode(else_expr, four_state)),
        },
        sv::ir::Expr::Inside { expr, items } => sv::ir::Expr::Inside {
            expr: Box::new(rewrite_expr_for_state_mode(expr, four_state)),
            items: items
                .iter()
                .map(|item| {
                    item.map(&mut |operand| rewrite_expr_for_state_mode(operand, four_state))
                })
                .collect(),
        },
        sv::ir::Expr::Call {
            callee,
            args,
            arg_order,
        } => sv::ir::Expr::Call {
            arg_order: arg_order.clone(),
            callee: callee.clone(),
            args: args
                .iter()
                .map(|arg| {
                    arg.as_ref()
                        .map(|arg| rewrite_expr_for_state_mode(arg, four_state))
                })
                .collect(),
        },
    }
}

/// Process kind and index identify a thread without merging the lowering paths.
/// Each instance owns an initialization RNG seeded with the same default
/// (IEEE 1800-2023 18.14.1); relocation keeps sibling RNG states independent.
fn process_random_seeds(module: &sv::ir::Module) -> HashMap<(u8, usize), u32> {
    let mut processes: Vec<_> = module
        .ff_processes()
        .iter()
        .enumerate()
        .map(|(i, p)| (p.creation_order(), 0u8, i))
        .chain(
            module
                .initial_processes()
                .iter()
                .enumerate()
                .filter(|(_, p)| !p.is_synthetic())
                .map(|(i, p)| (p.creation_order(), 1, i)),
        )
        .chain(
            module
                .comb_processes()
                .iter()
                .enumerate()
                .map(|(i, p)| (p.creation_order(), 2, i)),
        )
        .collect();
    processes.sort_by_key(|&(order, kind, i)| (order, kind, i));
    let mut state = 0x4d59_5df4u32;
    processes
        .into_iter()
        .map(|(_, kind, index)| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((kind, index), state)
        })
        .collect()
}

/// The source procedure of a process kernel, for diagnostics.
/// The synthetic process that groups a module's delayed flip-flop
/// nonblocking sites for timed publication. Its kernel is empty: the
/// domains' commit units stage the updates and set the sites' pending
/// flags, and publication queues them for their due times.
fn delayed_publication_process(
    variables: &mut HashMap<SourceVarId, SvVariable>,
    index: usize,
) -> SymbolicProcess {
    let mut next_id = SourceVarId(
        variables
            .keys()
            .map(|id| id.0 + 1)
            .max()
            .unwrap_or_default(),
    );
    let mut declare = |slot: &str, width: usize| {
        let id = next_var_id(&mut next_id);
        variables.insert(
            id,
            SvVariable {
                float: None,
                value_kind: celox_design::ValueKind::Bits,
                path: vec![format!("$ff-nba[{index}]"), slot.to_string()],
                width,
                signed: false,
                is_4state: false,
                is_unpacked_struct: false,
                packed_ranges: vec![(width as i128 - 1, 0)],
                array_dims: Vec::new(),
                unpacked_ranges: Vec::new(),
                domain_kind: DomainKind::Other,
                kind: VariableKind::Variable,
                type_kind: PortTypeKind::Bit,
                source: None,
                hidden: true,
                force_control: None,
                sampled_from: None,
                history_of: None,
                activation_scratch: false,
            },
        );
        id
    };
    let resume = declare("resume", PROCESS_RESUME_WIDTH);
    let status = declare("status", celox_design::PROCESS_STATUS_WIDTH);
    let delay = declare("delay", celox_design::PROCESS_DELAY_WIDTH);
    // The kernel only marks itself done: it never runs the sites.
    let mut kernel = SIRBuilder::new();
    let code = kernel.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
    kernel.emit(SIRInstruction::Imm(
        code,
        SIRValue::new(celox_design::ProcessStatus::Done.code()),
    ));
    kernel.emit(SIRInstruction::Store(
        RegionedVarAddrBase {
            region: STABLE_REGION,
            var_id: status,
        },
        SIROffset::Static(0),
        celox_design::PROCESS_STATUS_WIDTH,
        code,
        Vec::new(),
        Vec::new(),
    ));
    SymbolicProcess {
        kernel: seal_builder(kernel),
        frame: None,
        slots: ProcessSlots {
            kind: celox_design::ProcessKind::Regular,
            requires_timed_simulation: false,
            const_delays: true,
            program: None,
            resume,
            status,
            delay,
            call_top: None,
        },
        call_resumes: Vec::new(),
        origin: celox_design::ProcessOrigin {
            construct: "flip-flop delayed nonblocking publication".to_string(),
            location: None,
            scope: None,
        },
    }
}

fn process_origin(process: &sv::ir::InitialProcess) -> celox_design::ProcessOrigin {
    let construct = match process.site() {
        Some(site) => site.construct,
        None if process.is_initializer() => "declaration initializer",
        None if process.is_final() => "final",
        None if process.repeats() => "always",
        None => "initial",
    };
    celox_design::ProcessOrigin {
        construct: construct.to_string(),
        location: process.site().map(ToString::to_string),
        scope: None,
    }
}

/// Lower declaration initializers and procedural kernels. Final procedures
/// retain kernels but execute only at simulation termination (9.2.3); they
/// must never contribute folded initial state.
///
/// A block whose writes have constant values defines initial state, with
/// the hidden variables used while executing it discarded. Declaration
/// initializers come first, and blocks start from the values they define.
/// A block that reads design state or runs a system task becomes a process
/// kernel; such kernels start at time zero in declaration order, initializers
/// first.
#[allow(clippy::too_many_arguments)]
fn lower_initial_processes(
    module: &sv::ir::Module,
    variables: &mut HashMap<SourceVarId, SvVariable>,
    name_to_id: &mut HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    four_state: bool,
    runtime_event_sites: &mut Vec<RuntimeEventSite>,
    runtime_errors: &mut HashMap<i64, RuntimeErrorInfo<SourceVarId>>,
    extern_functions: &mut Vec<ExternFunction>,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    mut calls: Option<&mut recursion::CallGraph>,
) -> Result<
    (
        Vec<InitialStateValue<SourceVarId>>,
        Vec<InitialStateValue<SourceVarId>>,
        Vec<InitialStateValue<SourceVarId>>,
        Vec<SymbolicProcess>,
        Vec<WaitSite<SourceVarId>>,
        Vec<NbaSite<SourceVarId>>,
        Vec<NbaSwitch<SourceVarId>>,
        Vec<celox_design::HostRequestSite<SourceVarId>>,
        Vec<(SourceVarId, Vec<sv::ir::Stmt>)>,
    ),
    sv::AnalyzerError,
> {
    if module.initial_processes().is_empty() {
        return Ok(Default::default());
    }
    let mut values = Vec::new();
    let mut declaration_values = Vec::new();
    let mut folded_values = Vec::new();
    let mut runtime = Vec::new();
    // The unit of the call graph of each runtime process.
    let mut runtime_units = Vec::new();
    let mut pm = procedural::ProcModule::new(
        module,
        variables,
        name_to_id,
        constants,
        parameter_types,
        four_state,
        warnings,
    );
    pm.calls = calls.as_deref_mut();
    let mut arena = SLTNodeArena::new();
    let mut seed = comb::Store::default();
    let random_seeds = process_random_seeds(module);
    for (index, process) in module.initial_processes().iter().enumerate() {
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
        let random_seed = random_seeds
            .get(&(1, index))
            .copied()
            .unwrap_or(0x4d59_5df4);
        let unit = pm.call_unit();
        // An `always` process never finishes, so it is never folded into
        // initial state. A program's `initial` procedure runs in the reactive
        // region set (IEEE 1800-2023 24.3.1), after the design's processes of
        // time zero, so it is not folded either.
        if process.is_final()
            || process.repeats()
            || (module.is_program() && !process.is_initializer())
        {
            runtime.push((process, random_seed));
            runtime_units.push(unit);
            continue;
        }
        // A process that cannot be folded runs as a kernel instead.
        pm.attempt_calls();
        let mut comb = comb::Comb::new(&mut pm, &mut arena);
        let folded = comb.lower_initial(process.body(), process.is_initializer(), &seed);
        pm.end_attempt_calls(folded.is_ok());
        match folded {
            Ok((written, store)) => {
                if process.is_initializer() {
                    declaration_values.extend(written.iter().cloned());
                    seed = store;
                } else {
                    folded_values.extend(written.iter().cloned());
                }
                values.extend(written);
            }
            Err(_) => {
                runtime.push((process, random_seed));
                runtime_units.push(unit);
            }
        }
    }
    let created = std::mem::take(&mut pm.created);
    for id in created {
        if let Some(variable) = variables.remove(&id) {
            name_to_id.remove(&variable.path.join("."));
        }
    }
    if runtime.is_empty() {
        return Ok((
            values,
            declaration_values,
            folded_values,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ));
    }

    let mut next_id = SourceVarId(
        variables
            .keys()
            .map(|id| id.0 + 1)
            .max()
            .unwrap_or_default(),
    );
    let mut slots: Vec<_> = runtime
        .iter()
        .enumerate()
        .map(|(index, (process, _))| {
            let mut declare = |slot: &str, width: usize| {
                let id = next_var_id(&mut next_id);
                variables.insert(
                    id,
                    SvVariable {
                        float: None,

                        value_kind: celox_design::ValueKind::Bits,
                        path: vec![format!("$initial[{index}]"), slot.to_string()],
                        width,
                        signed: false,
                        is_4state: false,
                        is_unpacked_struct: false,
                        packed_ranges: vec![(width as i128 - 1, 0)],
                        array_dims: Vec::new(),
                        unpacked_ranges: Vec::new(),
                        domain_kind: DomainKind::Other,
                        kind: VariableKind::Variable,
                        type_kind: PortTypeKind::Bit,
                        source: None,
                        // Control slots are not signals of the design.
                        hidden: true,
                        force_control: None,
                        sampled_from: None,
                        history_of: None,
                        activation_scratch: false,
                    },
                );
                id
            };
            ProcessSlots {
                kind: process.kind(),
                requires_timed_simulation: process.is_scheduled_edge() || process.is_final(),
                const_delays: false,
                program: None,
                resume: declare("resume", PROCESS_RESUME_WIDTH),
                status: declare("status", PROCESS_STATUS_WIDTH),
                delay: declare("delay", PROCESS_DELAY_WIDTH),
                call_top: None,
            }
        })
        .collect();
    // Every `initial` kernel of a program instance names the status slot of
    // the first, the instance's identity for `$exit` and its end (IEEE
    // 1800-2023 24.3, 24.7).
    if module.is_program() {
        let program = slots
            .iter()
            .zip(&runtime)
            .find(|(_, (process, _))| !process.is_initializer() && !process.is_final())
            .map(|(slots, _)| slots.status);
        for (slots, (process, _)) in slots.iter_mut().zip(&runtime) {
            if !process.is_initializer() && !process.is_final() {
                slots.program = program;
            }
        }
    }
    // The kernels add their event sites, runtime errors and extern functions
    // after those of the sequential processes. Their hidden variables are
    // state the kernels keep.
    let mut pm = procedural::ProcModule::new(
        module,
        variables,
        name_to_id,
        constants,
        parameter_types,
        four_state,
        warnings,
    );
    pm.calls = calls;
    pm.runtime_event_sites = std::mem::take(runtime_event_sites);
    pm.runtime_errors = std::mem::take(runtime_errors);
    pm.extern_functions = std::mem::take(extern_functions);
    let mut wait_sites = Vec::new();
    let mut nba_sites = Vec::new();
    let mut nba_switches = Vec::new();
    let mut host_request_sites = Vec::new();
    let processes = runtime
        .into_iter()
        .zip(slots)
        .enumerate()
        .map(|(index, ((process, random_seed), slots))| {
            pm.resume_call_unit(runtime_units[index]);
            // `always @(list) statement` waits for a change (IEEE 1800-2023
            // 9.4.2). In two-state mode its outputs would then start as a
            // real 0, so it runs once at time zero, after the time-zero
            // statements of the initial blocks, before it waits, as
            // Verilator settles it: `#0; forever begin statement; @(list); end`.
            let settled;
            let body = match process.level_sensitive().filter(|_| !four_state) {
                Some((items, statements)) => {
                    let mut looped = statements.to_vec();
                    looped.push(sv::ir::Stmt::WaitEvent(items.to_vec()));
                    settled = [
                        sv::ir::Stmt::Delay {
                            amount: sv::ir::Expr::Literal("0".to_string()),
                            ticks_per_unit: 1,
                            ticks_per_precision: 1,
                            precision_steps_per_tick: 1,
                        },
                        sv::ir::Stmt::Loop {
                            kind: sv::ir::LoopKind::Forever,
                            init: Vec::new(),
                            condition: None,
                            step: Vec::new(),
                            body: looped,
                        },
                    ];
                    &settled[..]
                }
                None => process.body(),
            };
            let startup_inactive = !four_state
                && process.level_sensitive().is_some_and(|(_, body)| {
                    body.iter()
                        .all(|stmt| !stmt_may_suspend(module.subroutines(), stmt))
                });
            let mut kernel = ff::Ff::new(&mut pm, random_seed).lower_initial_kernel(
                body,
                slots,
                index,
                process.is_final(),
            )?;
            if startup_inactive {
                // This level-sensitive body contains no timing controls. Its
                // synthetic #0 executes once at time zero in the reference
                // loop, before any compiled slot can start. Every subsequent
                // suspension is an event wait.
                kernel.slots.const_delays = true;
            }
            wait_sites.extend(kernel.wait_sites);
            nba_sites.extend(kernel.nba_sites);
            nba_switches.extend(kernel.nba_switches);
            host_request_sites.extend(kernel.host_request_sites.into_iter().map(|mut site| {
                site.process = celox_design::SiteProcess::own(index);
                site
            }));
            Ok(SymbolicProcess {
                kernel: kernel.unit,
                frame: kernel.frame,
                slots: kernel.slots,
                call_resumes: kernel.call_resumes,
                origin: process_origin(process),
            })
        })
        .collect::<Result<Vec<_>, sv::AnalyzerError>>();
    // A kernel that can suspend inside a call lists the requests of the
    // module's function units after its own.
    if let Ok(processes) = &processes {
        for (index, process) in processes.iter().enumerate() {
            if process.slots.call_top.is_some() {
                // And its waits, after its own (see `Ff::function_wait_word`).
                wait_sites.extend(pm.function_wait_sites.iter().map(|site| {
                    celox_design::WaitSite {
                        process: index,
                        ..site.clone()
                    }
                }));
                nba_sites.extend(
                    pm.function_nba_sites
                        .iter()
                        .map(|site| celox_design::NbaSite {
                            process: index,
                            ..site.clone()
                        }),
                );
                host_request_sites.extend(pm.function_host_request_sites.iter().map(|site| {
                    celox_design::HostRequestSite {
                        process: celox_design::SiteProcess::function(index),
                        ..site.clone()
                    }
                }));
            }
        }
    }
    *runtime_event_sites = std::mem::take(&mut pm.runtime_event_sites);
    *runtime_errors = std::mem::take(&mut pm.runtime_errors);
    *extern_functions = std::mem::take(&mut pm.extern_functions);
    Ok((
        values,
        declaration_values,
        folded_values,
        processes?,
        wait_sites,
        nba_sites,
        nba_switches,
        host_request_sites,
        std::mem::take(&mut pm.event_assignments),
    ))
}

fn lower_comb_processes(
    module: &sv::ir::Module,
    port_assignments: &[(SourceVarId, Vec<sv::ir::Stmt>)],
    variables: &mut HashMap<SourceVarId, SvVariable>,
    name_to_id: &mut HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    four_state: bool,
    allow_inferred_latches: bool,
    runtime_errors: &mut HashMap<i64, RuntimeErrorInfo<SourceVarId>>,
    extern_functions: &mut Vec<ExternFunction>,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    waits: bool,
    watch_seeds: Option<&HashSet<SourceVarId>>,
    gated_waits: bool,
    calls: Option<&mut recursion::CallGraph>,
) -> Result<
    (
        Vec<LogicPath<SourceVarId>>,
        SLTNodeArena<SourceVarId>,
        Vec<SourceVarId>,
        Vec<CombObserver<SourceVarId>>,
        Vec<RuntimeEventSite>,
        Vec<CombAlternative>,
    ),
    sv::AnalyzerError,
> {
    let mut arena = SLTNodeArena::new();
    let mut comb_blocks = Vec::new();
    let mut alternatives = Vec::new();
    let mut observers = Vec::new();
    let mut sites = Vec::new();
    let mut pm = procedural::ProcModule::new(
        module,
        variables,
        name_to_id,
        constants,
        parameter_types,
        four_state,
        warnings,
    );
    pm.calls = calls;
    pm.runtime_errors = std::mem::take(runtime_errors);
    pm.extern_functions = std::mem::take(extern_functions);
    let random_seeds = process_random_seeds(module);
    let redundant = redundant_net_drivers(module);
    let watchable =
        watch_seeds.map(|seeds| watchable_variables(&pm, module, port_assignments, seeds));
    // In two-state mode an `always @*` runs once at time zero like an
    // `always_comb` (#2021), and its sensitivity is its implicit reads. One
    // whose written variables have no other driver therefore defines the same
    // function as an `always_comb`: it gets the symbolic form, and the guarded
    // unit only when the symbolic lowering refuses it.
    let mut drivers = None;
    let mut symbolic_star = |index: usize, process: &sv::ir::CombProcess| {
        if four_state || process.kind() != sv::ir::CombProcessKind::AlwaysStar {
            return false;
        }
        let drivers =
            drivers.get_or_insert_with(|| ModuleDrivers::new(module, constants, parameter_types));
        let mut names = HashSet::default();
        procedural::written_names_with_calls(module, process.body(), &mut names);
        // Another process or a continuous assignment may drive other bits of
        // the same variable, as of any symbolic definition. A variable an
        // `initial` block or declaration initializer writes keeps the guarded
        // unit: its time-zero run changes that value, which observers and
        // waiting processes see as a change.
        names.iter().all(|name| {
            drivers.writes.get(name.as_str()).is_none_or(|writes| {
                if writes.iter().any(|write| write.initial.is_some()) {
                    return false;
                }
                let bits = |write: &DriverWrite| write.range.or(write.element);
                let (own, others): (Vec<_>, Vec<_>) =
                    writes.iter().partition(|write| write.local == Some(index));
                others.iter().all(|other| {
                    (other.local.is_none() && other.continuous.is_none())
                        || own
                            .iter()
                            .all(|mine| !net_driver_ranges_overlap(bits(mine), bits(other)))
                })
            })
        })
    };
    for (index, process) in module.comb_processes().iter().enumerate() {
        if redundant.contains(&index) {
            continue;
        }
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
        pm.call_unit();
        // A wildcard control waits for its first source event even when
        // every output is assigned. A symbolic combinational definition
        // would instead execute during the initial fixed-point evaluation.
        // It also follows every variable its statement reads, while an
        // `always @*` is sensitive only to the arguments of the functions it
        // calls, not to what their bodies read (IEEE 1800-2023 9.2.2.2.2):
        // one whose callees read more keeps the guarded unit.
        let symbolic_star =
            symbolic_star(index, process) && !pm.callees_read_beyond_wildcard(process);
        if (process.kind() == sv::ir::CombProcessKind::AlwaysStar && !symbolic_star)
            || pm.reads_simulation_state(process.body())
            // The symbolic form would expand every call.
            || pm.calls_out_of_line(process.body())
            || (process.kind() != sv::ir::CombProcessKind::ContinuousAssign
                && pm.calls_task(process.body()))
        {
            pm.runtime_event_sites = std::mem::take(&mut sites);
            comb_blocks.extend(
                ff::Ff::new(&mut pm, random_seeds[&(2, index)]).lower_comb_process(
                    process.body(),
                    process.sensitivity_body(),
                    process.kind(),
                    allow_inferred_latches,
                    process.site(),
                    &[],
                    &mut arena,
                )?,
            );
            sites = std::mem::take(&mut pm.runtime_event_sites);
            continue;
        }
        // A process that latches falls back to Ff below.
        pm.attempt_calls();
        let mut comb = comb::Comb::new(&mut pm, &mut arena);
        comb.site_base = sites.len() as u32;
        comb.continuous = process.kind() == sv::ir::CombProcessKind::ContinuousAssign;
        let lowered = comb.lower_process(process.body());
        // The guarded unit accepts whatever the symbolic form refuses of an
        // `always @*`, as it did before the symbolic attempt.
        let fallback = (symbolic_star && lowered.is_err())
            || matches!(
                &lowered,
                Err(sv::AnalyzerError::Unsupported(reason))
                    if reason.starts_with("latch inference inside always_comb")
                        && process.kind() != sv::ir::CombProcessKind::ContinuousAssign
                        && (allow_inferred_latches
                            || matches!(
                                process.kind(),
                                sv::ir::CombProcessKind::AlwaysLatch
                                    | sv::ir::CombProcessKind::AlwaysStar
                            ))
            );
        comb.m.end_attempt_calls(!fallback);
        match lowered {
            // In a design with a wait, a variable the process stores twice
            // may be watched, so it also gets its imperative alternative.
            // A process with runtime events keeps only its symbolic form.
            Ok(paths)
                if waits
                    && (process.kind() == sv::ir::CombProcessKind::AlwaysComb || symbolic_star)
                    && comb.observers.is_empty()
                    && comb.sites.is_empty() =>
            {
                drop(comb);
                let mut targets: Vec<_> = paths
                    .iter()
                    .filter_map(|path| match &path.target {
                        LogicPathTarget::Var(target) => Some(*target),
                        _ => None,
                    })
                    .collect();
                targets.sort_unstable();
                targets.dedup();
                comb_blocks.extend(paths);
                if let Some(watchable) = &watchable
                    && targets
                        .iter()
                        .map(|target| target.id)
                        .chain(pm.written_variables(process.body()))
                        .all(|id| !watchable.contains(&id))
                {
                    continue;
                }
                if let Some((paths, alternative)) = lower_comb_alternative(
                    &mut pm,
                    process,
                    random_seeds[&(2, index)],
                    targets,
                    gated_waits,
                    &mut arena,
                ) {
                    comb_blocks.extend(paths);
                    alternatives.push(alternative);
                }
                continue;
            }
            Ok(paths) => comb_blocks.extend(paths),
            Err(_) if fallback => {
                // A retained output is procedural state. The guarded unit
                // executes once per implicit-sensitivity change, without
                // interpreting held values as a combinational feedback loop.
                drop(comb);
                pm.runtime_event_sites = std::mem::take(&mut sites);
                comb_blocks.extend(
                    ff::Ff::new(&mut pm, random_seeds[&(2, index)]).lower_comb_process(
                        process.body(),
                        process.sensitivity_body(),
                        process.kind(),
                        allow_inferred_latches,
                        process.site(),
                        &[],
                        &mut arena,
                    )?,
                );
                sites = std::mem::take(&mut pm.runtime_event_sites);
                continue;
            }
            Err(error) => return Err(error),
        }
        if comb.observers.iter().any(|observer| {
            observer
                .sensitivity
                .iter()
                .any(|atom| !comb.m.var(atom.id).array_dims.is_empty())
        }) {
            // Symbolic array reads may conservatively depend on every element.
            // Process activation follows the syntactic static selection instead.
            let include_functions = process.kind() != sv::ir::CombProcessKind::AlwaysStar;
            let mut reads = comb.m.implicit_reads(process.body(), include_functions);
            reads.extend(
                comb.m
                    .implicit_reads(process.sensitivity_body(), include_functions),
            );
            for observer in &mut comb.observers {
                observer.sensitivity = observer
                    .sensitivity
                    .iter()
                    .flat_map(|atom| {
                        if comb.m.var(atom.id).array_dims.is_empty() {
                            return vec![*atom];
                        }
                        let selected: Vec<_> = reads
                            .iter()
                            .filter(|read| read.id == atom.id && read.access.overlaps(&atom.access))
                            .map(|read| {
                                VarAtomBase::new(
                                    atom.id,
                                    read.access.lsb.max(atom.access.lsb),
                                    read.access.msb.min(atom.access.msb),
                                )
                            })
                            .collect();
                        if selected.is_empty() {
                            vec![*atom]
                        } else {
                            selected
                        }
                    })
                    .collect();
            }
        }
        // The sites of one process activate together. Every change of a
        // source schedules the process (IEEE 1800-2023 4.3), also one that a
        // later write of the same time slot undoes, so each sensitivity atom
        // counts its occurrences.
        let base = sites.len() as u32;
        let mut counters: Vec<(VarAtomBase<SourceVarId>, SourceVarId)> = Vec::new();
        for mut observer in std::mem::take(&mut comb.observers) {
            observer.activation_group = base;
            for atom in &observer.sensitivity {
                if !counters.iter().any(|(counted, _)| counted == atom) {
                    let (counter, _) = comb.m.temp(
                        "comb_occurrences",
                        celox_design::EVENT_COUNTER_WIDTH,
                        false,
                        false,
                    );
                    counters.push((*atom, counter));
                }
            }
            observer.sensitivity_counters = observer
                .sensitivity
                .iter()
                .map(|atom| {
                    *counters
                        .iter()
                        .find(|(counted, _)| counted == atom)
                        .expect("a counter for every sensitivity atom")
                })
                .collect();
            // A deferred action waits in the queue of its process.
            if let Some(celox_design::DeferredSite::Action { queue, .. }) =
                &mut comb.sites[(observer.site_id - base) as usize].deferred
            {
                *queue = base;
            }
            observers.push(observer);
        }
        sites.append(&mut comb.sites);
    }
    for (target, body) in port_assignments {
        pm.call_unit();
        pm.runtime_event_sites = std::mem::take(&mut sites);
        comb_blocks.extend(ff::Ff::new(&mut pm, 0).lower_comb_process(
            body,
            &[],
            sv::ir::CombProcessKind::ContinuousAssign,
            false,
            None,
            &[*target],
            &mut arena,
        )?);
        sites = std::mem::take(&mut pm.runtime_event_sites);
    }
    *runtime_errors = std::mem::take(&mut pm.runtime_errors);
    *extern_functions = std::mem::take(&mut pm.extern_functions);
    let created = std::mem::take(&mut pm.created);
    Ok((comb_blocks, arena, created, observers, sites, alternatives))
}

/// `seeds` and every variable a combinational process or a port assignment
/// connects to them, in either direction. Clock-domain unification aliases
/// only whole-variable copies, but any variable a process writes and any it
/// writes or reads join one group, at any width and through any expression.
fn watchable_variables(
    pm: &procedural::ProcModule<'_>,
    module: &sv::ir::Module,
    port_assignments: &[(SourceVarId, Vec<sv::ir::Stmt>)],
    seeds: &HashSet<SourceVarId>,
) -> HashSet<SourceVarId> {
    let mut groups: Vec<Vec<SourceVarId>> = Vec::new();
    let mut group = |writes: HashSet<SourceVarId>, reads: HashSet<VarAtomBase<SourceVarId>>| {
        if !writes.is_empty() {
            groups.push(
                writes
                    .into_iter()
                    .chain(reads.iter().map(|read| read.id))
                    .collect(),
            );
        }
    };
    for process in module.comb_processes() {
        let mut reads = pm.implicit_reads(process.body(), true);
        reads.extend(pm.implicit_reads(process.sensitivity_body(), true));
        group(pm.written_variables(process.body()), reads);
    }
    for (target, body) in port_assignments {
        let mut writes = pm.written_variables(body);
        writes.insert(*target);
        group(writes, pm.implicit_reads(body, true));
    }
    let mut watchable = seeds.clone();
    loop {
        let before = groups.len();
        groups.retain(|group| {
            if !group.iter().any(|id| watchable.contains(id)) {
                return true;
            }
            watchable.extend(group.iter().copied());
            false
        });
        if groups.len() == before {
            return watchable;
        }
    }
}

/// The imperative unit of an `always_comb` process lowered symbolically to
/// `targets`, and its [`CombAlternative`]. Keep even single-store alternatives:
/// assembly resolves cross-module gated wait dependencies only after lowering.
/// It chooses imperative execution only when those global dependencies or
/// occurrence counters require preserving procedural order.
fn lower_comb_alternative(
    pm: &mut procedural::ProcModule<'_>,
    process: &sv::ir::CombProcess,
    random_seed: u32,
    targets: Vec<VarAtomBase<SourceVarId>>,
    gated_waits: bool,
    arena: &mut SLTNodeArena<SourceVarId>,
) -> Option<(Vec<LogicPath<SourceVarId>>, CombAlternative)> {
    let checkpoint = pm.checkpoint();
    pm.attempt_calls();
    let mut scratch = SLTNodeArena::new();
    let lowered = ff::Ff::new(pm, random_seed)
        .without_input_occurrences()
        .lower_comb_process(
            process.body(),
            process.sensitivity_body(),
            process.kind(),
            false,
            process.site(),
            &[],
            &mut scratch,
        );
    let alternative = match lowered {
        Ok(paths) if pm.runtime_event_sites.len() == checkpoint.runtime_event_sites() => paths
            .first()
            .and_then(|path| match &path.target {
                LogicPathTarget::Process { process, unit, .. } => Some((*process, unit)),
                _ => None,
            })
            .map(|(id, unit)| {
                let candidates: Vec<_> =
                    celox_frontend_core::watch::multi_store_candidates(unit, |&address| {
                        (!pm.is_hidden(address) && pm.var(address).array_dims.is_empty())
                            .then_some(address)
                    });
                (id, candidates)
            })
            .filter(|(_, candidates)| gated_waits || !candidates.is_empty())
            .map(|(process, candidates)| (paths, process, candidates)),
        _ => None,
    };
    pm.end_attempt_calls(alternative.is_some());
    let Some((mut paths, process, mut candidates)) = alternative else {
        pm.rollback(checkpoint);
        return None;
    };
    for path in &mut paths {
        path.expr = arena
            .alloc(scratch.get(path.expr).clone())
            .expect("an input node of a fresh arena");
    }
    candidates.sort_unstable();
    candidates.dedup();
    Some((
        paths,
        CombAlternative {
            process,
            targets,
            candidates,
        },
    ))
}

/// The continuous assignments `copy = signal` of the hidden event copies of
/// [`FfEventVars`]: a whole-variable copy, which clock-domain unification
/// keeps in the signal's domain unless the copy is sensed at the other edge.
fn push_event_copies(
    copies: &[(SourceVarId, SourceVarId)],
    comb_blocks: &mut Vec<LogicPath<SourceVarId>>,
    arena: &mut SLTNodeArena<SourceVarId>,
) -> Result<(), ParserError> {
    for &(copy, signal) in copies {
        let access = BitAccess::new(0, 0);
        let expr = arena
            .alloc(SLTNode::Input {
                variable: signal,
                signed: false,
                index: Vec::new(),
                access,
            })
            .map_err(|error| {
                ParserError::illegal_context(
                    "systemverilog always_ff event copy",
                    error.to_string(),
                    None,
                )
            })?;
        comb_blocks.push(LogicPath {
            transparent_copy: false,
            target: LogicPathTarget::Var(VarAtomBase::new(copy, 0, 0)),
            sources: std::iter::once(VarAtomBase::new(signal, 0, 0)).collect(),
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        });
    }
    Ok(())
}

/// The bit boundaries at which combinational processes write or statically
/// read each variable. Flattening splits paths at these boundaries (and at
/// those propagated through ports), so bit-disjoint dependencies on one
/// variable are scheduled independently instead of forming a false loop.
fn comb_boundaries(paths: &[LogicPath<SourceVarId>]) -> HashMap<SourceVarId, BTreeSet<usize>> {
    let mut boundaries: HashMap<SourceVarId, BTreeSet<usize>> = HashMap::default();
    for path in paths {
        for atom in path.target.var().into_iter().chain(&path.sources) {
            boundaries
                .entry(atom.id)
                .or_default()
                .extend([atom.access.lsb, atom.access.msb + 1]);
        }
    }
    boundaries
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
        let formal = child_var.path.join(".");
        let Some(connection) = connections
            .iter()
            .find(|connection| connection.formal == formal)
        else {
            continue;
        };
        if child_var.kind == VariableKind::Inout {
            check_inout_port_actual(
                connection,
                parent_variables,
                parent_signal_names,
                implicit_output_signals,
                parent_source,
                parent_constants,
                parent_parameter_types,
            )?;
        }
        if child_port_direction(child, child_var) != Some(VariableKind::Output) {
            continue;
        }
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
        if !ModuleDrivers::new(parent_source, parent_constants, parent_parameter_types)
            .local(actual)
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
            float: None,

            value_kind: celox_design::ValueKind::Bits,
            path: vec![actual.to_string()],
            width: 1,
            signed: false,
            is_4state: true,
            is_unpacked_struct: false,
            packed_ranges: Vec::new(),
            array_dims: Vec::new(),
            unpacked_ranges: Vec::new(),
            domain_kind: DomainKind::Other,
            kind: VariableKind::Variable,
            type_kind: PortTypeKind::Logic,
            source: None,
            hidden: false,
            force_control: None,
            sampled_from: None,
            history_of: None,
            activation_scratch: false,
        };
        parent
            .variables
            .insert(next_id, variable.to_symbolic_variable());
        parent_variables.insert(next_id, variable);
    }
    Ok(())
}

/// Rejects a variable or an expression connected to an `inout` port: only
/// nets may be on either side of one (IEEE 1800-2023 23.3.3.2). An
/// undeclared identifier is an implicit net.
fn check_inout_port_actual(
    connection: &LoweredSvPortConnection,
    parent_variables: &HashMap<SourceVarId, SvVariable>,
    parent_signal_names: &HashMap<String, SourceVarId>,
    implicit_nets: &HashSet<String>,
    parent_source: &sv::ir::Module,
    parent_constants: &HashMap<String, i128>,
    parent_parameter_types: &HashMap<String, (usize, bool)>,
) -> Result<(), ParserError> {
    let Some(actual) = connection.actual_expr.as_ref() else {
        return Ok(());
    };
    let is_net = |name: &str| {
        implicit_nets.contains(name)
            || parent_source
                .signals()
                .iter()
                .any(|signal| signal.name() == name && signal.is_net())
            || parent_source
                .ports()
                .iter()
                .any(|port| port.name() == name && port.is_net())
    };
    let error = |detail: String| {
        Err(ParserError::illegal_context(
            "systemverilog inout port connection",
            detail,
            None,
        ))
    };
    let Some(accesses) = output_lvalue_accesses(
        actual,
        parent_variables,
        parent_signal_names,
        parent_constants,
        parent_parameter_types,
    ) else {
        if simple_output_lvalue_ident(actual)
            .is_some_and(|name| !parent_signal_names.contains_key(name))
        {
            return Ok(());
        }
        return error(format!(
            "inout port `{}` must connect to a net, not `{}`",
            connection.formal, connection.actual
        ));
    };
    for OutputLvalueAccess { signal, .. } in accesses {
        let name = parent_variables[&signal].path.join(".");
        if !is_net(&name) {
            return error(format!(
                "inout port `{}` must connect to a net, not variable `{name}`",
                connection.formal
            ));
        }
    }
    Ok(())
}

type SvGlue = (
    Vec<(Vec<SourceVarId>, LogicPath<GlueAddr>)>,
    Vec<(Vec<SourceVarId>, LogicPath<GlueAddr>)>,
    SLTNodeArena<GlueAddr>,
);

/// The type of `variable` when it is an unpacked array.
fn unpacked_array_type(variable: &SvVariable) -> Option<sv::typecheck::UnpackedArrayType> {
    let element_width = unpacked_element_width(variable)?;
    Some(sv::typecheck::UnpackedArrayType {
        dims: variable.array_dims.clone(),
        element_width,
        signed: variable.signed,
        four_state: variable.is_4state,
        // Lowered variables keep only the bits of their elements, so a port
        // connection compares the integral shape of the element types.
        element: None,
    })
}

/// Rejects an unpacked array connected to an unpacked array input or output
/// port of an incompatible type: such a connection is an assignment-like
/// context (IEEE 1800-2023 10.8, 23.3.3), which requires equivalent element
/// types and equal element counts (7.6).
fn check_unpacked_port_connection(
    connection: &LoweredSvPortConnection,
    child: &LoweredSvModule,
    parent_variables: &HashMap<SourceVarId, SvVariable>,
    parent_signal_names: &HashMap<String, SourceVarId>,
) -> Result<(), ParserError> {
    let Some(sv::ir::Expr::Ident(actual)) = connection.actual_expr.as_ref() else {
        return Ok(());
    };
    let Some(port) = child
        .signal_names
        .get(&connection.formal)
        .and_then(|id| child.variables.get(id))
        .filter(|port| matches!(port.kind, VariableKind::Input | VariableKind::Output))
    else {
        return Ok(());
    };
    let (Some(target), Some(actual_type)) = (
        unpacked_array_type(port),
        parent_signal_names
            .get(actual)
            .and_then(|id| parent_variables.get(id))
            .and_then(unpacked_array_type),
    ) else {
        return Ok(());
    };
    if actual_type.is_assignment_compatible_with(&target) {
        return Ok(());
    }
    let error = sv::AnalyzerError::IncompatibleUnpackedArray {
        context: format!("connection of port `{}`", connection.formal),
        actual: actual_type.to_string(),
        target: target.to_string(),
    };
    Err(ParserError::illegal_context(
        "systemverilog port connection",
        error.to_string(),
        None,
    ))
}

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
        check_unpacked_port_connection(connection, child, parent_variables, parent_signal_names)?;
    }

    for child_port_id in &child.port_order {
        let child_var = &child.variables[child_port_id];
        let formal = child_var.path.join(".");
        let connection = connections
            .iter()
            .find(|connection| connection.formal == formal);
        let width = child_var.width;
        let Some(direction) = child_port_direction(child, child_var) else {
            return Err(unsupported_sv_inout(formal));
        };
        match direction {
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
                    let from =
                        floating::precision(&actual_expr, parent_variables, parent_signal_names);
                    let to = child_var.float;
                    let (expr, sources, source_ids) = lower_glue_parent_expr(
                        &actual_expr,
                        parent_variables,
                        parent_signal_names,
                        parent_constants,
                        parent_parameter_types,
                        &mut arena,
                        if from.is_some() || to.is_some() {
                            None
                        } else {
                            Some(width)
                        },
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
                    let source_signed = sv_glue_expr_is_signed(
                        &actual_expr,
                        parent_variables,
                        parent_signal_names,
                        parent_parameter_types,
                    );
                    let expr = floating::convert(
                        &mut arena,
                        expr,
                        from,
                        to,
                        width,
                        if from.is_none() {
                            source_signed
                        } else {
                            child_var.signed
                        },
                        true,
                    )
                    .ok_or_else(|| {
                        ParserError::unsupported(
                            sv::SV_FRONTEND_TRACKING_ISSUE,
                            LoweringPhase::SimulatorParser,
                            "real input port conversion",
                            formal.clone(),
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
                } else if let Some(default) = child
                    .source
                    .ports()
                    .iter()
                    .find(|port| port.name() == formal)
                    .and_then(|port| port.default())
                {
                    // IEEE 1800-2023 23.2.2.4: defaults belong to the child's
                    // parameter scope, and apply only to omitted connections.
                    let literal = sv::typecheck::context_size_const_integral_literal(
                        default,
                        &child.constants,
                        &child.parameter_types,
                        width,
                        child_var.signed,
                    )
                    .ok_or_else(|| {
                        ParserError::illegal_context(
                            "systemverilog input port default",
                            formal.clone(),
                            None,
                        )
                    })?;
                    (
                        arena.alloc(SLTNode::Constant(
                            literal.value,
                            literal.mask,
                            width,
                            child_var.signed,
                        ))?,
                        HashSet::default(),
                        Vec::new(),
                    )
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
                        transparent_copy: false,
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
                let target_float = if let [target] = accesses.as_slice() {
                    parent_variables[&target.signal].float
                } else {
                    None
                };
                if child_var.float.is_some() && accesses.len() != 1 {
                    return Err(ParserError::unsupported(
                        sv::SV_FRONTEND_TRACKING_ISSUE,
                        LoweringPhase::SimulatorParser,
                        "real output port concatenation",
                        formal.clone(),
                        None,
                    ));
                }
                let target_signed = if let [target] = accesses.as_slice() {
                    parent_variables[&target.signal].signed
                } else {
                    child_var.signed
                };
                let child_input = floating::convert(
                    &mut arena,
                    child_input,
                    child_var.float,
                    target_float,
                    target_width,
                    if child_var.float.is_none() {
                        child_var.signed
                    } else {
                        target_signed
                    },
                    true,
                )
                .ok_or_else(|| {
                    ParserError::unsupported(
                        sv::SV_FRONTEND_TRACKING_ISSUE,
                        LoweringPhase::SimulatorParser,
                        "real output port conversion",
                        formal.clone(),
                        None,
                    )
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
                            transparent_copy: false,
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
            _ => {}
        }
    }

    Ok((input_ports, output_ports, arena))
}

/// The direction in which the connection of `port` carries its value. An
/// `inout` port carries the value of its net out of the child when the child
/// drives the net, and into it otherwise. `None` is an inout port that the
/// child's frontend does not describe.
fn child_port_direction(child: &LoweredSvModule, port: &SvVariable) -> Option<VariableKind> {
    if port.kind != VariableKind::Inout {
        return Some(port.kind);
    }
    let name = port.path.join(".");
    child
        .source
        .ports()
        .iter()
        .any(|source| source.name() == name && source.direction() == sv::ir::PortDirection::Inout)
        .then(|| {
            if child.inout_driven_inside.contains(&name) {
                VariableKind::Output
            } else {
                VariableKind::Input
            }
        })
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
    if floating::handles(expr, variables, name_to_id) {
        let mut native_arena = SLTNodeArena::new();
        let (node, sources) = floating::lower(
            expr,
            variables,
            name_to_id,
            constants,
            parameter_types,
            &mut native_arena,
        )?;
        let mut cache = HashMap::default();
        let mapped = native_arena
            .get(node)
            .map_addr(node, &native_arena, arena, &mut cache, &|id| {
                GlueAddr::Parent(*id)
            })
            .ok()?;
        let source_ids = sources.iter().map(|source| source.id).collect();
        let sources = sources
            .into_iter()
            .map(|source| {
                VarAtomBase::new(
                    GlueAddr::Parent(source.id),
                    source.access.lsb,
                    source.access.msb,
                )
            })
            .collect();
        return Some((mapped, sources, source_ids));
    }
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
                false,
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
            // As in `lower_expr_with_context`: a run-time bit the element
            // lowering cannot express is read from the flattened array.
            if let Some(rewritten) = runtime_select_as_shift(
                expr,
                msb,
                lsb,
                *signed,
                variables,
                name_to_id,
                constants,
                parameter_types,
                true,
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
                if width != 0 {
                    nodes.push((node, width));
                }
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
            let natural_width =
                sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types);
            let operation_context = context_width
                .map(|width| width.max(natural_width.unwrap_or(width)))
                .or(natural_width);
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
            if context_determined {
                left = coerce_node_width(
                    arena,
                    left,
                    left_context,
                    if shift { left_signed } else { operands_signed },
                )
                .ok()?;
                right = coerce_node_width(arena, right, right_context, operands_signed).ok()?;
            }
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
            let float_condition = floating::condition(condition, variables, name_to_id);
            let (condition, mut sources, mut source_ids) = lower_glue_parent_expr(
                &float_condition,
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
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::Lookup { width, signed },
            args,
            ..
        } => {
            let mut sources = HashSet::default();
            let mut source_ids = Vec::new();
            let node = lower_lookup(
                args,
                *width,
                *signed,
                arena,
                &mut |expr, arena, context_width, context_signed| {
                    let (node, expr_sources, expr_source_ids) = lower_glue_parent_expr(
                        expr,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        context_width,
                        context_signed,
                    )?;
                    sources.extend(expr_sources);
                    source_ids.extend(expr_source_ids);
                    Some(node)
                },
            )?;
            source_ids.sort();
            source_ids.dedup();
            Some((node, sources, source_ids))
        }
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::Stream(stream),
            args,
            ..
        } => {
            let mut sources = HashSet::default();
            let mut source_ids = Vec::new();
            let node = lower_stream(
                args,
                stream,
                arena,
                &mut |expr, arena, context_width, context_signed| {
                    let (node, expr_sources, expr_source_ids) = lower_glue_parent_expr(
                        expr,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        context_width,
                        context_signed,
                    )?;
                    sources.extend(expr_sources);
                    source_ids.extend(expr_source_ids);
                    Some(node)
                },
            )?;
            source_ids.sort();
            source_ids.dedup();
            Some((
                coerce_node_width(arena, node, context_width, context_signed.unwrap_or(false))
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
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::System(function),
            args,
            ..
        } if function.result_type().is_some() => {
            let mut operands = Vec::with_capacity(args.len());
            let mut sources = HashSet::default();
            let mut source_ids = Vec::new();
            for arg in args {
                let arg = arg.as_ref()?;
                let width =
                    sv_expr_natural_width(arg, variables, name_to_id, constants, parameter_types)?;
                let (inner, arg_sources, arg_source_ids) = lower_glue_parent_expr(
                    arg,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    Some(width),
                    None,
                )?;
                operands.push(inner);
                sources.extend(arg_sources);
                source_ids.extend(arg_source_ids);
            }
            Some((
                lower_bit_vector_function_slt(
                    arena,
                    *function,
                    &operands,
                    context_width,
                    context_signed,
                )?,
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
    float: Option<celox_sir::FloatPrecision>,
    value_kind: celox_design::ValueKind,
    width: usize,
    signed: bool,
    is_4state: bool,
    /// Flat structure storage is selected only through analyzer-checked member windows.
    is_unpacked_struct: bool,
    packed_ranges: Vec<(i128, i128)>,
    array_dims: Vec<usize>,
    unpacked_ranges: Vec<(i128, i128)>,
    type_kind: PortTypeKind,
}

fn unpacked_range_bounds(
    range: &sv::ir::UnpackedRange,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<(i128, i128)> {
    // User constant functions belong to the analyzer session, which has
    // ended by lowering. Retain its evaluated bounds as a fallback while
    // keeping the symbolic declaration contract available to other consumers.
    sv::typecheck::eval_const_expr_with_types(range.left(), constants, parameter_types)
        .zip(sv::typecheck::eval_const_expr_with_types(
            range.right(),
            constants,
            parameter_types,
        ))
        .or_else(|| range.resolved_bounds())
}

fn signal_type_from_sv(
    typ: &sv::ir::Type,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Result<SvSignalType, sv::AnalyzerError> {
    // The bounds of the packed dimensions, as the analysis resolved them:
    // a range may name constants of a scope, such as a package, that
    // `constants` does not hold. A range that neither resolves is an error,
    // not a dimension to leave out.
    let packed_ranges: Vec<(i128, i128)> = match typ.packed_bounds() {
        Some(bounds) => bounds.to_vec(),
        None => typ
            .packed_ranges()
            .iter()
            .map(|range| {
                let bound = |expr| {
                    sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types)
                };
                bound(range.left())
                    .zip(bound(range.right()))
                    .ok_or_else(|| {
                        sv::AnalyzerError::Unsupported(format!(
                            "packed dimension whose bounds {:?} and {:?} are not constant here",
                            range.left(),
                            range.right(),
                        ))
                    })
            })
            .collect::<Result<_, _>>()?,
    };
    let packed_width = packed_ranges
        .iter()
        .try_fold(1usize, |acc, &(left, right)| {
            acc.checked_mul(usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?)
        })
        .ok_or_else(|| sv::AnalyzerError::Unsupported("packed width overflow".to_string()))?
        .max(1);
    let array_dims = typ
        .unpacked_ranges()
        .iter()
        .map(|range| {
            let (left, right) = unpacked_range_bounds(range, constants, parameter_types)?;
            usize::try_from(left.abs_diff(right))
                .ok()
                .and_then(|width| width.checked_add(1))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            sv::AnalyzerError::Unsupported("unresolved unpacked array dimension".to_string())
        })?;
    let unpacked_ranges = typ
        .unpacked_ranges()
        .iter()
        .filter_map(|range| unpacked_range_bounds(range, constants, parameter_types))
        .collect();
    let element_count = array_dims
        .iter()
        .copied()
        .try_fold(1usize, usize::checked_mul)
        .ok_or_else(|| sv::AnalyzerError::Unsupported("signal width overflow".to_string()))?;
    let width = packed_width
        .checked_mul(element_count)
        .ok_or_else(|| sv::AnalyzerError::Unsupported("signal width overflow".to_string()))?;
    let signed = typ.is_signed();
    let float = match typ.kind() {
        sv::ir::TypeKind::Real => Some(celox_sir::FloatPrecision::F64),
        sv::ir::TypeKind::Shortreal => Some(celox_sir::FloatPrecision::F32),
        _ => None,
    };
    let is_4state = float.is_none()
        && !matches!(
            typ.kind(),
            sv::ir::TypeKind::Bit
                | sv::ir::TypeKind::String
                | sv::ir::TypeKind::Object
                | sv::ir::TypeKind::Process
                | sv::ir::TypeKind::Event
        );
    let type_kind = match typ.kind() {
        sv::ir::TypeKind::Bit => PortTypeKind::Bit,
        sv::ir::TypeKind::Real => PortTypeKind::Real,
        sv::ir::TypeKind::Shortreal => PortTypeKind::ShortReal,
        sv::ir::TypeKind::String => PortTypeKind::String,
        sv::ir::TypeKind::Logic | sv::ir::TypeKind::Reg | sv::ir::TypeKind::Implicit => {
            PortTypeKind::Logic
        }
        sv::ir::TypeKind::Object | sv::ir::TypeKind::Process | sv::ir::TypeKind::Event => {
            PortTypeKind::Other
        }
    };
    let value_kind = match typ.kind() {
        sv::ir::TypeKind::String => celox_design::ValueKind::StrRef,
        sv::ir::TypeKind::Object => celox_design::ValueKind::ObjRef,
        _ => celox_design::ValueKind::Bits,
    };
    Ok(SvSignalType {
        float,
        value_kind,
        width,
        signed,
        is_4state,
        is_unpacked_struct: typ.structure_identity().is_some(),
        packed_ranges,
        array_dims,
        unpacked_ranges,
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
        sv::ir::Expr::Call { args, .. } => args
            .iter()
            .flatten()
            .any(|arg| expr_references_ident(arg, name)),
    }
}

/// A write to a packed vector whose selected position is a runtime value.
struct DynamicPackedWrite {
    /// Number of bits written.
    select_width: usize,
    /// Where the selected bits sit; see [`RuntimePosition`].
    up: sv::ir::Expr,
    down: sv::ir::Expr,
    /// For a run-time select inside a constant element of an array, the
    /// bits of that element: only they are written.
    window: Option<BitAccess>,
}

/// Recover a constant outer element for a checked dynamic bit select of a
/// one-bit packed element. Its only valid inner index is its declared bound.
/// An arbitrary dynamic unpacked element permits more than one index and must
/// retain its dynamic element addressing.
fn single_bit_select_window(
    lsb: &sv::ir::ConstExpr,
    width: usize,
    constants: &HashMap<String, i128>,
    types: &HashMap<String, (usize, bool)>,
) -> Option<BitAccess> {
    use sv::ir::{BinaryOp, ConstExpr};
    fn selected<'a>(
        mut expr: &'a ConstExpr,
        constants: &HashMap<String, i128>,
        types: &HashMap<String, (usize, bool)>,
    ) -> &'a ConstExpr {
        while let ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } = expr
        {
            match sv::typecheck::eval_const_expr_with_types(condition, constants, types) {
                Some(0) => expr = else_expr,
                Some(_) => expr = then_expr,
                None => break,
            }
        }
        expr
    }
    let (base, offset) = split_dynamic_array_offset(lsb, constants, types)?;
    let ConstExpr::Mux {
        condition,
        then_expr,
        else_expr,
    } = selected(base, constants, types)
    else {
        return None;
    };
    let ConstExpr::Binary {
        left,
        op: BinaryOp::LogicAnd,
        right,
    } = selected(condition, constants, types)
    else {
        return None;
    };
    let bound = |expr, expected| {
        let ConstExpr::Binary { left, op, right } = selected(expr, constants, types) else {
            return None;
        };
        let ConstExpr::Ident(name) = left.as_ref() else {
            return None;
        };
        if *op != expected {
            return None;
        }
        Some((
            name,
            sv::typecheck::eval_const_expr_with_types(right, constants, types)?,
        ))
    };
    let lower = bound(left, BinaryOp::Ge)?;
    let upper = bound(right, BinaryOp::Le)?;
    if lower != upper {
        return None;
    }
    let invalid = sv::typecheck::eval_const_expr_with_types(else_expr, constants, types)?
        .checked_add(offset)?;
    let width = i128::try_from(width).ok()?;
    if (0..width).contains(&invalid) {
        return None;
    }
    let mut checked_index = constants.clone();
    checked_index.insert(lower.0.clone(), lower.1);
    let position = sv::typecheck::eval_const_expr_with_types(then_expr, &checked_index, types)?
        .checked_add(offset)?;
    if !(0..width).contains(&position) {
        return None;
    }
    let position = usize::try_from(position).ok()?;
    Some(BitAccess::new(position, position))
}

/// For a run-time select inside one element of an unpacked array, such as
/// `mem[1][i]`, the bits of that element: the constant part of the flattened
/// position names the element, and the run-time part moves within it.
fn runtime_select_window(
    lsb: &sv::ir::ConstExpr,
    element_width: usize,
    width: usize,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<BitAccess> {
    // A checked bit select inside a one-bit element can only write its zero
    // bit. Preserve the constant outer element in both driver analysis and
    // lowering; an arbitrary run-time array index remains unconstrained.
    if element_width == 1 {
        return single_bit_select_window(lsb, width, constants, parameter_types);
    }
    if element_width == 0 || element_width == width {
        return None;
    }
    let (base, offset) = split_dynamic_array_offset(lsb, constants, parameter_types)?;
    // A run-time element index can reach every element.
    if dynamic_array_base_steps_elements(
        base,
        i128::try_from(element_width).ok()?,
        constants,
        parameter_types,
    ) {
        return None;
    }
    let element = usize::try_from(offset).ok()? / element_width;
    let lsb = element.checked_mul(element_width)?;
    let msb = lsb.checked_add(element_width - 1)?;
    (msb < width).then(|| BitAccess::new(lsb, msb))
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
    // Callers try the element lowering of an array first.
    let position = runtime_select_position(
        name,
        msb,
        lsb,
        variables,
        name_to_id,
        constants,
        parameter_types,
        true,
        false,
    )?;
    let window = (!variable.is_unpacked_struct)
        .then(|| unpacked_element_width(variable))
        .flatten()
        .and_then(|element_width| {
            runtime_select_window(
                lsb,
                element_width,
                variable.width,
                constants,
                parameter_types,
            )
        });
    (position.width <= variable.width).then_some(DynamicPackedWrite {
        select_width: position.width,
        up: position.up,
        down: position.down,
        window,
    })
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

// IEEE 1800-2023 20.9: bit vector functions return known counts/predicates.
fn lower_bit_vector_function_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    function: sv::ir::SystemFunction,
    operands: &[NodeId],
    context_width: Option<usize>,
    context_signed: Option<bool>,
) -> Option<NodeId> {
    let inner = *operands.first()?;
    let known = arena
        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, inner))
        .ok()?;
    use sv::ir::SystemFunction;
    let (width, signed) = function.result_type()?;
    let result = match function {
        // The analyzer folds coverage calls to literals; none reach here.
        SystemFunction::CoverageControl
        | SystemFunction::CoverageGetMax
        | SystemFunction::CoverageGet
        | SystemFunction::CoverageMerge
        | SystemFunction::CoverageSave
        | SystemFunction::GetCoverage => return None,
        SystemFunction::Process(_) => return None,
        SystemFunction::Countbits => {
            let controls = operands[1..]
                .iter()
                .map(|&expr| {
                    arena
                        .alloc(SLTNode::Slice {
                            expr,
                            access: BitAccess::new(0, 0),
                        })
                        .ok()
                })
                .collect::<Option<Vec<_>>>()?;
            let operand_width = celox_slt::get_width(inner, arena);
            let mut matches = Vec::with_capacity(operand_width);
            for bit in (0..operand_width).rev() {
                let selected = arena
                    .alloc(SLTNode::Slice {
                        expr: inner,
                        access: BitAccess::new(bit, bit),
                    })
                    .ok()?;
                let mut matched = None;
                for &control in &controls {
                    // Case equality distinguishes all four states and returns a
                    // known bit. OR makes repeated controls count only once.
                    let equal = arena
                        .alloc(SLTNode::Binary(selected, BinaryOp::EqCase, control))
                        .ok()?;
                    matched = Some(match matched {
                        None => equal,
                        Some(previous) => arena
                            .alloc(SLTNode::Binary(previous, BinaryOp::Or, equal))
                            .ok()?,
                    });
                }
                matches.push((matched?, 1));
            }
            let matching_bits = arena.alloc(SLTNode::Concat(matches)).ok()?;
            let count = arena
                .alloc(SLTNode::Unary(UnaryOp::PopCount, matching_bits))
                .ok()?;
            coerce_node_width(arena, count, Some(32), false).ok()?
        }
        SystemFunction::Clog2 => {
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
        }
        SystemFunction::Isunknown => {
            // Case inequality detects either X or Z, including unknown bits whose
            // value plane is zero and would disappear during two-state conversion.
            arena
                .alloc(SLTNode::Binary(inner, BinaryOp::NeCase, known))
                .ok()?
        }
        SystemFunction::Countones | SystemFunction::Onehot | SystemFunction::Onehot0 => {
            let count = arena.alloc(SLTNode::Unary(UnaryOp::PopCount, known)).ok()?;
            // PopCount has a minimal unsigned width; $countones returns signed int.
            let count = coerce_node_width(arena, count, Some(32), false).ok()?;
            if function == SystemFunction::Countones {
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
                let op = if function == SystemFunction::Onehot {
                    BinaryOp::Eq
                } else {
                    BinaryOp::LeU
                };
                arena.alloc(SLTNode::Binary(count, op, one)).ok()?
            }
        }
        // The random number and time functions need process state, and the
        // analyzer folds the other functions to constants.
        SystemFunction::Ln
        | SystemFunction::Log10
        | SystemFunction::Exp
        | SystemFunction::Sqrt
        | SystemFunction::Floor
        | SystemFunction::Ceil
        | SystemFunction::Sin
        | SystemFunction::Cos
        | SystemFunction::Tan
        | SystemFunction::Asin
        | SystemFunction::Acos
        | SystemFunction::Atan
        | SystemFunction::Sinh
        | SystemFunction::Cosh
        | SystemFunction::Tanh
        | SystemFunction::Asinh
        | SystemFunction::Acosh
        | SystemFunction::Atanh
        | SystemFunction::Pow
        | SystemFunction::Atan2
        | SystemFunction::Hypot
        | SystemFunction::Rtoi
        | SystemFunction::Itor
        | SystemFunction::Realtobits
        | SystemFunction::Bitstoreal
        | SystemFunction::Shortrealtobits
        | SystemFunction::Bitstoshortreal
        | SystemFunction::Time
        | SystemFunction::Stime
        | SystemFunction::Realtime
        | SystemFunction::Random
        | SystemFunction::DistUniform
        | SystemFunction::DistNormal
        | SystemFunction::DistExponential
        | SystemFunction::DistPoisson
        | SystemFunction::DistChiSquare
        | SystemFunction::DistT
        | SystemFunction::DistErlang
        | SystemFunction::TestPlusargs
        | SystemFunction::ValuePlusargs
        | SystemFunction::Urandom
        | SystemFunction::UrandomRange
        | SystemFunction::System
        | SystemFunction::Fopen
        | SystemFunction::ScopeRandomize
        | SystemFunction::ScopeRandomizeEnum
        | SystemFunction::Fread
        | SystemFunction::Fgets
        | SystemFunction::Ferror
        | SystemFunction::Ftell
        | SystemFunction::Fseek
        | SystemFunction::Rewind
        | SystemFunction::Fgetc
        | SystemFunction::Ungetc
        | SystemFunction::Feof
        | SystemFunction::Sscanf
        | SystemFunction::Fscanf
        | SystemFunction::Timeunit
        | SystemFunction::Timeprecision
        | SystemFunction::Isunbounded
        | SystemFunction::Signed
        | SystemFunction::Unsigned
        | SystemFunction::Sampled
        | SystemFunction::Past
        | SystemFunction::Rose
        | SystemFunction::Fell
        | SystemFunction::Stable
        | SystemFunction::Changed
        | SystemFunction::Typename
        | SystemFunction::Bits
        | SystemFunction::Size
        | SystemFunction::Dimensions
        | SystemFunction::UnpackedDimensions
        | SystemFunction::ArrayQuery(_) => return None,
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
    if floating::handles(expr, variables, name_to_id) {
        return floating::lower(
            expr,
            variables,
            name_to_id,
            constants,
            parameter_types,
            arena,
        );
    }
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
                false,
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
            if let Some(rewritten) = runtime_select_as_shift(
                expr,
                msb,
                lsb,
                *signed,
                variables,
                name_to_id,
                constants,
                parameter_types,
                true,
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
                if width != 0 {
                    nodes.push((node, width));
                }
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
            let natural_width =
                sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types);
            let operation_context = context_width
                .map(|width| width.max(natural_width.unwrap_or(width)))
                .or(natural_width);
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
                let base = coerce_node_width(arena, base, operation_context, left_signed).ok()?;
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
            if context_determined {
                left = coerce_node_width(
                    arena,
                    left,
                    left_context,
                    if shift { left_signed } else { operands_signed },
                )
                .ok()?;
                right = coerce_node_width(arena, right, right_context, operands_signed).ok()?;
            }
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
            let float_condition = floating::condition(condition, variables, name_to_id);
            let (condition, mut sources) = lower_expr(
                &float_condition,
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
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::Lookup { width, signed },
            args,
            ..
        } => {
            let mut sources = HashSet::default();
            let node = lower_lookup(
                args,
                *width,
                *signed,
                arena,
                &mut |expr, arena, context_width, context_signed| {
                    let (node, expr_sources) = lower_expr_with_context(
                        expr,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        context_width,
                        context_signed,
                    )?;
                    sources.extend(expr_sources);
                    Some(node)
                },
            )?;
            Some((node, sources))
        }
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::Stream(stream),
            args,
            ..
        } => {
            let mut sources = HashSet::default();
            let node = lower_stream(
                args,
                stream,
                arena,
                &mut |expr, arena, context_width, context_signed| {
                    let (node, expr_sources) = lower_expr_with_context(
                        expr,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        context_width,
                        context_signed,
                    )?;
                    sources.extend(expr_sources);
                    Some(node)
                },
            )?;
            Some((
                coerce_node_width(arena, node, context_width, context_signed.unwrap_or(false))
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
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::System(function),
            args,
            ..
        } if function.result_type().is_some() => {
            let mut operands = Vec::with_capacity(args.len());
            let mut sources = HashSet::default();

            for arg in args {
                let arg = arg.as_ref()?;
                let width =
                    sv_expr_natural_width(arg, variables, name_to_id, constants, parameter_types)?;
                let (inner, arg_sources) = lower_expr_with_context(
                    arg,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    Some(width),
                    None,
                )?;
                operands.push(inner);
                sources.extend(arg_sources);
            }
            Some((
                lower_bit_vector_function_slt(
                    arena,
                    *function,
                    &operands,
                    context_width,
                    context_signed,
                )?,
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

/// The bit position of declared index `index` of `variable`, which may lie
/// outside it; see [`packed_index_offset`].
fn packed_index_position(variable: &SvVariable, index: i128) -> Option<i128> {
    if !variable.array_dims.is_empty() {
        return Some(index);
    }
    match variable.packed_ranges.as_slice() {
        [(left, right)] if left >= right => index.checked_sub(*right),
        [(_, right)] => right.checked_sub(index),
        _ => Some(index),
    }
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
        return dynamic_array_input_slt(
            arena,
            variable,
            signed,
            SLTIndex {
                node: index,
                stride: element_width,
                kind: dynamic_array_index_kind(variable_info, element_width),
            },
            access,
            variable_info.width,
        );
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
        let node = dynamic_array_input_slt(
            arena,
            variable.clone(),
            signed,
            SLTIndex {
                node,
                stride: packed_element_width,
                kind: SLTIndexKind::Unpacked {
                    element_width: packed_element_width,
                },
            },
            BitAccess::new(0, packed_element_width - 1),
            variable_info.width,
        )?;
        nodes.push((node, packed_element_width));
    }
    arena.alloc(SLTNode::Concat(nodes)).ok()
}

/// Bits `access` of the element `index` selects. The input itself spans the
/// whole variable, because shared dependency analysis takes an indexed input's
/// footprint from its access range: a run-time index may reach any element.
/// Lowering composes the outer slice into the indexed load, so the selected
/// element is still loaded directly.
fn dynamic_array_input_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    variable: A,
    signed: bool,
    index: SLTIndex,
    access: BitAccess,
    variable_width: usize,
) -> Option<NodeId> {
    let input = arena
        .alloc(SLTNode::Input {
            variable,
            signed,
            index: vec![index],
            access: BitAccess::new(0, variable_width.checked_sub(1)?),
        })
        .ok()?;
    arena
        .alloc(SLTNode::Slice {
            expr: input,
            access,
        })
        .ok()
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

/// The bits of `variable` that a constant select from declared index `msb`
/// to `lsb` writes, and the number of low bits of the assigned value that
/// fall below the variable. A select partly outside the variable writes only
/// the bits inside, one wholly outside writes none (IEEE 1800-2023 11.5.1).
fn constant_select_write(
    variable: &SvVariable,
    msb: i128,
    lsb: i128,
) -> Option<(Option<BitAccess>, usize)> {
    let (msb, lsb) = (
        packed_index_position(variable, msb)?,
        packed_index_position(variable, lsb)?,
    );
    let (low, high) = (msb.min(lsb), msb.max(lsb));
    let top = i128::try_from(variable.width).ok()? - 1;
    let (start, end) = (low.max(0), high.min(top));
    if start > end {
        return Some((None, 0));
    }
    Some((
        Some(BitAccess::new(
            usize::try_from(start).ok()?,
            usize::try_from(end).ok()?,
        )),
        usize::try_from(start - low).ok()?,
    ))
}

/// `folded`, the select `original` with the values its position reads
/// substituted, unless that turned a run-time select within one array
/// element into a constant one reaching past the element: the run-time form
/// keeps its write within the element (see `dynamic_packed_write`), while
/// the constant position counts from bit 0 of the whole array.
fn keep_element_window(
    original: &sv::ir::LValue,
    folded: sv::ir::LValue,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> sv::ir::LValue {
    let constant =
        |expr| sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types);
    let (
        sv::ir::LValue::Select {
            msb: original_msb,
            lsb: original_lsb,
            ..
        },
        sv::ir::LValue::Select { msb, lsb, .. },
    ) = (original, &folded)
    else {
        return folded;
    };
    if constant(original_msb).is_some() && constant(original_lsb).is_some() {
        return folded;
    }
    let (Some(msb), Some(lsb)) = (constant(msb), constant(lsb)) else {
        return folded;
    };
    let Some(window) =
        dynamic_packed_write(original, variables, name_to_id, constants, parameter_types)
            .and_then(|write| write.window)
    else {
        return folded;
    };
    let inside = |position: i128| {
        usize::try_from(position)
            .is_ok_and(|position| (window.lsb..=window.msb).contains(&position))
    };
    if inside(msb) && inside(lsb) {
        folded
    } else {
        original.clone()
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
    // A bit select has width one regardless of its runtime position. Sampling
    // both bounds evaluates the same (potentially inlined) expression twice,
    // and the zero sample need not even be a valid index computation.
    if msb == lsb {
        return Some(1);
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
    /// The bits of the array element the select stays within, when the
    /// position is measured from its bottom rather than from bit 0.
    window: Option<BitAccess>,
}

/// The select width and runtime position of the `lsb` index for a packed
/// select of `name` whose bounds depend on a runtime value. A variable keeps
/// its declared range; a parameter is a zero-based vector. With
/// `element_window`, a select within one array element is measured within
/// that element, so a position outside it selects no bits.
#[allow(clippy::too_many_arguments)]
fn runtime_select_position(
    name: &str,
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    flat_arrays: bool,
    element_window: bool,
) -> Option<RuntimePosition> {
    let mut window = None;
    let mut whole_array = false;
    let (left, right) = match name_to_id.get(name).and_then(|id| variables.get(id)) {
        Some(variable) if variable.is_unpacked_struct => {
            // Whole structures cannot be bit selected in authored SV. These
            // coordinates come from member selection, which guards each
            // member's own window before flattening its array indices.
            if !flat_arrays {
                return None;
            }
            (i128::try_from(variable.width).ok()?.checked_sub(1)?, 0)
        }
        Some(variable) if !variable.array_dims.is_empty() => {
            // A select the element lowering cannot express, such as a
            // run-time bit of a constant element, addresses the flattened
            // bits of the array.
            if !flat_arrays {
                return None;
            }
            // The analyzer moves a bit select whose inner index is out of
            // range past the whole array, so its flattened position never
            // reaches a neighbouring element. Within a constant element, the
            // position can be measured within it.
            // A write stays in its constant element through its own window
            // (see `dynamic_packed_write`).
            let element = unpacked_element_width(variable).and_then(|element_width| {
                runtime_select_window(
                    lsb,
                    element_width,
                    variable.width,
                    constants,
                    parameter_types,
                )
            });
            // A single element spans the whole array.
            whole_array =
                element.is_none() && unpacked_element_width(variable) != Some(variable.width);
            window = element.filter(|_| element_window);
            match window {
                Some(window) => (
                    i128::try_from(window.msb).ok()?,
                    i128::try_from(window.lsb).ok()?,
                ),
                None => (i128::try_from(variable.width).ok()?.checked_sub(1)?, 0),
            }
        }
        Some(variable) => {
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
    runtime_select_position_in_range(
        left,
        right,
        window,
        whole_array,
        msb,
        lsb,
        variables,
        name_to_id,
        constants,
        parameter_types,
    )
}

fn runtime_select_position_in_range(
    left: i128,
    right: i128,
    window: Option<BitAccess>,
    whole_array: bool,
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<RuntimePosition> {
    if right < 0 {
        return None;
    }
    let width = runtime_select_width(msb, lsb, name_to_id, constants, parameter_types)?;
    // Only a bit select (the same position for both bounds) is moved past
    // the whole array when out of range; a part-select, even of one bit,
    // could reach a neighbouring element.
    if whole_array && (width != 1 || msb != lsb) {
        return None;
    }
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
        window,
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
    flat_arrays: bool,
) -> Option<sv::ir::Expr> {
    let position = if let sv::ir::Expr::Ident(name) = expr {
        runtime_select_position(
            name,
            msb,
            lsb,
            variables,
            name_to_id,
            constants,
            parameter_types,
            flat_arrays,
            flat_arrays,
        )?
    } else {
        let width = sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)?;
        runtime_select_position_in_range(
            i128::try_from(width).ok()?.checked_sub(1)?,
            0,
            None,
            false,
            msb,
            lsb,
            variables,
            name_to_id,
            constants,
            parameter_types,
        )?
    };
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
    // Within an array element, only the element's bits can be selected.
    let vector = match position.window {
        Some(window) => sv::ir::Expr::Select {
            expr: Box::new(expr.clone()),
            msb: sv::ir::ConstExpr::Literal(window.msb.to_string()),
            lsb: sv::ir::ConstExpr::Literal(window.lsb.to_string()),
            signed: false,
        },
        None => expr.clone(),
    };
    let moved = move_down(vector);
    // Shifting fills the missing bits with 0, which is what a two-state
    // vector (or a parameter) reads.
    let four_state = match expr {
        sv::ir::Expr::Ident(name) => name_to_id
            .get(name)
            .and_then(|id| variables.get(id))
            .is_some_and(|variable| variable.is_4state),
        _ => true,
    };
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
    expr_from_const_expr_with_calls(expr, false)
}

/// Reconstruct a select operand, optionally retaining nested runtime calls.
fn expr_from_const_expr_with_calls(
    expr: &sv::ir::ConstExpr,
    allow_calls: bool,
) -> Option<sv::ir::Expr> {
    let convert = |expr| expr_from_const_expr_with_calls(expr, allow_calls);
    Some(match expr {
        sv::ir::ConstExpr::Literal(value) => sv::ir::Expr::Literal(value.clone()),
        sv::ir::ConstExpr::Ident(name) => sv::ir::Expr::Ident(name.clone()),
        sv::ir::ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => sv::ir::Expr::Select {
            expr: Box::new(convert(expr)?),
            msb: (**msb).clone(),
            lsb: (**lsb).clone(),
            signed: *signed,
        },
        sv::ir::ConstExpr::Select { expr, bit } => sv::ir::Expr::Select {
            expr: Box::new(convert(expr)?),
            msb: (**bit).clone(),
            lsb: (**bit).clone(),
            signed: false,
        },
        sv::ir::ConstExpr::Resize {
            expr,
            width,
            signed,
        } => sv::ir::Expr::Resize {
            expr: Box::new(convert(expr)?),
            width: *width,
            signed: *signed,
        },
        sv::ir::ConstExpr::Function { callee, args, .. } if allow_calls => sv::ir::Expr::Call {
            callee: callee.clone(),
            arg_order: Vec::new(),
            args: args
                .iter()
                .map(|arg| convert(arg).map(Some))
                .collect::<Option<_>>()?,
        },
        sv::ir::ConstExpr::Function { .. } => return None,
        sv::ir::ConstExpr::Unary { op, expr } => sv::ir::Expr::Unary {
            op: *op,
            expr: Box::new(convert(expr)?),
        },
        sv::ir::ConstExpr::Binary { left, op, right } => sv::ir::Expr::Binary {
            left: Box::new(convert(left)?),
            op: *op,
            right: Box::new(convert(right)?),
        },
        sv::ir::ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => sv::ir::Expr::Mux {
            condition: Box::new(convert(condition)?),
            then_expr: Box::new(convert(then_expr)?),
            else_expr: Box::new(convert(else_expr)?),
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
    // This is a dynamic bit inside one constant one-bit element, rather
    // than a dynamic element of the whole array. Let the packed lowering
    // retain the checked element window.
    if packed_element_width == 1
        && single_bit_select_window(lsb, variable.width, constants, parameter_types).is_some()
    {
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

/// Whether some run-time term of the base steps by whole elements, as a
/// run-time element index does.
fn dynamic_array_base_steps_elements(
    expr: &sv::ir::ConstExpr,
    element_width: i128,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> bool {
    let steps =
        |expr| dynamic_array_base_steps_elements(expr, element_width, constants, parameter_types);
    let constant =
        |expr| sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types);
    match expr {
        sv::ir::ConstExpr::Binary { left, op, right } => {
            if *op == sv::ir::BinaryOp::Mul {
                let (left_value, right_value) = (constant(left), constant(right));
                if left_value.is_some_and(|value| value > 0 && value % element_width == 0)
                    && right_value.is_none()
                    || right_value.is_some_and(|value| value > 0 && value % element_width == 0)
                        && left_value.is_none()
                {
                    return true;
                }
            }
            steps(left) || steps(right)
        }
        sv::ir::ConstExpr::Mux {
            then_expr,
            else_expr,
            ..
        } => steps(then_expr) || steps(else_expr),
        _ => false,
    }
}

fn dynamic_array_base_has_stride(
    expr: &sv::ir::ConstExpr,
    element_width: i128,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> bool {
    // Every value of the base must be a multiple of the element width: a
    // run-time term without the stride, such as an inner bit index, moves
    // within an element.
    if let Some(value) = sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types)
    {
        return value % element_width == 0;
    }
    let has_stride =
        |expr| dynamic_array_base_has_stride(expr, element_width, constants, parameter_types);
    match expr {
        sv::ir::ConstExpr::Binary { left, op, right } => match op {
            sv::ir::BinaryOp::Mul => has_stride(left) || has_stride(right),
            sv::ir::BinaryOp::Add | sv::ir::BinaryOp::Sub => has_stride(left) && has_stride(right),
            _ => false,
        },
        sv::ir::ConstExpr::Mux {
            then_expr,
            else_expr,
            ..
        } => has_stride(then_expr) && has_stride(else_expr),
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
    // This is a dynamic bit inside one constant one-bit element, rather
    // than a dynamic element of the whole array. Let the packed lowering
    // retain the checked element window.
    if packed_element_width == 1
        && single_bit_select_window(lsb, variable.width, constants, parameter_types).is_some()
    {
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
    HashMap<
        TriggerSet<SourceVarId>,
        celox_frontend_core::symbolic::artifact::SplitCommit<RegionedVarAddr>,
    >,
    HashMap<SourceVarId, SourceVarId>,
    HashMap<
        TriggerSet<SourceVarId>,
        Vec<celox_frontend_core::symbolic::artifact::FfPart<RegionedVarAddr>>,
    >,
    FfEventVars,
    Vec<celox_design::NbaSite<SourceVarId>>,
);

/// The variables `always_ff` event controls are sensed on.
///
/// A variable carries one domain kind, the edge and role of its event, so a
/// signal is sensed on itself at one edge and role, and at any other on a
/// hidden copy of the signal: `negedge clk` beside `posedge clk`, or
/// `posedge rst` as a clock beside `negedge rst` as an asynchronous reset.
/// Each copy is a whole-variable continuous assignment of the signal, so it
/// changes in the same time step as the signal and is a clock derived from
/// it; only domains sensed on a copy pay for it.
///
/// The signal itself keeps the first of rising clock, falling clock, active
/// high reset, active low reset that it is sensed as, independent of the
/// order of the processes: a clock port stays in the domain of the rising
/// clock that drives it, and `Simulator::tick` of the signal is its rising
/// edge whenever a process uses that.
#[derive(Clone, Default)]
pub(crate) struct FfEventVars {
    /// Per signal, the domain kinds it is sensed at and their variables.
    sensed: HashMap<SourceVarId, Vec<(DomainKind, SourceVarId)>>,
    /// The `(copy, signal)` pairs of the hidden copies, in creation order.
    copies: Vec<(SourceVarId, SourceVarId)>,
}

impl FfEventVars {
    /// Sense each signal of `events` on itself at its first kind in
    /// priority order.
    fn new(events: impl Iterator<Item = (SourceVarId, DomainKind)>) -> Self {
        let priority = |kind: DomainKind| match kind {
            DomainKind::ClockPosedge => 0,
            DomainKind::ClockNegedge => 1,
            DomainKind::ResetAsyncHigh => 2,
            DomainKind::ResetAsyncLow => 3,
            DomainKind::Other => 4,
        };
        let mut primary = HashMap::<SourceVarId, DomainKind>::default();
        for (signal, kind) in events {
            primary
                .entry(signal)
                .and_modify(|primary| {
                    if priority(kind) < priority(*primary) {
                        *primary = kind;
                    }
                })
                .or_insert(kind);
        }
        Self {
            sensed: primary
                .into_iter()
                .map(|(signal, kind)| (signal, vec![(kind, signal)]))
                .collect(),
            copies: Vec::new(),
        }
    }

    /// The variable on which `signal` is sensed as `kind`.
    fn sensed(
        &mut self,
        pm: &mut procedural::ProcModule<'_>,
        signal: SourceVarId,
        kind: DomainKind,
    ) -> SourceVarId {
        let sensed = self.sensed.entry(signal).or_default();
        if let Some(&(_, var)) = sensed.iter().find(|(sensed_kind, _)| *sensed_kind == kind) {
            return var;
        }
        let copy = pm.event_copy(signal, kind);
        self.copies.push((copy, signal));
        sensed.push((kind, copy));
        copy
    }

    /// Every sensed variable with its domain kind.
    fn kinds(&self) -> impl Iterator<Item = (SourceVarId, DomainKind)> + '_ {
        self.sensed
            .values()
            .flatten()
            .map(|&(kind, var)| (var, kind))
    }
}

/// A signal a flip-flop evaluation writes and, for nonblocking writes staged
/// apart from blocking ones, its staging value; see
/// `celox_frontend_core::watch::PendingSlots`.
type FfWriteStream = (SourceVarId, Option<SourceVarId>);

/// The write streams one execution of the flip-flop evaluation `eval` may
/// write twice. `staged` lists its staged variables as (variable, value,
/// mask).
fn repeated_ff_writes(
    eval: &ExecutionUnit<RegionedVarAddr>,
    staged: &[(SourceVarId, SourceVarId, SourceVarId)],
) -> Vec<FfWriteStream> {
    // A seed commits the stable state into the next state; only the stores
    // of the body are writes. Every nonblocking write of a staged variable
    // marks its mask, so the value alone tells a repeated write.
    celox_frontend_core::watch::multi_write_candidates(eval, |instruction| match instruction {
        SIRInstruction::Store(address, ..) if address.region == WORKING_REGION => {
            Some(address.var_id)
        }
        _ => None,
    })
    .into_iter()
    .filter_map(
        |id| match staged.iter().find(|&&(_, value, _)| value == id) {
            Some(&(signal, value, _)) => Some((signal, Some(value))),
            None if staged.iter().any(|&(_, _, mask)| mask == id) => None,
            None => Some((id, None)),
        },
    )
    .collect()
}

fn lower_ff_processes(
    module: &sv::ir::Module,
    pm: &mut procedural::ProcModule<'_>,
    ff_parts: bool,
    ff_event_roles: &[FfEventRole],
    shared_memories: &HashSet<String>,
    foreign_writes: &HashMap<String, hierarchy::ForeignWrites>,
    shared_variables: &HashSet<String>,
    repeated_writes: &mut Vec<FfWriteStream>,
) -> Result<SvFfBlocks, sv::AnalyzerError> {
    let mut parallel_ff_parts = HashMap::<_, Vec<_>>::default();
    let mut eval_only_ff_blocks = HashMap::default();
    let mut apply_ff_blocks = HashMap::default();
    let mut active_ff_blocks = HashMap::default();
    let mut nba_ff_blocks: HashMap<_, ExecutionUnit<RegionedVarAddr>> = HashMap::default();
    let mut eval_apply_ff_blocks = HashMap::default();
    let mut reset_clock_map = HashMap::default();
    let mut all_delayed_sites: Vec<celox_design::NbaSite<SourceVarId>> = Vec::new();

    // Every event of every process, as (signal, domain kind): the clock, then
    // the resets.
    let mut process_events = Vec::new();
    for process in module.ff_processes() {
        let clock = clock_event_from_ff_process(process)
            .ok_or_else(|| sv::AnalyzerError::Unsupported("always_ff event control".to_string()))?;
        let event_id = |event: &sv::ir::FfEvent| {
            let id = pm.id(event.signal()).ok_or_else(|| {
                sv::AnalyzerError::Unsupported("always_ff event control".to_string())
            })?;
            if pm
                .variables
                .get(&id)
                .is_some_and(|variable| variable.width != 1)
            {
                return Err(sv::AnalyzerError::Unsupported(
                    "multi-bit always_ff event signal".to_string(),
                ));
            }
            Ok(id)
        };
        let clock_kind = match clock.edge() {
            sv::ir::FfEdge::Pos => DomainKind::ClockPosedge,
            sv::ir::FfEdge::Neg => DomainKind::ClockNegedge,
        };
        let mut events = vec![(event_id(clock)?, clock_kind)];
        for reset in process
            .events()
            .iter()
            .filter(|event| event.signal() != clock.signal())
        {
            let reset_kind = match reset.edge() {
                sv::ir::FfEdge::Pos => DomainKind::ResetAsyncHigh,
                sv::ir::FfEdge::Neg => DomainKind::ResetAsyncLow,
            };
            events.push((event_id(reset)?, reset_kind));
        }
        process_events.push(events);
    }
    // Select primary roles globally before creating copies for retained FFs.
    // Scheduled processes still contribute clock/reset roles and must satisfy
    // the same one-bit event restriction, even though they use wait sites.
    let global_events = ff_event_roles
        .iter()
        .map(|role| {
            let id = pm
                .id(&role.signal)
                .ok_or_else(|| sv::AnalyzerError::Unsupported("always_ff event control".into()))?;
            if pm
                .variables
                .get(&id)
                .is_some_and(|variable| variable.width != 1)
            {
                return Err(sv::AnalyzerError::Unsupported(
                    "multi-bit always_ff event signal".into(),
                ));
            }
            Ok((id, role.kind))
        })
        .collect::<Result<Vec<_>, sv::AnalyzerError>>()?;
    // A hierarchical name denotes another instance's object, whose own
    // declaration decides how it is sensed there: sense it on copies here.
    let hierarchical: HashSet<SourceVarId> = module
        .signals()
        .iter()
        .filter(|signal| signal.hierarchical().is_some())
        .filter_map(|signal| pm.id(signal.name()))
        .collect();
    let mut event_vars = FfEventVars::new(
        global_events
            .into_iter()
            .filter(|(id, _)| !hierarchical.contains(id)),
    );
    // Reflection must retain associations from scheduled host processes too.
    // Resolve sensed roles just as retained FF trigger sets do, including aliases
    // when one authored signal has more than one clock/reset role.
    for role in ff_event_roles {
        if matches!(
            role.kind,
            DomainKind::ResetAsyncHigh | DomainKind::ResetAsyncLow
        ) {
            let reset = pm.id(&role.signal).expect("validated FF event signal");
            let clock = pm.id(&role.clock).expect("validated FF clock signal");
            let reset = event_vars.sensed(pm, reset, role.kind);
            let clock = event_vars.sensed(pm, clock, role.clock_kind);
            reset_clock_map.entry(reset).or_insert(clock);
        }
    }

    let edge_memories =
        edge_only_memories(module, pm.constants, pm.parameter_types, foreign_writes);
    let window_writes = kernel_window_writes(module, foreign_writes);
    let random_seeds = process_random_seeds(module);
    for ((index, process), events) in module.ff_processes().iter().enumerate().zip(process_events) {
        pm.call_unit();
        let mut events = events
            .into_iter()
            .map(|(signal, kind)| event_vars.sensed(pm, signal, kind));
        let mut trigger_set = TriggerSet {
            clock: events.next().expect("a process has a clock event"),
            resets: Vec::new(),
        };
        trigger_set.resets.extend(events);
        // A reset shared by several clock domains is associated with the
        // first; the association only names a clock for the reset signal.
        for reset in &trigger_set.resets {
            reset_clock_map.entry(*reset).or_insert(trigger_set.clock);
        }
        let sites_before = pm.runtime_event_sites.len();
        let extern_calls_before = pm.extern_calls;
        let clock = inferred_clock(pm, process);
        let ff::FfProcessUnits {
            eval: eval_only,
            apply,
            active,
            nba,
            targets,
            staged,
            delayed_sites,
        } = ff::Ff::new(pm, random_seeds[&(0, index)])
            .with_sparse_memories(shared_memories)
            .with_shared_variables(shared_variables)
            .with_sparse_candidates(&edge_memories)
            .with_window_writes(&window_writes)
            .with_inferred_clock(clock)
            .lower_process(process.body())?;
        all_delayed_sites.extend(delayed_sites);
        repeated_writes.extend(repeated_ff_writes(&eval_only, &staged));
        // A process without writes still runs for its runtime events and
        // extern calls.
        let has_effects =
            pm.runtime_event_sites.len() > sites_before || pm.extern_calls > extern_calls_before;
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
        insert_or_merge_ff_unit(&mut apply_ff_blocks, trigger_set.clone(), apply);
        insert_or_merge_ff_unit(&mut active_ff_blocks, trigger_set.clone(), active);
        insert_or_merge_ff_unit(&mut nba_ff_blocks, trigger_set, nba);
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

    let split_commit_ff_blocks = active_ff_blocks
        .into_iter()
        .map(|(trigger_set, active)| {
            let nba = nba_ff_blocks
                .remove(&trigger_set)
                .expect("every trigger group has both commits");
            (
                trigger_set,
                celox_frontend_core::symbolic::artifact::SplitCommit { active, nba },
            )
        })
        .collect();
    Ok((
        eval_only_ff_blocks,
        apply_ff_blocks,
        eval_apply_ff_blocks,
        split_commit_ff_blocks,
        reset_clock_map,
        parallel_ff_parts,
        event_vars,
        all_delayed_sites,
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

/// A plain `always` sensitive to edges is a flip-flop only in the shape of
/// one: a clock and asynchronous resets, each a one-bit variable. Any other
/// edge-sensitive `always` runs as a process waiting on the same events, and
/// so does an edge process of either keyword whose delayed nonblocking
/// assignments the flip-flop form cannot stage.
fn runs_as_process(
    module: &sv::ir::Module,
    process: &sv::ir::FfProcess,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> bool {
    let multi_bit = |name: &str| {
        let r#type = module
            .ports()
            .iter()
            .find(|port| port.name() == name)
            .map(sv::ir::Port::r#type)
            .or_else(|| {
                module
                    .signals()
                    .iter()
                    .find(|signal| signal.name() == name)
                    .map(sv::ir::Signal::r#type)
            });
        r#type
            .and_then(|r#type| signal_type_from_sv(r#type, constants, parameter_types).ok())
            .is_some_and(|type_info| type_info.width != 1)
    };
    // A blocking write through a hierarchical name updates the other
    // instance's object at once, outside that instance's flip-flop commit.
    let writes_other_instance = || {
        let mut hierarchical = module
            .signals()
            .iter()
            .filter(|signal| signal.hierarchical().is_some())
            .peekable();
        if hierarchical.peek().is_none() {
            return false;
        }
        let mut written = HashSet::default();
        procedural::written_names_with_calls(module, process.body(), &mut written);
        hierarchical.any(|signal| written.contains(signal.name()))
    };
    ineligible_delayed_nonblocking(module, process)
        || (!process.is_always_ff()
            && (clock_event_from_ff_process(process).is_none()
                || process
                    .events()
                    .iter()
                    .any(|event| multi_bit(event.signal()))
                || writes_other_instance()))
}

/// Whether a flip-flop's delayed nonblocking assignments cannot take the
/// fused staging path: an event control or nonconstant delay, a partial or
/// array target, a target one run of the body may assign more than once,
/// or a target another process or assignment also writes. Those keep the
/// process-kernel path, whose queued updates preserve the exact timing and
/// order (IEEE 1800-2023 9.4.5, 10.4.2).
fn ineligible_delayed_nonblocking(module: &sv::ir::Module, process: &sv::ir::FfProcess) -> bool {
    let mut targets: Vec<String> = Vec::new();
    let mut ineligible = false;
    for stmt in process.body() {
        stmt.walk(&mut |stmt| {
            let sv::ir::Stmt::DelayedNonblocking {
                control,
                assignment,
            } = stmt
            else {
                return;
            };
            let sv::ir::Stmt::Delay { amount, .. } = control.as_ref() else {
                ineligible = true;
                return;
            };
            if !matches!(amount, sv::ir::Expr::Literal(_) | sv::ir::Expr::Ident(_)) {
                ineligible = true;
                return;
            }
            match assignment.as_ref() {
                sv::ir::Stmt::Assign {
                    lhs: sv::ir::LValue::Ident(name),
                    nonblocking: true,
                    ..
                } => targets.push(name.clone()),
                _ => ineligible = true,
            }
        });
    }
    if ineligible {
        return true;
    }
    if targets.is_empty() {
        return false;
    }
    // A stage holds one value per run. Two delayed assignments of one run
    // are two updates, each an event of its own (10.4.2), which only the
    // queue keeps apart.
    if delayed_targets(process.body(), &mut HashSet::default()) {
        return true;
    }
    // A target of an unpacked array keeps the kernel path: the fused
    // staging publishes whole variables only.
    let array_target = targets.iter().any(|name| {
        module
            .ports()
            .iter()
            .find(|port| port.name() == name)
            .map(sv::ir::Port::r#type)
            .or_else(|| {
                module
                    .signals()
                    .iter()
                    .find(|signal| signal.name() == name)
                    .map(sv::ir::Signal::r#type)
            })
            .is_some_and(|ty| !ty.unpacked_ranges().is_empty())
    });
    if array_target {
        return true;
    }
    // A target another process or a continuous assignment writes keeps the
    // kernel path: the fused commit publishes its variable on its own.
    let mut written = HashSet::default();
    for other in module.ff_processes() {
        if !std::ptr::eq(other, process) {
            procedural::written_names_with_calls(module, other.body(), &mut written);
        }
    }
    for other in module.comb_processes() {
        procedural::written_names_with_calls(module, other.body(), &mut written);
        for assignment in other.assignments() {
            written.insert(assignment.lhs().to_string());
        }
    }
    for other in module.initial_processes() {
        procedural::written_names_with_calls(module, other.body(), &mut written);
    }
    for assignment in module.assignments() {
        written.insert(assignment.lhs().to_string());
    }
    targets.iter().any(|target| written.contains(target))
}

/// Collect into `targets` the variables the delayed nonblocking assignments
/// of `body` may write in one run, and report whether one run may write one
/// of them more than once: two assignments that are not in alternative
/// branches, or one in a loop.
fn delayed_targets<'a>(body: &'a [sv::ir::Stmt], targets: &mut HashSet<&'a str>) -> bool {
    let mut repeated = false;
    for stmt in body {
        let mut own = HashSet::default();
        match stmt {
            sv::ir::Stmt::DelayedNonblocking { assignment, .. } => {
                if let sv::ir::Stmt::Assign { lhs, .. } = assignment.as_ref() {
                    own.insert(lhs.name());
                }
            }
            sv::ir::Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                repeated |= delayed_targets(then_body, &mut own);
                let mut other = HashSet::default();
                repeated |= delayed_targets(else_body, &mut other);
                own.extend(other);
            }
            sv::ir::Stmt::Case { items, default, .. } => {
                let bodies = items
                    .iter()
                    .map(|item| item.body.as_slice())
                    .chain(default.as_deref());
                for body in bodies {
                    let mut alternative = HashSet::default();
                    repeated |= delayed_targets(body, &mut alternative);
                    own.extend(alternative);
                }
            }
            sv::ir::Stmt::Loop {
                init, step, body, ..
            } => {
                repeated |= delayed_targets(init, &mut own);
                let mut iteration = HashSet::default();
                delayed_targets(body, &mut iteration);
                delayed_targets(step, &mut iteration);
                repeated |= !iteration.is_empty();
                own.extend(iteration);
            }
            sv::ir::Stmt::Fork { branches, .. } => {
                for branch in branches {
                    repeated |= delayed_targets(branch, &mut own);
                }
            }
            _ => {}
        }
        repeated |= own.iter().any(|target| targets.contains(target));
        targets.extend(own);
    }
    repeated
}

fn clock_event_from_ff_process(process: &sv::ir::FfProcess) -> Option<&sv::ir::FfEvent> {
    if let [clock] = process.events() {
        return Some(clock);
    }

    // Event OR has no ordering (IEEE 1800-2023 9.4.2.1). For this
    // optimized FF path, resets are the events used as conditions in the
    // body; require exactly one remaining clock candidate.
    let mut clocks = process
        .events()
        .iter()
        .filter(|event| !ff_event_used_as_condition(process, event));
    let clock = clocks.next()?;
    clocks.next().is_none().then_some(clock)
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
            .flatten()
            .any(|arg| expr_uses_ident_as_condition(arg, name)),
        sv::ir::Expr::Ident(_) | sv::ir::Expr::Literal(_) => false,
    }
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
        sv::ir::Expr::Call { callee, .. } => callee.result_type().is_some_and(|(_, signed)| signed),
        sv::ir::Expr::Concat(_) | sv::ir::Expr::RepeatConcat { .. } => false,
        sv::ir::Expr::Unary { op, expr } => {
            matches!(
                op,
                sv::ir::UnaryOp::Plus
                    | sv::ir::UnaryOp::Minus
                    | sv::ir::UnaryOp::BitNot
                    | sv::ir::UnaryOp::ToTwoState
            ) && sv_glue_expr_is_signed(expr, variables, name_to_id, parameter_types)
        }
        sv::ir::Expr::Binary { left, op, right } => match op {
            sv::ir::BinaryOp::Pow
            | sv::ir::BinaryOp::Shl
            | sv::ir::BinaryOp::Shr
            | sv::ir::BinaryOp::Sar => {
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
        sv::ir::Expr::Call { callee, .. } => callee.result_type().is_some_and(|(_, signed)| signed),
        sv::ir::Expr::Concat(_) | sv::ir::Expr::RepeatConcat { .. } => false,
        sv::ir::Expr::Unary { op, expr } => {
            matches!(
                op,
                sv::ir::UnaryOp::Plus
                    | sv::ir::UnaryOp::Minus
                    | sv::ir::UnaryOp::BitNot
                    | sv::ir::UnaryOp::ToTwoState
            ) && sv_expr_is_signed_with_parameters(expr, variables, name_to_id, parameter_types)
        }
        sv::ir::Expr::Binary { left, op, right } => match op {
            sv::ir::BinaryOp::Pow
            | sv::ir::BinaryOp::Shl
            | sv::ir::BinaryOp::Shr
            | sv::ir::BinaryOp::Sar => {
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
                sv::ir::BinaryOp::Shl
                    | sv::ir::BinaryOp::Shr
                    | sv::ir::BinaryOp::Sar
                    | sv::ir::BinaryOp::Pow
            ) {
                // Shift counts and exponents are self-determined (Table 11-21).
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
        sv::ir::Expr::Call { callee, .. } => callee.result_type().map(|(width, _)| width),
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

/// The widths and signedness of the parameters a module can name.
fn module_parameter_types(module: &sv::ir::Module) -> HashMap<String, (usize, bool)> {
    module
        .imported_parameters()
        .iter()
        .chain(module.parameters())
        .filter_map(|parameter| {
            Some((
                parameter.name().to_string(),
                (
                    parameter.resolved_width()?,
                    parameter.resolved_signed().unwrap_or(false),
                ),
            ))
        })
        .collect()
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
    // The package parameters a module uses come before its own.
    for parameter in module.imported_parameters() {
        if let Some(value) = parameter.resolved_value() {
            constants.insert(parameter.name().to_string(), value);
        }
    }
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

/// The clock that the sampled value functions of an edge-triggered process
/// infer (IEEE 1800-2023 16.14.6): its event expression of the clock, when
/// it is the only one whose signal the body reads nowhere else. With an
/// asynchronous reset the body reads the reset, so the clock remains.
fn inferred_clock(
    pm: &procedural::ProcModule<'_>,
    process: &sv::ir::FfProcess,
) -> Option<ff::Clock> {
    let clock = clock_event_from_ff_process(process)?;
    let reads: HashSet<SourceVarId> = pm
        .implicit_reads(process.body(), true)
        .into_iter()
        .map(|atom| atom.id)
        .collect();
    let unread: Vec<_> = process
        .events()
        .iter()
        .filter(|event| pm.id(event.signal()).is_none_or(|id| !reads.contains(&id)))
        .collect();
    match unread.as_slice() {
        [only] if only.signal() == clock.signal() && only.edge() == clock.edge() => {
            Some(ff::Clock {
                signal: clock.signal().to_owned(),
                rising: clock.edge() == sv::ir::FfEdge::Pos,
                alone: process.events().len() == 1,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod interface_reflection_tests;
