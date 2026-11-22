//! Store lowering: static and dynamic offsets, value and mask planes,
//! comb-capture probes and trigger detection.

use super::*;

/// Lower `Store(addr, offset, width_bits, src_reg, triggers, comb_capture_sites)`.
pub(super) fn lower_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    width_bits: &usize,
    src_reg: &RegisterId,
    triggers: &[crate::TriggerIdWithKind],
    comb_capture_sites: &[u32],
    sparse_write_state: SparseWriteState,
    sparse_chunk_state: SparseChunkState,
    sparse_dirty_word_state: SparseChunkState,
    sparse_metadata_action: SparseMetadataAction,
) {
    if try_emit_single_chunk_sparse_store(
        ctx,
        block,
        addr,
        offset,
        *width_bits,
        *src_reg,
        triggers,
        comb_capture_sites,
        sparse_write_state,
    ) {
        return;
    }
    if addr.region == crate::SPARSE_WORKING_REGION && *width_bits != 0 {
        prepare_sparse_store(
            ctx,
            block,
            addr,
            offset,
            *width_bits,
            sparse_write_state,
            sparse_chunk_state,
            sparse_dirty_word_state,
            sparse_metadata_action,
        );
    }
    // width=0: identity Store optimized away; only emit triggers.
    if *width_bits == 0 {
        lower_trigger_only_store(ctx, block, addr, offset, triggers);
        // Skip value store entirely
    } else {
        ctx.trigger_only_seen.clear();
        match offset {
            SIROffset::Static(bit_off)
            | SIROffset::PackedElements {
                bit_offset: bit_off,
                ..
            } => {
                let four_state = ctx.is_4state_var(addr);
                let mut probes = Vec::new();
                if !comb_capture_sites.is_empty() {
                    probes = collect_static_comb_store_byte_probes(
                        ctx,
                        block,
                        addr,
                        *bit_off,
                        *width_bits,
                        false,
                    );
                    if four_state {
                        probes.extend(collect_static_comb_store_byte_probes(
                            ctx,
                            block,
                            addr,
                            *bit_off,
                            *width_bits,
                            true,
                        ));
                    }
                }
                lower_static_value_store(ctx, block, addr, bit_off, width_bits, src_reg);
                if four_state {
                    let mask = ctx.get_mask(*src_reg, block);
                    lower_static_mask_store(ctx, block, addr, bit_off, width_bits, src_reg, mask);
                }
                emit_enable_comb_capture_sites_if_byte_probes_changed(
                    ctx,
                    block,
                    probes,
                    comb_capture_sites,
                );
            }
            SIROffset::Dynamic(_) | SIROffset::Element { .. } | SIROffset::ElementRun { .. } => {
                lower_dynamic_value_store(
                    ctx,
                    block,
                    addr,
                    offset,
                    width_bits,
                    src_reg,
                    comb_capture_sites,
                );
                if ctx.is_4state_var(addr) {
                    let mask = ctx.get_mask(*src_reg, block);
                    lower_dynamic_mask_store(
                        ctx,
                        block,
                        addr,
                        offset,
                        width_bits,
                        src_reg,
                        mask,
                        comb_capture_sites,
                    );
                }
            }
        }

        // Trigger detection: compare old vs new value, set triggered_bits
        lower_store_triggers(ctx, block, addr, offset, width_bits, triggers);
    } // else (width != 0)
}

