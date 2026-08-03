//! Detects dynamic `for` loops whose continuation bound may be affected while
//! the loop is executing.
//!
//! # Analysis contract
//!
//! This is a Celox frontend rule, not a choice between capture-on-entry and
//! per-iteration evaluation as Veryl language semantics. Celox may capture a
//! continuation bound while emitted synthesizable SystemVerilog evaluates its
//! loop condition on every iteration. An immediate body write that makes this
//! difference observable is therefore an error.
//!
//! Testbench loops have a different compatibility requirement. Celox currently
//! executes them only through its native testbench compiler, which captures
//! the bound before entering the loop; it does not emit a SystemVerilog
//! testbench. A time-advancing body is consequently not rejected. Instead, the
//! checker warns only when the advanced event's write closure reaches state
//! read by the original bound. This identifies fragile code without warning on
//! unrelated clocks or writes to disjoint state. A procedural `let` can be
//! used when an explicit snapshot is intended.
//!
//! The checker uses a structural, bit-level may-dependency relation. It does
//! not attempt to prove runtime value equality, branch reachability, event-edge
//! feasibility, or program termination:
//!
//! - selects, slices, concatenations, and fixed shifts preserve their known bit
//!   mapping;
//! - bitwise operators relate corresponding operand and result bits;
//! - arithmetic, dynamic shifts, and other non-local operators conservatively
//!   affect their whole result;
//! - a tainted branch condition may affect every write controlled by it;
//! - a dynamic index may address the whole underlying object; and
//! - time-advancing operations close transitively over combinational logic,
//!   generated or gated events, and the FF domains those events may activate.
//!
//! The relation is conservative rather than canonical. A sound simplification
//! may prove that an apparent dependency, such as `x & 0`, cannot affect the
//! result and remove it. Dependencies which remain after such simplification
//! may still include algebraically redundant cases. The guarantee below
//! depends on never discarding a possible influence merely because it is hard
//! to analyze; those cases must remain dependent or become unknown.
//!
//! A known overlap between continuation-bound reads and immediate body writes
//! is an error. Event-mediated overlap is a warning, as is an opaque or
//! otherwise unresolvable effect. More precise event analysis can therefore
//! turn a conservative warning into a clean pass, but never changes an
//! immediate-write error.
//!
//! Opaque SystemVerilog components, DPI/VPI callbacks, `force`/`release`,
//! `bind`, and other external mechanisms are outside that guarantee. If Celox
//! can see an opaque boundary but cannot derive its effects, the checker must
//! warn rather than silently treating it as pure.
//!
//! The analyzer-IR pass runs before lowering captures a bound into a pre-loop
//! value. It preserves statement provenance, expands known Veryl functions,
//! uses flattened bit accesses for alias checks, and distinguishes immediate
//! procedural writes from FF/NBA writes. A second pass after hierarchy
//! elaboration resolves event-mediated effects to concrete state identities.

use celox_design::{BinaryOp, BitAccess, STABLE_REGION, StateAddr, UnaryOp};
use celox_sir::{BlockId, ExecutionUnit, RegisterId, SIRInstruction, SIRTerminator};
use num_traits::ToPrimitive as _;
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignDestination, CasePattern, Component, Declaration, Expression, Factor,
    ForBound, ForRange, ForStatement, FunctionCall, Ir, Module, Statement, SystemFunctionCall,
    SystemFunctionKind, TbMethod, VarId, VarIndex, VarSelect,
};
use veryl_analyzer::symbol::Affiliation;
use veryl_parser::resource_table::StrId;

use crate::{
    FrontendDiagnostic, FusedSirOptimizationHints, HashMap, HashSet, ScheduledRtl,
    bitaccess::eval_var_select,
};

#[derive(Clone, Copy)]
struct Access {
    id: VarId,
    bits: BitAccess,
    /// The write is committed after the enclosing always_ff activation and is
    /// therefore invisible to the loop's continuation test.
    deferred: bool,
}

#[derive(Clone, Copy)]
enum StateChange {
    Clock(StrId),
    Reset { reset: StrId, clock: StrId },
}

#[derive(Default)]
struct Effects {
    reads: Vec<Access>,
    writes: Vec<Access>,
    state_changes: Vec<StateChange>,
    observable: bool,
    unknown: Option<String>,
}

impl Effects {
    fn mark_unknown(&mut self, detail: impl Into<String>) {
        if self.unknown.is_none() {
            self.unknown = Some(detail.into());
        }
    }

    fn append(&mut self, mut other: Effects) {
        self.reads.append(&mut other.reads);
        self.writes.append(&mut other.writes);
        self.state_changes.append(&mut other.state_changes);
        self.observable |= other.observable;
        if self.unknown.is_none() {
            self.unknown = other.unknown;
        }
    }

    fn discard_function_locals(&mut self, module: &Module) {
        let is_function_local = |id: VarId| {
            module
                .variables
                .get(&id)
                .is_some_and(|variable| variable.affiliation == Affiliation::Function)
        };
        self.reads.retain(|access| !is_function_local(access.id));
        self.writes.retain(|access| !is_function_local(access.id));
    }
}

pub fn check_dynamic_for_bounds(ir: &Ir) -> Vec<FrontendDiagnostic> {
    let mut diagnostics = Vec::new();
    for component in &ir.components {
        if let Component::Module(module) = component
            && !module.suppress_unassigned
        {
            check_module(module, &mut diagnostics);
        }
    }
    diagnostics
}

/// Resolves testbench time-advancing effects after hierarchy flattening.
///
/// At this point every root variable and event domain has a concrete state
/// identity, and the scheduled SIR contains the transitive FF/comb work for
/// each event. This lets a loop that advances time either receive a targeted
/// warning or pass cleanly instead of warning about every `clock.next()`.
pub fn check_elaborated_dynamic_for_bounds(
    scheduled: &ScheduledRtl,
    module: &Module,
    hints: &FusedSirOptimizationHints,
) -> Vec<FrontendDiagnostic> {
    let source = &scheduled.testbench_source;
    let Some(statements) = &source.initial_statements else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    check_elaborated_statements(statements, module, scheduled, hints, &mut diagnostics);
    for function in module.functions.values() {
        for body in &function.functions {
            check_elaborated_statements(
                &body.statements,
                module,
                scheduled,
                hints,
                &mut diagnostics,
            );
        }
    }
    diagnostics
}

