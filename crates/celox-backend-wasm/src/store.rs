//! Stores to state memory and commits.

use super::*;

pub(super) fn compile_prepare_sparse_store(
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    op_width: usize,
    layout: &MemoryLayout,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let abs = addr.absolute_addr();
    let sparse = &layout.sparse_layouts[&abs];
    let stable_base = layout.offsets[&abs];
    let sparse_base = layout.sparse_base_offset + layout.sparse_offsets[&abs];
    let byte_size = layout.plane_size(&abs);
    let bit_offset = locals.alloc(1);
    let logical_offset = compile_logical_bit_offset(offset, locals, instrs);
    instrs.push(Instruction::LocalGet(logical_offset));
    instrs.push(Instruction::LocalSet(bit_offset));
    let start_chunk = locals.alloc(1);
    instrs.push(Instruction::LocalGet(bit_offset));
    instrs.push(Instruction::I64Const(6));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::LocalSet(start_chunk));
    let end_chunk = locals.alloc(1);
    instrs.push(Instruction::LocalGet(bit_offset));
    instrs.push(Instruction::I64Const(op_width.saturating_sub(1) as i64));
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::I64Const(6));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::LocalSet(end_chunk));
    let memarg = wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };

    for delta in 0..(op_width.div_ceil(64) + 1) {
        let candidate = locals.alloc(1);
        instrs.push(Instruction::LocalGet(start_chunk));
        instrs.push(Instruction::I64Const(delta as i64));
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalSet(candidate));
        let chunk = locals.alloc(1);
        instrs.push(Instruction::LocalGet(candidate));
        instrs.push(Instruction::LocalGet(start_chunk));
        instrs.push(Instruction::LocalGet(candidate));
        instrs.push(Instruction::LocalGet(end_chunk));
        instrs.push(Instruction::I64LeU);
        instrs.push(Instruction::Select);
        instrs.push(Instruction::LocalSet(chunk));

        let dirty_word = locals.alloc(1);
        instrs.push(Instruction::LocalGet(chunk));
        instrs.push(Instruction::I64Const(6));
        instrs.push(Instruction::I64ShrU);
        instrs.push(Instruction::LocalSet(dirty_word));
        let dirty_addr = locals.alloc(1);
        instrs.push(Instruction::I64Const(sparse.dirty_words_offset as i64));
        instrs.push(Instruction::LocalGet(dirty_word));
        instrs.push(Instruction::I64Const(8));
        instrs.push(Instruction::I64Mul);
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalTee(dirty_addr));
        instrs.push(Instruction::I32WrapI64);
        instrs.push(Instruction::I64Load(memarg));
        let dirty_bits = locals.alloc(1);
        instrs.push(Instruction::LocalSet(dirty_bits));
        let dirty_mask = locals.alloc(1);
        instrs.push(Instruction::I64Const(1));
        instrs.push(Instruction::LocalGet(chunk));
        instrs.push(Instruction::I64Const(63));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::I64Shl);
        instrs.push(Instruction::LocalSet(dirty_mask));
        let was_dirty = locals.alloc(1);
        instrs.push(Instruction::LocalGet(dirty_bits));
        instrs.push(Instruction::LocalGet(dirty_mask));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::I64ExtendI32U);
        instrs.push(Instruction::LocalSet(was_dirty));

        // Copy each dirty chunk only once, bounded to its own plane.
        let byte_index = locals.alloc(1);
        instrs.push(Instruction::LocalGet(chunk));
        instrs.push(Instruction::I64Const(8));
        instrs.push(Instruction::I64Mul);
        instrs.push(Instruction::LocalSet(byte_index));
        instrs.push(Instruction::LocalGet(was_dirty));
        instrs.push(Instruction::I64Eqz);
        instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
        let last_len = byte_size - (sparse.chunk_count - 1) * 8;
        let planes = if four_state && layout.is_4states[&abs] {
            2
        } else {
            1
        };
        if last_len == 8 {
            for plane in 0..planes {
                compile_sparse_chunk_copy(
                    stable_base + plane * byte_size,
                    sparse_base + plane * byte_size,
                    byte_index,
                    8,
                    instrs,
                );
            }
        } else {
            instrs.push(Instruction::LocalGet(chunk));
            instrs.push(Instruction::I64Const((sparse.chunk_count - 1) as i64));
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            for plane in 0..planes {
                compile_sparse_chunk_copy(
                    stable_base + plane * byte_size,
                    sparse_base + plane * byte_size,
                    byte_index,
                    last_len,
                    instrs,
                );
            }
            instrs.push(Instruction::Else);
            for plane in 0..planes {
                compile_sparse_chunk_copy(
                    stable_base + plane * byte_size,
                    sparse_base + plane * byte_size,
                    byte_index,
                    8,
                    instrs,
                );
            }
            instrs.push(Instruction::End);
        }
        instrs.push(Instruction::End);

        instrs.push(Instruction::LocalGet(dirty_addr));
        instrs.push(Instruction::I32WrapI64);
        instrs.push(Instruction::LocalGet(dirty_bits));
        instrs.push(Instruction::LocalGet(dirty_mask));
        instrs.push(Instruction::I64Or);
        instrs.push(Instruction::I64Store(memarg));

        let summary_addr = locals.alloc(1);
        instrs.push(Instruction::I64Const(sparse.summary_words_offset as i64));
        instrs.push(Instruction::LocalGet(dirty_word));
        instrs.push(Instruction::I64Const(6));
        instrs.push(Instruction::I64ShrU);
        instrs.push(Instruction::I64Const(8));
        instrs.push(Instruction::I64Mul);
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalTee(summary_addr));
        instrs.push(Instruction::I32WrapI64);
        instrs.push(Instruction::I64Load(memarg));
        let summary_bits = locals.alloc(1);
        instrs.push(Instruction::LocalSet(summary_bits));
        instrs.push(Instruction::LocalGet(summary_addr));
        instrs.push(Instruction::I32WrapI64);
        instrs.push(Instruction::LocalGet(summary_bits));
        instrs.push(Instruction::I64Const(1));
        instrs.push(Instruction::LocalGet(dirty_word));
        instrs.push(Instruction::I64Const(63));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::I64Shl);
        instrs.push(Instruction::I64Or);
        instrs.push(Instruction::I64Store(memarg));
    }
}

