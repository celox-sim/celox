//! Four-state mask propagation and value normalization.

use super::*;

// Known unequal bits dominate X/Z in logical equality. Compute each chunk's
// mismatch before reducing chunks, so an unknown in another chunk cannot hide it.
fn equality_chunk_state(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    lv: VReg,
    rv: VReg,
    lm: VReg,
    rm: VReg,
) -> (VReg, VReg) {
    let unknown = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: unknown,
        lhs: lm,
        rhs: rm,
    });
    let known = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::BitNot {
        dst: known,
        src: unknown,
    });
    let diff = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Xor {
        dst: diff,
        lhs: lv,
        rhs: rv,
    });
    let mismatch = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::And {
        dst: mismatch,
        lhs: diff,
        rhs: known,
    });
    (unknown, mismatch)
}

fn equality_result_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    unknown: VReg,
    mismatch: VReg,
    d_width: usize,
) -> VReg {
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let has_mismatch = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_mismatch,
        lhs: mismatch,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let conservative = conservative_mask(ctx, block, unknown, zero, d_width);
    let result = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: result,
        cond: has_mismatch,
        true_val: zero,
        false_val: conservative,
    });
    result
}

/// Compute result mask for a binary operation.
///
/// Mask formulas (from IEEE 1800 / Cranelift backend):
/// - AND: m = (lm & rm) | (lm & rv) | (rm & lv)  ... but dominant-0 cancels X
///   Simplified: any X bit where the other operand's corresponding bit is not
///   definite-0 propagates as X. If the other bit is definite-0, AND = 0 regardless.
///   Formula: res_m = (lm | rm) & ~(~lv & ~lm) & ~(~rv & ~rm)
///   equivalently: res_m = (lm & rm) | (lm & rv) | (rm & lv)
/// - OR:  dual of AND — dominant-1 cancels X
///   res_m = (lm & rm) | (lm & ~rv) | (rm & ~lv)
/// - XOR: res_m = lm | rm
/// - Shift: if shift amount has X → all-X; else shift mask normally
/// - Arithmetic (Add/Sub/Mul/Div/Rem): any X → all-X; Div/Rem by zero → all-X
/// - Equality: known mismatch → definite result; otherwise any X → result X
/// - Ordered comparison: any X → result X (1-bit mask)
/// - LogicAnd: dominant-false (v|m==0) → mask=0; else if any X → mask=all-X
/// - LogicOr: dominant-true (v&~m!=0) → mask=0; else if any X → mask=all-X
pub(super) fn lower_binary_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    op: &BinaryOp,
    lv: VReg,
    rv: VReg,
    lm: VReg,
    rm: VReg,
    d_width: usize,
) -> VReg {
    match op {
        BinaryOp::Eq | BinaryOp::Ne => {
            let (unknown, mismatch) = equality_chunk_state(ctx, block, lv, rv, lm, rm);
            equality_result_mask(ctx, block, unknown, mismatch, d_width)
        }
        BinaryOp::And => {
            // res_m = (lm & rm) | (lm & rv) | (rm & lv)
            let t1 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t1,
                lhs: lm,
                rhs: rm,
            });
            let t2 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t2,
                lhs: lm,
                rhs: rv,
            });
            let t3 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t3,
                lhs: rm,
                rhs: lv,
            });
            let t4 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: t4,
                lhs: t1,
                rhs: t2,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: res,
                lhs: t4,
                rhs: t3,
            });
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, res, mask_for_width(d_width));
                masked
            } else {
                res
            }
        }
        BinaryOp::Or => {
            // res_m = (lm & rm) | (lm & ~rv) | (rm & ~lv)
            let t1 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t1,
                lhs: lm,
                rhs: rm,
            });
            let not_rv = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_rv,
                src: rv,
            });
            let t2 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t2,
                lhs: lm,
                rhs: not_rv,
            });
            let not_lv = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_lv,
                src: lv,
            });
            let t3 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t3,
                lhs: rm,
                rhs: not_lv,
            });
            let t4 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: t4,
                lhs: t1,
                rhs: t2,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: res,
                lhs: t4,
                rhs: t3,
            });
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, res, mask_for_width(d_width));
                masked
            } else {
                res
            }
        }
        BinaryOp::Xor => {
            // res_m = lm | rm
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: res,
                lhs: lm,
                rhs: rm,
            });
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, res, mask_for_width(d_width));
                masked
            } else {
                res
            }
        }
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
            // If shift amount has X → all-X; else shift mask by same amount
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: rm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            // Shift mask by value
            let shifted_m = ctx.alloc_vreg(SpillDesc::transient());
            match op {
                BinaryOp::Shl => block.push(MInst::Shl {
                    dst: shifted_m,
                    lhs: lm,
                    rhs: rv,
                }),
                BinaryOp::Shr => block.push(MInst::Shr {
                    dst: shifted_m,
                    lhs: lm,
                    rhs: rv,
                }),
                BinaryOp::Sar => block.push(MInst::Sar {
                    dst: shifted_m,
                    lhs: lm,
                    rhs: rv,
                }),
                _ => unreachable!(),
            }
            let all_x = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
            block.push(MInst::LoadImm {
                dst: all_x,
                value: u64::MAX,
            });
            let raw = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: raw,
                cond: has_x,
                true_val: all_x,
                false_val: shifted_m,
            });
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, raw, mask_for_width(d_width));
                masked
            } else {
                raw
            }
        }
        BinaryOp::LogicAnd => {
            // Dominant false: if either operand is definite false (v|m == 0) → mask=0
            // else if any X → mask = all-X
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let l_vm = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: l_vm,
                lhs: lv,
                rhs: lm,
            });
            let r_vm = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: r_vm,
                lhs: rv,
                rhs: rm,
            });
            let l_def_false = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: l_def_false,
                lhs: l_vm,
                rhs: zero,
                kind: CmpKind::Eq,
            });
            let r_def_false = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: r_def_false,
                lhs: r_vm,
                rhs: zero,
                kind: CmpKind::Eq,
            });
            let either_false = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: either_false,
                lhs: l_def_false,
                rhs: r_def_false,
            });
            // any X?
            let l_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: l_has_x,
                lhs: lm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let r_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: r_has_x,
                lhs: rm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let any_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: any_x,
                lhs: l_has_x,
                rhs: r_has_x,
            });
            let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
            block.push(MInst::LoadImm {
                dst: all_ones,
                value: mask_for_width(d_width),
            });
            let conservative = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: conservative,
                cond: any_x,
                true_val: all_ones,
                false_val: zero,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: either_false,
                true_val: zero,
                false_val: conservative,
            });
            res
        }
        BinaryOp::LogicOr => {
            // Dominant true: if either operand is definite true → mask=0
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let not_lm = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_lm,
                src: lm,
            });
            let l_def_v = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: l_def_v,
                lhs: lv,
                rhs: not_lm,
            });
            let not_rm = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_rm,
                src: rm,
            });
            let r_def_v = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: r_def_v,
                lhs: rv,
                rhs: not_rm,
            });
            let l_def_true = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: l_def_true,
                lhs: l_def_v,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let r_def_true = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: r_def_true,
                lhs: r_def_v,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let either_true = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: either_true,
                lhs: l_def_true,
                rhs: r_def_true,
            });
            let l_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: l_has_x,
                lhs: lm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let r_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: r_has_x,
                lhs: rm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let any_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: any_x,
                lhs: l_has_x,
                rhs: r_has_x,
            });
            let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
            block.push(MInst::LoadImm {
                dst: all_ones,
                value: mask_for_width(d_width),
            });
            let conservative = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: conservative,
                cond: any_x,
                true_val: all_ones,
                false_val: zero,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: either_true,
                true_val: zero,
                false_val: conservative,
            });
            res
        }
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            // Wildcard mask is handled inline in the Binary handler;
            // this arm should never be reached.
            unreachable!("wildcard mask is computed inline, not via lower_binary_mask")
        }
        BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
            // IEEE 1800-2023 11.4.3: a zero divisor also produces all X.
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let zero_divisor = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: zero_divisor,
                lhs: rv,
                rhs: zero,
                kind: CmpKind::Eq,
            });
            let invalid_rhs = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: invalid_rhs,
                lhs: rm,
                rhs: zero_divisor,
            });
            conservative_mask(ctx, block, lm, invalid_rhs, d_width)
        }
        _ => {
            // Conservative: any X in either operand → all-X result
            // Covers: Add, Sub, Mul, ordered comparisons (Lt/Le/Gt/Ge)
            conservative_mask(ctx, block, lm, rm, d_width)
        }
    }
}

