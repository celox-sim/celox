//! Mask planes of binary operations on multi-word values: the dispatcher
//! and one function per operator family.

use super::four_state::{any_chunk_has_x, equality_chunk_state, equality_result_mask};
use super::*;

/// Compute wide mask for binary operations.
pub(super) fn lower_wide_binary_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    op: &BinaryOp,
    rhs: RegisterId,
    d_width: usize,
) {
    // Four-state wide wildcard comparisons lower value and mask together so
    // a definite mismatch can dominate an unknown LHS bit across chunks.
    if matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard) {
        return;
    }

    let n_chunks =
        ISelContext::num_chunks(d_width.max(ctx.sir_width(&lhs)).max(ctx.sir_width(&rhs)));
    let lm_chunks = get_wide_mask_chunks(ctx, block, &lhs, n_chunks);
    let rm_chunks = get_wide_mask_chunks(ctx, block, &rhs, n_chunks);

    match op {
        BinaryOp::Eq | BinaryOp::Ne => {
            lower_wide_equality_mask(
                ctx, block, dst, lhs, rhs, d_width, n_chunks, &lm_chunks, &rm_chunks,
            );
        }
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
            lower_wide_bitwise_mask(
                ctx, block, dst, lhs, op, rhs, d_width, &lm_chunks, &rm_chunks,
            );
        }
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
            lower_wide_shift_mask(
                ctx, block, dst, lhs, op, rhs, d_width, n_chunks, lm_chunks, rm_chunks,
            );
        }
        BinaryOp::LogicAnd | BinaryOp::LogicOr => {
            lower_wide_logical_mask(ctx, block, dst, lhs, op, rhs, d_width);
        }
        BinaryOp::EqCase | BinaryOp::NeCase => {
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            ctx.set_mask(dst, zero);
            ctx.wide_masks.insert(dst, vec![(zero, d_width)]);
        }
        _ => {
            lower_wide_conservative_mask(ctx, block, dst, op, rhs, d_width, &lm_chunks, &rm_chunks);
        }
    }
}

/// `Eq` and `Ne`: unknown when an operand has an unknown bit, unless known bits
/// already differ.
fn lower_wide_equality_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    rhs: RegisterId,
    d_width: usize,
    n_chunks: usize,
    lm_chunks: &[VReg],
    rm_chunks: &[VReg],
) {
    let lv_chunks = ctx.get_wide_chunks(&lhs, block);
    let rv_chunks = ctx.get_wide_chunks(&rhs, block);
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let mut unknown = zero;
    let mut mismatch = zero;
    for i in 0..n_chunks {
        let lv = ctx.wide_chunk_or_zero(&lv_chunks, i, block);
        let rv = ctx.wide_chunk_or_zero(&rv_chunks, i, block);
        let (chunk_unknown, chunk_mismatch) =
            equality_chunk_state(ctx, block, lv, rv, lm_chunks[i], rm_chunks[i]);
        let next_unknown = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next_unknown,
            lhs: unknown,
            rhs: chunk_unknown,
        });
        let next_mismatch = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next_mismatch,
            lhs: mismatch,
            rhs: chunk_mismatch,
        });
        unknown = next_unknown;
        mismatch = next_mismatch;
    }
    let mask = equality_result_mask(ctx, block, unknown, mismatch, d_width);
    ctx.set_mask(dst, mask);
    ctx.wide_masks.insert(dst, vec![(mask, d_width)]);
}

