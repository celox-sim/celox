//! Condition codes, arithmetic, shifts, division, and mask emission.

use super::*;

/// Emit setcc instruction for a comparison kind.
pub(super) fn emit_jcc(
    asm: &mut CodeAssembler,
    label: CodeLabel,
    kind: CmpKind,
) -> Result<(), IcedError> {
    match kind {
        CmpKind::Eq => asm.je(label),
        CmpKind::Ne => asm.jne(label),
        CmpKind::LtU => asm.jb(label),
        CmpKind::LtS => asm.jl(label),
        CmpKind::LeU => asm.jbe(label),
        CmpKind::LeS => asm.jle(label),
        CmpKind::GtU => asm.ja(label),
        CmpKind::GtS => asm.jg(label),
        CmpKind::GeU => asm.jae(label),
        CmpKind::GeS => asm.jge(label),
    }
}

pub(super) fn emit_inverse_jcc(
    asm: &mut CodeAssembler,
    label: CodeLabel,
    kind: CmpKind,
) -> Result<(), IcedError> {
    match kind {
        CmpKind::Eq => asm.jne(label),
        CmpKind::Ne => asm.je(label),
        CmpKind::LtU => asm.jae(label),
        CmpKind::LtS => asm.jge(label),
        CmpKind::LeU => asm.ja(label),
        CmpKind::LeS => asm.jg(label),
        CmpKind::GtU => asm.jbe(label),
        CmpKind::GtS => asm.jle(label),
        CmpKind::GeU => asm.jb(label),
        CmpKind::GeS => asm.jl(label),
    }
}

fn emit_cmovcc(
    asm: &mut CodeAssembler,
    dst: AsmRegister64,
    src: AsmRegister64,
    kind: CmpKind,
) -> Result<(), IcedError> {
    match kind {
        CmpKind::Eq => asm.cmove(dst, src),
        CmpKind::Ne => asm.cmovne(dst, src),
        CmpKind::LtU => asm.cmovb(dst, src),
        CmpKind::LtS => asm.cmovl(dst, src),
        CmpKind::LeU => asm.cmovbe(dst, src),
        CmpKind::LeS => asm.cmovle(dst, src),
        CmpKind::GtU => asm.cmova(dst, src),
        CmpKind::GtS => asm.cmovg(dst, src),
        CmpKind::GeU => asm.cmovae(dst, src),
        CmpKind::GeS => asm.cmovge(dst, src),
    }
}

