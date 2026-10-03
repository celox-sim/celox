//! Multi-word arithmetic, comparisons, and runtime shifts.

use super::*;

// ────────────────────────────────────────────────────────────────
// Wide (>64-bit) operation lowering via multi-word chunks
// ────────────────────────────────────────────────────────────────

fn wide_sign_bit(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    chunks: &[(VReg, usize)],
    width: usize,
) -> VReg {
    if width == 0 {
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        return zero;
    }
    let sign_index = width - 1;
    let source = ctx.wide_chunk_or_zero(chunks, sign_index / 64, block);
    let shifted = if sign_index.is_multiple_of(64) {
        source
    } else {
        let shifted = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShrImm {
            dst: shifted,
            src: source,
            imm: (sign_index % 64) as u8,
        });
        shifted
    };
    let sign = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, sign, shifted, 1);
    sign
}

fn sign_extend_wide_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    chunks: &[(VReg, usize)],
    width: usize,
    num_chunks: usize,
) -> Vec<VReg> {
    let sign = wide_sign_bit(ctx, block, chunks, width);
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
    block.push(MInst::LoadImm {
        dst: all_ones,
        value: u64::MAX,
    });
    let fill = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: fill,
        cond: sign,
        true_val: all_ones,
        false_val: zero,
    });
    let source_chunks = width.div_ceil(64);
    let top_bits = width % 64;
    let mut extended = Vec::with_capacity(num_chunks);
    for index in 0..num_chunks {
        if index >= source_chunks {
            extended.push(fill);
            continue;
        }
        let raw = ctx.wide_chunk_or_zero(chunks, index, block);
        if index + 1 != source_chunks || top_bits == 0 {
            extended.push(raw);
            continue;
        }
        let low_mask = mask_for_width(top_bits);
        let low = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, low, raw, low_mask);
        let high = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, high, fill, !low_mask);
        let combined = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: combined,
            lhs: low,
            rhs: high,
        });
        extended.push(combined);
    }
    extended
}

fn conditional_negate_wide_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    chunks: &[VReg],
    negate: VReg,
    num_chunks: usize,
) -> Vec<VReg> {
    let one = ctx.alloc_vreg(SpillDesc::remat(1));
    block.push(MInst::LoadImm { dst: one, value: 1 });
    let mut carry = one;
    let mut negated = Vec::with_capacity(num_chunks);
    for &chunk in chunks.iter().take(num_chunks) {
        let inverted = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::BitNot {
            dst: inverted,
            src: chunk,
        });
        let sum = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Add {
            dst: sum,
            lhs: inverted,
            rhs: carry,
        });
        let next_carry = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: next_carry,
            lhs: sum,
            rhs: inverted,
            kind: CmpKind::LtU,
        });
        negated.push(sum);
        carry = next_carry;
    }
    (0..num_chunks)
        .map(|index| {
            let selected = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: selected,
                cond: negate,
                true_val: negated[index],
                false_val: chunks[index],
            });
            selected
        })
        .collect()
}

