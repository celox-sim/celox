//! Binary operation translation: dispatch to the float, scalar (at most
//! 64-bit) and wide paths, and the scalar operator families.

use cranelift::prelude::*;
use cranelift_frontend::FunctionBuilder;

use super::arith::{apply_d_width_mask, apply_d_width_mask_arith};
use super::core::{TransValue, get_cl_type, promote_to_physical};
use super::{SIRTranslator, TranslationState};
use crate::{BinaryOp, RegisterId};

impl SIRTranslator {
    pub(super) fn translate_binary_inst(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        lhs: &RegisterId,
        op: &BinaryOp,
        rhs: &RegisterId,
    ) {
        if let BinaryOp::Float { op, precision } = op {
            self.translate_float(
                state,
                *dst,
                *lhs,
                Some(*rhs),
                super::core::FloatOperation::Binary(*op),
                *precision,
            );
            return;
        }
        let l_width = state.register_map[lhs].width();
        let r_width = state.register_map[rhs].width();
        let d_width = state.register_map[dst].width();

        let common_logical_width = l_width.max(r_width).max(d_width);

        if common_logical_width <= 64 {
            self.translate_scalar_binary(
                state,
                dst,
                lhs,
                op,
                rhs,
                l_width,
                r_width,
                d_width,
                common_logical_width,
            );
        } else {
            self.translate_wide_binary(
                state,
                dst,
                lhs,
                op,
                rhs,
                l_width,
                r_width,
                d_width,
                common_logical_width,
            );
        }
    }

