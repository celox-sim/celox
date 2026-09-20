//! Split wide Concat+Store into at most one native-vector-width per store,
//! placing each store after its source computation and preceding memory effects.
//! This dramatically reduces register pressure for large arrays.
//!
//! Complexity: O(n) per block where n = number of instructions.

use super::pass_manager::ExecutionUnitPass;
use super::sir_analysis::{UseSite, collect_uses};
use crate::HashMap;
use crate::PassOptions;
use crate::ir::*;

pub(in crate::optimizer) struct SplitCoalescedStoresPass {
    pub max_store_width: usize,
}

impl ExecutionUnitPass for SplitCoalescedStoresPass {
    fn name(&self) -> &'static str {
        "split_coalesced_stores"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        split_coalesced_stores(eu, self.max_store_width);
    }
}

fn split_coalesced_stores(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, max_store_width: usize) {
    let block_ids: Vec<BlockId> = eu.blocks.keys().copied().collect();
    let mut reg_counter = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);
    let uses = collect_uses(eu);

    for bid in block_ids {
        let block = match eu.blocks.get(&bid) {
            Some(b) => b,
            None => continue,
        };

        // Phase 1: Build def position map — O(n)
        let mut def_pos: HashMap<RegisterId, usize> = HashMap::default();
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
        let mut last_access: HashMap<RegionedAbsoluteAddr, usize> = HashMap::default();
        let mut last_observable = None;

        for (si, inst) in block.instructions.iter().enumerate() {
            // A source value being ready does not make its Store movable:
            // another Concat operand can still read the destination's old
            // value. Preserve accesses to either side of a Commit as well.
            // Whole-object barriers keep this scan linear and still permit
            // early stores to independent state objects.
            let memory_floor = match inst {
                SIRInstruction::Store(addr, ..) => last_access.get(addr).copied(),
                _ => None,
            }
            .max(last_observable);
            match inst {
                SIRInstruction::Load(_, addr, ..) => {
                    last_access.insert(*addr, si);
                }
                SIRInstruction::Store(addr, _, _, _, triggers, sites) => {
                    last_access.insert(*addr, si);
                    if !triggers.is_empty() || !sites.is_empty() {
                        last_observable = Some(si);
                    }
                }
                SIRInstruction::Commit(src, dst, _, _, triggers) => {
                    last_access.insert(*src, si);
                    last_access.insert(*dst, si);
                    if !triggers.is_empty() {
                        last_observable = Some(si);
                    }
                }
                SIRInstruction::RuntimeEvent { .. }
                | SIRInstruction::CombCaptureEvent { .. }
                | SIRInstruction::CombCaptureEnableIfChanged { .. } => {
                    last_observable = Some(si);
                }
                _ => {}
            }
            let (addr, offset, width, src_reg, comb_capture_sites) = match inst {
                SIRInstruction::Store(
                    addr,
                    SIROffset::Static(off),
                    width,
                    src,
                    triggers,
                    sites,
                ) if *width > 64 && triggers.is_empty() && sites.is_empty() => {
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
            if args.len() < 4 {
                continue;
            }
            let Some(arg_widths) = args
                .iter()
                .map(|source| eu.register_map.get(source).map(RegisterType::width))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            if arg_widths.iter().any(|width| *width == 0 || *width > 64)
                || arg_widths.iter().sum::<usize>() != width
            {
                continue;
            }

            // Build <=128-bit chunks. Concat args are MSB-first while Store
            // offsets grow from the LSB, and ordinary RTL packing can mix
            // unrelated operand widths.
            let args_lsb = args.into_iter().zip(arg_widths).rev().collect::<Vec<_>>();
            let mut chunks = Vec::<Vec<(RegisterId, usize)>>::new();
            for (source, source_width) in args_lsb {
                if chunks.last().is_none_or(|chunk| {
                    chunk.iter().map(|(_, width)| *width).sum::<usize>() + source_width
                        > max_store_width
                }) {
                    chunks.push(Vec::new());
                }
                chunks
                    .last_mut()
                    .expect("a chunk was created for the operand")
                    .push((source, source_width));
            }
            let mut insertions: Vec<(usize, Vec<SIRInstruction<RegionedAbsoluteAddr>>)> =
                Vec::new();
            let mut chunk_offset = offset;
            for chunk_elems in chunks {
                let chunk_width = chunk_elems.iter().map(|(_, width)| *width).sum::<usize>();

                let mut insts_to_insert: Vec<SIRInstruction<RegionedAbsoluteAddr>> = Vec::new();

                let (store_src, insert_after) = if chunk_elems.len() == 1 {
                    let source = chunk_elems[0].0;
                    let pos = def_pos.get(&source).copied().unwrap_or(0);
                    (source, pos)
                } else {
                    reg_counter += 1;
                    let chunk_reg = RegisterId(reg_counter);
                    eu.register_map
                        .insert(chunk_reg, RegisterType::Logic { width: chunk_width });

                    let last_pos = chunk_elems
                        .iter()
                        .filter_map(|(source, _)| def_pos.get(source).copied())
                        .max()
                        .unwrap_or(0);

                    let concat_args: Vec<RegisterId> = chunk_elems
                        .iter()
                        .rev()
                        .map(|(source, _)| *source)
                        .collect();
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

                insertions.push((insert_after.max(memory_floor.unwrap_or(0)), insts_to_insert));
                chunk_offset += chunk_width;
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
        let mut skip: crate::HashSet<usize> = crate::HashSet::default();
        for plan in &plans {
            skip.insert(plan.store_idx);
            if plan.remove_concat {
                skip.insert(plan.concat_idx);
            }
        }

        // Collect insertions by position: after index i, insert these instructions
        let mut insert_map: HashMap<usize, Vec<SIRInstruction<RegionedAbsoluteAddr>>> =
            HashMap::default();
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

    fn make_var_id(n: u32) -> celox_design::StateObjectId {
        celox_design::StateObjectId(n)
    }

    fn make_addr() -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(0),
            var_id: make_var_id(0),
        }
    }

    fn store_around(
        access: SIRInstruction<RegionedAbsoluteAddr>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = crate::HashMap::default();
        let mut instructions = Vec::new();
        for index in 0..4 {
            let register = RegisterId(index);
            register_map.insert(
                register,
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Imm(register, SIRValue::new(index as u64)));
            if index == 1 {
                instructions.push(access.clone());
            }
        }
        register_map.insert(RegisterId(4), RegisterType::Logic { width: 256 });
        register_map.insert(
            RegisterId(5),
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Concat(
            RegisterId(4),
            vec![RegisterId(3), RegisterId(2), RegisterId(1), RegisterId(0)],
        ));
        instructions.push(SIRInstruction::Store(
            make_addr(),
            SIROffset::Static(0),
            256,
            RegisterId(4),
            Vec::new(),
            Vec::new(),
        ));
        let block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions,
            terminator: SIRTerminator::Return,
        };
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), block)].into_iter().collect(),
            register_map,
        }
    }

    #[test]
    fn split_stores_preserve_old_reads_and_both_sides_of_commits() {
        let addr = make_addr();
        let other = RegionedAbsoluteAddr {
            var_id: make_var_id(1),
            ..addr
        };
        for access in [
            SIRInstruction::Load(RegisterId(5), addr, SIROffset::Static(0), 64),
            SIRInstruction::Load(RegisterId(5), addr, SIROffset::Dynamic(RegisterId(0)), 64),
            SIRInstruction::Commit(addr, other, SIROffset::Static(0), 64, Vec::new()),
            SIRInstruction::Commit(other, addr, SIROffset::Static(0), 64, Vec::new()),
        ] {
            let mut eu = store_around(access);
            split_coalesced_stores(&mut eu, 64);
            eu.verify_result().unwrap();
            let instructions = &eu.blocks[&BlockId(0)].instructions;
            let barrier = instructions
                .iter()
                .position(|i| matches!(i, SIRInstruction::Load(..) | SIRInstruction::Commit(..)))
                .unwrap();
            assert!(
                instructions[..barrier]
                    .iter()
                    .all(|i| !matches!(i, SIRInstruction::Store(..)))
            );
            assert_eq!(
                instructions
                    .iter()
                    .filter(|i| matches!(i, SIRInstruction::Store(..)))
                    .count(),
                4
            );
        }
    }

    #[test]
    fn split_stores_can_still_move_across_independent_reads() {
        let other = RegionedAbsoluteAddr {
            var_id: make_var_id(1),
            ..make_addr()
        };
        let mut eu = store_around(SIRInstruction::Load(
            RegisterId(5),
            other,
            SIROffset::Static(0),
            64,
        ));
        split_coalesced_stores(&mut eu, 64);
        eu.verify_result().unwrap();
        let instructions = &eu.blocks[&BlockId(0)].instructions;
        let load = instructions
            .iter()
            .position(|i| matches!(i, SIRInstruction::Load(..)))
            .unwrap();
        assert_eq!(
            instructions[..load]
                .iter()
                .filter(|i| matches!(i, SIRInstruction::Store(..)))
                .count(),
            2
        );
    }

    #[test]
    fn split_stores_do_not_duplicate_capture_effects() {
        let mut eu = store_around(SIRInstruction::Load(
            RegisterId(5),
            make_addr(),
            SIROffset::Static(0),
            64,
        ));
        let instructions = &mut eu.blocks.get_mut(&BlockId(0)).unwrap().instructions;
        let Some(SIRInstruction::Store(_, _, _, _, _, sites)) = instructions.last_mut() else {
            panic!("fixture ends in Store")
        };
        sites.push(7);
        let original = instructions.clone();
        split_coalesced_stores(&mut eu, 64);
        assert_eq!(eu.blocks[&BlockId(0)].instructions, original);
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

        split_coalesced_stores(&mut eu, 64);

        eu.verify_result().unwrap();
        assert!(eu.blocks[&BlockId(0)].instructions.iter().any(
            |instruction| matches!(instruction, SIRInstruction::Concat(dst, _) if *dst == inner)
        ));
    }

    #[test]
    fn splits_a_mixed_width_concat_by_actual_operand_width() {
        let addr = make_addr();
        let widths = [1, 6, 64, 5];
        let mut register_map = crate::HashMap::default();
        let mut instructions = Vec::new();
        for (index, width) in widths.into_iter().enumerate() {
            let register = RegisterId(index);
            register_map.insert(
                register,
                RegisterType::Bit {
                    width,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Imm(register, SIRValue::new(index as u64)));
        }
        let packed = RegisterId(4);
        register_map.insert(packed, RegisterType::Logic { width: 76 });
        instructions.push(SIRInstruction::Concat(
            packed,
            vec![RegisterId(0), RegisterId(1), RegisterId(2), RegisterId(3)],
        ));
        instructions.push(SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            76,
            packed,
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

        split_coalesced_stores(&mut eu, 64);

        eu.verify_result().unwrap();
        let mut stores = eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(_, SIROffset::Static(offset), width, source, _, _) => {
                    Some((*offset, *width, *source))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        stores.sort_unstable_by_key(|(offset, ..)| *offset);
        assert_eq!(
            stores
                .iter()
                .map(|(offset, width, _)| (*offset, *width))
                .collect::<Vec<_>>(),
            vec![(0, 5), (5, 64), (69, 7)]
        );
        assert!(
            stores
                .iter()
                .all(|(_, width, source)| { eu.register_map[source].width() >= *width })
        );
    }
}