fn check_elaborated_statements(
    statements: &[Statement],
    module: &Module,
    scheduled: &ScheduledRtl,
    hints: &FusedSirOptimizationHints,
    diagnostics: &mut Vec<FrontendDiagnostic>,
) {
    for statement in statements {
        match statement {
            Statement::If(statement) => {
                check_elaborated_statements(
                    &statement.true_side,
                    module,
                    scheduled,
                    hints,
                    diagnostics,
                );
                check_elaborated_statements(
                    &statement.false_side,
                    module,
                    scheduled,
                    hints,
                    diagnostics,
                );
            }
            Statement::IfReset(statement) => {
                check_elaborated_statements(
                    &statement.true_side,
                    module,
                    scheduled,
                    hints,
                    diagnostics,
                );
                check_elaborated_statements(
                    &statement.false_side,
                    module,
                    scheduled,
                    hints,
                    diagnostics,
                );
            }
            Statement::Case(statement) => {
                for arm in &statement.arms {
                    check_elaborated_statements(&arm.body, module, scheduled, hints, diagnostics);
                }
                check_elaborated_statements(
                    &statement.default,
                    module,
                    scheduled,
                    hints,
                    diagnostics,
                );
            }
            Statement::For(statement) => {
                check_elaborated_for(statement, module, scheduled, hints, diagnostics);
                check_elaborated_statements(&statement.body, module, scheduled, hints, diagnostics);
            }
            Statement::Assign(_)
            | Statement::FunctionCall(_)
            | Statement::SystemFunctionCall(_)
            | Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => {}
        }
    }
}