/// `And`, `Or` and `Xor`: the scalar mask rule on each chunk.
fn lower_wide_bitwise_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    op: &BinaryOp,
    rhs: RegisterId,
    d_width: usize,
    lm_chunks: &[VReg],
    rm_chunks: &[VReg],
) {
    // Per-chunk mask computation
    let lv_chunks = ctx.get_wide_chunks(&lhs, block);
    let rv_chunks = ctx.get_wide_chunks(&rhs, block);
    let n_dst = ISelContext::num_chunks(d_width);
    let mut dst_m_chunks = Vec::with_capacity(n_dst);

    for i in 0..n_dst {
        let lm = lm_chunks.get(i).copied().unwrap_or_else(|| {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            z
        });
        let rm = rm_chunks.get(i).copied().unwrap_or_else(|| {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            z
        });
        let lv = ctx.wide_chunk_or_zero(&lv_chunks, i, block);
        let rv = ctx.wide_chunk_or_zero(&rv_chunks, i, block);
        let chunk_w = if i == n_dst - 1 {
            let r = d_width % 64;
            if r == 0 { 64 } else { r }
        } else {
            64
        };
        let m = lower_binary_mask(ctx, block, op, lv, rv, lm, rm, chunk_w);
        dst_m_chunks.push((m, 64));
    }
    // Set scalar mask from chunk[0]
    ctx.set_mask(dst, dst_m_chunks[0].0);
    ctx.wide_masks.insert(dst, dst_m_chunks);
}