/// Compute result mask for a unary operation.
pub(super) fn lower_unary_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    op: &UnaryOp,
    src_v: VReg,
    src_m: VReg,
    d_width: usize,
    src_width: usize,
) -> VReg {
    match op {
        UnaryOp::ToTwoState => {
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            zero
        }
        UnaryOp::Ident => {
            // Mask passes through but must be zero-extended for widening.
            // When src is narrower than dst, upper bits should be 0 (definite 0).
            let effective_width = src_width.min(d_width);
            if effective_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, src_m, mask_for_width(effective_width));
                masked
            } else {
                src_m
            }
        }
        UnaryOp::BitNot => {
            // ~X = X, mask passes through
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, src_m, mask_for_width(d_width));
                masked
            } else {
                src_m
            }
        }
        UnaryOp::Minus
        | UnaryOp::PopCount
        | UnaryOp::CountLeadingZeros
        | UnaryOp::CountTrailingZeros => {
            // Conservative: any X → all-X
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: src_m,
                rhs: zero,
                kind: CmpKind::Ne,
            });
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
            res
        }
        UnaryOp::And => {
            // Reduction AND: dominant-0. If any definite 0 → result definite (mask=0).
            // definite_zeros = ~src_v & ~src_m (bits that are definitely 0)
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let not_v = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_v,
                src: src_v,
            });
            let not_m = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_m,
                src: src_m,
            });
            let def_zeros = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: def_zeros,
                lhs: not_v,
                rhs: not_m,
            });
            // Mask to src_width
            let def_zeros_masked = if src_width < 64 {
                let m = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, m, def_zeros, mask_for_width(src_width));
                m
            } else {
                def_zeros
            };
            let has_def_zero = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_def_zero,
                lhs: def_zeros_masked,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: src_m,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            // If definite 0 → mask=0; else if X → mask=1; else mask=0
            let x_mask = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: x_mask,
                cond: has_x,
                true_val: has_x,
                false_val: zero,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: has_def_zero,
                true_val: zero,
                false_val: x_mask,
            });
            res
        }
        UnaryOp::LogicNot | UnaryOp::Or => {
            // Reduction OR: dominant-1. If any definite 1 → result definite (mask=0).
            // definite_ones = src_v & ~src_m (bits that are definitely 1)
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let not_m = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_m,
                src: src_m,
            });
            let def_ones = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: def_ones,
                lhs: src_v,
                rhs: not_m,
            });
            let has_def_one = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_def_one,
                lhs: def_ones,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: src_m,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let x_mask = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: x_mask,
                cond: has_x,
                true_val: has_x,
                false_val: zero,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: has_def_one,
                true_val: zero,
                false_val: x_mask,
            });
            res
        }
        UnaryOp::Xor => {
            // Reduction XOR: any X → result X
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: src_m,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            has_x
        }
    }
}

