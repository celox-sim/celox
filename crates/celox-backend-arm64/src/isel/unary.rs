//! Unary operation lowering: dispatch to the wide, float and plane paths,
//! and the scalar (at most 64-bit) operators.

use super::*;

/// Lower `Unary(dst, op, src)`.
pub(super) fn lower_unary(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    op: &UnaryOp,
    src: &RegisterId,
) {
    if let UnaryOp::Float { op, precision } = op {
        if extern_call::lower_native_float_unary(ctx, block, *dst, *src, *op, *precision) {
            return;
        }
        extern_call::lower_float(
            ctx,
            block,
            *dst,
            *src,
            None,
            celox_sir::float::unary_code(*op, *precision),
        );
        return;
    }
    if matches!(op, UnaryOp::ToPlanes | UnaryOp::FromPlanes) {
        lower_planes(ctx, block, *dst, op, *src);
        return;
    }
    let d_width = ctx.sir_width(dst);
    let src_width = ctx.sir_width(src);
    debug_assert!(src_width > 64 || !ctx.wide_regs.contains_key(src));
    if d_width > 64 || src_width > 64 {
        lower_wide_unary(ctx, block, *dst, op, *src);
        if ctx.four_state {
            lower_wide_unary_mask(ctx, block, *dst, op, *src, d_width, src_width);
            if matches!(op, UnaryOp::ToTwoState) {
                lower_wide_to_two_state(ctx, block, *dst, *src, d_width, src_width);
            } else if !matches!(op, UnaryOp::Ident) {
                normalize_wide_value(ctx, block, *dst);
            }
        }
        ctx.canonicalize_narrow_wide_result(block, *dst);
        return;
    }
    let dst_vreg = ctx.reg_map.get(*dst);
    let src_vreg = ctx.reg_map.get(*src);

    match op {
        UnaryOp::Float { .. } | UnaryOp::ToPlanes | UnaryOp::FromPlanes => {
            unreachable!("float and plane operations are lowered before integer operations")
        }
        UnaryOp::Ident | UnaryOp::ToTwoState => {
            // A narrower destination takes the low bits of the source, and
            // a narrow register keeps no bits above its width.
            if d_width < src_width {
                ctx.emit_and_imm(block, dst_vreg, src_vreg, mask_for_width(d_width));
            } else {
                ctx.emit_mov(block, dst_vreg, src_vreg);
            }
        }
        UnaryOp::Minus => {
            if d_width < 64 {
                let negated = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Neg {
                    dst: negated,
                    src: src_vreg,
                });
                ctx.emit_and_imm(block, dst_vreg, negated, mask_for_width(d_width));
            } else {
                block.push(MInst::Neg {
                    dst: dst_vreg,
                    src: src_vreg,
                });
            }
        }
        UnaryOp::BitNot => {
            let width = ctx.sir_width(src);
            if width < 64 {
                let tmp = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::BitNot {
                    dst: tmp,
                    src: src_vreg,
                });
                ctx.emit_and_imm(block, dst_vreg, tmp, mask_for_width(width));
            } else {
                block.push(MInst::BitNot {
                    dst: dst_vreg,
                    src: src_vreg,
                });
            }
        }
        UnaryOp::LogicNot => {
            // dst = (src == 0) ? 1 : 0
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            block.push(MInst::Cmp {
                dst: dst_vreg,
                lhs: src_vreg,
                rhs: zero,
                kind: CmpKind::Eq,
            });
        }
        UnaryOp::And => {
            // Reduction AND: dst = (src == all_ones_mask) ? 1 : 0
            let width = ctx.sir_width(src);
            let mask = if width >= 64 {
                u64::MAX
            } else {
                mask_for_width(width)
            };
            let mask_vreg = ctx.alloc_vreg(SpillDesc::remat(mask));
            block.push(MInst::LoadImm {
                dst: mask_vreg,
                value: mask,
            });
            block.push(MInst::Cmp {
                dst: dst_vreg,
                lhs: src_vreg,
                rhs: mask_vreg,
                kind: CmpKind::Eq,
            });
        }
        UnaryOp::Or => {
            // Reduction OR: dst = (src != 0) ? 1 : 0
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            block.push(MInst::Cmp {
                dst: dst_vreg,
                lhs: src_vreg,
                rhs: zero,
                kind: CmpKind::Ne,
            });
        }
        UnaryOp::Xor => {
            // Reduction XOR: dst = popcount(src) & 1.
            let pc = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Popcnt {
                dst: pc,
                src: src_vreg,
            });
            ctx.emit_and_imm(block, dst_vreg, pc, 1);
            ctx.known_bits.insert(dst_vreg, 1);
        }
        UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
            lower_narrow_bit_count(ctx, block, dst_vreg, op, src_vreg, src_width);
            ctx.known_bits.insert(dst_vreg, d_width);
        }
    }

    // 4-state: compute result mask for unary ops
    if ctx.four_state {
        let s_m = ctx.get_mask(*src, block);
        if matches!(op, UnaryOp::ToTwoState) {
            let defined = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: defined,
                src: s_m,
            });
            let cleared = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: cleared,
                lhs: ctx.reg_map.get(*dst),
                rhs: defined,
            });
            ctx.reg_map.set(*dst, cleared);
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            ctx.set_mask(*dst, zero);
            return;
        }
        let res_m = lower_unary_mask(ctx, block, op, src_vreg, s_m, d_width, src_width);
        ctx.set_mask(*dst, res_m);

        if !matches!(op, UnaryOp::Ident) {
            // Identity/casts preserve X versus Z. Other unary
            // operations normalize unknown bits to X.
            let old_v = ctx.reg_map.get(*dst);
            let normalized = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: normalized,
                lhs: old_v,
                rhs: res_m,
            });
            ctx.reg_map.set(*dst, normalized);
        }
    }
}
