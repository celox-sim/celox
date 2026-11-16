//! Binary operations with an operand or a result wider than 64 bits: the
//! memory-backed shifts, and the operator families on chunks in registers.

use cranelift::{codegen::ir::MemFlagsData as MemFlags, prelude::*};

use super::core::{MEM_SHIFT_THRESHOLD, TransValue, alloc_stack_slot, cast_type, get_chunk_as_i64};
use super::{SIRTranslator, TranslationState, wide_ops};
use crate::{BinaryOp, RegisterId};

impl SIRTranslator {
    /// A binary operation with an operand or a result wider than 64 bits.
    pub(super) fn translate_wide_binary(
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
        // --- Over 64-bit: Multi-word (i64) Operation ---
        let num_chunks = common_logical_width.div_ceil(64);
        let final_num_chunks = d_width.div_ceil(64);

        let is_shift = matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar);

        if is_shift && num_chunks >= MEM_SHIFT_THRESHOLD {
            self.translate_wide_shift_mem(
                state,
                dst,
                lhs,
                op,
                rhs,
                l_width,
                d_width,
                num_chunks,
                final_num_chunks,
            );
        } else {
            self.translate_wide_binary_regs(
                state,
                dst,
                lhs,
                op,
                rhs,
                l_width,
                r_width,
                d_width,
                common_logical_width,
                num_chunks,
                final_num_chunks,
                is_shift,
            );
        }
    }

    /// A wide shift of at least `MEM_SHIFT_THRESHOLD` chunks, through stack slots.
    fn translate_wide_shift_mem(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        lhs: &RegisterId,
        op: &BinaryOp,
        rhs: &RegisterId,
        l_width: usize,
        d_width: usize,
        num_chunks: usize,
        final_num_chunks: usize,
    ) {
        // === Memory-backed shift path (O(n)) ===
        // Allocate source slot sized to num_chunks (common_logical_width)
        // and zero-pad beyond the actual source length to avoid OOB reads.
        let src_chunks = state.regs[lhs].load_value_chunks(state.builder);
        let (_, l_addr) = alloc_stack_slot(state.builder, num_chunks);
        for (i, &v) in src_chunks.iter().enumerate() {
            state
                .builder
                .ins()
                .store(MemFlags::new(), v, l_addr, (i * 8) as i32);
        }
        let zero_pad = state.builder.ins().iconst(types::I64, 0);
        for i in src_chunks.len()..num_chunks {
            state
                .builder
                .ins()
                .store(MemFlags::new(), zero_pad, l_addr, (i * 8) as i32);
        }
        let r_chunks = state.regs[rhs].load_value_chunks(state.builder);

        // Allocate destination slot (large enough for full computation)
        let (_, dst_addr) = alloc_stack_slot(state.builder, num_chunks);

        if matches!(op, BinaryOp::Sar) {
            wide_ops::emit_wide_sar_mem(
                state.builder,
                l_addr,
                &r_chunks,
                dst_addr,
                num_chunks,
                l_width,
            );
        } else {
            wide_ops::emit_wide_shift_mem(
                state.builder,
                op,
                l_addr,
                &r_chunks,
                dst_addr,
                num_chunks,
            );
        }

        let mask_addr = if self.options.four_state {
            Some(wide_shift_mem_mask(
                state, lhs, op, rhs, &r_chunks, dst_addr, num_chunks, l_width,
            ))
        } else {
            None
        };

        // Physical chunks can outlive this shift in a wider consumer.
        // Clear padding in both planes before keeping the stack slot
        // or materializing it into registers, including all-X results.
        let last_chunk_bits = d_width % 64;
        if last_chunk_bits != 0 {
            let width_mask = state
                .builder
                .ins()
                .iconst(types::I64, ((1u64 << last_chunk_bits) - 1) as i64);
            let offset = ((final_num_chunks - 1) * 8) as i32;
            for addr in std::iter::once(dst_addr).chain(mask_addr) {
                let value = state
                    .builder
                    .ins()
                    .load(types::I64, MemFlags::new(), addr, offset);
                let value = state.builder.ins().band(value, width_mask);
                state
                    .builder
                    .ins()
                    .store(MemFlags::new(), value, addr, offset);
            }
        }

        // Only keep MemBacked if the destination is still wide enough.
        // Narrow destinations must be materialized as TwoState/FourState
        // so that subsequent narrow paths can access first_value().
        if final_num_chunks >= MEM_SHIFT_THRESHOLD {
            state.regs.insert(
                *dst,
                TransValue::MemBacked {
                    addr: dst_addr,
                    num_chunks: final_num_chunks,
                    mask_addr,
                },
            );
        } else {
            // Load result chunks from stack slot
            let res_chunks: Vec<Value> = (0..final_num_chunks)
                .map(|i| {
                    state
                        .builder
                        .ins()
                        .load(types::I64, MemFlags::new(), dst_addr, (i * 8) as i32)
                })
                .collect();

            if self.options.four_state {
                let res_masks: Vec<Value> = if let Some(ma) = mask_addr {
                    (0..final_num_chunks)
                        .map(|i| {
                            state.builder.ins().load(
                                types::I64,
                                MemFlags::new(),
                                ma,
                                (i * 8) as i32,
                            )
                        })
                        .collect()
                } else {
                    (0..final_num_chunks)
                        .map(|_| state.builder.ins().iconst(types::I64, 0))
                        .collect()
                };
                // The memory shift has already handled an unknown count.
                state.regs.insert(
                    *dst,
                    TransValue::FourState {
                        values: res_chunks,
                        masks: res_masks,
                    },
                );
            } else {
                state.regs.insert(*dst, TransValue::TwoState(res_chunks));
            }
        }
    }

    /// A wide binary operation on chunks held in registers.
    fn translate_wide_binary_regs(
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
        num_chunks: usize,
        final_num_chunks: usize,
        is_shift: bool,
    ) {
        // === Register-based path (existing) ===
        let l_chunks = state.regs[lhs].load_value_chunks(state.builder);
        let r_chunks = state.regs[rhs].load_value_chunks(state.builder);

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
                r_width,
                common_logical_width,
            )
        };

        res_chunks.truncate(final_num_chunks);
        while res_chunks.len() < final_num_chunks {
            res_chunks.push(state.builder.ins().iconst(types::I64, 0));
        }

        if self.options.four_state {
            // Get or generate zero masks for each operand
            let l_masks: Vec<Value> = state.regs[lhs]
                .load_mask_chunks(state.builder)
                .unwrap_or_else(|| {
                    (0..num_chunks)
                        .map(|_| state.builder.ins().iconst(types::I64, 0))
                        .collect()
                });
            let r_masks: Vec<Value> = state.regs[rhs]
                .load_mask_chunks(state.builder)
                .unwrap_or_else(|| {
                    (0..num_chunks)
                        .map(|_| state.builder.ins().iconst(types::I64, 0))
                        .collect()
                });

            let mut res_masks = wide_binary_mask(
                state,
                op,
                &l_chunks,
                &r_chunks,
                &l_masks,
                &r_masks,
                &mut res_chunks,
                num_chunks,
                final_num_chunks,
                l_width,
            );

            res_masks.truncate(final_num_chunks);
            while res_masks.len() < final_num_chunks {
                res_masks.push(state.builder.ins().iconst(types::I64, 0));
            }

            // Shifts preserve the payload (including Z), but neither
            // plane may retain bits beyond the logical result width.
            let last_chunk_bits = d_width % 64;
            if last_chunk_bits != 0 && !res_masks.is_empty() {
                let width_mask_val = ((1u64 << last_chunk_bits) - 1) as i64;
                let width_mask = state.builder.ins().iconst(types::I64, width_mask_val);
                let last_idx = res_masks.len() - 1;
                res_masks[last_idx] = state.builder.ins().band(res_masks[last_idx], width_mask);
                if is_shift {
                    res_chunks[last_idx] =
                        state.builder.ins().band(res_chunks[last_idx], width_mask);
                }
            }

            // Logical/arithmetic computations produce X; shifts move Z.
            let normalized: Vec<_> = if is_shift {
                res_chunks
            } else {
                res_chunks
                    .iter()
                    .zip(&res_masks)
                    .map(|(&v, &m)| state.builder.ins().bor(v, m))
                    .collect()
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

/// The mask of a memory-backed wide shift, shifted through stack slots of
/// its own. An unknown count also replaces the value at `dst_addr` with X.
/// Returns the address of the mask.
fn wide_shift_mem_mask(
    state: &mut TranslationState,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    r_chunks: &[Value],
    dst_addr: Value,
    num_chunks: usize,
    l_width: usize,
) -> Value {
    // Get masks for both operands
    let l_masks: Vec<Value> = state.regs[lhs]
        .load_mask_chunks(state.builder)
        .unwrap_or_else(|| {
            (0..num_chunks)
                .map(|_| state.builder.ins().iconst(types::I64, 0))
                .collect()
        });
    let r_masks: Vec<Value> = state.regs[rhs]
        .load_mask_chunks(state.builder)
        .unwrap_or_else(|| {
            (0..num_chunks)
                .map(|_| state.builder.ins().iconst(types::I64, 0))
                .collect()
        });

    // Check if shift amount has any X
    let mut r_any_x = state.builder.ins().iconst(types::I64, 0);
    for m in &r_masks {
        let m_i64 = cast_type(state.builder, *m, types::I64);
        r_any_x = state.builder.ins().bor(r_any_x, m_i64);
    }
    let zero = state.builder.ins().iconst(types::I64, 0);
    let shift_has_x = state.builder.ins().icmp(IntCC::NotEqual, r_any_x, zero);

    // Spill LHS masks to stack slot (zero-pad beyond actual length)
    let (_, l_mask_addr) = alloc_stack_slot(state.builder, num_chunks);
    for (i, &m) in l_masks.iter().enumerate() {
        state
            .builder
            .ins()
            .store(MemFlags::new(), m, l_mask_addr, (i * 8) as i32);
    }
    let zero_mask_pad = state.builder.ins().iconst(types::I64, 0);
    for i in l_masks.len()..num_chunks {
        state
            .builder
            .ins()
            .store(MemFlags::new(), zero_mask_pad, l_mask_addr, (i * 8) as i32);
    }

    // Shift masks using memory-backed path
    let (_, mask_dst_addr) = alloc_stack_slot(state.builder, num_chunks);
    if matches!(op, BinaryOp::Sar) {
        wide_ops::emit_wide_sar_mem(
            state.builder,
            l_mask_addr,
            r_chunks,
            mask_dst_addr,
            num_chunks,
            l_width,
        );
    } else {
        wide_ops::emit_wide_shift_mem(
            state.builder,
            op,
            l_mask_addr,
            r_chunks,
            mask_dst_addr,
            num_chunks,
        );
    }

    // An unknown count produces X in both planes; known counts
    // preserve the shifted payload, including Z bits.
    let all_ones = state.builder.ins().iconst(types::I64, -1i64);
    for i in 0..num_chunks {
        let m =
            state
                .builder
                .ins()
                .load(types::I64, MemFlags::new(), mask_dst_addr, (i * 8) as i32);
        let v = state
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), dst_addr, (i * 8) as i32);
        let v = state.builder.ins().select(shift_has_x, all_ones, v);
        state
            .builder
            .ins()
            .store(MemFlags::new(), v, dst_addr, (i * 8) as i32);
        let selected = state.builder.ins().select(shift_has_x, all_ones, m);
        state
            .builder
            .ins()
            .store(MemFlags::new(), selected, mask_dst_addr, (i * 8) as i32);
    }

    mask_dst_addr
}

