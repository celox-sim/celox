//! Instruction folding and memory-source operand emission.

use super::*;

pub(super) fn count_vreg_uses(func: &MFunction, plan: &SsaDestructionPlan) -> HashMap<VReg, usize> {
    let mut counts = HashMap::default();
    for edge in plan.edges() {
        for row in &edge.rows {
            *counts.entry(row.source_value).or_default() += 1;
        }
    }
    for block in &func.blocks {
        for inst in &block.insts {
            for vreg in inst.uses() {
                *counts.entry(vreg).or_default() += 1;
            }
        }
    }
    counts
}

pub(super) fn try_emit_not_and_fold(
    asm: &mut CodeAssembler,
    first: &MInst,
    second: &MInst,
    uses: &HashMap<VReg, usize>,
    assignment: &AssignmentMap,
) -> Result<bool, IcedError> {
    let MInst::BitNot { dst: inverted, src } = first else {
        return Ok(false);
    };
    if uses.get(inverted).copied() != Some(1) {
        return Ok(false);
    }
    let (dst, lhs, rhs, word32) = match *second {
        MInst::And { dst, lhs, rhs } => (dst, lhs, rhs, false),
        MInst::And32 { dst, lhs, rhs } => (dst, lhs, rhs, true),
        _ => return Ok(false),
    };
    let other = if lhs == *inverted {
        rhs
    } else if rhs == *inverted {
        lhs
    } else {
        return Ok(false);
    };
    let destination = resolve(assignment, dst);
    let source = resolve(assignment, *src);
    let other = resolve(assignment, other);
    if word32 {
        asm.andn(
            preg_to_reg32(destination),
            preg_to_reg32(source),
            preg_to_reg32(other),
        )?;
    } else {
        asm.andn(
            preg_to_reg64(destination),
            preg_to_reg64(source),
            preg_to_reg64(other),
        )?;
    }
    Ok(true)
}

pub(super) fn try_emit_load_fold(
    asm: &mut CodeAssembler,
    inst: &MInst,
    next: &MInst,
    use_counts: &HashMap<VReg, usize>,
    assignment: &AssignmentMap,
    func: &MFunction,
    spill_register_cache: SpillRegisterCache,
) -> Result<bool, IcedError> {
    let MInst::Load {
        dst,
        base,
        offset,
        size,
    } = inst
    else {
        return Ok(false);
    };
    if *base == BaseReg::StackFrame && spill_register_cache.register(*offset).is_some() {
        return Ok(false);
    }
    if use_counts.get(dst).copied().unwrap_or(0) != 1 || !next.uses().contains(dst) {
        return Ok(false);
    }
    let memory = mem_operand(*base, *offset);
    if *size == OpSize::S64 {
        return emit_inst_with_memory(asm, next, *dst, memory, assignment, func);
    }
    if let MInst::CmpImm {
        dst: result,
        lhs,
        imm,
        kind,
    } = *next
        && lhs == *dst
        && imm >= 0
        && (imm as u64) <= ((1u64 << (size.bytes() * 8)) - 1)
    {
        match size {
            OpSize::S8 => asm.cmp(byte_ptr(memory), imm)?,
            OpSize::S16 => asm.cmp(word_ptr(memory), imm)?,
            OpSize::S32 => asm.cmp(dword_ptr(memory), imm)?,
            OpSize::S64 => unreachable!(),
        }
        // The original narrow load zero-extends to 64 bits, so both operands
        // of a signed comparison against a nonnegative immediate are positive.
        let kind = match kind {
            CmpKind::LtS => CmpKind::LtU,
            CmpKind::LeS => CmpKind::LeU,
            CmpKind::GtS => CmpKind::GtU,
            CmpKind::GeS => CmpKind::GeU,
            other => other,
        };
        let d8 = preg_to_reg8(resolve(assignment, result));
        let d32 = preg_to_reg32(resolve(assignment, result));
        emit_setcc(asm, d8, kind)?;
        asm.movzx(d32, d8)?;
        return Ok(true);
    }
    Ok(false)
}