    /// A binary operation whose operands and result fit in 64 bits.
    fn translate_scalar_binary(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        lhs: &RegisterId,
        op: &BinaryOp,
        rhs: &RegisterId,
        l_width: usize,
        r_width: usize,
        d_width: usize,
        common_logical_width: usize,
    ) {
        // --- 64-bit or less: Cranelift Native Type Operation ---
        let common_ty = get_cl_type(common_logical_width);
        let signed_divrem = matches!(op, BinaryOp::DivS | BinaryOp::RemS);
        let any_divrem = matches!(
            op,
            BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
        );
        let signed_comparison = matches!(
            op,
            BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GtS | BinaryOp::GeS
        );
        let any_comparison = matches!(
            op,
            BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::EqCase
                | BinaryOp::NeCase
                | BinaryOp::EqWildcard
                | BinaryOp::NeWildcard
                | BinaryOp::LtU
                | BinaryOp::LtS
                | BinaryOp::LeU
                | BinaryOp::LeS
                | BinaryOp::GtU
                | BinaryOp::GtS
                | BinaryOp::GeU
                | BinaryOp::GeS
        );
        // Division signedness is carried by the opcode, not by either
        // operand's declaration. This also makes mixed signed/unsigned
        // operands deterministic after width promotion.
        let l_is_signed = if any_divrem {
            signed_divrem
        } else if any_comparison {
            signed_comparison
        } else if matches!(op, BinaryOp::Shr) {
            // A logical right shift must zero-fill from the operand's
            // logical width.  The SIR register can still carry the
            // source declaration's signedness after an equal-width
            // unsigned context conversion; sign-extending it to the
            // Cranelift physical type would turn `0xf9 >> 1` into
            // `0xfc` after truncation instead of `0x7c`.
            false
        } else {
            state.register_map[lhs].is_signed() || matches!(op, BinaryOp::Sar)
        };
        let r_is_signed = signed_divrem || signed_comparison;
        let l_val = state.regs[lhs].first_value(state.builder);
        let r_val = state.regs[rhs].first_value(state.builder);

        let l = promote_to_physical(state, l_val, l_width, l_is_signed, common_ty);
        let r = promote_to_physical(state, r_val, r_width, r_is_signed, common_ty);

        // Machine shifts wrap their counts. SAR must saturate at sign fill.
        let sar_count = if matches!(op, BinaryOp::Sar) {
            let limit = state
                .builder
                .ins()
                .iconst(common_ty, i64::from(common_ty.bits() - 1));
            let in_range = state.builder.ins().icmp(IntCC::UnsignedLessThan, r, limit);
            state.builder.ins().select(in_range, r, limit)
        } else {
            r
        };
        // Logical shifts by at least the physical width produce zero.
        // `r` is zero-extended from a logical width no wider than
        // `common_ty`, so this compares the real count without truncation.
        let logical_shift_in_range = if matches!(op, BinaryOp::Shl | BinaryOp::Shr) {
            Some(state.builder.ins().icmp_imm_u(
                IntCC::UnsignedLessThan,
                r,
                i64::from(common_ty.bits()),
            ))
        } else {
            None
        };
        let saturate_logical_shift = |builder: &mut FunctionBuilder, shifted: Value| {
            let in_range = logical_shift_in_range.expect("logical shift count range is computed");
            let zero = builder.ins().iconst(common_ty, 0);
            builder.ins().select(in_range, shifted, zero)
        };

        let build_icmp = |builder: &mut FunctionBuilder, cc: IntCC| {
            let b1_res = builder.ins().icmp(cc, l, r);
            let zero = builder.ins().iconst(common_ty, 0);
            let one = builder.ins().iconst(common_ty, 1);
            builder.ins().select(b1_res, one, zero)
        };

        let mut res_v = match op {
            BinaryOp::Float { .. } => unreachable!("lowered separately"),
            BinaryOp::Add => state.builder.ins().iadd(l, r),
            BinaryOp::Sub => state.builder.ins().isub(l, r),
            BinaryOp::Mul => state.builder.ins().imul(l, r),
            BinaryOp::DivU => {
                let zero = state.builder.ins().iconst(common_ty, 0);
                let one = state.builder.ins().iconst(common_ty, 1);
                let is_zero = state.builder.ins().icmp(IntCC::Equal, r, zero);
                let safe_r = state.builder.ins().select(is_zero, one, r);
                let div_result = state.builder.ins().udiv(l, safe_r);
                state.builder.ins().select(is_zero, zero, div_result)
            }
            BinaryOp::RemU => {
                let zero = state.builder.ins().iconst(common_ty, 0);
                let one = state.builder.ins().iconst(common_ty, 1);
                let is_zero = state.builder.ins().icmp(IntCC::Equal, r, zero);
                let safe_r = state.builder.ins().select(is_zero, one, r);
                let rem_result = state.builder.ins().urem(l, safe_r);
                state.builder.ins().select(is_zero, zero, rem_result)
            }
            BinaryOp::DivS | BinaryOp::RemS => scalar_signed_division(state, op, l, r, common_ty),
            BinaryOp::And => state.builder.ins().band(l, r),
            BinaryOp::Or => state.builder.ins().bor(l, r),
            BinaryOp::Xor => state.builder.ins().bxor(l, r),
            BinaryOp::Shr => {
                let raw = state.builder.ins().ushr(l, r);
                let shifted = saturate_logical_shift(state.builder, raw);
                apply_d_width_mask(state, shifted, common_ty, d_width)
            }
            BinaryOp::Shl => {
                let raw = state.builder.ins().ishl(l, r);
                let shifted = saturate_logical_shift(state.builder, raw);
                apply_d_width_mask(state, shifted, common_ty, d_width)
            }
            BinaryOp::Sar => {
                let raw_shifted = state.builder.ins().sshr(l, sar_count);
                apply_d_width_mask_arith(state, raw_shifted, common_ty, d_width)
            }
            BinaryOp::Eq => build_icmp(state.builder, IntCC::Equal),
            BinaryOp::Ne => build_icmp(state.builder, IntCC::NotEqual),
            BinaryOp::EqCase => build_icmp(state.builder, IntCC::Equal),
            BinaryOp::NeCase => build_icmp(state.builder, IntCC::NotEqual),
            BinaryOp::LtS => build_icmp(state.builder, IntCC::SignedLessThan),
            BinaryOp::LtU => build_icmp(state.builder, IntCC::UnsignedLessThan),
            BinaryOp::GtS => build_icmp(state.builder, IntCC::SignedGreaterThan),
            BinaryOp::GtU => build_icmp(state.builder, IntCC::UnsignedGreaterThan),
            BinaryOp::LeS => build_icmp(state.builder, IntCC::SignedLessThanOrEqual),
            BinaryOp::LeU => build_icmp(state.builder, IntCC::UnsignedLessThanOrEqual),
            BinaryOp::GeS => build_icmp(state.builder, IntCC::SignedGreaterThanOrEqual),
            BinaryOp::GeU => build_icmp(state.builder, IntCC::UnsignedGreaterThanOrEqual),
            BinaryOp::LogicAnd => {
                let zero = state.builder.ins().iconst(common_ty, 0);
                let l_bool = state.builder.ins().icmp(IntCC::NotEqual, l, zero);
                let r_bool = state.builder.ins().icmp(IntCC::NotEqual, r, zero);
                let res_bool = state.builder.ins().band(l_bool, r_bool);
                let one = state.builder.ins().iconst(common_ty, 1);
                state.builder.ins().select(res_bool, one, zero)
            }
            BinaryOp::LogicOr => {
                let zero = state.builder.ins().iconst(common_ty, 0);
                let l_bool = state.builder.ins().icmp(IntCC::NotEqual, l, zero);
                let r_bool = state.builder.ins().icmp(IntCC::NotEqual, r, zero);
                let res_bool = state.builder.ins().bor(l_bool, r_bool);
                let one = state.builder.ins().iconst(common_ty, 1);
                state.builder.ins().select(res_bool, one, zero)
            }
            BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
                // Value computation: same as Eq/Ne (compare l and r).
                // Wildcard semantics are handled in the mask computation.
                build_icmp(
                    state.builder,
                    if matches!(op, BinaryOp::EqWildcard) {
                        IntCC::Equal
                    } else {
                        IntCC::NotEqual
                    },
                )
            }
        };