fn check_elaborated_for(
    statement: &ForStatement,
    module: &Module,
    scheduled: &ScheduledRtl,
    hints: &FusedSirOptimizationHints,
    diagnostics: &mut Vec<FrontendDiagnostic>,
) {
    let continuation_bound = match &statement.range {
        ForRange::Forward { end, .. } | ForRange::Stepped { end, .. } => end,
        ForRange::Reverse { start, .. } => start,
    };
    let ForBound::Expression(bound) = continuation_bound else {
        return;
    };

    let mut active_functions = HashSet::default();
    let bound_effects = collect_expression_effects(bound, module, &mut active_functions);
    let mut active_functions = HashSet::default();
    let body_effects =
        collect_statement_effects(&statement.body, module, &mut active_functions, false);
    if body_effects.state_changes.is_empty() {
        return;
    }

    // Immediate conflicts were already diagnosed by the analyzer-IR pass.
    let immediate_conflict = bound_effects.reads.iter().any(|read| {
        body_effects
            .writes
            .iter()
            .any(|write| accesses_conflict(read, write))
    });
    let bound_has_visible_effect =
        bound_effects.observable || bound_effects.writes.iter().any(|write| !write.deferred);
    if immediate_conflict || bound_has_visible_effect {
        return;
    }

    let mut unknown = bound_effects.unknown.or(body_effects.unknown);
    let writes =
        collect_state_change_writes(&body_effects.state_changes, scheduled, hints, &mut unknown);
    let mut conflict = false;
    for read in &bound_effects.reads {
        let Some((address, _)) = scheduled.frontend_lookup.root_variable(read.id) else {
            unknown.get_or_insert_with(|| {
                format!(
                    "bound variable `{}` could not be projected after elaboration",
                    read.id
                )
            });
            continue;
        };
        if writes.iter().any(|write| {
            write.address == address
                && write
                    .bits
                    .is_none_or(|write_bits| write_bits.overlaps(&read.bits))
        }) {
            conflict = true;
            break;
        }
    }

    if conflict {
        diagnostics.push(FrontendDiagnostic::time_advancing_for_bound(
            &statement.token,
            "the loop body advances an event that may update state read by the continuation bound",
        ));
    } else if let Some(detail) = unknown {
        diagnostics.push(FrontendDiagnostic::unknown_for_bound_effect(
            &statement.token,
            detail,
        ));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct StateWrite {
    address: StateAddr,
    /// `None` denotes a dynamic or otherwise non-static range of this object.
    bits: Option<BitAccess>,
}

fn collect_state_change_writes(
    changes: &[StateChange],
    scheduled: &ScheduledRtl,
    hints: &FusedSirOptimizationHints,
    unknown: &mut Option<String>,
) -> Vec<StateWrite> {
    let mut writes = Vec::new();
    for change in changes {
        match *change {
            StateChange::Clock(clock) => {
                let Some((event, info)) = scheduled.frontend_lookup.root_named_variable(clock)
                else {
                    unknown.get_or_insert_with(|| {
                        format!("clock `{clock}` could not be resolved after elaboration")
                    });
                    continue;
                };
                push_whole_state_write(event, info.width, &mut writes);
                push_direct_event_writes(event, scheduled, hints, &mut writes);
            }
            StateChange::Reset { reset, clock } => {
                let Some((reset_signal, reset_info)) =
                    scheduled.frontend_lookup.root_named_variable(reset)
                else {
                    unknown.get_or_insert_with(|| {
                        format!("reset `{reset}` could not be resolved after elaboration")
                    });
                    continue;
                };
                push_whole_state_write(reset_signal, reset_info.width, &mut writes);

                let Some((clock_event, clock_info)) =
                    scheduled.frontend_lookup.root_named_variable(clock)
                else {
                    unknown.get_or_insert_with(|| {
                        format!("reset clock `{clock}` could not be resolved after elaboration")
                    });
                    continue;
                };
                push_whole_state_write(clock_event, clock_info.width, &mut writes);
                push_direct_event_writes(clock_event, scheduled, hints, &mut writes);
            }
        }
    }
    // Advancing one event can update a generated/gated clock through comb
    // logic, which in turn activates another FF domain in the same timed
    // step. Mirror the runtime's event-discovery loop and close over both
    // comb propagation and newly reached event domains.
    loop {
        let before = writes.clone();
        propagate_comb_writes(&scheduled.sir.eval_comb, &mut writes);
        for event in &scheduled.design.events.ordered_events {
            if writes
                .iter()
                .any(|write| scheduled.design.events.canonical(write.address) == *event)
            {
                push_direct_event_writes(*event, scheduled, hints, &mut writes);
            }
        }
        if writes == before {
            break;
        }
    }
    writes
}

fn push_whole_state_write(address: StateAddr, width: usize, writes: &mut Vec<StateWrite>) {
    writes.push(StateWrite {
        address,
        bits: width.checked_sub(1).map(|msb| BitAccess::new(0, msb)),
    });
}

fn push_direct_event_writes(
    event: StateAddr,
    scheduled: &ScheduledRtl,
    hints: &FusedSirOptimizationHints,
    writes: &mut Vec<StateWrite>,
) {
    let event = scheduled.design.events.canonical(event);
    if let Some(direct) = hints.direct_ff_writes.get(&event) {
        for atom in direct {
            add_state_write(
                writes,
                StateWrite {
                    address: atom.id.absolute_addr(),
                    bits: Some(atom.access),
                },
            );
        }
    }
    for units in [
        scheduled.sir.eval_apply_ffs.get(&event),
        scheduled.sir.apply_ffs.get(&event),
    ]
    .into_iter()
    .flatten()
    {
        for unit in units {
            push_unit_stable_writes(unit, writes);
        }
    }
}

fn push_unit_stable_writes(
    unit: &ExecutionUnit<celox_design::RegionedStateAddr>,
    writes: &mut Vec<StateWrite>,
) {
    for block in unit.blocks.values() {
        for instruction in &block.instructions {
            let (address, offset, width) = match instruction {
                SIRInstruction::Store(address, offset, width, ..)
                    if address.region == STABLE_REGION =>
                {
                    (address.absolute_addr(), offset, *width)
                }
                SIRInstruction::Commit(_, address, offset, width, _)
                    if address.region == STABLE_REGION =>
                {
                    (address.absolute_addr(), offset, *width)
                }
                _ => continue,
            };
            add_state_write(
                writes,
                StateWrite {
                    address,
                    bits: offset
                        .constant_bit_offset()
                        .and_then(|lsb| access_from_offset(lsb, width)),
                },
            );
        }
    }
}

fn propagate_comb_writes(
    units: &[ExecutionUnit<celox_design::RegionedStateAddr>],
    writes: &mut Vec<StateWrite>,
) {
    // A later unit may feed an earlier one when a conservative scheduler path
    // was retained. Iterate to a fixed point rather than relying on unit order.
    loop {
        let before = writes.clone();
        for unit in units {
            propagate_comb_unit(unit, writes);
        }
        if *writes == before {
            break;
        }
    }
}

fn propagate_comb_unit(
    unit: &ExecutionUnit<celox_design::RegionedStateAddr>,
    writes: &mut Vec<StateWrite>,
) {
    let mut registers: HashMap<RegisterId, Vec<BitAccess>> = HashMap::default();
    let mut control_tainted: HashSet<BlockId> = HashSet::default();
    let constants = known_usize_constants(unit);

    loop {
        let mut changed = false;
        for block in unit.blocks.values() {
            let block_control = control_tainted.contains(&block.id);
            for instruction in &block.instructions {
                match instruction {
                    SIRInstruction::Imm(..) => {}
                    SIRInstruction::Load(dst, address, offset, width) => {
                        let mut taint = load_taint(address.absolute_addr(), offset, *width, writes);
                        if offset
                            .dynamic_registers()
                            .into_iter()
                            .flatten()
                            .any(|register| register_is_tainted(&registers, register))
                        {
                            taint = whole_register_taint(unit, *dst);
                        }
                        changed |= extend_register_taint(&mut registers, *dst, taint);
                    }
                    SIRInstruction::Unary(dst, operation, source) => {
                        let source_taint = registers.get(source).cloned().unwrap_or_default();
                        let taint = match operation {
                            UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::BitNot
                                if register_width(unit, *dst) == register_width(unit, *source) =>
                            {
                                source_taint
                            }
                            _ if !source_taint.is_empty() => whole_register_taint(unit, *dst),
                            _ => Vec::new(),
                        };
                        changed |= extend_register_taint(&mut registers, *dst, taint);
                    }
                    SIRInstruction::Binary(dst, lhs, operation, rhs) => {
                        let lhs_taint = registers.get(lhs).cloned().unwrap_or_default();
                        let rhs_taint = registers.get(rhs).cloned().unwrap_or_default();
                        let taint = match operation {
                            BinaryOp::Shl if rhs_taint.is_empty() => {
                                constants.get(rhs).map_or_else(
                                    || {
                                        if lhs_taint.is_empty() {
                                            Vec::new()
                                        } else {
                                            whole_register_taint(unit, *dst)
                                        }
                                    },
                                    |shift| {
                                        shift_taint_left(
                                            &lhs_taint,
                                            *shift,
                                            register_width(unit, *dst),
                                        )
                                    },
                                )
                            }
                            BinaryOp::Shr if rhs_taint.is_empty() => {
                                constants.get(rhs).map_or_else(
                                    || {
                                        if lhs_taint.is_empty() {
                                            Vec::new()
                                        } else {
                                            whole_register_taint(unit, *dst)
                                        }
                                    },
                                    |shift| {
                                        shift_taint_right(
                                            &lhs_taint,
                                            *shift,
                                            register_width(unit, *dst),
                                        )
                                    },
                                )
                            }
                            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor
                                if register_width(unit, *dst) == register_width(unit, *lhs)
                                    && register_width(unit, *dst) == register_width(unit, *rhs) =>
                            {
                                lhs_taint.into_iter().chain(rhs_taint).collect()
                            }
                            _ if !lhs_taint.is_empty() || !rhs_taint.is_empty() => {
                                whole_register_taint(unit, *dst)
                            }
                            _ => Vec::new(),
                        };
                        changed |= extend_register_taint(&mut registers, *dst, taint);
                    }
                    SIRInstruction::Concat(dst, sources) => {
                        let total_width = sources
                            .iter()
                            .map(|source| register_width(unit, *source))
                            .sum::<usize>();
                        let mut cursor = total_width;
                        let mut taint = Vec::new();
                        for source in sources {
                            let width = register_width(unit, *source);
                            cursor = cursor.saturating_sub(width);
                            taint.extend(
                                registers
                                    .get(source)
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|bits| shift_access(*bits, cursor)),
                            );
                        }
                        changed |= extend_register_taint(&mut registers, *dst, taint);
                    }
                    SIRInstruction::Slice(dst, source, offset, width) => {
                        let selected = access_from_offset(*offset, *width);
                        let taint = selected.map_or_else(Vec::new, |selected| {
                            registers
                                .get(source)
                                .into_iter()
                                .flatten()
                                .filter_map(|bits| intersect_access(*bits, selected))
                                .filter_map(|bits| shift_access_down(bits, *offset))
                                .collect()
                        });
                        changed |= extend_register_taint(&mut registers, *dst, taint);
                    }
                    SIRInstruction::Mux(dst, condition, then_value, else_value) => {
                        let taint = if register_is_tainted(&registers, *condition) {
                            whole_register_taint(unit, *dst)
                        } else {
                            registers
                                .get(then_value)
                                .into_iter()
                                .flatten()
                                .chain(registers.get(else_value).into_iter().flatten())
                                .copied()
                                .collect()
                        };
                        changed |= extend_register_taint(&mut registers, *dst, taint);
                    }
                    SIRInstruction::Store(address, offset, width, source, ..) => {
                        let address_tainted = offset
                            .dynamic_registers()
                            .into_iter()
                            .flatten()
                            .any(|register| register_is_tainted(&registers, register));
                        let source_taint = registers.get(source).cloned().unwrap_or_default();
                        let state_taint = store_taint(
                            address.absolute_addr(),
                            offset,
                            *width,
                            &source_taint,
                            block_control || address_tainted,
                        );
                        for write in state_taint {
                            changed |= add_state_write(writes, write);
                        }
                    }
                    SIRInstruction::Commit(source, destination, offset, width, _) => {
                        let source_taint =
                            load_taint(source.absolute_addr(), offset, *width, writes);
                        let state_taint = store_taint(
                            destination.absolute_addr(),
                            offset,
                            *width,
                            &source_taint,
                            block_control,
                        );
                        for write in state_taint {
                            changed |= add_state_write(writes, write);
                        }
                    }
                    SIRInstruction::RuntimeEvent { .. }
                    | SIRInstruction::CombCaptureEvent { .. }
                    | SIRInstruction::CombCaptureEnableIfChanged { .. } => {}
                }
            }

            match &block.terminator {
                SIRTerminator::Jump(target, args) => {
                    let source_registers = registers.clone();
                    changed |= propagate_block_args(
                        unit,
                        &source_registers,
                        *target,
                        args,
                        &mut registers,
                    );
                    if block_control {
                        changed |= control_tainted.insert(*target);
                    }
                }
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    for (target, args) in [true_block, false_block] {
                        let target = *target;
                        changed |= propagate_block_args(
                            unit,
                            &registers.clone(),
                            target,
                            args,
                            &mut registers,
                        );
                        if block_control || register_is_tainted(&registers, *cond) {
                            changed |= control_tainted.insert(target);
                        }
                    }
                }
                SIRTerminator::Switch {
                    selector,
                    cases,
                    default,
                } => {
                    if block_control || register_is_tainted(&registers, *selector) {
                        for target in cases
                            .iter()
                            .map(|case| case.target)
                            .chain(std::iter::once(*default))
                        {
                            changed |= control_tainted.insert(target);
                        }
                    }
                }
                SIRTerminator::Return | SIRTerminator::Error(_) => {}
            }
        }
        if !changed {
            break;
        }
    }
}