/// Conservative mask: if either operand has any X bits, result is all-X.
fn conservative_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    lm: VReg,
    rm: VReg,
    d_width: usize,
) -> VReg {
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let l_has_x = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: l_has_x,
        lhs: lm,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let r_has_x = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: r_has_x,
        lhs: rm,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let any_x = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: any_x,
        lhs: l_has_x,
        rhs: r_has_x,
    });
    let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
    block.push(MInst::LoadImm {
        dst: all_ones,
        value: mask_for_width(d_width),
    });
    let res = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: res,
        cond: any_x,
        true_val: all_ones,
        false_val: zero,
    });
    res
}

// Preserve shifted Z bits. Only an X/Z in the count forces an all-X payload.
pub(super) fn finish_shift_value(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    rhs: RegisterId,
) {
    let masks = get_wide_mask_chunks(
        ctx,
        block,
        &rhs,
        ISelContext::num_chunks(ctx.sir_width(&rhs)),
    );
    let has_unknown = any_chunk_has_x(ctx, block, &masks);
    let chunks = ctx
        .wide_regs
        .get(&dst)
        .cloned()
        .unwrap_or_else(|| vec![(ctx.reg_map.get(dst), ctx.sir_width(&dst))]);
    let mut result = Vec::with_capacity(chunks.len());
    for (value, width) in chunks {
        let all_x = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(width)));
        block.push(MInst::LoadImm {
            dst: all_x,
            value: mask_for_width(width),
        });
        let selected = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: selected,
            cond: has_unknown,
            true_val: all_x,
            false_val: value,
        });
        result.push((selected, width));
    }
    if ctx.sir_width(&dst) > 64 {
        ctx.set_wide_chunks(dst, result);
    } else {
        ctx.reg_map.set(dst, result[0].0);
    }
}

