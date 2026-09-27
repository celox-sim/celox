//! Memory addressing, sparse stores, and dynamic memory accesses.

use super::*;

fn emit_sparse_mark_active(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    sparse: &celox_state_layout::SparseWorkingLayout,
) {
    if ctx.sparse_descriptor_table.is_some() {
        block.push(MInst::SparseMarkActive {
            active_index: sparse.active_index as u32,
            active_bits_offset: ctx.layout.sparse_active_bits_offset as i32,
            active_capacity: ctx.layout.sparse_active_capacity,
        });
    }
}

fn emit_full_sparse_bitset(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    offset: usize,
    bit_count: usize,
) {
    let full_words = bit_count / 64;
    if full_words != 0 {
        block.push(MInst::MemFill {
            dst_offset: offset as i32,
            byte_len: full_words * 8,
            value: u8::MAX,
        });
    }
    let tail_bits = bit_count % 64;
    if tail_bits != 0 {
        let tail = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(tail_bits)));
        block.push(MInst::LoadImm {
            dst: tail,
            value: mask_for_width(tail_bits),
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: (offset + full_words * 8) as i32,
            src: tail,
            size: OpSize::S64,
        });
    }
}

/// Lower a complete logical zero overwrite directly into physical storage.
/// Every byte in each physical element slot may be canonicalized to zero
/// because padding is not part of RTL state. A sparse write marks every data
/// chunk dirty; a dependency-proved direct STABLE write needs only the fill.
pub(super) fn emit_state_zero_fill(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    address: RegionedAbsoluteAddr,
) {
    let object = address.absolute_addr();
    let plane_size = ctx.layout.plane_size(&object);
    if address.region == STABLE_REGION {
        let stable_base = ctx.layout.offsets[&object];
        block.push(MInst::MemFill {
            dst_offset: stable_base as i32,
            byte_len: plane_size,
            value: 0,
        });
        if ctx.is_4state_var(&address) {
            block.push(MInst::MemFill {
                dst_offset: (stable_base + plane_size) as i32,
                byte_len: plane_size,
                value: 0,
            });
        }
        return;
    }

    debug_assert_eq!(address.region, crate::SPARSE_WORKING_REGION);
    let sparse = ctx.layout.sparse_layouts[&object].clone();
    let sparse_base = ctx.layout.sparse_base_offset + ctx.layout.sparse_offsets[&object];

    emit_sparse_mark_active(ctx, block, &sparse);
    block.push(MInst::MemFill {
        dst_offset: sparse_base as i32,
        byte_len: plane_size,
        value: 0,
    });
    if ctx.is_4state_var(&address) {
        block.push(MInst::MemFill {
            dst_offset: (sparse_base + plane_size) as i32,
            byte_len: plane_size,
            value: 0,
        });
    }
    emit_full_sparse_bitset(ctx, block, sparse.dirty_words_offset, sparse.chunk_count);
    emit_full_sparse_bitset(
        ctx,
        block,
        sparse.summary_words_offset,
        sparse.dirty_word_count,
    );
}

fn logical_offset_vreg(ctx: &mut ISelContext, block: &mut MBlock, offset: &SIROffset) -> VReg {
    match offset {
        SIROffset::Static(value)
        | SIROffset::PackedElements {
            bit_offset: value, ..
        } => {
            let result = ctx.alloc_vreg(SpillDesc::remat(*value as u64));
            block.push(MInst::LoadImm {
                dst: result,
                value: *value as u64,
            });
            result
        }
        SIROffset::Dynamic(reg) => ctx.reg_map.get(*reg),
        SIROffset::Element {
            index,
            element_width,
            bit_offset,
            dynamic_bit_offset,
        } => {
            let index = ctx.reg_map.get(*index);
            let scaled = if *element_width == 1 {
                index
            } else {
                let scale = ctx.alloc_vreg(SpillDesc::remat(*element_width as u64));
                block.push(MInst::LoadImm {
                    dst: scale,
                    value: *element_width as u64,
                });
                let scaled = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Mul {
                    dst: scaled,
                    lhs: index,
                    rhs: scale,
                });
                scaled
            };
            let with_static = if *bit_offset == 0 {
                scaled
            } else if let Ok(imm) = i32::try_from(*bit_offset) {
                let result = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::AddImm {
                    dst: result,
                    src: scaled,
                    imm,
                });
                result
            } else {
                let constant = ctx.alloc_vreg(SpillDesc::remat(*bit_offset as u64));
                block.push(MInst::LoadImm {
                    dst: constant,
                    value: *bit_offset as u64,
                });
                let result = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Add {
                    dst: result,
                    lhs: scaled,
                    rhs: constant,
                });
                result
            };
            if let Some(dynamic) = dynamic_bit_offset {
                let result = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Add {
                    dst: result,
                    lhs: with_static,
                    rhs: ctx.reg_map.get(*dynamic),
                });
                result
            } else {
                with_static
            }
        }
    }
}

fn logical_offset_low_zero_bits(ctx: &ISelContext, offset: &SIROffset) -> u32 {
    match offset {
        SIROffset::Static(value)
        | SIROffset::PackedElements {
            bit_offset: value, ..
        } => value.trailing_zeros(),
        SIROffset::Dynamic(reg) => ctx.low_zero_bits.get(reg).copied().unwrap_or(0),
        SIROffset::Element {
            index,
            element_width,
            bit_offset,
            dynamic_bit_offset,
        } => {
            let product_zeros = ctx
                .low_zero_bits
                .get(index)
                .copied()
                .unwrap_or(0)
                .saturating_add(element_width.trailing_zeros());
            let static_zeros = if *bit_offset == 0 {
                product_zeros
            } else {
                product_zeros.min(bit_offset.trailing_zeros())
            };
            if let Some(dynamic) = dynamic_bit_offset {
                static_zeros.min(ctx.low_zero_bits.get(dynamic).copied().unwrap_or(0))
            } else {
                static_zeros
            }
        }
    }
}

pub(super) fn memory_offset_vreg(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
) -> VReg {
    let abs = addr.absolute_addr();
    let Some(array) = ctx.layout.unpacked_arrays.get(&abs).copied() else {
        return logical_offset_vreg(ctx, block, offset);
    };
    match offset {
        SIROffset::Element {
            index,
            bit_offset,
            dynamic_bit_offset,
            ..
        } => {
            let index = ctx.reg_map.get(*index);
            let stride_bits = array.element_stride * 8;
            let stride = ctx.alloc_vreg(SpillDesc::remat(stride_bits as u64));
            block.push(MInst::LoadImm {
                dst: stride,
                value: stride_bits as u64,
            });
            let scaled = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Mul {
                dst: scaled,
                lhs: index,
                rhs: stride,
            });
            let with_static = if *bit_offset == 0 {
                scaled
            } else if let Ok(imm) = i32::try_from(*bit_offset) {
                let result = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::AddImm {
                    dst: result,
                    src: scaled,
                    imm,
                });
                result
            } else {
                let constant = ctx.alloc_vreg(SpillDesc::remat(*bit_offset as u64));
                block.push(MInst::LoadImm {
                    dst: constant,
                    value: *bit_offset as u64,
                });
                let result = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Add {
                    dst: result,
                    lhs: scaled,
                    rhs: constant,
                });
                result
            };
            if let Some(dynamic) = dynamic_bit_offset {
                let result = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Add {
                    dst: result,
                    lhs: with_static,
                    rhs: ctx.reg_map.get(*dynamic),
                });
                result
            } else {
                with_static
            }
        }
        SIROffset::Static(bit_offset) | SIROffset::PackedElements { bit_offset, .. } => {
            let (byte_offset, intra) = ctx.layout.map_static_bit_offset(&abs, *bit_offset);
            let physical = byte_offset * 8 + intra;
            let result = ctx.alloc_vreg(SpillDesc::remat(physical as u64));
            block.push(MInst::LoadImm {
                dst: result,
                value: physical as u64,
            });
            result
        }
        SIROffset::Dynamic(_) => {
            unreachable!("arbitrary dynamic offsets disqualify an element-strided array")
        }
    }
}

