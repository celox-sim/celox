//! MIR instruction dispatch and machine instruction encoding.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_instruction(
    ops: &mut VecAssembler<Aarch64Relocation>,
    instruction: &MInst,
    block: BlockId,
    next_block: Option<BlockId>,
    assignment: &Assignment<VReg>,
    spill_base: usize,
    temporary_offset: usize,
    plan: &EdgeCopyPlan<BlockId>,
    labels: &HashMap<BlockId, DynamicLabel>,
    table_labels: &[DynamicLabel],
    constant_tables: &[Vec<u64>],
    epilogue: DynamicLabel,
    tick_entry: Option<DynamicLabel>,
    tick_success: Option<DynamicLabel>,
    check_runtime_events: bool,
    state_pages: StatePageBases,
    tick_counter_in_fp: bool,
    direct_branches: bool,
) -> Result<(), EmitError> {
    let is_next = |target| next_block.is_some_and(|next| labels[&next] == labels[&target]);
    match instruction {
        MInst::CallExtern { .. } => unreachable!("the block loop emits extern calls"),
        MInst::FloatBinary {
            dst,
            lhs,
            rhs,
            op,
            precision,
        } => {
            use celox_sir::{FloatBinaryOp as F, FloatPrecision as P};
            let (dst, lhs, rhs) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
            );
            // v0/v1 are caller-clobbered scratch; persistent loop state uses v29/v31.
            match precision {
                P::F32 => {
                    dynasm!(ops ; .arch aarch64 ; fmov s0, W(lhs) ; fmov s1, W(rhs));
                    match op {
                        F::Add => dynasm!(ops ; .arch aarch64 ; fadd s0, s0, s1),
                        F::Sub => dynasm!(ops ; .arch aarch64 ; fsub s0, s0, s1),
                        F::Mul => dynasm!(ops ; .arch aarch64 ; fmul s0, s0, s1),
                        F::Div => dynasm!(ops ; .arch aarch64 ; fdiv s0, s0, s1),
                        _ => dynasm!(ops ; .arch aarch64 ; fcmp s0, s1),
                    }
                }
                P::F64 => {
                    dynasm!(ops ; .arch aarch64 ; fmov d0, X(lhs) ; fmov d1, X(rhs));
                    match op {
                        F::Add => dynasm!(ops ; .arch aarch64 ; fadd d0, d0, d1),
                        F::Sub => dynasm!(ops ; .arch aarch64 ; fsub d0, d0, d1),
                        F::Mul => dynasm!(ops ; .arch aarch64 ; fmul d0, d0, d1),
                        F::Div => dynasm!(ops ; .arch aarch64 ; fdiv d0, d0, d1),
                        _ => dynasm!(ops ; .arch aarch64 ; fcmp d0, d1),
                    }
                }
            }
            match op {
                F::Eq => dynasm!(ops ; .arch aarch64 ; cset X(dst), eq),
                F::Ne => dynasm!(ops ; .arch aarch64 ; cset X(dst), ne),
                // FCMP unordered has NZCV=0011: MI/LS/GT/GE all reject NaN.
                F::Lt => dynasm!(ops ; .arch aarch64 ; cset X(dst), mi),
                F::Le => dynasm!(ops ; .arch aarch64 ; cset X(dst), ls),
                F::Gt => dynasm!(ops ; .arch aarch64 ; cset X(dst), gt),
                F::Ge => dynasm!(ops ; .arch aarch64 ; cset X(dst), ge),
                F::Add | F::Sub | F::Mul | F::Div => match precision {
                    P::F32 => dynasm!(ops ; .arch aarch64 ; fmov W(dst), s0),
                    P::F64 => dynasm!(ops ; .arch aarch64 ; fmov X(dst), d0),
                },
                F::Rem | F::Pow | F::Atan2 | F::Hypot => {
                    unreachable!("complex float operations use helpers")
                }
            }
        }
        MInst::FloatUnary {
            dst,
            src,
            op,
            precision,
        } => {
            use celox_sir::{FloatPrecision as P, FloatUnaryOp as F};
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            match precision {
                P::F32 => dynasm!(ops ; .arch aarch64 ; fmov s0, W(src)),
                P::F64 => dynasm!(ops ; .arch aarch64 ; fmov d0, X(src)),
            }
            match (op, precision) {
                (F::Neg, P::F32) => dynasm!(ops ; .arch aarch64 ; fneg s0, s0),
                (F::Neg, P::F64) => dynasm!(ops ; .arch aarch64 ; fneg d0, d0),
                (F::ToBool, P::F32) => dynasm!(ops ; .arch aarch64 ; fcmp s0, #0.0),
                (F::ToBool, P::F64) => dynasm!(ops ; .arch aarch64 ; fcmp d0, #0.0),
                (F::Convert { to: P::F64 }, P::F32) => dynasm!(ops ; .arch aarch64 ; fcvt d0, s0),
                (F::Convert { to: P::F32 }, P::F64) => dynasm!(ops ; .arch aarch64 ; fcvt s0, d0),
                (F::Convert { .. }, _) => {}
                _ => unreachable!("integer float conversions use helpers"),
            }
            match op {
                F::ToBool => dynasm!(ops ; .arch aarch64 ; cset X(dst), ne),
                _ => match match op {
                    F::Convert { to } => to,
                    _ => precision,
                } {
                    P::F32 => dynasm!(ops ; .arch aarch64 ; fmov W(dst), s0),
                    P::F64 => dynasm!(ops ; .arch aarch64 ; fmov X(dst), d0),
                },
            }
        }
        MInst::Mov { dst, src } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            if dst != src {
                dynasm!(ops ; .arch aarch64 ; mov X(dst), X(src));
            }
        }
        MInst::Mov32 { dst, src } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            dynasm!(ops ; .arch aarch64 ; mov W(dst), W(src));
        }
        MInst::LoadImm { dst, value } => emit_load_imm(ops, resolve(assignment, *dst)?, *value),
        MInst::BitExtract {
            dst,
            src,
            lsb,
            width,
        } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            let (lsb, width) = (u32::from(*lsb), u32::from(*width));
            dynasm!(ops ; .arch aarch64 ; ubfx X(dst), X(src), lsb, width);
        }
        MInst::BitInsert {
            dst,
            base,
            src,
            lsb,
            width,
        } => {
            let (dst, base, src) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *base)?,
                resolve(assignment, *src)?,
            );
            emit_bit_insert(ops, dst, base, src, *lsb, *width);
        }
        MInst::OrShifted {
            dst,
            lhs,
            rhs,
            shift,
        } => {
            let (dst, lhs, rhs) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
            );
            let shift = u32::from(*shift);
            dynasm!(ops ; .arch aarch64 ; orr X(dst), X(lhs), X(rhs), LSL #shift);
        }
        MInst::KeepAlive { .. } => {}
        MInst::LoadConstantTableAddr { dst, table } => {
            let dst = resolve(assignment, *dst)?;
            let label = *table_labels
                .get(table.0)
                .ok_or(EmitError::Range("constant table identity out of range"))?;
            dynasm!(ops ; .arch aarch64 ; adr X(dst), =>label);
        }
        MInst::Load {
            dst,
            base,
            offset,
            size,
        } => {
            let offset = base_offset(*base, *offset);
            let destination = resolve(assignment, *dst)?;
            let (base_register, offset) =
                select_memory_base(*base, offset, destination, *size, false, state_pages);
            emit_load_at(ops, destination, base_register, offset, *size);
        }
        MInst::Store {
            base,
            offset,
            src,
            size,
        } => {
            let offset = base_offset(*base, *offset);
            let source = resolve(assignment, *src)?;
            let (base_register, offset) =
                select_memory_base(*base, offset, source, *size, true, state_pages);
            emit_store_at(ops, source, base_register, offset, *size);
        }
        MInst::LoadPtr {
            dst,
            ptr,
            offset,
            size,
        } => {
            emit_load_at(
                ops,
                resolve(assignment, *dst)?,
                resolve(assignment, *ptr)?,
                i64::from(*offset),
                *size,
            );
        }
        MInst::StorePtr {
            ptr,
            offset,
            src,
            size,
        } => {
            emit_store_at(
                ops,
                resolve(assignment, *src)?,
                resolve(assignment, *ptr)?,
                i64::from(*offset),
                *size,
            );
        }
        MInst::ReleaseStorePtr {
            ptr,
            offset,
            src,
            size,
        } => {
            emit_address(ops, resolve(assignment, *ptr)?, i64::from(*offset));
            emit_release_store(ops, resolve(assignment, *src)?, SCRATCH0, *size);
        }
        MInst::LoadIndexed {
            dst,
            base,
            offset,
            index,
            scale,
            size,
            ..
        } => {
            let index = resolve(assignment, *index)?;
            let destination = resolve(assignment, *dst)?;
            let (base, offset) =
                select_indexed_memory_base(*base, i64::from(*offset), *size, false, state_pages);
            emit_load_indexed_at(ops, destination, base, index, offset, *scale, *size);
        }
        MInst::StoreIndexed {
            base,
            offset,
            index,
            src,
            size,
            ..
        }
        | MInst::OrStoreIndexed {
            base,
            offset,
            index,
            src,
            size,
            ..
        } => {
            let index = resolve(assignment, *index)?;
            if matches!(instruction, MInst::OrStoreIndexed { .. }) {
                let src = resolve(assignment, *src)?;
                let (base, offset) = select_indexed_memory_base(
                    *base,
                    i64::from(*offset),
                    *size,
                    false,
                    state_pages,
                );
                if offset == 0 {
                    emit_load_indexed_at(ops, SCRATCH1, base, index, 0, 1, *size);
                    dynasm!(ops ; .arch aarch64 ; orr x17, x17, X(src));
                    emit_store_indexed_at(ops, SCRATCH1, base, index, 0, 1, *size);
                } else {
                    dynasm!(ops ; .arch aarch64 ; add x16, X(base), X(index));
                    emit_add_offset(ops, offset);
                    emit_load(ops, SCRATCH1, SCRATCH0, *size);
                    dynasm!(ops ; .arch aarch64 ; orr x17, x17, X(src));
                    emit_store(ops, SCRATCH1, SCRATCH0, *size);
                }
            } else {
                let src = resolve(assignment, *src)?;
                let (base, offset) =
                    select_indexed_memory_base(*base, i64::from(*offset), *size, true, state_pages);
                emit_store_indexed_at(ops, src, base, index, offset, 1, *size);
            }
        }
        MInst::LoadPtrIndexed {
            dst,
            ptr,
            offset,
            index,
            size,
        } => {
            let (ptr, index) = (resolve(assignment, *ptr)?, resolve(assignment, *index)?);
            emit_load_indexed_at(
                ops,
                resolve(assignment, *dst)?,
                ptr,
                index,
                i64::from(*offset),
                1,
                *size,
            );
        }
        MInst::StorePtrIndexed {
            ptr,
            offset,
            index,
            src,
            size,
        }
        | MInst::ReleaseStorePtrIndexed {
            ptr,
            offset,
            index,
            src,
            size,
        } => {
            let (ptr, index) = (resolve(assignment, *ptr)?, resolve(assignment, *index)?);
            let src = resolve(assignment, *src)?;
            if matches!(instruction, MInst::ReleaseStorePtrIndexed { .. }) {
                dynasm!(ops ; .arch aarch64 ; add x16, X(ptr), X(index));
                emit_add_offset(ops, i64::from(*offset));
                emit_release_store(ops, src, SCRATCH0, *size);
            } else {
                emit_store_indexed_at(ops, src, ptr, index, i64::from(*offset), 1, *size);
            }
        }
        MInst::Add { dst, lhs, rhs }
        | MInst::Sub { dst, lhs, rhs }
        | MInst::Mul { dst, lhs, rhs }
        | MInst::And { dst, lhs, rhs }
        | MInst::Or { dst, lhs, rhs }
        | MInst::Xor { dst, lhs, rhs }
        | MInst::Shr { dst, lhs, rhs }
        | MInst::Shl { dst, lhs, rhs }
        | MInst::Sar { dst, lhs, rhs } => {
            let (dst, lhs, rhs) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
            );
            match instruction {
                MInst::Add { .. } => dynasm!(ops ; .arch aarch64 ; add X(dst), X(lhs), X(rhs)),
                MInst::Sub { .. } => dynasm!(ops ; .arch aarch64 ; sub X(dst), X(lhs), X(rhs)),
                MInst::Mul { .. } => dynasm!(ops ; .arch aarch64 ; mul X(dst), X(lhs), X(rhs)),
                MInst::And { .. } => dynasm!(ops ; .arch aarch64 ; and X(dst), X(lhs), X(rhs)),
                MInst::Or { .. } => dynasm!(ops ; .arch aarch64 ; orr X(dst), X(lhs), X(rhs)),
                MInst::Xor { .. } => dynasm!(ops ; .arch aarch64 ; eor X(dst), X(lhs), X(rhs)),
                MInst::Shr { .. } => dynasm!(ops ; .arch aarch64 ; lsrv X(dst), X(lhs), X(rhs)),
                MInst::Shl { .. } => dynasm!(ops ; .arch aarch64 ; lslv X(dst), X(lhs), X(rhs)),
                MInst::Sar { .. } => dynasm!(ops ; .arch aarch64 ; asrv X(dst), X(lhs), X(rhs)),
                _ => unreachable!(),
            }
        }
        MInst::Add32 { dst, lhs, rhs }
        | MInst::Sub32 { dst, lhs, rhs }
        | MInst::Mul32 { dst, lhs, rhs }
        | MInst::And32 { dst, lhs, rhs }
        | MInst::Or32 { dst, lhs, rhs }
        | MInst::Xor32 { dst, lhs, rhs } => {
            let (dst, lhs, rhs) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
            );
            match instruction {
                MInst::Add32 { .. } => dynasm!(ops ; .arch aarch64 ; add W(dst), W(lhs), W(rhs)),
                MInst::Sub32 { .. } => dynasm!(ops ; .arch aarch64 ; sub W(dst), W(lhs), W(rhs)),
                MInst::Mul32 { .. } => dynasm!(ops ; .arch aarch64 ; mul W(dst), W(lhs), W(rhs)),
                MInst::And32 { .. } => dynasm!(ops ; .arch aarch64 ; and W(dst), W(lhs), W(rhs)),
                MInst::Or32 { .. } => dynasm!(ops ; .arch aarch64 ; orr W(dst), W(lhs), W(rhs)),
                MInst::Xor32 { .. } => dynasm!(ops ; .arch aarch64 ; eor W(dst), W(lhs), W(rhs)),
                _ => unreachable!(),
            }
        }
        MInst::UMulHi { dst, lhs, rhs } => {
            let (dst, lhs, rhs) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
            );
            dynasm!(ops ; .arch aarch64 ; umulh X(dst), X(lhs), X(rhs));
        }
        MInst::AndImm { dst, src, imm } | MInst::OrImm { dst, src, imm } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            let is_and = matches!(instruction, MInst::AndImm { .. });
            if !emit_logical_immediate(ops, dst, src, *imm, 64, is_and) {
                emit_load_imm(ops, SCRATCH0, *imm);
                if is_and {
                    dynasm!(ops ; .arch aarch64 ; and X(dst), X(src), x16);
                } else {
                    dynasm!(ops ; .arch aarch64 ; orr X(dst), X(src), x16);
                }
            }
        }
        MInst::AndImm32 { dst, src, imm } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            if !emit_logical_immediate(ops, dst, src, u64::from(*imm), 32, true) {
                emit_load_imm(ops, SCRATCH0, u64::from(*imm));
                dynasm!(ops ; .arch aarch64 ; and W(dst), W(src), w16);
            }
        }
        MInst::ShrImm { dst, src, imm }
        | MInst::ShlImm { dst, src, imm }
        | MInst::SarImm { dst, src, imm } => {
            let (dst, src, imm) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *src)?,
                u32::from(*imm),
            );
            match instruction {
                MInst::ShrImm { .. } => dynasm!(ops ; .arch aarch64 ; lsr X(dst), X(src), #imm),
                MInst::ShlImm { .. } => dynasm!(ops ; .arch aarch64 ; lsl X(dst), X(src), #imm),
                MInst::SarImm { .. } => dynasm!(ops ; .arch aarch64 ; asr X(dst), X(src), #imm),
                _ => unreachable!(),
            }
        }
        MInst::AddImm { dst, src, imm } | MInst::SubImm { dst, src, imm } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            let offset = if matches!(instruction, MInst::SubImm { .. }) {
                -i64::from(*imm)
            } else {
                i64::from(*imm)
            };
            if !emit_add_sub_immediate(ops, dst, src, offset) {
                emit_load_imm(ops, SCRATCH0, offset.unsigned_abs());
                if offset < 0 {
                    dynasm!(ops ; .arch aarch64 ; sub X(dst), X(src), x16);
                } else {
                    dynasm!(ops ; .arch aarch64 ; add X(dst), X(src), x16);
                }
            }
        }
        MInst::Cmp {
            dst,
            lhs,
            rhs,
            kind,
        } => {
            let (dst, lhs, rhs) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
            );
            dynasm!(ops ; .arch aarch64 ; cmp X(lhs), X(rhs));
            emit_cset(ops, dst, *kind);
        }
        MInst::CmpImm {
            dst,
            lhs,
            imm,
            kind,
        } => {
            let (dst, lhs) = (resolve(assignment, *dst)?, resolve(assignment, *lhs)?);
            if !emit_cmp_immediate(ops, lhs, *imm) {
                emit_load_imm(ops, SCRATCH0, *imm as i64 as u64);
                dynasm!(ops ; .arch aarch64 ; cmp X(lhs), x16);
            }
            emit_cset(ops, dst, *kind);
        }
        MInst::UDiv { dst, lhs, rhs }
        | MInst::URem { dst, lhs, rhs }
        | MInst::SDiv { dst, lhs, rhs }
        | MInst::SRem { dst, lhs, rhs } => {
            let (dst, lhs, rhs) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
            );
            if matches!(instruction, MInst::UDiv { .. } | MInst::URem { .. }) {
                dynasm!(ops ; .arch aarch64 ; udiv x16, X(lhs), X(rhs));
            } else {
                dynasm!(ops ; .arch aarch64 ; sdiv x16, X(lhs), X(rhs));
            }
            if matches!(instruction, MInst::URem { .. } | MInst::SRem { .. }) {
                dynasm!(ops ; .arch aarch64 ; msub X(dst), x16, X(rhs), X(lhs));
            } else {
                dynasm!(ops ; .arch aarch64 ; mov X(dst), x16);
            }
        }
        MInst::BitNot { dst, src } | MInst::Neg { dst, src } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            if matches!(instruction, MInst::BitNot { .. }) {
                dynasm!(ops ; .arch aarch64 ; mvn X(dst), X(src));
            } else {
                dynasm!(ops ; .arch aarch64 ; neg X(dst), X(src));
            }
        }
        MInst::Popcnt { dst, src } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            dynasm!(ops
                ; .arch aarch64
                ; fmov d0, X(src)
                ; cnt v0.b8, v0.b8
                ; addv b0, v0.b8
                ; umov W(dst), v0.b[0]
            );
        }
        MInst::Bsf { dst, src } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            dynasm!(ops ; .arch aarch64 ; rbit x16, X(src) ; clz X(dst), x16);
        }
        MInst::Bsr { dst, src } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            dynasm!(ops ; .arch aarch64 ; clz x16, X(src));
            emit_load_imm(ops, SCRATCH1, 63);
            dynasm!(ops ; .arch aarch64 ; sub x16, x17, x16);
            dynasm!(ops ; .arch aarch64 ; mov X(dst), x16);
        }
        MInst::Select {
            dst,
            cond,
            true_val,
            false_val,
        } => {
            let (dst, cond, true_val, false_val) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *cond)?,
                resolve(assignment, *true_val)?,
                resolve(assignment, *false_val)?,
            );
            dynasm!(ops
                ; .arch aarch64
                ; cmp XSP(cond), #0
                ; csel X(dst), X(true_val), X(false_val), ne
            );
        }
        MInst::CmpSelect {
            dst,
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
        } => {
            let (dst, lhs, rhs, true_val, false_val) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
                resolve(assignment, *true_val)?,
                resolve(assignment, *false_val)?,
            );
            dynasm!(ops ; .arch aarch64 ; cmp X(lhs), X(rhs));
            emit_csel(ops, dst, true_val, false_val, *kind);
        }
        MInst::CmpImmSelect {
            dst,
            lhs,
            imm,
            kind,
            true_val,
            false_val,
        } => {
            let (dst, lhs, true_val, false_val) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *true_val)?,
                resolve(assignment, *false_val)?,
            );
            if !emit_cmp_immediate(ops, lhs, *imm) {
                emit_load_imm(ops, SCRATCH0, *imm as i64 as u64);
                dynasm!(ops ; .arch aarch64 ; cmp X(lhs), x16);
            }
            emit_csel(ops, dst, true_val, false_val, *kind);
        }
        MInst::JumpTable { index, targets, .. } => {
            let Some((&default_target, compared_targets)) = targets.split_last() else {
                return Err(EmitError::Range("jump table has no targets"));
            };
            let index = resolve(assignment, *index)?;
            let paths = compared_targets
                .iter()
                .map(|_| ops.new_dynamic_label())
                .collect::<Vec<_>>();
            for (table_index, path) in paths.iter().copied().enumerate() {
                emit_load_imm(ops, SCRATCH0, table_index as u64);
                dynasm!(ops ; .arch aarch64 ; cmp X(index), x16 ; b.eq =>path);
            }
            emit_edge_copies(
                ops,
                plan,
                block,
                default_target,
                spill_base,
                temporary_offset,
            )?;
            let default_label = labels[&default_target];
            dynasm!(ops ; .arch aarch64 ; b =>default_label);
            for (&target, path) in compared_targets.iter().zip(paths) {
                dynasm!(ops ; .arch aarch64 ; =>path);
                emit_edge_copies(ops, plan, block, target, spill_base, temporary_offset)?;
                let target_label = labels[&target];
                dynasm!(ops ; .arch aarch64 ; b =>target_label);
            }
        }
        MInst::Branch {
            cond,
            true_bb,
            false_bb,
        } => {
            if direct_branches
                && let Some(branch_on_true) =
                    direct_branch_side(plan, block, *true_bb, *false_bb, is_next(*true_bb))
            {
                let (target, fallthrough) = if branch_on_true {
                    (*true_bb, *false_bb)
                } else {
                    (*false_bb, *true_bb)
                };
                let cond = resolve(assignment, *cond)?;
                let target_label = labels[&target];
                if branch_on_true {
                    dynasm!(ops ; .arch aarch64 ; cbnz X(cond), =>target_label);
                } else {
                    dynasm!(ops ; .arch aarch64 ; cbz X(cond), =>target_label);
                }
                emit_edge_copies(ops, plan, block, fallthrough, spill_base, temporary_offset)?;
                if !is_next(fallthrough) {
                    let label = labels[&fallthrough];
                    dynasm!(ops ; .arch aarch64 ; b =>label);
                }
                return Ok(());
            }
            // Put the physically adjacent successor last so its copies can
            // fall through. The conditional branch still targets a nearby
            // copy stub, preserving its range even in very large functions.
            let invert = is_next(*false_bb);
            let (true_bb, false_bb) = if invert {
                (false_bb, true_bb)
            } else {
                (true_bb, false_bb)
            };
            let true_path = ops.new_dynamic_label();
            let cond = resolve(assignment, *cond)?;
            if invert {
                dynasm!(ops ; .arch aarch64 ; cbz X(cond), =>true_path);
            } else {
                dynasm!(ops ; .arch aarch64 ; cbnz X(cond), =>true_path);
            }
            emit_edge_copies(ops, plan, block, *false_bb, spill_base, temporary_offset)?;
            let false_label = labels[false_bb];
            dynasm!(ops ; .arch aarch64 ; b =>false_label ; =>true_path);
            emit_edge_copies(ops, plan, block, *true_bb, spill_base, temporary_offset)?;
            let true_label = labels[true_bb];
            if !is_next(*true_bb) {
                dynasm!(ops ; .arch aarch64 ; b =>true_label);
            }
        }
        MInst::BranchPred {
            predicate,
            true_bb,
            false_bb,
        } => {
            emit_branch_predicate(ops, *predicate, assignment, state_pages)?;
            if direct_branches
                && let Some(branch_on_true) =
                    direct_branch_side(plan, block, *true_bb, *false_bb, is_next(*true_bb))
            {
                let (target, fallthrough) = if branch_on_true {
                    (*true_bb, *false_bb)
                } else {
                    (*false_bb, *true_bb)
                };
                let kind = predicate_kind(*predicate);
                emit_conditional_branch(
                    ops,
                    labels[&target],
                    if branch_on_true {
                        kind
                    } else {
                        inverse_condition(kind)
                    },
                );
                emit_edge_copies(ops, plan, block, fallthrough, spill_base, temporary_offset)?;
                if !is_next(fallthrough) {
                    let label = labels[&fallthrough];
                    dynasm!(ops ; .arch aarch64 ; b =>label);
                }
                return Ok(());
            }
            let invert = is_next(*false_bb);
            let (true_bb, false_bb) = if invert {
                (false_bb, true_bb)
            } else {
                (true_bb, false_bb)
            };
            let true_path = ops.new_dynamic_label();
            let kind = predicate_kind(*predicate);
            emit_conditional_branch(
                ops,
                true_path,
                if invert {
                    inverse_condition(kind)
                } else {
                    kind
                },
            );
            emit_edge_copies(ops, plan, block, *false_bb, spill_base, temporary_offset)?;
            let false_label = labels[false_bb];
            dynasm!(ops ; .arch aarch64 ; b =>false_label ; =>true_path);
            emit_edge_copies(ops, plan, block, *true_bb, spill_base, temporary_offset)?;
            let true_label = labels[true_bb];
            if !is_next(*true_bb) {
                dynasm!(ops ; .arch aarch64 ; b =>true_label);
            }
        }
        MInst::Jump { target } => {
            emit_edge_copies(ops, plan, block, *target, spill_base, temporary_offset)?;
            let label = labels[target];
            if !is_next(*target) {
                dynasm!(ops ; .arch aarch64 ; b =>label);
            }
        }
        MInst::Return => {
            if let (Some(entry), Some(success)) = (tick_entry, tick_success) {
                if tick_counter_in_fp {
                    dynasm!(ops
                        ; .arch aarch64
                        ; fmov x16, d31
                        ; subs x16, x16, #1
                        ; fmov d31, x16
                        ; b.eq =>success
                    );
                } else {
                    dynasm!(ops
                        ; .arch aarch64
                        ; subs x29, x29, #1
                        ; b.eq =>success
                    );
                }
                if check_runtime_events {
                    emit_address(
                        ops,
                        STATE_REG,
                        STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET as i64,
                    );
                    emit_load(ops, SCRATCH0, SCRATCH0, OpSize::S64);
                    emit_load(ops, SCRATCH0, SCRATCH0, OpSize::S64);
                    dynasm!(ops ; .arch aarch64 ; mov x30, x16);
                    emit_address(
                        ops,
                        STATE_REG,
                        STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET as i64,
                    );
                    emit_load(ops, SCRATCH1, SCRATCH0, OpSize::S64);
                    dynasm!(ops ; .arch aarch64 ; cmp x30, x17 ; b.ne =>success);
                }
                dynasm!(ops ; .arch aarch64 ; b =>entry);
            } else {
                dynasm!(ops ; .arch aarch64 ; mov x0, xzr ; b =>epilogue);
            }
        }
        MInst::ReturnError { code } => {
            if tick_entry.is_some() {
                if tick_counter_in_fp {
                    dynasm!(ops
                        ; .arch aarch64
                        ; fmov x16, d31
                        ; sub x16, x16, #1
                        ; fmov d31, x16
                    );
                } else {
                    dynasm!(ops
                        ; .arch aarch64
                        ; sub x29, x29, #1
                    );
                }
            }
            emit_load_imm(ops, STATE_REG, *code as u64);
            dynasm!(ops ; .arch aarch64 ; b =>epilogue);
        }
        MInst::PackedLaneCompare {
            dst,
            rhs,
            kind,
            offset,
            lane_count,
            element_stride,
            bit_offset,
            field_width,
            ..
        } => emit_packed_lane_compare(
            ops,
            resolve(assignment, *dst)?,
            *rhs,
            *kind,
            *offset,
            *lane_count,
            *element_stride,
            *bit_offset,
            *field_width,
            assignment,
            state_pages,
        )?,
        MInst::PackedByteAffineCompare {
            dst,
            base,
            rhs,
            kind,
        } => emit_packed_byte_affine_compare(
            ops,
            resolve(assignment, *dst)?,
            resolve(assignment, *base)?,
            resolve(assignment, *rhs)?,
            *kind,
        ),
        MInst::MemCopy {
            src_offset,
            dst_offset,
            byte_len,
        } => emit_mem_copy(ops, *src_offset, *dst_offset, *byte_len, state_pages),
        MInst::MemFill {
            dst_offset,
            byte_len,
            value,
        } => emit_mem_fill(ops, *dst_offset, *byte_len, *value, state_pages),
        MInst::SparseCommit {
            src_offset,
            dst_offset,
            byte_size,
            dirty_words_offset,
            dirty_word_count,
            summary_words_offset,
            summary_word_count,
            four_state,
        } => emit_sparse_commit(
            ops,
            *src_offset,
            *dst_offset,
            *byte_size,
            *dirty_words_offset,
            *dirty_word_count,
            *summary_words_offset,
            *summary_word_count,
            *four_state,
            state_pages,
        ),
        MInst::SparseMarkActive {
            active_index,
            active_bits_offset,
            ..
        } => {
            let word_offset = i32::try_from((*active_index as usize / 64) * 8)
                .map_err(|_| EmitError::Range("sparse active bitmap offset exceeds i32"))?;
            let offset = active_bits_offset
                .checked_add(word_offset)
                .ok_or(EmitError::Range("sparse active bitmap offset overflow"))?;
            let offset = i64::from(offset);
            let (base, offset) = select_memory_base(
                BaseReg::SimState,
                offset,
                SCRATCH1,
                OpSize::S64,
                false,
                state_pages,
            );
            emit_load_at(ops, SCRATCH1, base, offset, OpSize::S64);
            ops.push_u32(
                logical_immediate_encoding(1_u64 << (*active_index % 64), 64, 30, SCRATCH1, false)
                    .unwrap(),
            );
            let (base, offset) = select_memory_base(
                BaseReg::SimState,
                i64::from(*active_bits_offset) + i64::from(word_offset),
                30,
                OpSize::S64,
                true,
                state_pages,
            );
            emit_store_at(ops, 30, base, offset, OpSize::S64);
        }
        MInst::SparseCommitWorklist {
            descriptor_table,
            active_bits_offset,
            active_capacity,
        } => emit_sparse_commit_worklist(
            ops,
            constant_tables
                .get(descriptor_table.0)
                .ok_or(EmitError::Range("sparse descriptor table is missing"))?,
            *active_bits_offset,
            *active_capacity,
            state_pages,
        )?,
        MInst::GuardedCmpSelect {
            dst,
            guard,
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
        } => {
            let (dst, guard, lhs, rhs, true_val, false_val) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *guard)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
                resolve(assignment, *true_val)?,
                resolve(assignment, *false_val)?,
            );
            // Materialize the comparison before testing the guard because both
            // operations write NZCV. x16 is reserved from the allocator.
            dynasm!(ops ; .arch aarch64 ; cmp X(lhs), X(rhs));
            emit_cset(ops, SCRATCH0, *kind);
            dynasm!(ops
                ; .arch aarch64
                ; cmp XSP(guard), #0
                ; csel x16, x16, xzr, ne
                ; cmp x16, #0
                ; csel X(dst), X(true_val), X(false_val), ne
            );
        }
    }
    Ok(())
}

pub(super) fn emit_bit_insert(
    ops: &mut VecAssembler<Aarch64Relocation>,
    dst: u8,
    base: u8,
    mut src: u8,
    lsb: u8,
    width: u8,
) {
    if dst != base {
        if dst == src {
            dynasm!(ops ; .arch aarch64 ; mov x16, X(src));
            src = SCRATCH0;
        }
        dynasm!(ops ; .arch aarch64 ; mov X(dst), X(base));
    }
    let (lsb, width) = (u32::from(lsb), u32::from(width));
    dynasm!(ops ; .arch aarch64 ; bfi X(dst), X(src), lsb, width);
}