/// Normalize wide 4-state value: operations produce X (v=1,m=1), never Z.
/// For each chunk, computes `v_chunk |= m_chunk`.
pub(super) fn normalize_wide_value(ctx: &mut ISelContext, block: &mut MBlock, dst: RegisterId) {
    let mask_chunks: Vec<VReg> = if let Some(mc) = ctx.wide_masks.get(&dst).cloned() {
        mc.iter().map(|c| c.0).collect()
    } else {
        return;
    };

    if ctx.sir_width(&dst) <= 64 {
        let value = ctx.reg_map.get(dst);
        let normalized = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: normalized,
            lhs: value,
            rhs: mask_chunks[0],
        });
        ctx.reg_map.set(dst, normalized);
        ctx.wide_regs.remove(&dst);
        ctx.wide_masks.remove(&dst);
        return;
    }

    if let Some(val_chunks) = ctx.wide_regs.get(&dst).cloned() {
        let mut new_chunks = Vec::with_capacity(val_chunks.len());
        for (i, &(vc, width)) in val_chunks.iter().enumerate() {
            if let Some(&mc) = mask_chunks.get(i) {
                let normed = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: normed,
                    lhs: vc,
                    rhs: mc,
                });
                new_chunks.push((normed, width));
            } else {
                new_chunks.push((vc, width));
            }
        }
        ctx.set_wide_chunks(dst, new_chunks);
    }
}

/// Collapse unknown source bits to zero for an explicit four-state to
/// two-state conversion, then clear every destination mask chunk.
pub(super) fn lower_wide_to_two_state(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    src: RegisterId,
    d_width: usize,
    src_width: usize,
) {
    let source_masks =
        get_wide_mask_chunks(ctx, block, &src, ISelContext::num_chunks(src_width).max(1));
    let values = ctx.get_wide_chunks(&dst, block);
    let n_dst = ISelContext::num_chunks(d_width).max(1);
    let mut cleared_chunks = Vec::with_capacity(n_dst);

    for index in 0..n_dst {
        let value = ctx.wide_chunk_or_zero(&values, index, block);
        let source_mask = source_masks.get(index).copied().unwrap_or_else(|| {
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            zero
        });
        let defined = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::BitNot {
            dst: defined,
            src: source_mask,
        });
        let cleared = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: cleared,
            lhs: value,
            rhs: defined,
        });
        let chunk_width = (d_width - index * 64).min(64);
        cleared_chunks.push((cleared, chunk_width));
    }

    ctx.set_wide_chunks(dst, cleared_chunks);
}

// ────────────────────────────────────────────────────────────────
// Wide (>64-bit) 4-state mask computation
// ────────────────────────────────────────────────────────────────

/// Helper: get wide mask chunks for a register, or create zero chunks.
pub(super) fn get_wide_mask_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    reg: &RegisterId,
    n_chunks: usize,
) -> Vec<VReg> {
    if let Some(mchunks) = ctx.wide_masks.get(reg).cloned() {
        let mut result: Vec<VReg> = mchunks.iter().map(|c| c.0).collect();
        while result.len() < n_chunks {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            result.push(z);
        }
        result
    } else {
        // Use scalar mask if available
        let scalar_m = ctx.mask_map.map.get(reg.0).copied().flatten();
        let mut result = Vec::with_capacity(n_chunks);
        if let Some(m) = scalar_m {
            result.push(m);
        } else {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            result.push(z);
        }
        for _ in 1..n_chunks {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            result.push(z);
        }
        result
    }
}