fn emit_inverse_cmovcc(
    asm: &mut CodeAssembler,
    dst: AsmRegister64,
    src: AsmRegister64,
    kind: CmpKind,
) -> Result<(), IcedError> {
    match kind {
        CmpKind::Eq => asm.cmovne(dst, src),
        CmpKind::Ne => asm.cmove(dst, src),
        CmpKind::LtU => asm.cmovae(dst, src),
        CmpKind::LtS => asm.cmovge(dst, src),
        CmpKind::LeU => asm.cmova(dst, src),
        CmpKind::LeS => asm.cmovg(dst, src),
        CmpKind::GtU => asm.cmovbe(dst, src),
        CmpKind::GtS => asm.cmovle(dst, src),
        CmpKind::GeU => asm.cmovb(dst, src),
        CmpKind::GeS => asm.cmovl(dst, src),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_cmp_select(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    kind: CmpKind,
    true_val: VReg,
    false_val: VReg,
) -> Result<(), IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let r = preg_to_reg64(resolve(assignment, rhs));
    let tv = preg_to_reg64(resolve(assignment, true_val));
    let fv = preg_to_reg64(resolve(assignment, false_val));

    if tv == fv {
        if d != tv {
            asm.mov(d, tv)?;
        }
        return Ok(());
    }

    asm.cmp(l, r)?;
    if d == fv {
        emit_cmovcc(asm, d, tv, kind)?;
    } else if d == tv {
        emit_inverse_cmovcc(asm, d, fv, kind)?;
    } else {
        asm.mov(d, fv)?;
        emit_cmovcc(asm, d, tv, kind)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_cmp_imm_select(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    imm: i32,
    kind: CmpKind,
    true_val: VReg,
    false_val: VReg,
) -> Result<(), IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let tv = preg_to_reg64(resolve(assignment, true_val));
    let fv = preg_to_reg64(resolve(assignment, false_val));

    if tv == fv {
        if d != tv {
            asm.mov(d, tv)?;
        }
        return Ok(());
    }

    if imm == 0 && matches!(kind, CmpKind::Eq | CmpKind::Ne) {
        asm.test(l, l)?;
    } else {
        asm.cmp(l, imm)?;
    }
    if d == fv {
        emit_cmovcc(asm, d, tv, kind)?;
    } else if d == tv {
        emit_inverse_cmovcc(asm, d, fv, kind)?;
    } else {
        asm.mov(d, fv)?;
        emit_cmovcc(asm, d, tv, kind)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_guarded_cmp_select(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    guard: VReg,
    lhs: VReg,
    rhs: VReg,
    kind: CmpKind,
    true_val: VReg,
    false_val: VReg,
    continuation_label: Option<&mut CodeLabel>,
) -> Result<bool, IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let g = preg_to_reg64(resolve(assignment, guard));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let r = preg_to_reg64(resolve(assignment, rhs));
    let tv = preg_to_reg64(resolve(assignment, true_val));
    let fv = preg_to_reg64(resolve(assignment, false_val));

    if d == g || d == l || d == r {
        return emit_guarded_cmp_select_branchy(asm, d, g, l, r, kind, tv, fv, continuation_label);
    }

    if tv == fv {
        if d != tv {
            asm.mov(d, tv)?;
        }
    } else if d == fv {
        if let Some(done) = continuation_label {
            asm.test(g, g)?;
            asm.je(*done)?;
            asm.cmp(l, r)?;
            emit_cmovcc(asm, d, tv, kind)?;
            asm.set_label(done)?;
            return Ok(true);
        } else {
            let mut done = asm.create_label();
            asm.test(g, g)?;
            asm.je(done)?;
            asm.cmp(l, r)?;
            emit_cmovcc(asm, d, tv, kind)?;
            asm.set_label(&mut done)?;
        }
    } else if d == tv {
        asm.cmp(l, r)?;
        emit_inverse_cmovcc(asm, d, fv, kind)?;
        asm.test(g, g)?;
        asm.cmove(d, fv)?;
    } else {
        asm.mov(d, fv)?;
        asm.cmp(l, r)?;
        emit_cmovcc(asm, d, tv, kind)?;
        asm.test(g, g)?;
        asm.cmove(d, fv)?;
    }
    Ok(false)
}

fn emit_guarded_cmp_select_branchy(
    asm: &mut CodeAssembler,
    dst: AsmRegister64,
    guard: AsmRegister64,
    lhs: AsmRegister64,
    rhs: AsmRegister64,
    kind: CmpKind,
    true_val: AsmRegister64,
    false_val: AsmRegister64,
    continuation_label: Option<&mut CodeLabel>,
) -> Result<bool, IcedError> {
    let mut false_label = asm.create_label();
    let mut true_label = asm.create_label();
    if let Some(done) = continuation_label {
        asm.test(guard, guard)?;
        asm.je(false_label)?;
        asm.cmp(lhs, rhs)?;
        emit_jcc(asm, true_label, kind)?;
        asm.set_label(&mut false_label)?;
        if dst != false_val {
            asm.mov(dst, false_val)?;
        }
        asm.jmp(*done)?;
        asm.set_label(&mut true_label)?;
        if dst != true_val {
            asm.mov(dst, true_val)?;
        } else {
            asm.nop()?;
        }
        asm.set_label(done)?;
        Ok(true)
    } else {
        let mut done = asm.create_label();
        asm.test(guard, guard)?;
        asm.je(false_label)?;
        asm.cmp(lhs, rhs)?;
        emit_jcc(asm, true_label, kind)?;
        asm.set_label(&mut false_label)?;
        if dst != false_val {
            asm.mov(dst, false_val)?;
        }
        asm.jmp(done)?;
        asm.set_label(&mut true_label)?;
        if dst != true_val {
            asm.mov(dst, true_val)?;
        } else {
            asm.nop()?;
        }
        asm.set_label(&mut done)?;
        Ok(false)
    }
}

pub(super) fn emit_setcc(
    asm: &mut CodeAssembler,
    d8: AsmRegister8,
    kind: CmpKind,
) -> Result<(), IcedError> {
    match kind {
        CmpKind::Eq => asm.sete(d8),
        CmpKind::Ne => asm.setne(d8),
        CmpKind::LtU => asm.setb(d8),
        CmpKind::LtS => asm.setl(d8),
        CmpKind::LeU => asm.setbe(d8),
        CmpKind::LeS => asm.setle(d8),
        CmpKind::GtU => asm.seta(d8),
        CmpKind::GtS => asm.setg(d8),
        CmpKind::GeU => asm.setae(d8),
        CmpKind::GeS => asm.setge(d8),
    }
}

/// Shift operation kind.
pub(super) enum ShiftOp {
    Shr,
    Shl,
    Sar,
}

/// Emit the shift encoding selected by the function's target-feature snapshot.
pub(super) fn emit_shift(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: ShiftOp,
    encoding: VariableShiftEncoding,
) -> Result<(), IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let r = preg_to_reg64(resolve(assignment, rhs));

    match encoding {
        VariableShiftEncoding::Bmi2 => match op {
            ShiftOp::Shr => asm.shrx(d, l, r)?,
            ShiftOp::Shl => asm.shlx(d, l, r)?,
            ShiftOp::Sar => asm.sarx(d, l, r)?,
        },
        VariableShiftEncoding::LegacyCl => {
            // The allocation verifier proves the fixed-use constraint.
            debug_assert!(r == rcx, "legacy shift rhs must be in RCX");
            if d == rcx && l != rcx {
                // Moving lhs into RCX first would destroy the count in CL.
                // Shift an arena-saved copy in place and reload it into RCX, so
                // the original lhs register remains untouched.
                asm.mov(qword_ptr(scratch_operand(0)), l)?;
                match op {
                    ShiftOp::Shr => asm.shr(qword_ptr(scratch_operand(0)), cl)?,
                    ShiftOp::Shl => asm.shl(qword_ptr(scratch_operand(0)), cl)?,
                    ShiftOp::Sar => asm.sar(qword_ptr(scratch_operand(0)), cl)?,
                }
                asm.mov(rcx, qword_ptr(scratch_operand(0)))?;
            } else {
                if d != l {
                    asm.mov(d, l)?;
                }
                match op {
                    ShiftOp::Shr => asm.shr(d, cl)?,
                    ShiftOp::Shl => asm.shl(d, cl)?,
                    ShiftOp::Sar => asm.sar(d, cl)?,
                }
            }
        }
    }
    Ok(())
}

/// Division operation kind.
#[derive(Clone, Copy)]
pub(super) enum DivOp {
    Div, // quotient in RAX
    Rem, // remainder in RDX
    SDiv,
    SRem,
}

/// Emit integer division/remainder using unsigned `div` or signed `idiv`.
/// Both consume RDX:RAX and produce the quotient in RAX and remainder in RDX.
///
/// The assignment phase avoids placing live-across VRegs in RAX/RDX around
/// div/rem instructions, so no save/restore is needed here.
pub(super) fn emit_divrem(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: DivOp,
) -> Result<(), IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let r = preg_to_reg64(resolve(assignment, rhs));

    let result_reg: AsmRegister64 = match op {
        DivOp::Div | DivOp::SDiv => rax,
        DivOp::Rem | DivOp::SRem => rdx,
    };
    let signed = matches!(op, DivOp::SDiv | DivOp::SRem);

    // Divisor cannot be read from RAX/RDX because div consumes RDX:RAX.
    // Use a stack copy instead of an unmodeled scratch register clobber.
    let rhs_on_stack = r == rax || r == rdx;
    if rhs_on_stack {
        asm.mov(qword_ptr(scratch_operand(0)), r)?;
    }

    if l != rax {
        asm.mov(rax, l)?;
    }
    if signed {
        asm.cqo()?;
    } else {
        asm.xor(edx, edx)?;
    }
    if rhs_on_stack {
        if signed {
            asm.idiv(qword_ptr(scratch_operand(0)))?;
        } else {
            asm.div(qword_ptr(scratch_operand(0)))?;
        }
    } else if signed {
        asm.idiv(r)?;
    } else {
        asm.div(r)?;
    }

    if d != result_reg {
        asm.mov(d, result_reg)?;
    }

    Ok(())
}

/// Helper for 2-operand binary operations (add, sub, and, or, xor).
pub(super) enum BinOp {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
}

impl BinOp {
    /// Whether the operation is commutative (a op b == b op a).
    fn is_commutative(&self) -> bool {
        matches!(
            self,
            BinOp::Add | BinOp::Mul | BinOp::And | BinOp::Or | BinOp::Xor
        )
    }
}

pub(super) fn emit_binop_rr(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: BinOp,
    narrow32: bool,
) -> Result<(), IcedError> {
    if narrow32 {
        emit_binop_rr_32(asm, assignment, dst, lhs, rhs, op)
    } else {
        emit_binop_rr_64(asm, assignment, dst, lhs, rhs, op)
    }
}

pub(super) fn emit_binop_memory(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    op: BinOp,
    narrow32: bool,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    memory_vreg: VReg,
    memory: AsmMemoryOperand,
) -> Result<bool, IcedError> {
    if rhs == memory_vreg {
        let other = lhs;
        if narrow32 {
            let d = preg_to_reg32(resolve(assignment, dst));
            let o = preg_to_reg32(resolve(assignment, other));
            if d != o {
                asm.mov(d, o)?;
            }
            let mem = dword_ptr(memory);
            match op {
                BinOp::Add => asm.add(d, mem)?,
                BinOp::Sub => asm.sub(d, mem)?,
                BinOp::Mul => asm.imul_2(d, mem)?,
                BinOp::And => asm.and(d, mem)?,
                BinOp::Or => asm.or(d, mem)?,
                BinOp::Xor => asm.xor(d, mem)?,
            }
        } else {
            let d = preg_to_reg64(resolve(assignment, dst));
            let o = preg_to_reg64(resolve(assignment, other));
            if d != o {
                asm.mov(d, o)?;
            }
            let mem = qword_ptr(memory);
            match op {
                BinOp::Add => asm.add(d, mem)?,
                BinOp::Sub => asm.sub(d, mem)?,
                BinOp::Mul => asm.imul_2(d, mem)?,
                BinOp::And => asm.and(d, mem)?,
                BinOp::Or => asm.or(d, mem)?,
                BinOp::Xor => asm.xor(d, mem)?,
            }
        }
        return Ok(true);
    }

    if lhs == memory_vreg && op.is_commutative() {
        let other = rhs;
        if narrow32 {
            let d = preg_to_reg32(resolve(assignment, dst));
            let o = preg_to_reg32(resolve(assignment, other));
            if d != o {
                asm.mov(d, o)?;
            }
            let mem = dword_ptr(memory);
            match op {
                BinOp::Add => asm.add(d, mem)?,
                BinOp::Mul => asm.imul_2(d, mem)?,
                BinOp::And => asm.and(d, mem)?,
                BinOp::Or => asm.or(d, mem)?,
                BinOp::Xor => asm.xor(d, mem)?,
                BinOp::Sub => unreachable!(),
            }
        } else {
            let d = preg_to_reg64(resolve(assignment, dst));
            let o = preg_to_reg64(resolve(assignment, other));
            if d != o {
                asm.mov(d, o)?;
            }
            let mem = qword_ptr(memory);
            match op {
                BinOp::Add => asm.add(d, mem)?,
                BinOp::Mul => asm.imul_2(d, mem)?,
                BinOp::And => asm.and(d, mem)?,
                BinOp::Or => asm.or(d, mem)?,
                BinOp::Xor => asm.xor(d, mem)?,
                BinOp::Sub => unreachable!(),
            }
        }
        return Ok(true);
    }

    Ok(false)
}

fn emit_binop_rr_64(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: BinOp,
) -> Result<(), IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let r = preg_to_reg64(resolve(assignment, rhs));

    if matches!(op, BinOp::Add) && d != l && d != r {
        asm.lea(d, ptr(l + r))?;
        return Ok(());
    }

    let (eff_l, eff_r) = if d == r && d != l {
        if op.is_commutative() {
            (r, l)
        } else {
            asm.neg(d)?;
            asm.add(d, l)?;
            return Ok(());
        }
    } else {
        if d != l {
            asm.mov(d, l)?;
        }
        (d, r)
    };

    let _ = eff_l;
    match op {
        BinOp::Add => asm.add(d, eff_r)?,
        BinOp::Sub => asm.sub(d, eff_r)?,
        BinOp::Mul => asm.imul_2(d, eff_r)?,
        BinOp::And => asm.and(d, eff_r)?,
        BinOp::Or => asm.or(d, eff_r)?,
        BinOp::Xor => asm.xor(d, eff_r)?,
    }
    Ok(())
}

