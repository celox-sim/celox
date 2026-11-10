//! Mux lowering.

use super::*;

/// Lower `Mux(dst, cond, then_val, else_val)`.
pub(super) fn lower_mux(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    cond: &RegisterId,
    then_val: &RegisterId,
    else_val: &RegisterId,
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
) {
    let d_width = ctx.sir_width(dst);
    let (cond_is_true, cond_is_unknown) = lower_mux_condition_state(ctx, block, *cond);

    if d_width > 64 {
        let n_chunks = ISelContext::num_chunks(d_width);
        let tv_chunks = ctx.get_wide_chunks(then_val, block);
        let ev_chunks = ctx.get_wide_chunks(else_val, block);
        let zero_v = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero_v,
            value: 0,
        });
        if ctx.four_state {
            let tm_chunks = get_wide_mask_chunks(ctx, block, then_val, n_chunks);
            let em_chunks = get_wide_mask_chunks(ctx, block, else_val, n_chunks);
            let mut value_chunks = Vec::with_capacity(n_chunks);
            let mut mask_chunks = Vec::with_capacity(n_chunks);
            for i in 0..n_chunks {
                let tv = tv_chunks.get(i).map(|chunk| chunk.0).unwrap_or(zero_v);
                let ev = ev_chunks.get(i).map(|chunk| chunk.0).unwrap_or(zero_v);
                let tm = *tm_chunks.get(i).unwrap_or(&zero_v);
                let em = *em_chunks.get(i).unwrap_or(&zero_v);
                let chunk_width = (d_width - i * 64).min(64);
                let (value, mask) = lower_four_state_mux_chunk(
                    ctx,
                    block,
                    cond_is_true,
                    cond_is_unknown,
                    tv,
                    tm,
                    ev,
                    em,
                    chunk_width,
                );
                value_chunks.push((value, chunk_width));
                mask_chunks.push((mask, chunk_width));
            }
            ctx.set_wide_chunks(*dst, value_chunks);
            ctx.set_mask(*dst, mask_chunks[0].0);
            ctx.wide_masks.insert(*dst, mask_chunks);
        } else {
            let mut value_chunks = Vec::with_capacity(n_chunks);
            for i in 0..n_chunks {
                let tv = tv_chunks.get(i).map(|chunk| chunk.0).unwrap_or(zero_v);
                let ev = ev_chunks.get(i).map(|chunk| chunk.0).unwrap_or(zero_v);
                let chunk_width = (d_width - i * 64).min(64);
                let selected = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: selected,
                    cond: cond_is_true,
                    true_val: tv,
                    false_val: ev,
                });
                let value = if chunk_width < 64 {
                    let masked = ctx.alloc_vreg(SpillDesc::transient());
                    ctx.emit_and_imm(block, masked, selected, mask_for_width(chunk_width));
                    masked
                } else {
                    selected
                };
                value_chunks.push((value, chunk_width));
            }
            ctx.set_wide_chunks(*dst, value_chunks);
        }
    } else {
        let dst_vreg = ctx.reg_map.get(*dst);
        let tv = if ctx.wide_regs.contains_key(then_val) {
            ctx.get_wide_chunks(then_val, block)[0].0
        } else {
            ctx.reg_map.get(*then_val)
        };
        let ev = if ctx.wide_regs.contains_key(else_val) {
            ctx.get_wide_chunks(else_val, block)[0].0
        } else {
            ctx.reg_map.get(*else_val)
        };

        if !ctx.four_state
            && d_width <= 64
            && let Some(shift) =
                match_loaded_bit_select(sir_block, sir_defs, *cond, *then_val, *else_val)
        {
            // `cond ? 1 << k : 0` of a loaded two-state bit is the
            // bit shifted into place: a compare condition would have
            // to materialize first, so it keeps the compare + select.
            if shift == 0 {
                block.push(MInst::Mov {
                    dst: dst_vreg,
                    src: cond_is_true,
                });
            } else {
                block.push(MInst::ShlImm {
                    dst: dst_vreg,
                    src: cond_is_true,
                    imm: shift,
                });
            }
            ctx.known_bits.insert(dst_vreg, shift as usize + 1);
            return;
        }

        if !ctx.four_state
            && d_width <= 64
            && ctx
                .known_bits
                .get(&cond_is_true)
                .is_some_and(|&bits| bits <= 1)
            && !is_fusible_select_condition(sir_block, sir_defs, *cond)
            && let Some((kept, kept_when_set)) =
                if sir_imm_u64(sir_block, sir_defs, *then_val) == Some(0) {
                    Some((ev, false))
                } else if sir_imm_u64(sir_block, sir_defs, *else_val) == Some(0) {
                    Some((tv, true))
                } else {
                    None
                }
            && ctx.known_bits.get(&kept).copied().unwrap_or(64) <= d_width
        {
            // `cond ? 0 : v` of a materialized bit is `v & (cond - 1)`,
            // `cond ? v : 0` is `v & -cond`: one instruction per
            // select against a test and a conditional move, and the
            // mask is shared by every select on the same condition.
            let mask = ctx.alloc_vreg(SpillDesc::transient());
            if kept_when_set {
                block.push(MInst::Neg {
                    dst: mask,
                    src: cond_is_true,
                });
            } else {
                block.push(MInst::SubImm {
                    dst: mask,
                    src: cond_is_true,
                    imm: 1,
                });
            }
            block.push(MInst::And {
                dst: dst_vreg,
                lhs: kept,
                rhs: mask,
            });
            ctx.known_bits
                .insert(dst_vreg, ctx.known_bits.get(&kept).copied().unwrap_or(64));
            return;
        }

        if !ctx.four_state && d_width == 1 {
            let tv = lower_low_bit(ctx, block, tv);
            let ev = lower_low_bit(ctx, block, ev);

            block.push(MInst::Select {
                dst: dst_vreg,
                cond: cond_is_true,
                true_val: tv,
                false_val: ev,
            });
            ctx.known_bits.insert(dst_vreg, 1);
            return;
        }

        if !ctx.four_state
            && d_width <= 64
            && ctx.known_bits.get(&tv).copied().unwrap_or(64) <= d_width
            && ctx.known_bits.get(&ev).copied().unwrap_or(64) <= d_width
            && let Some((guard, lhs, rhs, kind)) =
                match_guarded_cmp_select_cond(ctx, block, sir_block, sir_defs, *cond)
        {
            block.push(MInst::GuardedCmpSelect {
                dst: dst_vreg,
                guard,
                lhs,
                rhs,
                kind,
                true_val: tv,
                false_val: ev,
            });
            ctx.known_bits.insert(dst_vreg, d_width);
            return;
        }

        if !ctx.four_state
            && d_width <= 64
            && ctx.known_bits.get(&tv).copied().unwrap_or(64) <= d_width
            && ctx.known_bits.get(&ev).copied().unwrap_or(64) <= d_width
        {
            block.push(MInst::Select {
                dst: dst_vreg,
                cond: cond_is_true,
                true_val: tv,
                false_val: ev,
            });
            ctx.known_bits.insert(dst_vreg, d_width);
            return;
        }

        if ctx.four_state {
            let tm = ctx.get_mask(*then_val, block);
            let em = ctx.get_mask(*else_val, block);
            let (value, mask) = lower_four_state_mux_chunk(
                ctx,
                block,
                cond_is_true,
                cond_is_unknown,
                tv,
                tm,
                ev,
                em,
                d_width,
            );
            ctx.emit_mov(block, dst_vreg, value);
            ctx.set_mask(*dst, mask);
        } else {
            let selected = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: selected,
                cond: cond_is_true,
                true_val: tv,
                false_val: ev,
            });
            if d_width < 64 {
                ctx.emit_and_imm(block, dst_vreg, selected, mask_for_width(d_width));
            } else {
                ctx.emit_mov(block, dst_vreg, selected);
            }
        }
    }
}