/// Recover a byte index from the quotient/remainder form produced by a
/// dynamic packed selection inside an unpacked element:
///
/// ```text
/// (x >> log2(D / L)) * D + ((x * L) & (D - 1)) == x * L
/// ```
///
/// Here `D` is the logical unpacked-element width and `L` is the selected
/// packed-lane width.  This is also a physical-address identity only when the
/// storage layout has no padding between those unpacked elements.
pub(super) fn recomposed_element_byte_offset(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
) -> Option<VReg> {
    let SIROffset::Element {
        index,
        element_width,
        bit_offset,
        dynamic_bit_offset: Some(dynamic),
    } = offset
    else {
        return None;
    };
    if !element_width.is_power_of_two() || !bit_offset.is_multiple_of(8) {
        return None;
    }
    if let Some(array) = ctx.layout.unpacked_arrays.get(&addr.absolute_addr())
        && array.element_stride.checked_mul(8) != Some(*element_width)
    {
        return None;
    }

    let instruction = |register: RegisterId| {
        sir_defs
            .get(&register)
            .and_then(|&position| sir_block.instructions.get(position))
    };
    let SIRInstruction::Binary(_, source, BinaryOp::Shr, shift_register) = instruction(*index)?
    else {
        return None;
    };
    let shift = *ctx.consts.get(shift_register)?;

    let SIRInstruction::Binary(_, and_lhs, BinaryOp::And, and_rhs) = instruction(*dynamic)? else {
        return None;
    };
    let (product, remainder_mask) = match (ctx.consts.get(and_lhs), ctx.consts.get(and_rhs)) {
        (Some(&mask), None) => (*and_rhs, mask),
        (None, Some(&mask)) => (*and_lhs, mask),
        _ => return None,
    };
    if remainder_mask != (*element_width as u64).wrapping_sub(1) {
        return None;
    }

    let SIRInstruction::Binary(_, mul_lhs, BinaryOp::Mul, mul_rhs) = instruction(product)? else {
        return None;
    };
    let (product_source, lane_width) = match (ctx.consts.get(mul_lhs), ctx.consts.get(mul_rhs)) {
        (Some(&lane_width), None) => (*mul_rhs, lane_width),
        (None, Some(&lane_width)) => (*mul_lhs, lane_width),
        _ => return None,
    };
    if product_source != *source
        || lane_width == 0
        || !lane_width.is_power_of_two()
        || !lane_width.is_multiple_of(8)
        || lane_width > *element_width as u64
    {
        return None;
    }
    let lanes_per_element = (*element_width as u64) / lane_width;
    if !lanes_per_element.is_power_of_two() || shift != lanes_per_element.trailing_zeros() as u64 {
        return None;
    }

    // All arithmetic which formed the quotient and remainder must have the
    // same modulo semantics.  The recovered physical byte index must also fit
    // the native 64-bit address calculation without wrapping.
    let source_width = ctx.sir_width(source);
    if [*index, product, *dynamic]
        .into_iter()
        .any(|register| ctx.sir_width(&register) != source_width)
    {
        return None;
    }
    let lane_byte_shift = (lane_width / 8).trailing_zeros() as usize;
    if source_width.saturating_add(lane_width.trailing_zeros() as usize) > 64 {
        return None;
    }

    let source = ctx.reg_map.get(*source);
    let scaled = if lane_byte_shift == 0 {
        source
    } else {
        let scaled = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShlImm {
            dst: scaled,
            src: source,
            imm: lane_byte_shift as u8,
        });
        ctx.known_bits
            .insert(scaled, source_width + lane_byte_shift);
        scaled
    };
    let static_bytes = bit_offset / 8;
    if static_bytes == 0 {
        Some(scaled)
    } else if let Ok(imm) = i32::try_from(static_bytes) {
        let result = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::AddImm {
            dst: result,
            src: scaled,
            imm,
        });
        Some(result)
    } else {
        None
    }
}

/// Lower a byte-aligned element address in byte units from the start.  The
/// physical stride comes from MemoryLayout, so this handles both compact and
/// padded element storage without constructing a bit offset only to divide it
/// by eight again.
pub(super) fn direct_element_byte_offset(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
) -> Option<VReg> {
    let SIROffset::Element {
        index,
        element_width,
        bit_offset,
        dynamic_bit_offset,
    } = offset
    else {
        return None;
    };
    if dynamic_bit_offset
        .as_ref()
        .is_some_and(|dynamic| ctx.consts.get(dynamic) != Some(&0))
    {
        return None;
    }
    if !bit_offset.is_multiple_of(8) {
        return None;
    }
    let stride_bytes = if let Some(array) = ctx.layout.unpacked_arrays.get(&addr.absolute_addr()) {
        array.element_stride
    } else {
        element_width
            .checked_div(8)
            .filter(|_| element_width.is_multiple_of(8))?
    };
    if stride_bytes == 0 {
        return None;
    }

    // The old path formed `index * (stride_bytes * 8)` in a u64 and then
    // shifted right.  Only bypass it when that bit product cannot wrap.
    let source_width = ctx.sir_width(index);
    let stride_bits =
        usize::BITS as usize - stride_bytes.saturating_sub(1).leading_zeros() as usize;
    if source_width.saturating_add(stride_bits) > 61 {
        return None;
    }

    let index = ctx.reg_map.get(*index);
    let scaled = if stride_bytes == 1 {
        index
    } else if stride_bytes.is_power_of_two() {
        let scaled = ctx.alloc_vreg(SpillDesc::transient());
        let shift = stride_bytes.trailing_zeros() as u8;
        block.push(MInst::ShlImm {
            dst: scaled,
            src: index,
            imm: shift,
        });
        ctx.known_bits.insert(scaled, source_width + shift as usize);
        scaled
    } else {
        let stride = ctx.alloc_vreg(SpillDesc::remat(stride_bytes as u64));
        block.push(MInst::LoadImm {
            dst: stride,
            value: stride_bytes as u64,
        });
        let scaled = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Mul {
            dst: scaled,
            lhs: index,
            rhs: stride,
        });
        scaled
    };
    let static_bytes = bit_offset / 8;
    if static_bytes == 0 {
        Some(scaled)
    } else if let Ok(imm) = i32::try_from(static_bytes) {
        let result = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::AddImm {
            dst: result,
            src: scaled,
            imm,
        });
        Some(result)
    } else {
        None
    }
}

