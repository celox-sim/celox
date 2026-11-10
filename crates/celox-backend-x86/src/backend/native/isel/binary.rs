//! Binary operation lowering: dispatch to the wide and float paths, and
//! the scalar (at most 64-bit) operator families.

use super::four_state::lower_bitwise_planes;
use super::*;

/// Lower `Binary(dst, lhs, op, rhs)`; `inst` is that instruction.
pub(super) fn lower_binary(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
) {
    if let BinaryOp::Float { op, precision } = op {
        extern_call::lower_float(
            ctx,
            block,
            *dst,
            *lhs,
            Some(*rhs),
            celox_sir::float::binary_code(*op, *precision),
        );
        return;
    }
    if four_state::try_lower_associative_binary(ctx, block, inst, sir_block, sir_defs) {
        return;
    }
    let d_width = ctx.sir_width(dst);
    let lhs_width = ctx.sir_width(lhs);
    let rhs_width = ctx.sir_width(rhs);
    // With one-bit operands, logical AND/OR have exactly the bitwise
    // truth table, including X/Z. Operand evaluation is already in
    // the SIR CFG; this changes only the pure value/mask calculation.
    let op = match op {
        BinaryOp::LogicAnd if d_width == 1 && lhs_width == 1 && rhs_width == 1 => &BinaryOp::And,
        BinaryOp::LogicOr if d_width == 1 && lhs_width == 1 && rhs_width == 1 => &BinaryOp::Or,
        _ => op,
    };
    debug_assert!(lhs_width > 64 || !ctx.wide_regs.contains_key(lhs));
    debug_assert!(rhs_width > 64 || !ctx.wide_regs.contains_key(rhs));

    // A constant logical shift whose wide source is consumed as one
    // narrow result is an extraction, not a full-width shift. Select
    // it before the generic wide dispatcher; otherwise that path
    // constructs every shifted source chunk and only afterwards drops
    // all but the low destination bits.
    if !ctx.four_state
        && d_width <= 64
        && lhs_width > 64
        && rhs_width <= 64
        && matches!(op, BinaryOp::Shr)
        && ctx.consts.contains_key(rhs)
    {
        lower_wide_extract(ctx, block, *dst, *lhs, *rhs);
        return;
    }

    // Wide (>64-bit) binary operations: dispatch to multi-word handler.
    // For comparisons/logic, the result may be narrow (1 bit) but the
    // operands can be wide — dispatch based on operand width too.
    if d_width > 64 || lhs_width > 64 || rhs_width > 64 {
        lower_wide_binary(ctx, block, *dst, *lhs, op, *rhs);
        if ctx.four_state {
            lower_wide_binary_mask(ctx, block, *dst, *lhs, op, *rhs, d_width);
            if matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
                finish_shift_value(ctx, block, *dst, *rhs);
            } else {
                normalize_wide_value(ctx, block, *dst);
            }
        }
        ctx.canonicalize_narrow_wide_result(block, *dst);
        return;
    }
    // Constant folding: if both operands are known constants, compute result
    let lhs_const = ctx.consts.get(lhs).copied();
    let rhs_const = ctx.consts.get(rhs).copied();
    let masks_are_known_zero =
        ctx.const_mask_value(*lhs) == Some(0) && ctx.const_mask_value(*rhs) == Some(0);
    if masks_are_known_zero && let (Some(lc), Some(rc)) = (lhs_const, rhs_const) {
        let result = match op {
            BinaryOp::Add => Some(lc.wrapping_add(rc)),
            BinaryOp::Sub => Some(lc.wrapping_sub(rc)),
            BinaryOp::Mul => Some(lc.wrapping_mul(rc)),
            BinaryOp::And => Some(lc & rc),
            BinaryOp::Or => Some(lc | rc),
            BinaryOp::Xor => Some(lc ^ rc),
            BinaryOp::Shl => Some(if rc >= 64 { 0 } else { lc << rc }),
            BinaryOp::Shr => Some(if rc >= 64 { 0 } else { lc >> rc }),
            _ => None,
        };
        if let Some(val) = result {
            let mask = mask_for_width(d_width);
            let val = val & mask;
            let dst_vreg = ctx.reg_map.get(*dst);
            ctx.spill_descs[dst_vreg.0 as usize] = SpillDesc::remat(val);
            block.push(MInst::LoadImm {
                dst: dst_vreg,
                value: val,
            });
            ctx.consts.insert(*dst, val);
            set_low_zero_bits(ctx, *dst, low_zero_bits_const(val));
            if ctx.four_state {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                ctx.set_mask(*dst, z);
            }
            return;
        }
    }

    let dst_vreg = ctx.reg_map.get(*dst);
    let lhs_vreg = ctx.reg_map.get(*lhs);
    let rhs_vreg = ctx.reg_map.get(*rhs);

    lower_scalar_binary_value(
        ctx, block, dst, lhs, op, rhs, dst_vreg, lhs_vreg, rhs_vreg, d_width,
    );

    if matches!(
        op,
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
            | BinaryOp::LogicOr
            | BinaryOp::EqWildcard
            | BinaryOp::NeWildcard
    ) {
        ctx.known_bits.insert(dst_vreg, 1);
    }

    // 4-state: compute result mask (skip for wildcards which handle it inline)
    if ctx.four_state
        && !matches!(
            op,
            BinaryOp::EqWildcard | BinaryOp::NeWildcard | BinaryOp::EqCase | BinaryOp::NeCase
        )
    {
        let l_m = ctx.get_mask(*lhs, block);
        let l_m = if matches!(op, BinaryOp::Sar) {
            sign_extend_scalar(ctx, block, l_m, ctx.sir_width(lhs))
        } else {
            l_m
        };
        let r_m = ctx.get_mask(*rhs, block);
        if matches!(op, BinaryOp::And | BinaryOp::Or)
            && ctx.const_mask_value(*lhs) != Some(0)
            && ctx.const_mask_value(*rhs) != Some(0)
        {
            let (value, mask) = lower_bitwise_planes(
                ctx,
                block,
                *op,
                &[(lhs_vreg, l_m), (rhs_vreg, r_m)],
                d_width,
            );
            ctx.reg_map.set(*dst, value);
            ctx.set_mask(*dst, mask);
            return;
        }
        let res_m = lower_binary_mask(ctx, block, op, lhs_vreg, rhs_vreg, l_m, r_m, d_width);
        ctx.set_mask(*dst, res_m);

        if matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
            finish_shift_value(ctx, block, *dst, *rhs);
        } else {
            // Normalize: X positions must have v=1 (X encoding = v:1, m:1)
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

/// The value of a binary operation whose operands and result fit in 64 bits.
fn lower_scalar_binary_value(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    dst_vreg: VReg,
    lhs_vreg: VReg,
    rhs_vreg: VReg,
    d_width: usize,
) {
    match op {
        BinaryOp::Float { .. } => {
            unreachable!("float operations are lowered before integer operations")
        }
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
            lower_scalar_arithmetic(
                ctx, block, dst, lhs, op, rhs, dst_vreg, lhs_vreg, rhs_vreg, d_width,
            );
        }
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
            lower_scalar_bitwise(
                ctx, block, dst, lhs, op, rhs, dst_vreg, lhs_vreg, rhs_vreg, d_width,
            );
        }
        BinaryOp::Shr => {
            lower_scalar_shr(
                ctx, block, dst, lhs, rhs, dst_vreg, lhs_vreg, rhs_vreg, d_width,
            );
        }
        BinaryOp::Shl => {
            lower_scalar_shl(
                ctx, block, dst, lhs, rhs, dst_vreg, lhs_vreg, rhs_vreg, d_width,
            );
        }
        BinaryOp::Sar => {
            lower_scalar_sar(ctx, block, lhs, rhs, dst_vreg, lhs_vreg, rhs_vreg);
        }
        BinaryOp::Eq => block.push(MInst::Cmp {
            dst: dst_vreg,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
            kind: CmpKind::Eq,
        }),
        BinaryOp::Ne => {
            // A two-state one-bit value is its own non-zero test.
            let one_bit_against_zero = |value: &RegisterId, zero: &RegisterId| {
                !ctx.four_state
                    && ctx.consts.get(zero) == Some(&0)
                    && ctx.known_bits.get(&ctx.reg_map.get(*value)) == Some(&1)
            };
            if one_bit_against_zero(lhs, rhs) {
                ctx.emit_mov(block, dst_vreg, lhs_vreg);
            } else if one_bit_against_zero(rhs, lhs) {
                ctx.emit_mov(block, dst_vreg, rhs_vreg);
            } else {
                block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::Ne,
                });
            }
        }
        BinaryOp::EqCase | BinaryOp::NeCase => {
            lower_scalar_case_equality(ctx, block, dst, lhs, op, rhs, dst_vreg, lhs_vreg, rhs_vreg);
        }
        BinaryOp::LtU => block.push(MInst::Cmp {
            dst: dst_vreg,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
            kind: CmpKind::LtU,
        }),
        BinaryOp::LtS => {
            let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
            block.push(MInst::Cmp {
                dst: dst_vreg,
                lhs: sl,
                rhs: sr,
                kind: CmpKind::LtS,
            });
        }
        BinaryOp::LeU => block.push(MInst::Cmp {
            dst: dst_vreg,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
            kind: CmpKind::LeU,
        }),
        BinaryOp::LeS => {
            let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
            block.push(MInst::Cmp {
                dst: dst_vreg,
                lhs: sl,
                rhs: sr,
                kind: CmpKind::LeS,
            });
        }
        BinaryOp::GtU => block.push(MInst::Cmp {
            dst: dst_vreg,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
            kind: CmpKind::GtU,
        }),
        BinaryOp::GtS => {
            let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
            block.push(MInst::Cmp {
                dst: dst_vreg,
                lhs: sl,
                rhs: sr,
                kind: CmpKind::GtS,
            });
        }
        BinaryOp::GeU => block.push(MInst::Cmp {
            dst: dst_vreg,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
            kind: CmpKind::GeU,
        }),
        BinaryOp::GeS => {
            let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
            block.push(MInst::Cmp {
                dst: dst_vreg,
                lhs: sl,
                rhs: sr,
                kind: CmpKind::GeS,
            });
        }
        BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
            lower_scalar_division(
                ctx, block, lhs, op, rhs, dst_vreg, lhs_vreg, rhs_vreg, d_width,
            );
        }
        BinaryOp::LogicAnd => {
            // dst = (lhs != 0) && (rhs != 0) ? 1 : 0
            let l_bool = lower_bool_value(ctx, block, lhs_vreg);
            let r_bool = lower_bool_value(ctx, block, rhs_vreg);
            block.push(MInst::And {
                dst: dst_vreg,
                lhs: l_bool,
                rhs: r_bool,
            });
        }
        BinaryOp::LogicOr => {
            let l_bool = lower_bool_value(ctx, block, lhs_vreg);
            let r_bool = lower_bool_value(ctx, block, rhs_vreg);
            block.push(MInst::Or {
                dst: dst_vreg,
                lhs: l_bool,
                rhs: r_bool,
            });
        }
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            lower_scalar_wildcard_equality(
                ctx, block, dst, lhs, op, rhs, dst_vreg, lhs_vreg, rhs_vreg,
            );
        }
    }
}