/// A zero-width store: no value is written, only its triggers are raised.
fn lower_trigger_only_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    triggers: &[crate::TriggerIdWithKind],
) {
    if !triggers.is_empty() {
        if let SIROffset::Static(bit_off) = offset {
            // Load current value for trigger comparison
            // (self-copy alias: addr points to the canonical location)
            let byte_off = ctx.byte_offset(addr, *bit_off);
            let triggers = triggers
                .iter()
                .copied()
                .filter(|trigger| ctx.trigger_only_seen.insert((byte_off, trigger.id)))
                .collect::<Vec<_>>();
            if triggers.is_empty() {
                return;
            }
            // We need *some* value for trigger comparison.
            // Since width was originally 1 (clock/reset), load 1 byte.
            let new_val = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: new_val,
                base: BaseReg::SimState,
                offset: byte_off,
                size: OpSize::S8,
            });

            for trigger in &triggers {
                let trigger_byte_idx = trigger.id / 8;
                let trigger_bit_idx = trigger.id % 8;
                let trigger_offset = ctx.layout.triggered_bits_offset + trigger_byte_idx;

                let triggered = ctx.alloc_vreg(SpillDesc::transient());
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero,
                    value: 0,
                });
                block.push(MInst::Cmp {
                    dst: triggered,
                    lhs: new_val,
                    rhs: zero,
                    kind: CmpKind::Ne,
                });

                let old_byte = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Load {
                    dst: old_byte,
                    base: BaseReg::SimState,
                    offset: trigger_offset as i32,
                    size: OpSize::S8,
                });
                let bit_mask = ctx.alloc_vreg(SpillDesc::remat(1u64 << trigger_bit_idx));
                block.push(MInst::LoadImm {
                    dst: bit_mask,
                    value: 1u64 << trigger_bit_idx,
                });
                let selected_mask = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: selected_mask,
                    cond: triggered,
                    true_val: bit_mask,
                    false_val: zero,
                });
                let new_byte = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: new_byte,
                    lhs: old_byte,
                    rhs: selected_mask,
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: trigger_offset as i32,
                    src: new_byte,
                    size: OpSize::S8,
                });
            }
        }
    }
}

/// Write a byte-aligned scalar fragment without reading its destination. A
/// partial last byte is writable only at a private storage boundary. Keep
/// scalar padding zero: whole-object loads can rely on that invariant.
fn try_store_bytes(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    bit_offset: usize,
    width: usize,
    src: VReg,
    plane_offset: i32,
) -> bool {
    let (byte, intra) = ctx.static_byte_and_intra(addr, bit_offset);
    if intra != 0 {
        return false;
    }
    let whole = ctx.full_static_store_size(addr, bit_offset, width);
    let bytes = if let Some(size) = whole {
        size.bytes() as usize
    } else {
        if !width.is_multiple_of(8) {
            // Only an element-strided layout explicitly reserves a private
            // tail for each element. Packed slices keep the enclosing bits.
            let private_tail = ctx
                .layout
                .unpacked_arrays
                .get(&addr.absolute_addr())
                .is_some_and(|array| {
                    bit_offset % array.element_width + width == array.element_width
                });
            if !private_tail {
                return false;
            }
        }
        width.div_ceil(8)
    };
    let mut consumed = 0;
    while consumed < bytes {
        let size = match bytes - consumed {
            1 => OpSize::S8,
            2..=3 => OpSize::S16,
            4..=7 => OpSize::S32,
            _ => OpSize::S64,
        };
        let mut piece = src;
        if consumed != 0 {
            piece = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: piece,
                src,
                imm: (consumed * 8) as u8,
            });
        }
        let remaining = width - consumed * 8;
        if remaining < size.bytes() as usize * 8 {
            piece = ctx.mask_for_store_width(block, piece, remaining);
        }
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: byte + plane_offset + consumed as i32,
            src: piece,
            size,
        });
        consumed += size.bytes() as usize;
    }
    true
}

