//! Tier-0 SIR interpreter for tiered execution.
//!
//! When [`crate::ExecutionStrategy::Tiered`] is selected the simulator starts
//! executing execution units on this interpreter immediately after layout,
//! hiding compilation latency behind the first simulated steps. Hot units are
//! compiled in the background and swapped in at scheduler safe points; this
//! interpreter remains the permanent fallback for units that are never
//! compiled (cold paths) or whose compilation failed.
//!
//! Correctness contract: the interpreter and the compiled backends must agree
//! bit-exactly, including four-state X/Z propagation, because a unit may
//! migrate between tiers mid-simulation. Rules marked `SEMANTICS-CHECK` below
//! follow standard HDL semantics and must be reconciled against the backend
//! truth-table implementations.
//!
//! Memory accesses and simulation side effects (triggers, comb captures,
//! runtime events) are delegated to the [`InterpMachine`] trait so the
//! interpreter stays independent of backend storage details.

use std::fmt;

use celox_sir::{
    BinaryOp, BlockId, ExecutionUnit, RegisterId, RegisterType, SIRInstruction, SIROffset,
    SIRTerminator, SIRValue, TriggerIdWithKind, UnaryOp,
};
use num_bigint::{BigInt, BigUint};
use num_traits::{Signed, ToPrimitive, Zero};

use crate::HashMap;

/// Why an interpreted execution unit stopped abnormally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterpError {
    /// Terminator targeted a block that does not exist in the unit.
    UnknownBlock(BlockId),
    /// An instruction read a register that was never written.
    MissingRegister(RegisterId),
    /// A jump supplied a different number of arguments than the target
    /// block declares parameters.
    RegisterArityMismatch { expected: usize, found: usize },
    /// The instruction uses an operator the interpreter does not implement
    /// yet. This fails loudly instead of producing wrong simulation results.
    UnsupportedOperation(String),
    /// The unit terminated with `SIRTerminator::Error`.
    Fatal(i64),
    /// The [`InterpMachine`] reported a failure.
    Machine(String),
}

impl fmt::Display for InterpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InterpError::UnknownBlock(block) => {
                write!(f, "interpreted jump to unknown block b{}", block.0)
            }
            InterpError::MissingRegister(register) => {
                write!(f, "read of unwritten register r{}", register.0)
            }
            InterpError::RegisterArityMismatch { expected, found } => {
                write!(
                    f,
                    "jump argument count mismatch: target expects {expected}, jump supplies {found}"
                )
            }
            InterpError::UnsupportedOperation(description) => {
                write!(f, "interpreter does not support operation: {description}")
            }
            InterpError::Fatal(code) => write!(f, "simulation fatal error ({code})"),
            InterpError::Machine(message) => write!(f, "machine error: {message}"),
        }
    }
}

impl std::error::Error for InterpError {}

/// One memory access with its dynamic offset components resolved.
///
/// The unresolved [`SIROffset`] is preserved alongside the values of its
/// dynamic registers because element-strided layouts distinguish `Element`
/// accesses from plain bit offsets when computing physical addresses.
#[derive(Clone, Copy, Debug)]
pub struct ResolvedAccess<'a> {
    pub offset: &'a SIROffset,
    /// Values of the registers returned by [`SIROffset::dynamic_registers`],
    /// in the same order.
    pub dynamics: [Option<&'a SIRValue>; 2],
}

/// Pre-store snapshot of a stored range, used to detect whether a store
/// actually changed any covered bit before enabling comb capture observers.
#[derive(Clone, Debug, Default)]
pub struct StoreSnapshot {
    pub value_words: Vec<u64>,
    pub mask_words: Vec<u64>,
}