fn known_usize_constants(
    unit: &ExecutionUnit<celox_design::RegionedStateAddr>,
) -> HashMap<RegisterId, usize> {
    unit.blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            SIRInstruction::Imm(register, value) if value.mask == 0u8.into() => {
                value.payload.to_usize().map(|value| (*register, value))
            }
            _ => None,
        })
        .collect()
}

fn shift_taint_left(taint: &[BitAccess], shift: usize, width: usize) -> Vec<BitAccess> {
    let Some(result) = access_from_offset(0, width) else {
        return Vec::new();
    };
    taint
        .iter()
        .filter_map(|bits| shift_access(*bits, shift))
        .filter_map(|bits| intersect_access(bits, result))
        .collect()
}

fn shift_taint_right(taint: &[BitAccess], shift: usize, width: usize) -> Vec<BitAccess> {
    let Some(source) = access_from_offset(shift, width) else {
        return Vec::new();
    };
    taint
        .iter()
        .filter_map(|bits| intersect_access(*bits, source))
        .filter_map(|bits| shift_access_down(bits, shift))
        .collect()
}

fn register_width(
    unit: &ExecutionUnit<celox_design::RegionedStateAddr>,
    register: RegisterId,
) -> usize {
    unit.register_map
        .get(&register)
        .map(|r#type| r#type.width())
        .unwrap_or(0)
}

fn whole_register_taint(
    unit: &ExecutionUnit<celox_design::RegionedStateAddr>,
    register: RegisterId,
) -> Vec<BitAccess> {
    access_from_offset(0, register_width(unit, register))
        .into_iter()
        .collect()
}

fn register_is_tainted(
    registers: &HashMap<RegisterId, Vec<BitAccess>>,
    register: RegisterId,
) -> bool {
    registers
        .get(&register)
        .is_some_and(|bits| !bits.is_empty())
}

fn extend_register_taint(
    registers: &mut HashMap<RegisterId, Vec<BitAccess>>,
    register: RegisterId,
    taint: Vec<BitAccess>,
) -> bool {
    let target = registers.entry(register).or_default();
    let before = target.len();
    for bits in taint {
        if !target
            .iter()
            .any(|known| known.lsb <= bits.lsb && known.msb >= bits.msb)
        {
            target.push(bits);
        }
    }
    target.len() != before
}

fn propagate_block_args(
    unit: &ExecutionUnit<celox_design::RegionedStateAddr>,
    source_registers: &HashMap<RegisterId, Vec<BitAccess>>,
    target: BlockId,
    args: &[RegisterId],
    registers: &mut HashMap<RegisterId, Vec<BitAccess>>,
) -> bool {
    let Some(block) = unit.blocks.get(&target) else {
        return false;
    };
    let mut changed = false;
    for (parameter, argument) in block.params.iter().zip(args) {
        changed |= extend_register_taint(
            registers,
            *parameter,
            source_registers.get(argument).cloned().unwrap_or_default(),
        );
    }
    changed
}

fn load_taint(
    address: StateAddr,
    offset: &celox_sir::SIROffset,
    width: usize,
    writes: &[StateWrite],
) -> Vec<BitAccess> {
    let Some(load_bits) = offset
        .constant_bit_offset()
        .and_then(|offset| access_from_offset(offset, width))
    else {
        return writes
            .iter()
            .any(|write| write.address == address)
            .then(|| access_from_offset(0, width))
            .flatten()
            .into_iter()
            .collect();
    };
    writes
        .iter()
        .filter(|write| write.address == address)
        .filter_map(|write| {
            write
                .bits
                .and_then(|bits| intersect_access(bits, load_bits))
                .or(write.bits.is_none().then_some(load_bits))
        })
        .filter_map(|bits| shift_access_down(bits, load_bits.lsb))
        .collect()
}

fn store_taint(
    address: StateAddr,
    offset: &celox_sir::SIROffset,
    width: usize,
    source_taint: &[BitAccess],
    whole_write: bool,
) -> Vec<StateWrite> {
    let Some(lsb) = offset.constant_bit_offset() else {
        return (whole_write || !source_taint.is_empty())
            .then_some(StateWrite {
                address,
                bits: None,
            })
            .into_iter()
            .collect();
    };
    if whole_write {
        return vec![StateWrite {
            address,
            bits: access_from_offset(lsb, width),
        }];
    }
    let source_width = access_from_offset(0, width);
    source_taint
        .iter()
        .filter_map(|bits| source_width.and_then(|range| intersect_access(*bits, range)))
        .filter_map(|bits| shift_access(bits, lsb))
        .map(|bits| StateWrite {
            address,
            bits: Some(bits),
        })
        .collect()
}

fn add_state_write(writes: &mut Vec<StateWrite>, write: StateWrite) -> bool {
    if writes.iter().any(|known| {
        known.address == write.address
            && match (known.bits, write.bits) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(known), Some(new)) => known.lsb <= new.lsb && known.msb >= new.msb,
            }
    }) {
        return false;
    }
    if write.bits.is_none() {
        writes.retain(|known| known.address != write.address);
    }
    writes.push(write);
    true
}

