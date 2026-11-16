//! Slice lowering, and keeping direct slice loads only while their source
//! memory is unchanged.

use super::*;

impl ISelContext<'_> {
    pub(super) fn update_slice_load_origins(
        &mut self,
        inst: &SIRInstruction<RegionedAbsoluteAddr>,
    ) {
        // A function unit may write any state.
        if matches!(inst, SIRInstruction::Call { .. }) {
            self.reg_addrs.clear();
        }
        if let Some(write) = inst.memory_write()
            && write.width != 0
        {
            let layout = self.layout;
            // Compare physical homes: distinct regions can share a home.
            // Include both value and mask planes, and conservatively cover the
            // whole object for dynamic offsets and element-strided storage.
            let physical_range = |addr: &RegionedAbsoluteAddr| {
                let start = i64::from(layout.regioned_static_byte_and_intra(addr, 0)?.0);
                let planes = if self.four_state
                    && layout
                        .is_4states
                        .get(&addr.absolute_addr())
                        .copied()
                        .unwrap_or(false)
                {
                    2
                } else {
                    1
                };
                let size = layout
                    .plane_size(&addr.absolute_addr())
                    .checked_mul(planes)?;
                Some((start, start.checked_add(i64::try_from(size).ok()?)?))
            };
            let written = physical_range(write.addr);
            self.reg_addrs
                .retain(|_, (addr, _)| match (physical_range(addr), written) {
                    (Some((start, end)), Some((write_start, write_end))) => {
                        end <= write_start || write_end <= start
                    }
                    _ => false,
                });
        }
        if let SIRInstruction::Load(dst, addr, offset, _) = inst {
            match offset {
                SIROffset::Static(bit_offset) | SIROffset::PackedElements { bit_offset, .. } => {
                    self.reg_addrs.insert(*dst, (*addr, *bit_offset));
                }
                _ => {
                    self.reg_addrs.remove(dst);
                }
            }
        }
    }
}

/// Lower `Slice(dst, src, bit_offset, width)`.
pub(super) fn lower_slice(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: &RegisterId,
    src: &RegisterId,
    bit_offset: &usize,
    width: &usize,
) {
    let dst_vreg = ctx.reg_map.get(*dst);
    let src_width = ctx.sir_width(src);

    // Reload only while the source's memory still holds its captured
    // value. Overlapping writes invalidate the origin before lowering.
    if let Some((addr, source_bit_offset)) = ctx.reg_addrs.get(src).cloned() {
        let slice_bit_offset = source_bit_offset + *bit_offset;
        let value_base = ctx.byte_offset(&addr, slice_bit_offset);
        let intra = ctx.static_byte_and_intra(&addr, slice_bit_offset).1;
        let value_chunks = lower_static_wide_load_chunks(ctx, block, value_base, intra, *width);
        if *width <= 64 {
            ctx.emit_mov(block, dst_vreg, value_chunks[0].0);
        } else {
            ctx.set_wide_chunks(*dst, value_chunks);
        }

        if ctx.four_state {
            let mask_chunks = if ctx.is_4state_var(&addr) {
                let mask_base = ctx.mask_byte_offset(&addr, slice_bit_offset);
                lower_static_wide_load_chunks(ctx, block, mask_base, intra, *width)
            } else {
                let n_chunks = ISelContext::num_chunks(*width).max(1);
                let mut chunks = Vec::with_capacity(n_chunks);
                for index in 0..n_chunks {
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    let chunk_width = width.saturating_sub(index * 64).min(64);
                    chunks.push((zero, chunk_width));
                }
                chunks
            };
            ctx.set_mask(*dst, mask_chunks[0].0);
            if *width > 64 {
                ctx.wide_masks.insert(*dst, mask_chunks);
            }
        }
        return;
    }

    if *width <= 64 && src_width <= 64 {
        let src_vreg = ctx.reg_map.get(*src);
        if *bit_offset == 0 && *width == src_width {
            ctx.emit_mov(block, dst_vreg, src_vreg);
        } else if *bit_offset == 0 {
            let mask = mask_for_width(*width);
            ctx.emit_and_imm(block, dst_vreg, src_vreg, mask);
        } else {
            let shifted = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted,
                src: src_vreg,
                imm: *bit_offset as u8,
            });
            let mask = mask_for_width(*width);
            ctx.emit_and_imm(block, dst_vreg, shifted, mask);
        }
    } else if *width <= 64 {
        // Narrow slice from wide source
        let src_chunks = ctx.get_wide_chunks(src, block);
        let chunk_idx = *bit_offset / 64;
        let intra_bit = *bit_offset % 64;
        let main = ctx.wide_chunk_or_zero(&src_chunks, chunk_idx, block);

        if intra_bit == 0 {
            let mask = mask_for_width(*width);
            ctx.emit_and_imm(block, dst_vreg, main, mask);
        } else if intra_bit + *width <= 64 {
            // Fits in one chunk after shift
            let shifted = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted,
                src: main,
                imm: intra_bit as u8,
            });
            let mask = mask_for_width(*width);
            ctx.emit_and_imm(block, dst_vreg, shifted, mask);
        } else {
            // Crosses chunk boundary
            let lo = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: lo,
                src: main,
                imm: intra_bit as u8,
            });
            let upper = ctx.wide_chunk_or_zero(&src_chunks, chunk_idx + 1, block);
            let hi = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShlImm {
                dst: hi,
                src: upper,
                imm: (64 - intra_bit) as u8,
            });
            let combined = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: combined,
                lhs: lo,
                rhs: hi,
            });
            let mask = mask_for_width(*width);
            ctx.emit_and_imm(block, dst_vreg, combined, mask);
        }
    } else {
        // Wide slice: extract bits from a wide source.
        // Get source chunks, then extract the requested range.
        let src_chunks = ctx.get_wide_chunks(src, block);
        let dst_n_chunks = ISelContext::num_chunks(*width);
        let chunk_start = *bit_offset / 64;
        let intra_bit = *bit_offset % 64;

        let mut dst_chunks = Vec::with_capacity(dst_n_chunks);
        for i in 0..dst_n_chunks {
            let src_idx = chunk_start + i;
            let main = ctx.wide_chunk_or_zero(&src_chunks, src_idx, block);

            if intra_bit == 0 {
                dst_chunks.push((main, 64));
            } else {
                // Cross-chunk: combine bits from src[src_idx] and src[src_idx+1]
                let lo = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: lo,
                    src: main,
                    imm: intra_bit as u8,
                });
                let upper = ctx.wide_chunk_or_zero(&src_chunks, src_idx + 1, block);
                let hi = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShlImm {
                    dst: hi,
                    src: upper,
                    imm: (64 - intra_bit) as u8,
                });
                let combined = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: combined,
                    lhs: lo,
                    rhs: hi,
                });
                dst_chunks.push((combined, 64));
            }
        }

        // Mask the top chunk to the exact width
        let top_bits = *width % 64;
        if top_bits != 0 && !dst_chunks.is_empty() {
            let last_idx = dst_chunks.len() - 1;
            let (last_vreg, _) = dst_chunks[last_idx];
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, last_vreg, mask_for_width(top_bits));
            dst_chunks[last_idx] = (masked, top_bits);
        }

        ctx.set_wide_chunks(*dst, dst_chunks);
    }

    if ctx.four_state {
        lower_slice_mask(ctx, block, *dst, *src, *bit_offset, *width);
    }
}