/// Storage and side-effect interface driven by the interpreter.
///
/// The production implementation wraps the simulator's live memory image and
/// event plumbing; tests use an in-memory fake. The address type `A` matches
/// the executed SIR (for example `RegionedAbsoluteAddr`).
pub trait InterpMachine<A> {
    fn load(
        &mut self,
        addr: &A,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<SIRValue, InterpError>;

    fn store(
        &mut self,
        addr: &A,
        access: ResolvedAccess<'_>,
        bits: usize,
        value: &SIRValue,
    ) -> Result<(), InterpError>;

    /// `Commit`: copy `bits` at `access` from `src` to `dst`.
    fn commit(
        &mut self,
        src: &A,
        dst: &A,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<(), InterpError>;

    /// Edge/level triggers attached to a `Store` or `Commit`.
    fn notify_triggers(&mut self, addr: &A, triggers: &[TriggerIdWithKind]);

    /// Snapshot the stored range before a `Store` with comb capture sites so
    /// the post-store comparison can detect actual changes.
    fn capture_store_range(
        &mut self,
        addr: &A,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<StoreSnapshot, InterpError>;

    /// Enable comb capture sites when the stored range differs from the
    /// pre-store snapshot.
    fn enable_comb_captures(
        &mut self,
        addr: &A,
        access: ResolvedAccess<'_>,
        bits: usize,
        before: &StoreSnapshot,
        sites: &[u32],
    ) -> Result<(), InterpError>;

    fn emit_runtime_event(&mut self, site_id: u32, args: &[SIRValue]) -> Result<(), InterpError>;

    /// Emit a comb capture event. Returns `Err(InterpError::Fatal)` when the
    /// event carries a fatal code and its site was enabled, mirroring the
    /// compiled unit's early return.
    fn emit_comb_capture_event(
        &mut self,
        site_id: u32,
        args: &[SIRValue],
        fatal_error_code: Option<i64>,
        consume_enabled: bool,
    ) -> Result<(), InterpError>;

    fn enable_comb_capture_if_changed(
        &mut self,
        old: &SIRValue,
        new: &SIRValue,
        sites: &[u32],
    ) -> Result<(), InterpError>;
}

/// Outcome of one interpreted unit invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitExit {
    /// The unit reached `SIRTerminator::Return`.
    Return,
}

/// Execute one execution unit on the interpreter.
///
/// `entry_args` binds the entry block's parameters (event arguments). The
/// simulator drives repeated invocations (comb settle fixpoint, clocked
/// events); this function performs exactly one entry-to-return traversal.
///
/// `four_state` selects the value model: in two-state mode immediate masks
/// are discarded exactly like the compiled backends' translation does, so
/// unknown-bit payloads never leak into stored values.
pub fn execute_unit<A, M: InterpMachine<A>>(
    unit: &ExecutionUnit<A>,
    machine: &mut M,
    entry_args: &[SIRValue],
    four_state: bool,
) -> Result<UnitExit, InterpError> {
    let mut regs = Registers::new(&unit.register_map);
    let entry = unit
        .blocks
        .get(&unit.entry_block_id)
        .ok_or(InterpError::UnknownBlock(unit.entry_block_id))?;
    if entry.params.len() != entry_args.len() {
        return Err(InterpError::RegisterArityMismatch {
            expected: entry.params.len(),
            found: entry_args.len(),
        });
    }
    for (param, value) in entry.params.iter().zip(entry_args) {
        regs.set(*param, value.clone());
    }

    let mut current = unit.entry_block_id;
    loop {
        let block = unit
            .blocks
            .get(&current)
            .ok_or(InterpError::UnknownBlock(current))?;
        for instruction in &block.instructions {
            exec_instruction(instruction, &mut regs, machine, four_state)?;
        }
        match &block.terminator {
            SIRTerminator::Return => return Ok(UnitExit::Return),
            SIRTerminator::Error(code) => return Err(InterpError::Fatal(*code)),
            SIRTerminator::Jump(target, args) => {
                transfer(&mut regs, unit, *target, args)?;
                current = *target;
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                let cond_width = regs.width(*cond);
                let cond = regs.get(*cond)?.clone();
                // SEMANTICS-CHECK: a known one selects the true branch; an
                // all-unknown or all-zero condition selects the false branch.
                let target = if branch_condition_holds(&cond, cond_width) {
                    true_block
                } else {
                    false_block
                };
                transfer(&mut regs, unit, target.0, &target.1)?;
                current = target.0;
            }
            SIRTerminator::Switch {
                selector,
                cases,
                default,
            } => {
                // SEMANTICS-CHECK: masked (X/Z) selector bits participate in
                // the comparison as their payload value; unmatched selectors
                // take the default target.
                let selector = regs.get(*selector)?.payload.clone();
                let target = cases
                    .iter()
                    .find(|case| case.value == selector)
                    .map(|case| case.target)
                    .unwrap_or(*default);
                transfer(&mut regs, unit, target, &[])?;
                current = target;
            }
        }
    }
}

/// Bind jump arguments to the target block's parameters.
fn transfer<A>(
    regs: &mut Registers,
    unit: &ExecutionUnit<A>,
    target: BlockId,
    args: &[RegisterId],
) -> Result<(), InterpError> {
    let params = &unit
        .blocks
        .get(&target)
        .ok_or(InterpError::UnknownBlock(target))?
        .params;
    if params.len() != args.len() {
        return Err(InterpError::RegisterArityMismatch {
            expected: params.len(),
            found: args.len(),
        });
    }
    let mut values = Vec::with_capacity(args.len());
    for arg in args {
        values.push(regs.get(*arg)?.clone());
    }
    for (param, value) in params.iter().zip(values) {
        regs.set(*param, value);
    }
    Ok(())
}

fn exec_instruction<A, M: InterpMachine<A>>(
    instruction: &SIRInstruction<A>,
    regs: &mut Registers,
    machine: &mut M,
    four_state: bool,
) -> Result<(), InterpError> {
    match instruction {
        SIRInstruction::Imm(dst, value) => {
            // Two-state translation drops unknown-bit masks entirely; keep
            // only the payload so X constants behave as their payload bits.
            let value = if four_state {
                value.clone()
            } else {
                SIRValue::new(value.payload.clone())
            };
            regs.set(*dst, value);
        }
        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            let lhs_value = regs.get(*lhs)?.clone();
            let rhs_value = regs.get(*rhs)?.clone();
            let dst_width = regs.width(*dst);
            let lhs_signed = regs.is_signed(*lhs);
            let out = alu_binary(
                op,
                &lhs_value,
                &rhs_value,
                regs.width(*lhs),
                regs.width(*rhs),
                dst_width,
                lhs_signed,
            )?;
            regs.set(*dst, truncate(out, dst_width));
        }
        SIRInstruction::Unary(dst, op, src) => {
            let src_value = regs.get(*src)?.clone();
            let out = alu_unary(
                op,
                &src_value,
                regs.width(*src),
                regs.is_signed(*src),
                regs.width(*dst),
            )?;
            regs.set(*dst, truncate(out, regs.width(*dst)));
        }
        SIRInstruction::Load(dst, addr, offset, bits) => {
            let access = resolve_access(offset, regs)?;
            let value = machine.load(addr, access, *bits)?;
            regs.set(*dst, value);
        }
        SIRInstruction::Store(addr, offset, bits, src, triggers, sites) => {
            let value = regs.get(*src)?.clone();
            let access = resolve_access(offset, regs)?;
            let before = if sites.is_empty() {
                None
            } else {
                Some(machine.capture_store_range(addr, access, *bits)?)
            };
            machine.store(addr, access, *bits, &value)?;
            if !triggers.is_empty() {
                machine.notify_triggers(addr, triggers);
            }
            if let Some(before) = before {
                let access = resolve_access(offset, regs)?;
                machine.enable_comb_captures(addr, access, *bits, &before, sites)?;
            }
        }
        SIRInstruction::Commit(src, dst, offset, bits, triggers) => {
            let access = resolve_access(offset, regs)?;
            machine.commit(src, dst, access, *bits)?;
            if !triggers.is_empty() {
                machine.notify_triggers(dst, triggers);
            }
        }
        SIRInstruction::Concat(dst, sources) => {
            let mut payload = BigUint::zero();
            let mut mask = BigUint::zero();
            for source in sources {
                let value = regs.get(*source)?;
                let width = regs.width(*source);
                payload = (payload << width) | &value.payload;
                mask = (mask << width) | &value.mask;
            }
            // Keep the declared-register-width invariant shared by every other
            // operation: results never exceed the destination width.
            regs.set(*dst, truncate(SIRValue { payload, mask }, regs.width(*dst)));
        }
        SIRInstruction::Slice(dst, src, offset, width) => {
            let value = regs.get(*src)?;
            let payload = extract_bits(&value.payload, *offset, *width);
            let mask = extract_bits(&value.mask, *offset, *width);
            regs.set(*dst, SIRValue { payload, mask });
        }
        SIRInstruction::Mux(dst, cond, then_value, else_value) => {
            let cond_width = regs.width(*cond);
            let cond = regs.get(*cond)?.clone();
            let then_value = regs.get(*then_value)?.clone();
            let else_value = regs.get(*else_value)?.clone();
            // Evaluate the condition in its own width; shape the selected
            // arms to the destination width.
            let out = eval_mux(
                &cond,
                &then_value,
                &else_value,
                cond_width,
                regs.width(*dst),
            );
            regs.set(*dst, out);
        }
        SIRInstruction::RuntimeEvent { site_id, args } => {
            let values = resolve_args(args, regs)?;
            machine.emit_runtime_event(*site_id, &values)?;
        }
        SIRInstruction::CombCaptureEvent {
            site_id,
            args,
            fatal_error_code,
            consume_enabled,
        } => {
            let values = resolve_args(args, regs)?;
            machine.emit_comb_capture_event(
                *site_id,
                &values,
                *fatal_error_code,
                *consume_enabled,
            )?;
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, sites } => {
            let old = regs.get(*old)?.clone();
            let new = regs.get(*new)?.clone();
            machine.enable_comb_capture_if_changed(&old, &new, sites)?;
        }
    }
    Ok(())
}

fn resolve_access<'a>(
    offset: &'a SIROffset,
    regs: &'a Registers,
) -> Result<ResolvedAccess<'a>, InterpError> {
    let mut dynamics = [None, None];
    for (slot, register) in offset.dynamic_registers().into_iter().enumerate() {
        if let Some(register) = register {
            dynamics[slot] = Some(regs.get(register)?);
        }
    }
    Ok(ResolvedAccess { offset, dynamics })
}