fn access_from_offset(offset: usize, width: usize) -> Option<BitAccess> {
    width
        .checked_sub(1)
        .and_then(|tail| offset.checked_add(tail))
        .map(|msb| BitAccess::new(offset, msb))
}

fn intersect_access(lhs: BitAccess, rhs: BitAccess) -> Option<BitAccess> {
    let lsb = lhs.lsb.max(rhs.lsb);
    let msb = lhs.msb.min(rhs.msb);
    (lsb <= msb).then(|| BitAccess::new(lsb, msb))
}

fn shift_access(bits: BitAccess, offset: usize) -> Option<BitAccess> {
    Some(BitAccess::new(
        bits.lsb.checked_add(offset)?,
        bits.msb.checked_add(offset)?,
    ))
}

fn shift_access_down(bits: BitAccess, offset: usize) -> Option<BitAccess> {
    Some(BitAccess::new(
        bits.lsb.checked_sub(offset)?,
        bits.msb.checked_sub(offset)?,
    ))
}

fn check_module(module: &Module, diagnostics: &mut Vec<FrontendDiagnostic>) {
    for declaration in &module.declarations {
        let statements = match declaration {
            Declaration::Comb(declaration) => Some((declaration.statements.as_slice(), false)),
            Declaration::Ff(declaration) => Some((declaration.statements.as_slice(), true)),
            Declaration::Initial(declaration) => Some((declaration.statements.as_slice(), false)),
            Declaration::Final(declaration) => Some((declaration.statements.as_slice(), false)),
            Declaration::Inst(_) | Declaration::Unsupported(_) | Declaration::Null => None,
        };
        if let Some((statements, from_ff)) = statements {
            check_statements(statements, module, diagnostics, from_ff);
        }
    }

    for function in module.functions.values() {
        for body in &function.functions {
            check_statements(&body.statements, module, diagnostics, false);
        }
    }
}

