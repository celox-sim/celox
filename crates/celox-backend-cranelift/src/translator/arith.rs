use cranelift::prelude::*;

use super::core::promote_to_physical;
use super::core::{TransValue, cast_type, get_chunk_as_i64, get_cl_type};
use super::{SIRTranslator, TranslationState, wide_ops};
use crate::{RegisterId, SIRValue, UnaryOp};

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
            let mask_digits = val.mask.to_u64_digits();
            if width <= 64 {
                let ty = get_cl_type(width);
                let raw_m = mask_digits.first().copied().unwrap_or(0);
                cl_masks.push(state.builder.ins().iconst(ty, raw_m as i64));
            } else {
                for i in 0..num_chunks {
                    let d = mask_digits.get(i).copied().unwrap_or(0);
                    cl_masks.push(state.builder.ins().iconst(types::I64, d as i64));
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

    /// Try to fold a SIR Concat of all-constant (imm_constants-tracked) args
    /// into a single Cranelift iconst (or multi-chunk iconst for wide values).
    pub(super) fn try_fold_const_sir_concat(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        args: &[RegisterId],
    ) -> Option<TransValue> {
        if self.options.four_state {
            return None; // Conservative: skip 4-state for now.
        }

        // Check if all args are tracked constants.
        let mut const_vals: Vec<(u64, usize)> = Vec::with_capacity(args.len());
        for arg in args {
            let val = state.imm_constants.get(arg)?;
            let width = state.register_map[arg].width();
            if width > 64 {
                return None; // Wide arg — bail.
            }
            const_vals.push((*val, width));
        }

        let dst_width = state.register_map[dst].width();
        let num_chunks = dst_width.div_ceil(64).max(1);

        // Build combined value (args are MSB-first, reverse for LSB-first).
        let mut combined = vec![0u64; num_chunks];
        let mut bit_offset = 0usize;
        for &(val, width) in const_vals.iter().rev() {
            let mask = if width >= 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            let masked_val = val & mask;
            let chunk_idx = bit_offset / 64;
            let bit_in_chunk = bit_offset % 64;
            if chunk_idx < num_chunks {
                combined[chunk_idx] |= masked_val << bit_in_chunk;
                // Handle cross-chunk boundary
                if bit_in_chunk + width > 64 && chunk_idx + 1 < num_chunks {
                    combined[chunk_idx + 1] |= masked_val >> (64 - bit_in_chunk);
                }
            }
            bit_offset += width;
        }

        // Emit Cranelift iconst for each chunk.
        let cl_chunks: Vec<Value> = combined
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                if num_chunks == 1 {
                    let ty = get_cl_type(dst_width);
                    state.builder.ins().iconst(ty, v as i64)
                } else {
                    // For the last chunk, mask to the remaining width
                    let remaining = dst_width - i * 64;
                    let _ty = if remaining >= 64 {
                        types::I64
                    } else {
                        get_cl_type(remaining)
                    };
                    state.builder.ins().iconst(types::I64, v as i64)
                }
            })
            .collect();

        Some(TransValue::TwoState(cl_chunks))
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
            let arg_chunks_v = state.regs[arg_reg].load_value_chunks(state.builder);
            let arg_chunks_m: Vec<Value> = if self.options.four_state {
                state.regs[arg_reg]
                    .load_mask_chunks(state.builder)
                    .unwrap_or_else(|| {
                        let ty = get_cl_type(arg_width);
                        vec![state.builder.ins().iconst(ty, 0)]
                    })
            } else {
                vec![]
            };

            // Since args can be > 64 bits, we iterate arg chunks.
            let arg_num_chunks = if arg_width <= 64 {
                1
            } else {
                arg_width.div_ceil(64)
            };

            for i in 0..arg_num_chunks {
                let arg_chunk_bits = arg_width.saturating_sub(i * 64).min(64);
                let raw_chunk_v = get_chunk_as_i64(state.builder, &arg_chunks_v, i);
                let raw_chunk_m = if self.options.four_state {
                    get_chunk_as_i64(state.builder, &arg_chunks_m, i)
                } else {
                    state.builder.ins().iconst(types::I64, 0)
                };
                // Cranelift represents sub-byte HDL values with an i8.  Mask the
                // physical padding before concatenation so it cannot become part
                // of an adjacent field.
                let (chunk_val_v, chunk_val_m) = if arg_chunk_bits < 64 {
                    let mask_value = (1u64 << arg_chunk_bits) - 1;
                    let mask = state.builder.ins().iconst(types::I64, mask_value as i64);
                    (
                        state.builder.ins().band(raw_chunk_v, mask),
                        state.builder.ins().band(raw_chunk_m, mask),
                    )
                } else {
                    (raw_chunk_v, raw_chunk_m)
                };

                let abs_bit_offset = current_bit_offset + i * 64;
                let dst_chunk_idx = abs_bit_offset / 64;
                let bit_shift = abs_bit_offset % 64;

                // 1. Write to dst_chunks[dst_chunk_idx]
                if dst_chunk_idx < num_chunks {
                    let shifted_v = state
                        .builder
                        .ins()
                        .ishl_imm_s(chunk_val_v, bit_shift as i64);
                    dst_chunks_v[dst_chunk_idx] = state
                        .builder
                        .ins()
                        .bor(dst_chunks_v[dst_chunk_idx], shifted_v);

                    if self.options.four_state {
                        let shifted_m = state
                            .builder
                            .ins()
                            .ishl_imm_s(chunk_val_m, bit_shift as i64);
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
                    let shifted_down_v = state
                        .builder
                        .ins()
                        .ushr_imm_s(chunk_val_v, shift_down as i64);
                    dst_chunks_v[dst_chunk_idx + 1] = state
                        .builder
                        .ins()
                        .bor(dst_chunks_v[dst_chunk_idx + 1], shifted_down_v);

                    if self.options.four_state {
                        let shifted_down_m = state
                            .builder
                            .ins()
                            .ushr_imm_s(chunk_val_m, shift_down as i64);
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
                state.regs.insert(
                    *dst,
                    TransValue::FourState {
                        values: vec![val_v],
                        masks: vec![val_m],
                    },
                );
            } else {
                state.regs.insert(*dst, TransValue::TwoState(vec![val_v]));
            }
        } else if self.options.four_state {
            state.regs.insert(
                *dst,
                TransValue::FourState {
                    values: dst_chunks_v,
                    masks: dst_chunks_m,
                },
            );
        } else {
            state.regs.insert(*dst, TransValue::TwoState(dst_chunks_v));
        }
    }

    /// `ToPlanes` and `FromPlanes` move whole 64-bit words between the value
    /// and mask chunks of a four-state register and one two-state register.
    fn translate_planes_inst(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        op: &UnaryOp,
        rhs: &RegisterId,
    ) {
        let source = state.regs[rhs].clone();
        let mut values = source.load_value_chunks(state.builder);
        let result = if *op == UnaryOp::ToPlanes {
            let masks = source.load_mask_chunks(state.builder).unwrap_or_else(|| {
                (0..values.len())
                    .map(|_| state.builder.ins().iconst(types::I64, 0))
                    .collect()
            });
            values.extend(masks);
            TransValue::TwoState(values)
        } else {
            let masks = values.split_off(values.len() / 2);
            if self.options.four_state {
                TransValue::FourState { values, masks }
            } else {
                TransValue::TwoState(values)
            }
        };
        state.regs.insert(*dst, result);
    }

    pub(super) fn translate_unary_inst(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        op: &UnaryOp,
        rhs: &RegisterId,
    ) {
        if let UnaryOp::Float { op, precision } = op {
            self.translate_float(
                state,
                *dst,
                *rhs,
                None,
                super::core::FloatOperation::Unary(*op),
                *precision,
            );
            return;
        }
        if matches!(
            op,
            UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros
        ) {
            self.translate_count_unary_inst(state, dst, op, rhs);
            return;
        }
        if matches!(op, UnaryOp::ToPlanes | UnaryOp::FromPlanes) {
            self.translate_planes_inst(state, dst, op, rhs);
            return;
        }

        // 1. 各オペランドの「論理幅」を取得
        let r_width = state.register_map[rhs].width();
        let d_width = state.register_map[dst].width();

        // 2. HDLのルール：演算幅 = max(RHS_width, DST_width)
        let common_logical_width = r_width.max(d_width);

        if common_logical_width <= 64 {
            // --- 64-bit or less: Single-word Operation ---
            let r_val = state.regs[rhs].first_value(state.builder);
            let common_ty = get_cl_type(common_logical_width);

            let r_is_signed = state.register_map[rhs].is_signed() || matches!(op, UnaryOp::Minus);
            let r = promote_to_physical(state, r_val, r_width, r_is_signed, common_ty);

            let res_v = match op {
                UnaryOp::Float { .. } | UnaryOp::ToPlanes | UnaryOp::FromPlanes => {
                    unreachable!("lowered separately")
                }
                UnaryOp::Minus => state.builder.ins().ineg(r),
                UnaryOp::Ident | UnaryOp::ToTwoState => r,
                UnaryOp::BitNot => {
                    let notted = state.builder.ins().bnot(r);
                    // Width-mask: bnot flips ALL bits in the physical type (e.g., i8 = 8 bits),
                    // but only d_width bits are logically valid.
                    let phys_bits = common_ty.bits() as usize;
                    if d_width < phys_bits {
                        let mask = ((1u64 << d_width) - 1) as i64;
                        state.builder.ins().band_imm_s(notted, mask)
                    } else {
                        notted
                    }
                }

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
                    state.builder.ins().band_imm_s(popcnt, 1)
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
                UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
                    unreachable!()
                }
            };

            let dst_ty = get_cl_type(d_width);

            if self.options.four_state {
                let r_m_val = state.regs[rhs]
                    .first_mask(state.builder)
                    .unwrap_or_else(|| state.builder.ins().iconst(common_ty, 0));
                let r_m = promote_to_physical(state, r_m_val, r_width, r_is_signed, common_ty);

                let res_m = match op {
                    UnaryOp::Float { .. } | UnaryOp::ToPlanes | UnaryOp::FromPlanes => {
                        unreachable!("lowered separately")
                    }
                    UnaryOp::Ident | UnaryOp::BitNot => r_m,
                    UnaryOp::ToTwoState => state.builder.ins().iconst(common_ty, 0),
                    UnaryOp::LogicNot | UnaryOp::Or => {
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
                        let has_definite_zero =
                            state
                                .builder
                                .ins()
                                .icmp(IntCC::NotEqual, definite_zeros_masked, zero);
                        let has_any_x = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
                        let all_ones = state.builder.ins().iconst(common_ty, -1);
                        let x_mask = state.builder.ins().select(has_any_x, all_ones, zero);
                        state.builder.ins().select(has_definite_zero, zero, x_mask)
                    }
                    UnaryOp::Minus
                    | UnaryOp::Xor
                    | UnaryOp::PopCount
                    | UnaryOp::CountLeadingZeros
                    | UnaryOp::CountTrailingZeros => {
                        let zero = state.builder.ins().iconst(common_ty, 0);
                        let any_x = state.builder.ins().icmp(IntCC::NotEqual, r_m, zero);
                        let all_ones = state.builder.ins().iconst(common_ty, -1);
                        state.builder.ins().select(any_x, all_ones, zero)
                    }
                };

                let final_res_v = promote_to_physical(state, res_v, d_width, false, dst_ty);
                let final_res_m = promote_to_physical(state, res_m, d_width, false, dst_ty);
                // Identity/casts preserve X versus Z. Other unary operations
                // produce X for an unknown input bit.
                let normalized_v = match op {
                    UnaryOp::Ident => final_res_v,
                    UnaryOp::ToTwoState => {
                        let final_src_m = if common_ty.bits() > dst_ty.bits() {
                            state.builder.ins().ireduce(dst_ty, r_m)
                        } else {
                            r_m
                        };
                        let defined = state.builder.ins().bnot(final_src_m);
                        state.builder.ins().band(final_res_v, defined)
                    }
                    _ => state.builder.ins().bor(final_res_v, final_res_m),
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
        } else {
            // --- Over 64-bit: Multi-word Operation ---
            let num_chunks = common_logical_width.div_ceil(64);
            let r_chunks = state.regs[rhs].load_value_chunks(state.builder);

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
                let r_masks: Vec<Value> = state.regs[rhs]
                    .load_mask_chunks(state.builder)
                    .unwrap_or_else(|| {
                        (0..num_chunks)
                            .map(|_| state.builder.ins().iconst(types::I64, 0))
                            .collect()
                    });

                let mut res_masks = match op {
                    // Widen scalar masks to i64 before constructing a chunked
                    // result, just as emit_wide_unary does for the payload.
                    UnaryOp::Ident | UnaryOp::BitNot => (0..final_num_chunks)
                        .map(|i| get_chunk_as_i64(state.builder, &r_masks, i))
                        .collect(),
                    UnaryOp::ToTwoState => (0..final_num_chunks)
                        .map(|_| state.builder.ins().iconst(types::I64, 0))
                        .collect(),
                    UnaryOp::LogicNot | UnaryOp::Or => {
                        // IEEE 1800 dominant-value: |a is definite 1 if any bit is definite 1.
                        let mut accumulated_definite_ones =
                            state.builder.ins().iconst(types::I64, 0);
                        let mut accumulated_mask = state.builder.ins().iconst(types::I64, 0);
                        for i in 0..num_chunks {
                            let rv = get_chunk_as_i64(state.builder, &r_chunks, i);
                            let rm = get_chunk_as_i64(state.builder, &r_masks, i);
                            let not_m = state.builder.ins().bnot(rm);
                            let definite_ones = state.builder.ins().band(rv, not_m);
                            accumulated_definite_ones = state
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
                                    let last_chunk_mask = ((1u64 << remaining) - 1) as i64;
                                    let mask_val =
                                        state.builder.ins().iconst(types::I64, last_chunk_mask);
                                    definite_zeros =
                                        state.builder.ins().band(definite_zeros, mask_val);
                                }
                            }
                            accumulated_definite_zeros = state
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
                        let mask_val = state.builder.ins().select(has_definite_zero, zero, x_mask);
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
                    let width_mask = state.builder.ins().iconst(types::I64, width_mask_val);
                    let last_idx = res_masks.len() - 1;
                    res_masks[last_idx] = state.builder.ins().band(res_masks[last_idx], width_mask);
                }

                let normalized: Vec<_> = match op {
                    UnaryOp::Ident => res_chunks,
                    UnaryOp::ToTwoState => res_chunks
                        .iter()
                        .enumerate()
                        .map(|(index, &value)| {
                            let source_mask = get_chunk_as_i64(state.builder, &r_masks, index);
                            let defined = state.builder.ins().bnot(source_mask);
                            state.builder.ins().band(value, defined)
                        })
                        .collect(),
                    _ => res_chunks
                        .iter()
                        .zip(res_masks.iter())
                        .map(|(&v, &m)| state.builder.ins().bor(v, m))
                        .collect(),
                };
                state.regs.insert(
                    *dst,
                    TransValue::FourState {
                        values: normalized,
                        masks: res_masks,
                    },
                );
            } else {
                state.regs.insert(*dst, TransValue::TwoState(res_chunks));
            }
        }
    }

    /// Mux: dst = if cond { then_val } else { else_val }.
    ///
    /// A condition is definitely true when any known bit is one, unknown when
    /// it has no known-one bit but contains X/Z, and otherwise false.  For an
    /// unknown condition, identical four-state arm bits are preserved and only
    /// differing bits become X, as required by SystemVerilog's conditional
    /// operator semantics.
    pub(super) fn translate_mux_inst(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        cond: &RegisterId,
        then_val: &RegisterId,
        else_val: &RegisterId,
    ) {
        let d_width = state.register_map[dst].width();
        let cond_width = state.register_map[cond].width();
        let zero = state.builder.ins().iconst(types::I64, 0);
        let cond_chunks = state.regs[cond].load_value_chunks(state.builder);
        let cond_masks = state.regs[cond]
            .load_mask_chunks(state.builder)
            .unwrap_or_default();
        let cond_n_chunks = cond_width.div_ceil(64).max(1);
        let mut known_one_bits = zero;
        let mut unknown_bits = zero;
        for index in 0..cond_n_chunks {
            let mut value = get_chunk_as_i64(state.builder, &cond_chunks, index);
            let mut mask = get_chunk_as_i64(state.builder, &cond_masks, index);
            if index + 1 == cond_n_chunks && !cond_width.is_multiple_of(64) {
                let logical_mask = ((1u64 << (cond_width % 64)) - 1) as i64;
                value = state.builder.ins().band_imm_s(value, logical_mask);
                mask = state.builder.ins().band_imm_s(mask, logical_mask);
            }
            let defined = state.builder.ins().bnot(mask);
            let known_ones = state.builder.ins().band(value, defined);
            known_one_bits = state.builder.ins().bor(known_one_bits, known_ones);
            unknown_bits = state.builder.ins().bor(unknown_bits, mask);
        }
        let cond_is_true = state
            .builder
            .ins()
            .icmp(IntCC::NotEqual, known_one_bits, zero);
        let cond_has_unknown = state
            .builder
            .ins()
            .icmp(IntCC::NotEqual, unknown_bits, zero);
        let cond_is_not_true = state.builder.ins().icmp(IntCC::Equal, known_one_bits, zero);
        let cond_is_unknown = state.builder.ins().band(cond_has_unknown, cond_is_not_true);

        if d_width <= 64 {
            let ty = get_cl_type(d_width);
            let tv_raw = state.regs[then_val].first_value(state.builder);
            let ev_raw = state.regs[else_val].first_value(state.builder);
            let tv = cast_type(state.builder, tv_raw, ty);
            let ev = cast_type(state.builder, ev_raw, ty);
            let selected_value = state.builder.ins().select(cond_is_true, tv, ev);

            if self.options.four_state {
                let then_mc = state.regs[then_val]
                    .load_mask_chunks(state.builder)
                    .unwrap_or_else(|| vec![state.builder.ins().iconst(ty, 0)]);
                let else_mc = state.regs[else_val]
                    .load_mask_chunks(state.builder)
                    .unwrap_or_else(|| vec![state.builder.ins().iconst(ty, 0)]);
                let tm = cast_type(state.builder, then_mc[0], ty);
                let em = cast_type(state.builder, else_mc[0], ty);
                let selected_mask = state.builder.ins().select(cond_is_true, tm, em);

                let value_diff = state.builder.ins().bxor(tv, ev);
                let mask_diff = state.builder.ins().bxor(tm, em);
                let diff = state.builder.ins().bor(value_diff, mask_diff);
                let unknown_value = state.builder.ins().bor(tv, diff);
                let unknown_mask = state.builder.ins().bor(tm, diff);
                let result =
                    state
                        .builder
                        .ins()
                        .select(cond_is_unknown, unknown_value, selected_value);
                let result_mask =
                    state
                        .builder
                        .ins()
                        .select(cond_is_unknown, unknown_mask, selected_mask);
                let result = apply_d_width_mask(state, result, ty, d_width);
                let result_mask = apply_d_width_mask(state, result_mask, ty, d_width);

                state.regs.insert(
                    *dst,
                    TransValue::FourState {
                        values: vec![result],
                        masks: vec![result_mask],
                    },
                );
            } else {
                let result = apply_d_width_mask(state, selected_value, ty, d_width);
                state.regs.insert(*dst, TransValue::TwoState(vec![result]));
            }
        } else {
            let n_chunks = d_width.div_ceil(64);
            let tv_chunks = state.regs[then_val].load_value_chunks(state.builder);
            let ev_chunks = state.regs[else_val].load_value_chunks(state.builder);
            let tm_chunks = state.regs[then_val]
                .load_mask_chunks(state.builder)
                .unwrap_or_default();
            let em_chunks = state.regs[else_val]
                .load_mask_chunks(state.builder)
                .unwrap_or_default();
            let mut res_chunks = Vec::with_capacity(n_chunks);
            let mut mask_chunks = Vec::with_capacity(n_chunks);
            for i in 0..n_chunks {
                let tv = get_chunk_as_i64(state.builder, &tv_chunks, i);
                let ev = get_chunk_as_i64(state.builder, &ev_chunks, i);
                let selected_value = state.builder.ins().select(cond_is_true, tv, ev);
                let mut result = selected_value;

                if self.options.four_state {
                    let tm = get_chunk_as_i64(state.builder, &tm_chunks, i);
                    let em = get_chunk_as_i64(state.builder, &em_chunks, i);
                    let selected_mask = state.builder.ins().select(cond_is_true, tm, em);
                    let value_diff = state.builder.ins().bxor(tv, ev);
                    let mask_diff = state.builder.ins().bxor(tm, em);
                    let diff = state.builder.ins().bor(value_diff, mask_diff);
                    let unknown_value = state.builder.ins().bor(tv, diff);
                    let unknown_mask = state.builder.ins().bor(tm, diff);
                    result =
                        state
                            .builder
                            .ins()
                            .select(cond_is_unknown, unknown_value, selected_value);
                    let result_mask =
                        state
                            .builder
                            .ins()
                            .select(cond_is_unknown, unknown_mask, selected_mask);
                    mask_chunks.push(result_mask);
                }

                res_chunks.push(result);
            }

            if !d_width.is_multiple_of(64) {
                let logical_mask = ((1u64 << (d_width % 64)) - 1) as i64;
                let last = n_chunks - 1;
                res_chunks[last] = state
                    .builder
                    .ins()
                    .band_imm_s(res_chunks[last], logical_mask);
                if self.options.four_state {
                    mask_chunks[last] = state
                        .builder
                        .ins()
                        .band_imm_s(mask_chunks[last], logical_mask);
                }
            }

            if self.options.four_state {
                state.regs.insert(
                    *dst,
                    TransValue::FourState {
                        values: res_chunks,
                        masks: mask_chunks,
                    },
                );
            } else {
                state.regs.insert(*dst, TransValue::TwoState(res_chunks));
            }
        }
    }

    fn translate_count_unary_inst(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        op: &UnaryOp,
        rhs: &RegisterId,
    ) {
        let r_width = state.register_map[rhs].width();
        let d_width = state.register_map[dst].width();
        let r_chunks = state.regs[rhs].load_value_chunks(state.builder);
        let count = wide_ops::emit_wide_bit_count(state.builder, op, &r_chunks, r_width);
        let final_num_chunks = d_width.div_ceil(64);
        let zero = state.builder.ins().iconst(types::I64, 0);

        let mut values = if d_width <= 64 {
            let dst_ty = get_cl_type(d_width);
            let value = cast_type(state.builder, count, dst_ty);
            vec![apply_d_width_mask(state, value, dst_ty, d_width)]
        } else {
            std::iter::once(count)
                .chain(std::iter::repeat_n(
                    zero,
                    final_num_chunks.saturating_sub(1),
                ))
                .collect()
        };

        if !self.options.four_state {
            state.regs.insert(*dst, TransValue::TwoState(values));
            return;
        }

        let r_masks = state.regs[rhs]
            .load_mask_chunks(state.builder)
            .unwrap_or_default();
        let source_chunks = r_width.div_ceil(64);
        let mut any_x = zero;
        for index in 0..source_chunks {
            let mut mask = get_chunk_as_i64(state.builder, &r_masks, index);
            if index + 1 == source_chunks && !r_width.is_multiple_of(64) {
                let valid_bits = r_width % 64;
                mask = state
                    .builder
                    .ins()
                    .band_imm_s(mask, ((1u64 << valid_bits) - 1) as i64);
            }
            any_x = state.builder.ins().bor(any_x, mask);
        }
        let has_x = state.builder.ins().icmp_imm_s(IntCC::NotEqual, any_x, 0);

        let masks = if d_width <= 64 {
            let dst_ty = get_cl_type(d_width);
            let zero = state.builder.ins().iconst(dst_ty, 0);
            let logical_mask = if d_width == dst_ty.bits() as usize {
                state.builder.ins().iconst(dst_ty, -1)
            } else {
                state
                    .builder
                    .ins()
                    .iconst(dst_ty, ((1u64 << d_width) - 1) as i64)
            };
            vec![state.builder.ins().select(has_x, logical_mask, zero)]
        } else {
            (0..final_num_chunks)
                .map(|index| {
                    let logical_mask =
                        if index + 1 == final_num_chunks && !d_width.is_multiple_of(64) {
                            ((1u64 << (d_width % 64)) - 1) as i64
                        } else {
                            -1
                        };
                    let logical_mask = state.builder.ins().iconst(types::I64, logical_mask);
                    state.builder.ins().select(has_x, logical_mask, zero)
                })
                .collect::<Vec<_>>()
        };
        for (value, mask) in values.iter_mut().zip(&masks) {
            *value = state.builder.ins().bor(*value, *mask);
        }
        state
            .regs
            .insert(*dst, TransValue::FourState { values, masks });
    }
}
pub(super) fn apply_d_width_mask(
    state: &mut TranslationState,
    val: Value,
    ty: Type,
    d_width: usize,
) -> Value {
    if d_width < ty.bits() as usize {
        let mask_val = (1u64 << d_width).wrapping_sub(1);
        let mask = state.builder.ins().iconst(ty, mask_val as i64);
        state.builder.ins().band(val, mask)
    } else {
        val
    }
}
pub(super) fn apply_d_width_mask_arith(
    state: &mut TranslationState,
    val: Value,
    ty: Type,
    d_width: usize,
) -> Value {
    let phys_bits = ty.bits() as i64;
    if d_width < (phys_bits as usize) {
        let shift_back_amt = phys_bits - (d_width as i64);

        let tmp = state.builder.ins().ishl_imm_s(val, shift_back_amt);
        state.builder.ins().sshr_imm_s(tmp, shift_back_amt)
    } else {
        val
    }
}