/// Slice the four-state mask in exactly the same bit positions as the value.
/// Keeping this separate from value selection avoids the static-load shortcut
/// silently leaving a destination mask undefined.
pub(super) fn lower_slice_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    src: RegisterId,
    bit_offset: usize,
    width: usize,
) {
    let src_width = ctx.sir_width(&src);
    let n_src = ISelContext::num_chunks(src_width).max(1);
    let src_chunks = get_wide_mask_chunks(ctx, block, &src, n_src);
    let n_dst = ISelContext::num_chunks(width).max(1);
    let mut dst_chunks = Vec::with_capacity(n_dst);

    let chunk_or_zero = |ctx: &mut ISelContext, block: &mut MBlock, index: usize| -> VReg {
        src_chunks.get(index).copied().unwrap_or_else(|| {
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            zero
        })
    };

    for dst_index in 0..n_dst {
        let absolute_bit = bit_offset + dst_index * 64;
        let src_index = absolute_bit / 64;
        let intra_bit = absolute_bit % 64;
        let low = chunk_or_zero(ctx, block, src_index);
        let combined = if intra_bit == 0 {
            low
        } else {
            let shifted_low = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted_low,
                src: low,
                imm: intra_bit as u8,
            });
            let high = chunk_or_zero(ctx, block, src_index + 1);
            let shifted_high = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShlImm {
                dst: shifted_high,
                src: high,
                imm: (64 - intra_bit) as u8,
            });
            let combined = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: combined,
                lhs: shifted_low,
                rhs: shifted_high,
            });
            combined
        };

        let chunk_width = (width - dst_index * 64).min(64);
        let masked = if chunk_width < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, combined, mask_for_width(chunk_width));
            masked
        } else {
            combined
        };
        dst_chunks.push((masked, chunk_width));
    }

    ctx.set_mask(dst, dst_chunks[0].0);
    if width > 64 {
        ctx.wide_masks.insert(dst, dst_chunks);
    }
}

