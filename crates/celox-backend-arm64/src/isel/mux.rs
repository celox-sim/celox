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