pub(super) fn memory_offset_low_zero_bits(
    ctx: &ISelContext,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
) -> u32 {
    let abs = addr.absolute_addr();
    let Some(array) = ctx.layout.unpacked_arrays.get(&abs) else {
        return logical_offset_low_zero_bits(ctx, offset);
    };
    match offset {
        SIROffset::Element {
            index,
            bit_offset,
            dynamic_bit_offset,
            ..
        } => {
            let product_zeros = ctx
                .low_zero_bits
                .get(index)
                .copied()
                .unwrap_or(0)
                .saturating_add((array.element_stride * 8).trailing_zeros());
            let static_zeros = if *bit_offset == 0 {
                product_zeros
            } else {
                product_zeros.min(bit_offset.trailing_zeros())
            };
            if let Some(dynamic) = dynamic_bit_offset {
                static_zeros.min(ctx.low_zero_bits.get(dynamic).copied().unwrap_or(0))
            } else {
                static_zeros
            }
        }
        SIROffset::Static(bit_offset) | SIROffset::PackedElements { bit_offset, .. } => {
            let (byte_offset, intra) = ctx.layout.map_static_bit_offset(&abs, *bit_offset);
            (byte_offset * 8 + intra).trailing_zeros()
        }
        SIROffset::Dynamic(_) => {
            unreachable!("arbitrary dynamic offsets disqualify an element-strided array")
        }
    }
}

/// Number of logical bits which can be accessed from `bit_offset` by one
/// native scalar operation without crossing an element-strided storage gap.
/// Packed values have no element boundary, while a strided element may need
/// multiple native accesses when its byte extent is not 1, 2, 4, or 8 bytes.
fn static_commit_chunk_capacity(
    ctx: &ISelContext,
    addr: &RegionedAbsoluteAddr,
    bit_offset: usize,
) -> usize {
    let abs = addr.absolute_addr();
    let Some(array) = ctx.layout.unpacked_arrays.get(&abs) else {
        return 64 - bit_offset % 8;
    };

    let element_bit = bit_offset % array.element_width;
    let byte_in_element = element_bit / 8;
    let intra_byte = element_bit % 8;
    let bytes_left = array.element_stride - byte_in_element;
    let native_bits = if bytes_left >= 8 {
        64
    } else if bytes_left >= 4 {
        32
    } else if bytes_left >= 2 {
        16
    } else {
        8
    };
    (array.element_width - element_bit).min(native_bits - intra_byte)
}

/// Copy one static logical bit range between potentially different physical
/// array layouts.  Source and destination chunks are bounded independently:
/// for example, `logic<2>[4]` may be byte-strided at an external interface but
/// packed into one byte in an internal alias.
pub(super) fn emit_static_commit_plane(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    src_addr: &RegionedAbsoluteAddr,
    dst_addr: &RegionedAbsoluteAddr,
    bit_offset: usize,
    width: usize,
    mask_plane: bool,
) {
    if let (Some(src_size), Some(dst_size)) = (
        ctx.full_static_load_size(src_addr, bit_offset, width),
        ctx.full_static_store_size(dst_addr, bit_offset, width),
    ) && src_size == dst_size
    {
        // A complete logical object owns every byte in its physical slot.
        // Padding bits are not RTL state and no other object aliases them, so
        // copying the native storage unit directly is both exact and avoids
        // the load/mask/load/insert/store sequence used for partial ranges.
        let src_offset = if mask_plane {
            ctx.mask_byte_offset(src_addr, bit_offset)
        } else {
            ctx.byte_offset(src_addr, bit_offset)
        };
        let dst_offset = if mask_plane {
            ctx.mask_byte_offset(dst_addr, bit_offset)
        } else {
            ctx.byte_offset(dst_addr, bit_offset)
        };
        let value = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Load {
            dst: value,
            base: BaseReg::SimState,
            offset: src_offset,
            size: src_size,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: dst_offset,
            src: value,
            size: dst_size,
        });
        return;
    }

    let mut copied = 0usize;
    while copied < width {
        let part_bit_offset = bit_offset + copied;
        let (_, src_intra) = ctx.static_byte_and_intra(src_addr, part_bit_offset);
        let (_, dst_intra) = ctx.static_byte_and_intra(dst_addr, part_bit_offset);
        let mut part_width = (width - copied)
            .min(static_commit_chunk_capacity(ctx, src_addr, part_bit_offset))
            .min(static_commit_chunk_capacity(ctx, dst_addr, part_bit_offset));
        if src_intra == 0 && dst_intra == 0 {
            part_width = match part_width {
                64.. => 64,
                32.. => 32,
                16.. => 16,
                8.. => 8,
                _ => part_width,
            };
        }
        debug_assert!(part_width != 0);

        let src_size = ISelContext::op_size_for_width(src_intra + part_width);
        let dst_size = ISelContext::op_size_for_width(dst_intra + part_width);
        let containing_src = if mask_plane {
            ctx.mask_byte_offset(src_addr, part_bit_offset)
        } else {
            ctx.byte_offset(src_addr, part_bit_offset)
        };
        let containing_dst = if mask_plane {
            ctx.mask_byte_offset(dst_addr, part_bit_offset)
        } else {
            ctx.byte_offset(dst_addr, part_bit_offset)
        };

        if src_intra == 0
            && dst_intra == 0
            && let Some(size) = OpSize::from_bits(part_width)
        {
            let value = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: value,
                base: BaseReg::SimState,
                offset: containing_src,
                size,
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: containing_dst,
                src: value,
                size,
            });
            copied += part_width;
            continue;
        }

        let raw = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Load {
            dst: raw,
            base: BaseReg::SimState,
            offset: containing_src,
            size: src_size,
        });
        let shifted = if src_intra == 0 {
            raw
        } else {
            let shifted = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted,
                src: raw,
                imm: src_intra as u8,
            });
            shifted
        };
        let value = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, value, shifted, mask_for_width(part_width));

        let old = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Load {
            dst: old,
            base: BaseReg::SimState,
            offset: containing_dst,
            size: dst_size,
        });
        let new = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_bfi(
            block,
            new,
            old,
            value,
            dst_intra as u8,
            mask_for_width(part_width),
        );
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: containing_dst,
            src: new,
            size: dst_size,
        });

        copied += part_width;
    }
}