/// The mask of a wide binary operation on chunks held in registers. Shifts
/// and case and wildcard equality also change the value `res_chunks`.
fn wide_binary_mask(
    state: &mut TranslationState,
    op: &BinaryOp,
    l_chunks: &[Value],
    r_chunks: &[Value],
    l_masks: &[Value],
    r_masks: &[Value],
    res_chunks: &mut Vec<Value>,
    num_chunks: usize,
    final_num_chunks: usize,
    l_width: usize,
) -> Vec<Value> {
    match op {
        // Bitwise ops: precise mask computation per chunk
        BinaryOp::And => (0..num_chunks)
            .map(|i| {
                let lm = get_chunk_as_i64(state.builder, l_masks, i);
                let rm = get_chunk_as_i64(state.builder, r_masks, i);
                let lv = get_chunk_as_i64(state.builder, l_chunks, i);
                let rv = get_chunk_as_i64(state.builder, r_chunks, i);

                let m1 = state.builder.ins().band(lm, rm);
                let m2 = state.builder.ins().band(lm, rv);
                let m3 = state.builder.ins().band(rm, lv);
                let mt = state.builder.ins().bor(m1, m2);
                state.builder.ins().bor(mt, m3)
            })
            .collect(),
        BinaryOp::Or => (0..num_chunks)
            .map(|i| {
                let lm = get_chunk_as_i64(state.builder, l_masks, i);
                let rm = get_chunk_as_i64(state.builder, r_masks, i);
                let lv = get_chunk_as_i64(state.builder, l_chunks, i);
                let rv = get_chunk_as_i64(state.builder, r_chunks, i);

                let m1 = state.builder.ins().band(lm, rm);
                let m2 = state.builder.ins().band_not(lm, rv);
                let m3 = state.builder.ins().band_not(rm, lv);
                let mt = state.builder.ins().bor(m1, m2);
                state.builder.ins().bor(mt, m3)
            })
            .collect(),
        BinaryOp::Xor => (0..num_chunks)
            .map(|i| {
                let lm = get_chunk_as_i64(state.builder, l_masks, i);
                let rm = get_chunk_as_i64(state.builder, r_masks, i);
                state.builder.ins().bor(lm, rm)
            })
            .collect(),
        BinaryOp::Shr | BinaryOp::Shl | BinaryOp::Sar => wide_shift_mask(
            state, op, l_masks, r_masks, r_chunks, res_chunks, num_chunks, l_width,
        ),
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => wide_wildcard_equality(
            state,
            op,
            l_chunks,
            r_chunks,
            l_masks,
            r_masks,
            res_chunks,
            num_chunks,
            final_num_chunks,
        ),
        BinaryOp::Eq | BinaryOp::Ne => wide_equality_mask(
            state,
            l_chunks,
            r_chunks,
            l_masks,
            r_masks,
            num_chunks,
            final_num_chunks,
        ),
        BinaryOp::EqCase | BinaryOp::NeCase => wide_case_equality(
            state,
            op,
            l_chunks,
            r_chunks,
            l_masks,
            r_masks,
            res_chunks,
            num_chunks,
            final_num_chunks,
        ),
        BinaryOp::LogicAnd | BinaryOp::LogicOr => wide_logical_mask(
            state,
            op,
            l_chunks,
            r_chunks,
            l_masks,
            r_masks,
            num_chunks,
            final_num_chunks,
        ),
        // All other ops: conservative — if any mask chunk is non-zero, result is all-X
        _ => wide_conservative_mask(state, op, r_chunks, l_masks, r_masks, final_num_chunks),
    }
}