/// The value plane of a static-offset store.
fn lower_static_value_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    bit_off: &usize,
    width_bits: &usize,
    src_reg: &RegisterId,
) {
    // Check for wide value from Concat
    if *width_bits > 64 {
        if let Some(chunks) = ctx.wide_regs.get(src_reg).cloned() {
            // Wide store: emit chunk-by-chunk stores
            let mut bit_pos = 0usize;
            let mut store_remaining = *width_bits;
            for (chunk_vreg, chunk_width) in &chunks {
                if store_remaining == 0 {
                    break;
                }
                let logical_chunk_width = (*chunk_width).min(store_remaining);
                let mut consumed = 0usize;
                while consumed < logical_chunk_width {
                    let part_bit_off = *bit_off + bit_pos + consumed;
                    let intra = ctx.static_byte_and_intra(addr, part_bit_off).1;
                    let remaining = logical_chunk_width - consumed;
                    let part_width = remaining.min(64 - intra);
                    let part_byte_off = ctx.byte_offset(addr, part_bit_off);

                    let part_src = if consumed == 0 {
                        *chunk_vreg
                    } else {
                        let shifted = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: shifted,
                            src: *chunk_vreg,
                            imm: consumed as u8,
                        });
                        shifted
                    };

                    if !try_store_bytes(ctx, block, addr, part_bit_off, part_width, part_src, 0) {
                        // Non-aligned chunk part: RMW via BitFieldInsert.
                        let containing_off = part_byte_off;
                        let load_size = ISelContext::op_size_for_width(part_width + intra);
                        let old = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Load {
                            dst: old,
                            base: BaseReg::SimState,
                            offset: containing_off,
                            size: load_size,
                        });
                        let mask = mask_for_width(part_width);
                        let new = ctx.alloc_vreg(SpillDesc::transient());
                        ctx.emit_bfi(block, new, old, part_src, intra as u8, mask);
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: containing_off,
                            src: new,
                            size: load_size,
                        });
                    }
                    consumed += part_width;
                }
                bit_pos += logical_chunk_width;
                store_remaining -= logical_chunk_width;
            }
        } else {
            // Wide store without Concat source: chunk-by-chunk copy
            // This shouldn't happen in practice since wide stores
            // come from Concat, but handle it as raw memory copy.
            let mut remaining = *width_bits;
            let mut off = ctx.byte_offset(addr, *bit_off);
            while remaining > 0 {
                let chunk_bits = remaining.min(64);
                let chunk_size = ISelContext::op_size_for_width(chunk_bits);
                let tmp = ctx.alloc_vreg(SpillDesc::transient());
                // We can't split a single vreg. This is a fallback.
                block.push(MInst::LoadImm { dst: tmp, value: 0 });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: off,
                    src: tmp,
                    size: chunk_size,
                });
                let advance = chunk_bits.div_ceil(8);
                off += advance as i32;
                remaining -= chunk_bits;
            }
        }
        // Skip the rest of the static offset handling
    } else {
        let src_vreg = ctx.reg_map.get(*src_reg);
        if !try_store_bytes(ctx, block, addr, *bit_off, *width_bits, src_vreg, 0) {
            let mut consumed = 0usize;
            while consumed < *width_bits {
                let part_bit_off = *bit_off + consumed;
                let intra = ctx.static_byte_and_intra(addr, part_bit_off).1;
                let part_width = (*width_bits - consumed).min(64 - intra);
                let containing_byte_off = ctx.byte_offset(addr, part_bit_off);
                let load_size = ISelContext::op_size_for_width(part_width + intra);

                let part_src = if consumed == 0 {
                    src_vreg
                } else {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: shifted,
                        src: src_vreg,
                        imm: consumed as u8,
                    });
                    shifted
                };

                let old_word = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Load {
                    dst: old_word,
                    base: BaseReg::SimState,
                    offset: containing_byte_off,
                    size: load_size,
                });

                let descriptor = if consumed == 0 && part_width == *width_bits {
                    SpillDesc::transient().with_state_insert(src_vreg, intra, part_width)
                } else {
                    SpillDesc::transient()
                        .with_state_insert_fragment(src_vreg, consumed, intra, part_width)
                };
                let new_word = ctx.alloc_vreg(descriptor);
                ctx.emit_bfi(
                    block,
                    new_word,
                    old_word,
                    part_src,
                    intra as u8,
                    mask_for_width(part_width),
                );

                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: containing_byte_off,
                    src: new_word,
                    size: load_size,
                });

                consumed += part_width;
            }
        }
    }
}

/// The access itself truncates to the machine width. Normalize any extra
/// bits inside the slot, and compare only logical bits when observing it.
fn direct_indexed_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    base: i32,
    index: VReg,
    size: OpSize,
    width: usize,
    src: VReg,
    range: Option<MemoryAliasRange>,
    sites: &[u32],
) {
    let old = if sites.is_empty() {
        None
    } else {
        let old = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadIndexed {
            dst: old,
            base: BaseReg::SimState,
            offset: base,
            index,
            scale: 1,
            size,
            alias_range: range,
        });
        Some(old)
    };
    let src = if ISelContext::access_size_has_padding(size, width) {
        ctx.mask_for_store_width(block, src, width)
    } else {
        src
    };
    block.push(MInst::StoreIndexed {
        base: BaseReg::SimState,
        offset: base,
        index,
        src,
        size,
        alias_range: range,
    });
    if let Some(old) = old {
        let changed = ctx.alloc_vreg(SpillDesc::transient());
        let old = if ISelContext::access_size_has_padding(size, width) {
            ctx.mask_for_store_width(block, old, width)
        } else {
            old
        };
        let new = ctx.mask_for_store_width(block, src, width);
        block.push(MInst::Cmp {
            dst: changed,
            lhs: old,
            rhs: new,
            kind: CmpKind::Ne,
        });
        emit_enable_comb_capture_sites(ctx, block, changed, sites);
    }
}

