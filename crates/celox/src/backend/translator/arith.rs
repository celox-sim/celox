use cranelift::prelude::*;
use cranelift_frontend::FunctionBuilder;

use super::core::{TransValue, cast_type, get_chunk_as_i64, get_cl_type};
use super::{SIRTranslator, TranslationState, wide_ops};
use crate::backend::translator::core::promote_to_physical;
use crate::ir::{BinaryOp, RegisterId, SIRValue, UnaryOp};

impl SIRTranslator {
    pub(super) fn translate_imm_inst(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        val: &SIRValue,
    ) {
        let width = state.register_map[dst].width();

        // 1. Determine number of chunks to generate
        // Single Value (i8~i64) if width <= 64
        // Vec of i64 if width > 64
        let num_chunks = if width <= 64 { 1 } else { width.div_ceil(64) };
        let mut cl_chunks = Vec::with_capacity(num_chunks);

        // 2. Extract data from BigUint in 64-bit units (Little-Endian)
        let digits = val.payload.to_u64_digits();

        if width <= 64 {
            // --- Cranelift Native Type (i8 ~ i64) ---
            let ty = get_cl_type(width);
            // i8 ~ i64
            let raw = digits.first().copied().unwrap_or(0);
            cl_chunks.push(state.builder.ins().iconst(ty, raw as i64));
        } else {
            // --- Wide Width over 64 bits (Array of i64 chunks) ---
            for i in 0..num_chunks {
                let d = digits.get(i).copied().unwrap_or(0);
                cl_chunks.push(state.builder.ins().iconst(types::I64, d as i64));
            }
        }

        if self.options.four_state {
            let mut cl_masks = Vec::with_capacity(num_chunks);
            if width <= 64 {
                let ty = get_cl_type(width);
                cl_masks.push(state.builder.ins().iconst(ty, 0));
            } else {
                for _ in 0..num_chunks {
                    cl_masks.push(state.builder.ins().iconst(types::I64, 0));
                }
            }
            state.regs.insert(
                *dst,
                TransValue::FourState {
                    values: cl_chunks,
                    masks: cl_masks,
                },
            );
        } else {
            state.regs.insert(*dst, TransValue::TwoState(cl_chunks));
        }
    }

    pub(super) fn translate_concat_inst(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        args: &[RegisterId],
    ) {
        // Determine destination width and chunk count
        let dst_width = state.register_map[dst].width();
        let num_chunks = if dst_width <= 64 {
            1
        } else {
            dst_width.div_ceil(64)
        };

        let mut dst_chunks_v = vec![state.builder.ins().iconst(types::I64, 0); num_chunks];
        let mut dst_chunks_m = vec![state.builder.ins().iconst(types::I64, 0); num_chunks];

        // HDL Concatenation Rule: Concat({a, b}) = (a << width(b)) | b
        // The last argument is positioned at the LSB (offset 0), while preceding arguments
        // are shifted into higher bit positions.
        let mut current_bit_offset: usize = 0;

        for arg_reg in args.iter().rev() {
            let arg_width = state.register_map[arg_reg].width();
            let arg_chunks_v = state.regs[arg_reg].values();
            let arg_masks_owned;
            let arg_chunks_m = if self.options.four_state {
                match state.regs[arg_reg].masks() {
                    Some(m) => m,
                    None => {
                        let ty = get_cl_type(arg_width);
                        arg_masks_owned = vec![state.builder.ins().iconst(ty, 0)];
                        &arg_masks_owned
                    }
                }
            } else {
                &[]
            };

            // Since args can be > 64 bits, we iterate arg chunks.
            let arg_num_chunks = if arg_width <= 64 {
                1
            } else {
                arg_width.div_ceil(64)
            };

            for i in 0..arg_num_chunks {
                let chunk_val_v = get_chunk_as_i64(state.builder, arg_chunks_v, i);
                let chunk_val_m = if self.options.four_state {
                    get_chunk_as_i64(state.builder, arg_chunks_m, i)
                } else {
                    state.builder.ins().iconst(types::I64, 0)
                };

                let abs_bit_offset = current_bit_offset + i * 64;
                let dst_chunk_idx = abs_bit_offset / 64;
                let bit_shift = abs_bit_offset % 64;
                let arg_chunk_bits = arg_width.saturating_sub(i * 64).min(64);

                // 1. Write to dst_chunks[dst_chunk_idx]
                if dst_chunk_idx < num_chunks {
                    let shifted_v = state.builder.ins().ishl_imm(chunk_val_v, bit_shift as i64);
                    dst_chunks_v[dst_chunk_idx] = state
                        .builder
                        .ins()
                        .bor(dst_chunks_v[dst_chunk_idx], shifted_v);

                    if self.options.four_state {
                        let shifted_m = state.builder.ins().ishl_imm(chunk_val_m, bit_shift as i64);
                        dst_chunks_m[dst_chunk_idx] = state
                            .builder
                            .ins()
                            .bor(dst_chunks_m[dst_chunk_idx], shifted_m);
                    }
                }

                // 2. Write overlap to dst_chunks[dst_chunk_idx + 1] if any
                if bit_shift > 0
                    && (dst_chunk_idx + 1) < num_chunks
                    && (bit_shift + arg_chunk_bits > 64)
                {
                    let shift_down = 64 - bit_shift;
                    let shifted_down_v =
                        state.builder.ins().ushr_imm(chunk_val_v, shift_down as i64);
                    dst_chunks_v[dst_chunk_idx + 1] = state
                        .builder
                        .ins()
                        .bor(dst_chunks_v[dst_chunk_idx + 1], shifted_down_v);

                    if self.options.four_state {
                        let shifted_down_m =
                            state.builder.ins().ushr_imm(chunk_val_m, shift_down as i64);
                        dst_chunks_m[dst_chunk_idx + 1] = state
                            .builder
                            .ins()
                            .bor(dst_chunks_m[dst_chunk_idx + 1], shifted_down_m);
                    }
                }
            }

            current_bit_offset += arg_width;
        }

        // Truncate to native type if <= 64
        if dst_width <= 64 {
            let ty = get_cl_type(dst_width);
            let val_v = cast_type(state.builder, dst_chunks_v[0], ty);

            if self.options.four_state {
                let val_m = cast_type(state.builder, dst_chunks_m[0], ty);
                // IEEE 1800 normalization: v &= ~m
                let not_m = state.builder.ins().bnot(val_m);
                let safe_v = state.builder.ins().band(val_v, not_m);
                state.regs.insert(
                    *dst,
                    TransValue::FourState {
                        values: vec![safe_v],
                        masks: vec![val_m],
                    },
                );
            } else {
                state.regs.insert(*dst, TransValue::TwoState(vec![val_v]));
            }
        } else if self.options.four_state {
            // IEEE 1800 normalization: v &= ~m per chunk
            let safe_chunks_v: Vec<Value> = dst_chunks_v
                .iter()
                .zip(dst_chunks_m.iter())
                .map(|(&v, &m)| {
                    let not_m = state.builder.ins().bnot(m);
                    state.builder.ins().band(v, not_m)
                })
                .collect();
            state.regs.insert(
                *dst,
                TransValue::FourState {
                    values: safe_chunks_v,
                    masks: dst_chunks_m,
                },
            );
        } else {
            state.regs.insert(*dst, TransValue::TwoState(dst_chunks_v));
        }
    }

