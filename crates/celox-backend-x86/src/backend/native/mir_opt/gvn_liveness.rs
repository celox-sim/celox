//! Sparse SSA liveness for GVN's register-pressure profitability check.

use crate::HashMap;
use crate::native::mir::*;

#[derive(Clone, Copy)]
enum LivePoint {
    Entry(usize),
    Exit(usize),
}

/// Return sorted live-out values for each block. Trace each value backwards
/// from its uses, stopping at its unique SSA definition. This visits each live
/// block/value pair once, independently of block storage order, without
/// repeatedly allocating and merging sets in whole-function fixed-point scans.
pub(super) fn live_out(
    func: &MFunction,
    block_indices: &HashMap<BlockId, usize>,
    predecessors: &[Vec<usize>],
) -> Vec<Vec<VReg>> {
    let value_count = func.vregs.count() as usize;
    let block_count = func.blocks.len();
    let mut definition_blocks = vec![usize::MAX; value_count];
    let mut uses = vec![Vec::new(); value_count];
    for (block_index, block) in func.blocks.iter().enumerate() {
        for phi in &block.phis {
            definition_blocks[phi.dst.0 as usize] = block_index;
        }
        for inst in &block.insts {
            for used in inst.uses() {
                let value = used.0 as usize;
                if definition_blocks[value] != block_index
                    && !matches!(uses[value].last(), Some(LivePoint::Entry(b)) if *b == block_index)
                {
                    uses[value].push(LivePoint::Entry(block_index));
                }
            }
            if let Some(defined) = inst.def() {
                definition_blocks[defined.0 as usize] = block_index;
            }
        }
    }
    for block in &func.blocks {
        for phi in &block.phis {
            for &(predecessor, source) in &phi.sources {
                // A phi input is live at its own predecessor's exit, even
                // when that predecessor is also the value's defining block.
                uses[source.0 as usize].push(LivePoint::Exit(block_indices[&predecessor]));
            }
        }
    }

    let mut live_out = vec![Vec::new(); block_count];
    let mut visited_entry = vec![u32::MAX; block_count];
    let mut visited_exit = vec![u32::MAX; block_count];
    let mut work = Vec::new();
    for (value, use_sites) in uses.iter().enumerate() {
        let vreg = VReg(value as u32);
        work.extend_from_slice(use_sites);
        while let Some(point) = work.pop() {
            match point {
                LivePoint::Entry(block) => {
                    if visited_entry[block] == vreg.0 {
                        continue;
                    }
                    visited_entry[block] = vreg.0;
                    work.extend(predecessors[block].iter().copied().map(LivePoint::Exit));
                }
                LivePoint::Exit(block) => {
                    if visited_exit[block] == vreg.0 {
                        continue;
                    }
                    visited_exit[block] = vreg.0;
                    // Processing VRegs in order produces sorted sets directly.
                    live_out[block].push(vreg);
                    if definition_blocks[value] != block {
                        work.push(LivePoint::Entry(block));
                    }
                }
            }
        }
    }
    live_out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HashSet;

    fn analyze(function: &MFunction) -> Vec<Vec<VReg>> {
        let indices = function
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (block.id, index))
            .collect::<HashMap<_, _>>();
        let mut predecessors = vec![Vec::new(); function.blocks.len()];
        for (index, block) in function.blocks.iter().enumerate() {
            for successor in block.successors() {
                predecessors[indices[&successor]].push(index);
            }
        }
        live_out(function, &indices, &predecessors)
    }

    // Conventional block equations provide an independent oracle for loops,
    // phi edges, duplicate successors, and disconnected components.
    fn reference(function: &MFunction) -> Vec<Vec<VReg>> {
        let count = function.blocks.len();
        let indices = function
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (block.id, index))
            .collect::<HashMap<_, _>>();
        let mut uses = vec![HashSet::default(); count];
        let mut defs = vec![HashSet::default(); count];
        for (index, block) in function.blocks.iter().enumerate() {
            defs[index].extend(block.phis.iter().map(|phi| phi.dst));
            for inst in &block.insts {
                for source in inst.uses() {
                    if !defs[index].contains(&source) {
                        uses[index].insert(source);
                    }
                }
                defs[index].extend(inst.def());
            }
        }
        let mut live_in = vec![HashSet::default(); count];
        let mut live_out = vec![HashSet::default(); count];
        loop {
            let mut changed = false;
            for (index, block) in function.blocks.iter().enumerate().rev() {
                let mut outgoing = HashSet::default();
                for successor in block.successors() {
                    let successor = indices[&successor];
                    outgoing.extend(&live_in[successor]);
                    for phi in &function.blocks[successor].phis {
                        outgoing.extend(
                            phi.sources
                                .iter()
                                .filter_map(|&(pred, source)| (pred == block.id).then_some(source)),
                        );
                    }
                }
                let incoming = uses[index]
                    .union(&outgoing.difference(&defs[index]).copied().collect())
                    .copied()
                    .collect::<HashSet<_>>();
                changed |= incoming != live_in[index];
                live_in[index] = incoming;
                live_out[index] = outgoing;
            }
            if !changed {
                break;
            }
        }
        live_out
            .into_iter()
            .map(|set| {
                let mut values = set.into_iter().collect::<Vec<_>>();
                values.sort_unstable();
                values
            })
            .collect()
    }

    fn function(blocks: Vec<MBlock>, value_count: u32) -> MFunction {
        let mut vregs = VRegAllocator::new();
        for _ in 0..value_count {
            vregs.alloc();
        }
        let mut function =
            MFunction::new(vregs, vec![SpillDesc::transient(); value_count as usize]);
        function.blocks = blocks;
        function
    }

    #[test]
    fn phi_inputs_are_live_only_on_their_own_edges() {
        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 8,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 9,
            },
            MInst::Jump { target: BlockId(1) },
        ];
        let mut header = MBlock::new(BlockId(1));
        header.phis = vec![
            PhiNode {
                dst: VReg(2),
                sources: vec![(BlockId(0), VReg(0)), (BlockId(2), VReg(3))],
            },
            PhiNode {
                dst: VReg(4),
                sources: vec![(BlockId(0), VReg(1)), (BlockId(2), VReg(4))],
            },
        ];
        header.push(MInst::Branch {
            cond: VReg(2),
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        let mut latch = MBlock::new(BlockId(2));
        latch.insts = vec![
            MInst::SubImm {
                dst: VReg(3),
                src: VReg(2),
                imm: 1,
            },
            MInst::Jump { target: BlockId(1) },
        ];
        let mut exit = MBlock::new(BlockId(3));
        exit.insts = vec![
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let function = function(vec![entry, header, latch, exit], 5);
        function.verify_result().unwrap();
        assert_eq!(
            analyze(&function),
            vec![
                vec![VReg(0), VReg(1)],
                vec![VReg(2), VReg(4)],
                vec![VReg(3), VReg(4)],
                vec![]
            ]
        );
        assert_eq!(analyze(&function), reference(&function));
    }

    #[test]
    fn matches_block_equations_on_reordered_cyclic_graphs() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as usize
        };
        for case in 0..512 {
            let count = 2 + next() % 23;
            let id = |block: usize| BlockId((block * 7 + 10) as u32);
            let local = |block: usize| VReg((3 + block * 2) as u32);
            let merged = |block: usize| VReg((4 + block * 2) as u32);
            let mut blocks = Vec::new();
            for index in 0..count {
                let mut block = MBlock::new(id(index));
                if index == 0 {
                    for value in 0..3 {
                        block.push(MInst::LoadImm {
                            dst: VReg(value),
                            value: value.into(),
                        });
                    }
                }
                block.push(MInst::LoadImm {
                    dst: local(index),
                    value: next() as u64,
                });
                for _ in 0..4 {
                    let source = match next() % 5 {
                        3 => local(index),
                        4 => merged(index),
                        value => VReg(value as u32),
                    };
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 8,
                        src: source,
                        size: OpSize::S64,
                    });
                }
                block.push(if index + 1 == count {
                    MInst::Return
                } else {
                    MInst::Branch {
                        cond: VReg((next() % 3) as u32),
                        true_bb: id(1 + next() % (count - 1)),
                        false_bb: id(1 + next() % (count - 1)),
                    }
                });
                blocks.push(block);
            }
            for index in 0..count {
                let sources = blocks
                    .iter()
                    .enumerate()
                    .filter(|(_, block)| block.successors().contains(&id(index)))
                    .map(|(pred, block)| (block.id, local(pred)))
                    .collect::<Vec<_>>();
                if sources.is_empty() {
                    blocks[index].insts.insert(
                        0,
                        MInst::LoadImm {
                            dst: merged(index),
                            value: 0,
                        },
                    );
                } else {
                    blocks[index].phis.push(PhiNode {
                        dst: merged(index),
                        sources,
                    });
                }
            }
            for index in (2..count).rev() {
                blocks.swap(index, 1 + next() % index);
            }
            let function = function(blocks, (3 + 2 * count) as u32);
            assert_eq!(
                analyze(&function),
                reference(&function),
                "case {case}: {function:?}"
            );
        }
    }

    #[test]
    fn long_reverse_order_chain_does_not_need_repeated_sweeps() {
        const COUNT: u32 = 16_384;
        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 42,
            },
            MInst::Jump { target: BlockId(1) },
        ];
        let mut blocks = vec![entry];
        for index in (1..=COUNT).rev() {
            let mut block = MBlock::new(BlockId(index));
            if index == COUNT {
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(0),
                    size: OpSize::S64,
                });
                block.push(MInst::Return);
            } else {
                block.push(MInst::Jump {
                    target: BlockId(index + 1),
                });
            }
            blocks.push(block);
        }
        let function = function(blocks, 1);
        for (block, live) in function.blocks.iter().zip(analyze(&function)) {
            assert_eq!(
                live,
                if block.id == BlockId(COUNT) {
                    vec![]
                } else {
                    vec![VReg(0)]
                }
            );
        }
    }
}