fn emit_binop_rr_32(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: BinOp,
) -> Result<(), IcedError> {
    let dp = resolve(assignment, dst);
    let lp = resolve(assignment, lhs);
    let rp = resolve(assignment, rhs);
    let d = preg_to_reg32(dp);
    let l = preg_to_reg32(lp);
    let r = preg_to_reg32(rp);

    if matches!(op, BinOp::Add) && d != l && d != r {
        asm.lea(d, ptr(preg_to_reg64(lp) + preg_to_reg64(rp)))?;
        return Ok(());
    }

    let (eff_l, eff_r) = if d == r && d != l {
        if op.is_commutative() {
            (r, l)
        } else {
            // Non-commutative (sub): d == rhs, d != lhs.
            asm.neg(d)?;
            asm.add(d, l)?;
            return Ok(());
        }
    } else {
        if d != l {
            asm.mov(d, l)?;
        }
        (d, r)
    };

    let _ = eff_l;
    match op {
        BinOp::Add => asm.add(d, eff_r)?,
        BinOp::Sub => asm.sub(d, eff_r)?,
        BinOp::Mul => asm.imul_2(d, eff_r)?,
        BinOp::And => asm.and(d, eff_r)?,
        BinOp::Or => asm.or(d, eff_r)?,
        BinOp::Xor => asm.xor(d, eff_r)?,
    }
    Ok(())
}

