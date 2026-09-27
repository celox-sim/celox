//! SIR instruction dispatch and scalar instruction lowering.

use super::*;

pub(super) fn lower_instruction(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
    sparse_write_state: SparseWriteState,
    sparse_chunk_state: SparseChunkState,
    sparse_dirty_word_state: SparseChunkState,
    sparse_metadata_action: SparseMetadataAction,
) {
    if let SIRInstruction::Commit(src, dst, _, _, _) = inst
        && src.region == crate::SPARSE_WORKING_REGION
        && dst.region == STABLE_REGION
    {
        let abs = src.absolute_addr();
        let sparse = &ctx.layout.sparse_layouts[&abs];
        block.push(MInst::SparseCommit {
            src_offset: (ctx.layout.sparse_base_offset + ctx.layout.sparse_offsets[&abs]) as i32,
            dst_offset: ctx.layout.offsets[&abs] as i32,
            byte_size: ctx.layout.plane_size(&abs),
            dirty_words_offset: sparse.dirty_words_offset as i32,
            dirty_word_count: sparse.dirty_word_count,
            summary_words_offset: sparse.summary_words_offset as i32,
            summary_word_count: sparse.summary_word_count,
            four_state: ctx.four_state && ctx.layout.is_4states[&abs],
        });
        return;
    }
    match inst {
        SIRInstruction::RuntimeEvent { site_id, args } => {
            let event_ptr = load_runtime_event_ptr(ctx, block);
            lower_runtime_event_write(ctx, block, event_ptr, *site_id, args);
        }
        SIRInstruction::CombCaptureEvent { .. } => {
            unreachable!("comb capture events are CFG-lowered by lower_execution_unit")
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, sites } => {
            emit_enable_comb_capture_sites_if_regs_changed(ctx, block, *old, *new, sites);
        }
        SIRInstruction::Mux(dst, cond, then_val, else_val) => {
            let d_width = ctx.sir_width(dst);
            let (cond_is_true, cond_is_unknown) = lower_mux_condition_state(ctx, block, *cond);

            if d_width > 64 {
                let n_chunks = ISelContext::num_chunks(d_width);
                let tv_chunks = ctx.get_wide_chunks(then_val, block);
                let ev_chunks = ctx.get_wide_chunks(else_val, block);
                let zero_v = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero_v,
                    value: 0,
                });
                if ctx.four_state {
                    let tm_chunks = get_wide_mask_chunks(ctx, block, then_val, n_chunks);
                    let em_chunks = get_wide_mask_chunks(ctx, block, else_val, n_chunks);
                    let mut value_chunks = Vec::with_capacity(n_chunks);
                    let mut mask_chunks = Vec::with_capacity(n_chunks);
                    for i in 0..n_chunks {
                        let tv = tv_chunks.get(i).map(|chunk| chunk.0).unwrap_or(zero_v);
                        let ev = ev_chunks.get(i).map(|chunk| chunk.0).unwrap_or(zero_v);
                        let tm = *tm_chunks.get(i).unwrap_or(&zero_v);
                        let em = *em_chunks.get(i).unwrap_or(&zero_v);
                        let chunk_width = (d_width - i * 64).min(64);
                        let (value, mask) = lower_four_state_mux_chunk(
                            ctx,
                            block,
                            cond_is_true,
                            cond_is_unknown,
                            tv,
                            tm,
                            ev,
                            em,
                            chunk_width,
                        );
                        value_chunks.push((value, chunk_width));
                        mask_chunks.push((mask, chunk_width));
                    }
                    ctx.set_wide_chunks(*dst, value_chunks);
                    ctx.set_mask(*dst, mask_chunks[0].0);
                    ctx.wide_masks.insert(*dst, mask_chunks);
                } else {
                    let mut value_chunks = Vec::with_capacity(n_chunks);
                    for i in 0..n_chunks {
                        let tv = tv_chunks.get(i).map(|chunk| chunk.0).unwrap_or(zero_v);
                        let ev = ev_chunks.get(i).map(|chunk| chunk.0).unwrap_or(zero_v);
                        let chunk_width = (d_width - i * 64).min(64);
                        let selected = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Select {
                            dst: selected,
                            cond: cond_is_true,
                            true_val: tv,
                            false_val: ev,
                        });
                        let value = if chunk_width < 64 {
                            let masked = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_and_imm(block, masked, selected, mask_for_width(chunk_width));
                            masked
                        } else {
                            selected
                        };
                        value_chunks.push((value, chunk_width));
                    }
                    ctx.set_wide_chunks(*dst, value_chunks);
                }
            } else {
                let dst_vreg = ctx.reg_map.get(*dst);
                let tv = if ctx.wide_regs.contains_key(then_val) {
                    ctx.get_wide_chunks(then_val, block)[0].0
                } else {
                    ctx.reg_map.get(*then_val)
                };
                let ev = if ctx.wide_regs.contains_key(else_val) {
                    ctx.get_wide_chunks(else_val, block)[0].0
                } else {
                    ctx.reg_map.get(*else_val)
                };

                if !ctx.four_state && d_width == 1 {
                    let tv = lower_low_bit(ctx, block, tv);
                    let ev = lower_low_bit(ctx, block, ev);

                    block.push(MInst::Select {
                        dst: dst_vreg,
                        cond: cond_is_true,
                        true_val: tv,
                        false_val: ev,
                    });
                    ctx.known_bits.insert(dst_vreg, 1);
                    return;
                }

                if !ctx.four_state
                    && d_width <= 64
                    && ctx.known_bits.get(&tv).copied().unwrap_or(64) <= d_width
                    && ctx.known_bits.get(&ev).copied().unwrap_or(64) <= d_width
                    && let Some((guard, lhs, rhs, kind)) =
                        match_guarded_cmp_select_cond(ctx, block, sir_block, sir_defs, *cond)
                {
                    block.push(MInst::GuardedCmpSelect {
                        dst: dst_vreg,
                        guard,
                        lhs,
                        rhs,
                        kind,
                        true_val: tv,
                        false_val: ev,
                    });
                    ctx.known_bits.insert(dst_vreg, d_width);
                    return;
                }

                if !ctx.four_state
                    && d_width <= 64
                    && ctx.known_bits.get(&tv).copied().unwrap_or(64) <= d_width
                    && ctx.known_bits.get(&ev).copied().unwrap_or(64) <= d_width
                {
                    block.push(MInst::Select {
                        dst: dst_vreg,
                        cond: cond_is_true,
                        true_val: tv,
                        false_val: ev,
                    });
                    ctx.known_bits.insert(dst_vreg, d_width);
                    return;
                }

                if ctx.four_state {
                    let tm = ctx.get_mask(*then_val, block);
                    let em = ctx.get_mask(*else_val, block);
                    let (value, mask) = lower_four_state_mux_chunk(
                        ctx,
                        block,
                        cond_is_true,
                        cond_is_unknown,
                        tv,
                        tm,
                        ev,
                        em,
                        d_width,
                    );
                    ctx.emit_mov(block, dst_vreg, value);
                    ctx.set_mask(*dst, mask);
                } else {
                    let selected = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: selected,
                        cond: cond_is_true,
                        true_val: tv,
                        false_val: ev,
                    });
                    if d_width < 64 {
                        ctx.emit_and_imm(block, dst_vreg, selected, mask_for_width(d_width));
                    } else {
                        ctx.emit_mov(block, dst_vreg, selected);
                    }
                }
            }
        }
        SIRInstruction::Imm(dst, val) => {
            let d_width = ctx.sir_width(dst);
            let digits = val.payload.to_u64_digits();
            let imm_val = digits.first().copied().unwrap_or(0);

            let vreg = ctx.reg_map.get(*dst);
            ctx.spill_descs[vreg.0 as usize] = SpillDesc::remat(imm_val);
            block.push(MInst::LoadImm {
                dst: vreg,
                value: imm_val,
            });
            // Track constant value for later folding
            ctx.consts.insert(*dst, imm_val);
            set_low_zero_bits(ctx, *dst, low_zero_bits_const(imm_val));
            // Known bits from the constant value
            let imm_bits = if imm_val == 0 {
                0
            } else {
                64 - imm_val.leading_zeros() as usize
            };
            ctx.known_bits.insert(vreg, imm_bits.min(d_width));

            // For wide values, also store chunks in wide_regs
            if d_width > 64 {
                let n_chunks = ISelContext::num_chunks(d_width);
                let mut chunks = Vec::with_capacity(n_chunks);
                for i in 0..n_chunks {
                    let chunk_val = digits.get(i).copied().unwrap_or(0);
                    if i == 0 {
                        chunks.push((vreg, 64));
                    } else {
                        let cv = ctx.alloc_vreg(SpillDesc::remat(chunk_val));
                        block.push(MInst::LoadImm {
                            dst: cv,
                            value: chunk_val,
                        });
                        chunks.push((cv, 64));
                    }
                }
                ctx.set_wide_chunks(*dst, chunks);
            }

            // 4-state: load mask immediate
            if ctx.four_state {
                let mask_digits = val.mask.to_u64_digits();
                let mask_val = mask_digits.first().copied().unwrap_or(0);
                let mvreg = ctx.alloc_vreg(SpillDesc::remat(mask_val));
                block.push(MInst::LoadImm {
                    dst: mvreg,
                    value: mask_val,
                });
                ctx.set_mask(*dst, mvreg);

                if d_width > 64 {
                    let n_chunks = ISelContext::num_chunks(d_width);
                    let mut mchunks = Vec::with_capacity(n_chunks);
                    mchunks.push((mvreg, 64));
                    for i in 1..n_chunks {
                        let cv = mask_digits.get(i).copied().unwrap_or(0);
                        let mv = ctx.alloc_vreg(SpillDesc::remat(cv));
                        block.push(MInst::LoadImm { dst: mv, value: cv });
                        mchunks.push((mv, 64));
                    }
                    ctx.wide_masks.insert(*dst, mchunks);
                }
            }
        }

        SIRInstruction::Load(dst, addr, offset, width_bits) => {
            match offset {
                SIROffset::Static(bit_offset) | SIROffset::PackedElements { bit_offset, .. } => {
                    ctx.reg_addrs.insert(*dst, (*addr, *bit_offset));
                }
                SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                    ctx.reg_addrs.remove(dst);
                }
            }
            let vreg = ctx.reg_map.get(*dst);

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
                        let chunks = lower_static_wide_load_chunks(
                            ctx,
                            block,
                            value_base,
                            intra_byte,
                            *width_bits,
                        );
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
                        let n_chunks = ISelContext::num_chunks(*width_bits);
                        let mut chunks = Vec::with_capacity(n_chunks);
                        let mut remaining = *width_bits;
                        let mut bit_pos = *bit_off;
                        for _ in 0..n_chunks {
                            let chunk_bits = remaining.min(64);
                            let chunk_byte_off = ctx.byte_offset(addr, bit_pos);
                            let chunk_size = ISelContext::op_size_for_width(chunk_bits);
                            let chunk_vreg = ctx.alloc_vreg(SpillDesc::sim_state(
                                *addr, bit_pos, chunk_bits, false,
                            ));
                            block.push(MInst::Load {
                                dst: chunk_vreg,
                                base: BaseReg::SimState,
                                offset: chunk_byte_off,
                                size: chunk_size,
                            });
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
                                block.push(MInst::Load {
                                    dst: mv,
                                    base: BaseReg::SimState,
                                    offset: chunk_byte_off,
                                    size: chunk_size,
                                });
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
                        return;
                    }

                    let byte_off = ctx.byte_offset(addr, *bit_off);
                    let op_size = ISelContext::op_size_for_width(*width_bits);

                    // Update spill desc
                    ctx.spill_descs[vreg.0 as usize] =
                        SpillDesc::sim_state(*addr, *bit_off, *width_bits, false);

                    if !ctx.four_state
                        && let Some(load_size) =
                            ctx.full_static_load_size(addr, *bit_off, *width_bits)
                    {
                        // Padding can retain unrelated bits after partial writes.
                        // Strip it before the element participates in a concat.
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
                                && let Some(load_size) =
                                    ctx.full_static_load_size(addr, *bit_off, var_width)
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
                SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                    let full_element_size = ctx.full_element_access_size(addr, offset, *width_bits);
                    let direct_byte_off = (*width_bits <= 64)
                        .then(|| {
                            recomposed_element_byte_offset(
                                ctx, block, addr, offset, sir_block, sir_defs,
                            )
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
                    let value_alias_range = MemoryAliasRange::new(
                        base_off,
                        ctx.layout.plane_size(&addr.absolute_addr()),
                    );
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
                        let offset_vreg =
                            offset_vreg.expect("wide dynamic load requires a bit offset");
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
                        let padded_full_element = full_element_size.is_some_and(|size| {
                            ISelContext::access_size_has_padding(size, *width_bits)
                        });
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
                                let load_size =
                                    ISelContext::op_size_for_width(*width_bits + intra_byte);
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
                                    ctx.emit_and_imm(
                                        block,
                                        mvreg,
                                        shifted,
                                        mask_for_width(*width_bits),
                                    );
                                } else {
                                    ctx.emit_and_imm(
                                        block,
                                        mvreg,
                                        tmp,
                                        mask_for_width(*width_bits),
                                    );
                                }
                            }
                            ctx.set_mask(*dst, mvreg);
                        }
                    }
                    SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                        // Dynamic: load mask similarly with indexed addressing
                        let full_element_size =
                            ctx.full_element_access_size(addr, offset, *width_bits);
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

        SIRInstruction::Store(addr, offset, width_bits, src_reg, triggers, comb_capture_sites) => {
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
                            let trigger_offset =
                                ctx.layout.triggered_bits_offset + trigger_byte_idx;

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
                            let bit_mask =
                                ctx.alloc_vreg(SpillDesc::remat(1u64 << trigger_bit_idx));
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
                    collect_static_comb_store_byte_probes(
                        ctx,
                        block,
                        addr,
                        *bit_off,
                        *width_bits,
                        false,
                    )
                } else {
                    Vec::new()
                };
                let old_comb_mask_probe = if comb_capture_sites.is_empty()
                    || *width_bits > 64
                    || !ctx.is_4state_var(addr)
                {
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
                let old_comb_wide_mask_probe = if comb_capture_sites.is_empty()
                    || *width_bits <= 64
                    || !ctx.is_4state_var(addr)
                {
                    Vec::new()
                } else if let SIROffset::Static(bit_off) = offset {
                    collect_static_comb_store_byte_probes(
                        ctx,
                        block,
                        addr,
                        *bit_off,
                        *width_bits,
                        true,
                    )
                } else {
                    Vec::new()
                };
                match offset {
                    SIROffset::Static(bit_off)
                    | SIROffset::PackedElements {
                        bit_offset: bit_off,
                        ..
                    } => {
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
                                            let load_size =
                                                ISelContext::op_size_for_width(part_width + intra);
                                            let old = ctx.alloc_vreg(SpillDesc::transient());
                                            block.push(MInst::Load {
                                                dst: old,
                                                base: BaseReg::SimState,
                                                offset: containing_off,
                                                size: load_size,
                                            });
                                            let mask = mask_for_width(part_width);
                                            let new = ctx.alloc_vreg(SpillDesc::transient());
                                            ctx.emit_bfi(
                                                block,
                                                new,
                                                old,
                                                part_src,
                                                intra as u8,
                                                mask,
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

                            if let Some(size) =
                                ctx.full_static_store_size(addr, *bit_off, *width_bits)
                            {
                                let src_vreg =
                                    ctx.mask_for_store_width(block, src_vreg, *width_bits);
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
                            } else {
                                let mut consumed = 0usize;
                                while consumed < *width_bits {
                                    let part_bit_off = *bit_off + consumed;
                                    let intra = ctx.static_byte_and_intra(addr, part_bit_off).1;
                                    let part_width = (*width_bits - consumed).min(64 - intra);
                                    let containing_byte_off = ctx.byte_offset(addr, part_bit_off);
                                    let load_size =
                                        ISelContext::op_size_for_width(part_width + intra);

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
                                        SpillDesc::transient()
                                            .with_state_insert(src_vreg, intra, part_width)
                                    } else {
                                        SpillDesc::transient().with_state_insert_fragment(
                                            src_vreg, consumed, intra, part_width,
                                        )
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
                    SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                        // Dynamic offset store: RMW with register-indexed addressing.
                        let full_element_size =
                            ctx.full_element_access_size(addr, offset, *width_bits);
                        let src_vreg = ctx.reg_map.get(*src_reg);
                        let offset_vreg = memory_offset_vreg(ctx, block, addr, offset);
                        let offset_low_zero_bits = memory_offset_low_zero_bits(ctx, addr, offset);
                        let base_off = ctx.byte_offset(addr, 0);
                        let value_alias_range = MemoryAliasRange::new(
                            base_off,
                            ctx.layout.plane_size(&addr.absolute_addr()),
                        );

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
                                emit_enable_comb_capture_sites(
                                    ctx,
                                    block,
                                    changed,
                                    comb_capture_sites,
                                );
                            }
                        } else if offset_low_zero_bits >= 3
                            && let Some(store_size) =
                                full_element_size.or_else(|| OpSize::from_bits(*width_bits))
                        {
                            let store_src = if *width_bits < 64 {
                                let masked = ctx.alloc_vreg(SpillDesc::transient());
                                ctx.emit_and_imm(
                                    block,
                                    masked,
                                    src_vreg,
                                    mask_for_width(*width_bits),
                                );
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
                                emit_enable_comb_capture_sites(
                                    ctx,
                                    block,
                                    changed,
                                    comb_capture_sites,
                                );
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
                                emit_enable_comb_capture_sites(
                                    ctx,
                                    block,
                                    changed,
                                    comb_capture_sites,
                                );
                            }
                        }
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
                            if *width_bits > 64 {
                                // Wide mask store: chunk-by-chunk
                                if let Some(mchunks) = ctx.wide_masks.get(src_reg).cloned() {
                                    let mut bit_pos = 0usize;
                                    let mut store_remaining = *width_bits;
                                    for (chunk_vreg, chunk_width) in &mchunks {
                                        if store_remaining == 0 {
                                            break;
                                        }
                                        let logical_chunk_width =
                                            (*chunk_width).min(store_remaining);
                                        let mut consumed = 0usize;
                                        while consumed < logical_chunk_width {
                                            let part_bit_off = *bit_off + bit_pos + consumed;
                                            let intra =
                                                ctx.static_byte_and_intra(addr, part_bit_off).1;
                                            let remaining = logical_chunk_width - consumed;
                                            let part_width = remaining.min(64 - intra);
                                            let part_byte_off =
                                                ctx.mask_byte_offset(addr, part_bit_off);
                                            let part_src = if consumed == 0 {
                                                *chunk_vreg
                                            } else {
                                                let shifted =
                                                    ctx.alloc_vreg(SpillDesc::transient());
                                                block.push(MInst::ShrImm {
                                                    dst: shifted,
                                                    src: *chunk_vreg,
                                                    imm: consumed as u8,
                                                });
                                                shifted
                                            };

                                            if intra == 0 && OpSize::from_bits(part_width).is_some()
                                            {
                                                block.push(MInst::Store {
                                                    base: BaseReg::SimState,
                                                    offset: part_byte_off,
                                                    src: part_src,
                                                    size: OpSize::from_bits(part_width).unwrap(),
                                                });
                                            } else {
                                                let containing_off = part_byte_off;
                                                let load_size = ISelContext::op_size_for_width(
                                                    part_width + intra,
                                                );
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
                                }
                            } else {
                                let mask_off = ctx.mask_byte_offset(addr, *bit_off);
                                let intra_byte = ctx.static_byte_and_intra(addr, *bit_off).1;
                                if let Some(size) =
                                    ctx.full_static_store_size(addr, *bit_off, *width_bits)
                                {
                                    let mask_vreg =
                                        ctx.mask_for_store_width(block, mask_vreg, *width_bits);
                                    block.push(MInst::Store {
                                        base: BaseReg::SimState,
                                        offset: mask_off,
                                        src: mask_vreg,
                                        size,
                                    });
                                } else if intra_byte == 0
                                    && OpSize::from_bits(*width_bits).is_some()
                                {
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
                                        let containing_off =
                                            ctx.mask_byte_offset(addr, part_bit_off);
                                        let load_size =
                                            ISelContext::op_size_for_width(part_width + intra);
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
                                        let descriptor =
                                            if consumed == 0 && part_width == *width_bits {
                                                SpillDesc::transient()
                                                    .with_state_insert(mask_vreg, intra, part_width)
                                            } else {
                                                SpillDesc::transient().with_state_insert_fragment(
                                                    mask_vreg, consumed, intra, part_width,
                                                )
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
                        SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                            // Dynamic mask store: same RMW pattern as value store,
                            // but targeting the mask memory region.
                            let full_element_size =
                                ctx.full_element_access_size(addr, offset, *width_bits);
                            let offset_vreg = memory_offset_vreg(ctx, block, addr, offset);
                            let offset_low_zero_bits =
                                memory_offset_low_zero_bits(ctx, addr, offset);
                            let mask_base_off = ctx.mask_byte_offset(addr, 0);
                            let mask_alias_range = MemoryAliasRange::new(
                                mask_base_off,
                                ctx.layout.plane_size(&addr.absolute_addr()),
                            );

                            let m_byte_off = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: m_byte_off,
                                src: offset_vreg,
                                imm: 3,
                            });
                            if *width_bits > 64 && offset_low_zero_bits >= 3 {
                                let n_chunks = width_bits.div_ceil(64);
                                let mask_vregs =
                                    get_wide_mask_chunks(ctx, block, src_reg, n_chunks);
                                let mask_chunks = mask_vregs
                                    .into_iter()
                                    .enumerate()
                                    .map(|(index, chunk)| {
                                        (chunk, (*width_bits - index * 64).min(64))
                                    })
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
                                let mask_chunks =
                                    get_wide_mask_chunks(ctx, block, src_reg, n_chunks)
                                        .into_iter()
                                        .enumerate()
                                        .map(|(index, chunk)| {
                                            (chunk, (*width_bits - index * 64).min(64))
                                        })
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
                                    emit_enable_comb_capture_sites(
                                        ctx,
                                        block,
                                        changed,
                                        comb_capture_sites,
                                    );
                                }
                            } else if offset_low_zero_bits >= 3
                                && let Some(store_size) =
                                    full_element_size.or_else(|| OpSize::from_bits(*width_bits))
                            {
                                let store_src = if *width_bits < 64 {
                                    let masked = ctx.alloc_vreg(SpillDesc::transient());
                                    ctx.emit_and_imm(
                                        block,
                                        masked,
                                        mask_vreg,
                                        mask_for_width(*width_bits),
                                    );
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
                                    emit_enable_comb_capture_sites(
                                        ctx,
                                        block,
                                        changed,
                                        comb_capture_sites,
                                    );
                                }
                            }
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
                            let trigger_offset =
                                ctx.layout.triggered_bits_offset + trigger_byte_idx;

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

                            let bit_mask =
                                ctx.alloc_vreg(SpillDesc::remat(1u64 << trigger_bit_idx));
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
            } // else (width != 0)
        } // Store

        SIRInstruction::Commit(src_addr, dst_addr, offset, width_bits, _triggers) => {
            ctx.trigger_only_seen.clear();
            let whole_array_plane = match offset {
                SIROffset::Static(0) => ctx
                    .layout
                    .unpacked_arrays
                    .get(&src_addr.absolute_addr())
                    .zip(ctx.layout.unpacked_arrays.get(&dst_addr.absolute_addr()))
                    .filter(|(src, dst)| {
                        src == dst && *width_bits == src.element_width * src.element_count
                    })
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
                        let (dst_byte_off, dst_intra) =
                            ctx.static_byte_and_intra(dst_addr, *bit_off);
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
                            && width_bits % 8 == 0
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
                            let (src_value_off, intra) =
                                ctx.static_byte_and_intra(src_addr, *bit_off);
                            let (dst_value_off, dst_intra) =
                                ctx.static_byte_and_intra(dst_addr, *bit_off);
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
                                && width_bits % 8 == 0
                                && *width_bits >= 512
                            {
                                let src_byte_off = src_value_off
                                    + ctx.layout.plane_size(&src_addr.absolute_addr()) as i32;
                                let dst_byte_off = dst_value_off
                                    + ctx.layout.plane_size(&dst_addr.absolute_addr()) as i32;
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

        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            let d_width = ctx.sir_width(dst);
            let lhs_width = ctx.sir_width(lhs);
            let rhs_width = ctx.sir_width(rhs);
            debug_assert!(lhs_width > 64 || !ctx.wide_regs.contains_key(lhs));
            debug_assert!(rhs_width > 64 || !ctx.wide_regs.contains_key(rhs));

            // A constant logical shift whose wide source is consumed as one
            // narrow result is an extraction, not a full-width shift. Select
            // it before the generic wide dispatcher; otherwise that path
            // constructs every shifted source chunk and only afterwards drops
            // all but the low destination bits.
            if !ctx.four_state
                && d_width <= 64
                && lhs_width > 64
                && rhs_width <= 64
                && matches!(op, BinaryOp::Shr)
                && ctx.consts.contains_key(rhs)
            {
                lower_wide_extract(ctx, block, *dst, *lhs, *rhs);
                return;
            }

            // Wide (>64-bit) binary operations: dispatch to multi-word handler.
            // For comparisons/logic, the result may be narrow (1 bit) but the
            // operands can be wide — dispatch based on operand width too.
            if d_width > 64 || lhs_width > 64 || rhs_width > 64 {
                lower_wide_binary(ctx, block, *dst, *lhs, op, *rhs);
                if ctx.four_state {
                    lower_wide_binary_mask(ctx, block, *dst, *lhs, op, *rhs, d_width);
                    if !matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
                        normalize_wide_value(ctx, block, *dst);
                    }
                }
                ctx.canonicalize_narrow_wide_result(block, *dst);
                return;
            }
            // Constant folding: if both operands are known constants, compute result
            let lhs_const = ctx.consts.get(lhs).copied();
            let rhs_const = ctx.consts.get(rhs).copied();
            let masks_are_known_zero =
                ctx.const_mask_value(*lhs) == Some(0) && ctx.const_mask_value(*rhs) == Some(0);
            if masks_are_known_zero && let (Some(lc), Some(rc)) = (lhs_const, rhs_const) {
                let result = match op {
                    BinaryOp::Add => Some(lc.wrapping_add(rc)),
                    BinaryOp::Sub => Some(lc.wrapping_sub(rc)),
                    BinaryOp::Mul => Some(lc.wrapping_mul(rc)),
                    BinaryOp::And => Some(lc & rc),
                    BinaryOp::Or => Some(lc | rc),
                    BinaryOp::Xor => Some(lc ^ rc),
                    BinaryOp::Shl => Some(if rc >= 64 { 0 } else { lc << rc }),
                    BinaryOp::Shr => Some(if rc >= 64 { 0 } else { lc >> rc }),
                    _ => None,
                };
                if let Some(val) = result {
                    let mask = mask_for_width(d_width);
                    let val = val & mask;
                    let dst_vreg = ctx.reg_map.get(*dst);
                    ctx.spill_descs[dst_vreg.0 as usize] = SpillDesc::remat(val);
                    block.push(MInst::LoadImm {
                        dst: dst_vreg,
                        value: val,
                    });
                    ctx.consts.insert(*dst, val);
                    set_low_zero_bits(ctx, *dst, low_zero_bits_const(val));
                    if ctx.four_state {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        ctx.set_mask(*dst, z);
                    }
                    return;
                }
            }

            let dst_vreg = ctx.reg_map.get(*dst);
            let lhs_vreg = ctx.reg_map.get(*lhs);
            let rhs_vreg = ctx.reg_map.get(*rhs);

            match op {
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
                    // 64-bit arithmetic may produce upper bits; mask to output width.
                    let raw = if d_width < 64 {
                        ctx.alloc_vreg(SpillDesc::transient())
                    } else {
                        dst_vreg
                    };
                    let narrow32 = d_width <= 32;
                    match op {
                        BinaryOp::Add if narrow32 => block.push(MInst::Add32 {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Add => block.push(MInst::Add {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Sub if narrow32 => block.push(MInst::Sub32 {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Sub => block.push(MInst::Sub {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Mul if narrow32 => block.push(MInst::Mul32 {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Mul => block.push(MInst::Mul {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        _ => unreachable!(),
                    }
                    if d_width < 64 {
                        ctx.emit_and_imm(block, dst_vreg, raw, mask_for_width(d_width));
                    }
                    let lhs_lz = low_zero_bits_reg(ctx, *lhs);
                    let rhs_lz = low_zero_bits_reg(ctx, *rhs);
                    let lz = match op {
                        BinaryOp::Add | BinaryOp::Sub => lhs_lz.min(rhs_lz),
                        BinaryOp::Mul => {
                            let lhs_const = ctx.consts.get(lhs).copied();
                            let rhs_const = ctx.consts.get(rhs).copied();
                            match (lhs_const, rhs_const) {
                                (_, Some(rc)) => lhs_lz.saturating_add(low_zero_bits_const(rc)),
                                (Some(lc), _) => rhs_lz.saturating_add(low_zero_bits_const(lc)),
                                _ => lhs_lz.saturating_add(rhs_lz),
                            }
                        }
                        _ => unreachable!(),
                    };
                    set_low_zero_bits(ctx, *dst, lz);
                }
                BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
                    // For bitwise ops, result width = max(lhs_bits, rhs_bits).
                    // If both inputs fit within d_width, AND mask is redundant.
                    let lhs_bits = ctx.known_bits.get(&lhs_vreg).copied().unwrap_or(64);
                    let rhs_bits = ctx.known_bits.get(&rhs_vreg).copied().unwrap_or(64);
                    let result_bits = lhs_bits.max(rhs_bits);
                    let needs_mask = d_width < 64 && result_bits > d_width;

                    let raw = if needs_mask {
                        ctx.alloc_vreg(SpillDesc::transient())
                    } else {
                        dst_vreg
                    };
                    let narrow32 = d_width <= 32;
                    match op {
                        BinaryOp::And if narrow32 => block.push(MInst::And32 {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::And => block.push(MInst::And {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Or if narrow32 => block.push(MInst::Or32 {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Or => block.push(MInst::Or {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Xor if narrow32 => block.push(MInst::Xor32 {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Xor => block.push(MInst::Xor {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        _ => unreachable!(),
                    }
                    if needs_mask {
                        ctx.emit_and_imm(block, dst_vreg, raw, mask_for_width(d_width));
                    }
                    let lhs_lz = low_zero_bits_reg(ctx, *lhs);
                    let rhs_lz = low_zero_bits_reg(ctx, *rhs);
                    let lz = match op {
                        BinaryOp::And => lhs_lz.max(rhs_lz),
                        BinaryOp::Or | BinaryOp::Xor => lhs_lz.min(rhs_lz),
                        _ => unreachable!(),
                    };
                    set_low_zero_bits(ctx, *dst, lz);
                }
                BinaryOp::Shr => {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    if let Some(&shift_amt) = ctx.consts.get(rhs) {
                        if shift_amt < 64 {
                            block.push(MInst::ShrImm {
                                dst: shifted,
                                src: lhs_vreg,
                                imm: shift_amt as u8,
                            });
                        } else {
                            block.push(MInst::LoadImm {
                                dst: shifted,
                                value: 0,
                            });
                        }
                        // Track known bits: shr reduces width
                        let lhs_bits = ctx.known_bits.get(&lhs_vreg).copied().unwrap_or(64);
                        let shifted_bits = lhs_bits
                            .saturating_sub(usize::try_from(shift_amt).unwrap_or(usize::MAX));
                        ctx.known_bits.insert(shifted, shifted_bits);
                    } else {
                        let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                        ctx.emit_mov(block, rhs_copy, rhs_vreg);
                        block.push(MInst::Shr {
                            dst: shifted,
                            lhs: lhs_vreg,
                            rhs: rhs_copy,
                        });
                    }
                    // Mask to destination width
                    if d_width < 64 {
                        let mask = mask_for_width(d_width);
                        ctx.emit_and_imm(block, dst_vreg, shifted, mask);
                    } else {
                        ctx.emit_mov(block, dst_vreg, shifted);
                    }
                    let lz = if let Some(&shift_amt) = ctx.consts.get(rhs) {
                        low_zero_bits_reg(ctx, *lhs)
                            .saturating_sub(u32::try_from(shift_amt).unwrap_or(u32::MAX))
                    } else {
                        0
                    };
                    set_low_zero_bits(ctx, *dst, lz);
                }
                BinaryOp::Shl => {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    if let Some(&shift_amt) = ctx.consts.get(rhs) {
                        if shift_amt < 64 {
                            block.push(MInst::ShlImm {
                                dst: shifted,
                                src: lhs_vreg,
                                imm: shift_amt as u8,
                            });
                        } else {
                            block.push(MInst::LoadImm {
                                dst: shifted,
                                value: 0,
                            });
                        }
                    } else {
                        let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                        ctx.emit_mov(block, rhs_copy, rhs_vreg);
                        block.push(MInst::Shl {
                            dst: shifted,
                            lhs: lhs_vreg,
                            rhs: rhs_copy,
                        });
                    }
                    if d_width < 64 {
                        let mask = mask_for_width(d_width);
                        ctx.emit_and_imm(block, dst_vreg, shifted, mask);
                    } else {
                        ctx.emit_mov(block, dst_vreg, shifted);
                    }
                    let lz = if let Some(&shift_amt) = ctx.consts.get(rhs) {
                        low_zero_bits_reg(ctx, *lhs)
                            .saturating_add(u32::try_from(shift_amt).unwrap_or(u32::MAX))
                    } else {
                        0
                    };
                    set_low_zero_bits(ctx, *dst, lz);
                }
                BinaryOp::Sar => {
                    // Arithmetic shift right: sign-extend lhs to 64 bits, shift, mask result.
                    let width = ctx.sir_width(lhs);
                    if width < 64 {
                        let sext_shift = (64 - width) as u8;
                        let shifted_up = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShlImm {
                            dst: shifted_up,
                            src: lhs_vreg,
                            imm: sext_shift,
                        });
                        let sign_extended = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::SarImm {
                            dst: sign_extended,
                            src: shifted_up,
                            imm: sext_shift,
                        });
                        // Now do the actual shift
                        let sar_result = ctx.alloc_vreg(SpillDesc::transient());
                        if let Some(&shift_amt) = ctx.consts.get(rhs) {
                            block.push(MInst::SarImm {
                                dst: sar_result,
                                src: sign_extended,
                                imm: shift_amt.min(63) as u8,
                            });
                        } else {
                            let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_mov(block, rhs_copy, rhs_vreg);
                            block.push(MInst::Sar {
                                dst: sar_result,
                                lhs: sign_extended,
                                rhs: rhs_copy,
                            });
                        }
                        // Mask to output width
                        let mask = mask_for_width(width);
                        ctx.emit_and_imm(block, dst_vreg, sar_result, mask);
                    } else {
                        if let Some(&shift_amt) = ctx.consts.get(rhs) {
                            block.push(MInst::SarImm {
                                dst: dst_vreg,
                                src: lhs_vreg,
                                imm: shift_amt.min(63) as u8,
                            });
                        } else {
                            let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_mov(block, rhs_copy, rhs_vreg);
                            block.push(MInst::Sar {
                                dst: dst_vreg,
                                lhs: lhs_vreg,
                                rhs: rhs_copy,
                            });
                        }
                    }
                }
                BinaryOp::Eq => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::Eq,
                }),
                BinaryOp::Ne => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::Ne,
                }),
                BinaryOp::EqCase | BinaryOp::NeCase => {
                    if ctx.four_state {
                        let l_m = ctx.get_mask(*lhs, block);
                        let r_m = ctx.get_mask(*rhs, block);
                        let value_diff = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Xor {
                            dst: value_diff,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        });
                        let mask_diff = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Xor {
                            dst: mask_diff,
                            lhs: l_m,
                            rhs: r_m,
                        });
                        let diff = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: diff,
                            lhs: value_diff,
                            rhs: mask_diff,
                        });
                        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm {
                            dst: zero,
                            value: 0,
                        });
                        block.push(MInst::Cmp {
                            dst: dst_vreg,
                            lhs: diff,
                            rhs: zero,
                            kind: if matches!(op, BinaryOp::EqCase) {
                                CmpKind::Eq
                            } else {
                                CmpKind::Ne
                            },
                        });
                        ctx.set_mask(*dst, zero);
                    } else {
                        block.push(MInst::Cmp {
                            dst: dst_vreg,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                            kind: if matches!(op, BinaryOp::EqCase) {
                                CmpKind::Eq
                            } else {
                                CmpKind::Ne
                            },
                        });
                    }
                }
                BinaryOp::LtU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::LtU,
                }),
                BinaryOp::LtS => {
                    let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: sl,
                        rhs: sr,
                        kind: CmpKind::LtS,
                    });
                }
                BinaryOp::LeU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::LeU,
                }),
                BinaryOp::LeS => {
                    let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: sl,
                        rhs: sr,
                        kind: CmpKind::LeS,
                    });
                }
                BinaryOp::GtU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::GtU,
                }),
                BinaryOp::GtS => {
                    let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: sl,
                        rhs: sr,
                        kind: CmpKind::GtS,
                    });
                }
                BinaryOp::GeU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::GeU,
                }),
                BinaryOp::GeS => {
                    let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: sl,
                        rhs: sr,
                        kind: CmpKind::GeS,
                    });
                }
                BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
                    let signed = matches!(op, BinaryOp::DivS | BinaryOp::RemS);
                    let lhs_width = ctx.sir_width(lhs);
                    let rhs_width = ctx.sir_width(rhs);
                    let division_lhs = if signed {
                        sign_extend_scalar(ctx, block, lhs_vreg, lhs_width)
                    } else {
                        lhs_vreg
                    };
                    let division_rhs = if signed {
                        sign_extend_scalar(ctx, block, rhs_vreg, rhs_width)
                    } else {
                        rhs_vreg
                    };

                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    let one = ctx.alloc_vreg(SpillDesc::remat(1));
                    block.push(MInst::LoadImm { dst: one, value: 1 });
                    let is_zero = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: is_zero,
                        lhs: division_rhs,
                        rhs: zero,
                        kind: CmpKind::Eq,
                    });

                    let unsafe_divisor = if signed {
                        let min = ctx.alloc_vreg(SpillDesc::remat(1u64 << 63));
                        block.push(MInst::LoadImm {
                            dst: min,
                            value: 1u64 << 63,
                        });
                        let neg_one = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                        block.push(MInst::LoadImm {
                            dst: neg_one,
                            value: u64::MAX,
                        });
                        let is_min = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: is_min,
                            lhs: division_lhs,
                            rhs: min,
                            kind: CmpKind::Eq,
                        });
                        let is_neg_one = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: is_neg_one,
                            lhs: division_rhs,
                            rhs: neg_one,
                            kind: CmpKind::Eq,
                        });
                        let is_overflow = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: is_overflow,
                            lhs: is_min,
                            rhs: is_neg_one,
                        });
                        let unsafe_divisor = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: unsafe_divisor,
                            lhs: is_zero,
                            rhs: is_overflow,
                        });
                        unsafe_divisor
                    } else {
                        is_zero
                    };
                    let safe_rhs = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: safe_rhs,
                        cond: unsafe_divisor,
                        true_val: one,
                        false_val: division_rhs,
                    });
                    let division_result = ctx.alloc_vreg(SpillDesc::transient());
                    match op {
                        BinaryOp::DivU => block.push(MInst::UDiv {
                            dst: division_result,
                            lhs: division_lhs,
                            rhs: safe_rhs,
                        }),
                        BinaryOp::RemU => block.push(MInst::URem {
                            dst: division_result,
                            lhs: division_lhs,
                            rhs: safe_rhs,
                        }),
                        BinaryOp::DivS => block.push(MInst::SDiv {
                            dst: division_result,
                            lhs: division_lhs,
                            rhs: safe_rhs,
                        }),
                        BinaryOp::RemS => block.push(MInst::SRem {
                            dst: division_result,
                            lhs: division_lhs,
                            rhs: safe_rhs,
                        }),
                        _ => unreachable!(),
                    }
                    let defined_result = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: defined_result,
                        cond: is_zero,
                        true_val: zero,
                        false_val: division_result,
                    });
                    if d_width < 64 {
                        ctx.emit_and_imm(block, dst_vreg, defined_result, mask_for_width(d_width));
                    } else {
                        ctx.emit_mov(block, dst_vreg, defined_result);
                    }
                }
                BinaryOp::LogicAnd => {
                    // dst = (lhs != 0) && (rhs != 0) ? 1 : 0
                    let l_bool = lower_bool_value(ctx, block, lhs_vreg);
                    let r_bool = lower_bool_value(ctx, block, rhs_vreg);
                    block.push(MInst::And {
                        dst: dst_vreg,
                        lhs: l_bool,
                        rhs: r_bool,
                    });
                }
                BinaryOp::LogicOr => {
                    let l_bool = lower_bool_value(ctx, block, lhs_vreg);
                    let r_bool = lower_bool_value(ctx, block, rhs_vreg);
                    block.push(MInst::Or {
                        dst: dst_vreg,
                        lhs: l_bool,
                        rhs: r_bool,
                    });
                }
                BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
                    if ctx.four_state {
                        // IEEE 1800 ==?/!=?: RHS X/Z bits are wildcards (don't care)
                        let l_m = ctx.get_mask(*lhs, block);
                        let r_m = ctx.get_mask(*rhs, block);

                        // compare_mask = ~r_m (non-wildcard positions)
                        let compare_mask = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: compare_mask,
                            src: r_m,
                        });

                        // Compare only at non-wildcard positions
                        let l_eff = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: l_eff,
                            lhs: lhs_vreg,
                            rhs: compare_mask,
                        });
                        let r_eff = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: r_eff,
                            lhs: rhs_vreg,
                            rhs: compare_mask,
                        });

                        let kind = if matches!(op, BinaryOp::EqWildcard) {
                            CmpKind::Eq
                        } else {
                            CmpKind::Ne
                        };
                        block.push(MInst::Cmp {
                            dst: dst_vreg,
                            lhs: l_eff,
                            rhs: r_eff,
                            kind,
                        });

                        // Mask: check for LHS X at non-wildcard positions
                        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm {
                            dst: zero,
                            value: 0,
                        });
                        let x_at_compared = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: x_at_compared,
                            lhs: l_m,
                            rhs: compare_mask,
                        });
                        let has_x = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: has_x,
                            lhs: x_at_compared,
                            rhs: zero,
                            kind: CmpKind::Ne,
                        });

                        // If definite mismatch at compared positions → mask=0
                        let l_xor_r = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Xor {
                            dst: l_xor_r,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        });
                        let l_definite = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: l_definite,
                            src: l_m,
                        });
                        let definite_compare = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: definite_compare,
                            lhs: compare_mask,
                            rhs: l_definite,
                        });
                        let mismatch = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: mismatch,
                            lhs: l_xor_r,
                            rhs: definite_compare,
                        });
                        let has_mismatch = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: has_mismatch,
                            lhs: mismatch,
                            rhs: zero,
                            kind: CmpKind::Ne,
                        });

                        // mask = has_mismatch ? 0 : (has_x ? 1 : 0)
                        let res_m = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Select {
                            dst: res_m,
                            cond: has_mismatch,
                            true_val: zero,
                            false_val: has_x,
                        });
                        ctx.set_mask(*dst, res_m);
                        // Skip the general 4-state mask computation below
                    } else {
                        // 2-state: wildcards are same as Eq/Ne
                        let kind = if matches!(op, BinaryOp::EqWildcard) {
                            CmpKind::Eq
                        } else {
                            CmpKind::Ne
                        };
                        block.push(MInst::Cmp {
                            dst: dst_vreg,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                            kind,
                        });
                    }
                }
            }

            if matches!(
                op,
                BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::EqCase
                    | BinaryOp::NeCase
                    | BinaryOp::LtU
                    | BinaryOp::LtS
                    | BinaryOp::LeU
                    | BinaryOp::LeS
                    | BinaryOp::GtU
                    | BinaryOp::GtS
                    | BinaryOp::GeU
                    | BinaryOp::GeS
                    | BinaryOp::LogicAnd
                    | BinaryOp::LogicOr
                    | BinaryOp::EqWildcard
                    | BinaryOp::NeWildcard
            ) {
                ctx.known_bits.insert(dst_vreg, 1);
            }

            // 4-state: compute result mask (skip for wildcards which handle it inline)
            if ctx.four_state
                && !matches!(
                    op,
                    BinaryOp::EqWildcard
                        | BinaryOp::NeWildcard
                        | BinaryOp::EqCase
                        | BinaryOp::NeCase
                )
            {
                let l_m = ctx.get_mask(*lhs, block);
                let r_m = ctx.get_mask(*rhs, block);
                let res_m =
                    lower_binary_mask(ctx, block, op, lhs_vreg, rhs_vreg, l_m, r_m, d_width);
                ctx.set_mask(*dst, res_m);

                // IEEE 1800-2023 11.4.10: known shifts preserve Z. Only an
                // unknown count forces their payload to all X, like the mask.
                let old_v = ctx.reg_map.get(*dst);
                let normalized = ctx.alloc_vreg(SpillDesc::transient());
                if matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
                    block.push(MInst::Select {
                        dst: normalized,
                        cond: r_m,
                        true_val: res_m,
                        false_val: old_v,
                    });
                } else {
                    block.push(MInst::Or {
                        dst: normalized,
                        lhs: old_v,
                        rhs: res_m,
                    });
                }
                ctx.reg_map.set(*dst, normalized);
            }
        }

        SIRInstruction::Unary(dst, op, src) => {
            let d_width = ctx.sir_width(dst);
            let src_width = ctx.sir_width(src);
            debug_assert!(src_width > 64 || !ctx.wide_regs.contains_key(src));
            if d_width > 64 || src_width > 64 {
                lower_wide_unary(ctx, block, *dst, op, *src);
                if ctx.four_state {
                    lower_wide_unary_mask(ctx, block, *dst, op, *src, d_width, src_width);
                    if matches!(op, UnaryOp::ToTwoState) {
                        lower_wide_to_two_state(ctx, block, *dst, *src, d_width, src_width);
                    } else if !matches!(op, UnaryOp::Ident) {
                        normalize_wide_value(ctx, block, *dst);
                    }
                }
                ctx.canonicalize_narrow_wide_result(block, *dst);
                return;
            }
            let dst_vreg = ctx.reg_map.get(*dst);
            let src_vreg = ctx.reg_map.get(*src);

            match op {
                UnaryOp::Ident | UnaryOp::ToTwoState => {
                    ctx.emit_mov(block, dst_vreg, src_vreg);
                }
                UnaryOp::Minus => {
                    if d_width < 64 {
                        let negated = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Neg {
                            dst: negated,
                            src: src_vreg,
                        });
                        ctx.emit_and_imm(block, dst_vreg, negated, mask_for_width(d_width));
                    } else {
                        block.push(MInst::Neg {
                            dst: dst_vreg,
                            src: src_vreg,
                        });
                    }
                }
                UnaryOp::BitNot => {
                    let width = ctx.sir_width(src);
                    if width < 64 {
                        let tmp = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: tmp,
                            src: src_vreg,
                        });
                        ctx.emit_and_imm(block, dst_vreg, tmp, mask_for_width(width));
                    } else {
                        block.push(MInst::BitNot {
                            dst: dst_vreg,
                            src: src_vreg,
                        });
                    }
                }
                UnaryOp::LogicNot => {
                    // dst = (src == 0) ? 1 : 0
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: src_vreg,
                        rhs: zero,
                        kind: CmpKind::Eq,
                    });
                }
                UnaryOp::And => {
                    // Reduction AND: dst = (src == all_ones_mask) ? 1 : 0
                    let width = ctx.sir_width(src);
                    let mask = if width >= 64 {
                        u64::MAX
                    } else {
                        mask_for_width(width)
                    };
                    let mask_vreg = ctx.alloc_vreg(SpillDesc::remat(mask));
                    block.push(MInst::LoadImm {
                        dst: mask_vreg,
                        value: mask,
                    });
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: src_vreg,
                        rhs: mask_vreg,
                        kind: CmpKind::Eq,
                    });
                }
                UnaryOp::Or => {
                    // Reduction OR: dst = (src != 0) ? 1 : 0
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: src_vreg,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                }
                UnaryOp::Xor => {
                    // Reduction XOR: dst = popcount(src) & 1.
                    let pc = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Popcnt {
                        dst: pc,
                        src: src_vreg,
                    });
                    ctx.emit_and_imm(block, dst_vreg, pc, 1);
                    ctx.known_bits.insert(dst_vreg, 1);
                }
                UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
                    lower_narrow_bit_count(ctx, block, dst_vreg, op, src_vreg, src_width);
                    ctx.known_bits.insert(dst_vreg, d_width);
                }
            }

            // 4-state: compute result mask for unary ops
            if ctx.four_state {
                let s_m = ctx.get_mask(*src, block);
                if matches!(op, UnaryOp::ToTwoState) {
                    let defined = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::BitNot {
                        dst: defined,
                        src: s_m,
                    });
                    let cleared = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::And {
                        dst: cleared,
                        lhs: ctx.reg_map.get(*dst),
                        rhs: defined,
                    });
                    ctx.reg_map.set(*dst, cleared);
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    ctx.set_mask(*dst, zero);
                    return;
                }
                let res_m = lower_unary_mask(ctx, block, op, src_vreg, s_m, d_width, src_width);
                ctx.set_mask(*dst, res_m);

                if !matches!(op, UnaryOp::Ident) {
                    // Identity/casts preserve X versus Z. Other unary
                    // operations normalize unknown bits to X.
                    let old_v = ctx.reg_map.get(*dst);
                    let normalized = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: normalized,
                        lhs: old_v,
                        rhs: res_m,
                    });
                    ctx.reg_map.set(*dst, normalized);
                }
            }
        }

        SIRInstruction::Concat(dst, args) => {
            if try_lower_concat_of_muxes(ctx, block, *dst, args, sir_block, sir_defs) {
                return;
            }
            if try_lower_repeated_msb_concat(ctx, block, *dst, args) {
                return;
            }

            // Concat: build a wide value from chunks.
            // For ≤64-bit result, shift and OR the pieces together.
            let dst_vreg = ctx.reg_map.get(*dst);
            let dst_width = ctx.sir_width(dst);

            if dst_width <= 64 {
                // args are [MSB, ..., LSB]
                // Build from LSB to MSB
                let mut accumulated: Option<VReg> = None;
                let mut shift_pos = 0usize;

                for arg in args.iter().rev() {
                    let arg_vreg = ctx.reg_map.get(*arg);
                    let arg_width = ctx.sir_width(arg);

                    match accumulated {
                        None => {
                            // First (LSB) element
                            accumulated = Some(arg_vreg);
                        }
                        Some(acc) => {
                            // Shift this arg and OR with accumulator
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: shifted,
                                src: arg_vreg,
                                imm: shift_pos as u8,
                            });
                            let merged = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: merged,
                                lhs: acc,
                                rhs: shifted,
                            });
                            accumulated = Some(merged);
                        }
                    }
                    shift_pos += arg_width;
                }

                if let Some(result) = accumulated {
                    if result != dst_vreg {
                        ctx.emit_mov(block, dst_vreg, result);
                    }
                }

                // 4-state: concat masks the same way
                if ctx.four_state {
                    let mut m_acc: Option<VReg> = None;
                    let mut m_shift = 0usize;
                    for arg in args.iter().rev() {
                        let m = ctx.get_mask(*arg, block);
                        let aw = ctx.sir_width(arg);
                        match m_acc {
                            None => {
                                m_acc = Some(m);
                            }
                            Some(a) => {
                                let sh = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShlImm {
                                    dst: sh,
                                    src: m,
                                    imm: m_shift as u8,
                                });
                                let mg = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or {
                                    dst: mg,
                                    lhs: a,
                                    rhs: sh,
                                });
                                m_acc = Some(mg);
                            }
                        }
                        m_shift += aw;
                    }
                    if let Some(m_res) = m_acc {
                        ctx.set_mask(*dst, m_res);
                    }
                }
            } else {
                // Wide concat (>64 bits): record chunk vregs for use by Store.
                // args are [MSB, ..., LSB]. Collect bits in LSB-first order,
                // then repack into uniform 64-bit chunks so Slice can use
                // bit_offset / 64 for indexing.
                let total_width = args.iter().map(|a| ctx.sir_width(a)).sum::<usize>();

                // Collect a flat bit stream: list of (vreg, width) in LSB-first order
                let mut flat_bits: Vec<(VReg, usize)> = Vec::new();
                for arg in args.iter().rev() {
                    let arg_width = ctx.sir_width(arg);
                    if arg_width > 64 {
                        let arg_chunks = ctx.get_wide_chunks(arg, block);
                        for ch in arg_chunks {
                            flat_bits.push(ch);
                        }
                    } else {
                        let arg_vreg = ctx.reg_map.get(*arg);
                        flat_bits.push((arg_vreg, arg_width));
                    }
                }

                let dst_chunks = lower_flat_concat_to_chunks(ctx, block, flat_bits, total_width);
                ctx.set_wide_chunks(*dst, dst_chunks);

                // 4-state: repack mask chunks the same way
                if ctx.four_state {
                    let mut mask_flat: Vec<(VReg, usize)> = Vec::new();
                    for arg in args.iter().rev() {
                        let arg_width = ctx.sir_width(arg);
                        if arg_width > 64 {
                            let mc = get_wide_mask_chunks(
                                ctx,
                                block,
                                arg,
                                ISelContext::num_chunks(arg_width),
                            );
                            for (i, mv) in mc.into_iter().enumerate() {
                                let cw = if i == ISelContext::num_chunks(arg_width) - 1 {
                                    let r = arg_width % 64;
                                    if r == 0 { 64 } else { r }
                                } else {
                                    64
                                };
                                mask_flat.push((mv, cw));
                            }
                        } else {
                            let m = ctx.get_mask(*arg, block);
                            mask_flat.push((m, arg_width));
                        }
                    }

                    let dst_m_chunks =
                        lower_flat_concat_to_chunks(ctx, block, mask_flat, total_width);
                    ctx.set_mask(*dst, dst_m_chunks[0].0);
                    ctx.wide_masks.insert(*dst, dst_m_chunks);
                }
            }
        }

        SIRInstruction::Slice(dst, src, bit_offset, width) => {
            let dst_vreg = ctx.reg_map.get(*dst);
            let src_width = ctx.sir_width(src);

            // If src has a known sim-state address (from a preceding Load/Store)
            // load directly from memory. This handles partial Stores that
            // updated memory without rewriting the source register's VRegs.
            if let Some((addr, source_bit_offset)) = ctx.reg_addrs.get(src).cloned() {
                let slice_bit_offset = source_bit_offset + *bit_offset;
                let value_base = ctx.byte_offset(&addr, slice_bit_offset);
                let intra = ctx.static_byte_and_intra(&addr, slice_bit_offset).1;
                let value_chunks =
                    lower_static_wide_load_chunks(ctx, block, value_base, intra, *width);
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
    }
}
