//! Block parameters as phi nodes: the chunk registers of wide parameters,
//! and the phi nodes built from the arguments that predecessors pass.

use super::*;

/// Give every wide block parameter its chunk registers, for the value and,
/// in four-state, for the mask.
pub(super) fn preseed_wide_block_params(
    ctx: &mut ISelContext,
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_ids: &[crate::BlockId],
    four_state: bool,
) {
    for &sir_block_id in block_ids {
        let sir_block = &eu.blocks[&sir_block_id];
        for &param_reg in &sir_block.params {
            let width = eu.register_map[&param_reg].width();
            let num_chunks = width.div_ceil(64).max(1);
            if num_chunks <= 1 {
                continue;
            }
            if !ctx.wide_regs.contains_key(&param_reg) {
                let mut chunks = Vec::with_capacity(num_chunks);
                chunks.push((ctx.reg_map.get(param_reg), width.min(64)));
                for chunk_idx in 1..num_chunks {
                    let chunk_width = (width - chunk_idx * 64).min(64);
                    let vreg = ctx.alloc_vreg(SpillDesc::transient());
                    chunks.push((vreg, chunk_width));
                }
                ctx.set_wide_chunks(param_reg, chunks);
            }
            if four_state {
                if !ctx.wide_masks.contains_key(&param_reg) {
                    let mut chunks = Vec::with_capacity(num_chunks);
                    let mask0 = ctx.mask_map.get(param_reg);
                    chunks.push((mask0, width.min(64)));
                    for chunk_idx in 1..num_chunks {
                        let chunk_width = (width - chunk_idx * 64).min(64);
                        let vreg = ctx.alloc_vreg(SpillDesc::transient());
                        chunks.push((vreg, chunk_width));
                    }
                    ctx.wide_masks.insert(param_reg, chunks);
                }
            }
        }
    }
}

/// Record the mask registers that hold, at the end of `sir_block`, the
/// arguments its terminator passes to block parameters.
pub(super) fn collect_mask_phi_sources(
    ctx: &ISelContext,
    sir_block: &BasicBlock<RegionedAbsoluteAddr>,
    pred_mir_id: BlockId,
    mask_phi_sources: &mut HashMap<BlockId, Vec<(BlockId, usize, usize, VReg)>>,
) {
    let edges: Vec<(crate::BlockId, &[RegisterId])> = match &sir_block.terminator {
        SIRTerminator::Jump(target, args) => vec![(*target, args.as_slice())],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![
            (true_block.0, true_block.1.as_slice()),
            (false_block.0, false_block.1.as_slice()),
        ],
        SIRTerminator::Switch { .. } => Vec::new(),
        _ => vec![],
    };
    for (target_sir_id, args) in edges {
        if args.is_empty() {
            continue;
        }
        let target_mir_id = BlockId(target_sir_id.0 as u32);
        for (i, arg_reg) in args.iter().enumerate() {
            if let Some(mask_chunks) = ctx.wide_masks.get(arg_reg) {
                for (chunk_idx, (mask_vreg, _)) in mask_chunks.iter().enumerate() {
                    mask_phi_sources.entry(target_mir_id).or_default().push((
                        pred_mir_id,
                        i,
                        chunk_idx,
                        *mask_vreg,
                    ));
                }
            } else if let Some(mask_vreg) = ctx.mask_map.map.get(arg_reg.0).copied().flatten() {
                mask_phi_sources.entry(target_mir_id).or_default().push((
                    pred_mir_id,
                    i,
                    0,
                    mask_vreg,
                ));
            }
        }
    }
}

