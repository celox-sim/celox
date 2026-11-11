//! Immediates, slices, muxes and concatenations.

use super::*;

// ============================================================
// Instruction compilers
// ============================================================
pub(super) fn compile_imm(
    dst: &RegisterId,
    val: &SIRValue,
    _unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    _four_state: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let reg = &locals.reg_map[dst];
    let digits = val.payload.to_u64_digits();
    for c in 0..reg.num_chunks {
        let v = digits.get(c).copied().unwrap_or(0);
        instrs.push(Instruction::I64Const(v as i64));
        instrs.push(Instruction::LocalSet(reg.value_idx + c as u32));
    }
    if let Some(mask_idx) = reg.mask_idx {
        let mask_digits = val.mask.to_u64_digits();
        for c in 0..reg.num_chunks {
            let m = mask_digits.get(c).copied().unwrap_or(0);
            instrs.push(Instruction::I64Const(m as i64));
            instrs.push(Instruction::LocalSet(mask_idx + c as u32));
        }
    }
}

pub(super) fn emit_slice_chunks(
    dst: &RegLocal,
    src: &RegLocal,
    bit_offset: usize,
    width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let num_dst_chunks = width.div_ceil(64);
    let mut remaining = width;
    let mut pos = bit_offset;

    for out_idx in 0..dst.num_chunks {
        if out_idx >= num_dst_chunks {
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst.value_idx + out_idx as u32));
            continue;
        }

        let chunk_idx = pos / 64;
        let chunk_off = pos % 64;
        let chunk_width = remaining.min(64);
        let bits_in_chunk = (64 - chunk_off).min(remaining);

        emit_wide_get_chunk(instrs, src, chunk_idx);
        if chunk_off > 0 {
            instrs.push(Instruction::I64Const(chunk_off as i64));
            instrs.push(Instruction::I64ShrU);
        }

        // Only join the next source chunk when this destination chunk crosses
        // a boundary. Shifting by 64 would wrap to zero in Wasm.
        if bits_in_chunk < chunk_width {
            emit_wide_get_chunk(instrs, src, chunk_idx + 1);
            instrs.push(Instruction::I64Const(bits_in_chunk as i64));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }

        emit_mask_to_width(instrs, chunk_width);
        instrs.push(Instruction::LocalSet(dst.value_idx + out_idx as u32));

        remaining -= chunk_width;
        pos += chunk_width;
    }
}

pub(super) fn compile_slice(
    dst: &RegisterId,
    src: &RegisterId,
    bit_offset: usize,
    width: usize,
    four_state: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = locals.reg_map[dst].clone();
    let s = locals.reg_map[src].clone();
    emit_slice_chunks(&d, &s, bit_offset, width, instrs);

    if four_state {
        if let (Some(dst_mask), Some(src_mask)) = (d.mask_idx, s.mask_idx) {
            let dst_mask_reg = RegLocal {
                value_idx: dst_mask,
                num_chunks: d.num_chunks,
                mask_idx: None,
            };
            let src_mask_reg = RegLocal {
                value_idx: src_mask,
                num_chunks: s.num_chunks,
                mask_idx: None,
            };
            emit_slice_chunks(&dst_mask_reg, &src_mask_reg, bit_offset, width, instrs);
        }
    }
}