fn emit_inst_with_memory(
    asm: &mut CodeAssembler,
    inst: &MInst,
    memory_vreg: VReg,
    memory: AsmMemoryOperand,
    assignment: &AssignmentMap,
    _func: &MFunction,
) -> Result<bool, IcedError> {
    match inst {
        MInst::MulImm { dst, src, imm } if *src == memory_vreg => {
            asm.imul_3(
                preg_to_reg64(resolve(assignment, *dst)),
                qword_ptr(memory),
                *imm,
            )?;
            Ok(true)
        }
        MInst::MulImm32 { dst, src, imm } if *src == memory_vreg => {
            asm.imul_3(
                preg_to_reg32(resolve(assignment, *dst)),
                dword_ptr(memory),
                *imm,
            )?;
            Ok(true)
        }
        MInst::Mov { dst, src } if *src == memory_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(memory))?;
            Ok(true)
        }
        MInst::Mov32 { dst, src } if *src == memory_vreg => {
            let d = preg_to_reg32(resolve(assignment, *dst));
            asm.mov(d, dword_ptr(memory))?;
            Ok(true)
        }
        MInst::Add { dst, lhs, rhs } => emit_binop_memory(
            asm,
            assignment,
            BinOp::Add,
            false,
            *dst,
            *lhs,
            *rhs,
            memory_vreg,
            memory,
        ),
        MInst::Add32 { dst, lhs, rhs } => emit_binop_memory(
            asm,
            assignment,
            BinOp::Add,
            true,
            *dst,
            *lhs,
            *rhs,
            memory_vreg,
            memory,
        ),
        MInst::Sub { dst, lhs, rhs } => emit_binop_memory(
            asm,
            assignment,
            BinOp::Sub,
            false,
            *dst,
            *lhs,
            *rhs,
            memory_vreg,
            memory,
        ),
        MInst::Sub32 { dst, lhs, rhs } => emit_binop_memory(
            asm,
            assignment,
            BinOp::Sub,
            true,
            *dst,
            *lhs,
            *rhs,
            memory_vreg,
            memory,
        ),
        MInst::Mul { dst, lhs, rhs } => emit_binop_memory(
            asm,
            assignment,
            BinOp::Mul,
            false,
            *dst,
            *lhs,
            *rhs,
            memory_vreg,
            memory,
        ),
        MInst::Mul32 { dst, lhs, rhs } => emit_binop_memory(
            asm,
            assignment,
            BinOp::Mul,
            true,
            *dst,
            *lhs,
            *rhs,
            memory_vreg,
            memory,
        ),
        MInst::And { dst, lhs, rhs } => emit_binop_memory(
            asm,
            assignment,
            BinOp::And,
            false,
            *dst,
            *lhs,
            *rhs,
            memory_vreg,
            memory,
        ),
        MInst::And32 { dst, lhs, rhs } => emit_binop_memory(
            asm,
            assignment,
            BinOp::And,
            true,
            *dst,
            *lhs,
            *rhs,
            memory_vreg,
            memory,
        ),
        MInst::Or { dst, lhs, rhs } => emit_binop_memory(
            asm,
            assignment,
            BinOp::Or,
            false,
            *dst,
            *lhs,
            *rhs,
            memory_vreg,
            memory,
        ),
        MInst::Or32 { dst, lhs, rhs } => emit_binop_memory(
            asm,
            assignment,
            BinOp::Or,
            true,
            *dst,
            *lhs,
            *rhs,
            memory_vreg,
            memory,
        ),
        MInst::Xor { dst, lhs, rhs } => emit_binop_memory(
            asm,
            assignment,
            BinOp::Xor,
            false,
            *dst,
            *lhs,
            *rhs,
            memory_vreg,
            memory,
        ),
        MInst::Xor32 { dst, lhs, rhs } => emit_binop_memory(
            asm,
            assignment,
            BinOp::Xor,
            true,
            *dst,
            *lhs,
            *rhs,
            memory_vreg,
            memory,
        ),
        MInst::AndImm { dst, src, imm } if *src == memory_vreg => {
            emit_and_memory_imm(asm, resolve(assignment, *dst), memory, *imm)?;
            Ok(true)
        }
        MInst::AndImm32 { dst, src, imm } if *src == memory_vreg => {
            emit_and_memory_imm(asm, resolve(assignment, *dst), memory, u64::from(*imm))?;
            Ok(true)
        }
        MInst::OrImm { dst, src, imm } if *src == memory_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(memory))?;
            emit_or_imm64(asm, d, *imm)?;
            Ok(true)
        }
        MInst::AddImm { dst, src, imm } if *src == memory_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(memory))?;
            asm.add(d, *imm)?;
            Ok(true)
        }
        MInst::SubImm { dst, src, imm } if *src == memory_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(memory))?;
            asm.sub(d, *imm)?;
            Ok(true)
        }
        MInst::ShrImm { dst, src, imm } if *src == memory_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(memory))?;
            asm.shr(d, *imm as u32)?;
            Ok(true)
        }
        MInst::ShlImm { dst, src, imm } if *src == memory_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(memory))?;
            asm.shl(d, *imm as u32)?;
            Ok(true)
        }
        MInst::SarImm { dst, src, imm } if *src == memory_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(memory))?;
            asm.sar(d, *imm as u32)?;
            Ok(true)
        }
        MInst::Cmp {
            dst,
            lhs,
            rhs,
            kind,
        } if *lhs == memory_vreg || *rhs == memory_vreg => {
            let mem = qword_ptr(memory);
            if *lhs == memory_vreg {
                let r = preg_to_reg64(resolve(assignment, *rhs));
                asm.cmp(mem, r)?;
            } else {
                let l = preg_to_reg64(resolve(assignment, *lhs));
                asm.cmp(l, mem)?;
            }
            let d8 = preg_to_reg8(resolve(assignment, *dst));
            let d32 = preg_to_reg32(resolve(assignment, *dst));
            emit_setcc(asm, d8, *kind)?;
            asm.movzx(d32, d8)?;
            Ok(true)
        }
        MInst::CmpImm {
            dst,
            lhs,
            imm,
            kind,
        } if *lhs == memory_vreg => {
            asm.cmp(qword_ptr(memory), *imm)?;
            let d8 = preg_to_reg8(resolve(assignment, *dst));
            let d32 = preg_to_reg32(resolve(assignment, *dst));
            emit_setcc(asm, d8, *kind)?;
            asm.movzx(d32, d8)?;
            Ok(true)
        }
        MInst::BitNot { dst, src } if *src == memory_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(memory))?;
            asm.not(d)?;
            Ok(true)
        }
        MInst::Neg { dst, src } if *src == memory_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(memory))?;
            asm.neg(d)?;
            Ok(true)
        }
        MInst::Popcnt { dst, src } if *src == memory_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.popcnt(d, qword_ptr(memory))?;
            Ok(true)
        }
        MInst::Bsf { dst, src } if *src == memory_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.bsf(d, qword_ptr(memory))?;
            Ok(true)
        }
        MInst::Bsr { dst, src } if *src == memory_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.bsr(d, qword_ptr(memory))?;
            Ok(true)
        }
        MInst::Select {
            dst,
            cond,
            true_val,
            false_val,
        } => emit_select_memory(
            asm,
            assignment,
            *dst,
            *cond,
            *true_val,
            *false_val,
            memory_vreg,
            memory,
        ),
        _ => Ok(false),
    }
}
