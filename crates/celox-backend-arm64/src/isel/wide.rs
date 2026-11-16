//! Multi-word wildcard comparison, runtime shifts, and reduction.

use super::*;

// ────────────────────────────────────────────────────────────────
// Wide (>64-bit) operation lowering via multi-word chunks
// ────────────────────────────────────────────────────────────────

pub(super) fn lower_wide_wildcard_compare(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    op: &BinaryOp,
    rhs: RegisterId,
    operation_width: usize,
) {
    let n_chunks = ISelContext::num_chunks(operation_width);
    let lhs_values = ctx.get_wide_chunks(&lhs, block);
    let rhs_values = ctx.get_wide_chunks(&rhs, block);
    let lhs_masks = get_wide_mask_chunks(ctx, block, &lhs, n_chunks);
    let rhs_masks = get_wide_mask_chunks(ctx, block, &rhs, n_chunks);
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let mut mismatch_bits = zero;
    let mut unknown_bits = zero;

    for index in 0..n_chunks {
        let lhs_value = ctx.wide_chunk_or_zero(&lhs_values, index, block);
        let rhs_value = ctx.wide_chunk_or_zero(&rhs_values, index, block);
        let lhs_mask = lhs_masks[index];
        let rhs_mask = rhs_masks[index];
        let chunk_width = (operation_width - index * 64).min(64);

        let not_rhs_mask = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::BitNot {
            dst: not_rhs_mask,
            src: rhs_mask,
        });
        let not_lhs_mask = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::BitNot {
            dst: not_lhs_mask,
            src: lhs_mask,
        });
        let known_compared = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: known_compared,
            lhs: not_rhs_mask,
            rhs: not_lhs_mask,
        });
        let value_diff = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Xor {
            dst: value_diff,
            lhs: lhs_value,
            rhs: rhs_value,
        });
        let mismatch = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: mismatch,
            lhs: value_diff,
            rhs: known_compared,
        });
        let lhs_unknown = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: lhs_unknown,
            lhs: lhs_mask,
            rhs: not_rhs_mask,
        });

        let (mismatch, lhs_unknown) = if chunk_width < 64 {
            let valid = mask_for_width(chunk_width);
            let masked_mismatch = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked_mismatch, mismatch, valid);
            let masked_unknown = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked_unknown, lhs_unknown, valid);
            (masked_mismatch, masked_unknown)
        } else {
            (mismatch, lhs_unknown)
        };

        let next_mismatch = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next_mismatch,
            lhs: mismatch_bits,
            rhs: mismatch,
        });
        mismatch_bits = next_mismatch;
        let next_unknown = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next_unknown,
            lhs: unknown_bits,
            rhs: lhs_unknown,
        });
        unknown_bits = next_unknown;
    }

    let has_mismatch = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_mismatch,
        lhs: mismatch_bits,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let has_unknown = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_unknown,
        lhs: unknown_bits,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let value = if matches!(op, BinaryOp::EqWildcard) {
        let one = ctx.alloc_vreg(SpillDesc::remat(1));
        block.push(MInst::LoadImm { dst: one, value: 1 });
        let value = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: value,
            cond: has_mismatch,
            true_val: zero,
            false_val: one,
        });
        value
    } else {
        has_mismatch
    };
    let mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: mask,
        cond: has_mismatch,
        true_val: zero,
        false_val: has_unknown,
    });

    ctx.known_bits.insert(value, 1);
    ctx.set_wide_chunks(dst, vec![(value, 1)]);
    ctx.set_mask(dst, mask);
    ctx.wide_masks.insert(dst, vec![(mask, 1)]);
}

#[derive(Clone, Copy)]
pub(super) enum ShiftDir {
    Left,
    Right,
    ArithRight,
}