pub(super) fn compile_store(
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    op_width: usize,
    src: &RegisterId,
    triggers: &[TriggerIdWithKind],
    comb_capture_sites: &[u32],
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    if let Some(pieces) = strided_pieces(layout, &addr.absolute_addr(), offset, op_width) {
        for (start, width) in pieces {
            compile_store(
                addr,
                &SIROffset::Static(start),
                width,
                src,
                triggers,
                comb_capture_sites,
                layout,
                four_state,
                emit_triggers,
                locals,
                instrs,
            );
        }
        return;
    }
    let physical = physical_offset(layout, &addr.absolute_addr(), offset);
    let offset = &physical;
    // width=0: identity Store optimized away by alias; emit triggers only.
    if op_width == 0 {
        if emit_triggers && !triggers.is_empty() {
            emit_trigger_detection(addr, triggers, layout, locals, instrs);
        }
        return;
    }
    if addr.region == SPARSE_WORKING_REGION {
        compile_prepare_sparse_store(addr, offset, op_width, layout, four_state, locals, instrs);
    }
    let s = locals.reg_map[src].clone();
    let abs = addr.absolute_addr();
    let base_offset = compute_byte_offset(layout, &abs, addr.region);
    let var_width = layout.widths[&abs];
    let var_byte_size = layout.plane_size(&abs);
    let old_value_probe = if comb_capture_sites.is_empty() {
        None
    } else {
        capture_store_old_value(offset, op_width, base_offset, locals, instrs)
    };
    let old_mask_local = if comb_capture_sites.is_empty()
        || !four_state
        || !layout.is_4states.get(&abs).copied().unwrap_or(false)
    {
        None
    } else {
        capture_store_old_value(
            offset,
            op_width,
            base_offset + var_byte_size,
            locals,
            instrs,
        )
    };

    match offset {
        SIROffset::Static(bit_off)
        | SIROffset::PackedElements {
            bit_offset: bit_off,
            ..
        } => {
            let byte_off = bit_off / 8;
            let bit_shift = bit_off % 8;
            let store_offset = base_offset + byte_off;
            let effective_width = if !layout.unpacked_arrays.contains_key(&abs)
                && *bit_off == 0
                && bit_shift == 0
                && op_width < var_width
                && s.num_chunks > num_i64_chunks(op_width)
            {
                var_width
            } else {
                op_width
            };

            compile_store_at_offset(&s, store_offset, bit_shift, effective_width, locals, instrs);

            // 4-state mask store
            if four_state {
                let is_4state = layout.is_4states.get(&abs).copied().unwrap_or(false);
                if is_4state {
                    if let Some(mask_idx) = s.mask_idx {
                        let mask_local = RegLocal {
                            value_idx: mask_idx,
                            num_chunks: s.num_chunks,
                            mask_idx: None,
                        };
                        let mask_store_offset = base_offset + var_byte_size + byte_off;
                        compile_store_at_offset(
                            &mask_local,
                            mask_store_offset,
                            bit_shift,
                            effective_width,
                            locals,
                            instrs,
                        );
                    } else {
                        // Source is 2-state, clear mask
                        let mask_store_offset = base_offset + var_byte_size + byte_off;
                        let zero = locals.alloc(1);
                        instrs.push(Instruction::I64Const(0));
                        instrs.push(Instruction::LocalSet(zero));
                        let zero = RegLocal {
                            value_idx: zero,
                            num_chunks: 1,
                            mask_idx: None,
                        };
                        compile_store_at_offset(
                            &zero,
                            mask_store_offset,
                            bit_shift,
                            effective_width,
                            locals,
                            instrs,
                        );
                    }
                }
            }
        }
        SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
            let offset_reg_value_idx = compile_logical_bit_offset(offset, locals, instrs);
            compile_store_dynamic(
                &s,
                base_offset,
                offset_reg_value_idx,
                op_width,
                locals,
                instrs,
            );

            if four_state {
                let is_4state = layout.is_4states.get(&abs).copied().unwrap_or(false);
                if is_4state {
                    if let Some(mask_idx) = s.mask_idx {
                        let mask_local = RegLocal {
                            value_idx: mask_idx,
                            num_chunks: s.num_chunks,
                            mask_idx: None,
                        };
                        let mask_base = base_offset + var_byte_size;
                        compile_store_dynamic(
                            &mask_local,
                            mask_base,
                            offset_reg_value_idx,
                            op_width,
                            locals,
                            instrs,
                        );
                    } else {
                        let zero_idx = locals.alloc(s.num_chunks);
                        for chunk in 0..s.num_chunks {
                            instrs.push(Instruction::I64Const(0));
                            instrs.push(Instruction::LocalSet(zero_idx + chunk as u32));
                        }
                        let zero = RegLocal {
                            value_idx: zero_idx,
                            num_chunks: s.num_chunks,
                            mask_idx: None,
                        };
                        compile_store_dynamic(
                            &zero,
                            base_offset + var_byte_size,
                            offset_reg_value_idx,
                            op_width,
                            locals,
                            instrs,
                        );
                    }
                }
            }
        }
    }

    if !comb_capture_sites.is_empty() {
        emit_store_comb_capture_enable(
            old_value_probe,
            old_mask_local,
            offset,
            op_width,
            base_offset,
            var_byte_size,
            four_state && layout.is_4states.get(&abs).copied().unwrap_or(false),
            comb_capture_sites,
            locals,
            instrs,
        );
    }

    // Trigger detection
    if emit_triggers && !triggers.is_empty() {
        emit_trigger_detection(addr, triggers, layout, locals, instrs);
    }
}