fn emit_single_chunk_sparse_insert(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    base: VReg,
    value: VReg,
    offset: &SIROffset,
    width: usize,
) -> VReg {
    let result = ctx.alloc_vreg(SpillDesc::transient());
    let value_mask = mask_for_width(width);
    match offset {
        SIROffset::Static(bit_offset) | SIROffset::PackedElements { bit_offset, .. } => {
            let (byte_offset, intra_byte) = ctx
                .layout
                .map_static_bit_offset(&addr.absolute_addr(), *bit_offset);
            let physical_bit_offset = byte_offset * 8 + intra_byte;
            debug_assert!(physical_bit_offset + width <= 64);
            ctx.emit_bfi(
                block,
                result,
                base,
                value,
                physical_bit_offset as u8,
                value_mask,
            );
        }
        SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
            let offset = memory_offset_vreg(ctx, block, addr, offset);
            let masked_value = if value_mask == u64::MAX {
                value
            } else {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, value, value_mask);
                masked
            };
            let shifted_value = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Shl {
                dst: shifted_value,
                lhs: masked_value,
                rhs: offset,
            });
            let unshifted_mask = ctx.alloc_vreg(SpillDesc::remat(value_mask));
            block.push(MInst::LoadImm {
                dst: unshifted_mask,
                value: value_mask,
            });
            let shifted_mask = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Shl {
                dst: shifted_mask,
                lhs: unshifted_mask,
                rhs: offset,
            });
            let inverse_mask = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: inverse_mask,
                src: shifted_mask,
            });
            let cleared = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: cleared,
                lhs: base,
                rhs: inverse_mask,
            });
            block.push(MInst::Or {
                dst: result,
                lhs: cleared,
                rhs: shifted_value,
            });
        }
    }
    result
}

pub(super) fn try_emit_single_chunk_sparse_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    width: usize,
    src_reg: RegisterId,
    triggers: &[crate::TriggerIdWithKind],
    comb_capture_sites: &[u32],
    write_state: SparseWriteState,
) -> bool {
    if addr.region != crate::SPARSE_WORKING_REGION
        || width == 0
        || width > 64
        || !triggers.is_empty()
        || !comb_capture_sites.is_empty()
    {
        return false;
    }
    let abs = addr.absolute_addr();
    let sparse = ctx.layout.sparse_layouts[&abs].clone();
    if sparse.chunk_count != 1 {
        return false;
    }

    if write_state != SparseWriteState::Active {
        emit_sparse_mark_active(ctx, block, &sparse);
    }
    let stable_base = ctx.layout.offsets[&abs] as i32;
    let sparse_base = (ctx.layout.sparse_base_offset + ctx.layout.sparse_offsets[&abs]) as i32;
    let byte_size = ctx.layout.plane_size(&abs) as i32;

    let was_dirty = (write_state == SparseWriteState::Unknown).then(|| {
        let dirty_bits = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Load {
            dst: dirty_bits,
            base: BaseReg::SimState,
            offset: sparse.dirty_words_offset as i32,
            size: OpSize::S64,
        });
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        let was_dirty = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: was_dirty,
            lhs: dirty_bits,
            rhs: zero,
            kind: CmpKind::Ne,
        });
        was_dirty
    });

    let value = ctx.reg_map.get(src_reg);
    let mask = ctx
        .is_4state_var(addr)
        .then(|| ctx.get_mask(src_reg, block));
    for (plane_delta, value) in [(0, value)]
        .into_iter()
        .chain(mask.into_iter().map(|mask| (byte_size, mask)))
    {
        let load_value = |ctx: &mut ISelContext, block: &mut MBlock, offset: i32| {
            let value = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: value,
                base: BaseReg::SimState,
                offset,
                size: OpSize::S64,
            });
            value
        };
        let initialized = match write_state {
            SparseWriteState::First => load_value(ctx, block, stable_base + plane_delta),
            SparseWriteState::Active => load_value(ctx, block, sparse_base + plane_delta),
            SparseWriteState::Unknown => {
                let stable = load_value(ctx, block, stable_base + plane_delta);
                let working = load_value(ctx, block, sparse_base + plane_delta);
                let initialized = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: initialized,
                    cond: was_dirty.expect("unknown sparse state tests the dirty bit"),
                    true_val: working,
                    false_val: stable,
                });
                initialized
            }
        };
        let new_value =
            emit_single_chunk_sparse_insert(ctx, block, addr, initialized, value, offset, width);
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: sparse_base + plane_delta,
            src: new_value,
            size: OpSize::S64,
        });
    }

    if write_state != SparseWriteState::Active {
        let one = ctx.alloc_vreg(SpillDesc::remat(1));
        block.push(MInst::LoadImm { dst: one, value: 1 });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: sparse.dirty_words_offset as i32,
            src: one,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: sparse.summary_words_offset as i32,
            src: one,
            size: OpSize::S64,
        });
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn prepare_sparse_clean_single_chunk(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    stable_base: i32,
    sparse_base: i32,
    byte_size: i32,
    sparse_plane_access_len: Option<usize>,
    dirty_words_offset: i32,
    summary_words_offset: i32,
    dirty_alias_range: Option<MemoryAliasRange>,
    summary_alias_range: Option<MemoryAliasRange>,
    write_state: SparseWriteState,
    dirty_word_state: SparseChunkState,
    metadata_action: SparseMetadataAction,
) {
    let bit_offset = memory_offset_vreg(ctx, block, addr, offset);
    let chunk = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: chunk,
        src: bit_offset,
        imm: 6,
    });

    let data_index = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShlImm {
        dst: data_index,
        src: chunk,
        imm: 3,
    });
    for plane_delta in [0, byte_size]
        .into_iter()
        .take(if ctx.is_4state_var(addr) { 2 } else { 1 })
    {
        let stable = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadIndexed {
            dst: stable,
            base: BaseReg::SimState,
            offset: stable_base + plane_delta,
            index: data_index,
            scale: 1,
            size: OpSize::S64,
            alias_range: sparse_plane_access_len
                .and_then(|byte_len| MemoryAliasRange::new(stable_base + plane_delta, byte_len)),
        });
        block.push(MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: sparse_base + plane_delta,
            index: data_index,
            src: stable,
            size: OpSize::S64,
            alias_range: sparse_plane_access_len
                .and_then(|byte_len| MemoryAliasRange::new(sparse_base + plane_delta, byte_len)),
        });
    }

    match metadata_action {
        SparseMetadataAction::Immediate => emit_sparse_metadata_update(
            ctx,
            block,
            chunk,
            dirty_words_offset,
            summary_words_offset,
            dirty_alias_range,
            summary_alias_range,
            write_state,
            dirty_word_state,
        ),
        SparseMetadataAction::Deferred => {}
        SparseMetadataAction::Batch {
            dirty_word,
            dirty_mask,
            initial_write_state,
            initial_dirty_word_state,
        } => emit_sparse_metadata_batch(
            ctx,
            block,
            dirty_words_offset,
            summary_words_offset,
            dirty_word,
            dirty_mask,
            initial_write_state,
            initial_dirty_word_state,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_sparse_metadata_update(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    chunk: VReg,
    dirty_words_offset: i32,
    summary_words_offset: i32,
    dirty_alias_range: Option<MemoryAliasRange>,
    summary_alias_range: Option<MemoryAliasRange>,
    write_state: SparseWriteState,
    dirty_word_state: SparseChunkState,
) {
    let dirty_word = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: dirty_word,
        src: chunk,
        imm: 6,
    });
    let dirty_index = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShlImm {
        dst: dirty_index,
        src: dirty_word,
        imm: 3,
    });
    let bit_in_word = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, bit_in_word, chunk, 63);
    let one = ctx.alloc_vreg(SpillDesc::remat(1));
    block.push(MInst::LoadImm { dst: one, value: 1 });
    let dirty_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Shl {
        dst: dirty_mask,
        lhs: one,
        rhs: bit_in_word,
    });

    let preserve_dirty_word =
        write_state == SparseWriteState::Active && dirty_word_state != SparseChunkState::Clean;
    if preserve_dirty_word {
        block.push(MInst::OrStoreIndexed {
            base: BaseReg::SimState,
            offset: dirty_words_offset,
            index: dirty_index,
            src: dirty_mask,
            size: OpSize::S64,
            alias_range: dirty_alias_range,
        });
    } else {
        block.push(MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: dirty_words_offset,
            index: dirty_index,
            src: dirty_mask,
            size: OpSize::S64,
            alias_range: dirty_alias_range,
        });
    }

    if write_state != SparseWriteState::Active || dirty_word_state != SparseChunkState::Dirty {
        let summary_word = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShrImm {
            dst: summary_word,
            src: dirty_word,
            imm: 6,
        });
        let summary_index = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShlImm {
            dst: summary_index,
            src: summary_word,
            imm: 3,
        });
        let summary_bit = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, summary_bit, dirty_word, 63);
        let summary_mask = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Shl {
            dst: summary_mask,
            lhs: one,
            rhs: summary_bit,
        });
        if write_state == SparseWriteState::Active {
            block.push(MInst::OrStoreIndexed {
                base: BaseReg::SimState,
                offset: summary_words_offset,
                index: summary_index,
                src: summary_mask,
                size: OpSize::S64,
                alias_range: summary_alias_range,
            });
        } else {
            block.push(MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: summary_words_offset,
                index: summary_index,
                src: summary_mask,
                size: OpSize::S64,
                alias_range: summary_alias_range,
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_sparse_metadata_batch(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dirty_words_offset: i32,
    summary_words_offset: i32,
    dirty_word: usize,
    dirty_mask: u64,
    initial_write_state: SparseWriteState,
    initial_dirty_word_state: SparseChunkState,
) {
    let dirty_word_offset = dirty_words_offset
        .checked_add(
            i32::try_from(
                dirty_word
                    .checked_mul(8)
                    .expect("dirty word offset overflow"),
            )
            .expect("dirty word offset exceeds MIR displacement"),
        )
        .expect("dirty word offset exceeds MIR displacement");
    let mask = ctx.alloc_vreg(SpillDesc::remat(dirty_mask));
    block.push(MInst::LoadImm {
        dst: mask,
        value: dirty_mask,
    });
    if initial_write_state == SparseWriteState::Active
        && initial_dirty_word_state != SparseChunkState::Clean
    {
        emit_sparse_or_store(ctx, block, dirty_word_offset, mask);
    } else {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: dirty_word_offset,
            src: mask,
            size: OpSize::S64,
        });
    }

    if initial_write_state == SparseWriteState::Active
        && initial_dirty_word_state == SparseChunkState::Dirty
    {
        return;
    }
    let summary_word = dirty_word / 64;
    let summary_word_offset = summary_words_offset
        .checked_add(
            i32::try_from(
                summary_word
                    .checked_mul(8)
                    .expect("summary word offset overflow"),
            )
            .expect("summary word offset exceeds MIR displacement"),
        )
        .expect("summary word offset exceeds MIR displacement");
    let summary_mask_value = 1u64 << (dirty_word % 64);
    let summary_mask = ctx.alloc_vreg(SpillDesc::remat(summary_mask_value));
    block.push(MInst::LoadImm {
        dst: summary_mask,
        value: summary_mask_value,
    });
    if initial_write_state == SparseWriteState::Active {
        emit_sparse_or_store(ctx, block, summary_word_offset, summary_mask);
    } else {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: summary_word_offset,
            src: summary_mask,
            size: OpSize::S64,
        });
    }
}

