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
mod comb_processes;
mod comb_retention;
mod driver_validation;
mod drivers;
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
use comb_processes::*;
use driver_validation::*;
use drivers::*;
use ff_processes::*;
use glue::*;
use glue_expr::*;
use initial_processes::*;
use instances::*;
use interfaces::*;
use module::*;
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
mod tests;

#[cfg(test)]
mod interface_reflection_tests;