/// Check if any chunk has X bits (OR-reduce all mask chunks).
fn any_chunk_has_x(ctx: &mut ISelContext, block: &mut MBlock, mask_chunks: &[VReg]) -> VReg {
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    // OR all mask chunks together
    let mut combined = mask_chunks[0];
    for &mc in &mask_chunks[1..] {
        let t = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: t,
            lhs: combined,
            rhs: mc,
        });
        combined = t;
    }
    let has_x = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_x,
        lhs: combined,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    has_x
}

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
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
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
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
            // If shift amount has X → all-X. Otherwise, shift mask same way as value.
            let shift_has_x = any_chunk_has_x(ctx, block, &rm_chunks);
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
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
                            let cur = lm_chunks.get(src_i).copied().unwrap_or_else(|| {
                                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                block.push(MInst::LoadImm { dst: z, value: 0 });
                                z
                            });
                            if is == 0 {
                                dst_m_chunks.push((cur, 64));
                            } else {
                                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShrImm {
                                    dst: shifted,
                                    src: cur,
                                    imm: is,
                                });
                                let next = lm_chunks.get(src_i + 1).copied().unwrap_or_else(|| {
                                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                    block.push(MInst::LoadImm { dst: z, value: 0 });
                                    z
                                });
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

                        // SAR: if the sign bit is X, sign-extension produces X in upper bits.
                        // Check if bit (lhs_width-1) in the mask is set.
                        if matches!(op, BinaryOp::Sar) {
                            let lhs_w = ctx.sir_width(&lhs);
                            let sign_chunk = (lhs_w - 1) / 64;
                            let sign_bit = (lhs_w - 1) % 64;
                            let sign_mask_chunk =
                                lm_chunks.get(sign_chunk).copied().unwrap_or_else(|| {
                                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                    block.push(MInst::LoadImm { dst: z, value: 0 });
                                    z
                                });
                            // Extract sign bit from mask
                            let sign_x = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: sign_x,
                                src: sign_mask_chunk,
                                imm: sign_bit as u8,
                            });
                            let one = ctx.alloc_vreg(SpillDesc::remat(1));
                            block.push(MInst::LoadImm { dst: one, value: 1 });
                            let sign_x_bit = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::And {
                                dst: sign_x_bit,
                                lhs: sign_x,
                                rhs: one,
                            });
                            let sign_is_x = ctx.alloc_vreg(SpillDesc::transient());
                            let z_cmp = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm {
                                dst: z_cmp,
                                value: 0,
                            });
                            block.push(MInst::Cmp {
                                dst: sign_is_x,
                                lhs: sign_x_bit,
                                rhs: z_cmp,
                                kind: CmpKind::Ne,
                            });

                            // For chunks above the shifted sign position, OR with all-X if sign is X.
                            // The sign bit after shift is at position (lhs_width - 1 - shift_amount).
                            // All bits above this position in the result are sign-extended.
                            let effective_sign_pos =
                                lhs_w.saturating_sub(1).saturating_sub(amount as usize);
                            let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                            block.push(MInst::LoadImm {
                                dst: all_ones,
                                value: u64::MAX,
                            });
                            for (i, chunk) in dst_m_chunks.iter_mut().enumerate() {
                                let chunk_start = i * 64;
                                if chunk_start >= effective_sign_pos {
                                    // Entire chunk is above sign — all X if sign is X
                                    let new_m = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Select {
                                        dst: new_m,
                                        cond: sign_is_x,
                                        true_val: all_ones,
                                        false_val: chunk.0,
                                    });
                                    chunk.0 = new_m;
                                } else if chunk_start + 64 > effective_sign_pos {
                                    // Partial: bits above effective_sign_pos in this chunk
                                    let bit_in_chunk = effective_sign_pos - chunk_start;
                                    let upper_mask_val = u64::MAX << bit_in_chunk;
                                    let upper_mask =
                                        ctx.alloc_vreg(SpillDesc::remat(upper_mask_val));
                                    block.push(MInst::LoadImm {
                                        dst: upper_mask,
                                        value: upper_mask_val,
                                    });
                                    let x_fill = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Select {
                                        dst: x_fill,
                                        cond: sign_is_x,
                                        true_val: upper_mask,
                                        false_val: z_cmp,
                                    });
                                    let new_m = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Or {
                                        dst: new_m,
                                        lhs: chunk.0,
                                        rhs: x_fill,
                                    });
                                    chunk.0 = new_m;
                                }
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
                let shifted_mask_chunks = lower_wide_runtime_shift_chunks(
                    ctx,
                    block,
                    &mask_chunks_wide,
                    &rhs,
                    n_chunks,
                    dir,
                );

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
        }
        BinaryOp::LogicAnd | BinaryOp::LogicOr => {
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
            // Conservative: any X in any chunk of either operand → all-X result
            let mut all_masks: Vec<VReg> =
                lm_chunks.iter().chain(rm_chunks.iter()).copied().collect();
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
    }
}