fn emit_sparse_or_store(ctx: &mut ISelContext, block: &mut MBlock, offset: i32, src: VReg) {
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    block.push(MInst::OrStoreIndexed {
        base: BaseReg::SimState,
        offset,
        index: zero,
        src,
        size: OpSize::S64,
        alias_range: MemoryAliasRange::new(offset, 8),
    });
}

pub(super) fn prepare_sparse_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    width: usize,
    write_state: SparseWriteState,
    chunk_state: SparseChunkState,
    dirty_word_state: SparseChunkState,
    metadata_action: SparseMetadataAction,
) {
    let abs = addr.absolute_addr();
    let sparse = ctx.layout.sparse_layouts[&abs].clone();
    if write_state != SparseWriteState::Active {
        emit_sparse_mark_active(ctx, block, &sparse);
    }
    let stable_base = ctx.layout.offsets[&abs] as i32;
    let sparse_base = (ctx.layout.sparse_base_offset + ctx.layout.sparse_offsets[&abs]) as i32;
    let plane_size = ctx.layout.plane_size(&abs);
    let byte_size = plane_size as i32;
    let sparse_plane_access_len = plane_size.checked_add(7).map(|size| size & !7);
    let dirty_alias_range = sparse
        .dirty_word_count
        .checked_mul(8)
        .and_then(|byte_len| MemoryAliasRange::new(sparse.dirty_words_offset as i32, byte_len));
    let summary_alias_range = sparse
        .summary_word_count
        .checked_mul(8)
        .and_then(|byte_len| MemoryAliasRange::new(sparse.summary_words_offset as i32, byte_len));

    // A value that fits in one 64-bit chunk has no dynamic sparse-metadata
    // indexing at all: every valid store touches chunk zero, dirty word zero,
    // and summary bit zero.  This is common for arrays of one-bit FF fields.
    // Keeping the generic chunk calculation here used to turn every one-bit
    // store into roughly a dozen unnecessary MIR operations.
    if sparse.chunk_count == 1 {
        if write_state == SparseWriteState::Active {
            return;
        }
        let was_dirty = (write_state == SparseWriteState::Unknown).then(|| {
            let dirty_bits = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: dirty_bits,
                base: BaseReg::SimState,
                offset: sparse.dirty_words_offset as i32,
                size: OpSize::S64,
            });
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let was_dirty = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: was_dirty,
                lhs: dirty_bits,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            was_dirty
        });

        for plane_delta in
            [0, byte_size]
                .into_iter()
                .take(if ctx.is_4state_var(addr) { 2 } else { 1 })
        {
            let stable = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: stable,
                base: BaseReg::SimState,
                offset: stable_base + plane_delta,
                size: OpSize::S64,
            });
            let initialized = if let Some(was_dirty) = was_dirty {
                let working = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Load {
                    dst: working,
                    base: BaseReg::SimState,
                    offset: sparse_base + plane_delta,
                    size: OpSize::S64,
                });
                let initialized = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: initialized,
                    cond: was_dirty,
                    true_val: working,
                    false_val: stable,
                });
                initialized
            } else {
                stable
            };
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: sparse_base + plane_delta,
                src: initialized,
                size: OpSize::S64,
            });
        }

        let one = ctx.alloc_vreg(SpillDesc::remat(1));
        block.push(MInst::LoadImm { dst: one, value: 1 });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: sparse.dirty_words_offset as i32,
            src: one,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: sparse.summary_words_offset as i32,
            src: one,
            size: OpSize::S64,
        });
        return;
    }

    let max_chunks = match offset {
        SIROffset::Static(value)
        | SIROffset::PackedElements {
            bit_offset: value, ..
        } => ((value % 64) + width).div_ceil(64),
        SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
            let zero_bits = memory_offset_low_zero_bits(ctx, addr, offset).min(6);
            let alignment = 1usize << zero_bits;
            (width + (64 - alignment)).div_ceil(64)
        }
    };
    if write_state == SparseWriteState::Active && chunk_state == SparseChunkState::Dirty {
        return;
    }
    let clean_single_chunk = metadata_action != SparseMetadataAction::Immediate
        || write_state == SparseWriteState::First && max_chunks == 1
        || write_state == SparseWriteState::Active && chunk_state == SparseChunkState::Clean;
    if clean_single_chunk {
        prepare_sparse_clean_single_chunk(
            ctx,
            block,
            addr,
            offset,
            stable_base,
            sparse_base,
            byte_size,
            sparse_plane_access_len,
            sparse.dirty_words_offset as i32,
            sparse.summary_words_offset as i32,
            dirty_alias_range,
            summary_alias_range,
            write_state,
            dirty_word_state,
            metadata_action,
        );
        return;
    }
    debug_assert_eq!(metadata_action, SparseMetadataAction::Immediate);

    let bit_offset = memory_offset_vreg(ctx, block, addr, offset);
    let start_chunk = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: start_chunk,
        src: bit_offset,
        imm: 6,
    });
    let width_minus_one = ctx.alloc_vreg(SpillDesc::remat(width.saturating_sub(1) as u64));
    block.push(MInst::LoadImm {
        dst: width_minus_one,
        value: width.saturating_sub(1) as u64,
    });
    let end_bit = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Add {
        dst: end_bit,
        lhs: bit_offset,
        rhs: width_minus_one,
    });
    let end_chunk = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: end_chunk,
        src: end_bit,
        imm: 6,
    });

    for chunk_delta in 0..max_chunks {
        let delta = ctx.alloc_vreg(SpillDesc::remat(chunk_delta as u64));
        block.push(MInst::LoadImm {
            dst: delta,
            value: chunk_delta as u64,
        });
        let candidate = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Add {
            dst: candidate,
            lhs: start_chunk,
            rhs: delta,
        });
        let valid = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: valid,
            lhs: candidate,
            rhs: end_chunk,
            kind: CmpKind::LeU,
        });
        let chunk = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: chunk,
            cond: valid,
            true_val: candidate,
            false_val: start_chunk,
        });

        let dirty_word = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShrImm {
            dst: dirty_word,
            src: chunk,
            imm: 6,
        });
        let eight = ctx.alloc_vreg(SpillDesc::remat(8));
        block.push(MInst::LoadImm {
            dst: eight,
            value: 8,
        });
        let dirty_index = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Mul {
            dst: dirty_index,
            lhs: dirty_word,
            rhs: eight,
        });
        let dirty_bits = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadIndexed {
            dst: dirty_bits,
            base: BaseReg::SimState,
            offset: sparse.dirty_words_offset as i32,
            index: dirty_index,
            scale: 1,
            size: OpSize::S64,
            alias_range: dirty_alias_range,
        });
        let bit_in_word = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, bit_in_word, chunk, 63);
        let one = ctx.alloc_vreg(SpillDesc::remat(1));
        block.push(MInst::LoadImm { dst: one, value: 1 });
        let dirty_mask = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Shl {
            dst: dirty_mask,
            lhs: one,
            rhs: bit_in_word,
        });
        let dirty_test = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: dirty_test,
            lhs: dirty_bits,
            rhs: dirty_mask,
        });
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        let was_dirty = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: was_dirty,
            lhs: dirty_test,
            rhs: zero,
            kind: CmpKind::Ne,
        });

        let data_index = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Mul {
            dst: data_index,
            lhs: chunk,
            rhs: eight,
        });
        for plane_delta in
            [0, byte_size]
                .into_iter()
                .take(if ctx.is_4state_var(addr) { 2 } else { 1 })
        {
            let stable = ctx.alloc_vreg(SpillDesc::transient());
            let working = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::LoadIndexed {
                dst: stable,
                base: BaseReg::SimState,
                offset: stable_base + plane_delta,
                index: data_index,
                scale: 1,
                size: OpSize::S64,
                alias_range: sparse_plane_access_len.and_then(|byte_len| {
                    MemoryAliasRange::new(stable_base + plane_delta, byte_len)
                }),
            });
            block.push(MInst::LoadIndexed {
                dst: working,
                base: BaseReg::SimState,
                offset: sparse_base + plane_delta,
                index: data_index,
                scale: 1,
                size: OpSize::S64,
                alias_range: sparse_plane_access_len.and_then(|byte_len| {
                    MemoryAliasRange::new(sparse_base + plane_delta, byte_len)
                }),
            });
            let initialized = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: initialized,
                cond: was_dirty,
                true_val: working,
                false_val: stable,
            });
            block.push(MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: sparse_base + plane_delta,
                index: data_index,
                src: initialized,
                size: OpSize::S64,
                alias_range: sparse_plane_access_len.and_then(|byte_len| {
                    MemoryAliasRange::new(sparse_base + plane_delta, byte_len)
                }),
            });
        }

        let new_dirty = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: new_dirty,
            lhs: dirty_bits,
            rhs: dirty_mask,
        });
        block.push(MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: sparse.dirty_words_offset as i32,
            index: dirty_index,
            src: new_dirty,
            size: OpSize::S64,
            alias_range: dirty_alias_range,
        });

        let summary_word = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShrImm {
            dst: summary_word,
            src: dirty_word,
            imm: 6,
        });
        let summary_index = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Mul {
            dst: summary_index,
            lhs: summary_word,
            rhs: eight,
        });
        let summary_bits = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadIndexed {
            dst: summary_bits,
            base: BaseReg::SimState,
            offset: sparse.summary_words_offset as i32,
            index: summary_index,
            scale: 1,
            size: OpSize::S64,
            alias_range: summary_alias_range,
        });
        let summary_bit = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, summary_bit, dirty_word, 63);
        let summary_mask = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Shl {
            dst: summary_mask,
            lhs: one,
            rhs: summary_bit,
        });
        let new_summary = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: new_summary,
            lhs: summary_bits,
            rhs: summary_mask,
        });
        block.push(MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: sparse.summary_words_offset as i32,
            index: summary_index,
            src: new_summary,
            size: OpSize::S64,
            alias_range: summary_alias_range,
        });
    }
}