pub(super) fn capture_store_old_value(
    offset: &SIROffset,
    op_width: usize,
    base_offset: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) -> Option<RegLocal> {
    if op_width == 0 {
        return None;
    }
    let num_chunks = num_i64_chunks(op_width);
    let value_idx = locals.alloc(num_chunks);
    let probe = RegLocal {
        value_idx,
        num_chunks,
        mask_idx: None,
    };
    emit_load_store_value(offset, op_width, base_offset, &probe, locals, instrs)?;
    Some(probe)
}

pub(super) fn emit_load_store_value(
    offset: &SIROffset,
    op_width: usize,
    base_offset: usize,
    dst: &RegLocal,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) -> Option<()> {
    match offset {
        SIROffset::Static(bit_off)
        | SIROffset::PackedElements {
            bit_offset: bit_off,
            ..
        } => {
            let byte_off = bit_off / 8;
            let bit_shift = bit_off % 8;
            if bit_shift != 0 && op_width > 64 {
                return None;
            }
            if op_width <= 64 {
                let byte_len = get_byte_size(op_width + bit_shift);
                if byte_len > 8 {
                    return None;
                }
                emit_load_small_word(base_offset + byte_off, byte_len, instrs);
                if bit_shift != 0 {
                    instrs.push(Instruction::I64Const(bit_shift as i64));
                    instrs.push(Instruction::I64ShrU);
                }
                if op_width < 64 {
                    instrs.push(Instruction::I64Const(((1u64 << op_width) - 1) as i64));
                    instrs.push(Instruction::I64And);
                }
                instrs.push(Instruction::LocalSet(dst.value_idx));
            } else {
                let store_bytes = get_byte_size(op_width);
                for c in 0..dst.num_chunks {
                    let remaining = store_bytes.saturating_sub(c * 8);
                    let byte_len = remaining.min(8);
                    emit_load_small_word(base_offset + byte_off + c * 8, byte_len, instrs);
                    instrs.push(Instruction::LocalSet(dst.value_idx + c as u32));
                }
            }
        }
        SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
            let dyn_bit_offset_local = compile_logical_bit_offset(offset, locals, instrs);
            if op_width <= 64 {
                compile_load_dynamic(dst, base_offset, dyn_bit_offset_local, op_width, instrs);
            } else {
                let store_bytes = get_byte_size(op_width);
                for c in 0..dst.num_chunks {
                    let remaining = store_bytes.saturating_sub(c * 8);
                    let byte_len = remaining.min(8);
                    instrs.push(Instruction::I64Const(base_offset as i64));
                    instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
                    instrs.push(Instruction::I64Const(3));
                    instrs.push(Instruction::I64ShrU);
                    instrs.push(Instruction::I64Add);
                    if c > 0 {
                        instrs.push(Instruction::I64Const((c * 8) as i64));
                        instrs.push(Instruction::I64Add);
                    }
                    let addr_local = dst.value_idx + c as u32;
                    instrs.push(Instruction::LocalSet(addr_local));
                    emit_load_small_word_dynamic(addr_local, byte_len, instrs);
                    instrs.push(Instruction::LocalSet(addr_local));
                }
            }
        }
    }
    Some(())
}