/// The mask of a wide shift on chunks held in registers. An unknown count
/// also replaces the value `res_chunks` with X.
fn wide_shift_mask(
    state: &mut TranslationState,
    op: &BinaryOp,
    l_masks: &[Value],
    r_masks: &[Value],
    r_chunks: &[Value],
    res_chunks: &mut [Value],
    num_chunks: usize,
    l_width: usize,
) -> Vec<Value> {
    // If shift amount (rhs) has ANY X, result is all-X.
    let mut r_any_x = state.builder.ins().iconst(types::I64, 0);
    for m in r_masks {
        let m_i64 = cast_type(state.builder, *m, types::I64);
        r_any_x = state.builder.ins().bor(r_any_x, m_i64);
    }
    let zero = state.builder.ins().iconst(types::I64, 0);
    let shift_has_x = state.builder.ins().icmp(IntCC::NotEqual, r_any_x, zero);

    let all_x = state.builder.ins().iconst(types::I64, -1);
    for value in res_chunks.iter_mut() {
        *value = state.builder.ins().select(shift_has_x, all_x, *value);
    }

    let shifted_masks = if matches!(op, BinaryOp::Sar) {
        wide_ops::emit_wide_sar(state.builder, l_masks, r_chunks, num_chunks, l_width)
    } else {
        wide_ops::emit_wide_shift(state.builder, op, l_masks, r_chunks, num_chunks)
    };

    let all_ones = state.builder.ins().iconst(types::I64, -1i64);
    shifted_masks
        .into_iter()
        .map(|m| state.builder.ins().select(shift_has_x, all_ones, m))
        .collect()
}

