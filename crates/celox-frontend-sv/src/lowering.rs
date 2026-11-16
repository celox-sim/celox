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
    BinaryOp, BitAccess, DisplaySizing, DomainKind, ExternDirection, ExternFunction,
    ExternSignature, ExternType, InitialStateData, InitialStateValue, ModuleId, NbaSite,
    PROCESS_DELAY_WIDTH, PROCESS_STATUS_WIDTH, PortTypeKind, ProcessSlots, RegionedVarAddrBase,
    ReportLocation, RuntimeErrorInfo, RuntimeEventKind, RuntimeEventSite, STABLE_REGION, Severity,
    TIME_SLOT_WIDTH, TriggerSet, UnaryOp, VarAtomBase, WORKING_REGION, WaitSite,
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
mod comb_processes;
mod comb_retention;
mod driver_validation;
mod drivers;
mod expr;
mod external_hierarchy;
mod ff;
mod ff_processes;
mod floating;
mod glue;
mod glue_expr;
mod hierarchy;
mod initial_processes;
mod instances;
mod interfaces;
mod masked;
mod module;
mod net_resolution;
mod operators;
mod out_of_line;
mod packages;
mod procedural;
mod recursion;
mod schedule;
mod select;
mod shared_variables;
mod sources;
mod specialize;
mod state_mode;
mod types;
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
use comb_processes::*;
use driver_validation::*;
use drivers::*;
use expr::*;
use ff_processes::*;
use glue::*;
use glue_expr::*;
use initial_processes::*;
use instances::*;
use interfaces::*;
use module::*;
use operators::*;
use packages::*;
use select::*;
use shared_variables::*;
use sources::*;
use specialize::*;
use state_mode::*;
use types::*;

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
    /// Whether the design calls `$timeformat`, whose format every module
    /// shares (see `ff::TIME_FORMAT`).
    time_format: bool,
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
            time_format: false,
            virtual_ids: None,
            discovery: false,
        }
    }
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

fn next_var_id(next_id: &mut SourceVarId) -> SourceVarId {
    let id = *next_id;
    next_id.0 += 1;
    id
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
mod tests;

#[cfg(test)]
mod interface_reflection_tests;