/// Scalar `Add`, `Sub` and `Mul`.
fn lower_scalar_arithmetic(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    dst_vreg: VReg,
    lhs_vreg: VReg,
    rhs_vreg: VReg,
    d_width: usize,
) {
    // 64-bit arithmetic may produce upper bits; mask to output width.
    let raw = if d_width < 64 {
        ctx.alloc_vreg(SpillDesc::transient())
    } else {
        dst_vreg
    };
    let narrow32 = d_width <= 32;
    match op {
        BinaryOp::Add if narrow32 => block.push(MInst::Add32 {
            dst: raw,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        }),
        BinaryOp::Add => block.push(MInst::Add {
            dst: raw,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        }),
        BinaryOp::Sub if narrow32 => block.push(MInst::Sub32 {
            dst: raw,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        }),
        BinaryOp::Sub => block.push(MInst::Sub {
            dst: raw,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        }),
        BinaryOp::Mul if narrow32 => block.push(MInst::Mul32 {
            dst: raw,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        }),
        BinaryOp::Mul => block.push(MInst::Mul {
            dst: raw,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        }),
        _ => unreachable!(),
    }
    if d_width < 64 {
        ctx.emit_and_imm(block, dst_vreg, raw, mask_for_width(d_width));
    }
    let lhs_lz = low_zero_bits_reg(ctx, *lhs);
    let rhs_lz = low_zero_bits_reg(ctx, *rhs);
    let lz = match op {
        BinaryOp::Add | BinaryOp::Sub => lhs_lz.min(rhs_lz),
        BinaryOp::Mul => {
            let lhs_const = ctx.consts.get(lhs).copied();
            let rhs_const = ctx.consts.get(rhs).copied();
            match (lhs_const, rhs_const) {
                (_, Some(rc)) => lhs_lz.saturating_add(low_zero_bits_const(rc)),
                (Some(lc), _) => rhs_lz.saturating_add(low_zero_bits_const(lc)),
                _ => lhs_lz.saturating_add(rhs_lz),
            }
        }
        _ => unreachable!(),
    };
    set_low_zero_bits(ctx, *dst, lz);
}