pub(super) fn emit_store_comb_capture_enable(
    old_value_probe: Option<RegLocal>,
    old_mask_probe: Option<RegLocal>,
    offset: &SIROffset,
    op_width: usize,
    base_offset: usize,
    var_byte_size: usize,
    is_4state: bool,
    sites: &[u32],
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let Some(old_value_probe) = old_value_probe else {
        return;
    };
    let changed = locals.alloc(1);
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::LocalSet(changed));

    let new_value_probe = RegLocal {
        value_idx: locals.alloc(old_value_probe.num_chunks),
        num_chunks: old_value_probe.num_chunks,
        mask_idx: None,
    };
    if emit_load_store_value(
        offset,
        op_width,
        base_offset,
        &new_value_probe,
        locals,
        instrs,
    )
    .is_none()
    {
        return;
    }
    for c in 0..old_value_probe.num_chunks {
        instrs.push(Instruction::LocalGet(changed));
        instrs.push(Instruction::LocalGet(old_value_probe.value_idx + c as u32));
        instrs.push(Instruction::LocalGet(new_value_probe.value_idx + c as u32));
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::I64ExtendI32U);
        instrs.push(Instruction::I64Or);
        instrs.push(Instruction::LocalSet(changed));
    }

    if is_4state {
        if let Some(old_mask_probe) = old_mask_probe {
            let new_mask_probe = RegLocal {
                value_idx: locals.alloc(old_mask_probe.num_chunks),
                num_chunks: old_mask_probe.num_chunks,
                mask_idx: None,
            };
            if emit_load_store_value(
                offset,
                op_width,
                base_offset + var_byte_size,
                &new_mask_probe,
                locals,
                instrs,
            )
            .is_some()
            {
                for c in 0..old_mask_probe.num_chunks {
                    instrs.push(Instruction::LocalGet(changed));
                    instrs.push(Instruction::LocalGet(old_mask_probe.value_idx + c as u32));
                    instrs.push(Instruction::LocalGet(new_mask_probe.value_idx + c as u32));
                    instrs.push(Instruction::I64Ne);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::I64Or);
                    instrs.push(Instruction::LocalSet(changed));
                }
            }
        }
    }

    emit_enable_comb_capture_sites(changed, sites, locals, instrs);
}