fn resolve_args(args: &[RegisterId], regs: &Registers) -> Result<Vec<SIRValue>, InterpError> {
    args.iter().map(|arg| regs.get(*arg).cloned()).collect()
}

// ── Value helpers ─────────────────────────────────────────────────────

fn width_mask(width: usize) -> BigUint {
    if width == 0 {
        BigUint::zero()
    } else {
        (BigUint::from(1u8) << width) - 1u8
    }
}

fn truncate(mut value: SIRValue, width: usize) -> SIRValue {
    let mask = width_mask(width);
    value.payload &= &mask;
    value.mask &= mask;
    value
}

/// An all-unknown value following the compiled backends' normalized
/// convention: unknown bits carry payload one.
fn all_x(width: usize) -> SIRValue {
    SIRValue {
        payload: width_mask(width),
        mask: width_mask(width),
    }
}

fn extract_bits(value: &BigUint, offset: usize, width: usize) -> BigUint {
    if width == 0 {
        return BigUint::zero();
    }
    (value >> offset) & width_mask(width)
}

/// Bits that are unmasked and set to one.
fn known_ones(value: &SIRValue, width: usize) -> BigUint {
    &value.payload & (&width_mask(width) ^ &value.mask)
}

/// Bits that are unmasked and set to zero.
fn known_zeros(value: &SIRValue, width: usize) -> BigUint {
    (&width_mask(width) ^ &value.payload) & (&width_mask(width) ^ &value.mask)
}

fn branch_condition_holds(cond: &SIRValue, width: usize) -> bool {
    !known_ones(cond, width).is_zero()
}

// ── ALU ───────────────────────────────────────────────────────────────

/// Normalize a value into the compiled backends' register convention:
/// unknown bits carry payload one (`payload |= mask`), so a value migrated
/// between the interpreter and generated code is bit-identical in memory.
fn normalize(value: SIRValue) -> SIRValue {
    SIRValue {
        payload: &value.payload | &value.mask,
        mask: value.mask,
    }
}

