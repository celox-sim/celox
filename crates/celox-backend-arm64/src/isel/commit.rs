//! Commit lowering: copies between state regions.

use super::*;

/// Lower `Commit(src_addr, dst_addr, offset, width_bits, _)`.
pub(super) fn lower_commit(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    src_addr: &RegionedAbsoluteAddr,
    dst_addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    width_bits: &usize,
) {
    ctx.trigger_only_seen.clear();
    let whole_array_plane = match offset {
        SIROffset::Static(0) => ctx
            .layout
            .unpacked_arrays
            .get(&src_addr.absolute_addr())
            .zip(ctx.layout.unpacked_arrays.get(&dst_addr.absolute_addr()))
            .filter(|(src, dst)| src == dst && *width_bits == src.element_width * src.element_count)
            .map(|(layout, _)| layout.plane_size),
        _ => None,
    };
    // Commit = load from src region, store to dst region (same offset/width)
    if let Some(byte_len) = whole_array_plane {
        block.push(MInst::MemCopy {
            src_offset: ctx.byte_offset(src_addr, 0),
            dst_offset: ctx.byte_offset(dst_addr, 0),
            byte_len,
        });
    } else {
        match offset {
            SIROffset::Static(bit_off)
            | SIROffset::PackedElements {
                bit_offset: bit_off,
                ..
            } => {
                let (src_byte_off, intra) = ctx.static_byte_and_intra(src_addr, *bit_off);
                let (dst_byte_off, dst_intra) = ctx.static_byte_and_intra(dst_addr, *bit_off);
                let packed_layouts = !ctx
                    .layout
                    .unpacked_arrays
                    .contains_key(&src_addr.absolute_addr())
                    && !ctx
                        .layout
                        .unpacked_arrays
                        .contains_key(&dst_addr.absolute_addr());
                if packed_layouts
                    && intra == 0
                    && dst_intra == 0
                    && width_bits.is_multiple_of(8)
                    && *width_bits >= 512
                {
                    block.push(MInst::MemCopy {
                        src_offset: src_byte_off,
                        dst_offset: dst_byte_off,
                        byte_len: width_bits / 8,
                    });
                } else {
                    emit_static_commit_plane(
                        ctx,
                        block,
                        src_addr,
                        dst_addr,
                        *bit_off,
                        *width_bits,
                        false,
                    );
                }
            }
            SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                // Dynamic offset commit: copy from src to dst region.
                // Both use the same dynamic offset.
                let offset_vreg = memory_offset_vreg(ctx, block, src_addr, offset);
                let src_base_off = ctx.byte_offset(src_addr, 0);
                let dst_base_off = ctx.byte_offset(dst_addr, 0);

                let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: byte_off,
                    src: offset_vreg,
                    imm: 3,
                });

                // For simplicity, copy the containing bytes chunk-by-chunk.
                // The physical width covers width_bits + up to 7 bit shift.
                let phys_bytes = (*width_bits).div_ceil(8);
                let mut copied = 0usize;
                while copied < phys_bytes {
                    let remaining = phys_bytes - copied;
                    let chunk_size = if remaining >= 8 {
                        OpSize::S64
                    } else if remaining >= 4 {
                        OpSize::S32
                    } else if remaining >= 2 {
                        OpSize::S16
                    } else {
                        OpSize::S8
                    };
                    let tmp = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::LoadIndexed {
                        dst: tmp,
                        base: BaseReg::SimState,
                        offset: src_base_off + copied as i32,
                        index: byte_off,
                        scale: 1,
                        size: chunk_size,
                        alias_range: MemoryAliasRange::new(
                            src_base_off,
                            ctx.layout.plane_size(&src_addr.absolute_addr()),
                        ),
                    });
                    block.push(MInst::StoreIndexed {
                        base: BaseReg::SimState,
                        offset: dst_base_off + copied as i32,
                        index: byte_off,
                        src: tmp,
                        size: chunk_size,
                        alias_range: MemoryAliasRange::new(
                            dst_base_off,
                            ctx.layout.plane_size(&dst_addr.absolute_addr()),
                        ),
                    });
                    copied += chunk_size.bytes() as usize;
                }
            }
        }
    }

    // 4-state: also commit mask
    if ctx.is_4state_var(src_addr) && ctx.is_4state_var(dst_addr) {
        if let Some(byte_len) = whole_array_plane {
            block.push(MInst::MemCopy {
                src_offset: ctx.mask_byte_offset(src_addr, 0),
                dst_offset: ctx.mask_byte_offset(dst_addr, 0),
                byte_len,
            });
        } else {
            match offset {
                SIROffset::Static(bit_off)
                | SIROffset::PackedElements {
                    bit_offset: bit_off,
                    ..
                } => {
                    let (src_value_off, intra) = ctx.static_byte_and_intra(src_addr, *bit_off);
                    let (dst_value_off, dst_intra) = ctx.static_byte_and_intra(dst_addr, *bit_off);
                    let packed_layouts = !ctx
                        .layout
                        .unpacked_arrays
                        .contains_key(&src_addr.absolute_addr())
                        && !ctx
                            .layout
                            .unpacked_arrays
                            .contains_key(&dst_addr.absolute_addr());
                    if packed_layouts
                        && intra == 0
                        && dst_intra == 0
                        && width_bits.is_multiple_of(8)
                        && *width_bits >= 512
                    {
                        let src_byte_off =
                            src_value_off + ctx.layout.plane_size(&src_addr.absolute_addr()) as i32;
                        let dst_byte_off =
                            dst_value_off + ctx.layout.plane_size(&dst_addr.absolute_addr()) as i32;
                        block.push(MInst::MemCopy {
                            src_offset: src_byte_off,
                            dst_offset: dst_byte_off,
                            byte_len: width_bits / 8,
                        });
                    } else {
                        emit_static_commit_plane(
                            ctx,
                            block,
                            src_addr,
                            dst_addr,
                            *bit_off,
                            *width_bits,
                            true,
                        );
                    }
                }
                SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                    let offset_vreg = memory_offset_vreg(ctx, block, src_addr, offset);
                    let src_mask_base = ctx.mask_byte_offset(src_addr, 0);
                    let dst_mask_base = ctx.mask_byte_offset(dst_addr, 0);
                    let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: byte_off,
                        src: offset_vreg,
                        imm: 3,
                    });
                    let phys_bytes = (*width_bits).div_ceil(8);
                    let mut copied = 0usize;
                    while copied < phys_bytes {
                        let remaining = phys_bytes - copied;
                        let cs = if remaining >= 8 {
                            OpSize::S64
                        } else if remaining >= 4 {
                            OpSize::S32
                        } else if remaining >= 2 {
                            OpSize::S16
                        } else {
                            OpSize::S8
                        };
                        let tmp = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::LoadIndexed {
                            dst: tmp,
                            base: BaseReg::SimState,
                            offset: src_mask_base + copied as i32,
                            index: byte_off,
                            scale: 1,
                            size: cs,
                            alias_range: MemoryAliasRange::new(
                                src_mask_base,
                                ctx.layout.plane_size(&src_addr.absolute_addr()),
                            ),
                        });
                        block.push(MInst::StoreIndexed {
                            base: BaseReg::SimState,
                            offset: dst_mask_base + copied as i32,
                            index: byte_off,
                            src: tmp,
                            size: cs,
                            alias_range: MemoryAliasRange::new(
                                dst_mask_base,
                                ctx.layout.plane_size(&dst_addr.absolute_addr()),
                            ),
                        });
                        copied += cs.bytes() as usize;
                    }
                }
            }
        }
    }
}