/// Lower a binary operation on wide (>64-bit) values.
/// Supports: And, Or, Xor (chunk-wise) and Shl (multi-word shift).
pub(super) fn lower_wide_binary(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    op: &BinaryOp,
    rhs: RegisterId,
) {
    let d_width = ctx.sir_width(&dst);
    let lhs_width = ctx.sir_width(&lhs);
    let rhs_width = ctx.sir_width(&rhs);
    // For comparisons and logic ops, the result may be narrow (1 bit)
    // but we need to process all chunks of the wider operand.
    let operation_width = d_width.max(lhs_width).max(rhs_width);
    let n_chunks = ISelContext::num_chunks(operation_width);

    if ctx.four_state && matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard) {
        lower_wide_wildcard_compare(ctx, block, dst, lhs, op, rhs, operation_width);
        return;
    }

    match op {
        // Chunk-wise operations: apply to each 64-bit chunk independently
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let mut dst_chunks = Vec::with_capacity(n_chunks);

            for i in 0..n_chunks {
                let l = lhs_chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let r = rhs_chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let d = ctx.alloc_vreg(SpillDesc::transient());
                match op {
                    BinaryOp::And => block.push(MInst::And {
                        dst: d,
                        lhs: l,
                        rhs: r,
                    }),
                    BinaryOp::Or => block.push(MInst::Or {
                        dst: d,
                        lhs: l,
                        rhs: r,
                    }),
                    BinaryOp::Xor => block.push(MInst::Xor {
                        dst: d,
                        lhs: l,
                        rhs: r,
                    }),
                    _ => unreachable!(),
                }
                dst_chunks.push((d, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide left shift by a scalar amount.
        // If the shift amount is a known constant (common after loop unrolling),
        // compute chunk assignments directly without runtime select chains.
        BinaryOp::Shl => {
            // (debug removed)
            let src_chunks = ctx.get_wide_chunks(&lhs, block);
            let n_src = src_chunks.len();

            if let Some(&amount) = ctx.consts.get(&rhs) {
                // Constant shift: compute each chunk statically
                let cs = (amount / 64) as usize; // chunk shift
                let is = (amount % 64) as u8; // intra-chunk shift

                let mut dst_chunks = Vec::with_capacity(n_chunks);
                for i in 0..n_chunks {
                    if i < cs {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        dst_chunks.push((z, 64));
                    } else {
                        let src_idx = i - cs;
                        let main_vreg = if src_idx < n_src {
                            src_chunks[src_idx].0
                        } else {
                            let z = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm { dst: z, value: 0 });
                            z
                        };

                        if is == 0 {
                            dst_chunks.push((main_vreg, 64));
                        } else {
                            // main_part = src[src_idx] << is
                            let main_shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: main_shifted,
                                src: main_vreg,
                                imm: is,
                            });

                            // carry from lower chunk: src[src_idx-1] >> (64 - is)
                            if src_idx > 0 && (src_idx - 1) < n_src {
                                let carry_vreg = src_chunks[src_idx - 1].0;
                                let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShrImm {
                                    dst: carry_shifted,
                                    src: carry_vreg,
                                    imm: 64 - is,
                                });
                                let combined = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or {
                                    dst: combined,
                                    lhs: main_shifted,
                                    rhs: carry_shifted,
                                });
                                dst_chunks.push((combined, 64));
                            } else {
                                dst_chunks.push((main_shifted, 64));
                            }
                        }
                    }
                }
                ctx.set_wide_chunks(dst, dst_chunks);
            } else {
                // Runtime left shift: select chain + carry propagation.
                lower_wide_runtime_shift(
                    ctx,
                    block,
                    dst,
                    &lhs,
                    &rhs,
                    n_chunks,
                    ShiftDir::Left,
                    false,
                );
            }
        }

        // Wide addition with carry chain
        BinaryOp::Add => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            let mut carry: Option<VReg> = None;

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);

                if let Some(cin) = carry {
                    // s1 = l + r
                    let s1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: s1,
                        lhs: l,
                        rhs: r,
                    });
                    // c1 = (s1 < l) unsigned
                    let c1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c1,
                        lhs: s1,
                        rhs: l,
                        kind: CmpKind::LtU,
                    });
                    // s2 = s1 + cin
                    let s2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: s2,
                        lhs: s1,
                        rhs: cin,
                    });
                    // c2 = (s2 < s1) unsigned
                    let c2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c2,
                        lhs: s2,
                        rhs: s1,
                        kind: CmpKind::LtU,
                    });
                    // carry = c1 | c2
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: cout,
                        lhs: c1,
                        rhs: c2,
                    });
                    carry = Some(cout);
                    dst_chunks.push((s2, 64));
                } else {
                    // s = l + r
                    let s = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: s,
                        lhs: l,
                        rhs: r,
                    });
                    // carry = (s < l) unsigned
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: cout,
                        lhs: s,
                        rhs: l,
                        kind: CmpKind::LtU,
                    });
                    carry = Some(cout);
                    dst_chunks.push((s, 64));
                }
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide subtraction with borrow chain
        BinaryOp::Sub => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            let mut borrow: Option<VReg> = None;

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);

                if let Some(bin) = borrow {
                    // d1 = l - r
                    let d1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Sub {
                        dst: d1,
                        lhs: l,
                        rhs: r,
                    });
                    // b1 = (r > l) unsigned
                    let b1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: b1,
                        lhs: r,
                        rhs: l,
                        kind: CmpKind::GtU,
                    });
                    // d2 = d1 - bin
                    let d2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Sub {
                        dst: d2,
                        lhs: d1,
                        rhs: bin,
                    });
                    // b2 = (bin > d1) unsigned
                    let b2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: b2,
                        lhs: bin,
                        rhs: d1,
                        kind: CmpKind::GtU,
                    });
                    // borrow = b1 | b2
                    let bout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: bout,
                        lhs: b1,
                        rhs: b2,
                    });
                    borrow = Some(bout);
                    dst_chunks.push((d2, 64));
                } else {
                    // d = l - r
                    let d = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Sub {
                        dst: d,
                        lhs: l,
                        rhs: r,
                    });
                    // borrow = (r > l) unsigned
                    let bout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: bout,
                        lhs: r,
                        rhs: l,
                        kind: CmpKind::GtU,
                    });
                    borrow = Some(bout);
                    dst_chunks.push((d, 64));
                }
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide equality/inequality: chunk-wise AND/OR of per-chunk comparisons
        BinaryOp::Eq
        | BinaryOp::Ne
        | BinaryOp::EqCase
        | BinaryOp::NeCase
        | BinaryOp::EqWildcard
        | BinaryOp::NeWildcard => {
            let is_eq = matches!(op, BinaryOp::Eq | BinaryOp::EqCase | BinaryOp::EqWildcard);
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let lhs_masks = if ctx.four_state && matches!(op, BinaryOp::EqCase | BinaryOp::NeCase) {
                Some(get_wide_mask_chunks(ctx, block, &lhs, n_chunks))
            } else {
                None
            };
            let rhs_masks = if ctx.four_state && matches!(op, BinaryOp::EqCase | BinaryOp::NeCase) {
                Some(get_wide_mask_chunks(ctx, block, &rhs, n_chunks))
            } else {
                None
            };

            let init = ctx.alloc_vreg(SpillDesc::remat(if is_eq { 1 } else { 0 }));
            block.push(MInst::LoadImm {
                dst: init,
                value: if is_eq { 1 } else { 0 },
            });
            let mut cond = init;

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);
                let eq = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: eq,
                    lhs: l,
                    rhs: r,
                    kind: CmpKind::Eq,
                });
                let eq = if let (Some(lhs_masks), Some(rhs_masks)) =
                    (lhs_masks.as_ref(), rhs_masks.as_ref())
                {
                    let mask_eq = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: mask_eq,
                        lhs: lhs_masks[i],
                        rhs: rhs_masks[i],
                        kind: CmpKind::Eq,
                    });
                    let both_eq = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::And {
                        dst: both_eq,
                        lhs: eq,
                        rhs: mask_eq,
                    });
                    both_eq
                } else {
                    eq
                };
                let next = ctx.alloc_vreg(SpillDesc::transient());
                if is_eq {
                    block.push(MInst::And {
                        dst: next,
                        lhs: cond,
                        rhs: eq,
                    });
                } else {
                    // ne: accumulate OR of (chunk != chunk)
                    let neq = ctx.alloc_vreg(SpillDesc::transient());
                    let one = ctx.alloc_vreg(SpillDesc::remat(1));
                    block.push(MInst::LoadImm { dst: one, value: 1 });
                    block.push(MInst::Xor {
                        dst: neq,
                        lhs: eq,
                        rhs: one,
                    });
                    block.push(MInst::Or {
                        dst: next,
                        lhs: cond,
                        rhs: neq,
                    });
                }
                cond = next;
            }
            // Result is a 1-bit value in chunk 0, rest zero
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((cond, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide unsigned comparisons: compare from MSB chunk down
        BinaryOp::LtU | BinaryOp::LeU | BinaryOp::GtU | BinaryOp::GeU => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);

            // Init: Le/Ge → 1 (true when equal), Lt/Gt → 0 (false when equal)
            let init_val = if matches!(op, BinaryOp::LeU | BinaryOp::GeU) {
                1u64
            } else {
                0u64
            };
            let init = ctx.alloc_vreg(SpillDesc::remat(init_val));
            block.push(MInst::LoadImm {
                dst: init,
                value: init_val,
            });
            let mut res = init;

            let cmp_kind = match op {
                BinaryOp::LtU | BinaryOp::LeU => CmpKind::LtU,
                BinaryOp::GtU | BinaryOp::GeU => CmpKind::GtU,
                _ => unreachable!(),
            };

            // Process from LSB to MSB; each chunk: if equal keep previous, else use this chunk's cmp
            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);
                let eq = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: eq,
                    lhs: l,
                    rhs: r,
                    kind: CmpKind::Eq,
                });
                let cmp = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: cmp,
                    lhs: l,
                    rhs: r,
                    kind: cmp_kind,
                });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: next,
                    cond: eq,
                    true_val: res,
                    false_val: cmp,
                });
                res = next;
            }

            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((res, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide signed comparisons: compare MSB chunk signed, lower chunks unsigned
        BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GtS | BinaryOp::GeS => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let top_bits = operation_width - (n_chunks - 1) * 64;

            let init_val = if matches!(op, BinaryOp::LeS | BinaryOp::GeS) {
                1u64
            } else {
                0u64
            };
            let init = ctx.alloc_vreg(SpillDesc::remat(init_val));
            block.push(MInst::LoadImm {
                dst: init,
                value: init_val,
            });
            let mut res = init;

            let unsigned_kind = match op {
                BinaryOp::LtS | BinaryOp::LeS => CmpKind::LtU,
                BinaryOp::GtS | BinaryOp::GeS => CmpKind::GtU,
                _ => unreachable!(),
            };
            let signed_kind = match op {
                BinaryOp::LtS | BinaryOp::LeS => CmpKind::LtS,
                BinaryOp::GtS | BinaryOp::GeS => CmpKind::GtS,
                _ => unreachable!(),
            };

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);
                let eq = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: eq,
                    lhs: l,
                    rhs: r,
                    kind: CmpKind::Eq,
                });
                // MSB chunk uses signed comparison, lower chunks use unsigned
                let (l, r, kind) = if i == n_chunks - 1 {
                    // Wide values keep unused top-chunk bits clear. For a
                    // non-64-multiple width the logical sign is therefore not
                    // physical bit 63; extend it before the signed compare.
                    (
                        sign_extend_scalar(ctx, block, l, top_bits),
                        sign_extend_scalar(ctx, block, r, top_bits),
                        signed_kind,
                    )
                } else {
                    (l, r, unsigned_kind)
                };
                let cmp = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: cmp,
                    lhs: l,
                    rhs: r,
                    kind,
                });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: next,
                    cond: eq,
                    true_val: res,
                    false_val: cmp,
                });
                res = next;
            }

            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((res, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide right shifts (logical and arithmetic)
        BinaryOp::Shr | BinaryOp::Sar => {
            let is_sar = matches!(op, BinaryOp::Sar);
            let src_chunks = ctx.get_wide_chunks(&lhs, block);
            let n_src = src_chunks.len();

            if let Some(&amount) = ctx.consts.get(&rhs) {
                // Constant shift
                let cs = (amount / 64) as usize; // chunk shift
                let is = (amount % 64) as u8; // intra-chunk shift

                let mut dst_chunks = Vec::with_capacity(n_chunks);
                for i in 0..n_chunks {
                    let src_idx = i + cs;
                    let main_vreg = if src_idx < n_src {
                        src_chunks[src_idx].0
                    } else if is_sar {
                        // SAR: fill with sign extension from MSB chunk
                        let msb = src_chunks[n_src - 1].0;
                        let sign = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::SarImm {
                            dst: sign,
                            src: msb,
                            imm: 63,
                        });
                        sign
                    } else {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        z
                    };

                    if is == 0 {
                        dst_chunks.push((main_vreg, 64));
                    } else {
                        // main_part = src[src_idx] >> is  (logical for SHR, logical here too — sign handled by carry)
                        let main_shifted = ctx.alloc_vreg(SpillDesc::transient());
                        if is_sar && i == n_chunks - 1 {
                            // MSB chunk of SAR: arithmetic shift
                            block.push(MInst::SarImm {
                                dst: main_shifted,
                                src: main_vreg,
                                imm: is,
                            });
                        } else {
                            block.push(MInst::ShrImm {
                                dst: main_shifted,
                                src: main_vreg,
                                imm: is,
                            });
                        }

                        // carry from upper chunk: src[src_idx+1] << (64 - is)
                        let upper_idx = src_idx + 1;
                        if upper_idx < n_src {
                            let carry_vreg = src_chunks[upper_idx].0;
                            let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: carry_shifted,
                                src: carry_vreg,
                                imm: 64 - is,
                            });
                            let combined = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: combined,
                                lhs: main_shifted,
                                rhs: carry_shifted,
                            });
                            dst_chunks.push((combined, 64));
                        } else if is_sar && i < n_chunks - 1 {
                            // SAR: carry from sign-extended chunk
                            let msb = src_chunks[n_src - 1].0;
                            let sign = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::SarImm {
                                dst: sign,
                                src: msb,
                                imm: 63,
                            });
                            let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: carry_shifted,
                                src: sign,
                                imm: 64 - is,
                            });
                            let combined = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: combined,
                                lhs: main_shifted,
                                rhs: carry_shifted,
                            });
                            dst_chunks.push((combined, 64));
                        } else {
                            dst_chunks.push((main_shifted, 64));
                        }
                    }
                }
                ctx.set_wide_chunks(dst, dst_chunks);
            } else {
                // Runtime right shift: select chain + carry propagation.
                let dir = if is_sar {
                    ShiftDir::ArithRight
                } else {
                    ShiftDir::Right
                };
                lower_wide_runtime_shift(ctx, block, dst, &lhs, &rhs, n_chunks, dir, is_sar);
            }
        }

        // Wide logical operations (result is 1-bit)
        BinaryOp::LogicAnd | BinaryOp::LogicOr => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);

            // Reduce lhs to bool: any chunk non-zero?
            let lhs_bool = wide_reduce_or(ctx, block, &lhs_chunks, n_chunks);
            // Reduce rhs to bool
            let rhs_bool = wide_reduce_or(ctx, block, &rhs_chunks, n_chunks);

            let result = ctx.alloc_vreg(SpillDesc::transient());
            match op {
                BinaryOp::LogicAnd => block.push(MInst::And {
                    dst: result,
                    lhs: lhs_bool,
                    rhs: rhs_bool,
                }),
                BinaryOp::LogicOr => block.push(MInst::Or {
                    dst: result,
                    lhs: lhs_bool,
                    rhs: rhs_bool,
                }),
                _ => unreachable!(),
            }

            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide multiplication: schoolbook O(n²) using UMulHi for 64×64→128.
        BinaryOp::Mul => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);

            // Accumulator: n_chunks of VRegs initialized to 0
            let mut acc: Vec<VReg> = (0..n_chunks)
                .map(|_| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                })
                .collect();

            for i in 0..n_chunks {
                let a_i = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let mut carry = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: carry,
                    value: 0,
                });

                for j in 0..n_chunks {
                    let k = i + j;
                    if k >= n_chunks {
                        break;
                    }

                    let b_j = ctx.wide_chunk_or_zero(&rhs_chunks, j, block);

                    // lo = a_i * b_j, hi = umulhi(a_i, b_j)
                    let lo = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Mul {
                        dst: lo,
                        lhs: a_i,
                        rhs: b_j,
                    });
                    let hi = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::UMulHi {
                        dst: hi,
                        lhs: a_i,
                        rhs: b_j,
                    });

                    // sum1 = acc[k] + lo
                    let sum1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: sum1,
                        lhs: acc[k],
                        rhs: lo,
                    });
                    let c1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c1,
                        lhs: sum1,
                        rhs: acc[k],
                        kind: CmpKind::LtU,
                    });

                    // sum2 = sum1 + carry
                    let sum2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: sum2,
                        lhs: sum1,
                        rhs: carry,
                    });
                    let c2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c2,
                        lhs: sum2,
                        rhs: sum1,
                        kind: CmpKind::LtU,
                    });

                    acc[k] = sum2;

                    // carry = hi + c1 + c2
                    let carry1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: carry1,
                        lhs: hi,
                        rhs: c1,
                    });
                    let new_carry = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: new_carry,
                        lhs: carry1,
                        rhs: c2,
                    });
                    carry = new_carry;
                }
            }

            let dst_chunks: Vec<(VReg, usize)> = acc.into_iter().map(|v| (v, 64)).collect();
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide division/remainder: bit-by-bit restoring division.
        BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let signed = matches!(op, BinaryOp::DivS | BinaryOp::RemS);
            let lhs_negative = wide_sign_bit(ctx, block, &lhs_chunks, lhs_width);
            let rhs_negative = wide_sign_bit(ctx, block, &rhs_chunks, rhs_width);
            let normalized_lhs = if signed {
                let extended =
                    sign_extend_wide_chunks(ctx, block, &lhs_chunks, lhs_width, n_chunks);
                conditional_negate_wide_chunks(ctx, block, &extended, lhs_negative, n_chunks)
            } else {
                (0..n_chunks)
                    .map(|index| ctx.wide_chunk_or_zero(&lhs_chunks, index, block))
                    .collect()
            };
            let normalized_rhs = if signed {
                let extended =
                    sign_extend_wide_chunks(ctx, block, &rhs_chunks, rhs_width, n_chunks);
                conditional_negate_wide_chunks(ctx, block, &extended, rhs_negative, n_chunks)
            } else {
                (0..n_chunks)
                    .map(|index| ctx.wide_chunk_or_zero(&rhs_chunks, index, block))
                    .collect()
            };
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let mut divisor_or = zero;
            for &chunk in &normalized_rhs {
                let combined = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: combined,
                    lhs: divisor_or,
                    rhs: chunk,
                });
                divisor_or = combined;
            }
            let divisor_is_zero = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: divisor_is_zero,
                lhs: divisor_or,
                rhs: zero,
                kind: CmpKind::Eq,
            });
            let total_bits = operation_width;

            let mut q_chunks: Vec<VReg> = (0..n_chunks)
                .map(|_| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                })
                .collect();
            let mut rem_chunks: Vec<VReg> = (0..n_chunks)
                .map(|_| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                })
                .collect();

            for bit in (0..total_bits).rev() {
                let chunk_idx = bit / 64;
                let bit_idx = bit % 64;

                // remainder <<= 1
                for c in (0..n_chunks).rev() {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShlImm {
                        dst: shifted,
                        src: rem_chunks[c],
                        imm: 1,
                    });
                    if c > 0 {
                        let carry_bit = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: carry_bit,
                            src: rem_chunks[c - 1],
                            imm: 63,
                        });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: combined,
                            lhs: shifted,
                            rhs: carry_bit,
                        });
                        rem_chunks[c] = combined;
                    } else {
                        rem_chunks[c] = shifted;
                    }
                }

                // remainder[0] |= (dividend[chunk_idx] >> bit_idx) & 1
                let dividend_chunk = normalized_lhs[chunk_idx];
                let extracted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: extracted,
                    src: dividend_chunk,
                    imm: bit_idx as u8,
                });
                let one_bit = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, one_bit, extracted, 1);
                let new_rem0 = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: new_rem0,
                    lhs: rem_chunks[0],
                    rhs: one_bit,
                });
                rem_chunks[0] = new_rem0;

                // if remainder >= divisor (chunk-wise unsigned comparison)
                let init_ge = ctx.alloc_vreg(SpillDesc::remat(1));
                block.push(MInst::LoadImm {
                    dst: init_ge,
                    value: 1,
                });
                let mut ge = init_ge;
                for (c, &rc) in rem_chunks.iter().enumerate() {
                    let dc = normalized_rhs[c];
                    let eq = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: eq,
                        lhs: rc,
                        rhs: dc,
                        kind: CmpKind::Eq,
                    });
                    let gt = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: gt,
                        lhs: rc,
                        rhs: dc,
                        kind: CmpKind::GeU,
                    });
                    let next_ge = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: next_ge,
                        cond: eq,
                        true_val: ge,
                        false_val: gt,
                    });
                    ge = next_ge;
                }

                // conditional: remainder -= divisor (wide sub with borrow)
                let mut borrow: Option<VReg> = None;
                for (c, rc) in rem_chunks.iter_mut().enumerate() {
                    let old_rc = *rc;
                    let dc = normalized_rhs[c];

                    let (diff, bout) = if let Some(bin) = borrow {
                        let d1 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sub {
                            dst: d1,
                            lhs: old_rc,
                            rhs: dc,
                        });
                        let b1 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: b1,
                            lhs: dc,
                            rhs: old_rc,
                            kind: CmpKind::GtU,
                        });
                        let d2 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sub {
                            dst: d2,
                            lhs: d1,
                            rhs: bin,
                        });
                        let b2 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: b2,
                            lhs: bin,
                            rhs: d1,
                            kind: CmpKind::GtU,
                        });
                        let bout = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: bout,
                            lhs: b1,
                            rhs: b2,
                        });
                        (d2, bout)
                    } else {
                        let d = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sub {
                            dst: d,
                            lhs: old_rc,
                            rhs: dc,
                        });
                        let bout = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: bout,
                            lhs: dc,
                            rhs: old_rc,
                            kind: CmpKind::GtU,
                        });
                        (d, bout)
                    };

                    // select: if ge then subtracted else original
                    let new_rc = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: new_rc,
                        cond: ge,
                        true_val: diff,
                        false_val: old_rc,
                    });
                    *rc = new_rc;
                    borrow = Some(bout);
                }

                // quotient[chunk_idx] |= ge ? (1 << bit_idx) : 0
                let bit_mask = ctx.alloc_vreg(SpillDesc::remat(1u64 << bit_idx));
                block.push(MInst::LoadImm {
                    dst: bit_mask,
                    value: 1u64 << bit_idx,
                });
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero,
                    value: 0,
                });
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: masked,
                    cond: ge,
                    true_val: bit_mask,
                    false_val: zero,
                });
                let new_q = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: new_q,
                    lhs: q_chunks[chunk_idx],
                    rhs: masked,
                });
                q_chunks[chunk_idx] = new_q;
            }

            let magnitude = if matches!(op, BinaryOp::DivU | BinaryOp::DivS) {
                q_chunks
            } else {
                rem_chunks
            };
            let signed_result = if signed {
                let result_negative = if matches!(op, BinaryOp::DivS) {
                    let negative = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Xor {
                        dst: negative,
                        lhs: lhs_negative,
                        rhs: rhs_negative,
                    });
                    negative
                } else {
                    lhs_negative
                };
                conditional_negate_wide_chunks(ctx, block, &magnitude, result_negative, n_chunks)
            } else {
                magnitude
            };
            let mut result_chunks = Vec::with_capacity(n_chunks);
            for (index, chunk) in signed_result.into_iter().enumerate() {
                let defined = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: defined,
                    cond: divisor_is_zero,
                    true_val: zero,
                    false_val: chunk,
                });
                let top_bits = d_width % 64;
                let defined = if index + 1 == ISelContext::num_chunks(d_width) && top_bits != 0 {
                    let masked = ctx.alloc_vreg(SpillDesc::transient());
                    ctx.emit_and_imm(block, masked, defined, mask_for_width(top_bits));
                    masked
                } else {
                    defined
                };
                result_chunks.push(defined);
            }
            result_chunks.truncate(ISelContext::num_chunks(d_width));
            let dst_chunks: Vec<(VReg, usize)> = result_chunks
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    let bits = if index + 1 == ISelContext::num_chunks(d_width) {
                        let top = d_width % 64;
                        if top == 0 { 64 } else { top }
                    } else {
                        64
                    };
                    (value, bits)
                })
                .collect();
            ctx.set_wide_chunks(dst, dst_chunks);
        }
    }

    // When the result is narrow (≤64 bits, e.g. comparison result),
    // sync chunk[0] back to the scalar reg_map so scalar Store paths
    // can read it.
    if d_width <= 64 {
        if let Some(chunks) = ctx.wide_regs.get(&dst) {
            let chunk0 = chunks[0].0;
            let scalar = ctx.reg_map.get(dst);
            if chunk0 != scalar {
                ctx.emit_alias_mov(block, scalar, chunk0);
            }
        }
    }
}