fn alu_binary(
    op: &BinaryOp,
    lhs: &SIRValue,
    rhs: &SIRValue,
    lhs_width: usize,
    rhs_width: usize,
    dst_width: usize,
    lhs_signed: bool,
) -> Result<SIRValue, InterpError> {
    let out = match op {
        BinaryOp::Add => {
            // SEMANTICS-CHECK: any X/Z operand bit makes the whole result X.
            if lhs.mask.is_zero() && rhs.mask.is_zero() {
                SIRValue::new((&lhs.payload + &rhs.payload) & width_mask(dst_width))
            } else {
                all_x(dst_width)
            }
        }
        BinaryOp::Sub => {
            // Wrap-safe two's complement subtraction: l + (~r + 1) mod 2^w.
            // SEMANTICS-CHECK: any X/Z operand bit makes the whole result X.
            if lhs.mask.is_zero() && rhs.mask.is_zero() {
                let inverted_rhs = &width_mask(dst_width) ^ &rhs.payload;
                SIRValue::new((&lhs.payload + inverted_rhs + 1u8) & width_mask(dst_width))
            } else {
                all_x(dst_width)
            }
        }
        BinaryOp::Mul => {
            // SEMANTICS-CHECK: any X/Z operand bit makes the whole result X.
            if lhs.mask.is_zero() && rhs.mask.is_zero() {
                SIRValue::new((&lhs.payload * &rhs.payload) & width_mask(dst_width))
            } else {
                all_x(dst_width)
            }
        }
        BinaryOp::DivU | BinaryOp::RemU => {
            // SEMANTICS-CHECK: any X/Z operand bit makes the whole result X.
            // Division by zero yields zero, matching the compiled contract.
            if !lhs.mask.is_zero() || !rhs.mask.is_zero() {
                all_x(dst_width)
            } else if rhs.payload.is_zero() {
                SIRValue::new(BigUint::zero())
            } else if *op == BinaryOp::DivU {
                SIRValue::new(&lhs.payload / &rhs.payload)
            } else {
                SIRValue::new(&lhs.payload % &rhs.payload)
            }
        }
        BinaryOp::DivS | BinaryOp::RemS => {
            // SEMANTICS-CHECK: any X/Z operand bit makes the whole result X.
            // Signed division truncates toward zero; division by zero yields
            // zero, and MIN / -1 wraps to MIN (rem: 0), matching the compiled
            // overflow guards.
            if !lhs.mask.is_zero() || !rhs.mask.is_zero() {
                all_x(dst_width)
            } else {
                let dividend = to_signed(&lhs.payload, lhs_width);
                let divisor = to_signed(&rhs.payload, rhs_width);
                if divisor.is_zero() {
                    SIRValue::new(BigUint::zero())
                } else {
                    let raw = if *op == BinaryOp::DivS {
                        &dividend / &divisor
                    } else {
                        &dividend % &divisor
                    };
                    SIRValue::new(wrap_signed(&raw, dst_width))
                }
            }
        }
        BinaryOp::And => {
            // Per-bit truth tables evaluated at the common width so padding
            // above a narrower operand counts as known zero.
            let common = lhs_width.max(rhs_width).max(dst_width);
            let ones = known_ones(lhs, common) & known_ones(rhs, common);
            let zeros = known_zeros(lhs, common) | known_zeros(rhs, common);
            let mask = &width_mask(common) ^ (&ones | &zeros);
            SIRValue {
                payload: ones,
                mask,
            }
        }
        BinaryOp::Or => {
            let common = lhs_width.max(rhs_width).max(dst_width);
            let ones = known_ones(lhs, common) | known_ones(rhs, common);
            let zeros = known_zeros(lhs, common) & known_zeros(rhs, common);
            let mask = &width_mask(common) ^ (&ones | &zeros);
            SIRValue {
                payload: ones,
                mask,
            }
        }
        BinaryOp::Xor => {
            // SEMANTICS-CHECK: XOR is per-bit independent — an unknown input
            // bit only makes the corresponding output bit unknown (mask union),
            // unlike arithmetic ops where any X poisons the whole result.
            let common = lhs_width.max(rhs_width);
            SIRValue {
                payload: (&lhs.payload ^ &rhs.payload) & width_mask(common),
                mask: (&lhs.mask | &rhs.mask) & width_mask(common),
            }
        }
        BinaryOp::Shl => {
            // SEMANTICS-CHECK: an X/Z shift amount makes the whole result X.
            // The shift runs on the full left-hand value and the destination
            // keeps the low bits, matching the compiled common-width lowering
            // (a narrow destination still receives surviving low bits).
            if !rhs.mask.is_zero() {
                all_x(dst_width)
            } else {
                match shift_amount(&rhs.payload) {
                    Some(amount) => SIRValue {
                        payload: (&lhs.payload << amount) & width_mask(dst_width),
                        mask: (&lhs.mask << amount) & width_mask(dst_width),
                    },
                    None => SIRValue::new(BigUint::zero()),
                }
            }
        }
        BinaryOp::Shr => {
            // SEMANTICS-CHECK: an X/Z shift amount makes the whole result X.
            // Logical shift: vacated bits are zero regardless of width.
            if !rhs.mask.is_zero() {
                all_x(dst_width)
            } else {
                match shift_amount(&rhs.payload) {
                    Some(amount) => SIRValue {
                        payload: &lhs.payload >> amount,
                        mask: &lhs.mask >> amount,
                    },
                    None => SIRValue::new(BigUint::zero()),
                }
            }
        }
        BinaryOp::Sar => {
            // SEMANTICS-CHECK: an X/Z shift amount makes the whole result X.
            // The frontend emits `Sar` only for genuinely signed sources, and
            // the compiled backends sign-fill unconditionally for this opcode,
            // so the source value and its unknown-bit mask are interpreted as
            // two's complement at their declared width before shifting.
            if !rhs.mask.is_zero() {
                all_x(dst_width)
            } else {
                let signed_value = to_signed(&lhs.payload, lhs_width);
                let signed_mask = to_signed(&lhs.mask, lhs_width);
                match shift_amount(&rhs.payload) {
                    Some(amount) => {
                        let shifted = signed_value >> amount;
                        let shifted_mask = signed_mask >> amount;
                        SIRValue {
                            payload: wrap_signed(&shifted, dst_width),
                            mask: wrap_signed(&shifted_mask, dst_width),
                        }
                    }
                    None => {
                        // An unrepresentable shift distance drives every bit,
                        // including the mask, toward the sign fill.
                        if signed_value.is_negative() {
                            all_x(dst_width)
                        } else {
                            SIRValue::new(BigUint::zero())
                        }
                    }
                }
            }
        }
        BinaryOp::Eq
        | BinaryOp::Ne
        | BinaryOp::LtU
        | BinaryOp::LtS
        | BinaryOp::LeU
        | BinaryOp::LeS
        | BinaryOp::GtU
        | BinaryOp::GtS
        | BinaryOp::GeU
        | BinaryOp::GeS => {
            // SEMANTICS-CHECK: comparisons yield X when either operand holds
            // an unknown bit; otherwise the comparison runs on the operands'
            // declared values (signed ops interpret two's complement). `Eq`
            // and `Ne` sign-extend a signed left-hand operand and zero-extend
            // the right-hand one, mirroring the compiled promotion rules.
            if !lhs.mask.is_zero() || !rhs.mask.is_zero() {
                all_x(dst_width)
            } else {
                let holds = compare_holds(op, lhs, rhs, lhs_width, rhs_width, lhs_signed);
                SIRValue::new(u8::from(holds))
            }
        }
        BinaryOp::LogicAnd => {
            // IEEE 1800: 0 dominates `&&` — a definitely-false operand forces
            // a definite zero; any remaining X yields X.
            if logic_operand_definitely_false(lhs, lhs_width)
                || logic_operand_definitely_false(rhs, rhs_width)
            {
                SIRValue::new(BigUint::zero())
            } else if !lhs.mask.is_zero() || !rhs.mask.is_zero() {
                all_x(dst_width)
            } else {
                let truth = logic_truth(lhs, lhs_width) && logic_truth(rhs, rhs_width);
                SIRValue::new(u8::from(truth))
            }
        }
        BinaryOp::LogicOr => {
            // IEEE 1800: 1 dominates `||` — a definitely-true operand forces
            // a definite one; any remaining X yields X.
            if logic_operand_definitely_true(lhs, lhs_width)
                || logic_operand_definitely_true(rhs, rhs_width)
            {
                SIRValue::new(1u8)
            } else if !lhs.mask.is_zero() || !rhs.mask.is_zero() {
                all_x(dst_width)
            } else {
                let truth = logic_truth(lhs, lhs_width) || logic_truth(rhs, rhs_width);
                SIRValue::new(u8::from(truth))
            }
        }
        BinaryOp::EqCase | BinaryOp::NeCase => {
            // Case equality treats X/Z as ordinary values: payload and mask
            // must agree at every compared bit for a match. The result is
            // always definite.
            let common = lhs_width.max(rhs_width).max(dst_width);
            let lhs_ext = zero_extend(lhs, common);
            let rhs_ext = zero_extend(rhs, common);
            let diff = (&lhs_ext.payload ^ &rhs_ext.payload) | (&lhs_ext.mask ^ &rhs_ext.mask);
            let matched = diff.is_zero();
            let holds = if *op == BinaryOp::EqCase {
                matched
            } else {
                !matched
            };
            SIRValue::new(u8::from(holds))
        }
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            // IEEE 1800 ==?/!=?: X/Z bits on the right-hand side are
            // wildcards. A definite mismatch yields a definite result, an
            // unknown left-hand bit at a compared position yields X, and
            // otherwise the payloads compare at definitely-known positions.
            let common = lhs_width.max(rhs_width);
            let compare_mask = &width_mask(common) ^ &rhs.mask;
            let definite_compare = &compare_mask & (&width_mask(common) ^ &lhs.mask);
            let mismatch_bits = (&lhs.payload ^ &rhs.payload) & &definite_compare;
            let x_at_compared = &lhs.mask & &compare_mask;
            let mask = if !mismatch_bits.is_zero() {
                BigUint::zero()
            } else if !x_at_compared.is_zero() {
                BigUint::from(1u8)
            } else {
                BigUint::zero()
            };
            let lhs_eff = &lhs.payload & &definite_compare;
            let rhs_eff = &rhs.payload & &definite_compare;
            let equal = lhs_eff == rhs_eff;
            let holds = if *op == BinaryOp::EqWildcard {
                equal
            } else {
                !equal
            };
            SIRValue {
                payload: BigUint::from(u8::from(holds)),
                mask,
            }
        }
    };
    Ok(normalize(truncate(out, dst_width)))
}

/// Interpret a shift amount register value as `usize`, or `None` when it
/// exceeds any meaningful shift distance.
fn shift_amount(value: &BigUint) -> Option<usize> {
    value.to_usize()
}

/// Two's complement interpretation of `width`-truncated `value`.
fn to_signed(value: &BigUint, width: usize) -> BigInt {
    if width == 0 {
        return BigInt::from(0);
    }
    let masked = value & width_mask(width);
    if masked.bit((width - 1) as u64) {
        BigInt::from(masked) - (BigInt::from(1) << width)
    } else {
        BigInt::from(masked)
    }
}

