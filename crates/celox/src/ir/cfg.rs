//! SIR adapter for the IR-independent control-flow analyses.

use std::fmt;

use celox_analysis::cfg::ControlFlowGraph;
pub(crate) use celox_analysis::cfg::{
    DominatorTree, NaturalLoop, PostDominatorTree, StronglyConnectedRegion,
};

use crate::HashMap;

use super::{BlockId, ExecutionUnit, SIRTerminator};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SirCfgError {
    Empty,
    MissingEntry(BlockId),
    BlockIdentity { key: BlockId, embedded: BlockId },
    MissingTarget { source: BlockId, target: BlockId },
    Unreachable(Vec<BlockId>),
    InvalidGraph(&'static str),
}

impl fmt::Display for SirCfgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("SIR CFG has no blocks"),
            Self::MissingEntry(entry) => write!(formatter, "SIR CFG entry b{} is absent", entry.0),
            Self::BlockIdentity { key, embedded } => write!(
                formatter,
                "SIR block map key b{} contains block b{}",
                key.0, embedded.0
            ),
            Self::MissingTarget { source, target } => write!(
                formatter,
                "SIR block b{} targets absent block b{}",
                source.0, target.0
            ),
            Self::Unreachable(blocks) => {
                formatter.write_str("SIR CFG contains unreachable blocks:")?;
                for block in blocks {
                    write!(formatter, " b{}", block.0)?;
                }
                Ok(())
            }
            Self::InvalidGraph(message) => write!(formatter, "invalid SIR CFG: {message}"),
        }
    }
}

impl std::error::Error for SirCfgError {}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Some analyses are intentionally ahead of their first consumer.
pub(crate) struct SirCfg {
    /// Reachable blocks in deterministic reverse postorder. The entry is 0.
    pub block_ids: Vec<BlockId>,
    pub index: HashMap<BlockId, usize>,
    pub predecessors: Vec<Vec<usize>>,
    pub successors: Vec<Vec<usize>>,
    pub dominators: DominatorTree,
    pub dom_children: Vec<Vec<usize>>,
    pub dominance_frontier: Vec<Vec<usize>>,
    pub postdominators: PostDominatorTree,
    pub postdominance_frontier: Vec<Vec<usize>>,
    /// For each block, branches on which its execution is control-dependent.
    pub controllers: Vec<Vec<usize>>,
    /// For each branch, blocks whose execution it controls.
    pub control_dependents: Vec<Vec<usize>>,
    pub sccs: Vec<StronglyConnectedRegion>,
    pub scc_for_block: Vec<usize>,
    pub loops: Vec<NaturalLoop>,
}

#[allow(dead_code)]
impl SirCfg {
    pub(crate) fn analyze<A>(eu: &ExecutionUnit<A>) -> Result<Self, SirCfgError> {
        if eu.blocks.is_empty() {
            return Err(SirCfgError::Empty);
        }
        if !eu.blocks.contains_key(&eu.entry_block_id) {
            return Err(SirCfgError::MissingEntry(eu.entry_block_id));
        }

        let mut source_block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
        source_block_ids.sort_unstable();
        let source_index = source_block_ids
            .iter()
            .copied()
            .enumerate()
            .map(|(index, block)| (block, index))
            .collect::<HashMap<_, _>>();
        let mut source_successors = vec![Vec::new(); source_block_ids.len()];
        for (source, &block_id) in source_block_ids.iter().enumerate() {
            let block = &eu.blocks[&block_id];
            if block.id != block_id {
                return Err(SirCfgError::BlockIdentity {
                    key: block_id,
                    embedded: block.id,
                });
            }
            let mut outgoing = terminator_successors(&block.terminator);
            outgoing.sort_unstable();
            outgoing.dedup();
            for target in outgoing {
                let Some(&target_index) = source_index.get(&target) else {
                    return Err(SirCfgError::MissingTarget {
                        source: block_id,
                        target,
                    });
                };
                source_successors[source].push(target_index);
            }
        }

        let source_entry = source_index[&eu.entry_block_id];
        let order = celox_analysis::cfg::reverse_postorder(&source_successors, source_entry)
            .map_err(map_analysis_error)?;
        if order.len() != source_block_ids.len() {
            let mut reached = vec![false; source_block_ids.len()];
            for &block in &order {
                reached[block] = true;
            }
            return Err(SirCfgError::Unreachable(
                source_block_ids
                    .iter()
                    .copied()
                    .enumerate()
                    .filter_map(|(index, block)| (!reached[index]).then_some(block))
                    .collect(),
            ));
        }

        let block_ids = order
            .iter()
            .map(|&source| source_block_ids[source])
            .collect::<Vec<_>>();
        let index = block_ids
            .iter()
            .copied()
            .enumerate()
            .map(|(index, block)| (block, index))
            .collect::<HashMap<_, _>>();
        let mut successors = vec![Vec::new(); block_ids.len()];
        for (block, &source) in order.iter().enumerate() {
            successors[block] = source_successors[source]
                .iter()
                .map(|&target| index[&source_block_ids[target]])
                .collect();
        }

        let analysis = ControlFlowGraph::analyze(successors, 0).map_err(map_analysis_error)?;
        let dom_children = analysis.dominators.children.clone();
        Ok(Self {
            block_ids,
            index,
            predecessors: analysis.predecessors,
            successors: analysis.successors,
            dominators: analysis.dominators,
            dom_children,
            dominance_frontier: analysis.dominance_frontier,
            postdominators: analysis.postdominators,
            postdominance_frontier: analysis.postdominance_frontier,
            controllers: analysis.controllers,
            control_dependents: analysis.control_dependents,
            sccs: analysis.sccs,
            scc_for_block: analysis.scc_for_block,
            loops: analysis.loops,
        })
    }