pub(super) fn compile_store_at_offset(
    src: &RegLocal,
    byte_offset: usize,
    bit_shift: usize,
    op_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    compile_store_at_offset_based(
        Base::Absolute,
        src,
        byte_offset,
        bit_shift,
        op_width,
        locals,
        instrs,
    )
}

pub(super) fn compile_store_at_offset_based(
    base: Base,
    src: &RegLocal,
    byte_offset: usize,
    bit_shift: usize,
    op_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let store_bytes = get_byte_size(op_width);
    let num_chunks = num_i64_chunks(op_width);

    if num_chunks == 1
        && bit_shift + op_width <= 64
        && (bit_shift != 0 || !op_width.is_multiple_of(8))
    {
        emit_partial_store_small_based(base, src, byte_offset, bit_shift, op_width, locals, instrs);
    } else if bit_shift == 0 {
        // Byte-aligned store. Break remaining bytes into power-of-2
        // sized stores (8/4/2/1) so every byte of the value is written.
        for c in 0..num_chunks {
            let remaining_bytes = (store_bytes - c * 8).min(8);
            let src_local = (c < src.num_chunks).then_some(src.value_idx + c as u32);
            let chunk_width = (op_width - c * 64).min(64);
            if !chunk_width.is_multiple_of(8) {
                // The final byte shares storage with the next packed field.
                // Preserve its unused high bits, even for a multiword source.
                let value_idx = src_local.unwrap_or_else(|| {
                    let zero = locals.alloc(1);
                    instrs.push(Instruction::I64Const(0));
                    instrs.push(Instruction::LocalSet(zero));
                    zero
                });
                let tail = RegLocal {
                    value_idx,
                    num_chunks: 1,
                    mask_idx: None,
                };
                emit_partial_store_small_based(
                    base,
                    &tail,
                    byte_offset + c * 8,
                    0,
                    chunk_width,
                    locals,
                    instrs,
                );
                continue;
            }
            let mut written = 0usize;
            while written < remaining_bytes {
                let left = remaining_bytes - written;
                let chunk_off = byte_offset + c * 8 + written;
                let memarg = wasm_encoder::MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                };
                emit_address(base, chunk_off, instrs);
                if written == 0 {
                    if let Some(src_local) = src_local {
                        instrs.push(Instruction::LocalGet(src_local));
                    } else {
                        instrs.push(Instruction::I64Const(0));
                    }
                } else {
                    if let Some(src_local) = src_local {
                        instrs.push(Instruction::LocalGet(src_local));
                        instrs.push(Instruction::I64Const((written * 8) as i64));
                        instrs.push(Instruction::I64ShrU);
                    } else {
                        instrs.push(Instruction::I64Const(0));
                    }
                }
                if left >= 8 {
                    instrs.push(Instruction::I64Store(memarg));
                    written += 8;
                } else if left >= 4 {
                    instrs.push(Instruction::I64Store32(memarg));
                    written += 4;
                } else if left >= 2 {
                    instrs.push(Instruction::I64Store16(memarg));
                    written += 2;
                } else {
                    instrs.push(Instruction::I64Store8(memarg));
                    written += 1;
                }
            }
        }
    } else {
        // Unaligned wide store.  Build each affected destination word from
        // the adjacent source chunks, then preserve every memory bit outside
        // the exact logical store range.  Starting the words at byte_offset
        // is intentional: WebAssembly permits unaligned i64 accesses, and it
        // keeps bit_shift bounded to the sub-byte part of the SIR offset.
        let affected_bits = bit_shift + op_width;
        let affected_bytes = affected_bits.div_ceil(8);
        let destination_chunks = affected_bits.div_ceil(64);
        for chunk in 0..destination_chunks {
            let chunk_bit = chunk * 64;
            let field_start = bit_shift.saturating_sub(chunk_bit);
            let field_end = affected_bits.saturating_sub(chunk_bit).min(64);
            if field_start >= field_end {
                continue;
            }
            let field_width = field_end - field_start;
            let field_mask = if field_width == 64 {
                u64::MAX
            } else {
                ((1u64 << field_width) - 1) << field_start
            };

            let assembled = locals.alloc(1);
            if chunk < src.num_chunks {
                instrs.push(Instruction::LocalGet(src.value_idx + chunk as u32));
                instrs.push(Instruction::I64Const(bit_shift as i64));
                instrs.push(Instruction::I64Shl);
            } else {
                instrs.push(Instruction::I64Const(0));
            }
            if chunk > 0 && chunk - 1 < src.num_chunks {
                instrs.push(Instruction::LocalGet(src.value_idx + (chunk - 1) as u32));
                instrs.push(Instruction::I64Const((64 - bit_shift) as i64));
                instrs.push(Instruction::I64ShrU);
                instrs.push(Instruction::I64Or);
            }
            instrs.push(Instruction::LocalSet(assembled));

            let byte_len = affected_bytes.saturating_sub(chunk * 8).min(8);
            let merged = locals.alloc(1);
            emit_load_small_word_based(base, byte_offset + chunk * 8, byte_len, instrs);
            instrs.push(Instruction::I64Const(
                (mask_for_bytes(byte_len) & !field_mask) as i64,
            ));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalGet(assembled));
            instrs.push(Instruction::I64Const(field_mask as i64));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(merged));
            emit_store_small_word_based(base, byte_offset + chunk * 8, byte_len, merged, instrs);
        }
    }
}