        let dst_ty = get_cl_type(d_width);

        if self.options.four_state {
            let l_m_val = state.regs[lhs]
                .first_mask(state.builder)
                .unwrap_or_else(|| state.builder.ins().iconst(common_ty, 0));
            let r_m_val = state.regs[rhs]
                .first_mask(state.builder)
                .unwrap_or_else(|| state.builder.ins().iconst(common_ty, 0));
            let l_m = promote_to_physical(state, l_m_val, l_width, l_is_signed, common_ty);
            let r_m = promote_to_physical(state, r_m_val, r_width, r_is_signed, common_ty);

            let res_m = scalar_binary_mask(
                state,
                op,
                l,
                r,
                l_m,
                r_m,
                common_ty,
                d_width,
                sar_count,
                &saturate_logical_shift,
                &mut res_v,
            );

            let final_res_v = promote_to_physical(state, res_v, d_width, false, dst_ty);
            let final_res_m = promote_to_physical(state, res_m, d_width, false, dst_ty);
            // Shifts preserve Z. An unknown shift count instead produces all X.
            let normalized_v = if matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
                let has_x = state.builder.ins().icmp_imm_u(IntCC::NotEqual, r_m, 0);
                state.builder.ins().select(has_x, final_res_m, final_res_v)
            } else {
                state.builder.ins().bor(final_res_v, final_res_m)
            };
            state.regs.insert(
                *dst,
                TransValue::FourState {
                    values: vec![normalized_v],
                    masks: vec![final_res_m],
                },
            );
        } else {
            let final_res = promote_to_physical(state, res_v, d_width, false, dst_ty);
            state
                .regs
                .insert(*dst, TransValue::TwoState(vec![final_res]));
        }
    }
}

/// Scalar `DivS` and `RemS`.
fn scalar_signed_division(
    state: &mut TranslationState,
    op: &BinaryOp,
    l: Value,
    r: Value,
    common_ty: Type,
) -> Value {
    let zero = state.builder.ins().iconst(common_ty, 0);
    let one = state.builder.ins().iconst(common_ty, 1);
    let neg_one = state.builder.ins().iconst(common_ty, -1);
    let physical_bits = common_ty.bits() as usize;
    let min_payload = 1u64 << (physical_bits - 1);
    let min = state.builder.ins().iconst(common_ty, min_payload as i64);
    let is_zero = state.builder.ins().icmp(IntCC::Equal, r, zero);
    let is_min = state.builder.ins().icmp(IntCC::Equal, l, min);
    let is_neg_one = state.builder.ins().icmp(IntCC::Equal, r, neg_one);
    let is_overflow = state.builder.ins().band(is_min, is_neg_one);
    let unsafe_divisor = state.builder.ins().bor(is_zero, is_overflow);
    // Both exceptional cases can safely execute with divisor +1:
    // zero is selected to the SIR-defined zero result below, while
    // MIN / -1 naturally produces the wrapped MIN quotient (or 0 rem).
    let safe_r = state.builder.ins().select(unsafe_divisor, one, r);
    let raw = if matches!(op, BinaryOp::DivS) {
        state.builder.ins().sdiv(l, safe_r)
    } else {
        state.builder.ins().srem(l, safe_r)
    };
    state.builder.ins().select(is_zero, zero, raw)
}