    pub(crate) fn block_index(&self, block: BlockId) -> Option<usize> {
        self.index.get(&block).copied()
    }

    pub(crate) fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        let (Some(dominator), Some(block)) = (self.block_index(dominator), self.block_index(block))
        else {
            return false;
        };
        self.dominators.dominates(dominator, block)
    }

    pub(crate) fn postdominates(&self, postdominator: BlockId, block: BlockId) -> bool {
        let (Some(postdominator), Some(block)) =
            (self.block_index(postdominator), self.block_index(block))
        else {
            return false;
        };
        self.postdominators.postdominates(postdominator, block)
    }

    pub(crate) fn common_postdominator(&self, left: BlockId, right: BlockId) -> Option<BlockId> {
        let left = self.block_index(left)?;
        let right = self.block_index(right)?;
        self.postdominators
            .common_postdominator(left, right)
            .map(|block| self.block_ids[block])
    }
}

impl celox_analysis::ssa::SsaCfg for SirCfg {
    type FrontierIter<'a> = std::iter::Copied<std::slice::Iter<'a, usize>>;

    fn root(&self) -> usize {
        0
    }

    fn predecessors(&self) -> &[Vec<usize>] {
        &self.predecessors
    }

    fn successors(&self) -> &[Vec<usize>] {
        &self.successors
    }

    fn dominator_children(&self) -> &[Vec<usize>] {
        &self.dom_children
    }

    fn dominance_frontier_len(&self) -> usize {
        self.dominance_frontier.len()
    }

    fn dominance_frontier(&self, block: usize) -> Self::FrontierIter<'_> {
        self.dominance_frontier[block].iter().copied()
    }
}

fn map_analysis_error(error: celox_analysis::cfg::CfgError) -> SirCfgError {
    match error {
        celox_analysis::cfg::CfgError::InvalidGraph(message) => SirCfgError::InvalidGraph(message),
        celox_analysis::cfg::CfgError::Empty => SirCfgError::Empty,
        celox_analysis::cfg::CfgError::Unreachable(_) => {
            SirCfgError::InvalidGraph("shared CFG unexpectedly found an unreachable SIR block")
        }
        celox_analysis::cfg::CfgError::InvalidRoot { .. }
        | celox_analysis::cfg::CfgError::EdgeOutOfRange { .. } => {
            SirCfgError::InvalidGraph("shared CFG rejected validated SIR block indices")
        }
    }
}

pub(crate) fn terminator_successors(terminator: &SIRTerminator) -> Vec<BlockId> {
    match terminator {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|case| case.target)
            .chain(std::iter::once(*default))
            .collect(),
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use crate::HashMap;
    use crate::ir::{BasicBlock, RegisterType};

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
        assert!(cfg.postdominates(BlockId(2), BlockId(0)));
        assert_eq!(
            cfg.common_postdominator(BlockId(0), BlockId(1)),
            Some(BlockId(1))
        );
    }

    #[test]
    fn computes_diamond_frontiers_and_control_dependence() {
        let condition = crate::ir::RegisterId(0);
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
    fn computes_nested_diamond_control_dependence() {
        let condition = crate::ir::RegisterId(0);
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
        let condition = crate::ir::RegisterId(0);
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
        assert_eq!(
            cfg.postdominators
                .immediate_postdominator(cfg.index[&BlockId(0)]),
            None
        );
    }

    #[test]
    fn finds_natural_loop_and_irreducible_scc() {
        let condition = crate::ir::RegisterId(0);
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

        let condition = crate::ir::RegisterId(0);
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

    fn reference_frontier(
        predecessors: &[Vec<usize>],
        dominators: &DominatorTree,
    ) -> Vec<Vec<usize>> {
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
        let condition = crate::ir::RegisterId(0);
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
}