/// Build the phi nodes of every block from its parameters and the arguments
/// its predecessors pass: one per value chunk and, in four-state, one per
/// mask chunk.
pub(super) fn build_phi_nodes(
    func: &mut MFunction,
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_ids: &[crate::BlockId],
    sir_exit_mir_blocks: &HashMap<crate::BlockId, BlockId>,
    reg_map: &RegMap,
    saved_wide_regs: &WideRegMap,
    saved_mask_map: &RegMap,
    saved_wide_masks: &WideMaskMap,
    mask_phi_sources: &HashMap<BlockId, Vec<(BlockId, usize, usize, VReg)>>,
    four_state: bool,
) {
    use crate::HashMap;
    // Collect phi sources: target_block → [(pred_block, param_idx, chunk_idx, arg_vreg)]
    let mut phi_sources: HashMap<BlockId, Vec<(BlockId, usize, usize, VReg)>> = HashMap::default();
    for &sir_block_id in block_ids {
        let sir_block = &eu.blocks[&sir_block_id];
        let pred_mir_id = sir_exit_mir_blocks
            .get(&sir_block_id)
            .copied()
            .unwrap_or(BlockId(sir_block_id.0 as u32));
        let edges: Vec<(crate::BlockId, &[RegisterId])> = match &sir_block.terminator {
            SIRTerminator::Jump(target, args) => vec![(*target, args.as_slice())],
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => vec![
                (true_block.0, true_block.1.as_slice()),
                (false_block.0, false_block.1.as_slice()),
            ],
            SIRTerminator::Switch { .. } => Vec::new(),
            _ => vec![],
        };
        for (target_sir_id, args) in edges {
            if args.is_empty() {
                continue;
            }
            let target_mir_id = BlockId(target_sir_id.0 as u32);
            for (i, arg_reg) in args.iter().enumerate() {
                if let Some(chunks) = saved_wide_regs.get(arg_reg) {
                    for (chunk_idx, (arg_vreg, _)) in chunks.iter().enumerate() {
                        phi_sources.entry(target_mir_id).or_default().push((
                            pred_mir_id,
                            i,
                            chunk_idx,
                            *arg_vreg,
                        ));
                    }
                } else {
                    let arg_vreg = reg_map.get(*arg_reg);
                    phi_sources.entry(target_mir_id).or_default().push((
                        pred_mir_id,
                        i,
                        0,
                        arg_vreg,
                    ));
                }
            }
        }
    }
    // Build phi nodes on target blocks
    for mblock in &mut func.blocks {
        if let Some(sources) = phi_sources.remove(&mblock.id) {
            let sir_block_id = crate::BlockId(mblock.id.0 as usize);
            let sir_block = &eu.blocks[&sir_block_id];
            for (param_idx, param_reg) in sir_block.params.iter().enumerate() {
                if let Some(dst_chunks) = saved_wide_regs.get(param_reg) {
                    for (chunk_idx, (dst, _)) in dst_chunks.iter().enumerate() {
                        let phi_srcs: Vec<(BlockId, VReg)> = sources
                            .iter()
                            .filter(|(_, idx, src_chunk_idx, _)| {
                                *idx == param_idx && *src_chunk_idx == chunk_idx
                            })
                            .map(|(pred, _, _, vreg)| (*pred, *vreg))
                            .collect();
                        if !phi_srcs.is_empty() {
                            mblock.phis.push(PhiNode {
                                dst: *dst,
                                sources: phi_srcs,
                            });
                        }
                    }
                } else {
                    let dst = reg_map.get(*param_reg);
                    let phi_srcs: Vec<(BlockId, VReg)> = sources
                        .iter()
                        .filter(|(_, idx, src_chunk_idx, _)| {
                            *idx == param_idx && *src_chunk_idx == 0
                        })
                        .map(|(pred, _, _, vreg)| (*pred, *vreg))
                        .collect();
                    if !phi_srcs.is_empty() {
                        mblock.phis.push(PhiNode {
                            dst,
                            sources: phi_srcs,
                        });
                    }
                }

                // 4-state: add mask phi node
                if four_state {
                    if let Some(m_sources) = mask_phi_sources.get(&mblock.id) {
                        if let Some(mask_chunks) = saved_wide_masks.get(param_reg) {
                            for (chunk_idx, (mask_dst, _)) in mask_chunks.iter().enumerate() {
                                let mask_phi_srcs: Vec<(BlockId, VReg)> = m_sources
                                    .iter()
                                    .filter(|(_, idx, src_chunk_idx, _)| {
                                        *idx == param_idx && *src_chunk_idx == chunk_idx
                                    })
                                    .map(|(pred, _, _, vreg)| (*pred, *vreg))
                                    .collect();
                                if !mask_phi_srcs.is_empty() {
                                    mblock.phis.push(PhiNode {
                                        dst: *mask_dst,
                                        sources: mask_phi_srcs,
                                    });
                                }
                            }
                        } else if let Some(mask_dst) =
                            saved_mask_map.map.get(param_reg.0).copied().flatten()
                        {
                            let mask_phi_srcs: Vec<(BlockId, VReg)> = m_sources
                                .iter()
                                .filter(|(_, idx, src_chunk_idx, _)| {
                                    *idx == param_idx && *src_chunk_idx == 0
                                })
                                .map(|(pred, _, _, vreg)| (*pred, *vreg))
                                .collect();
                            if !mask_phi_srcs.is_empty() {
                                mblock.phis.push(PhiNode {
                                    dst: mask_dst,
                                    sources: mask_phi_srcs,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}