/// Scalar `And`, `Or` and `Xor`.
fn lower_scalar_bitwise(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    dst_vreg: VReg,
    lhs_vreg: VReg,
    rhs_vreg: VReg,
    d_width: usize,
) {
    // For bitwise ops, result width = max(lhs_bits, rhs_bits).
    // If both inputs fit within d_width, AND mask is redundant.
    let lhs_bits = ctx.known_bits.get(&lhs_vreg).copied().unwrap_or(64);
    let rhs_bits = ctx.known_bits.get(&rhs_vreg).copied().unwrap_or(64);
    let result_bits = lhs_bits.max(rhs_bits);
    let needs_mask = d_width < 64 && result_bits > d_width;

    let raw = if needs_mask {
        ctx.alloc_vreg(SpillDesc::transient())
    } else {
        dst_vreg
    };
    let narrow32 = d_width <= 32;
    match op {
        BinaryOp::And if narrow32 => block.push(MInst::And32 {
            dst: raw,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        }),
        BinaryOp::And => block.push(MInst::And {
            dst: raw,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        }),
        BinaryOp::Or if narrow32 => block.push(MInst::Or32 {
            dst: raw,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        }),
        BinaryOp::Or => block.push(MInst::Or {
            dst: raw,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        }),
        BinaryOp::Xor if narrow32 => block.push(MInst::Xor32 {
            dst: raw,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        }),
        BinaryOp::Xor => block.push(MInst::Xor {
            dst: raw,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        }),
        _ => unreachable!(),
    }
    if needs_mask {
        ctx.emit_and_imm(block, dst_vreg, raw, mask_for_width(d_width));
    }
    let lhs_lz = low_zero_bits_reg(ctx, *lhs);
    let rhs_lz = low_zero_bits_reg(ctx, *rhs);
    let lz = match op {
        BinaryOp::And => lhs_lz.max(rhs_lz),
        BinaryOp::Or | BinaryOp::Xor => lhs_lz.min(rhs_lz),
        _ => unreachable!(),
    };
    set_low_zero_bits(ctx, *dst, lz);
}