/// Wide `EqWildcard` and `NeWildcard` on four-state operands: the value
/// `res_chunks`, and the mask.
fn wide_wildcard_equality(
    state: &mut TranslationState,
    op: &BinaryOp,
    l_chunks: &[Value],
    r_chunks: &[Value],
    l_masks: &[Value],
    r_masks: &[Value],
    res_chunks: &mut Vec<Value>,
    num_chunks: usize,
    final_num_chunks: usize,
) -> Vec<Value> {
    // IEEE 1800 ==?/!=?: RHS X/Z bits are wildcards (don't care).
    // Compare only at positions where both LHS is definite and RHS is non-wildcard.
    let mut accumulated_mismatch = state.builder.ins().iconst(types::I64, 0);
    let mut accumulated_x = state.builder.ins().iconst(types::I64, 0);

    let effective_l: Vec<Value> = (0..num_chunks)
        .map(|i| {
            let lv = get_chunk_as_i64(state.builder, l_chunks, i);
            let lm = get_chunk_as_i64(state.builder, l_masks, i);
            let rm = get_chunk_as_i64(state.builder, r_masks, i);
            let compare_mask = state.builder.ins().bnot(rm);
            let l_definite = state.builder.ins().bnot(lm);
            let definite_compare = state.builder.ins().band(compare_mask, l_definite);
            state.builder.ins().band(lv, definite_compare)
        })
        .collect();
    let effective_r: Vec<Value> = (0..num_chunks)
        .map(|i| {
            let rv = get_chunk_as_i64(state.builder, r_chunks, i);
            let lm = get_chunk_as_i64(state.builder, l_masks, i);
            let rm = get_chunk_as_i64(state.builder, r_masks, i);
            let compare_mask = state.builder.ins().bnot(rm);
            let l_definite = state.builder.ins().bnot(lm);
            let definite_compare = state.builder.ins().band(compare_mask, l_definite);
            state.builder.ins().band(rv, definite_compare)
        })
        .collect();

    for i in 0..num_chunks {
        let lv = get_chunk_as_i64(state.builder, l_chunks, i);
        let rv = get_chunk_as_i64(state.builder, r_chunks, i);
        let lm = get_chunk_as_i64(state.builder, l_masks, i);
        let rm = get_chunk_as_i64(state.builder, r_masks, i);
        let compare_mask = state.builder.ins().bnot(rm);
        let l_definite = state.builder.ins().bnot(lm);
        let definite_compare = state.builder.ins().band(compare_mask, l_definite);
        let xor_bits = state.builder.ins().bxor(lv, rv);
        let mismatch = state.builder.ins().band(xor_bits, definite_compare);
        accumulated_mismatch = state.builder.ins().bor(accumulated_mismatch, mismatch);
        let x_at = state.builder.ins().band(lm, compare_mask);
        accumulated_x = state.builder.ins().bor(accumulated_x, x_at);
    }

    let zero = state.builder.ins().iconst(types::I64, 0);
    let has_mismatch = state
        .builder
        .ins()
        .icmp(IntCC::NotEqual, accumulated_mismatch, zero);
    let has_x = state
        .builder
        .ins()
        .icmp(IntCC::NotEqual, accumulated_x, zero);
    let all_ones = state.builder.ins().iconst(types::I64, -1i64);
    let x_mask = state.builder.ins().select(has_x, all_ones, zero);
    let mask_val = state.builder.ins().select(has_mismatch, zero, x_mask);

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
        new_res.push(state.builder.ins().iconst(types::I64, 0));
    }
    *res_chunks = new_res;

    vec![mask_val; final_num_chunks]
}