pub(super) fn emit_partial_store_small_based(
    base: Base,
    src: &RegLocal,
    byte_offset: usize,
    bit_shift: usize,
    op_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let affected_bytes = (bit_shift + op_width).div_ceil(8);
    let tmp = locals.alloc(1);

    emit_load_small_word_based(base, byte_offset, affected_bytes, instrs);
    let clear_mask = !((((1u128 << op_width) - 1) << bit_shift) as u64);
    instrs.push(Instruction::I64Const(mask_for_bytes(affected_bytes) as i64));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::I64Const(clear_mask as i64));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::LocalGet(src.value_idx));
    if op_width < 64 {
        let src_mask = (1u64 << op_width) - 1;
        instrs.push(Instruction::I64Const(src_mask as i64));
        instrs.push(Instruction::I64And);
    }
    if bit_shift > 0 {
        instrs.push(Instruction::I64Const(bit_shift as i64));
        instrs.push(Instruction::I64Shl);
    }
    instrs.push(Instruction::I64Or);
    instrs.push(Instruction::LocalSet(tmp));
    emit_store_small_word_based(base, byte_offset, affected_bytes, tmp, instrs);
}

pub(super) fn compile_store_dynamic(
    src: &RegLocal,
    base_offset: usize,
    dyn_bit_offset_local: u32,
    op_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    // Dynamic store with sub-byte bit offset support (single chunk).
    //
    // byte_offset = base_offset + (dyn_bits >> 3)
    // bit_shift = dyn_bits & 7
    //
    // For the single-chunk case with bit_shift:
    //   1. Load 8 bytes at byte_offset
    //   2. Clear bits [bit_shift..bit_shift+op_width]
    //   3. OR in (src << bit_shift)
    //   4. Store back
    //
    // For multi-chunk, we do chunk-by-chunk, handling the bit_shift for
    // the first chunk and carry between chunks.
    let num_chunks = num_i64_chunks(op_width);

    if num_chunks == 1 && op_width <= 64 {
        let addr_local = locals.alloc(1);
        let bit_shift_local = locals.alloc(1);
        let tmp_local = locals.alloc(1);
        let affected_bytes = (7 + op_width).div_ceil(8);
        let op_mask = if op_width >= 64 {
            u64::MAX
        } else {
            (1u64 << op_width) - 1
        };

        instrs.push(Instruction::I64Const(base_offset as i64));
        instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
        instrs.push(Instruction::I64Const(3));
        instrs.push(Instruction::I64ShrU);
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalSet(addr_local));

        instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
        instrs.push(Instruction::I64Const(7));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::LocalSet(bit_shift_local));

        emit_load_small_word_dynamic(addr_local, affected_bytes, instrs);
        instrs.push(Instruction::I64Const(mask_for_bytes(affected_bytes) as i64));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::I64Const(op_mask as i64));
        instrs.push(Instruction::LocalGet(bit_shift_local));
        instrs.push(Instruction::I64Shl);
        instrs.push(Instruction::I64Const(-1));
        instrs.push(Instruction::I64Xor);
        instrs.push(Instruction::I64And);

        instrs.push(Instruction::LocalGet(src.value_idx));
        if op_width < 64 {
            instrs.push(Instruction::I64Const(op_mask as i64));
            instrs.push(Instruction::I64And);
        }
        instrs.push(Instruction::LocalGet(bit_shift_local));
        instrs.push(Instruction::I64Shl);
        instrs.push(Instruction::I64Or);
        instrs.push(Instruction::LocalSet(tmp_local));

        emit_store_small_word_dynamic(addr_local, affected_bytes, tmp_local, instrs);
    } else {
        // Multi-chunk or wide single chunk: fall back to byte-aligned store.
        // This ignores sub-byte offset but handles the common case.
        for c in 0..num_chunks {
            // addr = base_offset + (dyn_bits >> 3) + c*8
            instrs.push(Instruction::I64Const(base_offset as i64));
            instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
            instrs.push(Instruction::I64Const(3));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Add);
            if c > 0 {
                instrs.push(Instruction::I64Const((c * 8) as i64));
                instrs.push(Instruction::I64Add);
            }
            instrs.push(Instruction::I32WrapI64);
            instrs.push(Instruction::LocalGet(src.value_idx + c as u32));
            instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
        }
    }
}