fn check_statements(
    statements: &[Statement],
    module: &Module,
    diagnostics: &mut Vec<FrontendDiagnostic>,
    from_ff: bool,
) {
    for statement in statements {
        match statement {
            Statement::If(statement) => {
                check_statements(&statement.true_side, module, diagnostics, from_ff);
                check_statements(&statement.false_side, module, diagnostics, from_ff);
            }
            Statement::IfReset(statement) => {
                check_statements(&statement.true_side, module, diagnostics, from_ff);
                check_statements(&statement.false_side, module, diagnostics, from_ff);
            }
            Statement::Case(statement) => {
                for arm in &statement.arms {
                    check_statements(&arm.body, module, diagnostics, from_ff);
                }
                check_statements(&statement.default, module, diagnostics, from_ff);
            }
            Statement::For(statement) => {
                check_for(statement, module, diagnostics, from_ff);
                check_statements(&statement.body, module, diagnostics, from_ff);
            }
            Statement::Assign(_)
            | Statement::FunctionCall(_)
            | Statement::SystemFunctionCall(_)
            | Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => {}
        }
    }
}

fn check_for(
    statement: &ForStatement,
    module: &Module,
    diagnostics: &mut Vec<FrontendDiagnostic>,
    from_ff: bool,
) {
    let continuation_bound = match &statement.range {
        ForRange::Forward { end, .. } | ForRange::Stepped { end, .. } => end,
        ForRange::Reverse { start, .. } => start,
    };
    let ForBound::Expression(bound) = continuation_bound else {
        return;
    };

    let mut active_functions = HashSet::default();
    let bound_effects = collect_expression_effects(bound, module, &mut active_functions);
    let mut active_functions = HashSet::default();
    let body_effects =
        collect_statement_effects(&statement.body, module, &mut active_functions, from_ff);

    let body_conflict = bound_effects.reads.iter().any(|read| {
        body_effects
            .writes
            .iter()
            .any(|write| accesses_conflict(read, write))
    });
    let bound_has_visible_effect =
        bound_effects.observable || bound_effects.writes.iter().any(|write| !write.deferred);

    if body_conflict || bound_has_visible_effect {
        diagnostics.push(FrontendDiagnostic::mutable_for_bound(
            &statement.token,
            if bound_has_visible_effect {
                "the continuation bound has an observable effect and would be evaluated a different number of times"
            } else {
                "the loop body may immediately modify state read by the continuation bound"
            },
        ));
        return;
    }

    // Known testbench state transitions are classified after elaboration,
    // where event and state identities can be compared exactly. Defer any
    // accompanying unknown effect as well so an exact conflict wins over a
    // provisional warning.
    if !body_effects.state_changes.is_empty() {
        return;
    }

    let unknown = bound_effects.unknown.or_else(|| {
        (!bound_effects.reads.is_empty())
            .then_some(body_effects.unknown)
            .flatten()
    });
    if let Some(detail) = unknown {
        diagnostics.push(FrontendDiagnostic::unknown_for_bound_effect(
            &statement.token,
            detail,
        ));
    }
}

fn accesses_conflict(read: &Access, write: &Access) -> bool {
    !write.deferred && read.id == write.id && read.bits.overlaps(&write.bits)
}

fn collect_statement_effects(
    statements: &[Statement],
    module: &Module,
    active_functions: &mut HashSet<VarId>,
    from_ff: bool,
) -> Effects {
    let mut effects = Effects::default();
    for statement in statements {
        match statement {
            Statement::Assign(statement) => {
                effects.append(collect_expression_effects(
                    &statement.expr,
                    module,
                    active_functions,
                ));
                for destination in &statement.dst {
                    collect_destination_effects(
                        destination,
                        module,
                        active_functions,
                        &mut effects,
                        from_ff,
                    );
                }
            }
            Statement::If(statement) => {
                effects.append(collect_expression_effects(
                    &statement.cond,
                    module,
                    active_functions,
                ));
                effects.append(collect_statement_effects(
                    &statement.true_side,
                    module,
                    active_functions,
                    from_ff,
                ));
                effects.append(collect_statement_effects(
                    &statement.false_side,
                    module,
                    active_functions,
                    from_ff,
                ));
            }
            Statement::IfReset(statement) => {
                effects.append(collect_statement_effects(
                    &statement.true_side,
                    module,
                    active_functions,
                    from_ff,
                ));
                effects.append(collect_statement_effects(
                    &statement.false_side,
                    module,
                    active_functions,
                    from_ff,
                ));
            }
            Statement::Case(statement) => {
                effects.append(collect_expression_effects(
                    &statement.case_target,
                    module,
                    active_functions,
                ));
                for arm in &statement.arms {
                    for pattern in &arm.patterns {
                        match pattern {
                            CasePattern::Eq(expression) => effects.append(
                                collect_expression_effects(expression, module, active_functions),
                            ),
                            CasePattern::Range { lo, hi, .. } => {
                                effects.append(collect_expression_effects(
                                    lo,
                                    module,
                                    active_functions,
                                ));
                                effects.append(collect_expression_effects(
                                    hi,
                                    module,
                                    active_functions,
                                ));
                            }
                        }
                    }
                    effects.append(collect_statement_effects(
                        &arm.body,
                        module,
                        active_functions,
                        from_ff,
                    ));
                }
                effects.append(collect_statement_effects(
                    &statement.default,
                    module,
                    active_functions,
                    from_ff,
                ));
            }
            Statement::For(statement) => {
                for bound in dynamic_bounds(&statement.range) {
                    effects.append(collect_expression_effects(bound, module, active_functions));
                }
                effects.append(collect_statement_effects(
                    &statement.body,
                    module,
                    active_functions,
                    from_ff,
                ));
            }
            Statement::FunctionCall(call) => {
                collect_call_effects(call, module, active_functions, &mut effects, from_ff);
            }
            Statement::SystemFunctionCall(call) => {
                collect_system_function_effects(call, module, active_functions, &mut effects);
            }
            Statement::TbMethodCall(call) => match &call.method {
                TbMethod::ClockNext { count, period } => {
                    if let Some(count) = count {
                        effects.append(collect_expression_effects(count, module, active_functions));
                    }
                    if let Some(period) = period {
                        effects.append(collect_expression_effects(
                            period,
                            module,
                            active_functions,
                        ));
                    }
                    effects.state_changes.push(StateChange::Clock(call.inst));
                }
                TbMethod::ResetAssert { clock, duration } => {
                    if let Some(duration) = duration {
                        effects.append(collect_expression_effects(
                            duration,
                            module,
                            active_functions,
                        ));
                    }
                    effects.state_changes.push(StateChange::Reset {
                        reset: call.inst,
                        clock: *clock,
                    });
                }
                TbMethod::FileOpen { name, .. } => effects.append(collect_expression_effects(
                    &name.0,
                    module,
                    active_functions,
                )),
                TbMethod::FileWrite { args } => {
                    for argument in args {
                        effects.append(collect_expression_effects(
                            &argument.0,
                            module,
                            active_functions,
                        ));
                    }
                }
                TbMethod::FileClose | TbMethod::FileFlush => {}
            },
            Statement::Unsupported(_) => {
                effects.mark_unknown("unsupported statement has unknown effects")
            }
            Statement::Break | Statement::Null => {}
        }
    }
    effects
}

