//! Load lowering: static and dynamic offsets, value and mask planes.

use super::*;

/// Lower `Load(dst, addr, offset, width_bits)`.
pub(super) fn lower_load(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    width_bits: &usize,
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
) {
    let vreg = ctx.reg_map.get(*dst);
    if try_emit_scalar_element_load(ctx, block, *dst, addr, offset, *width_bits) {
        return;
    }

    match offset {
        SIROffset::Static(bit_off)
        | SIROffset::PackedElements {
            bit_offset: bit_off,
            ..
        } => {
            let intra_byte = ctx.static_byte_and_intra(addr, *bit_off).1;
            let crosses_native_word = intra_byte + *width_bits > 64;
            if intra_byte != 0 && (*width_bits > 64 || crosses_native_word) {
                let value_base = ctx.byte_offset(addr, *bit_off);
                let chunks =
                    lower_static_wide_load_chunks(ctx, block, value_base, intra_byte, *width_bits);
                ctx.emit_alias_mov(block, vreg, chunks[0].0);
                if *width_bits > 64 {
                    ctx.set_wide_chunks(*dst, chunks);
                }

                if ctx.is_4state_var(addr) {
                    let mask_base = ctx.mask_byte_offset(addr, *bit_off);
                    let mask_chunks = lower_static_wide_load_chunks(
                        ctx,
                        block,
                        mask_base,
                        intra_byte,
                        *width_bits,
                    );
                    ctx.set_mask(*dst, mask_chunks[0].0);
                    if *width_bits > 64 {
                        ctx.wide_masks.insert(*dst, mask_chunks);
                    }
                } else if ctx.four_state {
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    ctx.set_mask(*dst, zero);
                }
                return;
            }
            // Wide load (>64 bits): chunk-by-chunk
            if *width_bits > 64 {
                lower_static_wide_load(ctx, block, dst, vreg, addr, bit_off, width_bits);
                return;
            }

            lower_static_narrow_load(ctx, block, vreg, addr, bit_off, width_bits, intra_byte);
        }
        SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
            let full_element_size = ctx.full_element_access_size(addr, offset, *width_bits);
            let direct_byte_off = (*width_bits <= 64)
                .then(|| {
                    recomposed_element_byte_offset(ctx, block, addr, offset, sir_block, sir_defs)
                        .or_else(|| direct_element_byte_offset(ctx, block, addr, offset))
                })
                .flatten();
            let offset_vreg = direct_byte_off
                .is_none()
                .then(|| memory_offset_vreg(ctx, block, addr, offset));
            let offset_low_zero_bits = if direct_byte_off.is_some() {
                3
            } else {
                memory_offset_low_zero_bits(ctx, addr, offset)
            };
            let base_off = ctx.byte_offset(addr, 0);
            let value_alias_range =
                MemoryAliasRange::new(base_off, ctx.layout.plane_size(&addr.absolute_addr()));
            // Compute byte offset and intra-byte bit shift
            let byte_off = if let Some(byte_off) = direct_byte_off {
                byte_off
            } else {
                let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: byte_off,
                    src: offset_vreg.expect("non-recomposed offset has a bit offset"),
                    imm: 3,
                });
                byte_off
            };
            let may_cross_native_word = *width_bits + 7 > 64 && offset_low_zero_bits < 3;
            if *width_bits > 64 || may_cross_native_word {
                let offset_vreg = offset_vreg.expect("wide dynamic load requires a bit offset");
                let chunks = lower_dynamic_wide_load_chunks(
                    ctx,
                    block,
                    base_off,
                    byte_off,
                    offset_vreg,
                    offset_low_zero_bits,
                    *width_bits,
                    value_alias_range,
                );
                if *width_bits > 64 {
                    ctx.set_wide_chunks(*dst, chunks);
                } else {
                    ctx.emit_alias_mov(block, vreg, chunks[0].0);
                }

                if ctx.is_4state_var(addr) {
                    let mask_base_off = ctx.mask_byte_offset(addr, 0);
                    let mask_alias_range = MemoryAliasRange::new(
                        mask_base_off,
                        ctx.layout.plane_size(&addr.absolute_addr()),
                    );
                    let mask_chunks = lower_dynamic_wide_load_chunks(
                        ctx,
                        block,
                        mask_base_off,
                        byte_off,
                        offset_vreg,
                        offset_low_zero_bits,
                        *width_bits,
                        mask_alias_range,
                    );
                    if let Some(&(mask0, _)) = mask_chunks.first() {
                        ctx.set_mask(*dst, mask0);
                    }
                    if *width_bits > 64 {
                        ctx.wide_masks.insert(*dst, mask_chunks);
                    }
                } else if ctx.four_state {
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    ctx.set_mask(*dst, zero);
                }
                return;
            }
            if offset_low_zero_bits >= 3 {
                let load_size = full_element_size
                    .unwrap_or_else(|| ISelContext::op_size_for_width(*width_bits));
                let padded_full_element = full_element_size
                    .is_some_and(|size| ISelContext::access_size_has_padding(size, *width_bits));
                let raw = if full_element_size.is_some() && !padded_full_element {
                    vreg
                } else {
                    ctx.alloc_vreg(SpillDesc::transient())
                };
                block.push(MInst::LoadIndexed {
                    dst: raw,
                    base: BaseReg::SimState,
                    offset: base_off,
                    index: byte_off,
                    scale: 1,
                    size: load_size,
                    alias_range: value_alias_range,
                });
                if padded_full_element {
                    ctx.emit_and_imm(block, vreg, raw, mask_for_width(*width_bits));
                } else if full_element_size.is_some() {
                    ctx.known_bits.insert(vreg, *width_bits);
                } else if *width_bits < 64 {
                    ctx.emit_and_imm(block, vreg, raw, mask_for_width(*width_bits));
                } else {
                    ctx.emit_mov(block, vreg, raw);
                }
            } else {
                let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(
                    block,
                    bit_shift,
                    offset_vreg.expect("unaligned dynamic load requires a bit offset"),
                    7,
                );

                // Load containing word at [sim + base_off + byte_off]
                // Use a slightly larger load to account for bit_shift
                let load_size = ISelContext::op_size_for_width(*width_bits + 7);
                let raw = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::LoadIndexed {
                    dst: raw,
                    base: BaseReg::SimState,
                    offset: base_off,
                    index: byte_off,
                    scale: 1,
                    size: load_size,
                    alias_range: value_alias_range,
                });

                // Shift right by bit_shift (dynamic)
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Shr {
                    dst: shifted,
                    lhs: raw,
                    rhs: bit_shift,
                });

                // Mask to width
                if *width_bits < 64 {
                    let mask = mask_for_width(*width_bits);
                    ctx.emit_and_imm(block, vreg, shifted, mask);
                } else {
                    ctx.emit_mov(block, vreg, shifted);
                }
            }
        }
    }

    // 4-state: load mask from memory (narrow path; wide handled above in early-return)
    lower_narrow_load_mask(ctx, block, dst, addr, offset, width_bits);
}