/// Reduce a signed integer modulo `2^width` into its two's complement bits.
fn wrap_signed(value: &BigInt, width: usize) -> BigUint {
    if width == 0 {
        return BigUint::zero();
    }
    let modulus = BigInt::from(1) << width;
    ((value % &modulus + &modulus) % modulus)
        .to_biguint()
        .expect("non-negative remainder")
}

/// Evaluate an ordering/equality operator on fully-known operands.
///
/// `lhs_signed` carries the left-hand register's declaration so `Eq`/`Ne`
/// replicate the compiled promotion: a signed lhs is sign-extended to the
/// common width while the rhs is always zero-extended.
fn compare_holds(
    op: &BinaryOp,
    lhs: &SIRValue,
    rhs: &SIRValue,
    lhs_width: usize,
    rhs_width: usize,
    lhs_signed: bool,
) -> bool {
    let ordering = match op {
        BinaryOp::Eq | BinaryOp::Ne => {
            let common = lhs_width.max(rhs_width);
            let lhs_ext = if lhs_signed {
                sign_extend(&lhs.payload, lhs_width, common)
            } else {
                lhs.payload.clone()
            };
            let rhs_ext = zero_extend(rhs, common).payload;
            lhs_ext.cmp(&rhs_ext)
        }
        BinaryOp::LtU | BinaryOp::LeU | BinaryOp::GtU | BinaryOp::GeU => {
            lhs.payload.cmp(&rhs.payload)
        }
        _ => to_signed(&lhs.payload, lhs_width).cmp(&to_signed(&rhs.payload, rhs_width)),
    };
    use std::cmp::Ordering;
    match op {
        BinaryOp::Eq | BinaryOp::EqCase => ordering == Ordering::Equal,
        BinaryOp::Ne | BinaryOp::NeCase => ordering != Ordering::Equal,
        BinaryOp::LtU | BinaryOp::LtS => ordering == Ordering::Less,
        BinaryOp::LeU | BinaryOp::LeS => ordering != Ordering::Greater,
        BinaryOp::GtU | BinaryOp::GtS => ordering == Ordering::Greater,
        BinaryOp::GeU | BinaryOp::GeS => ordering != Ordering::Less,
        _ => false,
    }
}

/// Zero-extend a value's payload/mask view up to `width` (padding reads as
/// known zero).
fn zero_extend(value: &SIRValue, width: usize) -> SIRValue {
    SIRValue {
        payload: &value.payload & width_mask(width),
        mask: &value.mask & width_mask(width),
    }
}

/// Sign-extend the top bit of a `from_width`-wide payload up to `to_width`.
fn sign_extend(payload: &BigUint, from_width: usize, to_width: usize) -> BigUint {
    if from_width == 0 || to_width <= from_width {
        return payload & width_mask(to_width);
    }
    if payload.bit((from_width - 1) as u64) {
        payload | (&width_mask(to_width) ^ &width_mask(from_width))
    } else {
        payload.clone()
    }
}

/// An operand of a logical operator is definitely false when every bit,
/// including unknown ones, is zero.
fn logic_operand_definitely_false(value: &SIRValue, width: usize) -> bool {
    (&value.payload | &value.mask) & width_mask(width) == BigUint::zero()
}

/// An operand of a logical operator is definitely true when some bit is a
/// known one.
fn logic_operand_definitely_true(value: &SIRValue, width: usize) -> bool {
    !known_ones(value, width).is_zero()
}

/// Boolean truth of a fully-known operand.
fn logic_truth(value: &SIRValue, width: usize) -> bool {
    !known_ones(value, width).is_zero()
}

fn alu_unary(
    op: &UnaryOp,
    src: &SIRValue,
    src_width: usize,
    src_signed: bool,
    dst_width: usize,
) -> Result<SIRValue, InterpError> {
    // Signedness is carried by dedicated opcodes (e.g. division); unary
    // operators do not consult the declaration today.
    let _ = src_signed;
    let out = match op {
        UnaryOp::Ident => src.clone(),
        UnaryOp::ToTwoState => SIRValue {
            payload: &src.payload & (&width_mask(src_width) ^ &src.mask),
            mask: BigUint::zero(),
        },
        UnaryOp::Minus => {
            // SEMANTICS-CHECK: negating a value containing X/Z yields all-X.
            if src.mask.is_zero() {
                let inverted = &width_mask(src_width) ^ &src.payload;
                SIRValue::new((inverted + 1u8) & width_mask(src_width))
            } else {
                all_x(src_width)
            }
        }
        UnaryOp::BitNot => SIRValue {
            payload: &width_mask(src_width) ^ &src.payload,
            mask: src.mask.clone(),
        },
        UnaryOp::LogicNot => {
            if !known_ones(src, src_width).is_zero() {
                SIRValue::new(BigUint::zero())
            } else if !src.mask.is_zero() {
                all_x(1)
            } else {
                SIRValue::new(1u8)
            }
        }
        UnaryOp::Or => {
            // Reduction-or over known bits.
            if !known_ones(src, src_width).is_zero() {
                SIRValue::new(1u8)
            } else if !src.mask.is_zero() {
                all_x(1)
            } else {
                SIRValue::new(BigUint::zero())
            }
        }
        UnaryOp::Xor => {
            // Reduction-xor; any X/Z input bit yields X.
            // SEMANTICS-CHECK: masked bits are treated as X inputs.
            if !src.mask.is_zero() {
                all_x(1)
            } else {
                SIRValue::new(u8::from(parity(&src.payload)))
            }
        }
        UnaryOp::And => {
            // Reduction-and over the payload bits; a definite zero bit makes
            // the result a definite zero, otherwise any X/Z input bit yields
            // X (IEEE 1800 dominant-value rule).
            let width = width_mask(src_width);
            let has_definite_zero = !(&width ^ &src.payload ^ &src.mask).is_zero()
                && !known_zeros(src, src_width).is_zero();
            let mask = if has_definite_zero {
                BigUint::zero()
            } else if !src.mask.is_zero() {
                BigUint::from(1u8)
            } else {
                BigUint::zero()
            };
            let all_ones = src.payload == width;
            SIRValue {
                payload: BigUint::from(u8::from(all_ones)),
                mask,
            }
        }
        UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
            // SEMANTICS-CHECK: any X/Z input bit makes the whole result X,
            // matching the compiled bit-count lowering; otherwise the count
            // runs on the payload bits.
            if !src.mask.is_zero() {
                all_x(dst_width)
            } else {
                match op {
                    UnaryOp::PopCount => SIRValue::new(src.payload.popcount()),
                    UnaryOp::CountLeadingZeros => {
                        let significant = src.payload.bits() as usize;
                        SIRValue::new(src_width.saturating_sub(significant) as u64)
                    }
                    _ => SIRValue::new(trailing_zeros(&src.payload, src_width) as u64),
                }
            }
        }
    };
    let truncated = truncate(out, dst_width);
    // Identity and two-state casts preserve the incoming bit pattern; every
    // other unary operation adopts the normalized unknown-bit convention.
    if matches!(op, UnaryOp::Ident | UnaryOp::ToTwoState) {
        Ok(truncated)
    } else {
        Ok(normalize(truncated))
    }
}