pub(super) fn emit_aligned_dynamic_wide_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    base_offset: i32,
    byte_offset: VReg,
    width: usize,
    alias_range: Option<MemoryAliasRange>,
    chunks: &[(VReg, usize)],
) {
    let mut bit_pos = 0usize;
    let mut remaining = width;

    for &(chunk, chunk_width) in chunks {
        if remaining == 0 {
            break;
        }
        let logical_width = chunk_width.min(remaining);
        debug_assert!(bit_pos.is_multiple_of(8));

        let whole_bytes = logical_width / 8;
        let mut copied = 0usize;
        for bytes in [8usize, 4, 2, 1] {
            while copied + bytes <= whole_bytes {
                let consumed_bits = copied * 8;
                let src = if consumed_bits == 0 {
                    chunk
                } else {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: shifted,
                        src: chunk,
                        imm: consumed_bits as u8,
                    });
                    shifted
                };
                block.push(MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset: base_offset + ((bit_pos / 8) + copied) as i32,
                    index: byte_offset,
                    src,
                    size: match bytes {
                        8 => OpSize::S64,
                        4 => OpSize::S32,
                        2 => OpSize::S16,
                        1 => OpSize::S8,
                        _ => unreachable!(),
                    },
                    alias_range,
                });
                copied += bytes;
            }
        }

        let tail_bits = logical_width % 8;
        if tail_bits != 0 {
            let consumed_bits = whole_bytes * 8;
            let src = if consumed_bits == 0 {
                chunk
            } else {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: shifted,
                    src: chunk,
                    imm: consumed_bits as u8,
                });
                shifted
            };
            let offset = base_offset + ((bit_pos / 8) + whole_bytes) as i32;
            let old = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::LoadIndexed {
                dst: old,
                base: BaseReg::SimState,
                offset,
                index: byte_offset,
                scale: 1,
                size: OpSize::S8,
                alias_range,
            });
            let new = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_bfi(block, new, old, src, 0, mask_for_width(tail_bits));
            block.push(MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset,
                index: byte_offset,
                src: new,
                size: OpSize::S8,
                alias_range,
            });
        }

        bit_pos += logical_width;
        remaining -= logical_width;
    }

    debug_assert_eq!(remaining, 0, "wide source does not cover store width");
}