/// Runtime multi-word shift via select chain + cross-chunk carry.
///
/// For each output chunk, a select chain picks the source chunk based on
/// `shift_amt >> 6` (word offset), then applies the intra-chunk bit shift
/// with carry from the adjacent chunk.
pub(super) fn lower_wide_runtime_shift(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: &RegisterId,
    rhs: &RegisterId,
    n_chunks: usize,
    dir: ShiftDir,
    _is_sar: bool,
) {
    let mut src_chunks = ctx.get_wide_chunks(lhs, block);
    if matches!(dir, ShiftDir::ArithRight) {
        let width = ctx.sir_width(lhs);
        let top = (width - 1) / 64;
        src_chunks[top].0 = sign_extend_scalar(ctx, block, src_chunks[top].0, (width - 1) % 64 + 1);
    }
    let dst_chunks = lower_wide_runtime_shift_chunks(ctx, block, &src_chunks, rhs, n_chunks, dir);
    ctx.set_wide_chunks(dst, dst_chunks);
}

// SAR callers extend the partial top source word first, in either plane.
pub(super) fn lower_wide_runtime_shift_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    src_chunks: &[(VReg, usize)],
    rhs: &RegisterId,
    n_chunks: usize,
    dir: ShiftDir,
) -> Vec<(VReg, usize)> {
    let n_src = src_chunks.len();
    let amount_vreg = ctx.reg_map.get(*rhs);

    let chunk_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: chunk_shift,
        src: amount_vreg,
        imm: 6,
    });
    let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, bit_shift, amount_vreg, 63);
    let sixty_four = ctx.alloc_vreg(SpillDesc::remat(64));
    block.push(MInst::LoadImm {
        dst: sixty_four,
        value: 64,
    });
    let inv_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Sub {
        dst: inv_bit_shift,
        lhs: sixty_four,
        rhs: bit_shift,
    });
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let has_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_bit_shift,
        lhs: bit_shift,
        rhs: zero,
        kind: CmpKind::Ne,
    });

    // Fill value: 0 for SHL/SHR, sign-extension for SAR
    let fill = if matches!(dir, ShiftDir::ArithRight) {
        let msb = src_chunks[n_src - 1].0;
        let sf = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::SarImm {
            dst: sf,
            src: msb,
            imm: 63,
        });
        sf
    } else {
        zero
    };

    let mut dst_chunks = Vec::with_capacity(n_chunks);
    for i in 0..n_chunks {
        // Select the "main" source chunk via word_offset.
        // For SHL: src_index = i - word_offset → select where j + word_offset == i
        // For SHR/SAR: src_index = i + word_offset → select where j == i + word_offset
        let main_chunk = {
            let mut val = fill;
            for j in (0..n_src).rev() {
                // Compute the effective index this source chunk maps to
                let j_vreg = ctx.alloc_vreg(SpillDesc::remat(j as u64));
                block.push(MInst::LoadImm {
                    dst: j_vreg,
                    value: j as u64,
                });
                let eff_idx = ctx.alloc_vreg(SpillDesc::transient());
                match dir {
                    ShiftDir::Left => {
                        // src[j] goes to dst[j + word_offset]
                        block.push(MInst::Add {
                            dst: eff_idx,
                            lhs: j_vreg,
                            rhs: chunk_shift,
                        });
                    }
                    ShiftDir::Right | ShiftDir::ArithRight => {
                        // src[j + word_offset] goes to dst[j], i.e., src[j] goes to dst[j - word_offset]
                        // Check: j >= word_offset, then eff = j - word_offset
                        // Simpler: for dst[i], source is src[i + word_offset]
                        // So we select j if j == i + word_offset
                        block.push(MInst::Sub {
                            dst: eff_idx,
                            lhs: j_vreg,
                            rhs: chunk_shift,
                        });
                    }
                }
                let i_vreg = ctx.alloc_vreg(SpillDesc::remat(i as u64));
                block.push(MInst::LoadImm {
                    dst: i_vreg,
                    value: i as u64,
                });
                let is_match = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: is_match,
                    lhs: eff_idx,
                    rhs: i_vreg,
                    kind: CmpKind::Eq,
                });
                let selected = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: selected,
                    cond: is_match,
                    true_val: src_chunks[j].0,
                    false_val: val,
                });
                val = selected;
            }
            val
        };

        // Select the "carry" source chunk (adjacent in shift direction)
        let carry_chunk = {
            let mut val = fill;
            for j in (0..n_src).rev() {
                let j_vreg = ctx.alloc_vreg(SpillDesc::remat(j as u64));
                block.push(MInst::LoadImm {
                    dst: j_vreg,
                    value: j as u64,
                });
                let eff_idx = ctx.alloc_vreg(SpillDesc::transient());
                let carry_i = match dir {
                    ShiftDir::Left => {
                        // carry comes from chunk below: i-1
                        if i == 0 { usize::MAX } else { i - 1 }
                    }
                    ShiftDir::Right | ShiftDir::ArithRight => {
                        // carry comes from chunk above: i+1
                        i + 1
                    }
                };
                match dir {
                    ShiftDir::Left => {
                        block.push(MInst::Add {
                            dst: eff_idx,
                            lhs: j_vreg,
                            rhs: chunk_shift,
                        });
                    }
                    ShiftDir::Right | ShiftDir::ArithRight => {
                        block.push(MInst::Sub {
                            dst: eff_idx,
                            lhs: j_vreg,
                            rhs: chunk_shift,
                        });
                    }
                }
                let ci_vreg = ctx.alloc_vreg(SpillDesc::remat(carry_i as u64));
                block.push(MInst::LoadImm {
                    dst: ci_vreg,
                    value: carry_i as u64,
                });
                let is_match = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: is_match,
                    lhs: eff_idx,
                    rhs: ci_vreg,
                    kind: CmpKind::Eq,
                });
                let selected = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: selected,
                    cond: is_match,
                    true_val: src_chunks[j].0,
                    false_val: val,
                });
                val = selected;
            }
            val
        };

        // Apply intra-chunk shift: result = (main_chunk SHIFT bit_shift) | (carry_chunk INVSHIFT inv_bit_shift)
        // (debug removed)
        let bit_shift_copy = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_mov(block, bit_shift_copy, bit_shift);
        let inv_copy = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_mov(block, inv_copy, inv_bit_shift);

        let main_shifted = ctx.alloc_vreg(SpillDesc::transient());
        let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());

        match dir {
            ShiftDir::Left => {
                block.push(MInst::Shl {
                    dst: main_shifted,
                    lhs: main_chunk,
                    rhs: bit_shift_copy,
                });
                block.push(MInst::Shr {
                    dst: carry_shifted,
                    lhs: carry_chunk,
                    rhs: inv_copy,
                });
            }
            ShiftDir::Right => {
                block.push(MInst::Shr {
                    dst: main_shifted,
                    lhs: main_chunk,
                    rhs: bit_shift_copy,
                });
                block.push(MInst::Shl {
                    dst: carry_shifted,
                    lhs: carry_chunk,
                    rhs: inv_copy,
                });
            }
            ShiftDir::ArithRight => {
                if i == n_chunks - 1 {
                    block.push(MInst::Sar {
                        dst: main_shifted,
                        lhs: main_chunk,
                        rhs: bit_shift_copy,
                    });
                } else {
                    block.push(MInst::Shr {
                        dst: main_shifted,
                        lhs: main_chunk,
                        rhs: bit_shift_copy,
                    });
                }
                block.push(MInst::Shl {
                    dst: carry_shifted,
                    lhs: carry_chunk,
                    rhs: inv_copy,
                });
            }
        }

        // Combine: if has_bit_shift then (main | carry) else main
        let combined = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: combined,
            lhs: main_shifted,
            rhs: carry_shifted,
        });
        let result = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: result,
            cond: has_bit_shift,
            true_val: combined,
            false_val: main_chunk,
        });

        dst_chunks.push((result, 64));
    }
    dst_chunks
}

/// Reduce a wide value to a boolean (any chunk non-zero → 1, else 0).
pub(super) fn wide_reduce_or(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    chunks: &[(VReg, usize)],
    n_chunks: usize,
) -> VReg {
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let mut acc = zero;
    for i in 0..n_chunks {
        let c = chunks.get(i).map(|c| c.0).unwrap_or(zero);
        let next = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next,
            lhs: acc,
            rhs: c,
        });
        acc = next;
    }
    // acc != 0 → 1
    let result = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: result,
        lhs: acc,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    result
}