pub(super) fn compile_mux(
    dst: &RegisterId,
    cond: &RegisterId,
    then_val: &RegisterId,
    else_val: &RegisterId,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d_width = unit.register_map[dst].width();
    let cond_width = unit.register_map[cond].width();
    let d_chunks = locals.reg_map[dst].num_chunks;
    let dst_reg = locals.reg_map[dst].clone();
    let cond_reg = locals.reg_map[cond].clone();
    let then_reg = locals.reg_map[then_val].clone();
    let else_reg = locals.reg_map[else_val].clone();

    // A condition is definitely true when any bit is a known one.  If there
    // is no known one but at least one X/Z bit, the condition is unknown;
    // otherwise it is definitely false.  Reduce every condition chunk so a
    // wide condition has the same semantics as a narrow one.
    let cond_is_true = locals.alloc(1);
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::LocalSet(cond_is_true));

    let cond_is_unknown = if four_state {
        let cond_mask = cond_reg.mask_idx.expect("four-state condition mask");
        let cond_has_unknown = locals.alloc(1);
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::LocalSet(cond_has_unknown));

        for i in 0..cond_reg.num_chunks {
            // Accumulate known-one bits: cond_value & !cond_mask.
            instrs.push(Instruction::LocalGet(cond_is_true));
            instrs.push(Instruction::LocalGet(cond_reg.value_idx + i as u32));
            instrs.push(Instruction::LocalGet(cond_mask + i as u32));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::I64And);
            emit_chunk_mask_to_width(instrs, i, cond_reg.num_chunks, cond_width);
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(cond_is_true));

            // Accumulate X/Z bits independently.  A known one dominates them.
            instrs.push(Instruction::LocalGet(cond_has_unknown));
            instrs.push(Instruction::LocalGet(cond_mask + i as u32));
            emit_chunk_mask_to_width(instrs, i, cond_reg.num_chunks, cond_width);
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(cond_has_unknown));
        }

        // Canonicalize the reductions to i64 booleans.
        instrs.push(Instruction::LocalGet(cond_is_true));
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::I64ExtendI32U);
        instrs.push(Instruction::LocalSet(cond_is_true));

        let is_unknown = locals.alloc(1);
        instrs.push(Instruction::LocalGet(cond_has_unknown));
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::LocalGet(cond_is_true));
        instrs.push(Instruction::I64Eqz);
        instrs.push(Instruction::I32And);
        instrs.push(Instruction::I64ExtendI32U);
        instrs.push(Instruction::LocalSet(is_unknown));
        Some(is_unknown)
    } else {
        for i in 0..cond_reg.num_chunks {
            instrs.push(Instruction::LocalGet(cond_is_true));
            instrs.push(Instruction::LocalGet(cond_reg.value_idx + i as u32));
            emit_chunk_mask_to_width(instrs, i, cond_reg.num_chunks, cond_width);
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(cond_is_true));
        }
        instrs.push(Instruction::LocalGet(cond_is_true));
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::I64ExtendI32U);
        instrs.push(Instruction::LocalSet(cond_is_true));
        None
    };

    let branch_masks = if four_state {
        Some((
            then_reg.mask_idx.expect("four-state then-value mask"),
            else_reg.mask_idx.expect("four-state else-value mask"),
            dst_reg.mask_idx.expect("four-state destination mask"),
        ))
    } else {
        None
    };
    let diff = branch_masks.map(|_| locals.alloc(1));

    // Broadcast each mutually-exclusive condition state to an i64 selection
    // mask so the per-chunk merge stays branchless.
    let true_bc = locals.alloc(1);
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::LocalGet(cond_is_true));
    instrs.push(Instruction::I64Sub);
    instrs.push(Instruction::LocalSet(true_bc));

    let unknown_bc = cond_is_unknown.map(|cond_is_unknown| {
        let local = locals.alloc(1);
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::LocalGet(cond_is_unknown));
        instrs.push(Instruction::I64Sub);
        instrs.push(Instruction::LocalSet(local));
        local
    });

    let false_bc = locals.alloc(1);
    instrs.push(Instruction::LocalGet(true_bc));
    if let Some(unknown_bc) = unknown_bc {
        instrs.push(Instruction::LocalGet(unknown_bc));
        instrs.push(Instruction::I64Or);
    }
    instrs.push(Instruction::I64Const(-1));
    instrs.push(Instruction::I64Xor);
    instrs.push(Instruction::LocalSet(false_bc));

    for i in 0..d_chunks {
        let tv_local = then_reg.value_idx + i as u32;
        let ev_local = else_reg.value_idx + i as u32;
        let dst_local = dst_reg.value_idx + i as u32;

        if let (Some((then_mask, else_mask, _)), Some(diff)) = (branch_masks, diff) {
            // For an unknown condition, bits that differ in either value or
            // mask become X: diff = (tv ^ ev) | (tm ^ em).
            instrs.push(Instruction::LocalGet(tv_local));
            instrs.push(Instruction::LocalGet(ev_local));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::LocalGet(then_mask + i as u32));
            instrs.push(Instruction::LocalGet(else_mask + i as u32));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::I64Or);
            emit_chunk_mask_to_width(instrs, i, d_chunks, d_width);
            instrs.push(Instruction::LocalSet(diff));
        }

        // Definite true/false selects the corresponding value.  An unknown
        // condition contributes the bitwise merge value.
        instrs.push(Instruction::LocalGet(true_bc));
        instrs.push(Instruction::LocalGet(tv_local));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::LocalGet(false_bc));
        instrs.push(Instruction::LocalGet(ev_local));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::I64Or);
        if let (Some(unknown_bc), Some(diff)) = (unknown_bc, diff) {
            instrs.push(Instruction::LocalGet(unknown_bc));
            instrs.push(Instruction::LocalGet(tv_local));
            instrs.push(Instruction::LocalGet(diff));
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
        }
        emit_chunk_mask_to_width(instrs, i, d_chunks, d_width);
        instrs.push(Instruction::LocalSet(dst_local));

        if let (Some((then_mask, else_mask, dst_mask)), Some(unknown_bc), Some(diff)) =
            (branch_masks, unknown_bc, diff)
        {
            instrs.push(Instruction::LocalGet(true_bc));
            instrs.push(Instruction::LocalGet(then_mask + i as u32));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalGet(false_bc));
            instrs.push(Instruction::LocalGet(else_mask + i as u32));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalGet(unknown_bc));
            instrs.push(Instruction::LocalGet(then_mask + i as u32));
            instrs.push(Instruction::LocalGet(diff));
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
            emit_chunk_mask_to_width(instrs, i, d_chunks, d_width);
            instrs.push(Instruction::LocalSet(dst_mask + i as u32));
        }
    }
}

