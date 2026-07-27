//! Split wide Concat+Store back into individual element stores,
//! placing each store immediately after its source value computation.
//! This dramatically reduces register pressure for large arrays.
//!
//! Complexity: O(n) per block where n = number of instructions.

use super::pass_manager::ExecutionUnitPass;
use super::sir_analysis::{UseSite, collect_uses};
use crate::ir::*;
use crate::optimizer::PassOptions;
use std::collections::HashMap;

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
    let mut reg_counter = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);
    let uses = collect_uses(eu);

    for bid in block_ids {
        let block = match eu.blocks.get(&bid) {
            Some(b) => b,
            None => continue,
        };

        // Phase 1: Build def position map — O(n)
        let mut def_pos: HashMap<RegisterId, usize> = HashMap::new();
        for (i, inst) in block.instructions.iter().enumerate() {
            if let Some(d) = inst_def(inst) {
                def_pos.insert(d, i);
            }
        }

        // Phase 2: Find wide Concat+Store pairs to split
        struct SplitPlan {
            store_idx: usize,
            concat_idx: usize,
            remove_concat: bool,
            /// (insert_after_idx, instructions_to_insert)
            insertions: Vec<(usize, Vec<SIRInstruction<RegionedAbsoluteAddr>>)>,
        }
        let mut plans: Vec<SplitPlan> = Vec::new();

        for (si, inst) in block.instructions.iter().enumerate() {
            let (addr, offset, width, src_reg, comb_capture_sites) = match inst {
                SIRInstruction::Store(addr, SIROffset::Static(off), width, src, _, sites)
                    if *width > 64 =>
                {
                    (*addr, *off, *width, *src, sites.clone())
                }
                _ => continue,
            };

            // Find Concat defining src_reg
            let concat =
                block.instructions[..si]
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(ci, cinst)| {
                        if let SIRInstruction::Concat(dst, args) = cinst {
                            if *dst == src_reg && args.len() >= 4 {
                                return Some((ci, args.clone()));
                            }
                        }
                        None
                    });

            let Some((concat_idx, args)) = concat else {
                continue;
            };
            let n_args = args.len();
            let elem_width = width / n_args;
            if elem_width == 0 || elem_width * n_args != width || n_args < 4 {
                continue;
            }

            // Build 64-bit chunks (Concat args are MSB-first, offsets are LSB-first)
            let args_lsb: Vec<RegisterId> = args.into_iter().rev().collect();
            let elems_per_chunk = (64 / elem_width).max(1);
            let mut insertions: Vec<(usize, Vec<SIRInstruction<RegionedAbsoluteAddr>>)> =
                Vec::new();

            for chunk_start in (0..n_args).step_by(elems_per_chunk) {
                let chunk_end = (chunk_start + elems_per_chunk).min(n_args);
                let chunk_elems = &args_lsb[chunk_start..chunk_end];
                let chunk_offset = offset + chunk_start * elem_width;
                let chunk_width = (chunk_end - chunk_start) * elem_width;

                let mut insts_to_insert: Vec<SIRInstruction<RegionedAbsoluteAddr>> = Vec::new();

                let (store_src, insert_after) = if chunk_elems.len() == 1 {
                    let pos = def_pos.get(&chunk_elems[0]).copied().unwrap_or(0);
                    (chunk_elems[0], pos)
                } else {
                    reg_counter += 1;
                    let chunk_reg = RegisterId(reg_counter);
                    eu.register_map
                        .insert(chunk_reg, RegisterType::Logic { width: chunk_width });

                    let last_pos = chunk_elems
                        .iter()
                        .filter_map(|r| def_pos.get(r).copied())
                        .max()
                        .unwrap_or(0);

                    let concat_args: Vec<RegisterId> = chunk_elems.iter().rev().copied().collect();
                    insts_to_insert.push(SIRInstruction::Concat(chunk_reg, concat_args));

                    (chunk_reg, last_pos)
                };

                insts_to_insert.push(SIRInstruction::Store(
                    addr,
                    SIROffset::Static(chunk_offset),
                    chunk_width,
                    store_src,
                    vec![],
                    comb_capture_sites.clone(),
                ));

                insertions.push((insert_after, insts_to_insert));
            }

            plans.push(SplitPlan {
                store_idx: si,
                concat_idx,
                remove_concat: matches!(
                    uses.get(&src_reg).map(Vec::as_slice),
                    Some([UseSite::Instruction { block, index }])
                        if *block == bid && *index == si
                ),
                insertions,
            });
        }

        if plans.is_empty() {
            continue;
        }

        // Phase 3: Rebuild instruction list in one pass — O(n)
        let block = eu.blocks.get_mut(&bid).unwrap();

        // Collect indices to skip (original Store + Concat)
        let mut skip: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for plan in &plans {
            skip.insert(plan.store_idx);
            if plan.remove_concat {
                skip.insert(plan.concat_idx);
            }
        }

        // Collect insertions by position: after index i, insert these instructions
        let mut insert_map: HashMap<usize, Vec<SIRInstruction<RegionedAbsoluteAddr>>> =
            HashMap::new();
        for plan in plans {
            for (after_idx, insts) in plan.insertions {
                insert_map.entry(after_idx).or_default().extend(insts);
            }
        }

        // Single-pass rebuild
        let mut new_insts: Vec<SIRInstruction<RegionedAbsoluteAddr>> =
            Vec::with_capacity(block.instructions.len());

        for (i, inst) in block.instructions.drain(..).enumerate() {
            if !skip.contains(&i) {
                new_insts.push(inst);
            }
            if let Some(extra) = insert_map.remove(&i) {
                new_insts.extend(extra);
            }
        }

        block.instructions = new_insts;
    }
}