pub(super) fn emit_dynamic_addr_with_offset(
    addr_local: u32,
    byte_offset: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    instrs.push(Instruction::LocalGet(addr_local));
    instrs.push(Instruction::I32WrapI64);
    if byte_offset > 0 {
        instrs.push(Instruction::I32Const(byte_offset as i32));
        instrs.push(Instruction::I32Add);
    }
}

pub(super) fn emit_load_small_word_dynamic(
    addr_local: u32,
    byte_len: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    emit_load_small_word_dynamic_at(addr_local, 0, byte_len, instrs);
}

pub(super) fn emit_load_small_word_dynamic_at(
    addr_local: u32,
    start_off: usize,
    byte_len: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    match byte_len {
        0 => instrs.push(Instruction::I64Const(0)),
        1 => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
        }
        2 => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::I32Load16U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
        }
        3 => {
            emit_load_small_word_dynamic_at(addr_local, start_off, 2, instrs);
            emit_load_small_word_dynamic_at(addr_local, start_off + 2, 1, instrs);
            instrs.push(Instruction::I64Const(16));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        4 => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
        }
        5 => {
            emit_load_small_word_dynamic_at(addr_local, start_off, 4, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 4, instrs);
            instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        6 => {
            emit_load_small_word_dynamic_at(addr_local, start_off, 4, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 4, instrs);
            instrs.push(Instruction::I32Load16U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        7 => {
            emit_load_small_word_dynamic_at(addr_local, start_off, 6, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 6, instrs);
            instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::I64Const(48));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        _ => {
            emit_dynamic_addr_with_offset(addr_local, 0, instrs);
            instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
        }
    }
}

pub(super) fn emit_store_small_word_dynamic(
    addr_local: u32,
    byte_len: usize,
    value_local: u32,
    instrs: &mut Vec<Instruction<'static>>,
) {
    emit_store_small_word_dynamic_at(addr_local, 0, byte_len, value_local, instrs);
}

pub(super) fn emit_store_small_word_dynamic_at(
    addr_local: u32,
    start_off: usize,
    byte_len: usize,
    value_local: u32,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let memarg = wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    match byte_len {
        0 => {}
        1 => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store8(memarg));
        }
        2 => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store16(memarg));
        }
        3 => {
            emit_store_small_word_dynamic_at(addr_local, start_off, 2, value_local, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 2, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(16));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Store8(memarg));
        }
        4 => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store32(memarg));
        }
        5 => {
            emit_store_small_word_dynamic_at(addr_local, start_off, 4, value_local, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 4, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Store8(memarg));
        }
        6 => {
            emit_store_small_word_dynamic_at(addr_local, start_off, 4, value_local, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 4, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Store16(memarg));
        }
        7 => {
            emit_store_small_word_dynamic_at(addr_local, start_off, 6, value_local, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 6, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(48));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Store8(memarg));
        }
        _ => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store(memarg));
        }
    }
}