/// The mask of a scalar binary operation. Case and wildcard equality also
/// replace the value `res_v`.
fn scalar_binary_mask(
    state: &mut TranslationState,
    op: &BinaryOp,
    l: Value,
    r: Value,
    l_m: Value,
    r_m: Value,
    common_ty: Type,
    d_width: usize,
    sar_count: Value,
    saturate_logical_shift: &impl Fn(&mut FunctionBuilder, Value) -> Value,
    res_v: &mut Value,
) -> Value {
    match op {
        BinaryOp::And => {
            let m1 = state.builder.ins().band(l_m, r_m);
            let m2 = state.builder.ins().band(l_m, r);
            let m3 = state.builder.ins().band(r_m, l);
            let m_tmp = state.builder.ins().bor(m1, m2);
            state.builder.ins().bor(m_tmp, m3)
        }
        BinaryOp::Or => {
            let m1 = state.builder.ins().band(l_m, r_m);
            let not_b_v = state.builder.ins().bnot(r);
            let m2 = state.builder.ins().band(l_m, not_b_v);
            let not_a_v = state.builder.ins().bnot(l);
            let m3 = state.builder.ins().band(r_m, not_a_v);
            let m_tmp = state.builder.ins().bor(m1, m2);
            state.builder.ins().bor(m_tmp, m3)
        }
        BinaryOp::Xor => state.builder.ins().bor(l_m, r_m),
        BinaryOp::Shr => scalar_shr_mask(
            state,
            r,
            l_m,
            r_m,
            common_ty,
            d_width,
            saturate_logical_shift,
        ),
        BinaryOp::Shl => scalar_shl_mask(
            state,
            r,
            l_m,
            r_m,
            common_ty,
            d_width,
            saturate_logical_shift,
        ),
        BinaryOp::Sar => scalar_sar_mask(state, l_m, r_m, common_ty, d_width, sar_count),
        BinaryOp::Eq | BinaryOp::Ne => scalar_equality_mask(state, l, r, l_m, r_m, common_ty),
        BinaryOp::EqCase | BinaryOp::NeCase => {
            scalar_case_equality(state, op, l, r, l_m, r_m, common_ty, res_v)
        }
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            scalar_wildcard_equality(state, op, l, r, l_m, r_m, common_ty, res_v)
        }
        BinaryOp::LogicAnd => scalar_logic_and_mask(state, l, r, l_m, r_m, common_ty),
        BinaryOp::LogicOr => scalar_logic_or_mask(state, l, r, l_m, r_m, common_ty),
        _ => scalar_conservative_mask(state, op, r, l_m, r_m, common_ty),
    }
}

/// The mask of a scalar `Shr`.
fn scalar_shr_mask(
    state: &mut TranslationState,
    r: Value,
    l_m: Value,
    r_m: Value,
    common_ty: Type,
    d_width: usize,
    saturate_logical_shift: &impl Fn(&mut FunctionBuilder, Value) -> Value,
) -> Value {
    let zero = state.builder.ins().iconst(common_ty, 0);
    let shift_amt_has_x = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
    let raw_m = state.builder.ins().ushr(l_m, r);
    let shifted_m = saturate_logical_shift(state.builder, raw_m);
    let all_ones = state.builder.ins().iconst(common_ty, -1);
    let m = state
        .builder
        .ins()
        .select(shift_amt_has_x, all_ones, shifted_m);
    apply_d_width_mask(state, m, common_ty, d_width)
}

/// The mask of a scalar `Shl`.
fn scalar_shl_mask(
    state: &mut TranslationState,
    r: Value,
    l_m: Value,
    r_m: Value,
    common_ty: Type,
    d_width: usize,
    saturate_logical_shift: &impl Fn(&mut FunctionBuilder, Value) -> Value,
) -> Value {
    let zero = state.builder.ins().iconst(common_ty, 0);
    let shift_amt_has_x = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
    let raw_m = state.builder.ins().ishl(l_m, r);
    let shifted_m = saturate_logical_shift(state.builder, raw_m);
    let all_ones = state.builder.ins().iconst(common_ty, -1);
    let m = state
        .builder
        .ins()
        .select(shift_amt_has_x, all_ones, shifted_m);
    apply_d_width_mask(state, m, common_ty, d_width)
}