/// The mask of a wide `Eq` or `Ne`.
fn wide_equality_mask(
    state: &mut TranslationState,
    l_chunks: &[Value],
    r_chunks: &[Value],
    l_masks: &[Value],
    r_masks: &[Value],
    num_chunks: usize,
    final_num_chunks: usize,
) -> Vec<Value> {
    let mut unknown = state.builder.ins().iconst(types::I64, 0);
    let mut mismatch = state.builder.ins().iconst(types::I64, 0);
    for i in 0..num_chunks {
        let lv = get_chunk_as_i64(state.builder, l_chunks, i);
        let rv = get_chunk_as_i64(state.builder, r_chunks, i);
        let lm = get_chunk_as_i64(state.builder, l_masks, i);
        let rm = get_chunk_as_i64(state.builder, r_masks, i);
        let lm = cast_type(state.builder, lm, types::I64);
        let rm = cast_type(state.builder, rm, types::I64);
        let chunk_unknown = state.builder.ins().bor(lm, rm);
        let known = state.builder.ins().bnot(chunk_unknown);
        let diff = state.builder.ins().bxor(lv, rv);
        let chunk_mismatch = state.builder.ins().band(diff, known);
        unknown = state.builder.ins().bor(unknown, chunk_unknown);
        mismatch = state.builder.ins().bor(mismatch, chunk_mismatch);
    }
    let no_mismatch = state.builder.ins().icmp_imm_u(IntCC::Equal, mismatch, 0);
    let has_unknown = state.builder.ins().icmp_imm_u(IntCC::NotEqual, unknown, 0);
    let uncertain = state.builder.ins().band(no_mismatch, has_unknown);
    let zero = state.builder.ins().iconst(types::I64, 0);
    let one = state.builder.ins().iconst(types::I64, 1);
    let mask = state.builder.ins().select(uncertain, one, zero);
    let mut masks = vec![zero; final_num_chunks];
    masks[0] = mask;
    masks
}