fn collect_expression_effects(
    expression: &Expression,
    module: &Module,
    active_functions: &mut HashSet<VarId>,
) -> Effects {
    let mut effects = Effects::default();
    match expression {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Variable(id, index, select, _) => {
                collect_index_select_effects(index, select, module, active_functions, &mut effects);
                push_access(module, *id, index, select, false, true, &mut effects);
            }
            Factor::FunctionCall(call) => {
                collect_call_effects(call, module, active_functions, &mut effects, false)
            }
            Factor::SystemFunctionCall(call) => {
                collect_system_function_effects(call, module, active_functions, &mut effects)
            }
            Factor::Unknown(_) => {
                effects.mark_unknown("bound expression contains an unknown value")
            }
            Factor::Value(_) | Factor::Anonymous(_) => {}
        },
        Expression::Unary(_, expression, _) => {
            effects.append(collect_expression_effects(
                expression,
                module,
                active_functions,
            ));
        }
        Expression::Binary(lhs, _, rhs, _) => {
            effects.append(collect_expression_effects(lhs, module, active_functions));
            effects.append(collect_expression_effects(rhs, module, active_functions));
        }
        Expression::Ternary(cond, lhs, rhs, _) => {
            effects.append(collect_expression_effects(cond, module, active_functions));
            effects.append(collect_expression_effects(lhs, module, active_functions));
            effects.append(collect_expression_effects(rhs, module, active_functions));
        }
        Expression::Concatenation(elements, _) => {
            for (expression, repeat) in elements {
                effects.append(collect_expression_effects(
                    expression,
                    module,
                    active_functions,
                ));
                if let Some(repeat) = repeat {
                    effects.append(collect_expression_effects(repeat, module, active_functions));
                }
            }
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, expression) in fields {
                effects.append(collect_expression_effects(
                    expression,
                    module,
                    active_functions,
                ));
            }
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expression, repeat) => {
                        effects.append(collect_expression_effects(
                            expression,
                            module,
                            active_functions,
                        ));
                        if let Some(repeat) = repeat {
                            effects.append(collect_expression_effects(
                                repeat,
                                module,
                                active_functions,
                            ));
                        }
                    }
                    ArrayLiteralItem::Defaul(expression) => effects.append(
                        collect_expression_effects(expression, module, active_functions),
                    ),
                }
            }
        }
    }
    effects
}

fn collect_call_effects(
    call: &FunctionCall,
    module: &Module,
    active_functions: &mut HashSet<VarId>,
    effects: &mut Effects,
    outputs_deferred: bool,
) {
    for input in call.inputs.values() {
        effects.append(collect_expression_effects(input, module, active_functions));
    }
    for destinations in call.outputs.values() {
        for destination in destinations {
            collect_destination_effects(
                destination,
                module,
                active_functions,
                effects,
                outputs_deferred,
            );
        }
    }

    if !active_functions.insert(call.id) {
        effects.mark_unknown(format!(
            "recursive function `{}` has unknown effects",
            call.id
        ));
        return;
    }
    let body = module.functions.get(&call.id).and_then(|function| {
        call.index
            .as_deref()
            .and_then(|index| function.get_function(index))
            .or_else(|| function.get_function(&[]))
    });
    if let Some(body) = body {
        let mut function_effects =
            collect_statement_effects(&body.statements, module, active_functions, false);
        function_effects.discard_function_locals(module);
        effects.append(function_effects);
    } else {
        effects.mark_unknown(format!("function `{}` could not be resolved", call.id));
    }
    active_functions.remove(&call.id);
}

fn collect_system_function_effects(
    call: &SystemFunctionCall,
    module: &Module,
    active_functions: &mut HashSet<VarId>,
    effects: &mut Effects,
) {
    match &call.kind {
        SystemFunctionKind::Bits(input)
        | SystemFunctionKind::Size(input)
        | SystemFunctionKind::Clog2(input)
        | SystemFunctionKind::Onehot(input)
        | SystemFunctionKind::Signed(input)
        | SystemFunctionKind::Unsigned(input) => effects.append(collect_expression_effects(
            &input.0,
            module,
            active_functions,
        )),
        SystemFunctionKind::Readmemh(filename, output) => {
            effects.append(collect_expression_effects(
                &filename.0,
                module,
                active_functions,
            ));
            for destination in &output.0 {
                collect_destination_effects(destination, module, active_functions, effects, false);
            }
        }
        SystemFunctionKind::Display(arguments) | SystemFunctionKind::Write(arguments) => {
            effects.observable = true;
            for argument in arguments {
                effects.append(collect_expression_effects(
                    &argument.0,
                    module,
                    active_functions,
                ));
            }
        }
        SystemFunctionKind::Assert { cond, args, .. } => {
            effects.observable = true;
            effects.append(collect_expression_effects(
                &cond.0,
                module,
                active_functions,
            ));
            for argument in args {
                effects.append(collect_expression_effects(
                    &argument.0,
                    module,
                    active_functions,
                ));
            }
        }
        SystemFunctionKind::Finish => effects.observable = true,
    }
}

