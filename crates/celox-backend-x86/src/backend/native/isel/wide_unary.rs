//! Multi-word unary operations and extraction.

use super::*;

/// Lower a unary operation on wide (>64-bit) values.
pub(super) fn lower_wide_unary(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    op: &UnaryOp,
    src: RegisterId,
) {
    let d_width = ctx.sir_width(&dst);
    let src_width = ctx.sir_width(&src);
    let n_chunks = ISelContext::num_chunks(d_width.max(src_width));

    match op {
        UnaryOp::BitNot => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            for i in 0..n_chunks {
                let s = src_chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let d = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::BitNot { dst: d, src: s });
                dst_chunks.push((d, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }
        UnaryOp::Ident | UnaryOp::ToTwoState => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            // Zero-pad to n_chunks if source has fewer chunks (narrow→wide cast)
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            for i in 0..n_chunks {
                if i < src_chunks.len() {
                    dst_chunks.push(src_chunks[i]);
                } else {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    dst_chunks.push((z, 64));
                }
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }
        // Wide negation: two's complement = ~x + 1
        UnaryOp::Minus => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            // First invert all bits
            let mut inv_chunks = Vec::with_capacity(n_chunks);
            for i in 0..n_chunks {
                let s = ctx.wide_chunk_or_zero(&src_chunks, i, block);
                let d = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::BitNot { dst: d, src: s });
                inv_chunks.push((d, 64usize));
            }
            // Then add 1 (wide add with constant 1)
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            let mut carry: Option<VReg> = None;
            for (i, &(l, _)) in inv_chunks.iter().enumerate() {
                let r = if i == 0 {
                    let one = ctx.alloc_vreg(SpillDesc::remat(1));
                    block.push(MInst::LoadImm { dst: one, value: 1 });
                    one
                } else {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                };
                let s = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Add {
                    dst: s,
                    lhs: l,
                    rhs: r,
                });
                if let Some(cin) = carry {
                    let s2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: s2,
                        lhs: s,
                        rhs: cin,
                    });
                    let c1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c1,
                        lhs: s,
                        rhs: l,
                        kind: CmpKind::LtU,
                    });
                    let c2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c2,
                        lhs: s2,
                        rhs: s,
                        kind: CmpKind::LtU,
                    });
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: cout,
                        lhs: c1,
                        rhs: c2,
                    });
                    carry = Some(cout);
                    dst_chunks.push((s2, 64));
                } else {
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

        // Wide logical not: result = (value == 0) ? 1 : 0
        UnaryOp::LogicNot => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let is_nonzero = wide_reduce_or(ctx, block, &src_chunks, n_chunks);
            // LogicNot: invert the boolean
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let result = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: result,
                lhs: is_nonzero,
                rhs: zero,
                kind: CmpKind::Eq,
            });

            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide reduction OR: result = (any bit set?) → 1
        UnaryOp::Or => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let result = wide_reduce_or(ctx, block, &src_chunks, n_chunks);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide reduction AND: result = (all bits set?) → 1
        UnaryOp::And => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
            block.push(MInst::LoadImm {
                dst: all_ones,
                value: u64::MAX,
            });
            let mut acc = all_ones;
            for i in 0..n_chunks {
                let c = ctx.wide_chunk_or_zero(&src_chunks, i, block);
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::And {
                    dst: next,
                    lhs: acc,
                    rhs: c,
                });
                acc = next;
            }
            let result = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: result,
                lhs: acc,
                rhs: all_ones,
                kind: CmpKind::Eq,
            });
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide reduction XOR: result = parity of all bits
        UnaryOp::Xor => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let mut acc = zero;
            for i in 0..n_chunks {
                let c = ctx.wide_chunk_or_zero(&src_chunks, i, block);
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Xor {
                    dst: next,
                    lhs: acc,
                    rhs: c,
                });
                acc = next;
            }
            // Now acc has XOR of all chunks. Need popcount parity (odd # of 1-bits → 1)
            // Fold 64-bit value to 1 bit by cascading XOR
            let mut val = acc;
            for shift in [32u8, 16, 8, 4, 2, 1] {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: shifted,
                    src: val,
                    imm: shift,
                });
                let folded = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Xor {
                    dst: folded,
                    lhs: val,
                    rhs: shifted,
                });
                val = folded;
            }
            let result = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, result, val, 1);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
            lower_wide_bit_count(ctx, block, dst, op, src);
        }
    }

    // Sync narrow results to scalar reg_map
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

/// Extract a ≤64-bit value from a wide (>64-bit) register by right-shifting.
pub(super) fn lower_wide_extract(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    wide_src: RegisterId,
    shift_amount: RegisterId,
) {
    let dst_vreg = ctx.reg_map.get(dst);
    let d_width = ctx.sir_width(&dst);
    let src_chunks = ctx.get_wide_chunks(&wide_src, block);
    let n_src = src_chunks.len();

    if let Some(&amount) = ctx.consts.get(&shift_amount) {
        // Constant extraction: directly pick the right chunk and shift
        let ci = (amount / 64) as usize;
        let is = (amount % 64) as u8;

        let main_vreg = if ci < n_src {
            src_chunks[ci].0
        } else {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            z
        };

        if is == 0 {
            if d_width < 64 {
                let mask = mask_for_width(d_width);
                ctx.emit_and_imm(block, dst_vreg, main_vreg, mask);
            } else {
                ctx.emit_mov(block, dst_vreg, main_vreg);
            }
        } else {
            let shifted = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted,
                src: main_vreg,
                imm: is,
            });

            // Include the next source word only if the declared result crosses
            // this word boundary. For a one-bit slice, unconditionally forming
            // `hi << (64 - bit)` creates two transients and an OR whose value
            // is discarded by the result mask.
            let crosses_chunk = d_width > 64 - usize::from(is);
            if crosses_chunk && (ci + 1) < n_src {
                let next_vreg = src_chunks[ci + 1].0;
                let carry = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShlImm {
                    dst: carry,
                    src: next_vreg,
                    imm: 64 - is,
                });
                let combined = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: combined,
                    lhs: shifted,
                    rhs: carry,
                });
                if d_width < 64 {
                    let mask = mask_for_width(d_width);
                    ctx.emit_and_imm(block, dst_vreg, combined, mask);
                } else {
                    ctx.emit_mov(block, dst_vreg, combined);
                }
            } else if d_width < 64 {
                let mask = mask_for_width(d_width);
                ctx.emit_and_imm(block, dst_vreg, shifted, mask);
            } else {
                ctx.emit_mov(block, dst_vreg, shifted);
            }
        }
    } else {
        // Wide Shr with non-constant amount is handled by lower_wide_binary's
        // runtime shift path. This code is only reachable if the narrow Binary
        // handler's Shr detects lhs_width > 64, which is pre-empted by the wide
        // dispatch at the top of the Binary handler.
        unreachable!(
            "wide extract with non-constant shift: should be handled by lower_wide_binary"
        );
    }
}