/// Scalar `Shr`.
fn lower_scalar_shr(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    lhs: &RegisterId,
    rhs: &RegisterId,
    dst_vreg: VReg,
    lhs_vreg: VReg,
    rhs_vreg: VReg,
    d_width: usize,
) {
    let shifted = ctx.alloc_vreg(SpillDesc::transient());
    if let Some(&shift_amt) = ctx.consts.get(rhs) {
        if shift_amt < 64 {
            block.push(MInst::ShrImm {
                dst: shifted,
                src: lhs_vreg,
                imm: shift_amt as u8,
            });
        } else {
            block.push(MInst::LoadImm {
                dst: shifted,
                value: 0,
            });
        }
        // Track known bits: shr reduces width
        let lhs_bits = ctx.known_bits.get(&lhs_vreg).copied().unwrap_or(64);
        let shifted_bits =
            lhs_bits.saturating_sub(usize::try_from(shift_amt).unwrap_or(usize::MAX));
        ctx.known_bits.insert(shifted, shifted_bits);
    } else {
        let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_mov(block, rhs_copy, rhs_vreg);
        block.push(MInst::Shr {
            dst: shifted,
            lhs: lhs_vreg,
            rhs: rhs_copy,
        });
    }
    // Mask to destination width
    if d_width < 64 {
        let mask = mask_for_width(d_width);
        ctx.emit_and_imm(block, dst_vreg, shifted, mask);
    } else {
        ctx.emit_mov(block, dst_vreg, shifted);
    }
    let lz = if let Some(&shift_amt) = ctx.consts.get(rhs) {
        low_zero_bits_reg(ctx, *lhs).saturating_sub(u32::try_from(shift_amt).unwrap_or(u32::MAX))
    } else {
        0
    };
    set_low_zero_bits(ctx, *dst, lz);
}