fn collect_destination_effects(
    destination: &AssignDestination,
    module: &Module,
    active_functions: &mut HashSet<VarId>,
    effects: &mut Effects,
    from_ff: bool,
) {
    collect_index_select_effects(
        &destination.index,
        &destination.select,
        module,
        active_functions,
        effects,
    );
    let deferred = from_ff
        && module
            .variables
            .get(&destination.id)
            .is_some_and(|variable| {
                variable.kind != veryl_analyzer::ir::VarKind::Let
                    && variable.affiliation != Affiliation::AlwaysFf
            });
    push_access(
        module,
        destination.id,
        &destination.index,
        &destination.select,
        deferred,
        false,
        effects,
    );
}

fn collect_index_select_effects(
    index: &VarIndex,
    select: &VarSelect,
    module: &Module,
    active_functions: &mut HashSet<VarId>,
    effects: &mut Effects,
) {
    for expression in &index.0 {
        effects.append(collect_expression_effects(
            expression,
            module,
            active_functions,
        ));
    }
    for expression in &select.0 {
        effects.append(collect_expression_effects(
            expression,
            module,
            active_functions,
        ));
    }
    if let Some((_, expression)) = &select.1 {
        effects.append(collect_expression_effects(
            expression,
            module,
            active_functions,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn push_access(
    module: &Module,
    id: VarId,
    index: &VarIndex,
    select: &VarSelect,
    deferred: bool,
    read: bool,
    effects: &mut Effects,
) {
    match eval_var_select(module, id, index, select) {
        Ok(bits) => {
            let access = Access { id, bits, deferred };
            if read {
                effects.reads.push(access);
            } else {
                effects.writes.push(access);
            }
        }
        Err(error) => effects.mark_unknown(format!(
            "the access range of `{id}` could not be resolved: {error}"
        )),
    }
}

fn dynamic_bounds(range: &ForRange) -> impl Iterator<Item = &Expression> {
    let (start, end) = match range {
        ForRange::Forward { start, end, .. }
        | ForRange::Reverse { start, end, .. }
        | ForRange::Stepped { start, end, .. } => (start, end),
    };
    [start, end].into_iter().filter_map(|bound| match bound {
        ForBound::Expression(expression) => Some(expression.as_ref()),
        ForBound::Const(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use veryl_analyzer::ir::{
        Comptime, FfTable, Shape, Type, TypeKind, VarKind, VarPath, Variable,
    };
    use veryl_parser::token_range::TokenRange;

    #[test]
    fn unknown_bound_ir_is_reported_as_a_warning() {
        // The pinned Veryl analyzer rejects source constructs that currently
        // lower to Unknown before Celox receives the IR. Build that boundary
        // state directly so the fail-open warning policy remains covered.
        let token = TokenRange::default();
        let statement = Statement::For(ForStatement {
            var_id: VarId::default(),
            var_name: StrId::default(),
            var_type: Type::default(),
            range: ForRange::Forward {
                start: ForBound::Const(0),
                end: ForBound::Expression(Box::new(Expression::Term(Box::new(Factor::Unknown(
                    Comptime::default(),
                ))))),
                inclusive: false,
                step: 1,
            },
            body: Vec::new(),
            token,
        });
        let module = Module {
            name: StrId::default(),
            token,
            ports: HashMap::default(),
            port_types: HashMap::default(),
            variables: HashMap::default(),
            functions: HashMap::default(),
            declarations: vec![Declaration::new_comb(vec![statement])],
            suppress_unassigned: false,
            per_decl_refs: HashMap::default(),
            assign_tokens: HashMap::default(),
            ff_table: FfTable::default(),
        };
        let ir = Ir {
            components: vec![Component::Module(module)],
        };

        let diagnostics = check_dynamic_for_bounds(&ir);
        assert!(matches!(
            diagnostics.as_slice(),
            [FrontendDiagnostic::UnknownForBoundEffect { .. }]
        ));
    }

    #[test]
    fn unknown_body_effect_ir_is_reported_as_a_warning_for_a_mutable_bound() {
        let token = TokenRange::default();
        let bound_id = VarId::from_raw(1);
        let mut bound_type = Type::new(TypeKind::Logic);
        bound_type.set_concrete_width(Shape::new(vec![Some(8)]));
        let bound = Expression::Term(Box::new(Factor::Variable(
            bound_id,
            VarIndex::default(),
            VarSelect::default(),
            Comptime {
                r#type: bound_type.clone(),
                ..Default::default()
            },
        )));
        let statement = Statement::For(ForStatement {
            var_id: VarId::default(),
            var_name: StrId::default(),
            var_type: Type::default(),
            range: ForRange::Forward {
                start: ForBound::Const(0),
                end: ForBound::Expression(Box::new(bound)),
                inclusive: false,
                step: 1,
            },
            body: vec![Statement::Unsupported(token)],
            token,
        });
        let variable = Variable {
            id: bound_id,
            path: VarPath::default(),
            kind: VarKind::Variable,
            r#type: bound_type,
            value: Vec::new(),
            assigned: Vec::new(),
            affiliation: Affiliation::Module,
            token,
        };
        let module = Module {
            name: StrId::default(),
            token,
            ports: HashMap::default(),
            port_types: HashMap::default(),
            variables: [(bound_id, variable)].into_iter().collect(),
            functions: HashMap::default(),
            declarations: vec![Declaration::new_comb(vec![statement])],
            suppress_unassigned: false,
            per_decl_refs: HashMap::default(),
            assign_tokens: HashMap::default(),
            ff_table: FfTable::default(),
        };
        let ir = Ir {
            components: vec![Component::Module(module)],
        };

        let diagnostics = check_dynamic_for_bounds(&ir);
        assert!(matches!(
            diagnostics.as_slice(),
            [FrontendDiagnostic::UnknownForBoundEffect { .. }]
        ));
    }
}