/// The value plane of a dynamic-offset store.
fn lower_dynamic_value_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    width_bits: &usize,
    src_reg: &RegisterId,
    comb_capture_sites: &[u32],
) {
    // Dynamic offset store: RMW with register-indexed addressing.
    let full_element_size = ctx.full_element_access_size(addr, offset, *width_bits);
    let src_vreg = ctx.reg_map.get(*src_reg);
    let offset_vreg = memory_offset_vreg(ctx, block, addr, offset);
    let offset_low_zero_bits = memory_offset_low_zero_bits(ctx, addr, offset);
    let base_off = ctx.byte_offset(addr, 0);
    let value_alias_range =
        MemoryAliasRange::new(base_off, ctx.layout.plane_size(&addr.absolute_addr()));

    let byte_off = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: byte_off,
        src: offset_vreg,
        imm: 3,
    });
    if *width_bits > 64 && offset_low_zero_bits >= 3 {
        let chunks = ctx.get_wide_chunks(src_reg, block);
        emit_aligned_dynamic_wide_store(
            ctx,
            block,
            base_off,
            byte_off,
            *width_bits,
            value_alias_range,
            &chunks,
        );
    } else if *width_bits > 64 {
        let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, bit_shift, offset_vreg, 7);
        let chunks = ctx.get_wide_chunks(src_reg, block);
        if let Some(changed) = emit_dynamic_wide_bitfield_store(
            ctx,
            block,
            base_off,
            byte_off,
            bit_shift,
            *width_bits,
            value_alias_range,
            &chunks,
            !comb_capture_sites.is_empty(),
        ) {
            emit_enable_comb_capture_sites(ctx, block, changed, comb_capture_sites);
        }
    } else if offset_low_zero_bits >= 3
        && let Some(store_size) = full_element_size.or_else(|| OpSize::from_bits(*width_bits))
    {
        direct_indexed_store(
            ctx,
            block,
            base_off,
            byte_off,
            store_size,
            *width_bits,
            src_vreg,
            value_alias_range,
            comb_capture_sites,
        );
    } else {
        let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, bit_shift, offset_vreg, 7);
        if let Some(changed) = emit_dynamic_scalar_bitfield_store(
            ctx,
            block,
            base_off,
            byte_off,
            bit_shift,
            src_vreg,
            *width_bits,
            value_alias_range,
            !comb_capture_sites.is_empty(),
        ) {
            emit_enable_comb_capture_sites(ctx, block, changed, comb_capture_sites);
        }
    }
}