pub(super) fn emit_select_memory(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    cond: VReg,
    true_val: VReg,
    false_val: VReg,
    memory_vreg: VReg,
    memory: AsmMemoryOperand,
) -> Result<bool, IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    if cond == memory_vreg {
        asm.cmp(qword_ptr(memory), 0)?;
        let tv = preg_to_reg64(resolve(assignment, true_val));
        let fv = preg_to_reg64(resolve(assignment, false_val));
        if d == tv {
            asm.cmove(d, fv)?;
        } else {
            if d != fv {
                asm.mov(d, fv)?;
            }
            asm.cmovne(d, tv)?;
        }
        return Ok(true);
    }

    let c = preg_to_reg64(resolve(assignment, cond));
    asm.test(c, c)?;
    if true_val == memory_vreg {
        let fv = preg_to_reg64(resolve(assignment, false_val));
        if d != fv {
            asm.mov(d, fv)?;
        }
        asm.cmovne(d, qword_ptr(memory))?;
        return Ok(true);
    }
    if false_val == memory_vreg {
        let tv = preg_to_reg64(resolve(assignment, true_val));
        if d != tv {
            asm.mov(d, tv)?;
        }
        asm.cmove(d, qword_ptr(memory))?;
        return Ok(true);
    }

    Ok(false)
}