    pub(super) fn translate_binary_inst(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        lhs: &RegisterId,
        op: &BinaryOp,
        rhs: &RegisterId,
    ) {
        let l_width = state.register_map[lhs].width();
        let r_width = state.register_map[rhs].width();
        let d_width = state.register_map[dst].width();

        let common_logical_width = l_width.max(r_width).max(d_width);

        if common_logical_width <= 64 {
            // --- 64-bit or less: Cranelift Native Type Operation ---
            let common_ty = get_cl_type(common_logical_width);
            let l_is_signed = state.register_map[lhs].is_signed() || matches!(op, BinaryOp::Sar);
            let l_val = state.regs[lhs].values()[0];
            let r_val = state.regs[rhs].values()[0];

            let l = promote_to_physical(state, l_val, l_width, l_is_signed, common_ty);
            let r = promote_to_physical(state, r_val, r_width, false, common_ty);

            let build_icmp = |builder: &mut FunctionBuilder, cc: IntCC| {
                let b1_res = builder.ins().icmp(cc, l, r);
                let zero = builder.ins().iconst(common_ty, 0);
                let one = builder.ins().iconst(common_ty, 1);
                builder.ins().select(b1_res, one, zero)
            };

            let mut res_v = match op {
                BinaryOp::Add => state.builder.ins().iadd(l, r),
                BinaryOp::Sub => state.builder.ins().isub(l, r),
                BinaryOp::Mul => state.builder.ins().imul(l, r),
                BinaryOp::Div => {
                    let zero = state.builder.ins().iconst(common_ty, 0);
                    let one = state.builder.ins().iconst(common_ty, 1);
                    let is_zero = state.builder.ins().icmp(IntCC::Equal, r, zero);
                    let safe_r = state.builder.ins().select(is_zero, one, r);
                    let div_result = state.builder.ins().udiv(l, safe_r);
                    state.builder.ins().select(is_zero, zero, div_result)
                }
                BinaryOp::Rem => {
                    let zero = state.builder.ins().iconst(common_ty, 0);
                    let one = state.builder.ins().iconst(common_ty, 1);
                    let is_zero = state.builder.ins().icmp(IntCC::Equal, r, zero);
                    let safe_r = state.builder.ins().select(is_zero, one, r);
                    let rem_result = state.builder.ins().urem(l, safe_r);
                    state.builder.ins().select(is_zero, zero, rem_result)
                }
                BinaryOp::And => state.builder.ins().band(l, r),
                BinaryOp::Or => state.builder.ins().bor(l, r),
                BinaryOp::Xor => state.builder.ins().bxor(l, r),
                BinaryOp::Shr => {
                    let shifted = state.builder.ins().ushr(l, r);
                    apply_d_width_mask(state, shifted, common_ty, d_width)
                }
                BinaryOp::Shl => {
                    let shifted = state.builder.ins().ishl(l, r);
                    apply_d_width_mask(state, shifted, common_ty, d_width)
                }
                BinaryOp::Sar => {
                    let raw_shifted = state.builder.ins().sshr(l, r);
                    apply_d_width_mask_arith(state, raw_shifted, common_ty, d_width)
                }
                BinaryOp::Eq => build_icmp(state.builder, IntCC::Equal),
                BinaryOp::Ne => build_icmp(state.builder, IntCC::NotEqual),
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
                    .masks()
                    .map(|m| m[0])
                    .unwrap_or_else(|| state.builder.ins().iconst(common_ty, 0));
                let r_m_val = state.regs[rhs]
                    .masks()
                    .map(|m| m[0])
                    .unwrap_or_else(|| state.builder.ins().iconst(common_ty, 0));
                let l_m = promote_to_physical(state, l_m_val, l_width, l_is_signed, common_ty);
                let r_m = promote_to_physical(state, r_m_val, r_width, false, common_ty);

                let res_m = match op {
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
                    BinaryOp::Shr => {
                        let zero = state.builder.ins().iconst(common_ty, 0);
                        let shift_amt_has_x = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
                        let shifted_m = state.builder.ins().ushr(l_m, r);
                        let all_ones = state.builder.ins().iconst(common_ty, -1);
                        let m = state
                            .builder
                            .ins()
                            .select(shift_amt_has_x, all_ones, shifted_m);
                        apply_d_width_mask(state, m, common_ty, d_width)
                    }
                    BinaryOp::Shl => {
                        let zero = state.builder.ins().iconst(common_ty, 0);
                        let shift_amt_has_x = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
                        let shifted_m = state.builder.ins().ishl(l_m, r);
                        let all_ones = state.builder.ins().iconst(common_ty, -1);
                        let m = state
                            .builder
                            .ins()
                            .select(shift_amt_has_x, all_ones, shifted_m);
                        apply_d_width_mask(state, m, common_ty, d_width)
                    }
                    BinaryOp::Sar => {
                        let zero = state.builder.ins().iconst(common_ty, 0);
                        let shift_amt_has_x = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
                        let shifted_m = state.builder.ins().sshr(l_m, r);
                        let all_ones = state.builder.ins().iconst(common_ty, -1);
                        let m = state
                            .builder
                            .ins()
                            .select(shift_amt_has_x, all_ones, shifted_m);
                        apply_d_width_mask_arith(state, m, common_ty, d_width)
                    }
                    BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
                        // IEEE 1800 ==?/!=?: RHS X/Z bits are wildcards (don't care).
                        let zero = state.builder.ins().iconst(common_ty, 0);
                        let compare_mask = state.builder.ins().bnot(r_m);
                        let l_definite = state.builder.ins().bnot(l_m);

                        // Positions where both LHS is definite AND RHS is non-wildcard
                        let definite_compare =
                            state.builder.ins().band(compare_mask, l_definite);

                        // Check for definite mismatch at those positions
                        let l_xor_r = state.builder.ins().bxor(l, r);
                        let mismatch_bits =
                            state.builder.ins().band(l_xor_r, definite_compare);
                        let has_definite_mismatch = state.builder.ins().icmp(
                            IntCC::NotEqual,
                            mismatch_bits,
                            zero,
                        );

                        // LHS X at non-wildcard positions
                        let x_at_compared = state.builder.ins().band(l_m, compare_mask);
                        let has_x_at_compared = state.builder.ins().icmp(
                            IntCC::NotEqual,
                            x_at_compared,
                            zero,
                        );

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
                        res_v = state.builder.ins().select(cmp_result, one, zero);

                        mask
                    }
                    BinaryOp::LogicAnd => {
                        // IEEE 1800: 0 is dominant for && (0 && x = 0)
                        let zero = state.builder.ins().iconst(common_ty, 0);
                        // Definite false: value=0 AND mask=0 → (v | m) == 0
                        let l_val_or_mask = state.builder.ins().bor(l, l_m);
                        let r_val_or_mask = state.builder.ins().bor(r, r_m);
                        let l_def_false = state
                            .builder
                            .ins()
                            .icmp(IntCC::Equal, l_val_or_mask, zero);
                        let r_def_false = state
                            .builder
                            .ins()
                            .icmp(IntCC::Equal, r_val_or_mask, zero);
                        let either_def_false =
                            state.builder.ins().bor(l_def_false, r_def_false);

                        let any_x_l =
                            state.builder.ins().icmp(IntCC::NotEqual, l_m, zero);
                        let any_x_r =
                            state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
                        let any_x = state.builder.ins().bor(any_x_l, any_x_r);
                        let all_ones = state.builder.ins().iconst(common_ty, -1);
                        let conservative =
                            state.builder.ins().select(any_x, all_ones, zero);
                        state
                            .builder
                            .ins()
                            .select(either_def_false, zero, conservative)
                    }
                    BinaryOp::LogicOr => {
                        // IEEE 1800: 1 is dominant for || (1 || x = 1)
                        let zero = state.builder.ins().iconst(common_ty, 0);
                        // Definite true: at least one definite-1 bit (v != 0, since normalized)
                        let l_def_true =
                            state.builder.ins().icmp(IntCC::NotEqual, l, zero);
                        let r_def_true =
                            state.builder.ins().icmp(IntCC::NotEqual, r, zero);
                        let either_def_true =
                            state.builder.ins().bor(l_def_true, r_def_true);

                        let any_x_l =
                            state.builder.ins().icmp(IntCC::NotEqual, l_m, zero);
                        let any_x_r =
                            state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
                        let any_x = state.builder.ins().bor(any_x_l, any_x_r);
                        let all_ones = state.builder.ins().iconst(common_ty, -1);
                        let conservative =
                            state.builder.ins().select(any_x, all_ones, zero);
                        state
                            .builder
                            .ins()
                            .select(either_def_true, zero, conservative)
                    }
                    _ => {
                        let zero = state.builder.ins().iconst(common_ty, 0);
                        let any_x_l = state.builder.ins().icmp(IntCC::NotEqual, l_m, zero);
                        let any_x_r = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
                        let any_x = state.builder.ins().bor(any_x_l, any_x_r);

                        let all_ones = state.builder.ins().iconst(common_ty, -1);
                        state.builder.ins().select(any_x, all_ones, zero)
                    }
                };

                // IEEE 1800 normalization: value bits at X positions must be 0.
                // v &= ~m is safe for all ops: Bit regs have mask=0 so v is unchanged.
                let not_m = state.builder.ins().bnot(res_m);
                let safe_res_v = state.builder.ins().band(res_v, not_m);

                let final_res_v = cast_type(state.builder, safe_res_v, dst_ty);
                let final_res_m = cast_type(state.builder, res_m, dst_ty);
                state.regs.insert(
                    *dst,
                    TransValue::FourState {
                        values: vec![final_res_v],
                        masks: vec![final_res_m],
                    },
                );
            } else {
                let final_res = cast_type(state.builder, res_v, dst_ty);
                state
                    .regs
                    .insert(*dst, TransValue::TwoState(vec![final_res]));
            }
        } else {
            // --- Over 64-bit: Multi-word (i64) Operation ---
            let num_chunks = common_logical_width.div_ceil(64);

            let l_chunks = state.regs[lhs].values().to_vec();
            let r_chunks = state.regs[rhs].values().to_vec();

            let mut res_chunks = if matches!(op, BinaryOp::LogicAnd | BinaryOp::LogicOr) {
                wide_ops::emit_wide_logic_andor(state.builder, op, &l_chunks, &r_chunks, num_chunks)
            } else {
                wide_ops::emit_wide_binary(
                    state.builder,
                    op,
                    &l_chunks,
                    &r_chunks,
                    num_chunks,
                    l_width,
                )
            };

            let final_num_chunks = d_width.div_ceil(64);
            res_chunks.truncate(final_num_chunks);
            while res_chunks.len() < final_num_chunks {
                res_chunks.push(state.builder.ins().iconst(types::I64, 0));
            }

            if self.options.four_state {
                // Get or generate zero masks for each operand
                let l_masks: Vec<Value> = match state.regs[lhs].masks() {
                    Some(m) => m.to_vec(),
                    None => (0..num_chunks)
                        .map(|_| state.builder.ins().iconst(types::I64, 0))
                        .collect(),
                };
                let r_masks: Vec<Value> = match state.regs[rhs].masks() {
                    Some(m) => m.to_vec(),
                    None => (0..num_chunks)
                        .map(|_| state.builder.ins().iconst(types::I64, 0))
                        .collect(),
                };

                let mut res_masks = match op {
                    // Bitwise ops: precise mask computation per chunk
                    BinaryOp::And => (0..num_chunks)
                        .map(|i| {
                            let lm = get_chunk_as_i64(state.builder, &l_masks, i);
                            let rm = get_chunk_as_i64(state.builder, &r_masks, i);
                            let lv = get_chunk_as_i64(state.builder, &l_chunks, i);
                            let rv = get_chunk_as_i64(state.builder, &r_chunks, i);

                            let m1 = state.builder.ins().band(lm, rm);
                            let m2 = state.builder.ins().band(lm, rv);
                            let m3 = state.builder.ins().band(rm, lv);
                            let mt = state.builder.ins().bor(m1, m2);
                            state.builder.ins().bor(mt, m3)
                        })
                        .collect(),
                    BinaryOp::Or => (0..num_chunks)
                        .map(|i| {
                            let lm = get_chunk_as_i64(state.builder, &l_masks, i);
                            let rm = get_chunk_as_i64(state.builder, &r_masks, i);
                            let lv = get_chunk_as_i64(state.builder, &l_chunks, i);
                            let rv = get_chunk_as_i64(state.builder, &r_chunks, i);

                            let m1 = state.builder.ins().band(lm, rm);
                            let m2 = state.builder.ins().band_not(lm, rv);
                            let m3 = state.builder.ins().band_not(rm, lv);
                            let mt = state.builder.ins().bor(m1, m2);
                            state.builder.ins().bor(mt, m3)
                        })
                        .collect(),
                    BinaryOp::Xor => (0..num_chunks)
                        .map(|i| {
                            let lm = get_chunk_as_i64(state.builder, &l_masks, i);
                            let rm = get_chunk_as_i64(state.builder, &r_masks, i);
                            state.builder.ins().bor(lm, rm)
                        })
                        .collect(),
                    BinaryOp::Shr | BinaryOp::Shl | BinaryOp::Sar => {
                        // If shift amount (rhs) has ANY X, result is all-X.
                        let mut r_any_x = state.builder.ins().iconst(types::I64, 0);
                        for m in &r_masks {
                            let m_i64 = cast_type(state.builder, *m, types::I64);
                            r_any_x = state.builder.ins().bor(r_any_x, m_i64);
                        }
                        let zero = state.builder.ins().iconst(types::I64, 0);
                        let shift_has_x = state.builder.ins().icmp(IntCC::NotEqual, r_any_x, zero);

                        let shifted_masks = if matches!(op, BinaryOp::Sar) {
                            wide_ops::emit_wide_sar(
                                state.builder,
                                &l_masks,
                                &r_chunks,
                                num_chunks,
                                l_width,
                            )
                        } else {
                            wide_ops::emit_wide_shift(
                                state.builder,
                                op,
                                &l_masks,
                                &r_chunks,
                                num_chunks,
                            )
                        };

                        let all_ones = state.builder.ins().iconst(types::I64, -1i64);
                        shifted_masks
                            .into_iter()
                            .map(|m| state.builder.ins().select(shift_has_x, all_ones, m))
                            .collect()
                    }
                    BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
                        // IEEE 1800 ==?/!=?: RHS X/Z bits are wildcards (don't care).
                        // Compare only at positions where both LHS is definite and RHS is non-wildcard.
                        let mut accumulated_mismatch =
                            state.builder.ins().iconst(types::I64, 0);
                        let mut accumulated_x = state.builder.ins().iconst(types::I64, 0);

                        let effective_l: Vec<Value> = (0..num_chunks)
                            .map(|i| {
                                let lv = get_chunk_as_i64(state.builder, &l_chunks, i);
                                let lm = get_chunk_as_i64(state.builder, &l_masks, i);
                                let rm = get_chunk_as_i64(state.builder, &r_masks, i);
                                let compare_mask = state.builder.ins().bnot(rm);
                                let l_definite = state.builder.ins().bnot(lm);
                                let definite_compare =
                                    state.builder.ins().band(compare_mask, l_definite);
                                state.builder.ins().band(lv, definite_compare)
                            })
                            .collect();
                        let effective_r: Vec<Value> = (0..num_chunks)
                            .map(|i| {
                                let rv = get_chunk_as_i64(state.builder, &r_chunks, i);
                                let lm = get_chunk_as_i64(state.builder, &l_masks, i);
                                let rm = get_chunk_as_i64(state.builder, &r_masks, i);
                                let compare_mask = state.builder.ins().bnot(rm);
                                let l_definite = state.builder.ins().bnot(lm);
                                let definite_compare =
                                    state.builder.ins().band(compare_mask, l_definite);
                                state.builder.ins().band(rv, definite_compare)
                            })
                            .collect();

                        for i in 0..num_chunks {
                            let lv = get_chunk_as_i64(state.builder, &l_chunks, i);
                            let rv = get_chunk_as_i64(state.builder, &r_chunks, i);
                            let lm = get_chunk_as_i64(state.builder, &l_masks, i);
                            let rm = get_chunk_as_i64(state.builder, &r_masks, i);
                            let compare_mask = state.builder.ins().bnot(rm);
                            let l_definite = state.builder.ins().bnot(lm);
                            let definite_compare =
                                state.builder.ins().band(compare_mask, l_definite);
                            let xor_bits = state.builder.ins().bxor(lv, rv);
                            let mismatch =
                                state.builder.ins().band(xor_bits, definite_compare);
                            accumulated_mismatch =
                                state.builder.ins().bor(accumulated_mismatch, mismatch);
                            let x_at = state.builder.ins().band(lm, compare_mask);
                            accumulated_x =
                                state.builder.ins().bor(accumulated_x, x_at);
                        }

                        let zero = state.builder.ins().iconst(types::I64, 0);
                        let has_mismatch = state.builder.ins().icmp(
                            IntCC::NotEqual,
                            accumulated_mismatch,
                            zero,
                        );
                        let has_x = state.builder.ins().icmp(
                            IntCC::NotEqual,
                            accumulated_x,
                            zero,
                        );
                        let all_ones = state.builder.ins().iconst(types::I64, -1i64);
                        let x_mask =
                            state.builder.ins().select(has_x, all_ones, zero);
                        let mask_val =
                            state.builder.ins().select(has_mismatch, zero, x_mask);

                        // Override value: recompute with masked operands
                        let cmp_op = if matches!(op, BinaryOp::EqWildcard) {
                            &BinaryOp::Eq
                        } else {
                            &BinaryOp::Ne
                        };
                        let mut new_res = wide_ops::emit_wide_unsigned_cmp(
                            state.builder,
                            cmp_op,
                            &effective_l,
                            &effective_r,
                            num_chunks,
                        );
                        new_res.truncate(final_num_chunks);
                        while new_res.len() < final_num_chunks {
                            new_res
                                .push(state.builder.ins().iconst(types::I64, 0));
                        }
                        res_chunks = new_res;

                        vec![mask_val; final_num_chunks]
                    }
                    BinaryOp::LogicAnd | BinaryOp::LogicOr => {
                        // IEEE 1800 dominant-value: 0 for &&, 1 for ||
                        let mut l_val_or =
                            state.builder.ins().iconst(types::I64, 0);
                        let mut r_val_or =
                            state.builder.ins().iconst(types::I64, 0);
                        let mut l_mask_or =
                            state.builder.ins().iconst(types::I64, 0);
                        let mut r_mask_or =
                            state.builder.ins().iconst(types::I64, 0);
                        for i in 0..num_chunks {
                            let lv =
                                get_chunk_as_i64(state.builder, &l_chunks, i);
                            let rv =
                                get_chunk_as_i64(state.builder, &r_chunks, i);
                            let lm =
                                get_chunk_as_i64(state.builder, &l_masks, i);
                            let rm =
                                get_chunk_as_i64(state.builder, &r_masks, i);
                            l_val_or =
                                state.builder.ins().bor(l_val_or, lv);
                            r_val_or =
                                state.builder.ins().bor(r_val_or, rv);
                            l_mask_or =
                                state.builder.ins().bor(l_mask_or, lm);
                            r_mask_or =
                                state.builder.ins().bor(r_mask_or, rm);
                        }

                        let zero = state.builder.ins().iconst(types::I64, 0);
                        let any_x_all = state
                            .builder
                            .ins()
                            .bor(l_mask_or, r_mask_or);
                        let has_x = state.builder.ins().icmp(
                            IntCC::NotEqual,
                            any_x_all,
                            zero,
                        );
                        let all_ones =
                            state.builder.ins().iconst(types::I64, -1i64);
                        let conservative =
                            state.builder.ins().select(has_x, all_ones, zero);

                        let dominant = if matches!(op, BinaryOp::LogicAnd) {
                            // 0 is dominant: check if either operand is definite false
                            // definite false = (v | m) == 0
                            let l_vm =
                                state.builder.ins().bor(l_val_or, l_mask_or);
                            let r_vm =
                                state.builder.ins().bor(r_val_or, r_mask_or);
                            let l_def_false = state
                                .builder
                                .ins()
                                .icmp(IntCC::Equal, l_vm, zero);
                            let r_def_false = state
                                .builder
                                .ins()
                                .icmp(IntCC::Equal, r_vm, zero);
                            state.builder.ins().bor(l_def_false, r_def_false)
                        } else {
                            // 1 is dominant: check if either operand is definite true
                            // definite true = v != 0 (since normalized, v != 0 means definite 1)
                            let l_def_true = state
                                .builder
                                .ins()
                                .icmp(IntCC::NotEqual, l_val_or, zero);
                            let r_def_true = state
                                .builder
                                .ins()
                                .icmp(IntCC::NotEqual, r_val_or, zero);
                            state.builder.ins().bor(l_def_true, r_def_true)
                        };

                        let mask_val = state
                            .builder
                            .ins()
                            .select(dominant, zero, conservative);
                        vec![mask_val; final_num_chunks]
                    }
                    // All other ops: conservative — if any mask chunk is non-zero, result is all-X
                    _ => {
                        // OR all mask chunks together to detect any X
                        let mut any_x = state.builder.ins().iconst(types::I64, 0);
                        for m in l_masks.iter().chain(r_masks.iter()) {
                            let m_i64 = cast_type(state.builder, *m, types::I64);
                            any_x = state.builder.ins().bor(any_x, m_i64);
                        }
                        let zero = state.builder.ins().iconst(types::I64, 0);
                        let has_x = state.builder.ins().icmp(IntCC::NotEqual, any_x, zero);
                        let all_ones = state.builder.ins().iconst(types::I64, -1i64);
                        let mask_val = state.builder.ins().select(has_x, all_ones, zero);
                        vec![mask_val; final_num_chunks]
                    }
                };

                res_masks.truncate(final_num_chunks);
                while res_masks.len() < final_num_chunks {
                    res_masks.push(state.builder.ins().iconst(types::I64, 0));
                }

                // Mask width normalization: clear bits beyond d_width in the last chunk
                let last_chunk_bits = d_width % 64;
                if last_chunk_bits != 0 && !res_masks.is_empty() {
                    let width_mask_val = ((1u64 << last_chunk_bits) - 1) as i64;
                    let width_mask =
                        state.builder.ins().iconst(types::I64, width_mask_val);
                    let last_idx = res_masks.len() - 1;
                    res_masks[last_idx] =
                        state.builder.ins().band(res_masks[last_idx], width_mask);
                }

                // IEEE 1800 normalization: v &= ~m per chunk
                let safe_chunks: Vec<Value> = res_chunks
                    .iter()
                    .zip(res_masks.iter())
                    .map(|(&v, &m)| {
                        let not_m = state.builder.ins().bnot(m);
                        state.builder.ins().band(v, not_m)
                    })
                    .collect();

                state.regs.insert(
                    *dst,
                    TransValue::FourState {
                        values: safe_chunks,
                        masks: res_masks,
                    },
                );
            } else {
                state.regs.insert(*dst, TransValue::TwoState(res_chunks));
            }
        }
    }

    pub(super) fn translate_unary_inst(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        op: &UnaryOp,
        rhs: &RegisterId,
    ) {
        // 1. 各オペランドの「論理幅」を取得
        let r_width = state.register_map[rhs].width();
        let d_width = state.register_map[dst].width();

        // 2. HDLのルール：演算幅 = max(RHS_width, DST_width)
        let common_logical_width = r_width.max(d_width);

        if common_logical_width <= 64 {
            // --- 64-bit or less: Single-word Operation ---
            let r_val = state.regs[rhs].values()[0];
            let common_ty = get_cl_type(common_logical_width);

            let r_is_signed = state.register_map[rhs].is_signed() || matches!(op, UnaryOp::Minus);
            let r = promote_to_physical(state, r_val, r_width, r_is_signed, common_ty);

            let res_v = match op {
                UnaryOp::Minus => state.builder.ins().ineg(r),
                UnaryOp::Ident => r,
                UnaryOp::BitNot => state.builder.ins().bnot(r),

                UnaryOp::LogicNot => {
                    let zero = state.builder.ins().iconst(common_ty, 0);
                    let is_zero = state.builder.ins().icmp(IntCC::Equal, r, zero);
                    let one_val = state.builder.ins().iconst(common_ty, 1);
                    state.builder.ins().select(is_zero, one_val, zero)
                }

                UnaryOp::Or => {
                    let zero = state.builder.ins().iconst(common_ty, 0);
                    let is_not_zero = state.builder.ins().icmp(IntCC::NotEqual, r, zero);
                    let one_val = state.builder.ins().iconst(common_ty, 1);
                    state.builder.ins().select(is_not_zero, one_val, zero)
                }

                UnaryOp::Xor => {
                    let popcnt = state.builder.ins().popcnt(r);
                    state.builder.ins().band_imm(popcnt, 1)
                }

                UnaryOp::And => {
                    let mask = if r_width >= 128 {
                        !0u128
                    } else {
                        (1u128 << r_width) - 1
                    };
                    let full_val = state.builder.ins().iconst(common_ty, mask as i64);
                    let is_all_ones = state.builder.ins().icmp(IntCC::Equal, r, full_val);

                    let zero = state.builder.ins().iconst(common_ty, 0);
                    let one_val = state.builder.ins().iconst(common_ty, 1);
                    state.builder.ins().select(is_all_ones, one_val, zero)
                }
            };

            let dst_ty = get_cl_type(d_width);

            if self.options.four_state {
                let r_m_val = state.regs[rhs]
                    .masks()
                    .map(|m| m[0])
                    .unwrap_or_else(|| state.builder.ins().iconst(common_ty, 0));
                let r_m = promote_to_physical(state, r_m_val, r_width, r_is_signed, common_ty);

                let res_m = match op {
                    UnaryOp::Ident | UnaryOp::BitNot => r_m,
                    UnaryOp::Or => {
                        // IEEE 1800 dominant-value: |a is definite 1 if any bit is definite 1.
                        // definite_ones = r & ~r_m (value=1 where mask=0)
                        let not_m = state.builder.ins().bnot(r_m);
                        let definite_ones = state.builder.ins().band(r, not_m);
                        let zero = state.builder.ins().iconst(common_ty, 0);
                        let has_definite_one =
                            state
                                .builder
                                .ins()
                                .icmp(IntCC::NotEqual, definite_ones, zero);
                        let has_any_x = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
                        // If definite 1 → result defined (mask=0); else if X → result X
                        let all_ones = state.builder.ins().iconst(common_ty, -1);
                        let x_mask = state.builder.ins().select(has_any_x, all_ones, zero);
                        state.builder.ins().select(has_definite_one, zero, x_mask)
                    }
                    UnaryOp::And => {
                        // IEEE 1800 dominant-value: &a is definite 0 if any bit is definite 0.
                        // definite_zeros = ~r & ~r_m (value=0 where mask=0)
                        let not_m = state.builder.ins().bnot(r_m);
                        let not_v = state.builder.ins().bnot(r);
                        let definite_zeros = state.builder.ins().band(not_v, not_m);
                        // Mask to logical width to avoid false positives from padding bits
                        let width_mask_val = if r_width >= 64 {
                            -1i64
                        } else {
                            ((1u64 << r_width) - 1) as i64
                        };
                        let width_mask = state.builder.ins().iconst(common_ty, width_mask_val);
                        let definite_zeros_masked =
                            state.builder.ins().band(definite_zeros, width_mask);
                        let zero = state.builder.ins().iconst(common_ty, 0);
                        let has_definite_zero = state.builder.ins().icmp(
                            IntCC::NotEqual,
                            definite_zeros_masked,
                            zero,
                        );
                        let has_any_x = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
                        let all_ones = state.builder.ins().iconst(common_ty, -1);
                        let x_mask = state.builder.ins().select(has_any_x, all_ones, zero);
                        state.builder.ins().select(has_definite_zero, zero, x_mask)
                    }
                    UnaryOp::Minus | UnaryOp::LogicNot | UnaryOp::Xor => {
                        let zero = state.builder.ins().iconst(common_ty, 0);
                        let any_x = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
                        let all_ones = state.builder.ins().iconst(common_ty, -1);
                        state.builder.ins().select(any_x, all_ones, zero)
                    }
                };

                // IEEE 1800 normalization: v &= ~m
                let not_m = state.builder.ins().bnot(res_m);
                let safe_res_v = state.builder.ins().band(res_v, not_m);
                let final_res_v = if common_ty.bits() > dst_ty.bits() {
                    state.builder.ins().ireduce(dst_ty, safe_res_v)
                } else {
                    safe_res_v
                };
                let final_res_m = if common_ty.bits() > dst_ty.bits() {
                    state.builder.ins().ireduce(dst_ty, res_m)
                } else {
                    res_m
                };
                state.regs.insert(
                    *dst,
                    TransValue::FourState {
                        values: vec![final_res_v],
                        masks: vec![final_res_m],
                    },
                );
            } else {
                let final_res = if common_ty.bits() > dst_ty.bits() {
                    state.builder.ins().ireduce(dst_ty, res_v)
                } else {
                    res_v
                };
                state
                    .regs
                    .insert(*dst, TransValue::TwoState(vec![final_res]));
            }
        } else {
            // --- Over 64-bit: Multi-word Operation ---
            let num_chunks = common_logical_width.div_ceil(64);
            let r_chunks = state.regs[rhs].values().to_vec();

            let mut res_chunks = wide_ops::emit_wide_unary(
                state.builder,
                op,
                &r_chunks,
                num_chunks,
                common_logical_width,
            );

            let final_num_chunks = d_width.div_ceil(64);
            res_chunks.truncate(final_num_chunks);
            while res_chunks.len() < final_num_chunks {
                res_chunks.push(state.builder.ins().iconst(types::I64, 0));
            }

            if self.options.four_state {
                let r_masks: Vec<Value> = match state.regs[rhs].masks() {
                    Some(m) => m.to_vec(),
                    None => (0..num_chunks)
                        .map(|_| state.builder.ins().iconst(types::I64, 0))
                        .collect(),
                };

                let mut res_masks = match op {
                    // Bitwise ops: mask is preserved per-chunk
                    UnaryOp::Ident | UnaryOp::BitNot => {
                        let mut masks = r_masks.clone();
                        masks.truncate(final_num_chunks);
                        while masks.len() < final_num_chunks {
                            masks.push(state.builder.ins().iconst(types::I64, 0));
                        }
                        masks
                    }
                    UnaryOp::Or => {
                        // IEEE 1800 dominant-value: |a is definite 1 if any bit is definite 1.
                        let mut accumulated_definite_ones =
                            state.builder.ins().iconst(types::I64, 0);
                        let mut accumulated_mask = state.builder.ins().iconst(types::I64, 0);
                        for i in 0..num_chunks {
                            let rv = get_chunk_as_i64(state.builder, &r_chunks, i);
                            let rm = get_chunk_as_i64(state.builder, &r_masks, i);
                            let not_m = state.builder.ins().bnot(rm);
                            let definite_ones = state.builder.ins().band(rv, not_m);
                            accumulated_definite_ones =
                                state
                                    .builder
                                    .ins()
                                    .bor(accumulated_definite_ones, definite_ones);
                            accumulated_mask = state.builder.ins().bor(accumulated_mask, rm);
                        }
                        let zero = state.builder.ins().iconst(types::I64, 0);
                        let has_definite_one = state.builder.ins().icmp(
                            IntCC::NotEqual,
                            accumulated_definite_ones,
                            zero,
                        );
                        let has_any_x =
                            state
                                .builder
                                .ins()
                                .icmp(IntCC::NotEqual, accumulated_mask, zero);
                        let all_ones = state.builder.ins().iconst(types::I64, -1i64);
                        let x_mask = state.builder.ins().select(has_any_x, all_ones, zero);
                        let mask_val = state.builder.ins().select(has_definite_one, zero, x_mask);
                        vec![mask_val; final_num_chunks]
                    }
                    UnaryOp::And => {
                        // IEEE 1800 dominant-value: &a is definite 0 if any bit is definite 0.
                        let mut accumulated_definite_zeros =
                            state.builder.ins().iconst(types::I64, 0);
                        let mut accumulated_mask = state.builder.ins().iconst(types::I64, 0);
                        for i in 0..num_chunks {
                            let rv = get_chunk_as_i64(state.builder, &r_chunks, i);
                            let rm = get_chunk_as_i64(state.builder, &r_masks, i);
                            let not_m = state.builder.ins().bnot(rm);
                            let not_v = state.builder.ins().bnot(rv);
                            let mut definite_zeros = state.builder.ins().band(not_v, not_m);
                            // Mask last chunk to logical width
                            if i == num_chunks - 1 {
                                let remaining = common_logical_width - i * 64;
                                if remaining < 64 {
                                    let last_chunk_mask =
                                        ((1u64 << remaining) - 1) as i64;
                                    let mask_val =
                                        state.builder.ins().iconst(types::I64, last_chunk_mask);
                                    definite_zeros =
                                        state.builder.ins().band(definite_zeros, mask_val);
                                }
                            }
                            accumulated_definite_zeros =
                                state
                                    .builder
                                    .ins()
                                    .bor(accumulated_definite_zeros, definite_zeros);
                            accumulated_mask = state.builder.ins().bor(accumulated_mask, rm);
                        }
                        let zero = state.builder.ins().iconst(types::I64, 0);
                        let has_definite_zero = state.builder.ins().icmp(
                            IntCC::NotEqual,
                            accumulated_definite_zeros,
                            zero,
                        );
                        let has_any_x =
                            state
                                .builder
                                .ins()
                                .icmp(IntCC::NotEqual, accumulated_mask, zero);
                        let all_ones = state.builder.ins().iconst(types::I64, -1i64);
                        let x_mask = state.builder.ins().select(has_any_x, all_ones, zero);
                        let mask_val =
                            state.builder.ins().select(has_definite_zero, zero, x_mask);
                        vec![mask_val; final_num_chunks]
                    }
                    // Other ops: conservative — if any mask chunk is non-zero, result is all-X
                    _ => {
                        let mut any_x = state.builder.ins().iconst(types::I64, 0);
                        for m in &r_masks {
                            any_x = state.builder.ins().bor(any_x, *m);
                        }
                        let zero = state.builder.ins().iconst(types::I64, 0);
                        let has_x = state.builder.ins().icmp(IntCC::NotEqual, any_x, zero);
                        let all_ones = state.builder.ins().iconst(types::I64, -1i64);
                        let mask_val = state.builder.ins().select(has_x, all_ones, zero);
                        vec![mask_val; final_num_chunks]
                    }
                };

                res_masks.truncate(final_num_chunks);
                while res_masks.len() < final_num_chunks {
                    res_masks.push(state.builder.ins().iconst(types::I64, 0));
                }

                // Mask width normalization: clear bits beyond d_width in the last chunk
                let last_chunk_bits = d_width % 64;
                if last_chunk_bits != 0 && !res_masks.is_empty() {
                    let width_mask_val = ((1u64 << last_chunk_bits) - 1) as i64;
                    let width_mask =
                        state.builder.ins().iconst(types::I64, width_mask_val);
                    let last_idx = res_masks.len() - 1;
                    res_masks[last_idx] =
                        state.builder.ins().band(res_masks[last_idx], width_mask);
                }

                // IEEE 1800 normalization: v &= ~m per chunk
                let safe_chunks: Vec<Value> = res_chunks
                    .iter()
                    .zip(res_masks.iter())
                    .map(|(&v, &m)| {
                        let not_m = state.builder.ins().bnot(m);
                        state.builder.ins().band(v, not_m)
                    })
                    .collect();

                state.regs.insert(
                    *dst,
                    TransValue::FourState {
                        values: safe_chunks,
                        masks: res_masks,
                    },
                );
            } else {
                state.regs.insert(*dst, TransValue::TwoState(res_chunks));
            }
        }
    }
}
fn apply_d_width_mask(state: &mut TranslationState, val: Value, ty: Type, d_width: usize) -> Value {
    if d_width < ty.bits() as usize {
        let mask_val = (1u64 << d_width).wrapping_sub(1);
        let mask = state.builder.ins().iconst(ty, mask_val as i64);
        state.builder.ins().band(val, mask)
    } else {
        val
    }
}
fn apply_d_width_mask_arith(
    state: &mut TranslationState,
    val: Value,
    ty: Type,
    d_width: usize,
) -> Value {
    let phys_bits = ty.bits() as i64;
    if d_width < (phys_bits as usize) {
        let shift_back_amt = phys_bits - (d_width as i64);

        let tmp = state.builder.ins().ishl_imm(val, shift_back_amt);
        state.builder.ins().sshr_imm(tmp, shift_back_amt)
    } else {
        val
    }
}