/// Scalar `Shl`.
fn lower_scalar_shl(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    lhs: &RegisterId,
    rhs: &RegisterId,
    dst_vreg: VReg,
    lhs_vreg: VReg,
    rhs_vreg: VReg,
    d_width: usize,
) {
    let shifted = ctx.alloc_vreg(SpillDesc::transient());
    if let Some(&shift_amt) = ctx.consts.get(rhs) {
        if shift_amt < 64 {
            block.push(MInst::ShlImm {
                dst: shifted,
                src: lhs_vreg,
                imm: shift_amt as u8,
            });
        } else {
            block.push(MInst::LoadImm {
                dst: shifted,
                value: 0,
            });
        }
    } else {
        let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_mov(block, rhs_copy, rhs_vreg);
        block.push(MInst::Shl {
            dst: shifted,
            lhs: lhs_vreg,
            rhs: rhs_copy,
        });
    }
    if d_width < 64 {
        let mask = mask_for_width(d_width);
        ctx.emit_and_imm(block, dst_vreg, shifted, mask);
    } else {
        ctx.emit_mov(block, dst_vreg, shifted);
    }
    let lz = if let Some(&shift_amt) = ctx.consts.get(rhs) {
        low_zero_bits_reg(ctx, *lhs).saturating_add(u32::try_from(shift_amt).unwrap_or(u32::MAX))
    } else {
        0
    };
    set_low_zero_bits(ctx, *dst, lz);
}

/// Scalar `Sar`.
fn lower_scalar_sar(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    lhs: &RegisterId,
    rhs: &RegisterId,
    dst_vreg: VReg,
    lhs_vreg: VReg,
    rhs_vreg: VReg,
) {
    // Arithmetic shift right: sign-extend lhs to 64 bits, shift, mask result.
    let width = ctx.sir_width(lhs);
    if width < 64 {
        let sext_shift = (64 - width) as u8;
        let shifted_up = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShlImm {
            dst: shifted_up,
            src: lhs_vreg,
            imm: sext_shift,
        });
        let sign_extended = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::SarImm {
            dst: sign_extended,
            src: shifted_up,
            imm: sext_shift,
        });
        // Now do the actual shift
        let sar_result = ctx.alloc_vreg(SpillDesc::transient());
        if let Some(&shift_amt) = ctx.consts.get(rhs) {
            block.push(MInst::SarImm {
                dst: sar_result,
                src: sign_extended,
                imm: shift_amt.min(63) as u8,
            });
        } else {
            let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_mov(block, rhs_copy, rhs_vreg);
            block.push(MInst::Sar {
                dst: sar_result,
                lhs: sign_extended,
                rhs: rhs_copy,
            });
        }
        // Mask to output width
        let mask = mask_for_width(width);
        ctx.emit_and_imm(block, dst_vreg, sar_result, mask);
    } else {
        if let Some(&shift_amt) = ctx.consts.get(rhs) {
            block.push(MInst::SarImm {
                dst: dst_vreg,
                src: lhs_vreg,
                imm: shift_amt.min(63) as u8,
            });
        } else {
            let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_mov(block, rhs_copy, rhs_vreg);
            block.push(MInst::Sar {
                dst: dst_vreg,
                lhs: lhs_vreg,
                rhs: rhs_copy,
            });
        }
    }
}