/// Wide `EqCase` and `NeCase` on four-state operands: the value
/// `res_chunks`, and the mask, which is zero.
fn wide_case_equality(
    state: &mut TranslationState,
    op: &BinaryOp,
    l_chunks: &[Value],
    r_chunks: &[Value],
    l_masks: &[Value],
    r_masks: &[Value],
    res_chunks: &mut Vec<Value>,
    num_chunks: usize,
    final_num_chunks: usize,
) -> Vec<Value> {
    let mut accumulated_diff = state.builder.ins().iconst(types::I64, 0);
    for i in 0..num_chunks {
        let lv = get_chunk_as_i64(state.builder, l_chunks, i);
        let rv = get_chunk_as_i64(state.builder, r_chunks, i);
        let lm = get_chunk_as_i64(state.builder, l_masks, i);
        let rm = get_chunk_as_i64(state.builder, r_masks, i);
        let value_diff = state.builder.ins().bxor(lv, rv);
        let mask_diff = state.builder.ins().bxor(lm, rm);
        let chunk_diff = state.builder.ins().bor(value_diff, mask_diff);
        accumulated_diff = state.builder.ins().bor(accumulated_diff, chunk_diff);
    }

    let zero = state.builder.ins().iconst(types::I64, 0);
    let matches = state.builder.ins().icmp(
        if matches!(op, BinaryOp::EqCase) {
            IntCC::Equal
        } else {
            IntCC::NotEqual
        },
        accumulated_diff,
        zero,
    );
    let one = state.builder.ins().iconst(types::I64, 1);
    let result = state.builder.ins().select(matches, one, zero);
    res_chunks.clear();
    res_chunks.push(result);
    while res_chunks.len() < final_num_chunks {
        res_chunks.push(state.builder.ins().iconst(types::I64, 0));
    }
    (0..final_num_chunks)
        .map(|_| state.builder.ins().iconst(types::I64, 0))
        .collect()
}