/// Shifts: the mask is shifted like the value; an unknown amount makes every bit
/// unknown.
fn lower_wide_shift_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    op: &BinaryOp,
    rhs: RegisterId,
    d_width: usize,
    n_chunks: usize,
    lm_chunks: Vec<VReg>,
    rm_chunks: Vec<VReg>,
) {
    // If shift amount has X → all-X. Otherwise, shift mask same way as value.
    let shift_has_x = any_chunk_has_x(ctx, block, &rm_chunks);

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    // Extend the mask from the same logical sign position as the
    // payload. Shifts transport X and Z without normalizing them.
    let mut lm_chunks = lm_chunks.clone();
    let sign_fill = if matches!(op, BinaryOp::Sar) {
        let width = ctx.sir_width(&lhs);
        // get_wide_mask_chunks pads to the common operand/result width.
        // SAR must use sign fill above the logical source, not those zeros.
        lm_chunks.truncate(ISelContext::num_chunks(width));
        let top = (width - 1) / 64;
        lm_chunks[top] = sign_extend_scalar(ctx, block, lm_chunks[top], (width - 1) % 64 + 1);
        let fill = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::SarImm {
            dst: fill,
            src: lm_chunks[top],
            imm: 63,
        });
        fill
    } else {
        zero
    };
    let n_dst = ISelContext::num_chunks(d_width);
    // Get the result value chunks (already computed by lower_wide_binary)
    // The mask should follow the same pattern as the value.
    // For constant shifts, shift mask chunks directly.
    if let Some(&amount) = ctx.consts.get(&rhs) {
        let cs = (amount / 64) as usize;
        let is = (amount % 64) as u8;
        let mut dst_m_chunks = Vec::with_capacity(n_dst);

        match op {
            BinaryOp::Shl => {
                for i in 0..n_dst {
                    if i < cs {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        dst_m_chunks.push((z, 64));
                    } else {
                        let src_i = i - cs;
                        let cur = lm_chunks.get(src_i).copied().unwrap_or_else(|| {
                            let z = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm { dst: z, value: 0 });
                            z
                        });
                        if is == 0 {
                            dst_m_chunks.push((cur, 64));
                        } else {
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: shifted,
                                src: cur,
                                imm: is,
                            });
                            let prev = if src_i > 0 {
                                lm_chunks.get(src_i - 1).copied().unwrap_or_else(|| {
                                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                    block.push(MInst::LoadImm { dst: z, value: 0 });
                                    z
                                })
                            } else {
                                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                block.push(MInst::LoadImm { dst: z, value: 0 });
                                z
                            };
                            let carry = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: carry,
                                src: prev,
                                imm: 64 - is,
                            });
                            let merged = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: merged,
                                lhs: shifted,
                                rhs: carry,
                            });
                            dst_m_chunks.push((merged, 64));
                        }
                    }
                }
            }
            BinaryOp::Shr | BinaryOp::Sar => {
                for i in 0..n_dst {
                    let src_i = i + cs;
                    let cur = lm_chunks.get(src_i).copied().unwrap_or(sign_fill);
                    if is == 0 {
                        dst_m_chunks.push((cur, 64));
                    } else {
                        let shifted = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: shifted,
                            src: cur,
                            imm: is,
                        });
                        let next = lm_chunks.get(src_i + 1).copied().unwrap_or(sign_fill);
                        let carry = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShlImm {
                            dst: carry,
                            src: next,
                            imm: 64 - is,
                        });
                        let merged = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: merged,
                            lhs: shifted,
                            rhs: carry,
                        });
                        dst_m_chunks.push((merged, 64));
                    }
                }
            }
            _ => unreachable!(),
        }

        // Apply X-in-shift-amount override
        let all_x = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
        block.push(MInst::LoadImm {
            dst: all_x,
            value: u64::MAX,
        });
        let mut final_chunks = Vec::with_capacity(n_dst);
        for (i, (m, w)) in dst_m_chunks.into_iter().enumerate() {
            let chunk_w = if i == n_dst - 1 {
                let r = d_width % 64;
                if r == 0 { 64 } else { r }
            } else {
                64
            };
            let x_val = if chunk_w < 64 {
                let v = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(chunk_w)));
                block.push(MInst::LoadImm {
                    dst: v,
                    value: mask_for_width(chunk_w),
                });
                v
            } else {
                all_x
            };
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: shift_has_x,
                true_val: x_val,
                false_val: m,
            });
            final_chunks.push((res, w));
        }
        ctx.set_mask(dst, final_chunks[0].0);
        ctx.wide_masks.insert(dst, final_chunks);
    } else {
        // Runtime shift: apply the same chunk operation to the masks.
        let mask_chunks_wide: Vec<(VReg, usize)> =
            lm_chunks.iter().map(|&v| (v, 64usize)).collect();
        let dir = match op {
            BinaryOp::Shl => ShiftDir::Left,
            BinaryOp::Sar => ShiftDir::ArithRight,
            _ => ShiftDir::Right,
        };
        let shifted_mask_chunks =
            lower_wide_runtime_shift_chunks(ctx, block, &mask_chunks_wide, &rhs, n_chunks, dir);

        // Apply shift-has-X override
        let all_x_v = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
        block.push(MInst::LoadImm {
            dst: all_x_v,
            value: u64::MAX,
        });
        let mut final_m_chunks = Vec::with_capacity(n_dst);
        for i in 0..n_dst {
            let chunk_w = if i == n_dst - 1 {
                let r = d_width % 64;
                if r == 0 { 64 } else { r }
            } else {
                64
            };
            let x_val = if chunk_w < 64 {
                let v = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(chunk_w)));
                block.push(MInst::LoadImm {
                    dst: v,
                    value: mask_for_width(chunk_w),
                });
                v
            } else {
                all_x_v
            };
            let shifted_m = shifted_mask_chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                z
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: shift_has_x,
                true_val: x_val,
                false_val: shifted_m,
            });
            final_m_chunks.push((res, 64));
        }
        ctx.set_mask(dst, final_m_chunks[0].0);
        ctx.wide_masks.insert(dst, final_m_chunks);
    }

    // Keep the shifted X/Z payload for a known count. An unknown
    // count instead produces all X in both planes, including when a
    // wide operation has a narrow destination.
    let values = ctx.get_wide_chunks(&dst, block);
    let masks = ctx.wide_masks[&dst].clone();
    let values = values
        .into_iter()
        .zip(masks)
        .map(|((value, width), (mask, _))| {
            let selected = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: selected,
                cond: shift_has_x,
                true_val: mask,
                false_val: value,
            });
            (selected, width)
        })
        .collect();
    ctx.set_wide_chunks(dst, values);
}