/// A static-offset load wider than 64 bits: value chunks, then mask chunks.
fn lower_static_wide_load(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    vreg: VReg,
    addr: &RegionedAbsoluteAddr,
    bit_off: &usize,
    width_bits: &usize,
) {
    let n_chunks = ISelContext::num_chunks(*width_bits);
    let mut chunks = Vec::with_capacity(n_chunks);
    let mut remaining = *width_bits;
    let mut bit_pos = *bit_off;
    for _ in 0..n_chunks {
        let chunk_bits = remaining.min(64);
        let chunk_byte_off = ctx.byte_offset(addr, bit_pos);
        let chunk_size = ISelContext::op_size_for_width(chunk_bits);
        let chunk_vreg = ctx.alloc_vreg(SpillDesc::sim_state(*addr, bit_pos, chunk_bits, false));
        let raw = if matches!(chunk_bits, 8 | 16 | 32 | 64) {
            chunk_vreg
        } else {
            ctx.alloc_vreg(SpillDesc::transient())
        };
        block.push(MInst::Load {
            dst: raw,
            base: BaseReg::SimState,
            offset: chunk_byte_off,
            size: chunk_size,
        });
        if raw != chunk_vreg {
            ctx.emit_and_imm(block, chunk_vreg, raw, mask_for_width(chunk_bits));
        }
        chunks.push((chunk_vreg, chunk_bits));
        bit_pos += chunk_bits;
        remaining -= chunk_bits;
    }
    // Also store chunk[0] in reg_map scalar slot for
    // fallback paths that read the scalar VReg.
    ctx.emit_alias_mov(block, vreg, chunks[0].0);
    ctx.set_wide_chunks(*dst, chunks);

    // 4-state: load wide mask chunks
    if ctx.is_4state_var(addr) {
        let mut mchunks = Vec::with_capacity(n_chunks);
        let mut m_remaining = *width_bits;
        let mut m_bit_pos = *bit_off;
        for _ in 0..n_chunks {
            let chunk_bits = m_remaining.min(64);
            let chunk_byte_off = ctx.mask_byte_offset(addr, m_bit_pos);
            let chunk_size = ISelContext::op_size_for_width(chunk_bits);
            let mv = ctx.alloc_vreg(SpillDesc::transient());
            let raw = if matches!(chunk_bits, 8 | 16 | 32 | 64) {
                mv
            } else {
                ctx.alloc_vreg(SpillDesc::transient())
            };
            block.push(MInst::Load {
                dst: raw,
                base: BaseReg::SimState,
                offset: chunk_byte_off,
                size: chunk_size,
            });
            if raw != mv {
                ctx.emit_and_imm(block, mv, raw, mask_for_width(chunk_bits));
            }
            mchunks.push((mv, chunk_bits));
            m_bit_pos += chunk_bits;
            m_remaining -= chunk_bits;
        }
        let mvreg = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_alias_mov(block, mvreg, mchunks[0].0);
        ctx.set_mask(*dst, mvreg);
        ctx.wide_masks.insert(*dst, mchunks);
    } else if ctx.four_state {
        let mvreg = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: mvreg,
            value: 0,
        });
        ctx.set_mask(*dst, mvreg);
    }
}

