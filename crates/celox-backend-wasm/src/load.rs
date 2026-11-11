//! Loads from state memory.

use super::*;

// ============================================================
// Memory operations
// ============================================================

/// Convert a logical array access to physical bits. Layout selection keeps
/// ordinary accesses within one element; aggregate zeroing/commits are split
/// before calling this helper.
pub(super) fn physical_offset(
    layout: &MemoryLayout,
    address: &AbsoluteAddr,
    offset: &SIROffset,
) -> SIROffset {
    let Some(array) = layout.unpacked_arrays.get(address) else {
        return offset.clone();
    };
    match offset {
        SIROffset::Static(start)
        | SIROffset::PackedElements {
            bit_offset: start, ..
        } => {
            let (byte, bit) = layout.map_static_bit_offset(address, *start);
            SIROffset::Static(byte * 8 + bit)
        }
        SIROffset::Element {
            index,
            bit_offset,
            dynamic_bit_offset,
            ..
        } => SIROffset::Element {
            index: *index,
            element_width: array.element_stride * 8,
            bit_offset: *bit_offset,
            dynamic_bit_offset: *dynamic_bit_offset,
        },
        SIROffset::Dynamic(_) => unreachable!("dynamic packed access to strided array"),
    }
}

pub(super) fn strided_pieces(
    layout: &MemoryLayout,
    address: &AbsoluteAddr,
    offset: &SIROffset,
    width: usize,
) -> Option<Vec<(usize, usize)>> {
    let array = layout.unpacked_arrays.get(address)?;
    let start = offset.constant_bit_offset()?;
    if width == 0
        || array.element_stride * 8 == array.element_width
        || width <= array.element_width - start % array.element_width
    {
        return None;
    }
    let mut pieces = Vec::new();
    let mut copied = 0;
    while copied < width {
        let position = start + copied;
        let count = (array.element_width - position % array.element_width).min(width - copied);
        pieces.push((position, count));
        copied += count;
    }
    Some(pieces)
}

pub(super) fn compile_logical_bit_offset(
    offset: &SIROffset,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) -> u32 {
    let result = locals.alloc(1);
    match offset {
        SIROffset::Static(value)
        | SIROffset::PackedElements {
            bit_offset: value, ..
        } => instrs.push(Instruction::I64Const(*value as i64)),
        SIROffset::Dynamic(reg) => {
            instrs.push(Instruction::LocalGet(locals.reg_map[reg].value_idx));
        }
        SIROffset::Element {
            index,
            element_width,
            bit_offset,
            dynamic_bit_offset,
        } => {
            instrs.push(Instruction::LocalGet(locals.reg_map[index].value_idx));
            if *element_width != 1 {
                instrs.push(Instruction::I64Const(*element_width as i64));
                instrs.push(Instruction::I64Mul);
            }
            if *bit_offset != 0 {
                instrs.push(Instruction::I64Const(*bit_offset as i64));
                instrs.push(Instruction::I64Add);
            }
            if let Some(dynamic) = dynamic_bit_offset {
                instrs.push(Instruction::LocalGet(locals.reg_map[dynamic].value_idx));
                instrs.push(Instruction::I64Add);
            }
        }
    }
    instrs.push(Instruction::LocalSet(result));
    result
}