fn inst_def(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<RegisterId> {
    match inst {
        SIRInstruction::Imm(d, _)
        | SIRInstruction::Load(d, _, _, _)
        | SIRInstruction::Binary(d, _, _, _)
        | SIRInstruction::Unary(d, _, _)
        | SIRInstruction::Concat(d, _)
        | SIRInstruction::Slice(d, _, _, _)
        | SIRInstruction::Mux(d, _, _, _) => Some(*d),
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_var_id(n: u32) -> veryl_analyzer::ir::VarId {
        let mut id = veryl_analyzer::ir::VarId::default();
        for _ in 0..n {
            id.inc();
        }
        id
    }

    fn make_addr() -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(0),
            var_id: make_var_id(0),
        }
    }

    #[test]
    fn preserves_a_split_concat_still_used_by_another_value() {
        let addr = make_addr();
        let mut register_map = crate::HashMap::default();
        let mut instructions = Vec::new();
        for index in 0..4 {
            let register = RegisterId(index);
            register_map.insert(
                register,
                RegisterType::Bit {
                    width: 32,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Imm(register, SIRValue::new(index as u64)));
        }
        let inner = RegisterId(4);
        register_map.insert(inner, RegisterType::Logic { width: 128 });
        instructions.push(SIRInstruction::Concat(
            inner,
            vec![RegisterId(3), RegisterId(2), RegisterId(1), RegisterId(0)],
        ));
        instructions.push(SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            128,
            inner,
            Vec::new(),
            Vec::new(),
        ));
        let outer = RegisterId(5);
        register_map.insert(outer, RegisterType::Logic { width: 512 });
        instructions.push(SIRInstruction::Concat(outer, vec![inner; 4]));
        instructions.push(SIRInstruction::Store(
            addr,
            SIROffset::Static(128),
            512,
            outer,
            Vec::new(),
            Vec::new(),
        ));
        let mut blocks = crate::HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        split_coalesced_stores(&mut eu);

        eu.verify_result().unwrap();
        assert!(eu.blocks[&BlockId(0)].instructions.iter().any(
            |instruction| matches!(instruction, SIRInstruction::Concat(dst, _) if *dst == inner)
        ));
    }
}