/// The value of a static-offset load of at most 64 bits.
fn lower_static_narrow_load(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    vreg: VReg,
    addr: &RegionedAbsoluteAddr,
    bit_off: &usize,
    width_bits: &usize,
    intra_byte: usize,
) {
    let byte_off = ctx.byte_offset(addr, *bit_off);
    let op_size = ISelContext::op_size_for_width(*width_bits);

    // Update spill desc
    ctx.spill_descs[vreg.0 as usize] = SpillDesc::sim_state(*addr, *bit_off, *width_bits, false);

    if !ctx.four_state
        && let Some(load_size) = ctx.full_static_load_size(addr, *bit_off, *width_bits)
    {
        // A padded slot is wider than its logical element.
        // Mask the raw load before claiming a known bit width.
        let padded_element = ctx
            .layout
            .unpacked_arrays
            .contains_key(&addr.absolute_addr())
            && ISelContext::access_size_has_padding(load_size, *width_bits);
        let raw = if padded_element {
            ctx.alloc_vreg(SpillDesc::transient())
        } else {
            vreg
        };
        block.push(MInst::Load {
            dst: raw,
            base: BaseReg::SimState,
            offset: byte_off,
            size: load_size,
        });
        if padded_element {
            ctx.emit_and_imm(block, vreg, raw, mask_for_width(*width_bits));
        }
        ctx.known_bits.insert(vreg, *width_bits);
    } else if intra_byte == 0 && OpSize::from_bits(*width_bits).is_some() {
        // Word-aligned, native size: single load.
        // If the load is wider than the variable (SIR optimizer widening),
        // mask the result to the variable's actual width.
        let var_width = ctx
            .layout
            .widths
            .get(&addr.absolute_addr())
            .copied()
            .unwrap_or(*width_bits);
        if var_width < *width_bits && var_width < 64 {
            if !ctx.four_state
                && OpSize::from_bits(var_width).is_some()
                && let Some(load_size) = ctx.full_static_load_size(addr, *bit_off, var_width)
            {
                // SIR may widen a whole-variable load even
                // though the physical variable still has a
                // native 8/16/32-bit representation. Loading
                // that representation already performs the
                // required zero extension; a wider load plus a
                // second truncating VReg only lengthens
                // allocation ranges and may read adjacent
                // packed state.
                ctx.spill_descs[vreg.0 as usize] =
                    SpillDesc::sim_state(*addr, *bit_off, var_width, false);
                block.push(MInst::Load {
                    dst: vreg,
                    base: BaseReg::SimState,
                    offset: byte_off,
                    size: load_size,
                });
                ctx.known_bits.insert(vreg, var_width);
            } else {
                let raw = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Load {
                    dst: raw,
                    base: BaseReg::SimState,
                    offset: byte_off,
                    size: op_size,
                });
                ctx.emit_and_imm(block, vreg, raw, mask_for_width(var_width));
            }
        } else {
            block.push(MInst::Load {
                dst: vreg,
                base: BaseReg::SimState,
                offset: byte_off,
                size: op_size,
            });
        }
    } else {
        // Unaligned or non-native width: load containing word + shift + mask
        let containing_byte_off = ctx.byte_offset(addr, *bit_off);
        let load_size = ISelContext::op_size_for_width(*width_bits + intra_byte);

        let tmp = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Load {
            dst: tmp,
            base: BaseReg::SimState,
            offset: containing_byte_off,
            size: load_size,
        });

        if intra_byte > 0 {
            let shifted = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted,
                src: tmp,
                imm: intra_byte as u8,
            });
            let mask = mask_for_width(*width_bits);
            ctx.emit_and_imm(block, vreg, shifted, mask);
        } else {
            // Byte-aligned but non-native width: just mask
            let mask = mask_for_width(*width_bits);
            ctx.emit_and_imm(block, vreg, tmp, mask);
        }
    }
}