/// The mask plane of a static-offset store to a four-state variable.
fn lower_static_mask_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    bit_off: &usize,
    width_bits: &usize,
    src_reg: &RegisterId,
    mask_vreg: VReg,
) {
    let plane_offset = ctx.layout.plane_size(&addr.absolute_addr()) as i32;
    if *width_bits > 64 {
        // Wide mask store: chunk-by-chunk. A source
        // without wide mask chunks (e.g. a load of a
        // two-state variable) still defines every
        // destination mask bit: its scalar mask, if
        // any, covers the low chunk and the rest is 0.
        let mchunks = wide_mask_store_chunks(ctx, block, src_reg, *width_bits);
        let mut bit_pos = 0usize;
        let mut store_remaining = *width_bits;
        for (chunk_vreg, chunk_width) in &mchunks {
            if store_remaining == 0 {
                break;
            }
            let logical_chunk_width = (*chunk_width).min(store_remaining);
            let mut consumed = 0usize;
            while consumed < logical_chunk_width {
                let part_bit_off = *bit_off + bit_pos + consumed;
                let intra = ctx.static_byte_and_intra(addr, part_bit_off).1;
                let remaining = logical_chunk_width - consumed;
                let part_width = remaining.min(64 - intra);
                let part_byte_off = ctx.mask_byte_offset(addr, part_bit_off);
                let part_src = if consumed == 0 {
                    *chunk_vreg
                } else {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: shifted,
                        src: *chunk_vreg,
                        imm: consumed as u8,
                    });
                    shifted
                };

                if !try_store_bytes(
                    ctx,
                    block,
                    addr,
                    part_bit_off,
                    part_width,
                    part_src,
                    plane_offset,
                ) {
                    let containing_off = part_byte_off;
                    let load_size = ISelContext::op_size_for_width(part_width + intra);
                    let old = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Load {
                        dst: old,
                        base: BaseReg::SimState,
                        offset: containing_off,
                        size: load_size,
                    });
                    let new = ctx.alloc_vreg(SpillDesc::transient());
                    ctx.emit_bfi(
                        block,
                        new,
                        old,
                        part_src,
                        intra as u8,
                        mask_for_width(part_width),
                    );
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: containing_off,
                        src: new,
                        size: load_size,
                    });
                }
                consumed += part_width;
            }
            bit_pos += logical_chunk_width;
            store_remaining -= logical_chunk_width;
        }
    } else {
        if !try_store_bytes(
            ctx,
            block,
            addr,
            *bit_off,
            *width_bits,
            mask_vreg,
            plane_offset,
        ) {
            let mut consumed = 0usize;
            while consumed < *width_bits {
                let part_bit_off = *bit_off + consumed;
                let intra = ctx.static_byte_and_intra(addr, part_bit_off).1;
                let part_width = (*width_bits - consumed).min(64 - intra);
                let containing_off = ctx.mask_byte_offset(addr, part_bit_off);
                let load_size = ISelContext::op_size_for_width(part_width + intra);
                let part_src = if consumed == 0 {
                    mask_vreg
                } else {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: shifted,
                        src: mask_vreg,
                        imm: consumed as u8,
                    });
                    shifted
                };
                let old = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Load {
                    dst: old,
                    base: BaseReg::SimState,
                    offset: containing_off,
                    size: load_size,
                });
                let descriptor = if consumed == 0 && part_width == *width_bits {
                    SpillDesc::transient().with_state_insert(mask_vreg, intra, part_width)
                } else {
                    SpillDesc::transient()
                        .with_state_insert_fragment(mask_vreg, consumed, intra, part_width)
                };
                let new_word = ctx.alloc_vreg(descriptor);
                ctx.emit_bfi(
                    block,
                    new_word,
                    old,
                    part_src,
                    intra as u8,
                    mask_for_width(part_width),
                );
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: containing_off,
                    src: new_word,
                    size: load_size,
                });
                consumed += part_width;
            }
        }
    }
}

/// The mask plane of a dynamic-offset store to a four-state variable.
fn lower_dynamic_mask_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    width_bits: &usize,
    src_reg: &RegisterId,
    mask_vreg: VReg,
    comb_capture_sites: &[u32],
) {
    // Dynamic mask store: same RMW pattern as value store,
    // but targeting the mask memory region.
    let full_element_size = ctx.full_element_access_size(addr, offset, *width_bits);
    let offset_vreg = memory_offset_vreg(ctx, block, addr, offset);
    let offset_low_zero_bits = memory_offset_low_zero_bits(ctx, addr, offset);
    let mask_base_off = ctx.mask_byte_offset(addr, 0);
    let mask_alias_range =
        MemoryAliasRange::new(mask_base_off, ctx.layout.plane_size(&addr.absolute_addr()));

    let m_byte_off = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: m_byte_off,
        src: offset_vreg,
        imm: 3,
    });
    if *width_bits > 64 && offset_low_zero_bits >= 3 {
        let n_chunks = width_bits.div_ceil(64);
        let mask_vregs = get_wide_mask_chunks(ctx, block, src_reg, n_chunks);
        let mask_chunks = mask_vregs
            .into_iter()
            .enumerate()
            .map(|(index, chunk)| (chunk, (*width_bits - index * 64).min(64)))
            .collect::<Vec<_>>();
        emit_aligned_dynamic_wide_store(
            ctx,
            block,
            mask_base_off,
            m_byte_off,
            *width_bits,
            mask_alias_range,
            &mask_chunks,
        );
    } else if *width_bits > 64 {
        let m_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, m_bit_shift, offset_vreg, 7);
        let n_chunks = width_bits.div_ceil(64);
        let mask_chunks = get_wide_mask_chunks(ctx, block, src_reg, n_chunks)
            .into_iter()
            .enumerate()
            .map(|(index, chunk)| (chunk, (*width_bits - index * 64).min(64)))
            .collect::<Vec<_>>();
        if let Some(changed) = emit_dynamic_wide_bitfield_store(
            ctx,
            block,
            mask_base_off,
            m_byte_off,
            m_bit_shift,
            *width_bits,
            mask_alias_range,
            &mask_chunks,
            !comb_capture_sites.is_empty(),
        ) {
            emit_enable_comb_capture_sites(ctx, block, changed, comb_capture_sites);
        }
    } else if offset_low_zero_bits >= 3
        && let Some(store_size) = full_element_size.or_else(|| OpSize::from_bits(*width_bits))
    {
        direct_indexed_store(
            ctx,
            block,
            mask_base_off,
            m_byte_off,
            store_size,
            *width_bits,
            mask_vreg,
            mask_alias_range,
            comb_capture_sites,
        );
    } else {
        let m_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, m_bit_shift, offset_vreg, 7);
        if let Some(changed) = emit_dynamic_scalar_bitfield_store(
            ctx,
            block,
            mask_base_off,
            m_byte_off,
            m_bit_shift,
            mask_vreg,
            *width_bits,
            mask_alias_range,
            !comb_capture_sites.is_empty(),
        ) {
            emit_enable_comb_capture_sites(ctx, block, changed, comb_capture_sites);
        }
    }
}

