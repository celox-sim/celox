//! SIR instruction dispatch and immediate lowering.

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
    if packed_state::capture(ctx, block, inst) || packed_state::commit(ctx, block, inst) {
        return;
    }
    match inst {
        SIRInstruction::Call {
            function,
            frame,
            args,
            outputs,
            dst,
            ..
        } => {
            call::lower_call(ctx, block, *function, *frame, args, outputs, *dst);
        }
        SIRInstruction::ExternCall { dst, func, args } => {
            extern_call::lower_extern_call(ctx, block, *dst, *func, args);
        }
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
            mux::lower_mux(
                ctx, block, dst, cond, then_val, else_val, sir_block, sir_defs,
            );
        }
        SIRInstruction::Imm(dst, val) => {
            lower_imm(ctx, block, dst, val);
        }

        SIRInstruction::Load(dst, addr, offset, width_bits) => {
            load::lower_load(
                ctx, block, dst, addr, offset, width_bits, sir_block, sir_defs,
            );
        }

        SIRInstruction::Store(addr, offset, width_bits, src_reg, triggers, comb_capture_sites) => {
            store::lower_store(
                ctx,
                block,
                addr,
                offset,
                width_bits,
                src_reg,
                triggers,
                comb_capture_sites,
                sparse_write_state,
                sparse_chunk_state,
                sparse_dirty_word_state,
                sparse_metadata_action,
            );
        }

        SIRInstruction::Commit(src_addr, dst_addr, offset, width_bits, _triggers) => {
            commit::lower_commit(ctx, block, src_addr, dst_addr, offset, width_bits);
        }

        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            binary::lower_binary(ctx, block, inst, dst, lhs, op, rhs, sir_block, sir_defs);
        }

        SIRInstruction::Unary(dst, op, src) => {
            unary::lower_unary(ctx, block, dst, op, src);
        }

        SIRInstruction::Concat(dst, args) => {
            concat::lower_concat(ctx, block, dst, args, sir_block, sir_defs);
        }

        SIRInstruction::Slice(dst, src, bit_offset, width) => {
            slice::lower_slice(ctx, block, dst, src, bit_offset, width);
        }
    }
}

/// Lower `Imm(dst, val)`.
fn lower_imm(ctx: &mut ISelContext, block: &mut MBlock, dst: &RegisterId, val: &crate::SIRValue) {
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
