//! Split wide Concat+Store back into individual element stores,
//! placing each store immediately after its source value computation.
//! This dramatically reduces register pressure for large arrays.

use crate::ir::*;
use crate::optimizer::PassOptions;
use super::pass_manager::ExecutionUnitPass;

pub(super) struct SplitCoalescedStoresPass;

impl ExecutionUnitPass for SplitCoalescedStoresPass {
    fn name(&self) -> &'static str {
        "split_coalesced_stores"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        split_coalesced_stores(eu);
    }
}

fn split_coalesced_stores(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let block_ids: Vec<BlockId> = eu.blocks.keys().copied().collect();

    for bid in block_ids {
        let block = match eu.blocks.get(&bid) {
            Some(b) => b,
            None => continue,
        };

        // Find wide Store backed by Concat that would benefit from splitting
        let mut splits: Vec<SplitInfo> = Vec::new();

        for (si, inst) in block.instructions.iter().enumerate() {
            let (addr, offset, width, src_reg) = match inst {
                SIRInstruction::Store(addr, SIROffset::Static(off), width, src, _)
                    if *width > 64 => (*addr, *off, *width, *src),
                _ => continue,
            };

            // Find the Concat that defines src_reg
            let concat = block.instructions[..si].iter()
                .enumerate().rev()
                .find_map(|(ci, cinst)| {
                    if let SIRInstruction::Concat(dst, args) = cinst {
                        if *dst == src_reg && args.len() >= 4 {
                            return Some((ci, args.clone()));
                        }
                    }
                    None
                });

            let Some((concat_idx, args)) = concat else { continue };
            let n_args = args.len();
            let elem_width = width / n_args;
            if elem_width == 0 || elem_width * n_args != width { continue; }
            // Only split if significantly wide (saves enough register pressure)
            if n_args < 4 { continue; }

            // Build 64-bit chunks: group consecutive Concat args into 64-bit stores.
            // Concat args: MSB-first → reverse for LSB-first offset order.
            let args_lsb: Vec<RegisterId> = args.into_iter().rev().collect();
            let elems_per_chunk = (64 / elem_width).max(1);
            let mut chunks: Vec<ChunkInfo> = Vec::new();

            for chunk_start in (0..n_args).step_by(elems_per_chunk) {
                let chunk_end = (chunk_start + elems_per_chunk).min(n_args);
                let chunk_elems: Vec<RegisterId> = args_lsb[chunk_start..chunk_end].to_vec();
                let chunk_offset = offset + chunk_start * elem_width;
                let chunk_width = (chunk_end - chunk_start) * elem_width;
                chunks.push(ChunkInfo { offset: chunk_offset, width: chunk_width, elems: chunk_elems });
            }

            splits.push(SplitInfo { store_idx: si, concat_idx, addr, chunks });
        }

        if splits.is_empty() { continue; }

        let block = eu.blocks.get_mut(&bid).unwrap();

        for split in splits.into_iter().rev() {
            // Remove Store and Concat
            block.instructions.remove(split.store_idx);
            block.instructions.remove(split.concat_idx);

            // Insert 64-bit stores, each right after its last element's definition
            for chunk in split.chunks.into_iter().rev() {
                let store_src = if chunk.elems.len() == 1 {
                    // Single element: store directly
                    chunk.elems[0]
                } else {
                    // Multi-element: need a Concat for this chunk
                    // Allocate a new register
                    let mut max_reg = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);
                    max_reg += 1;
                    let chunk_reg = RegisterId(max_reg);
                    eu.register_map.insert(chunk_reg, RegisterType::Logic { width: chunk.width });

                    // Find position: after the last element's definition
                    let last_def_pos = chunk.elems.iter()
                        .filter_map(|r| block.instructions.iter().position(|i| inst_def(i) == Some(*r)))
                        .max()
                        .unwrap_or(block.instructions.len().saturating_sub(1));

                    // Insert Concat for this chunk (MSB-first)
                    let concat_args: Vec<RegisterId> = chunk.elems.iter().rev().copied().collect();
                    block.instructions.insert(last_def_pos + 1,
                        SIRInstruction::Concat(chunk_reg, concat_args));

                    chunk_reg
                };

                // Find position for Store: after source definition
                let src_pos = block.instructions.iter()
                    .position(|i| inst_def(i) == Some(store_src))
                    .unwrap_or(block.instructions.len().saturating_sub(1));

                block.instructions.insert(src_pos + 1, SIRInstruction::Store(
                    split.addr,
                    SIROffset::Static(chunk.offset),
                    chunk.width,
                    store_src,
                    vec![],
                ));
            }
        }
    }
}

struct SplitInfo {
    store_idx: usize,
    concat_idx: usize,
    addr: RegionedAbsoluteAddr,
    chunks: Vec<ChunkInfo>,
}

struct ChunkInfo {
    offset: usize,
    width: usize,
    elems: Vec<RegisterId>,
}

fn inst_def(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<RegisterId> {
    match inst {
        SIRInstruction::Imm(d, _)
        | SIRInstruction::Load(d, _, _, _)
        | SIRInstruction::Binary(d, _, _, _)
        | SIRInstruction::Unary(d, _, _)
        | SIRInstruction::Concat(d, _)
        | SIRInstruction::Slice(d, _, _, _) => Some(*d),
        SIRInstruction::Store(..) | SIRInstruction::Commit(..) => None,
    }
}