/// The mask of a scalar `Sar`.
fn scalar_sar_mask(
    state: &mut TranslationState,
    l_m: Value,
    r_m: Value,
    common_ty: Type,
    d_width: usize,
    sar_count: Value,
) -> Value {
    let zero = state.builder.ins().iconst(common_ty, 0);
    let shift_amt_has_x = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
    let shifted_m = state.builder.ins().sshr(l_m, sar_count);
    let all_ones = state.builder.ins().iconst(common_ty, -1);
    let m = state
        .builder
        .ins()
        .select(shift_amt_has_x, all_ones, shifted_m);
    apply_d_width_mask_arith(state, m, common_ty, d_width)
}

/// The mask of a scalar `Eq` or `Ne`.
fn scalar_equality_mask(
    state: &mut TranslationState,
    l: Value,
    r: Value,
    l_m: Value,
    r_m: Value,
    common_ty: Type,
) -> Value {
    let unknown = state.builder.ins().bor(l_m, r_m);
    let known = state.builder.ins().bnot(unknown);
    let diff = state.builder.ins().bxor(l, r);
    let mismatch = state.builder.ins().band(diff, known);
    let no_mismatch = state.builder.ins().icmp_imm_u(IntCC::Equal, mismatch, 0);
    let has_unknown = state.builder.ins().icmp_imm_u(IntCC::NotEqual, unknown, 0);
    let uncertain = state.builder.ins().band(no_mismatch, has_unknown);
    let zero = state.builder.ins().iconst(common_ty, 0);
    let one = state.builder.ins().iconst(common_ty, 1);
    state.builder.ins().select(uncertain, one, zero)
}

/// Scalar `EqCase` and `NeCase` on four-state operands: the value `res_v`,
/// and the mask, which is zero.
fn scalar_case_equality(
    state: &mut TranslationState,
    op: &BinaryOp,
    l: Value,
    r: Value,
    l_m: Value,
    r_m: Value,
    common_ty: Type,
    res_v: &mut Value,
) -> Value {
    let value_diff = state.builder.ins().bxor(l, r);
    let mask_diff = state.builder.ins().bxor(l_m, r_m);
    let diff = state.builder.ins().bor(value_diff, mask_diff);
    let zero = state.builder.ins().iconst(common_ty, 0);
    let matches = state.builder.ins().icmp(
        if matches!(op, BinaryOp::EqCase) {
            IntCC::Equal
        } else {
            IntCC::NotEqual
        },
        diff,
        zero,
    );
    let one = state.builder.ins().iconst(common_ty, 1);
    *res_v = state.builder.ins().select(matches, one, zero);
    zero
}

/// Scalar `EqWildcard` and `NeWildcard` on four-state operands: the value
/// `res_v`, and the mask.
fn scalar_wildcard_equality(
    state: &mut TranslationState,
    op: &BinaryOp,
    l: Value,
    r: Value,
    l_m: Value,
    r_m: Value,
    common_ty: Type,
    res_v: &mut Value,
) -> Value {
    // IEEE 1800 ==?/!=?: RHS X/Z bits are wildcards (don't care).
    let zero = state.builder.ins().iconst(common_ty, 0);
    let compare_mask = state.builder.ins().bnot(r_m);
    let l_definite = state.builder.ins().bnot(l_m);

    // Positions where both LHS is definite AND RHS is non-wildcard
    let definite_compare = state.builder.ins().band(compare_mask, l_definite);

    // Check for definite mismatch at those positions
    let l_xor_r = state.builder.ins().bxor(l, r);
    let mismatch_bits = state.builder.ins().band(l_xor_r, definite_compare);
    let has_definite_mismatch = state
        .builder
        .ins()
        .icmp(IntCC::NotEqual, mismatch_bits, zero);

    // LHS X at non-wildcard positions
    let x_at_compared = state.builder.ins().band(l_m, compare_mask);
    let has_x_at_compared = state
        .builder
        .ins()
        .icmp(IntCC::NotEqual, x_at_compared, zero);

    // Mask: definite mismatch → 0 (definite); X at compared → all-X; else → 0
    let all_ones = state.builder.ins().iconst(common_ty, -1);
    let x_mask = state
        .builder
        .ins()
        .select(has_x_at_compared, all_ones, zero);
    let mask = state
        .builder
        .ins()
        .select(has_definite_mismatch, zero, x_mask);

    // Override value: compare only at definite non-wildcard positions
    let l_eff = state.builder.ins().band(l, definite_compare);
    let r_eff = state.builder.ins().band(r, definite_compare);
    let cmp_result = state.builder.ins().icmp(
        if matches!(op, BinaryOp::EqWildcard) {
            IntCC::Equal
        } else {
            IntCC::NotEqual
        },
        l_eff,
        r_eff,
    );
    let one = state.builder.ins().iconst(common_ty, 1);
    *res_v = state.builder.ins().select(cmp_result, one, zero);

    mask
}