/// Compute wide mask for unary operations.
pub(super) fn lower_wide_unary_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    op: &UnaryOp,
    src: RegisterId,
    d_width: usize,
    src_width: usize,
) {
    let n_src = ISelContext::num_chunks(src_width);
    let sm_chunks = get_wide_mask_chunks(ctx, block, &src, n_src);

    match op {
        UnaryOp::ToTwoState => {
            let n_dst = ISelContext::num_chunks(d_width);
            let mut dst_m_chunks = Vec::with_capacity(n_dst);
            for index in 0..n_dst {
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero,
                    value: 0,
                });
                let chunk_width = (d_width - index * 64).min(64);
                dst_m_chunks.push((zero, chunk_width));
            }
            ctx.set_mask(dst, dst_m_chunks[0].0);
            ctx.wide_masks.insert(dst, dst_m_chunks);
        }
        UnaryOp::Ident | UnaryOp::BitNot => {
            // Mask passes through (per-chunk)
            let n_dst = ISelContext::num_chunks(d_width);
            let mut dst_m_chunks = Vec::with_capacity(n_dst);
            for i in 0..n_dst {
                let m = sm_chunks.get(i).copied().unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                dst_m_chunks.push((m, 64));
            }
            ctx.set_mask(dst, dst_m_chunks[0].0);
            ctx.wide_masks.insert(dst, dst_m_chunks);
        }
        UnaryOp::Minus
        | UnaryOp::PopCount
        | UnaryOp::CountLeadingZeros
        | UnaryOp::CountTrailingZeros => {
            // Conservative: any X → all-X
            let has_x = any_chunk_has_x(ctx, block, &sm_chunks);
            let n_dst = ISelContext::num_chunks(d_width);
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });

            if d_width <= 64 {
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
                ctx.wide_masks.insert(dst, vec![(res, d_width)]);
            } else {
                let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                block.push(MInst::LoadImm {
                    dst: all_ones,
                    value: u64::MAX,
                });
                let mut dst_m_chunks = Vec::with_capacity(n_dst);
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
        UnaryOp::And | UnaryOp::LogicNot | UnaryOp::Or | UnaryOp::Xor => {
            // Reduction ops: result is 1-bit
            // AND: dominant-0 across all chunks
            // OR: dominant-1 across all chunks
            // XOR: any X → result X
            let sv_chunks: Vec<VReg> = if let Some(chunks) = ctx.wide_regs.get(&src).cloned() {
                chunks.iter().map(|c| c.0).collect()
            } else {
                vec![ctx.reg_map.get(src)]
            };

            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let has_x = any_chunk_has_x(ctx, block, &sm_chunks);

            match op {
                UnaryOp::And => {
                    // Check for definite-0 across all chunks: ~v & ~m
                    let mut any_def_zero = zero;
                    for i in 0..n_src {
                        let not_v = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: not_v,
                            src: sv_chunks[i.min(sv_chunks.len() - 1)],
                        });
                        let not_m = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: not_m,
                            src: sm_chunks[i],
                        });
                        let def_z = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: def_z,
                            lhs: not_v,
                            rhs: not_m,
                        });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: combined,
                            lhs: any_def_zero,
                            rhs: def_z,
                        });
                        any_def_zero = combined;
                    }
                    let has_def_zero = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: has_def_zero,
                        lhs: any_def_zero,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                    let x_mask = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: x_mask,
                        cond: has_x,
                        true_val: has_x,
                        false_val: zero,
                    });
                    let res = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: res,
                        cond: has_def_zero,
                        true_val: zero,
                        false_val: x_mask,
                    });
                    ctx.set_mask(dst, res);
                    ctx.wide_masks.insert(dst, vec![(res, d_width)]);
                }
                UnaryOp::LogicNot | UnaryOp::Or => {
                    // Check for definite-1 across all chunks: v & ~m
                    let mut any_def_one = zero;
                    for i in 0..n_src {
                        let not_m = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: not_m,
                            src: sm_chunks[i],
                        });
                        let def_one = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: def_one,
                            lhs: sv_chunks[i.min(sv_chunks.len() - 1)],
                            rhs: not_m,
                        });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: combined,
                            lhs: any_def_one,
                            rhs: def_one,
                        });
                        any_def_one = combined;
                    }
                    let has_def_one = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: has_def_one,
                        lhs: any_def_one,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                    let x_mask = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: x_mask,
                        cond: has_x,
                        true_val: has_x,
                        false_val: zero,
                    });
                    let res = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: res,
                        cond: has_def_one,
                        true_val: zero,
                        false_val: x_mask,
                    });
                    ctx.set_mask(dst, res);
                    ctx.wide_masks.insert(dst, vec![(res, d_width)]);
                }
                _ => {
                    // XOR: any X → result X
                    ctx.set_mask(dst, has_x);
                    ctx.wide_masks.insert(dst, vec![(has_x, d_width)]);
                }
            }
        }
    }
}