/// Emit OR with a potentially 64-bit immediate.
/// Uses the most efficient encoding available.
pub(super) fn emit_or_imm64(
    asm: &mut CodeAssembler,
    d: AsmRegister64,
    imm: u64,
) -> Result<(), IcedError> {
    if imm == 0 {
        return Ok(());
    }
    let signed = imm as i64;
    // ISel must decompose 64-bit OR immediates into LoadImm + Or.
    assert!(
        signed >= i32::MIN as i64 && signed <= i32::MAX as i64,
        "OrImm {imm:#x} exceeds i32: ISel should emit LoadImm + Or instead"
    );
    asm.or(d, signed as i32)?;
    Ok(())
}

/// Truncation masks can copy and clear the high bits in one instruction. MIR
/// does not expose the flags written by AND, so MOVZX/MOV may replace it.
pub(super) fn emit_and_imm(
    asm: &mut CodeAssembler,
    dst: PhysReg,
    src: PhysReg,
    imm: u64,
) -> Result<(), IcedError> {
    let d32 = preg_to_reg32(dst);
    match imm {
        0 => asm.xor(d32, d32)?,
        0xff => asm.movzx(d32, preg_to_reg8(src))?,
        0xffff => asm.movzx(d32, preg_to_reg16(src))?,
        0xffff_ffff => {
            // Even a self-copy must clear the upper half of the register.
            asm.mov(d32, preg_to_reg32(src))?;
        }
        1..=0xffff_fffe => {
            if dst != src {
                asm.mov(d32, preg_to_reg32(src))?;
            }
            asm.and(d32, imm as i32)?;
        }
        _ => {
            let signed = imm as i64;
            assert!(
                i32::try_from(signed).is_ok(),
                "AndImm {imm:#x} exceeds u32: ISel should emit LoadImm + And instead"
            );
            let d64 = preg_to_reg64(dst);
            if dst != src {
                asm.mov(d64, preg_to_reg64(src))?;
            }
            if imm != u64::MAX {
                asm.and(d64, signed as i32)?;
            }
        }
    }
    Ok(())
}

pub(super) fn emit_and_memory_imm(
    asm: &mut CodeAssembler,
    dst: PhysReg,
    memory: AsmMemoryOperand,
    imm: u64,
) -> Result<(), IcedError> {
    let d32 = preg_to_reg32(dst);
    match imm {
        0 => asm.xor(d32, d32)?,
        0xff => asm.movzx(d32, byte_ptr(memory))?,
        0xffff => asm.movzx(d32, word_ptr(memory))?,
        0xffff_ffff => asm.mov(d32, dword_ptr(memory))?,
        _ => {
            // Only the low bytes contribute to a zero-extended u32 mask.
            if imm <= u64::from(u32::MAX) {
                asm.mov(d32, dword_ptr(memory))?;
            } else {
                asm.mov(preg_to_reg64(dst), qword_ptr(memory))?;
            }
            emit_and_imm(asm, dst, dst, imm)?;
        }
    }
    Ok(())
}