pub(super) fn compile_concat(
    dst: &RegisterId,
    args: &[RegisterId],
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = &locals.reg_map[dst];
    let d_width = unit.register_map[dst].width();

    // Zero destination first
    for c in 0..d.num_chunks {
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
    }

    // Concatenate args (MSB first in args, so last arg = LSB).
    // Process in reverse: start at bit position 0.
    let mut bit_pos: usize = 0;
    for arg in args.iter().rev() {
        let a = &locals.reg_map[arg];
        let a_width = unit.register_map[arg].width();

        if d.num_chunks == 1 && a.num_chunks == 1 {
            // Simple case: everything fits in one chunk
            instrs.push(Instruction::LocalGet(d.value_idx));
            instrs.push(Instruction::LocalGet(a.value_idx));
            // Mask arg to its width
            if a_width < 64 {
                let mask = (1u64 << a_width) - 1;
                instrs.push(Instruction::I64Const(mask as i64));
                instrs.push(Instruction::I64And);
            }
            if bit_pos > 0 {
                instrs.push(Instruction::I64Const(bit_pos as i64));
                instrs.push(Instruction::I64Shl);
            }
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(d.value_idx));
        } else {
            // Wide concat: OR each src chunk into the correct position.
            for ac in 0..a.num_chunks {
                let src_bit_start = bit_pos + ac * 64;
                let dst_chunk = src_bit_start / 64;
                let dst_bit = src_bit_start % 64;

                if dst_chunk < d.num_chunks {
                    instrs.push(Instruction::LocalGet(d.value_idx + dst_chunk as u32));
                    instrs.push(Instruction::LocalGet(a.value_idx + ac as u32));
                    if dst_bit > 0 {
                        instrs.push(Instruction::I64Const(dst_bit as i64));
                        instrs.push(Instruction::I64Shl);
                    }
                    instrs.push(Instruction::I64Or);
                    instrs.push(Instruction::LocalSet(d.value_idx + dst_chunk as u32));

                    // If the shift causes overflow into next chunk
                    if dst_bit > 0 && dst_chunk + 1 < d.num_chunks {
                        instrs.push(Instruction::LocalGet(d.value_idx + (dst_chunk + 1) as u32));
                        instrs.push(Instruction::LocalGet(a.value_idx + ac as u32));
                        instrs.push(Instruction::I64Const((64 - dst_bit) as i64));
                        instrs.push(Instruction::I64ShrU);
                        instrs.push(Instruction::I64Or);
                        instrs.push(Instruction::LocalSet(d.value_idx + (dst_chunk + 1) as u32));
                    }
                }
            }
        }
        bit_pos += a_width;
    }

    // Mask top chunk to d_width
    let top_bits = d_width % 64;
    if top_bits > 0 && top_bits < 64 {
        let top = d.num_chunks - 1;
        let mask = (1u64 << top_bits) - 1;
        instrs.push(Instruction::LocalGet(d.value_idx + top as u32));
        instrs.push(Instruction::I64Const(mask as i64));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::LocalSet(d.value_idx + top as u32));
    }

    // 4-state concat: same logic for masks
    if four_state {
        if let Some(dm) = d.mask_idx {
            for c in 0..d.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dm + c as u32));
            }
            let mut bit_pos: usize = 0;
            for arg in args.iter().rev() {
                let a = &locals.reg_map[arg];
                let a_width = unit.register_map[arg].width();
                if let Some(am) = a.mask_idx {
                    for ac in 0..a.num_chunks {
                        let src_bit_start = bit_pos + ac * 64;
                        let dst_chunk = src_bit_start / 64;
                        let dst_bit = src_bit_start % 64;
                        if dst_chunk < d.num_chunks {
                            instrs.push(Instruction::LocalGet(dm + dst_chunk as u32));
                            instrs.push(Instruction::LocalGet(am + ac as u32));
                            if dst_bit > 0 {
                                instrs.push(Instruction::I64Const(dst_bit as i64));
                                instrs.push(Instruction::I64Shl);
                            }
                            instrs.push(Instruction::I64Or);
                            instrs.push(Instruction::LocalSet(dm + dst_chunk as u32));
                            if dst_bit > 0 && dst_chunk + 1 < d.num_chunks {
                                instrs.push(Instruction::LocalGet(dm + (dst_chunk + 1) as u32));
                                instrs.push(Instruction::LocalGet(am + ac as u32));
                                instrs.push(Instruction::I64Const((64 - dst_bit) as i64));
                                instrs.push(Instruction::I64ShrU);
                                instrs.push(Instruction::I64Or);
                                instrs.push(Instruction::LocalSet(dm + (dst_chunk + 1) as u32));
                            }
                        }
                    }
                }
                bit_pos += a_width;
            }
        }
    }
}
