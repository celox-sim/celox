//! Runtime event writes and combinational capture notifications.

use super::*;

pub(super) fn load_runtime_event_ptr(ctx: &mut ISelContext, block: &mut MBlock) -> VReg {
    let event_ptr = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Load {
        dst: event_ptr,
        base: BaseReg::SimState,
        offset: celox_state_layout::STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET as i32,
        size: OpSize::S64,
    });
    event_ptr
}

pub(super) fn load_runtime_event_ptr_and_comb_capture_enabled(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    site_id: u32,
) -> (VReg, VReg) {
    let event_ptr = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Load {
        dst: event_ptr,
        base: BaseReg::SimState,
        offset: celox_state_layout::STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET as i32,
        size: OpSize::S64,
    });
    let enabled_ptr = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Load {
        dst: enabled_ptr,
        base: BaseReg::SimState,
        offset: celox_state_layout::STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET as i32,
        size: OpSize::S64,
    });
    let enabled_byte = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::LoadPtr {
        dst: enabled_byte,
        ptr: enabled_ptr,
        offset: site_id as i32,
        size: OpSize::S8,
    });
    let enabled = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::CmpImm {
        dst: enabled,
        lhs: enabled_byte,
        imm: 0,
        kind: CmpKind::Ne,
    });
    (event_ptr, enabled)
}

pub(super) fn emit_enable_comb_capture_sites(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    changed: VReg,
    site_ids: &[u32],
) {
    let enabled_ptr = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Load {
        dst: enabled_ptr,
        base: BaseReg::SimState,
        offset: celox_state_layout::STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET as i32,
        size: OpSize::S64,
    });
    let one = ctx.alloc_vreg(SpillDesc::remat(1));
    block.push(MInst::LoadImm { dst: one, value: 1 });
    for &site_id in site_ids {
        let old = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadPtr {
            dst: old,
            ptr: enabled_ptr,
            offset: site_id as i32,
            size: OpSize::S8,
        });
        let next = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: next,
            cond: changed,
            true_val: one,
            false_val: old,
        });
        block.push(MInst::StorePtr {
            ptr: enabled_ptr,
            offset: site_id as i32,
            src: next,
            size: OpSize::S8,
        });
    }
}

pub(super) fn emit_enable_comb_capture_sites_if_regs_changed(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    old: RegisterId,
    new: RegisterId,
    site_ids: &[u32],
) {
    if site_ids.is_empty() {
        return;
    }

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let mut changed = zero;

    if ctx.sir_width(&old) > 64 || ctx.sir_width(&new) > 64 {
        let old_chunks = ctx.get_wide_chunks(&old, block);
        let new_chunks = ctx.get_wide_chunks(&new, block);
        let chunk_count = old_chunks.len().max(new_chunks.len());
        let compare_width = ctx.sir_width(&old).max(ctx.sir_width(&new));
        for idx in 0..chunk_count {
            let old_chunk = ctx.wide_chunk_or_zero(&old_chunks, idx, block);
            let new_chunk = ctx.wide_chunk_or_zero(&new_chunks, idx, block);
            let chunk_width = compare_width.saturating_sub(idx * 64).min(64);
            let (old_cmp, new_cmp) = if chunk_width < 64 {
                let masked_old = ctx.alloc_vreg(SpillDesc::transient());
                let masked_new = ctx.alloc_vreg(SpillDesc::transient());
                let mask = mask_for_width(chunk_width);
                ctx.emit_and_imm(block, masked_old, old_chunk, mask);
                ctx.emit_and_imm(block, masked_new, new_chunk, mask);
                (masked_old, masked_new)
            } else {
                (old_chunk, new_chunk)
            };
            let chunk_changed = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: chunk_changed,
                lhs: old_cmp,
                rhs: new_cmp,
                kind: CmpKind::Ne,
            });
            let next = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: next,
                lhs: changed,
                rhs: chunk_changed,
            });
            changed = next;
        }

        if ctx.four_state {
            let old_masks = get_wide_mask_chunks(ctx, block, &old, chunk_count);
            let new_masks = get_wide_mask_chunks(ctx, block, &new, chunk_count);
            for (idx, (old_mask, new_mask)) in old_masks.into_iter().zip(new_masks).enumerate() {
                let chunk_width = compare_width.saturating_sub(idx * 64).min(64);
                let (old_cmp, new_cmp) = if chunk_width < 64 {
                    let masked_old = ctx.alloc_vreg(SpillDesc::transient());
                    let masked_new = ctx.alloc_vreg(SpillDesc::transient());
                    let mask = mask_for_width(chunk_width);
                    ctx.emit_and_imm(block, masked_old, old_mask, mask);
                    ctx.emit_and_imm(block, masked_new, new_mask, mask);
                    (masked_old, masked_new)
                } else {
                    (old_mask, new_mask)
                };
                let mask_changed = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: mask_changed,
                    lhs: old_cmp,
                    rhs: new_cmp,
                    kind: CmpKind::Ne,
                });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: next,
                    lhs: changed,
                    rhs: mask_changed,
                });
                changed = next;
            }
        }
    } else {
        let value_changed = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: value_changed,
            lhs: ctx.reg_map.get(old),
            rhs: ctx.reg_map.get(new),
            kind: CmpKind::Ne,
        });
        changed = value_changed;

        if ctx.four_state {
            let old_mask = ctx.get_mask(old, block);
            let new_mask = ctx.get_mask(new, block);
            let mask_changed = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: mask_changed,
                lhs: old_mask,
                rhs: new_mask,
                kind: CmpKind::Ne,
            });
            let next = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: next,
                lhs: changed,
                rhs: mask_changed,
            });
            changed = next;
        }
    }

    emit_enable_comb_capture_sites(ctx, block, changed, site_ids);
}