pub(super) fn emit_dynamic_scalar_bitfield_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    base_offset: i32,
    byte_offset: VReg,
    bit_shift: VReg,
    src: VReg,
    width: usize,
    alias_range: Option<MemoryAliasRange>,
    track_change: bool,
) -> Option<VReg> {
    let width_mask = mask_for_width(width);
    let masked_src = ctx.alloc_vreg(SpillDesc::transient());
    if width_mask == u64::MAX {
        ctx.emit_mov(block, masked_src, src);
    } else {
        ctx.emit_and_imm(block, masked_src, src, width_mask);
    }

    let old_low = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::LoadIndexed {
        dst: old_low,
        base: BaseReg::SimState,
        offset: base_offset,
        index: byte_offset,
        scale: 1,
        size: ISelContext::op_size_for_width(width + 7),
        alias_range,
    });
    let shifted_src = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Shl {
        dst: shifted_src,
        lhs: masked_src,
        rhs: bit_shift,
    });
    let mask_value = ctx.alloc_vreg(SpillDesc::remat(width_mask));
    block.push(MInst::LoadImm {
        dst: mask_value,
        value: width_mask,
    });
    let shifted_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Shl {
        dst: shifted_mask,
        lhs: mask_value,
        rhs: bit_shift,
    });
    let inverted_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::BitNot {
        dst: inverted_mask,
        src: shifted_mask,
    });
    let cleared_low = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::And {
        dst: cleared_low,
        lhs: old_low,
        rhs: inverted_mask,
    });
    let new_low = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: new_low,
        lhs: cleared_low,
        rhs: shifted_src,
    });
    block.push(MInst::StoreIndexed {
        base: BaseReg::SimState,
        offset: base_offset,
        index: byte_offset,
        src: new_low,
        size: ISelContext::op_size_for_width(width + 7),
        alias_range,
    });

    let mut changed = track_change.then(|| {
        let changed = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: changed,
            lhs: old_low,
            rhs: new_low,
            kind: CmpKind::Ne,
        });
        changed
    });
    if width + 7 <= 64 {
        return changed;
    }

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let sixty_four = ctx.alloc_vreg(SpillDesc::remat(64));
    block.push(MInst::LoadImm {
        dst: sixty_four,
        value: 64,
    });
    let inverse_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Sub {
        dst: inverse_shift,
        lhs: sixty_four,
        rhs: bit_shift,
    });
    let inverse_shift_mod = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, inverse_shift_mod, inverse_shift, 63);
    let has_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_shift,
        lhs: bit_shift,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let high_src_raw = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Shr {
        dst: high_src_raw,
        lhs: masked_src,
        rhs: inverse_shift_mod,
    });
    let high_mask_raw = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Shr {
        dst: high_mask_raw,
        lhs: mask_value,
        rhs: inverse_shift_mod,
    });
    let high_src = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: high_src,
        cond: has_shift,
        true_val: high_src_raw,
        false_val: zero,
    });
    let high_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: high_mask,
        cond: has_shift,
        true_val: high_mask_raw,
        false_val: zero,
    });
    let old_high = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::LoadIndexed {
        dst: old_high,
        base: BaseReg::SimState,
        offset: base_offset + 8,
        index: byte_offset,
        scale: 1,
        size: OpSize::S8,
        alias_range,
    });
    let inverted_high_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::BitNot {
        dst: inverted_high_mask,
        src: high_mask,
    });
    let cleared_high = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::And {
        dst: cleared_high,
        lhs: old_high,
        rhs: inverted_high_mask,
    });
    let new_high = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: new_high,
        lhs: cleared_high,
        rhs: high_src,
    });
    block.push(MInst::StoreIndexed {
        base: BaseReg::SimState,
        offset: base_offset + 8,
        index: byte_offset,
        src: new_high,
        size: OpSize::S8,
        alias_range,
    });
    if track_change {
        let high_changed = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: high_changed,
            lhs: old_high,
            rhs: new_high,
            kind: CmpKind::Ne,
        });
        let any_changed = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: any_changed,
            lhs: changed.expect("low change was requested"),
            rhs: high_changed,
        });
        changed = Some(any_changed);
    }
    changed
}

pub(super) fn emit_dynamic_wide_bitfield_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    base_offset: i32,
    byte_offset: VReg,
    bit_shift: VReg,
    width: usize,
    alias_range: Option<MemoryAliasRange>,
    chunks: &[(VReg, usize)],
    track_change: bool,
) -> Option<VReg> {
    let mut remaining = width;
    let mut bit_pos = 0usize;
    let mut changed = None;
    for &(chunk, chunk_width) in chunks {
        if remaining == 0 {
            break;
        }
        let logical_width = chunk_width.min(remaining).min(64);
        let chunk_changed = emit_dynamic_scalar_bitfield_store(
            ctx,
            block,
            base_offset + (bit_pos / 8) as i32,
            byte_offset,
            bit_shift,
            chunk,
            logical_width,
            alias_range,
            track_change,
        );
        changed = match (changed, chunk_changed) {
            (None, next) => next,
            (Some(previous), Some(next)) => {
                let merged = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: merged,
                    lhs: previous,
                    rhs: next,
                });
                Some(merged)
            }
            (previous, None) => previous,
        };
        bit_pos += logical_width;
        remaining -= logical_width;
    }
    changed
}