pub(super) fn compile_load(
    dst: &RegisterId,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    op_width: usize,
    layout: &MemoryLayout,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let physical = physical_offset(layout, &addr.absolute_addr(), offset);
    let offset = &physical;
    let d = locals.reg_map[dst].clone();
    let abs = addr.absolute_addr();
    let base_offset = compute_byte_offset(layout, &abs, addr.region);
    let var_byte_size = layout.plane_size(&abs);

    match offset {
        SIROffset::Static(bit_off)
        | SIROffset::PackedElements {
            bit_offset: bit_off,
            ..
        } => {
            let byte_off = bit_off / 8;
            let bit_shift = bit_off % 8;
            let load_offset = base_offset + byte_off;

            compile_load_at_offset(&d, load_offset, bit_shift, op_width, instrs);

            // 4-state: load mask
            if four_state {
                if let Some(mask_idx) = d.mask_idx {
                    let is_4state = layout.is_4states.get(&abs).copied().unwrap_or(false);
                    if is_4state {
                        let mask_base = base_offset + var_byte_size;
                        let mask_load_offset = mask_base + byte_off;
                        let mask_local = RegLocal {
                            value_idx: mask_idx,
                            num_chunks: d.num_chunks,
                            mask_idx: None,
                        };
                        compile_load_at_offset(
                            &mask_local,
                            mask_load_offset,
                            bit_shift,
                            op_width,
                            instrs,
                        );
                    } else {
                        // Not a 4-state var: mask is 0
                        for c in 0..d.num_chunks {
                            instrs.push(Instruction::I64Const(0));
                            instrs.push(Instruction::LocalSet(mask_idx + c as u32));
                        }
                    }
                }
            }
        }
        SIROffset::Dynamic(reg) => {
            let offset_reg = &locals.reg_map[reg];
            // Dynamic bit offset is in offset_reg.value_idx (i64).
            // byte_offset = base_offset + (dynamic_bits / 8)
            // bit_shift = dynamic_bits % 8
            compile_load_dynamic(&d, base_offset, offset_reg.value_idx, op_width, instrs);

            if four_state {
                if let Some(mask_idx) = d.mask_idx {
                    let is_4state = layout.is_4states.get(&abs).copied().unwrap_or(false);
                    if is_4state {
                        let mask_base = base_offset + var_byte_size;
                        let mask_local = RegLocal {
                            value_idx: mask_idx,
                            num_chunks: d.num_chunks,
                            mask_idx: None,
                        };
                        compile_load_dynamic(
                            &mask_local,
                            mask_base,
                            offset_reg.value_idx,
                            op_width,
                            instrs,
                        );
                    } else {
                        for c in 0..d.num_chunks {
                            instrs.push(Instruction::I64Const(0));
                            instrs.push(Instruction::LocalSet(mask_idx + c as u32));
                        }
                    }
                }
            }
        }
        SIROffset::Element { .. } => {
            let offset_local = compile_logical_bit_offset(offset, locals, instrs);
            compile_load_dynamic(&d, base_offset, offset_local, op_width, instrs);

            if four_state && let Some(mask_idx) = d.mask_idx {
                let is_4state = layout.is_4states.get(&abs).copied().unwrap_or(false);
                if is_4state {
                    let mask_local = RegLocal {
                        value_idx: mask_idx,
                        num_chunks: d.num_chunks,
                        mask_idx: None,
                    };
                    compile_load_dynamic(
                        &mask_local,
                        base_offset + var_byte_size,
                        offset_local,
                        op_width,
                        instrs,
                    );
                } else {
                    for c in 0..d.num_chunks {
                        instrs.push(Instruction::I64Const(0));
                        instrs.push(Instruction::LocalSet(mask_idx + c as u32));
                    }
                }
            }
        }
    }
}

pub(super) fn compile_load_at_offset(
    dst: &RegLocal,
    byte_offset: usize,
    bit_shift: usize,
    op_width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    compile_load_at_offset_based(
        Base::Absolute,
        dst,
        byte_offset,
        bit_shift,
        op_width,
        instrs,
    )
}

pub(super) fn compile_load_at_offset_based(
    base: Base,
    dst: &RegLocal,
    byte_offset: usize,
    bit_shift: usize,
    op_width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let num_chunks = num_i64_chunks(op_width);

    for c in 0..dst.num_chunks {
        if c < num_chunks {
            let chunk_byte_off = byte_offset + c * 8;
            emit_address(base, chunk_byte_off, instrs);
            instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                offset: 0,
                align: 0, // unaligned
                memory_index: 0,
            }));
            if bit_shift > 0 {
                instrs.push(Instruction::I64Const(bit_shift as i64));
                instrs.push(Instruction::I64ShrU);
                // For multi-chunk with bit_shift, we need to OR in bits from the next chunk.
                if c + 1 < num_chunks || (c * 64 + 64 - bit_shift) < op_width {
                    // Load next 8 bytes and shift left
                    let next_off = chunk_byte_off + 8;
                    emit_address(base, next_off, instrs);
                    instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    }));
                    instrs.push(Instruction::I64Const((64 - bit_shift) as i64));
                    instrs.push(Instruction::I64Shl);
                    instrs.push(Instruction::I64Or);
                }
            }
        } else {
            instrs.push(Instruction::I64Const(0));
        }
        instrs.push(Instruction::LocalSet(dst.value_idx + c as u32));
    }

    // Mask top chunk to op_width
    let top_bits = op_width % 64;
    if top_bits > 0 && top_bits < 64 {
        let top_chunk = num_chunks - 1;
        if top_chunk < dst.num_chunks {
            let mask = (1u64 << top_bits) - 1;
            instrs.push(Instruction::LocalGet(dst.value_idx + top_chunk as u32));
            instrs.push(Instruction::I64Const(mask as i64));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(dst.value_idx + top_chunk as u32));
        }
    }
}

