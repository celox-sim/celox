use super::*;
use std::collections::BTreeSet;

use crate::HashMap;
use crate::{BasicBlock, RegisterType};

fn block(id: usize, terminator: SIRTerminator) -> BasicBlock<()> {
    BasicBlock {
        id: BlockId(id),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator,
    }
}

fn eu(entry: usize, blocks: Vec<BasicBlock<()>>) -> ExecutionUnit<()> {
    ExecutionUnit {
        entry_block_id: BlockId(entry),
        blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
        register_map: HashMap::<_, RegisterType>::default(),
    }
}

#[test]
fn dominance_only_queries_match_paths_with_each_vertex_removed() {
    const BLOCKS: usize = 64;
    let ids = (0..BLOCKS)
        .map(|index| BlockId((index * 17 % BLOCKS) * 5 + 7))
        .collect::<Vec<_>>();
    for seed in 0..8 {
        let successors = (0..BLOCKS)
            .map(|index| {
                if index + 1 == BLOCKS {
                    Vec::new()
                } else {
                    vec![index + 1, (index * 7 + seed) % BLOCKS]
                }
            })
            .collect::<Vec<_>>();
        let unit = eu(
            ids[0].0,
            successors
                .iter()
                .enumerate()
                .map(|(index, outgoing)| {
                    block(
                        ids[index].0,
                        if outgoing.is_empty() {
                            SIRTerminator::Return
                        } else {
                            SIRTerminator::Branch {
                                cond: crate::RegisterId(0),
                                true_block: (ids[outgoing[0]], Vec::new()),
                                false_block: (ids[outgoing[1]], Vec::new()),
                            }
                        },
                    )
                })
                .collect(),
        );
        let queries = SirDominance::analyze(&unit).unwrap();
        let full = SirCfg::analyze_forward_structure(&unit).unwrap();
        for removed in 0..BLOCKS {
            let mut reached = [false; BLOCKS];
            let mut pending = if removed == 0 { Vec::new() } else { vec![0] };
            while let Some(block) = pending.pop() {
                if reached[block] {
                    continue;
                }
                reached[block] = true;
                pending.extend(
                    successors[block]
                        .iter()
                        .copied()
                        .filter(|&next| next != removed),
                );
            }
            for target in 0..BLOCKS {
                assert_eq!(
                    queries.dominates(ids[removed], ids[target]),
                    !reached[target]
                );
                assert_eq!(
                    queries.dominates(ids[removed], ids[target]),
                    full.dominates(ids[removed], ids[target])
                );
            }
        }
        assert!(!queries.dominates(ids[0], BlockId(usize::MAX)));
        assert!(!queries.dominates(BlockId(usize::MAX), ids[0]));
    }
    let unreachable = eu(
        0,
        vec![
            block(0, SIRTerminator::Return),
            block(1, SIRTerminator::Return),
        ],
    );
    assert_eq!(
        SirDominance::analyze(&unreachable).unwrap_err(),
        SirCfgError::Unreachable(vec![BlockId(1)])
    );
}

#[test]
fn analyzes_linear_cfg() {
    let cfg = SirCfg::analyze(&eu(
        0,
        vec![
            block(0, SIRTerminator::Jump(BlockId(1), Vec::new())),
            block(1, SIRTerminator::Jump(BlockId(2), Vec::new())),
            block(2, SIRTerminator::Return),
        ],
    ))
    .unwrap();

    assert_eq!(cfg.block_ids, vec![BlockId(0), BlockId(1), BlockId(2)]);
    assert_eq!(cfg.dominators.idom, vec![None, Some(0), Some(1)]);
    assert!(cfg.dominates(BlockId(0), BlockId(2)));
    assert_eq!(
        cfg.common_dominator(BlockId(1), BlockId(2)),
        Some(BlockId(1))
    );
    assert!(cfg.postdominates(BlockId(2), BlockId(0)));
    assert_eq!(
        cfg.common_postdominator(BlockId(0), BlockId(1)),
        Some(BlockId(1))
    );
}