pub(super) fn lower_block_cached_dynamic_load(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    destination: RegisterId,
    address: RegionedAbsoluteAddr,
    offset: &SIROffset,
    width: usize,
    cache: &mut HashMap<RegionedAbsoluteAddr, BlockDynamicLoadCacheEntry>,
) {
    let entry = if let Some(&entry) = cache.get(&address) {
        entry
    } else {
        let absolute = address.absolute_addr();
        let byte_size = ctx.layout.plane_size(&absolute);
        let size = native_plane_access_size(byte_size)
            .expect("planned block-local state plane has a native access size");
        let logical_width = ctx.layout.widths[&absolute];
        let value = ctx.alloc_vreg(SpillDesc::sim_state(address, 0, logical_width, false));
        block.push(MInst::Load {
            dst: value,
            base: BaseReg::SimState,
            offset: ctx.byte_offset(&address, 0),
            size,
        });
        ctx.known_bits.insert(value, logical_width);
        let mask = ctx.is_4state_var(&address).then(|| {
            let mask = ctx.alloc_vreg(SpillDesc::sim_state(address, 0, logical_width, true));
            block.push(MInst::Load {
                dst: mask,
                base: BaseReg::SimState,
                offset: ctx.mask_byte_offset(&address, 0),
                size,
            });
            ctx.known_bits.insert(mask, logical_width);
            mask
        });
        let entry = BlockDynamicLoadCacheEntry { value, mask };
        cache.insert(address, entry);
        entry
    };
    let shift = memory_offset_vreg(ctx, block, &address, offset);

    let destination_vreg = ctx.reg_map.get(destination);
    let shifted = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Shr {
        dst: shifted,
        lhs: entry.value,
        rhs: shift,
    });
    if width < 64 {
        ctx.emit_and_imm(block, destination_vreg, shifted, mask_for_width(width));
    } else {
        ctx.emit_mov(block, destination_vreg, shifted);
    }
    ctx.known_bits.insert(destination_vreg, width);
    ctx.reg_addrs.remove(&destination);

    if let Some(mask) = entry.mask {
        let shifted_mask = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Shr {
            dst: shifted_mask,
            lhs: mask,
            rhs: shift,
        });
        let result_mask = if width < 64 {
            let result = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, result, shifted_mask, mask_for_width(width));
            result
        } else {
            shifted_mask
        };
        ctx.set_mask(destination, result_mask);
    } else if ctx.four_state {
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        ctx.set_mask(destination, zero);
    }
}

pub(super) fn lower_dynamic_wide_load_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    base_off: i32,
    byte_off: VReg,
    offset_vreg: VReg,
    offset_low_zero_bits: u32,
    width_bits: usize,
    alias_range: Option<MemoryAliasRange>,
) -> Vec<(VReg, usize)> {
    let n_chunks = ISelContext::num_chunks(width_bits);
    let mut chunks = Vec::with_capacity(n_chunks);

    if offset_low_zero_bits >= 3 {
        let mut remaining = width_bits;
        let mut bit_pos = 0usize;
        while remaining > 0 {
            let chunk_bits = remaining.min(64);
            let chunk = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::LoadIndexed {
                dst: chunk,
                base: BaseReg::SimState,
                offset: base_off + (bit_pos / 8) as i32,
                index: byte_off,
                scale: 1,
                size: ISelContext::op_size_for_width(chunk_bits),
                alias_range,
            });
            chunks.push((chunk, chunk_bits));
            bit_pos += chunk_bits;
            remaining -= chunk_bits;
        }
        return chunks;
    }

    let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, bit_shift, offset_vreg, 7);

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let sixty_four = ctx.alloc_vreg(SpillDesc::remat(64));
    block.push(MInst::LoadImm {
        dst: sixty_four,
        value: 64,
    });
    let inv_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Sub {
        dst: inv_shift,
        lhs: sixty_four,
        rhs: bit_shift,
    });
    let inv_shift_mod = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, inv_shift_mod, inv_shift, 63);
    let has_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_shift,
        lhs: bit_shift,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    ctx.known_bits.insert(has_shift, 1);

    let mut remaining = width_bits;
    let mut bit_pos = 0usize;
    while remaining > 0 {
        let chunk_bits = remaining.min(64);
        let byte_delta = (bit_pos / 8) as i32;
        let lo = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadIndexed {
            dst: lo,
            base: BaseReg::SimState,
            offset: base_off + byte_delta,
            index: byte_off,
            scale: 1,
            size: OpSize::S64,
            alias_range,
        });
        let lo_shifted = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Shr {
            dst: lo_shifted,
            lhs: lo,
            rhs: bit_shift,
        });

        let hi = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadIndexed {
            dst: hi,
            base: BaseReg::SimState,
            offset: base_off + byte_delta + 8,
            index: byte_off,
            scale: 1,
            size: OpSize::S8,
            alias_range,
        });
        let hi_shifted_raw = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Shl {
            dst: hi_shifted_raw,
            lhs: hi,
            rhs: inv_shift_mod,
        });
        let hi_shifted = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: hi_shifted,
            cond: has_shift,
            true_val: hi_shifted_raw,
            false_val: zero,
        });

        let combined = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: combined,
            lhs: lo_shifted,
            rhs: hi_shifted,
        });
        let chunk = if chunk_bits < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, combined, mask_for_width(chunk_bits));
            masked
        } else {
            combined
        };
        chunks.push((chunk, chunk_bits));

        bit_pos += chunk_bits;
        remaining -= chunk_bits;
    }

    chunks
}

// ────────────────────────────────────────────────────────────────
// 4-state mask computation
// ────────────────────────────────────────────────────────────────

pub(super) fn lower_static_wide_load_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    base_off: i32,
    bit_offset: usize,
    width_bits: usize,
) -> Vec<(VReg, usize)> {
    let n_chunks = ISelContext::num_chunks(width_bits).max(1);
    let mut chunks = Vec::with_capacity(n_chunks);
    let intra = bit_offset % 8;
    let first_byte = base_off + (bit_offset / 8) as i32;

    for index in 0..n_chunks {
        let chunk_bits = width_bits.saturating_sub(index * 64).min(64);
        let byte_off = first_byte + (index * 8) as i32;
        let needed_bits = chunk_bits + intra;
        let combined = if needed_bits <= 64 {
            let raw = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: raw,
                base: BaseReg::SimState,
                offset: byte_off,
                size: ISelContext::op_size_for_width(needed_bits),
            });
            if intra == 0 {
                raw
            } else {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: shifted,
                    src: raw,
                    imm: intra as u8,
                });
                shifted
            }
        } else {
            let low = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: low,
                base: BaseReg::SimState,
                offset: byte_off,
                size: OpSize::S64,
            });
            let shifted_low = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted_low,
                src: low,
                imm: intra as u8,
            });
            let high = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: high,
                base: BaseReg::SimState,
                offset: byte_off + 8,
                size: OpSize::S8,
            });
            let shifted_high = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShlImm {
                dst: shifted_high,
                src: high,
                imm: (64 - intra) as u8,
            });
            let combined = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: combined,
                lhs: shifted_low,
                rhs: shifted_high,
            });
            combined
        };
        let value = if chunk_bits < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, combined, mask_for_width(chunk_bits));
            masked
        } else {
            combined
        };
        chunks.push((value, chunk_bits));
    }
    chunks
}