/// Trigger detection after a store: reload the stored value and set its trigger bits.
fn lower_store_triggers(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    width_bits: &usize,
    triggers: &[crate::TriggerIdWithKind],
) {
    if !triggers.is_empty() {
        if let SIROffset::Static(bit_off) = offset {
            // Load new value (just stored)
            let byte_off = ctx.byte_offset(addr, *bit_off);
            let new_val = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: new_val,
                base: BaseReg::SimState,
                offset: byte_off,
                size: ISelContext::op_size_for_width(*width_bits),
            });

            for trigger in triggers {
                let trigger_byte_idx = trigger.id / 8;
                let trigger_bit_idx = trigger.id % 8;
                let trigger_offset = ctx.layout.triggered_bits_offset + trigger_byte_idx;

                // Check new_val for trigger condition
                let triggered = ctx.alloc_vreg(SpillDesc::transient());
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero,
                    value: 0,
                });

                // For posedge/async_high: triggered if new_val != 0
                // (old value comparison is handled by Simulation's step())
                block.push(MInst::Cmp {
                    dst: triggered,
                    lhs: new_val,
                    rhs: zero,
                    kind: CmpKind::Ne,
                });

                // Load current triggered byte, OR in the bit, store back
                let old_byte = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Load {
                    dst: old_byte,
                    base: BaseReg::SimState,
                    offset: trigger_offset as i32,
                    size: OpSize::S8,
                });

                let bit_mask = ctx.alloc_vreg(SpillDesc::remat(1u64 << trigger_bit_idx));
                block.push(MInst::LoadImm {
                    dst: bit_mask,
                    value: 1u64 << trigger_bit_idx,
                });

                // conditional: if triggered, OR in the bit
                let selected_mask = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: selected_mask,
                    cond: triggered,
                    true_val: bit_mask,
                    false_val: zero,
                });

                let new_byte = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: new_byte,
                    lhs: old_byte,
                    rhs: selected_mask,
                });

                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: trigger_offset as i32,
                    src: new_byte,
                    size: OpSize::S8,
                });
            }
        }
    }
}

/// The mask chunks a wide store writes for `src`, with their widths.
///
/// Registers produced without wide mask chunks (a load of a two-state
/// variable, a two-state operation) are known by at most a scalar mask for
/// their low chunk; every higher chunk is known (zero mask).
fn wide_mask_store_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    src: &RegisterId,
    width_bits: usize,
) -> Vec<(VReg, usize)> {
    if let Some(chunks) = ctx.wide_masks.get(src).cloned() {
        return chunks;
    }
    let n_chunks = ISelContext::num_chunks(width_bits);
    get_wide_mask_chunks(ctx, block, src, n_chunks)
        .into_iter()
        .enumerate()
        .map(|(index, vreg)| (vreg, (width_bits - index * 64).min(64)))
        .collect()
}