/// The shift `k` of a `Mux(cond, 1 << k, 0)` whose condition is a one-bit
/// two-state load (or its non-zero test) defined in the same block.
fn match_loaded_bit_select(
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
    cond: RegisterId,
    then_val: RegisterId,
    else_val: RegisterId,
) -> Option<u8> {
    let set = sir_imm_u64(sir_block, sir_defs, then_val)?;
    if !set.is_power_of_two() || sir_imm_u64(sir_block, sir_defs, else_val)? != 0 {
        return None;
    }
    let loaded_bit = |register: RegisterId| {
        matches!(
            sir_block.instructions[*sir_defs.get(&register)?],
            SIRInstruction::Load(_, _, _, 1)
        )
        .then_some(())
    };
    match &sir_block.instructions[*sir_defs.get(&cond)?] {
        SIRInstruction::Load(_, _, _, 1) => {}
        SIRInstruction::Binary(_, lhs, BinaryOp::Ne, rhs) => {
            let zero = |register| sir_imm_u64(sir_block, sir_defs, register) == Some(0);
            if !((zero(*rhs) && loaded_bit(*lhs).is_some())
                || (zero(*lhs) && loaded_bit(*rhs).is_some()))
            {
                return None;
            }
        }
        _ => return None,
    }
    Some(set.trailing_zeros() as u8)
}

/// Whether a select on `cond` would fuse with the compare computing it in
/// this block (`cmp; cmov`, or a guarded compare select): such a condition
/// is cheaper tested than materialized as a mask.
fn is_fusible_select_condition(
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
    cond: RegisterId,
) -> bool {
    sir_defs.get(&cond).is_some_and(|&index| {
        matches!(
            sir_block.instructions[index],
            SIRInstruction::Binary(
                _,
                _,
                BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::EqCase
                    | BinaryOp::NeCase
                    | BinaryOp::LtU
                    | BinaryOp::LtS
                    | BinaryOp::LeU
                    | BinaryOp::LeS
                    | BinaryOp::GtU
                    | BinaryOp::GtS
                    | BinaryOp::GeU
                    | BinaryOp::GeS
                    | BinaryOp::LogicAnd
                    | BinaryOp::LogicOr,
                _
            )
        )
    })
}