/// `Mux` following the SIR contract: a known one in the condition selects
/// `then` exactly (preserving its mask); a fully known zero selects `else`;
/// an unknown condition preserves bits where both arms agree and turns
/// differing bits into X.
///
/// The condition is evaluated in `cond_width` (its own register width) while
/// the agreement merge is shaped to `out_width` (the destination width).
fn eval_mux(
    cond: &SIRValue,
    then_value: &SIRValue,
    else_value: &SIRValue,
    cond_width: usize,
    out_width: usize,
) -> SIRValue {
    if branch_condition_holds(cond, cond_width) {
        return then_value.clone();
    }
    if cond.mask.is_zero() {
        return else_value.clone();
    }
    let mask = width_mask(out_width);
    let tv = &then_value.payload & &mask;
    let ev = &else_value.payload & &mask;
    let tm = &then_value.mask & &mask;
    let em = &else_value.mask & &mask;
    let difference = (&tv ^ &ev) | (&tm ^ &em);
    // Differing bits become X; agreeing bits keep the then-arm bits. The
    // payload absorbs the difference so unknown bits stay normalized.
    SIRValue {
        payload: tv | &difference,
        mask: tm | &difference,
    }
}

fn parity(value: &BigUint) -> bool {
    value
        .to_bytes_le()
        .iter()
        .fold(0u8, |acc, byte| acc ^ byte)
        .count_ones()
        % 2
        == 1
}

fn trailing_zeros(value: &BigUint, width: usize) -> usize {
    if value.is_zero() {
        return width;
    }
    // Lowest set bit position: isolate it with v & (v-1) complement trick.
    let isolated = value ^ (value - 1u8);
    isolated.bits() as usize - 1
}

trait BigUintExt {
    fn popcount(&self) -> u64;
}

impl BigUintExt for BigUint {
    fn popcount(&self) -> u64 {
        self.to_bytes_le()
            .iter()
            .map(|byte| u64::from(byte.count_ones()))
            .sum()
    }
}

// ── Register file ─────────────────────────────────────────────────────

struct Registers {
    values: Vec<Option<SIRValue>>,
    widths: Vec<usize>,
    signed: Vec<bool>,
}

impl Registers {
    fn new(register_map: &HashMap<RegisterId, RegisterType>) -> Self {
        let size = register_map.keys().map(|id| id.0 + 1).max().unwrap_or(0);
        let values = vec![None; size];
        let mut widths = vec![0; size];
        let mut signed = vec![false; size];
        for (id, register_type) in register_map {
            widths[id.0] = register_type.width();
            signed[id.0] = register_type.is_signed();
        }
        Self {
            values,
            widths,
            signed,
        }
    }

    fn get(&self, id: RegisterId) -> Result<&SIRValue, InterpError> {
        self.values
            .get(id.0)
            .and_then(|slot| slot.as_ref())
            .ok_or(InterpError::MissingRegister(id))
    }

    fn set(&mut self, id: RegisterId, value: SIRValue) {
        if let Some(slot) = self.values.get_mut(id.0) {
            *slot = Some(value);
        }
    }

    fn width(&self, id: RegisterId) -> usize {
        self.widths.get(id.0).copied().unwrap_or(0)
    }