/// The mask of a wide `LogicAnd` or `LogicOr`.
fn wide_logical_mask(
    state: &mut TranslationState,
    op: &BinaryOp,
    l_chunks: &[Value],
    r_chunks: &[Value],
    l_masks: &[Value],
    r_masks: &[Value],
    num_chunks: usize,
    final_num_chunks: usize,
) -> Vec<Value> {
    // IEEE 1800 dominant-value: 0 for &&, 1 for ||
    let mut l_val_or = state.builder.ins().iconst(types::I64, 0);
    let mut r_val_or = state.builder.ins().iconst(types::I64, 0);
    let mut l_mask_or = state.builder.ins().iconst(types::I64, 0);
    let mut r_mask_or = state.builder.ins().iconst(types::I64, 0);
    for i in 0..num_chunks {
        let lv = get_chunk_as_i64(state.builder, l_chunks, i);
        let rv = get_chunk_as_i64(state.builder, r_chunks, i);
        let lm = get_chunk_as_i64(state.builder, l_masks, i);
        let rm = get_chunk_as_i64(state.builder, r_masks, i);
        l_val_or = state.builder.ins().bor(l_val_or, lv);
        r_val_or = state.builder.ins().bor(r_val_or, rv);
        l_mask_or = state.builder.ins().bor(l_mask_or, lm);
        r_mask_or = state.builder.ins().bor(r_mask_or, rm);
    }

    let zero = state.builder.ins().iconst(types::I64, 0);
    let any_x_all = state.builder.ins().bor(l_mask_or, r_mask_or);
    let has_x = state.builder.ins().icmp(IntCC::NotEqual, any_x_all, zero);
    let all_ones = state.builder.ins().iconst(types::I64, -1i64);
    let conservative = state.builder.ins().select(has_x, all_ones, zero);

    let dominant = if matches!(op, BinaryOp::LogicAnd) {
        // 0 is dominant: check if either operand is definite false
        // definite false = (v | m) == 0
        let l_vm = state.builder.ins().bor(l_val_or, l_mask_or);
        let r_vm = state.builder.ins().bor(r_val_or, r_mask_or);
        let l_def_false = state.builder.ins().icmp(IntCC::Equal, l_vm, zero);
        let r_def_false = state.builder.ins().icmp(IntCC::Equal, r_vm, zero);
        state.builder.ins().bor(l_def_false, r_def_false)
    } else {
        // 1 is dominant: check if either operand is definite true
        // definite true = (v & ~m) != 0 — only definite-1 bits count
        let mut l_definite_or = state.builder.ins().iconst(types::I64, 0);
        let mut r_definite_or = state.builder.ins().iconst(types::I64, 0);
        for i in 0..num_chunks {
            let lv = get_chunk_as_i64(state.builder, l_chunks, i);
            let lm = get_chunk_as_i64(state.builder, l_masks, i);
            let rv = get_chunk_as_i64(state.builder, r_chunks, i);
            let rm = get_chunk_as_i64(state.builder, r_masks, i);
            let not_lm = state.builder.ins().bnot(lm);
            let l_def = state.builder.ins().band(lv, not_lm);
            let not_rm = state.builder.ins().bnot(rm);
            let r_def = state.builder.ins().band(rv, not_rm);
            l_definite_or = state.builder.ins().bor(l_definite_or, l_def);
            r_definite_or = state.builder.ins().bor(r_definite_or, r_def);
        }
        let l_def_true = state
            .builder
            .ins()
            .icmp(IntCC::NotEqual, l_definite_or, zero);
        let r_def_true = state
            .builder
            .ins()
            .icmp(IntCC::NotEqual, r_definite_or, zero);
        state.builder.ins().bor(l_def_true, r_def_true)
    };

    let mask_val = state.builder.ins().select(dominant, zero, conservative);
    vec![mask_val; final_num_chunks]
}

/// The mask of the remaining wide operations (arithmetic, division and
/// ordered comparison): all X when an operand has an unknown bit.
fn wide_conservative_mask(
    state: &mut TranslationState,
    op: &BinaryOp,
    r_chunks: &[Value],
    l_masks: &[Value],
    r_masks: &[Value],
    final_num_chunks: usize,
) -> Vec<Value> {
    // OR all mask chunks together to detect any X
    let mut any_x = state.builder.ins().iconst(types::I64, 0);
    for m in l_masks.iter().chain(r_masks.iter()) {
        let m_i64 = cast_type(state.builder, *m, types::I64);
        any_x = state.builder.ins().bor(any_x, m_i64);
    }
    let zero = state.builder.ins().iconst(types::I64, 0);
    let mut has_x = state.builder.ins().icmp(IntCC::NotEqual, any_x, zero);
    if matches!(
        op,
        BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
    ) {
        let mut divisor = zero;
        for &chunk in r_chunks {
            let chunk = cast_type(state.builder, chunk, types::I64);
            divisor = state.builder.ins().bor(divisor, chunk);
        }
        let zero_divisor = state.builder.ins().icmp(IntCC::Equal, divisor, zero);
        has_x = state.builder.ins().bor(has_x, zero_divisor);
    }
    let all_ones = state.builder.ins().iconst(types::I64, -1i64);
    let mask_val = state.builder.ins().select(has_x, all_ones, zero);
    vec![mask_val; final_num_chunks]
}
