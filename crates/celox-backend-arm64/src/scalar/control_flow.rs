//! Branch predicates, conditional branches, and edge copies.

use super::*;

pub(super) fn emit_branch_predicate(
    ops: &mut VecAssembler<Aarch64Relocation>,
    predicate: BranchPredicate,
    assignment: &Assignment<VReg>,
    state_pages: StatePageBases,
) -> Result<(), EmitError> {
    match predicate {
        BranchPredicate::Compare { lhs, rhs, .. } => {
            let (lhs, rhs) = (resolve(assignment, lhs)?, resolve(assignment, rhs)?);
            dynasm!(ops ; .arch aarch64 ; cmp X(lhs), X(rhs));
        }
        BranchPredicate::CompareImm { lhs, imm, .. } => {
            let lhs = resolve(assignment, lhs)?;
            if !emit_cmp_immediate(ops, lhs, imm) {
                emit_load_imm(ops, SCRATCH0, imm as i64 as u64);
                dynasm!(ops ; .arch aarch64 ; cmp X(lhs), x16);
            }
        }
        BranchPredicate::MemoryNonZero { base, offset, size } => {
            let offset = base_offset(base, offset);
            let (base_register, offset) =
                select_memory_base(base, offset, SCRATCH0, size, false, state_pages);
            emit_load_at(ops, SCRATCH0, base_register, offset, size);
            dynasm!(ops ; .arch aarch64 ; cmp x16, #0);
        }
    }
    Ok(())
}

pub(super) fn inverse_condition(kind: CmpKind) -> CmpKind {
    match kind {
        CmpKind::Eq => CmpKind::Ne,
        CmpKind::Ne => CmpKind::Eq,
        CmpKind::LtU => CmpKind::GeU,
        CmpKind::LeU => CmpKind::GtU,
        CmpKind::GtU => CmpKind::LeU,
        CmpKind::GeU => CmpKind::LtU,
        CmpKind::LtS => CmpKind::GeS,
        CmpKind::LeS => CmpKind::GtS,
        CmpKind::GtS => CmpKind::LeS,
        CmpKind::GeS => CmpKind::LtS,
    }
}

pub(super) fn predicate_kind(predicate: BranchPredicate) -> CmpKind {
    match predicate {
        BranchPredicate::Compare { kind, .. } | BranchPredicate::CompareImm { kind, .. } => kind,
        BranchPredicate::MemoryNonZero { .. } => CmpKind::Ne,
    }
}

pub(super) fn direct_branch_side(
    plan: &EdgeCopyPlan<BlockId>,
    block: BlockId,
    true_bb: BlockId,
    false_bb: BlockId,
    true_is_next: bool,
) -> Option<bool> {
    let prefer_true = !true_is_next;
    [prefer_true, !prefer_true].into_iter().find(|&on_true| {
        let target = if on_true { true_bb } else { false_bb };
        plan.edge(block, target)
            .is_none_or(|copies| copies.is_empty())
    })
}

pub(super) fn emit_conditional_branch(
    ops: &mut VecAssembler<Aarch64Relocation>,
    label: DynamicLabel,
    kind: CmpKind,
) {
    match kind {
        CmpKind::Eq => dynasm!(ops ; .arch aarch64 ; b.eq =>label),
        CmpKind::Ne => dynasm!(ops ; .arch aarch64 ; b.ne =>label),
        CmpKind::LtU => dynasm!(ops ; .arch aarch64 ; b.lo =>label),
        CmpKind::LtS => dynasm!(ops ; .arch aarch64 ; b.lt =>label),
        CmpKind::LeU => dynasm!(ops ; .arch aarch64 ; b.ls =>label),
        CmpKind::LeS => dynasm!(ops ; .arch aarch64 ; b.le =>label),
        CmpKind::GtU => dynasm!(ops ; .arch aarch64 ; b.hi =>label),
        CmpKind::GtS => dynasm!(ops ; .arch aarch64 ; b.gt =>label),
        CmpKind::GeU => dynasm!(ops ; .arch aarch64 ; b.hs =>label),
        CmpKind::GeS => dynasm!(ops ; .arch aarch64 ; b.ge =>label),
    }
}

pub(super) fn emit_edge_copies(
    ops: &mut VecAssembler<Aarch64Relocation>,
    plan: &EdgeCopyPlan<BlockId>,
    predecessor: BlockId,
    successor: BlockId,
    spill_base: usize,
    temporary_offset: usize,
) -> Result<(), EmitError> {
    let Some(operations) = plan.edge(predecessor, successor) else {
        return Ok(());
    };
    for operation in operations {
        match *operation {
            CopyOperation::Move {
                destination,
                source,
            } => emit_copy(ops, destination, source)?,
            CopyOperation::SwapRegisters { left, right } => {
                let (left, right) = (left.number(), right.number());
                dynasm!(ops
                    ; .arch aarch64
                    ; mov x16, X(left)
                    ; mov X(left), X(right)
                    ; mov X(right), x16
                );
            }
            CopyOperation::SaveTemporary(destination) => {
                read_copy_destination(ops, destination, SCRATCH1)?;
                // Address materialization uses x17 for the offset. Preserve
                // the value being saved before computing the temporary slot.
                dynasm!(ops ; .arch aarch64 ; mov x30, x17);
                let temporary = temporary_offset
                    .checked_sub(spill_base)
                    .ok_or(EmitError::Range("temporary spill offset underflow"))?;
                emit_store_at(
                    ops,
                    30,
                    SPILL_REG,
                    i64::try_from(temporary)
                        .map_err(|_| EmitError::Range("temporary spill offset overflow"))?,
                    OpSize::S64,
                );
            }
            CopyOperation::RestoreTemporary(destination) => {
                let temporary = temporary_offset
                    .checked_sub(spill_base)
                    .ok_or(EmitError::Range("temporary spill offset underflow"))?;
                emit_load_at(
                    ops,
                    SCRATCH1,
                    SPILL_REG,
                    i64::try_from(temporary)
                        .map_err(|_| EmitError::Range("temporary spill offset overflow"))?,
                    OpSize::S64,
                );
                write_copy_destination(ops, destination, SCRATCH1)?;
            }
        }
    }
    Ok(())
}

pub(super) fn emit_copy(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: CopyDestination,
    source: CopySource,
) -> Result<(), EmitError> {
    match source {
        CopySource::Register(register) => {
            write_copy_destination(ops, destination, register.number())
        }
        CopySource::Stack(offset) => {
            emit_load_at(ops, SCRATCH1, SPILL_REG, i64::from(offset), OpSize::S64);
            write_copy_destination(ops, destination, SCRATCH1)
        }
        CopySource::Immediate(value) => {
            emit_load_imm(ops, SCRATCH1, value);
            write_copy_destination(ops, destination, SCRATCH1)
        }
    }
}

pub(super) fn read_copy_destination(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: CopyDestination,
    output: u8,
) -> Result<(), EmitError> {
    match destination {
        CopyDestination::Register(register) => {
            let register = register.number();
            dynasm!(ops ; .arch aarch64 ; mov X(output), X(register));
        }
        CopyDestination::Stack(offset) => {
            emit_load_at(ops, output, SPILL_REG, i64::from(offset), OpSize::S64);
        }
    }
    Ok(())
}

pub(super) fn write_copy_destination(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: CopyDestination,
    source: u8,
) -> Result<(), EmitError> {
    match destination {
        CopyDestination::Register(register) => {
            let register = register.number();
            dynasm!(ops ; .arch aarch64 ; mov X(register), X(source));
        }
        CopyDestination::Stack(offset) => {
            let source = if source == SCRATCH1 {
                // emit_address reserves x17 for large/immediate offsets.
                // Stack-to-stack and temporary restores arrive in x17, so
                // retain their payload in the other fixed scratch register.
                dynasm!(ops ; .arch aarch64 ; mov x30, x17);
                30
            } else {
                source
            };
            emit_store_at(ops, source, SPILL_REG, i64::from(offset), OpSize::S64);
        }
    }
    Ok(())
}
