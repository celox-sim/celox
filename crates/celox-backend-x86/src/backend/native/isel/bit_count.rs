//! Scalar and multi-word bit counting.

use super::*;

/// Mask a source word to its logical SIR width before a bit-count operation.
///
/// Loads normally zero-extend narrow values, but keeping the mask here makes
/// the count operations correct for every producer, including values that
/// reached ISel through a wide-to-narrow path.
fn mask_bit_count_word(ctx: &mut ISelContext, block: &mut MBlock, src: VReg, width: usize) -> VReg {
    if width >= 64 {
        src
    } else {
        let masked = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, masked, src, mask_for_width(width));
        masked
    }
}

fn bit_count_imm(ctx: &mut ISelContext, block: &mut MBlock, value: u64) -> VReg {
    let reg = ctx.alloc_vreg(SpillDesc::remat(value));
    block.push(MInst::LoadImm { dst: reg, value });
    reg
}

fn bit_count_nonzero(ctx: &mut ISelContext, block: &mut MBlock, src: VReg) -> VReg {
    let nonzero = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::CmpImm {
        dst: nonzero,
        lhs: src,
        imm: 0,
        kind: CmpKind::Ne,
    });
    nonzero
}

/// Return `(src != 0, base - bsr(src))`.  ORing bit 0 makes BSR defined for
/// zero without changing the highest set bit of any non-zero source.
fn clz_word_candidate(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    src: VReg,
    base: u64,
) -> (VReg, VReg) {
    let safe_src = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::OrImm {
        dst: safe_src,
        src,
        imm: 1,
    });
    let highest = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Bsr {
        dst: highest,
        src: safe_src,
    });
    let base = bit_count_imm(ctx, block, base);
    let candidate = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Sub {
        dst: candidate,
        lhs: base,
        rhs: highest,
    });
    (bit_count_nonzero(ctx, block, src), candidate)
}

/// Return `(src != 0, offset + ctz(src))`.  BSF's result is unspecified for
/// zero, but the paired predicate ensures that candidate is selected only for
/// a non-zero source.
fn ctz_word_candidate(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    src: VReg,
    offset: u64,
) -> (VReg, VReg) {
    let local = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Bsf { dst: local, src });
    let candidate = if offset == 0 {
        local
    } else {
        let offset = bit_count_imm(ctx, block, offset);
        let candidate = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Add {
            dst: candidate,
            lhs: offset,
            rhs: local,
        });
        candidate
    };
    (bit_count_nonzero(ctx, block, src), candidate)
}

/// Lower a bit-count operation whose source fits in one machine word.
pub(super) fn lower_narrow_bit_count(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: VReg,
    op: &UnaryOp,
    src: VReg,
    src_width: usize,
) {
    if src_width == 0 {
        block.push(MInst::LoadImm { dst, value: 0 });
        return;
    }

    let src = mask_bit_count_word(ctx, block, src, src_width);
    let (nonzero, candidate) = match op {
        UnaryOp::PopCount => {
            block.push(MInst::Popcnt { dst, src });
            return;
        }
        UnaryOp::CountLeadingZeros => clz_word_candidate(ctx, block, src, (src_width - 1) as u64),
        UnaryOp::CountTrailingZeros => ctz_word_candidate(ctx, block, src, 0),
        _ => return,
    };
    let width = bit_count_imm(ctx, block, src_width as u64);
    block.push(MInst::Select {
        dst,
        cond: nonzero,
        true_val: candidate,
        false_val: width,
    });
}

/// Return chunk `index`, masked to the part that belongs to the logical source.
fn bit_count_chunk(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    chunks: &[(VReg, usize)],
    src_width: usize,
    index: usize,
) -> VReg {
    let chunk = ctx.wide_chunk_or_zero(chunks, index, block);
    let chunk_width = (src_width - index * 64).min(64);
    mask_bit_count_word(ctx, block, chunk, chunk_width)
}

/// Lower a bit-count operation over an arbitrary-width source.  The result of
/// all three operations is at most `src_width`, so its canonical SIR result
/// always fits in one native word even when the source spans many chunks.
pub(super) fn lower_wide_bit_count(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    op: &UnaryOp,
    src: RegisterId,
) {
    let d_width = ctx.sir_width(&dst);
    let src_width = ctx.sir_width(&src);
    let chunks = ctx.get_wide_chunks(&src, block);
    let n_src = ISelContext::num_chunks(src_width);

    let result = match op {
        UnaryOp::PopCount => {
            let mut total = None;
            for index in 0..n_src {
                let chunk = bit_count_chunk(ctx, block, &chunks, src_width, index);
                let count = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Popcnt {
                    dst: count,
                    src: chunk,
                });
                total = Some(if let Some(total) = total {
                    let next = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: next,
                        lhs: total,
                        rhs: count,
                    });
                    next
                } else {
                    count
                });
            }
            total.unwrap_or_else(|| bit_count_imm(ctx, block, 0))
        }
        UnaryOp::CountLeadingZeros => {
            let mut count = bit_count_imm(ctx, block, src_width as u64);

            // Visiting chunks from least to most significant lets each
            // non-zero chunk overwrite the previous candidate; the last one
            // is therefore the highest non-zero chunk.
            for index in 0..n_src {
                let chunk = bit_count_chunk(ctx, block, &chunks, src_width, index);
                let base_value = src_width - 1 - index * 64;
                let (nonzero, candidate) = clz_word_candidate(ctx, block, chunk, base_value as u64);
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: next,
                    cond: nonzero,
                    true_val: candidate,
                    false_val: count,
                });
                count = next;
            }
            count
        }
        UnaryOp::CountTrailingZeros => {
            let mut count = bit_count_imm(ctx, block, src_width as u64);

            // Visiting chunks from most to least significant lets each
            // non-zero chunk overwrite the previous candidate; the last one
            // is therefore the lowest non-zero chunk.
            for index in (0..n_src).rev() {
                let chunk = bit_count_chunk(ctx, block, &chunks, src_width, index);
                let (nonzero, candidate) =
                    ctz_word_candidate(ctx, block, chunk, (index * 64) as u64);
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: next,
                    cond: nonzero,
                    true_val: candidate,
                    false_val: count,
                });
                count = next;
            }
            count
        }
        _ => return,
    };

    ctx.known_bits
        .insert(result, op.result_width(src_width).min(d_width));
    let n_dst = ISelContext::num_chunks(d_width).max(1);
    let mut dst_chunks = Vec::with_capacity(n_dst);
    dst_chunks.push((result, d_width.min(64)));
    for index in 1..n_dst {
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        dst_chunks.push((zero, (d_width - index * 64).min(64)));
    }
    ctx.set_wide_chunks(dst, dst_chunks);
}