pub(super) fn compile_load_dynamic(
    dst: &RegLocal,
    base_offset: usize,
    dyn_bit_offset_local: u32,
    op_width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    // byte_offset = base_offset + (dyn_bits >> 3)
    // bit_shift = dyn_bits & 7
    //
    // For each 64-bit chunk:
    //   Load 8 bytes at (byte_offset + c*8), shift right by bit_shift.
    //   If bit_shift > 0 and we need bits that spilled into the next 8-byte word,
    //   load the next 8 bytes, shift left by (64 - bit_shift), OR into the result.
    let num_chunks = num_i64_chunks(op_width);

    for c in 0..dst.num_chunks {
        if c < num_chunks {
            // Compute addr = base_offset + (dyn_bits >> 3) + c*8
            // Load 8 bytes at addr
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
            instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            // bit_shift = dyn_bits & 7
            instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
            instrs.push(Instruction::I64Const(7));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64ShrU);

            // If bit_shift > 0 and op_width > 57, bits may cross a 64-bit boundary.
            // For the top chunk, we handle this by loading the next 8 bytes and
            // ORing in the spilled high bits: next_word << (64 - bit_shift).
            // This is always safe for single-chunk values and for the last chunk
            // of multi-chunk values.
            if c == num_chunks - 1 {
                // Load next 8 bytes at addr + 8
                instrs.push(Instruction::I64Const(base_offset as i64));
                instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
                instrs.push(Instruction::I64Const(3));
                instrs.push(Instruction::I64ShrU);
                instrs.push(Instruction::I64Add);
                instrs.push(Instruction::I64Const(((c + 1) * 8) as i64));
                instrs.push(Instruction::I64Add);
                instrs.push(Instruction::I32WrapI64);
                instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                }));
                // Shift left by (64 - bit_shift). If bit_shift == 0, shift by 64
                // which gives 0 in WASM (i64.shl masks shift to 0..63, 64 & 63 = 0).
                // So when bit_shift = 0, next_word << 64 = next_word << 0 = next_word,
                // which is wrong. We need to handle this: if bit_shift == 0, result is 0.
                // Use: (64 - bit_shift) & 63 as shift amount, then mask result with
                // (bit_shift != 0) check.
                // Alternatively: shift left by (64 - bit_shift), then AND with
                // a mask that's 0 when bit_shift == 0.
                //
                // Simplest approach: compute complement_shift = (-bit_shift) & 63 = (64 - bit_shift) & 63.
                // When bit_shift=0: complement_shift=0, so next_word << 0 = next_word.
                // We'd OR in a nonzero value incorrectly. Fix by multiplying with (bit_shift != 0):
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
                instrs.push(Instruction::I64Const(7));
                instrs.push(Instruction::I64And);
                // Stack: [next_word, 0, bit_shift]
                instrs.push(Instruction::I64Sub);
                // Stack: [next_word, -bit_shift]  (i.e., 64 - bit_shift when taken mod 64)
                instrs.push(Instruction::I64Shl);
                // Stack: [next_word << ((64 - bit_shift) & 63)]
                // When bit_shift = 0: this is next_word << 0 = next_word. We need to zero it.
                // Multiply by (bit_shift != 0):
                instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
                instrs.push(Instruction::I64Const(7));
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::I64Ne);
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::I64Mul);
                // Now OR into the shifted low word
                instrs.push(Instruction::I64Or);
            }
        } else {
            instrs.push(Instruction::I64Const(0));
        }
        instrs.push(Instruction::LocalSet(dst.value_idx + c as u32));
    }

    // Mask to width
    let top_bits = op_width % 64;
    if top_bits > 0 && top_bits < 64 {
        let top = num_chunks - 1;
        if top < dst.num_chunks {
            let mask = (1u64 << top_bits) - 1;
            instrs.push(Instruction::LocalGet(dst.value_idx + top as u32));
            instrs.push(Instruction::I64Const(mask as i64));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(dst.value_idx + top as u32));
        }
    }
}