pub(super) fn compile_commit(
    src_addr: &RegionedAbsoluteAddr,
    dst_addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    op_width: usize,
    triggers: &[TriggerIdWithKind],
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    if let Some(pieces) = strided_pieces(layout, &src_addr.absolute_addr(), offset, op_width) {
        for (start, width) in pieces {
            compile_commit(
                src_addr,
                dst_addr,
                &SIROffset::Static(start),
                width,
                triggers,
                layout,
                four_state,
                emit_triggers,
                locals,
                instrs,
            );
        }
        return;
    }
    let src_abs = src_addr.absolute_addr();
    let dst_abs = dst_addr.absolute_addr();
    let src_base = compute_byte_offset(layout, &src_abs, src_addr.region);
    let dst_base = compute_byte_offset(layout, &dst_abs, dst_addr.region);
    let physical = physical_offset(layout, &src_abs, offset);
    let chunks = num_i64_chunks(op_width);
    let temp = RegLocal {
        value_idx: locals.alloc(chunks),
        num_chunks: chunks,
        mask_idx: None,
    };
    let dynamic = if physical.constant_bit_offset().is_none() {
        Some(compile_logical_bit_offset(&physical, locals, instrs))
    } else {
        None
    };
    let planes = if four_state && layout.is_4states[&dst_abs] {
        2
    } else {
        1
    };
    for plane in 0..planes {
        let src = src_base + plane * layout.plane_size(&src_abs);
        let dst = dst_base + plane * layout.plane_size(&dst_abs);
        if let Some(bits) = physical.constant_bit_offset() {
            compile_load_at_offset(&temp, src + bits / 8, bits % 8, op_width, instrs);
            compile_store_at_offset(&temp, dst + bits / 8, bits % 8, op_width, locals, instrs);
        } else {
            let bits = dynamic.unwrap();
            compile_load_dynamic(&temp, src, bits, op_width, instrs);
            compile_store_dynamic(&temp, dst, bits, op_width, locals, instrs);
        }
    }
    if emit_triggers && !triggers.is_empty() {
        emit_trigger_detection(dst_addr, triggers, layout, locals, instrs);
    }
}