/// Lower the SystemVerilog truth state of a mux condition.
///
/// A vector condition is definitely true when at least one *known* bit is one.
/// It is unknown when there is no known-one bit and at least one X/Z bit; all
/// remaining conditions are definitely false.  Both returned VRegs are 0/1.
pub(super) fn lower_mux_condition_state(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    cond: RegisterId,
) -> (VReg, VReg) {
    let width = ctx.sir_width(&cond);
    let n_chunks = ISelContext::num_chunks(width).max(1);
    let value_chunks = ctx.get_wide_chunks(&cond, block);
    let mask_chunks = if ctx.four_state {
        get_wide_mask_chunks(ctx, block, &cond, n_chunks)
    } else {
        Vec::new()
    };

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let mut known_one_bits = zero;
    let mut unknown_bits = zero;

    for index in 0..n_chunks {
        let chunk_width = (width.saturating_sub(index * 64)).min(64);
        let value = value_chunks.get(index).map(|chunk| chunk.0).unwrap_or(zero);
        let mask = mask_chunks.get(index).copied().unwrap_or(zero);
        let (value, mask) = if chunk_width < 64 {
            let valid = mask_for_width(chunk_width);
            let masked_value = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked_value, value, valid);
            let masked_mask = if ctx.four_state {
                let masked_mask = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked_mask, mask, valid);
                masked_mask
            } else {
                zero
            };
            (masked_value, masked_mask)
        } else {
            (value, mask)
        };

        let known_ones = if ctx.four_state {
            let not_mask = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_mask,
                src: mask,
            });
            let known_ones = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: known_ones,
                lhs: value,
                rhs: not_mask,
            });
            known_ones
        } else {
            value
        };
        let next_known = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next_known,
            lhs: known_one_bits,
            rhs: known_ones,
        });
        known_one_bits = next_known;

        if ctx.four_state {
            let next_unknown = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: next_unknown,
                lhs: unknown_bits,
                rhs: mask,
            });
            unknown_bits = next_unknown;
        }
    }

    let is_true = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: is_true,
        lhs: known_one_bits,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    ctx.known_bits.insert(is_true, 1);

    let is_unknown = if ctx.four_state {
        let has_unknown = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: has_unknown,
            lhs: unknown_bits,
            rhs: zero,
            kind: CmpKind::Ne,
        });
        let is_not_true = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: is_not_true,
            lhs: is_true,
            rhs: zero,
            kind: CmpKind::Eq,
        });
        let is_unknown = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: is_unknown,
            lhs: has_unknown,
            rhs: is_not_true,
        });
        ctx.known_bits.insert(is_unknown, 1);
        is_unknown
    } else {
        zero
    };

    (is_true, is_unknown)
}

/// Merge one result chunk for an unknown four-state mux condition.
/// Identical 4-state bits are preserved; every differing bit becomes X.
pub(super) fn lower_four_state_mux_chunk(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    cond_is_true: VReg,
    cond_is_unknown: VReg,
    then_value: VReg,
    then_mask: VReg,
    else_value: VReg,
    else_mask: VReg,
    width: usize,
) -> (VReg, VReg) {
    let selected_value = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: selected_value,
        cond: cond_is_true,
        true_val: then_value,
        false_val: else_value,
    });
    let selected_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: selected_mask,
        cond: cond_is_true,
        true_val: then_mask,
        false_val: else_mask,
    });

    let value_diff = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Xor {
        dst: value_diff,
        lhs: then_value,
        rhs: else_value,
    });
    let mask_diff = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Xor {
        dst: mask_diff,
        lhs: then_mask,
        rhs: else_mask,
    });
    let diff = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: diff,
        lhs: value_diff,
        rhs: mask_diff,
    });
    let unknown_value = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: unknown_value,
        lhs: then_value,
        rhs: diff,
    });
    let unknown_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: unknown_mask,
        lhs: then_mask,
        rhs: diff,
    });

    let value = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: value,
        cond: cond_is_unknown,
        true_val: unknown_value,
        false_val: selected_value,
    });
    let mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: mask,
        cond: cond_is_unknown,
        true_val: unknown_mask,
        false_val: selected_mask,
    });

    if width < 64 {
        let logical_mask = mask_for_width(width);
        let masked_value = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, masked_value, value, logical_mask);
        let masked_mask = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, masked_mask, mask, logical_mask);
        (masked_value, masked_mask)
    } else {
        (value, mask)
    }
}