fn lower_wide_wildcard_compare(
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

/// Runtime multi-word shift via a word-level barrel network and bit carry.
///
/// Each word-offset bit selects one power-of-two displacement, bounding the
/// generated word-selection work by O(n log n) instead of comparing every
/// output word against every source word twice.
fn lower_wide_runtime_shift(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: &RegisterId,
    rhs: &RegisterId,
    n_chunks: usize,
    dir: ShiftDir,
    _is_sar: bool,
) {
    let src_chunks = ctx.get_wide_chunks(lhs, block);
    let dst_chunks = lower_wide_runtime_shift_chunks(ctx, block, &src_chunks, rhs, n_chunks, dir);
    ctx.set_wide_chunks(dst, dst_chunks);
}

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

    // Left shifts cannot bring a source word above the result width into the
    // result. Right shifts may select any source word, including when the
    // destination is narrower than the input.
    let span = if matches!(dir, ShiftDir::Left) {
        n_chunks
    } else {
        n_src.max(n_chunks)
    };
    let mut shifted_words = (0..span)
        .map(|index| src_chunks.get(index).map_or(fill, |chunk| chunk.0))
        .collect::<Vec<_>>();
    let mut distance = 1usize;
    while distance < span {
        let selected_bit = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, selected_bit, chunk_shift, distance as u64);
        let mut next_words = Vec::with_capacity(span);
        for index in 0..span {
            let source = match dir {
                ShiftDir::Left => index.checked_sub(distance),
                ShiftDir::Right | ShiftDir::ArithRight => index.checked_add(distance),
            };
            let shifted = source
                .and_then(|source| shifted_words.get(source))
                .copied()
                .unwrap_or(fill);
            let unchanged = shifted_words[index];
            if shifted == unchanged {
                next_words.push(unchanged);
            } else {
                let selected = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: selected,
                    cond: selected_bit,
                    true_val: shifted,
                    false_val: unchanged,
                });
                next_words.push(selected);
            }
        }
        shifted_words = next_words;
        distance = distance.checked_mul(2).unwrap_or(span);
    }

    // Bits above the barrel network's range must not wrap the shift amount.
    // This also handles non-power-of-two word counts and sign-fill for SAR.
    let limit = ctx.alloc_vreg(SpillDesc::remat(span as u64));
    block.push(MInst::LoadImm {
        dst: limit,
        value: span as u64,
    });
    let out_of_range = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: out_of_range,
        lhs: chunk_shift,
        rhs: limit,
        kind: CmpKind::GeU,
    });
    for word in &mut shifted_words {
        if *word != fill {
            let selected = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: selected,
                cond: out_of_range,
                true_val: fill,
                false_val: *word,
            });
            *word = selected;
        }
    }

    let mut dst_chunks = Vec::with_capacity(n_chunks);
    for i in 0..n_chunks {
        let main_chunk = shifted_words[i];
        let carry_index = match dir {
            ShiftDir::Left => i.checked_sub(1),
            ShiftDir::Right | ShiftDir::ArithRight => i.checked_add(1),
        };
        let carry_chunk = carry_index
            .and_then(|index| shifted_words.get(index))
            .copied()
            .unwrap_or(fill);

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