pub(super) fn collect_static_comb_store_byte_probes(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    bit_offset: usize,
    width_bits: usize,
    mask_region: bool,
) -> Vec<(VReg, i32, OpSize)> {
    let (value_start, intra) = ctx.static_byte_and_intra(addr, bit_offset);
    let start = if mask_region {
        value_start + ctx.layout.plane_size(&addr.absolute_addr()) as i32
    } else {
        value_start
    };
    let byte_len = (intra + width_bits).div_ceil(8);
    let mut probes = Vec::new();
    let mut byte_pos = 0usize;

    while byte_pos < byte_len {
        let remaining = byte_len - byte_pos;
        let size = if remaining >= 8 {
            OpSize::S64
        } else if remaining >= 4 {
            OpSize::S32
        } else if remaining >= 2 {
            OpSize::S16
        } else {
            OpSize::S8
        };
        let byte_off = start + byte_pos as i32;
        let old = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Load {
            dst: old,
            base: BaseReg::SimState,
            offset: byte_off,
            size,
        });
        probes.push((old, byte_off, size));
        byte_pos += size.bytes() as usize;
    }

    probes
}

pub(super) fn emit_enable_comb_capture_sites_if_byte_probes_changed(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    probes: Vec<(VReg, i32, OpSize)>,
    site_ids: &[u32],
) {
    if probes.is_empty() {
        return;
    }

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let mut changed = zero;
    for (old, byte_off, size) in probes {
        let new = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Load {
            dst: new,
            base: BaseReg::SimState,
            offset: byte_off,
            size,
        });
        let chunk_changed = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: chunk_changed,
            lhs: old,
            rhs: new,
            kind: CmpKind::Ne,
        });
        let next_changed = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next_changed,
            lhs: changed,
            rhs: chunk_changed,
        });
        changed = next_changed;
    }
    emit_enable_comb_capture_sites(ctx, block, changed, site_ids);
}