/// The mask of a load whose value went through the narrow path.
fn lower_narrow_load_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    width_bits: &usize,
) {
    if ctx.is_4state_var(addr) {
        match offset {
            SIROffset::Static(bit_off)
            | SIROffset::PackedElements {
                bit_offset: bit_off,
                ..
            } => {
                if *width_bits <= 64 {
                    let mask_off = ctx.mask_byte_offset(addr, *bit_off);
                    let intra_byte = ctx.static_byte_and_intra(addr, *bit_off).1;
                    let op_size = ISelContext::op_size_for_width(*width_bits);
                    let mvreg = ctx.alloc_vreg(SpillDesc::transient());
                    let var_width = ctx
                        .layout
                        .widths
                        .get(&addr.absolute_addr())
                        .copied()
                        .unwrap_or(*width_bits);

                    if intra_byte == 0 && OpSize::from_bits(*width_bits).is_some() {
                        // Mask to actual variable width if SIR optimizer widened the load
                        if var_width < *width_bits && var_width < 64 {
                            let raw = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: raw,
                                base: BaseReg::SimState,
                                offset: mask_off,
                                size: op_size,
                            });
                            ctx.emit_and_imm(block, mvreg, raw, mask_for_width(var_width));
                        } else {
                            block.push(MInst::Load {
                                dst: mvreg,
                                base: BaseReg::SimState,
                                offset: mask_off,
                                size: op_size,
                            });
                        }
                    } else {
                        let containing_off = ctx.mask_byte_offset(addr, *bit_off);
                        let load_size = ISelContext::op_size_for_width(*width_bits + intra_byte);
                        let tmp = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Load {
                            dst: tmp,
                            base: BaseReg::SimState,
                            offset: containing_off,
                            size: load_size,
                        });
                        if intra_byte > 0 {
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: shifted,
                                src: tmp,
                                imm: intra_byte as u8,
                            });
                            ctx.emit_and_imm(block, mvreg, shifted, mask_for_width(*width_bits));
                        } else {
                            ctx.emit_and_imm(block, mvreg, tmp, mask_for_width(*width_bits));
                        }
                    }
                    ctx.set_mask(*dst, mvreg);
                }
            }
            SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                // Dynamic: load mask similarly with indexed addressing
                let full_element_size = ctx.full_element_access_size(addr, offset, *width_bits);
                let offset_vreg = memory_offset_vreg(ctx, block, addr, offset);
                let mask_base_off = ctx.mask_byte_offset(addr, 0);
                let mask_alias_range = MemoryAliasRange::new(
                    mask_base_off,
                    ctx.layout.plane_size(&addr.absolute_addr()),
                );
                let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: byte_off,
                    src: offset_vreg,
                    imm: 3,
                });
                if memory_offset_low_zero_bits(ctx, addr, offset) >= 3 {
                    let load_size = full_element_size
                        .unwrap_or_else(|| ISelContext::op_size_for_width(*width_bits));
                    let mvreg = ctx.alloc_vreg(SpillDesc::transient());
                    let padded_full_element = full_element_size.is_some_and(|size| {
                        ISelContext::access_size_has_padding(size, *width_bits)
                    });
                    let raw = if full_element_size.is_some() && !padded_full_element {
                        mvreg
                    } else {
                        ctx.alloc_vreg(SpillDesc::transient())
                    };
                    block.push(MInst::LoadIndexed {
                        dst: raw,
                        base: BaseReg::SimState,
                        offset: mask_base_off,
                        index: byte_off,
                        scale: 1,
                        size: load_size,
                        alias_range: mask_alias_range,
                    });
                    if padded_full_element {
                        ctx.emit_and_imm(block, mvreg, raw, mask_for_width(*width_bits));
                    } else if full_element_size.is_some() {
                        ctx.known_bits.insert(mvreg, *width_bits);
                    } else if *width_bits < 64 {
                        ctx.emit_and_imm(block, mvreg, raw, mask_for_width(*width_bits));
                    } else {
                        ctx.emit_mov(block, mvreg, raw);
                    }
                    ctx.set_mask(*dst, mvreg);
                    return;
                }
                let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, bit_shift, offset_vreg, 7);
                let load_size = ISelContext::op_size_for_width(*width_bits + 7);
                let raw = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::LoadIndexed {
                    dst: raw,
                    base: BaseReg::SimState,
                    offset: mask_base_off,
                    index: byte_off,
                    scale: 1,
                    size: load_size,
                    alias_range: mask_alias_range,
                });
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Shr {
                    dst: shifted,
                    lhs: raw,
                    rhs: bit_shift,
                });
                let mvreg = ctx.alloc_vreg(SpillDesc::transient());
                if *width_bits < 64 {
                    ctx.emit_and_imm(block, mvreg, shifted, mask_for_width(*width_bits));
                } else {
                    ctx.emit_mov(block, mvreg, shifted);
                }
                ctx.set_mask(*dst, mvreg);
            }
        }
    } else if ctx.four_state {
        // Non-4-state variable in 4-state mode: mask is always 0
        let mvreg = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: mvreg,
            value: 0,
        });
        ctx.set_mask(*dst, mvreg);
    }
}
