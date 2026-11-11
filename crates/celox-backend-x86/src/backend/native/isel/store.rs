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
        let old_comb_probe = if comb_capture_sites.is_empty() || *width_bits > 64 {
            None
        } else {
            match offset {
                SIROffset::Static(bit_off)
                | SIROffset::PackedElements {
                    bit_offset: bit_off,
                    ..
                } => {
                    let intra = ctx.static_byte_and_intra(addr, *bit_off).1;
                    let containing_byte_off = ctx.byte_offset(addr, *bit_off);
                    let size = ISelContext::op_size_for_width(*width_bits + intra);
                    let old = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Load {
                        dst: old,
                        base: BaseReg::SimState,
                        offset: containing_byte_off,
                        size,
                    });
                    Some((old, containing_byte_off, size))
                }
                SIROffset::Dynamic(_) | SIROffset::Element { .. } => None,
            }
        };
        let old_comb_wide_probe = if comb_capture_sites.is_empty() || *width_bits <= 64 {
            Vec::new()
        } else if let SIROffset::Static(bit_off) = offset {
            collect_static_comb_store_byte_probes(ctx, block, addr, *bit_off, *width_bits, false)
        } else {
            Vec::new()
        };
        let old_comb_mask_probe =
            if comb_capture_sites.is_empty() || *width_bits > 64 || !ctx.is_4state_var(addr) {
                None
            } else {
                match offset {
                    SIROffset::Static(bit_off)
                    | SIROffset::PackedElements {
                        bit_offset: bit_off,
                        ..
                    } => {
                        let intra = ctx.static_byte_and_intra(addr, *bit_off).1;
                        let containing_byte_off = ctx.mask_byte_offset(addr, *bit_off);
                        let size = ISelContext::op_size_for_width(*width_bits + intra);
                        let old = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Load {
                            dst: old,
                            base: BaseReg::SimState,
                            offset: containing_byte_off,
                            size,
                        });
                        Some((old, containing_byte_off, size))
                    }
                    SIROffset::Dynamic(_) | SIROffset::Element { .. } => None,
                }
            };
        let old_comb_wide_mask_probe =
            if comb_capture_sites.is_empty() || *width_bits <= 64 || !ctx.is_4state_var(addr) {
                Vec::new()
            } else if let SIROffset::Static(bit_off) = offset {
                collect_static_comb_store_byte_probes(ctx, block, addr, *bit_off, *width_bits, true)
            } else {
                Vec::new()
            };
        match offset {
            SIROffset::Static(bit_off)
            | SIROffset::PackedElements {
                bit_offset: bit_off,
                ..
            } => {
                lower_static_value_store(ctx, block, addr, bit_off, width_bits, src_reg);
            }
            SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                lower_dynamic_value_store(
                    ctx,
                    block,
                    addr,
                    offset,
                    width_bits,
                    src_reg,
                    comb_capture_sites,
                );
            }
        }

        // 4-state: store mask to memory
        if ctx.is_4state_var(addr) {
            let mask_vreg = ctx.get_mask(*src_reg, block);
            match offset {
                SIROffset::Static(bit_off)
                | SIROffset::PackedElements {
                    bit_offset: bit_off,
                    ..
                } => {
                    lower_static_mask_store(
                        ctx, block, addr, bit_off, width_bits, src_reg, mask_vreg,
                    );
                }
                SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                    lower_dynamic_mask_store(
                        ctx,
                        block,
                        addr,
                        offset,
                        width_bits,
                        src_reg,
                        mask_vreg,
                        comb_capture_sites,
                    );
                }
            }
        }

        if let Some((old_comb_probe, byte_off, size)) = old_comb_probe {
            let new_comb_probe = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: new_comb_probe,
                base: BaseReg::SimState,
                offset: byte_off,
                size,
            });
            let changed = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: changed,
                lhs: old_comb_probe,
                rhs: new_comb_probe,
                kind: CmpKind::Ne,
            });
            emit_enable_comb_capture_sites(ctx, block, changed, comb_capture_sites);
        }
        if !old_comb_wide_probe.is_empty() {
            emit_enable_comb_capture_sites_if_byte_probes_changed(
                ctx,
                block,
                old_comb_wide_probe,
                comb_capture_sites,
            );
        }
        if let Some((old_comb_mask_probe, byte_off, size)) = old_comb_mask_probe {
            let new_comb_mask_probe = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: new_comb_mask_probe,
                base: BaseReg::SimState,
                offset: byte_off,
                size,
            });
            let changed = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: changed,
                lhs: old_comb_mask_probe,
                rhs: new_comb_mask_probe,
                kind: CmpKind::Ne,
            });
            emit_enable_comb_capture_sites(ctx, block, changed, comb_capture_sites);
        }
        if !old_comb_wide_mask_probe.is_empty() {
            emit_enable_comb_capture_sites_if_byte_probes_changed(
                ctx,
                block,
                old_comb_wide_mask_probe,
                comb_capture_sites,
            );
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

                    if intra == 0 && OpSize::from_bits(part_width).is_some() {
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: part_byte_off,
                            src: part_src,
                            size: OpSize::from_bits(part_width).unwrap(),
                        });
                    } else {
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
        let byte_off = ctx.byte_offset(addr, *bit_off);
        let intra_byte = ctx.static_byte_and_intra(addr, *bit_off).1;

        if let Some(size) = ctx.full_static_store_size(addr, *bit_off, *width_bits) {
            let src_vreg = ctx.mask_for_store_width(block, src_vreg, *width_bits);
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: byte_off,
                src: src_vreg,
                size,
            });
        } else if intra_byte == 0 && OpSize::from_bits(*width_bits).is_some() {
            // Word-aligned, native size: direct store
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: byte_off,
                src: src_vreg,
                size: OpSize::from_bits(*width_bits).unwrap(),
            });
        } else if intra_byte == 0 && *width_bits <= 64 && width_bits.is_multiple_of(8) {
            // Whole bytes of another count: store them in
            // native-size pieces, low first, instead of a
            // read-modify-write of the containing word.
            let mut consumed = 0usize;
            while consumed < *width_bits {
                let piece = [64, 32, 16, 8]
                    .into_iter()
                    .find(|&piece| piece <= *width_bits - consumed)
                    .unwrap();
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
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: byte_off + (consumed / 8) as i32,
                    src: part_src,
                    size: OpSize::from_bits(piece).unwrap(),
                });
                consumed += piece;
            }
        } else {
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
        let store_src = if *width_bits < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, src_vreg, mask_for_width(*width_bits));
            masked
        } else {
            src_vreg
        };
        let old_word = if comb_capture_sites.is_empty() {
            None
        } else {
            let old = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::LoadIndexed {
                dst: old,
                base: BaseReg::SimState,
                offset: base_off,
                index: byte_off,
                scale: 1,
                size: store_size,
                alias_range: value_alias_range,
            });
            Some(old)
        };
        block.push(MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: base_off,
            index: byte_off,
            src: store_src,
            size: store_size,
            alias_range: value_alias_range,
        });
        if let Some(old_word) = old_word {
            let changed = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: changed,
                lhs: old_word,
                rhs: store_src,
                kind: CmpKind::Ne,
            });
            emit_enable_comb_capture_sites(ctx, block, changed, comb_capture_sites);
        }
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

                if intra == 0 && OpSize::from_bits(part_width).is_some() {
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: part_byte_off,
                        src: part_src,
                        size: OpSize::from_bits(part_width).unwrap(),
                    });
                } else {
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
        let mask_off = ctx.mask_byte_offset(addr, *bit_off);
        let intra_byte = ctx.static_byte_and_intra(addr, *bit_off).1;
        if let Some(size) = ctx.full_static_store_size(addr, *bit_off, *width_bits) {
            let mask_vreg = ctx.mask_for_store_width(block, mask_vreg, *width_bits);
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: mask_off,
                src: mask_vreg,
                size,
            });
        } else if intra_byte == 0 && OpSize::from_bits(*width_bits).is_some() {
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: mask_off,
                src: mask_vreg,
                size: OpSize::from_bits(*width_bits).unwrap(),
            });
        } else {
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
        let store_src = if *width_bits < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, mask_vreg, mask_for_width(*width_bits));
            masked
        } else {
            mask_vreg
        };
        block.push(MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: mask_base_off,
            index: m_byte_off,
            src: store_src,
            size: store_size,
            alias_range: mask_alias_range,
        });
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