/// Scalar `EqCase` and `NeCase`, including their mask.
fn lower_scalar_case_equality(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    dst_vreg: VReg,
    lhs_vreg: VReg,
    rhs_vreg: VReg,
) {
    if packed_state::compare(ctx, block, dst_vreg, *lhs, *rhs, *op) {
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        ctx.set_mask(*dst, zero);
    } else if ctx.four_state {
        let l_m = ctx.get_mask(*lhs, block);
        let r_m = ctx.get_mask(*rhs, block);
        let value_diff = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Xor {
            dst: value_diff,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        });
        let mask_diff = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Xor {
            dst: mask_diff,
            lhs: l_m,
            rhs: r_m,
        });
        let diff = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: diff,
            lhs: value_diff,
            rhs: mask_diff,
        });
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        block.push(MInst::Cmp {
            dst: dst_vreg,
            lhs: diff,
            rhs: zero,
            kind: if matches!(op, BinaryOp::EqCase) {
                CmpKind::Eq
            } else {
                CmpKind::Ne
            },
        });
        ctx.set_mask(*dst, zero);
    } else {
        block.push(MInst::Cmp {
            dst: dst_vreg,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
            kind: if matches!(op, BinaryOp::EqCase) {
                CmpKind::Eq
            } else {
                CmpKind::Ne
            },
        });
    }
}

/// Scalar `DivU`, `DivS`, `RemU` and `RemS`.
fn lower_scalar_division(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    dst_vreg: VReg,
    lhs_vreg: VReg,
    rhs_vreg: VReg,
    d_width: usize,
) {
    let signed = matches!(op, BinaryOp::DivS | BinaryOp::RemS);
    let lhs_width = ctx.sir_width(lhs);
    let rhs_width = ctx.sir_width(rhs);
    let division_lhs = if signed {
        sign_extend_scalar(ctx, block, lhs_vreg, lhs_width)
    } else {
        lhs_vreg
    };
    let division_rhs = if signed {
        sign_extend_scalar(ctx, block, rhs_vreg, rhs_width)
    } else {
        rhs_vreg
    };

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let one = ctx.alloc_vreg(SpillDesc::remat(1));
    block.push(MInst::LoadImm { dst: one, value: 1 });
    let is_zero = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: is_zero,
        lhs: division_rhs,
        rhs: zero,
        kind: CmpKind::Eq,
    });

    let unsafe_divisor = if signed {
        let min = ctx.alloc_vreg(SpillDesc::remat(1u64 << 63));
        block.push(MInst::LoadImm {
            dst: min,
            value: 1u64 << 63,
        });
        let neg_one = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
        block.push(MInst::LoadImm {
            dst: neg_one,
            value: u64::MAX,
        });
        let is_min = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: is_min,
            lhs: division_lhs,
            rhs: min,
            kind: CmpKind::Eq,
        });
        let is_neg_one = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: is_neg_one,
            lhs: division_rhs,
            rhs: neg_one,
            kind: CmpKind::Eq,
        });
        let is_overflow = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: is_overflow,
            lhs: is_min,
            rhs: is_neg_one,
        });
        let unsafe_divisor = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: unsafe_divisor,
            lhs: is_zero,
            rhs: is_overflow,
        });
        unsafe_divisor
    } else {
        is_zero
    };
    let safe_rhs = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: safe_rhs,
        cond: unsafe_divisor,
        true_val: one,
        false_val: division_rhs,
    });
    let division_result = ctx.alloc_vreg(SpillDesc::transient());
    match op {
        BinaryOp::DivU => block.push(MInst::UDiv {
            dst: division_result,
            lhs: division_lhs,
            rhs: safe_rhs,
        }),
        BinaryOp::RemU => block.push(MInst::URem {
            dst: division_result,
            lhs: division_lhs,
            rhs: safe_rhs,
        }),
        BinaryOp::DivS => block.push(MInst::SDiv {
            dst: division_result,
            lhs: division_lhs,
            rhs: safe_rhs,
        }),
        BinaryOp::RemS => block.push(MInst::SRem {
            dst: division_result,
            lhs: division_lhs,
            rhs: safe_rhs,
        }),
        _ => unreachable!(),
    }
    let defined_result = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: defined_result,
        cond: is_zero,
        true_val: zero,
        false_val: division_result,
    });
    if d_width < 64 {
        ctx.emit_and_imm(block, dst_vreg, defined_result, mask_for_width(d_width));
    } else {
        ctx.emit_mov(block, dst_vreg, defined_result);
    }
}