    fn is_signed(&self, id: RegisterId) -> bool {
        self.signed.get(id.0).copied().unwrap_or(false)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use celox_sir::{BasicBlock, SIRSwitchCase};

    #[derive(Default)]
    struct FakeMachine {
        cells: HashMap<(u32, usize, usize), SIRValue>,
        runtime_events: Vec<(u32, Vec<SIRValue>)>,
        comb_captures: Vec<usize>,
        trigger_notifications: Vec<usize>,
    }

    impl FakeMachine {
        fn stored(&self, addr: u32, offset: usize, bits: usize) -> &SIRValue {
            self.cells.get(&(addr, offset, bits)).unwrap()
        }
    }

    impl InterpMachine<u32> for FakeMachine {
        fn load(
            &mut self,
            addr: &u32,
            access: ResolvedAccess<'_>,
            bits: usize,
        ) -> Result<SIRValue, InterpError> {
            let offset = match access.offset {
                SIROffset::Static(offset) => *offset,
                other => {
                    return Err(InterpError::Machine(format!(
                        "fake machine cannot resolve {other}"
                    )));
                }
            };
            Ok(self
                .cells
                .get(&(*addr, offset, bits))
                .cloned()
                .unwrap_or_else(|| SIRValue::new(BigUint::zero())))
        }

        fn store(
            &mut self,
            addr: &u32,
            access: ResolvedAccess<'_>,
            bits: usize,
            value: &SIRValue,
        ) -> Result<(), InterpError> {
            let offset = match access.offset {
                SIROffset::Static(offset) => *offset,
                other => {
                    return Err(InterpError::Machine(format!(
                        "fake machine cannot resolve {other}"
                    )));
                }
            };
            self.cells.insert((*addr, offset, bits), value.clone());
            Ok(())
        }

        fn commit(
            &mut self,
            src: &u32,
            dst: &u32,
            access: ResolvedAccess<'_>,
            bits: usize,
        ) -> Result<(), InterpError> {
            let offset = match access.offset {
                SIROffset::Static(offset) => *offset,
                other => {
                    return Err(InterpError::Machine(format!(
                        "fake machine cannot resolve {other}"
                    )));
                }
            };
            let value = self.cells.get(&(*src, offset, bits)).cloned();
            if let Some(value) = value {
                self.cells.insert((*dst, offset, bits), value);
            }
            Ok(())
        }

        fn notify_triggers(&mut self, _addr: &u32, triggers: &[TriggerIdWithKind]) {
            self.trigger_notifications.push(triggers.len());
        }

        fn capture_store_range(
            &mut self,
            addr: &u32,
            access: ResolvedAccess<'_>,
            bits: usize,
        ) -> Result<StoreSnapshot, InterpError> {
            let offset = match access.offset {
                SIROffset::Static(offset) => *offset,
                other => {
                    return Err(InterpError::Machine(format!(
                        "fake machine cannot resolve {other}"
                    )));
                }
            };
            Ok(StoreSnapshot {
                value_words: self
                    .cells
                    .get(&(*addr, offset, bits))
                    .map(|value| Self::words(&value.payload))
                    .unwrap_or_default(),
                mask_words: Vec::new(),
            })
        }

        fn enable_comb_captures(
            &mut self,
            _addr: &u32,
            _access: ResolvedAccess<'_>,
            _bits: usize,
            _before: &StoreSnapshot,
            sites: &[u32],
        ) -> Result<(), InterpError> {
            self.comb_captures.push(sites.len());
            Ok(())
        }

        fn emit_runtime_event(
            &mut self,
            site_id: u32,
            args: &[SIRValue],
        ) -> Result<(), InterpError> {
            self.runtime_events.push((site_id, args.to_vec()));
            Ok(())
        }

        fn emit_comb_capture_event(
            &mut self,
            _site_id: u32,
            _args: &[SIRValue],
            _fatal_error_code: Option<i64>,
            _consume_enabled: bool,
        ) -> Result<(), InterpError> {
            Ok(())
        }

        fn enable_comb_capture_if_changed(
            &mut self,
            _old: &SIRValue,
            _new: &SIRValue,
            _sites: &[u32],
        ) -> Result<(), InterpError> {
            Ok(())
        }
    }

    impl FakeMachine {
        fn words(_value: &BigUint) -> Vec<u64> {
            Vec::new()
        }
    }

    fn _addr_owned(addr: &u32) -> u32 {
        *addr
    }

    fn bit_regs(specs: &[(usize, usize)]) -> HashMap<RegisterId, RegisterType> {
        specs
            .iter()
            .map(|&(id, width)| {
                (
                    RegisterId(id),
                    RegisterType::Bit {
                        width,
                        signed: false,
                    },
                )
            })
            .collect()
    }

    fn block(
        id: usize,
        params: Vec<usize>,
        instructions: Vec<SIRInstruction<u32>>,
        terminator: SIRTerminator,
    ) -> (BlockId, BasicBlock<u32>) {
        (
            BlockId(id),
            BasicBlock {
                id: BlockId(id),
                params: params.into_iter().map(RegisterId).collect(),
                instructions,
                terminator,
            },
        )
    }

    #[test]
    fn executes_straight_line_arithmetic_and_store() {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [block(
                0,
                vec![],
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(5u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(3u8)),
                    SIRInstruction::Binary(
                        RegisterId(2),
                        RegisterId(0),
                        BinaryOp::Add,
                        RegisterId(1),
                    ),
                    SIRInstruction::Store(
                        7u32,
                        SIROffset::Static(0),
                        8,
                        RegisterId(2),
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                SIRTerminator::Return,
            )]
            .into_iter()
            .collect(),
            register_map: bit_regs(&[(0, 8), (1, 8), (2, 8)]),
        };

        let mut machine = FakeMachine::default();
        execute_unit(&unit, &mut machine, &[], true).unwrap();
        assert_eq!(machine.stored(7, 0, 8).payload, BigUint::from(8u8));
    }

    #[test]
    fn branch_selects_target_by_known_condition_bits() {
        for (cond, expected) in [(1u8, 10u8), (0, 20)] {
            let unit = ExecutionUnit {
                entry_block_id: BlockId(0),
                blocks: [
                    block(
                        0,
                        vec![],
                        vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(cond))],
                        SIRTerminator::Branch {
                            cond: RegisterId(0),
                            true_block: (BlockId(1), vec![]),
                            false_block: (BlockId(2), vec![]),
                        },
                    ),
                    block(
                        1,
                        vec![],
                        vec![SIRInstruction::Imm(RegisterId(1), SIRValue::new(10u8))],
                        SIRTerminator::Jump(BlockId(3), vec![]),
                    ),
                    block(
                        2,
                        vec![],
                        vec![SIRInstruction::Imm(RegisterId(1), SIRValue::new(20u8))],
                        SIRTerminator::Jump(BlockId(3), vec![]),
                    ),
                    block(
                        3,
                        vec![],
                        vec![SIRInstruction::Store(
                            1u32,
                            SIROffset::Static(0),
                            8,
                            RegisterId(1),
                            Vec::new(),
                            Vec::new(),
                        )],
                        SIRTerminator::Return,
                    ),
                ]
                .into_iter()
                .collect(),
                register_map: bit_regs(&[(0, 1), (1, 8)]),
            };

            let mut machine = FakeMachine::default();
            execute_unit(&unit, &mut machine, &[], true).unwrap();
            assert_eq!(machine.stored(1, 0, 8).payload, BigUint::from(expected));
        }
    }

    #[test]
    fn switch_matches_cases_and_falls_back_to_default() {
        for (selector, expected) in [(2u8, 2u8), (9, 99)] {
            let unit = ExecutionUnit {
                entry_block_id: BlockId(0),
                blocks: [
                    block(
                        0,
                        vec![],
                        vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(selector))],
                        SIRTerminator::Switch {
                            selector: RegisterId(0),
                            cases: vec![
                                SIRSwitchCase {
                                    value: BigUint::from(1u8),
                                    target: BlockId(1),
                                },
                                SIRSwitchCase {
                                    value: BigUint::from(2u8),
                                    target: BlockId(2),
                                },
                            ],
                            default: BlockId(3),
                        },
                    ),
                    block(
                        1,
                        vec![],
                        vec![SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8))],
                        SIRTerminator::Jump(BlockId(4), vec![]),
                    ),
                    block(
                        2,
                        vec![],
                        vec![SIRInstruction::Imm(RegisterId(1), SIRValue::new(2u8))],
                        SIRTerminator::Jump(BlockId(4), vec![]),
                    ),
                    block(
                        3,
                        vec![],
                        vec![SIRInstruction::Imm(RegisterId(1), SIRValue::new(99u8))],
                        SIRTerminator::Jump(BlockId(4), vec![]),
                    ),
                    block(
                        4,
                        vec![],
                        vec![SIRInstruction::Store(
                            1u32,
                            SIROffset::Static(0),
                            8,
                            RegisterId(1),
                            Vec::new(),
                            Vec::new(),
                        )],
                        SIRTerminator::Return,
                    ),
                ]
                .into_iter()
                .collect(),
                register_map: bit_regs(&[(0, 4), (1, 8)]),
            };

            let mut machine = FakeMachine::default();
            execute_unit(&unit, &mut machine, &[], true).unwrap();
            assert_eq!(machine.stored(1, 0, 8).payload, BigUint::from(expected));
        }
    }

    #[test]
    fn jump_binds_target_block_parameters() {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                block(
                    0,
                    vec![],
                    vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(7u8))],
                    SIRTerminator::Jump(BlockId(1), vec![RegisterId(0)]),
                ),
                block(
                    1,
                    vec![1],
                    vec![SIRInstruction::Store(
                        3u32,
                        SIROffset::Static(0),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    )],
                    SIRTerminator::Return,
                ),
            ]
            .into_iter()
            .collect(),
            register_map: bit_regs(&[(0, 8), (1, 8)]),
        };

        let mut machine = FakeMachine::default();
        execute_unit(&unit, &mut machine, &[], true).unwrap();
        assert_eq!(machine.stored(3, 0, 8).payload, BigUint::from(7u8));
    }

    #[test]
    fn entry_parameters_bind_caller_supplied_arguments() {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [block(
                0,
                vec![0],
                vec![SIRInstruction::Store(
                    5u32,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                )],
                SIRTerminator::Return,
            )]
            .into_iter()
            .collect(),
            register_map: bit_regs(&[(0, 8)]),
        };

        let mut machine = FakeMachine::default();
        execute_unit(&unit, &mut machine, &[SIRValue::new(0xDEu16)], true).unwrap();
        assert_eq!(machine.stored(5, 0, 8).payload, BigUint::from(0xDEu16));
    }

    #[test]
    fn concat_and_slice_roundtrip_msbf_order() {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [block(
                0,
                vec![],
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(0xABu8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(0xCDu8)),
                    SIRInstruction::Concat(RegisterId(2), vec![RegisterId(0), RegisterId(1)]),
                    SIRInstruction::Slice(RegisterId(3), RegisterId(2), 4, 8),
                    SIRInstruction::Store(
                        9u32,
                        SIROffset::Static(0),
                        8,
                        RegisterId(3),
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                SIRTerminator::Return,
            )]
            .into_iter()
            .collect(),
            register_map: bit_regs(&[(0, 8), (1, 8), (2, 16), (3, 8)]),
        };

        let mut machine = FakeMachine::default();
        execute_unit(&unit, &mut machine, &[], true).unwrap();
        // 0xABCD sliced [4 +: 8] == 0xBC.
        assert_eq!(machine.stored(9, 0, 8).payload, BigUint::from(0xBCu8));
    }

    #[test]
    fn concat_truncates_to_destination_width() {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [block(
                0,
                vec![],
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(0xFFu8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(0xFFu8)),
                    SIRInstruction::Concat(RegisterId(2), vec![RegisterId(0), RegisterId(1)]),
                    SIRInstruction::Store(
                        9u32,
                        SIROffset::Static(0),
                        8,
                        RegisterId(2),
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                SIRTerminator::Return,
            )]
            .into_iter()
            .collect(),
            // Destination is narrower than the concatenated sources: the
            // declared-register-width invariant keeps the low 8 bits.
            register_map: bit_regs(&[(0, 8), (1, 8), (2, 8)]),
        };

        let mut machine = FakeMachine::default();
        execute_unit(&unit, &mut machine, &[], true).unwrap();
        assert_eq!(machine.stored(9, 0, 8).payload, BigUint::from(0xFFu8));
    }

    #[test]
    fn mux_follows_four_state_selection_contract() {
        let known_one = SIRValue::new(1u8);
        let known_zero = SIRValue::new(0u8);
        let unknown = SIRValue::new_four_state(0u8, 1u8);

        // Known-one condition selects the then-arm exactly, mask included.
        let mixed_arm = SIRValue::new_four_state(0b01u8, 0b10u8);
        assert_eq!(
            eval_mux(&known_one, &mixed_arm, &SIRValue::new(0u8), 1, 2),
            mixed_arm
        );
        // Known-zero condition selects the else-arm.
        assert_eq!(
            eval_mux(&known_zero, &mixed_arm, &SIRValue::new(0b11u8), 1, 2),
            SIRValue::new(0b11u8)
        );
        // Unknown condition: agreeing bits preserved, differing bits become X
        // with a normalized (all-ones) payload.
        let out = eval_mux(
            &unknown,
            &SIRValue::new_four_state(0b1010u8, 0b0000u8),
            &SIRValue::new_four_state(0b0011u8, 0b0100u8),
            1,
            4,
        );
        // difference = 1101 → mask keeps those bits unknown, payload absorbs
        // them so X bits read as one.
        assert_eq!(out.payload, BigUint::from(0b1111u8));
        assert_eq!(out.mask, BigUint::from(0b1101u8));
    }

    #[test]
    fn mux_condition_is_evaluated_in_its_own_width() {
        // An 8-bit condition with a known one above the 4-bit destination
        // width must still select the then-arm.
        let wide_cond = SIRValue::new(0b1_0000u8);
        let out = eval_mux(
            &wide_cond,
            &SIRValue::new(0xAu8),
            &SIRValue::new(0x5u8),
            8,
            4,
        );
        assert_eq!(out.payload, BigUint::from(0xAu8));
    }

    #[test]
    fn error_terminator_surfaces_fatal_code() {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [block(0, vec![], vec![], SIRTerminator::Error(42))]
                .into_iter()
                .collect(),
            register_map: HashMap::default(),
        };

        let mut machine = FakeMachine::default();
        assert_eq!(
            execute_unit(&unit, &mut machine, &[], true).unwrap_err(),
            InterpError::Fatal(42)
        );
    }

    #[test]
    fn jump_to_missing_block_reports_unknown_block() {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [block(
                0,
                vec![],
                vec![],
                SIRTerminator::Jump(BlockId(99), vec![]),
            )]
            .into_iter()
            .collect(),
            register_map: HashMap::default(),
        };

        let mut machine = FakeMachine::default();
        assert_eq!(
            execute_unit(&unit, &mut machine, &[], true).unwrap_err(),
            InterpError::UnknownBlock(BlockId(99))
        );
    }

    #[test]
    fn arity_mismatch_between_jump_and_target_params_is_rejected() {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                block(0, vec![], vec![], SIRTerminator::Jump(BlockId(1), vec![])),
                block(1, vec![0], vec![], SIRTerminator::Return),
            ]
            .into_iter()
            .collect(),
            register_map: bit_regs(&[(0, 8)]),
        };

        let mut machine = FakeMachine::default();
        assert_eq!(
            execute_unit(&unit, &mut machine, &[], true).unwrap_err(),
            InterpError::RegisterArityMismatch {
                expected: 1,
                found: 0,
            }
        );
    }

    #[test]
    fn reading_unwritten_register_is_rejected() {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [block(
                0,
                vec![],
                vec![SIRInstruction::Store(
                    1u32,
                    SIROffset::Static(0),
                    8,
                    RegisterId(4),
                    Vec::new(),
                    Vec::new(),
                )],
                SIRTerminator::Return,
            )]
            .into_iter()
            .collect(),
            register_map: bit_regs(&[(4, 8)]),
        };

        let mut machine = FakeMachine::default();
        assert_eq!(
            execute_unit(&unit, &mut machine, &[], true).unwrap_err(),
            InterpError::MissingRegister(RegisterId(4))
        );
    }

    #[test]
    fn runtime_events_receive_resolved_argument_values() {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [block(
                0,
                vec![],
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(11u8)),
                    SIRInstruction::RuntimeEvent {
                        site_id: 3,
                        args: vec![RegisterId(0)],
                    },
                ],
                SIRTerminator::Return,
            )]
            .into_iter()
            .collect(),
            register_map: bit_regs(&[(0, 8)]),
        };

        let mut machine = FakeMachine::default();
        execute_unit(&unit, &mut machine, &[], true).unwrap();
        assert_eq!(machine.runtime_events, vec![(3, vec![SIRValue::new(11u8)])]);
    }
}