#[test]
fn computes_diamond_frontiers_and_control_dependence() {
    let condition = crate::RegisterId(0);
    let mut unit = eu(
        0,
        vec![
            block(
                0,
                SIRTerminator::Branch {
                    cond: condition,
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            ),
            block(1, SIRTerminator::Jump(BlockId(3), Vec::new())),
            block(2, SIRTerminator::Jump(BlockId(3), Vec::new())),
            block(3, SIRTerminator::Return),
        ],
    );
    unit.register_map.insert(
        condition,
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    let cfg = SirCfg::analyze(&unit).unwrap();
    let true_block = cfg.index[&BlockId(1)];
    let false_block = cfg.index[&BlockId(2)];
    let branch = cfg.index[&BlockId(0)];
    let join = cfg.index[&BlockId(3)];

    assert_eq!(cfg.dominance_frontier[true_block], vec![join]);
    assert_eq!(cfg.dominance_frontier[false_block], vec![join]);
    assert_eq!(cfg.controllers[true_block], vec![branch]);
    assert_eq!(cfg.controllers[false_block], vec![branch]);
    let mut arms = vec![true_block, false_block];
    arms.sort_unstable();
    assert_eq!(cfg.control_dependents[branch], arms);
    assert_eq!(
        cfg.common_postdominator(BlockId(1), BlockId(2)),
        Some(BlockId(3))
    );
}

#[test]
fn forward_analysis_keeps_ssa_frontiers_and_omits_dense_control_tables() {
    let condition = crate::RegisterId(0);
    let mut unit = eu(
        0,
        vec![
            block(
                0,
                SIRTerminator::Branch {
                    cond: condition,
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            ),
            block(1, SIRTerminator::Jump(BlockId(3), Vec::new())),
            block(2, SIRTerminator::Jump(BlockId(3), Vec::new())),
            block(3, SIRTerminator::Return),
        ],
    );
    unit.register_map.insert(
        condition,
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    let full = SirCfg::analyze(&unit).unwrap();
    let forward = SirCfg::analyze_forward(&unit).unwrap();
    assert_eq!(forward.block_ids, full.block_ids);
    assert_eq!(forward.dominators, full.dominators);
    assert_eq!(forward.dominance_frontier, full.dominance_frontier);
    assert_eq!(forward.sccs, full.sccs);
    assert!(forward.postdominators.is_none());
    assert!(forward.controllers.iter().all(Vec::is_empty));
    assert!(forward.control_dependents.iter().all(Vec::is_empty));
}

#[test]
fn computes_nested_diamond_control_dependence() {
    let condition = crate::RegisterId(0);
    let mut unit = eu(
        0,
        vec![
            block(
                0,
                SIRTerminator::Branch {
                    cond: condition,
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            ),
            block(
                1,
                SIRTerminator::Branch {
                    cond: condition,
                    true_block: (BlockId(3), Vec::new()),
                    false_block: (BlockId(4), Vec::new()),
                },
            ),
            block(2, SIRTerminator::Jump(BlockId(5), Vec::new())),
            block(3, SIRTerminator::Jump(BlockId(5), Vec::new())),
            block(4, SIRTerminator::Jump(BlockId(5), Vec::new())),
            block(5, SIRTerminator::Return),
        ],
    );
    unit.register_map.insert(
        condition,
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    let cfg = SirCfg::analyze(&unit).unwrap();
    let outer = cfg.index[&BlockId(0)];
    let inner = cfg.index[&BlockId(1)];
    let outer_false = cfg.index[&BlockId(2)];
    let inner_true = cfg.index[&BlockId(3)];
    let inner_false = cfg.index[&BlockId(4)];

    assert_eq!(cfg.controllers[inner], vec![outer]);
    assert_eq!(cfg.controllers[outer_false], vec![outer]);
    assert_eq!(cfg.controllers[inner_true], vec![inner]);
    assert_eq!(cfg.controllers[inner_false], vec![inner]);
    assert_eq!(
        cfg.common_postdominator(BlockId(1), BlockId(2)),
        Some(BlockId(5))
    );
    assert_eq!(
        cfg.common_postdominator(BlockId(3), BlockId(4)),
        Some(BlockId(5))
    );
}

#[test]
fn handles_multiple_exits_with_a_virtual_postdominator_root() {
    let condition = crate::RegisterId(0);
    let mut unit = eu(
        0,
        vec![
            block(
                0,
                SIRTerminator::Branch {
                    cond: condition,
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            ),
            block(1, SIRTerminator::Return),
            block(2, SIRTerminator::Error(1)),
        ],
    );
    unit.register_map.insert(
        condition,
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    let cfg = SirCfg::analyze(&unit).unwrap();

    assert_eq!(cfg.common_postdominator(BlockId(1), BlockId(2)), None);
    assert!(!cfg.postdominates(BlockId(1), BlockId(0)));
    assert_eq!(cfg.immediate_postdominator(cfg.index[&BlockId(0)]), None);
}

#[test]
fn finds_natural_loop_and_irreducible_scc() {
    let condition = crate::RegisterId(0);
    let mut natural = eu(
        0,
        vec![
            block(0, SIRTerminator::Jump(BlockId(1), Vec::new())),
            block(
                1,
                SIRTerminator::Branch {
                    cond: condition,
                    true_block: (BlockId(2), Vec::new()),
                    false_block: (BlockId(3), Vec::new()),
                },
            ),
            block(2, SIRTerminator::Jump(BlockId(1), Vec::new())),
            block(3, SIRTerminator::Return),
        ],
    );
    natural.register_map.insert(
        condition,
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    let natural_cfg = SirCfg::analyze(&natural).unwrap();
    assert_eq!(natural_cfg.loops.len(), 1);
    assert_eq!(
        natural_cfg.block_ids[natural_cfg.loops[0].header],
        BlockId(1)
    );

    let mut irreducible = eu(
        0,
        vec![
            block(
                0,
                SIRTerminator::Branch {
                    cond: condition,
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            ),
            block(1, SIRTerminator::Jump(BlockId(2), Vec::new())),
            block(
                2,
                SIRTerminator::Branch {
                    cond: condition,
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(3), Vec::new()),
                },
            ),
            block(3, SIRTerminator::Return),
        ],
    );
    irreducible.register_map.insert(
        condition,
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    let irreducible_cfg = SirCfg::analyze(&irreducible).unwrap();
    let region = irreducible_cfg
        .sccs
        .iter()
        .find(|region| region.cyclic && region.blocks.len() == 2)
        .unwrap();
    assert_eq!(region.entries.len(), 2);
    assert_eq!(region.reducible_header, None);
}

#[test]
fn reports_missing_and_unreachable_blocks() {
    let missing = eu(
        0,
        vec![block(0, SIRTerminator::Jump(BlockId(9), Vec::new()))],
    );
    assert_eq!(
        SirCfg::analyze(&missing).unwrap_err(),
        SirCfgError::MissingTarget {
            source: BlockId(0),
            target: BlockId(9),
        }
    );

    let unreachable = eu(
        0,
        vec![
            block(0, SIRTerminator::Return),
            block(1, SIRTerminator::Return),
        ],
    );
    assert_eq!(
        SirCfg::analyze(&unreachable).unwrap_err(),
        SirCfgError::Unreachable(vec![BlockId(1)])
    );
}

#[test]
fn deep_chain_uses_no_recursive_graph_walk() {
    const BLOCKS: usize = 20_000;
    let mut blocks = Vec::with_capacity(BLOCKS);
    for id in 0..BLOCKS - 1 {
        blocks.push(block(id, SIRTerminator::Jump(BlockId(id + 1), Vec::new())));
    }
    blocks.push(block(BLOCKS - 1, SIRTerminator::Return));

    let cfg = SirCfg::analyze(&eu(0, blocks)).unwrap();

    assert_eq!(cfg.block_ids.len(), BLOCKS);
    assert!(cfg.dominates(BlockId(0), BlockId(BLOCKS - 1)));
    assert!(cfg.postdominates(BlockId(BLOCKS - 1), BlockId(0)));
}

#[test]
fn analyzes_large_wide_cfg() {
    const LEAVES: usize = 4_096;
    const TREE_BLOCKS: usize = LEAVES * 2 - 1;
    const JOIN: usize = TREE_BLOCKS;

    let condition = crate::RegisterId(0);
    let mut blocks = Vec::with_capacity(TREE_BLOCKS + 1);
    for id in 0..LEAVES - 1 {
        blocks.push(block(
            id,
            SIRTerminator::Branch {
                cond: condition,
                true_block: (BlockId(id * 2 + 1), Vec::new()),
                false_block: (BlockId(id * 2 + 2), Vec::new()),
            },
        ));
    }
    for id in LEAVES - 1..TREE_BLOCKS {
        blocks.push(block(id, SIRTerminator::Jump(BlockId(JOIN), Vec::new())));
    }
    blocks.push(block(JOIN, SIRTerminator::Return));
    let mut unit = eu(0, blocks);
    unit.register_map.insert(
        condition,
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );

    let cfg = SirCfg::analyze(&unit).unwrap();
    let join = cfg.index[&BlockId(JOIN)];

    assert_eq!(cfg.block_ids.len(), TREE_BLOCKS + 1);
    assert!(cfg.dominates(BlockId(0), BlockId(JOIN)));
    assert!(cfg.postdominates(BlockId(JOIN), BlockId(0)));
    for id in LEAVES - 1..TREE_BLOCKS {
        assert_eq!(cfg.dominance_frontier[cfg.index[&BlockId(id)]], [join]);
    }
}

fn reference_idom(predecessors: &[Vec<usize>], root: usize) -> Vec<Option<usize>> {
    let blocks = predecessors.len();
    let universe = (0..blocks).collect::<BTreeSet<_>>();
    let mut dominators = vec![universe; blocks];
    dominators[root] = BTreeSet::from([root]);
    loop {
        let mut changed = false;
        for block in 0..blocks {
            if block == root {
                continue;
            }
            let mut incoming = predecessors[block].iter().copied();
            let Some(first) = incoming.next() else {
                continue;
            };
            let mut next = dominators[first].clone();
            for predecessor in incoming {
                next.retain(|candidate| dominators[predecessor].contains(candidate));
            }
            next.insert(block);
            if dominators[block] != next {
                dominators[block] = next;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    (0..blocks)
        .map(|block| {
            if block == root {
                return None;
            }
            dominators[block]
                .iter()
                .copied()
                .filter(|candidate| *candidate != block)
                .max_by_key(|candidate| dominators[*candidate].len())
        })
        .collect()
}

fn reference_frontier(predecessors: &[Vec<usize>], dominators: &DominatorTree) -> Vec<Vec<usize>> {
    (0..predecessors.len())
        .map(|candidate| {
            (0..predecessors.len())
                .filter(|&join| {
                    predecessors[join]
                        .iter()
                        .any(|predecessor| dominators.dominates(candidate, *predecessor))
                        && (candidate == join || !dominators.dominates(candidate, join))
                })
                .collect()
        })
        .collect()
}

#[test]
fn lengauer_tarjan_and_frontiers_match_set_equations() {
    let condition = crate::RegisterId(0);
    let mut random = 0x4d59_5df4_d0f3_3173u64;
    for blocks in 2..40usize {
        for _case in 0..20 {
            let mut graph_blocks = Vec::with_capacity(blocks);
            for id in 0..blocks - 1 {
                random = random
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let alternate = (random as usize) % blocks;
                let terminator = if random & 3 == 0 {
                    SIRTerminator::Branch {
                        cond: condition,
                        true_block: (BlockId(id + 1), Vec::new()),
                        false_block: (BlockId(alternate), Vec::new()),
                    }
                } else {
                    SIRTerminator::Jump(BlockId(id + 1), Vec::new())
                };
                graph_blocks.push(block(id, terminator));
            }
            graph_blocks.push(block(blocks - 1, SIRTerminator::Return));
            let mut unit = eu(0, graph_blocks);
            unit.register_map.insert(
                condition,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            let cfg = SirCfg::analyze(&unit).unwrap();

            assert_eq!(
                cfg.dominators.idom,
                reference_idom(&cfg.predecessors, 0),
                "dominator mismatch for {}-block generated graph",
                blocks
            );
            assert_eq!(
                cfg.dominance_frontier,
                reference_frontier(&cfg.predecessors, &cfg.dominators),
                "frontier mismatch for {}-block generated graph",
                blocks
            );
        }
    }
}