/// `LogicAnd` and `LogicOr`: unknown unless one operand decides the result.
fn lower_wide_logical_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    op: &BinaryOp,
    rhs: RegisterId,
    d_width: usize,
) {
    let (lhs_is_true, lhs_is_unknown) = lower_mux_condition_state(ctx, block, lhs);
    let (rhs_is_true, rhs_is_unknown) = lower_mux_condition_state(ctx, block, rhs);
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });

    let dominant = if matches!(op, BinaryOp::LogicAnd) {
        let lhs_not_false = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: lhs_not_false,
            lhs: lhs_is_true,
            rhs: lhs_is_unknown,
        });
        let lhs_is_false = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: lhs_is_false,
            lhs: lhs_not_false,
            rhs: zero,
            kind: CmpKind::Eq,
        });
        let rhs_not_false = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: rhs_not_false,
            lhs: rhs_is_true,
            rhs: rhs_is_unknown,
        });
        let rhs_is_false = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: rhs_is_false,
            lhs: rhs_not_false,
            rhs: zero,
            kind: CmpKind::Eq,
        });
        let either_is_false = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: either_is_false,
            lhs: lhs_is_false,
            rhs: rhs_is_false,
        });
        either_is_false
    } else {
        let either_is_true = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: either_is_true,
            lhs: lhs_is_true,
            rhs: rhs_is_true,
        });
        either_is_true
    };
    let either_is_unknown = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: either_is_unknown,
        lhs: lhs_is_unknown,
        rhs: rhs_is_unknown,
    });
    let result_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: result_mask,
        cond: dominant,
        true_val: zero,
        false_val: either_is_unknown,
    });
    ctx.set_mask(dst, result_mask);
    ctx.wide_masks.insert(dst, vec![(result_mask, d_width)]);
}

/// Every other operator: an unknown bit in either operand, or a zero divisor, makes
/// the whole result unknown.
fn lower_wide_conservative_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    op: &BinaryOp,
    rhs: RegisterId,
    d_width: usize,
    lm_chunks: &[VReg],
    rm_chunks: &[VReg],
) {
    // Conservative: any X in any chunk of either operand → all-X result
    let mut all_masks: Vec<VReg> = lm_chunks.iter().chain(rm_chunks.iter()).copied().collect();
    if matches!(
        op,
        BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
    ) {
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        let mut divisor = zero;
        for (chunk, _) in ctx.get_wide_chunks(&rhs, block) {
            let combined = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: combined,
                lhs: divisor,
                rhs: chunk,
            });
            divisor = combined;
        }
        let zero_divisor = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: zero_divisor,
            lhs: divisor,
            rhs: zero,
            kind: CmpKind::Eq,
        });
        all_masks.push(zero_divisor);
    }
    let has_x = any_chunk_has_x(ctx, block, &all_masks);

    let n_dst = ISelContext::num_chunks(d_width);
    if n_dst == 0 {
        // 1-bit result (comparison)
        ctx.set_mask(dst, has_x);
        return;
    }
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });

    if d_width <= 64 {
        // Narrow result from wide operands (comparisons)
        let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
        block.push(MInst::LoadImm {
            dst: all_ones,
            value: mask_for_width(d_width),
        });
        let res = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: res,
            cond: has_x,
            true_val: all_ones,
            false_val: zero,
        });
        ctx.set_mask(dst, res);
    } else {
        let mut dst_m_chunks = Vec::with_capacity(n_dst);
        let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
        block.push(MInst::LoadImm {
            dst: all_ones,
            value: u64::MAX,
        });
        for i in 0..n_dst {
            let chunk_m = ctx.alloc_vreg(SpillDesc::transient());
            let chunk_w = if i == n_dst - 1 {
                let r = d_width % 64;
                if r == 0 { 64 } else { r }
            } else {
                64
            };
            if chunk_w < 64 {
                let mask_val = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(chunk_w)));
                block.push(MInst::LoadImm {
                    dst: mask_val,
                    value: mask_for_width(chunk_w),
                });
                block.push(MInst::Select {
                    dst: chunk_m,
                    cond: has_x,
                    true_val: mask_val,
                    false_val: zero,
                });
            } else {
                block.push(MInst::Select {
                    dst: chunk_m,
                    cond: has_x,
                    true_val: all_ones,
                    false_val: zero,
                });
            }
            dst_m_chunks.push((chunk_m, 64));
        }
        ctx.set_mask(dst, dst_m_chunks[0].0);
        ctx.wide_masks.insert(dst, dst_m_chunks);
    }
}
