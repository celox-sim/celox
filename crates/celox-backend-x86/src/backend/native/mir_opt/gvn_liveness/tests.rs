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
    let mut liveness = live_out(function, &indices, &predecessors);
    let mut result = vec![Vec::new(); function.blocks.len()];
    // Query out of VReg order and repeat queries to exercise cached rows.
    for value in (0..function.vregs.count()).rev() {
        for (block, values) in result.iter_mut().enumerate() {
            let live = liveness.contains(VReg(value), block);
            assert_eq!(liveness.contains(VReg(value), block), live);
            if live {
                values.push(VReg(value));
            }
        }
    }
    for values in &mut result {
        values.reverse();
    }
    result
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
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); value_count as usize]);
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
fn many_long_lifetimes_are_only_retained_when_queried() {
    const BLOCKS: usize = 256;
    const VALUES: u32 = 512;
    let mut blocks = Vec::new();
    for index in 0..BLOCKS {
        let mut block = MBlock::new(BlockId(index as u32));
        if index == 0 {
            for value in 0..VALUES {
                block.push(MInst::LoadImm {
                    dst: VReg(value),
                    value: value.into(),
                });
            }
        }
        if index + 1 == BLOCKS {
            for value in 0..VALUES {
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(value),
                    size: OpSize::S64,
                });
            }
            block.push(MInst::Return);
        } else {
            block.push(MInst::Jump {
                target: BlockId((index + 1) as u32),
            });
        }
        blocks.push(block);
    }
    let function = function(blocks, VALUES);
    let indices = (0..BLOCKS).map(|i| (BlockId(i as u32), i)).collect();
    let predecessors = (0..BLOCKS)
        .map(|i| i.checked_sub(1).into_iter().collect())
        .collect::<Vec<_>>();
    let mut liveness = live_out(&function, &indices, &predecessors);
    assert!(liveness.rows.is_empty());
    for block in 0..BLOCKS {
        assert_eq!(
            liveness.contains(VReg(VALUES - 1), block),
            block + 1 < BLOCKS
        );
    }
    assert_eq!(liveness.rows.len(), 1);
    let LiveBlocks::Dense(words) = &liveness.rows[&VReg(VALUES - 1)] else {
        panic!("long lifetimes must use a compact bitmap");
    };
    assert_eq!(std::mem::size_of_val(words.as_ref()), BLOCKS / 8);
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