/// The mask of a scalar `LogicAnd`.
fn scalar_logic_and_mask(
    state: &mut TranslationState,
    l: Value,
    r: Value,
    l_m: Value,
    r_m: Value,
    common_ty: Type,
) -> Value {
    // IEEE 1800: 0 is dominant for && (0 && x = 0)
    let zero = state.builder.ins().iconst(common_ty, 0);
    // Definite false: value=0 AND mask=0 → (v | m) == 0
    let l_val_or_mask = state.builder.ins().bor(l, l_m);
    let r_val_or_mask = state.builder.ins().bor(r, r_m);
    let l_def_false = state.builder.ins().icmp(IntCC::Equal, l_val_or_mask, zero);
    let r_def_false = state.builder.ins().icmp(IntCC::Equal, r_val_or_mask, zero);
    let either_def_false = state.builder.ins().bor(l_def_false, r_def_false);

    let any_x_l = state.builder.ins().icmp(IntCC::NotEqual, l_m, zero);
    let any_x_r = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
    let any_x = state.builder.ins().bor(any_x_l, any_x_r);
    let all_ones = state.builder.ins().iconst(common_ty, -1);
    let conservative = state.builder.ins().select(any_x, all_ones, zero);
    state
        .builder
        .ins()
        .select(either_def_false, zero, conservative)
}

/// The mask of a scalar `LogicOr`.
fn scalar_logic_or_mask(
    state: &mut TranslationState,
    l: Value,
    r: Value,
    l_m: Value,
    r_m: Value,
    common_ty: Type,
) -> Value {
    // IEEE 1800: 1 is dominant for || (1 || x = 1)
    let zero = state.builder.ins().iconst(common_ty, 0);
    // Definite true: v & ~m extracts only definite-1 bits
    let not_l_m = state.builder.ins().bnot(l_m);
    let l_definite_v = state.builder.ins().band(l, not_l_m);
    let not_r_m = state.builder.ins().bnot(r_m);
    let r_definite_v = state.builder.ins().band(r, not_r_m);
    let l_def_true = state
        .builder
        .ins()
        .icmp(IntCC::NotEqual, l_definite_v, zero);
    let r_def_true = state
        .builder
        .ins()
        .icmp(IntCC::NotEqual, r_definite_v, zero);
    let either_def_true = state.builder.ins().bor(l_def_true, r_def_true);

    let any_x_l = state.builder.ins().icmp(IntCC::NotEqual, l_m, zero);
    let any_x_r = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
    let any_x = state.builder.ins().bor(any_x_l, any_x_r);
    let all_ones = state.builder.ins().iconst(common_ty, -1);
    let conservative = state.builder.ins().select(any_x, all_ones, zero);
    state
        .builder
        .ins()
        .select(either_def_true, zero, conservative)
}

/// The mask of the remaining scalar operations (arithmetic, division and
/// ordered comparison): all X when an operand has an unknown bit.
fn scalar_conservative_mask(
    state: &mut TranslationState,
    op: &BinaryOp,
    r: Value,
    l_m: Value,
    r_m: Value,
    common_ty: Type,
) -> Value {
    let zero = state.builder.ins().iconst(common_ty, 0);
    let any_x_l = state.builder.ins().icmp(IntCC::NotEqual, l_m, zero);
    let any_x_r = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
    let mut any_x = state.builder.ins().bor(any_x_l, any_x_r);
    if matches!(
        op,
        BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
    ) {
        // IEEE 1800-2023 11.4.3: a zero divisor also produces all X.
        let zero_divisor = state.builder.ins().icmp(IntCC::Equal, r, zero);
        any_x = state.builder.ins().bor(any_x, zero_divisor);
    }

    let all_ones = state.builder.ins().iconst(common_ty, -1);
    state.builder.ins().select(any_x, all_ones, zero)
}