/// Scalar `EqWildcard` and `NeWildcard`, including their mask.
fn lower_scalar_wildcard_equality(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    dst_vreg: VReg,
    lhs_vreg: VReg,
    rhs_vreg: VReg,
) {
    if ctx.four_state {
        // IEEE 1800 ==?/!=?: RHS X/Z bits are wildcards (don't care)
        let l_m = ctx.get_mask(*lhs, block);
        let r_m = ctx.get_mask(*rhs, block);

        // compare_mask = ~r_m (non-wildcard positions)
        let compare_mask = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::BitNot {
            dst: compare_mask,
            src: r_m,
        });

        // Compare only at non-wildcard positions
        let l_eff = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: l_eff,
            lhs: lhs_vreg,
            rhs: compare_mask,
        });
        let r_eff = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: r_eff,
            lhs: rhs_vreg,
            rhs: compare_mask,
        });

        let kind = if matches!(op, BinaryOp::EqWildcard) {
            CmpKind::Eq
        } else {
            CmpKind::Ne
        };
        block.push(MInst::Cmp {
            dst: dst_vreg,
            lhs: l_eff,
            rhs: r_eff,
            kind,
        });

        // Mask: check for LHS X at non-wildcard positions
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        let x_at_compared = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: x_at_compared,
            lhs: l_m,
            rhs: compare_mask,
        });
        let has_x = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: has_x,
            lhs: x_at_compared,
            rhs: zero,
            kind: CmpKind::Ne,
        });

        // If definite mismatch at compared positions → mask=0
        let l_xor_r = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Xor {
            dst: l_xor_r,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
        });
        let l_definite = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::BitNot {
            dst: l_definite,
            src: l_m,
        });
        let definite_compare = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: definite_compare,
            lhs: compare_mask,
            rhs: l_definite,
        });
        let mismatch = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: mismatch,
            lhs: l_xor_r,
            rhs: definite_compare,
        });
        let has_mismatch = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: has_mismatch,
            lhs: mismatch,
            rhs: zero,
            kind: CmpKind::Ne,
        });

        // mask = has_mismatch ? 0 : (has_x ? 1 : 0)
        let res_m = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: res_m,
            cond: has_mismatch,
            true_val: zero,
            false_val: has_x,
        });
        ctx.set_mask(*dst, res_m);
        // Wildcards compute their mask inline, but still need
        // value normalization: an unknown result is X, not Z.
        let old_v = ctx.reg_map.get(*dst);
        let normalized = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: normalized,
            lhs: old_v,
            rhs: res_m,
        });
        ctx.reg_map.set(*dst, normalized);
        // Skip the general 4-state mask computation below
    } else {
        // 2-state: wildcards are same as Eq/Ne
        let kind = if matches!(op, BinaryOp::EqWildcard) {
            CmpKind::Eq
        } else {
            CmpKind::Ne
        };
        block.push(MInst::Cmp {
            dst: dst_vreg,
            lhs: lhs_vreg,
            rhs: rhs_vreg,
            kind,
        });
    }
}