pub(super) fn lower_runtime_event_write(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    event_ptr: VReg,
    site_id: u32,
    args: &[RegisterId],
) {
    use celox_state_layout::{
        RUNTIME_EVENT_HEADER_SIZE, RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
        RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET, RUNTIME_EVENT_SLOT_SEQ_OFFSET,
        RUNTIME_EVENT_SLOT_SITE_OFFSET, RUNTIME_EVENT_WRITING,
    };

    let seq_v = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::LoadPtr {
        dst: seq_v,
        ptr: event_ptr,
        offset: 0,
        size: OpSize::S64,
    });
    let mask_v = ctx.alloc_vreg(SpillDesc::remat(
        (ctx.layout.runtime_event_capacity as u64) - 1,
    ));
    block.push(MInst::LoadImm {
        dst: mask_v,
        value: (ctx.layout.runtime_event_capacity as u64) - 1,
    });
    let slot_idx = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::And {
        dst: slot_idx,
        lhs: seq_v,
        rhs: mask_v,
    });
    let slot_size_v = ctx.alloc_vreg(SpillDesc::remat(ctx.layout.runtime_event_slot_size as u64));
    block.push(MInst::LoadImm {
        dst: slot_size_v,
        value: ctx.layout.runtime_event_slot_size as u64,
    });
    let slot_off = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Mul {
        dst: slot_off,
        lhs: slot_idx,
        rhs: slot_size_v,
    });

    let writing = ctx.alloc_vreg(SpillDesc::remat(RUNTIME_EVENT_WRITING));
    block.push(MInst::LoadImm {
        dst: writing,
        value: RUNTIME_EVENT_WRITING,
    });
    let slot_base = RUNTIME_EVENT_HEADER_SIZE as i32;
    block.push(MInst::ReleaseStorePtrIndexed {
        ptr: event_ptr,
        offset: slot_base + RUNTIME_EVENT_SLOT_SEQ_OFFSET as i32,
        index: slot_off,
        src: writing,
        size: OpSize::S64,
    });
    let site_v = ctx.alloc_vreg(SpillDesc::remat(site_id as u64));
    block.push(MInst::LoadImm {
        dst: site_v,
        value: site_id as u64,
    });
    block.push(MInst::StorePtrIndexed {
        ptr: event_ptr,
        offset: slot_base + RUNTIME_EVENT_SLOT_SITE_OFFSET as i32,
        index: slot_off,
        src: site_v,
        size: OpSize::S64,
    });
    let site_layout = &ctx.layout.runtime_event_site_layouts[site_id as usize];
    let arg_count = args.len() as u64;
    let arg_count_v = ctx.alloc_vreg(SpillDesc::remat(arg_count));
    block.push(MInst::LoadImm {
        dst: arg_count_v,
        value: arg_count,
    });
    block.push(MInst::StorePtrIndexed {
        ptr: event_ptr,
        offset: slot_base + RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET as i32,
        index: slot_off,
        src: arg_count_v,
        size: OpSize::S64,
    });
    for (idx, arg) in args.iter().enumerate() {
        let Some(arg_layout) = site_layout.args.get(idx) else {
            continue;
        };
        let value_chunks = if ctx.wide_regs.contains_key(arg) {
            ctx.get_wide_chunks(arg, block)
        } else {
            vec![(ctx.reg_map.get(*arg), ctx.sir_width(arg).min(64))]
        };
        let mask_chunks = if ctx.wide_regs.contains_key(arg) {
            get_wide_mask_chunks(ctx, block, arg, arg_layout.word_count)
        } else {
            vec![ctx.get_mask(*arg, block)]
        };
        for word_idx in 0..arg_layout.word_count {
            let value_vreg = value_chunks
                .get(word_idx)
                .map(|chunk| chunk.0)
                .unwrap_or_else(|| {
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    zero
                });
            block.push(MInst::StorePtrIndexed {
                ptr: event_ptr,
                offset: slot_base
                    + (RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                        + (arg_layout.value_word_offset + word_idx) * 8)
                        as i32,
                index: slot_off,
                src: value_vreg,
                size: OpSize::S64,
            });

            let mask_vreg = mask_chunks.get(word_idx).copied().unwrap_or_else(|| {
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero,
                    value: 0,
                });
                zero
            });
            block.push(MInst::StorePtrIndexed {
                ptr: event_ptr,
                offset: slot_base
                    + (RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                        + (arg_layout.mask_word_offset + word_idx) * 8)
                        as i32,
                index: slot_off,
                src: mask_vreg,
                size: OpSize::S64,
            });
        }
    }
    block.push(MInst::ReleaseStorePtrIndexed {
        ptr: event_ptr,
        offset: slot_base + RUNTIME_EVENT_SLOT_SEQ_OFFSET as i32,
        index: slot_off,
        src: seq_v,
        size: OpSize::S64,
    });
    let next_seq = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::AddImm {
        dst: next_seq,
        src: seq_v,
        imm: 1,
    });
    block.push(MInst::ReleaseStorePtr {
        ptr: event_ptr,
        offset: 0,
        src: next_seq,
        size: OpSize::S64,
    });
}
