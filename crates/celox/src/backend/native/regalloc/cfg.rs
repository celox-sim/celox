//! Normalized CFG information shared by every allocation phase.

use std::collections::{BTreeSet, HashMap};

use crate::backend::native::mir::{BlockId, MBlock, MFunction, MInst};

#[derive(Debug)]
pub(super) struct NaturalLoop {
    pub header: usize,
    pub blocks: BTreeSet<usize>,
    pub parent: Option<usize>,
}

#[derive(Debug)]
pub(super) struct NormalizedCfg {
    pub block_index: HashMap<BlockId, usize>,
    pub predecessors: Vec<Vec<usize>>,
    pub successors: Vec<Vec<usize>>,
    pub idom: Vec<Option<usize>>,
    pub dominance_frontier: Vec<BTreeSet<usize>>,
    pub loops: Vec<NaturalLoop>,
    pub loop_for_header: HashMap<usize, usize>,
}

impl NormalizedCfg {
    pub(super) fn verify(&self, func: &MFunction) {
        let blocks = func.blocks.len();
        assert_eq!(self.block_index.len(), blocks);
        assert_eq!(self.predecessors.len(), blocks);
        assert_eq!(self.successors.len(), blocks);
        assert_eq!(self.idom.len(), blocks);
        assert_eq!(self.dominance_frontier.len(), blocks);
        assert!(self.idom.first().is_some_and(Option::is_none));
        assert!(self.idom.iter().skip(1).all(Option::is_some));
        for (loop_index, natural_loop) in self.loops.iter().enumerate() {
            assert!(natural_loop.blocks.contains(&natural_loop.header));
            assert_eq!(self.loop_for_header[&natural_loop.header], loop_index);
            if let Some(parent) = natural_loop.parent {
                assert!(self.loops[parent].blocks.is_superset(&natural_loop.blocks));
            }
        }
        for (block, frontier) in self.dominance_frontier.iter().enumerate() {
            assert!(block < blocks);
            assert!(frontier.iter().all(|member| *member < blocks));
        }
        for (block, successors) in self.successors.iter().enumerate() {
            if successors.len() < 2 {
                continue;
            }
            for &successor in successors {
                assert_eq!(self.predecessors[successor], vec![block]);
                assert!(func.blocks[successor].phis.is_empty());
                assert!(matches!(
                    func.blocks[successor].insts.as_slice(),
                    [MInst::Jump { .. }]
                ));
            }
        }
    }
}

pub(super) fn normalize(func: &mut MFunction) -> NormalizedCfg {
    split_critical_edges(func);
    super::reorder_blocks_rpo(func);
    let (block_index, predecessors, successors) = graph(func);
    let idom = immediate_dominators(&predecessors);
    let dominance_frontier = dominance_frontiers(&predecessors, &idom);
    let loops = natural_loops(&predecessors, &successors, &idom);
    let loop_for_header = loops
        .iter()
        .enumerate()
        .map(|(loop_index, natural_loop)| (natural_loop.header, loop_index))
        .collect();
    NormalizedCfg {
        block_index,
        predecessors,
        successors,
        idom,
        dominance_frontier,
        loops,
        loop_for_header,
    }
}

fn split_critical_edges(func: &mut MFunction) {
    for block in &mut func.blocks {
        if let Some(MInst::Branch {
            true_bb, false_bb, ..
        }) = block.insts.last()
            && true_bb == false_bb
        {
            let target = *true_bb;
            *block.insts.last_mut().unwrap() = MInst::Jump { target };
        }
    }
    let (block_index, predecessors, _) = graph(func);
    let mut edges = Vec::<(BlockId, BlockId)>::new();
    for predecessor in &func.blocks {
        let mut successors = predecessor.successors();
        successors.sort();
        successors.dedup();
        if successors.len() < 2 {
            continue;
        }
        for successor in successors {
            // Every branch edge gets a dedicated insertion block.  Critical
            // edge splitting alone is insufficient for edge-local spill and
            // parallel-copy operations when the successor has one predecessor.
            let successor_index = block_index[&successor];
            let successor_block = &func.blocks[successor_index];
            let already_edge_block = predecessors[successor_index].len() == 1
                && successor_block.phis.is_empty()
                && matches!(successor_block.insts.as_slice(), [MInst::Jump { .. }]);
            if already_edge_block {
                continue;
            }
            edges.push((predecessor.id, successor));
        }
    }
    if edges.is_empty() {
        return;
    }

    let mut next_id = func
        .blocks
        .iter()
        .map(|block| block.id.0)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .expect("MIR BlockId overflow while splitting critical edges");
    for (predecessor, successor) in edges {
        let edge = BlockId(next_id);
        next_id = next_id
            .checked_add(1)
            .expect("MIR BlockId overflow while splitting critical edges");
        // New blocks are appended, so every original block keeps the index
        // recorded by the graph built above.  Looking both endpoints up in that
        // index avoids an O(blocks) scan for every split branch edge.
        let predecessor_index = block_index[&predecessor];
        rewrite_target(
            func.blocks[predecessor_index]
                .insts
                .last_mut()
                .expect("MIR block has a terminator"),
            successor,
            edge,
        );
        let successor_index = block_index[&successor];
        for phi in &mut func.blocks[successor_index].phis {
            let source = phi
                .sources
                .iter_mut()
                .find(|(source_predecessor, _)| *source_predecessor == predecessor)
                .expect("phi covers every predecessor");
            source.0 = edge;
        }
        let mut edge_block = MBlock::new(edge);
        edge_block.push(MInst::Jump { target: successor });
        func.blocks.push(edge_block);
    }
}

fn rewrite_target(terminator: &mut MInst, old: BlockId, new: BlockId) {
    match terminator {
        MInst::Branch {
            true_bb, false_bb, ..
        } => {
            if *true_bb == old {
                *true_bb = new;
            }
            if *false_bb == old {
                *false_bb = new;
            }
        }
        MInst::Jump { target } if *target == old => *target = new,
        _ => panic!("critical edge is not named by predecessor terminator"),
    }
}

fn graph(func: &MFunction) -> (HashMap<BlockId, usize>, Vec<Vec<usize>>, Vec<Vec<usize>>) {
    let block_index = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let mut predecessors = vec![Vec::new(); func.blocks.len()];
    let mut successors = vec![Vec::new(); func.blocks.len()];
    for (index, block) in func.blocks.iter().enumerate() {
        for successor in block.successors() {
            let successor = block_index[&successor];
            if !successors[index].contains(&successor) {
                successors[index].push(successor);
                predecessors[successor].push(index);
            }
        }
    }
    (block_index, predecessors, successors)
}

fn immediate_dominators(predecessors: &[Vec<usize>]) -> Vec<Option<usize>> {
    let mut idom = vec![None; predecessors.len()];
    idom[0] = Some(0);
    loop {
        let mut changed = false;
        for block in 1..predecessors.len() {
            let mut processed = predecessors[block]
                .iter()
                .copied()
                .filter(|predecessor| idom[*predecessor].is_some());
            let Some(first) = processed.next() else {
                continue;
            };
            let next = processed.fold(first, |current, predecessor| {
                intersect(current, predecessor, &idom)
            });
            if idom[block] != Some(next) {
                idom[block] = Some(next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    idom[0] = None;
    idom
}

fn intersect(mut left: usize, mut right: usize, idom: &[Option<usize>]) -> usize {
    while left != right {
        while left > right {
            left = idom[left].expect("processed block has an idom");
        }
        while right > left {
            right = idom[right].expect("processed block has an idom");
        }
    }
    left
}

fn dominates(dominator: usize, mut block: usize, idom: &[Option<usize>]) -> bool {
    loop {
        if block == dominator {
            return true;
        }
        let Some(parent) = idom[block] else {
            return false;
        };
        block = parent;
    }
}

fn dominance_frontiers(
    predecessors: &[Vec<usize>],
    idom: &[Option<usize>],
) -> Vec<BTreeSet<usize>> {
    let mut frontiers = vec![BTreeSet::new(); predecessors.len()];
    for block in 0..predecessors.len() {
        if predecessors[block].len() < 2 {
            continue;
        }
        let immediate = idom[block].expect("non-entry join has an idom");
        for &predecessor in &predecessors[block] {
            let mut runner = predecessor;
            while runner != immediate {
                frontiers[runner].insert(block);
                runner = idom[runner].expect("join predecessor is dominated by entry");
            }
        }
    }
    frontiers
}

fn natural_loops(
    predecessors: &[Vec<usize>],
    successors: &[Vec<usize>],
    idom: &[Option<usize>],
) -> Vec<NaturalLoop> {
    let mut by_header = HashMap::<usize, BTreeSet<usize>>::new();
    for (tail, tail_successors) in successors.iter().enumerate() {
        for &header in tail_successors {
            if !dominates(header, tail, idom) {
                continue;
            }
            let blocks = by_header
                .entry(header)
                .or_insert_with(|| BTreeSet::from([header]));
            let mut stack = vec![tail];
            while let Some(block) = stack.pop() {
                if blocks.insert(block) {
                    stack.extend(predecessors[block].iter().copied());
                }
            }
        }
    }
    let mut loops = by_header
        .into_iter()
        .map(|(header, blocks)| NaturalLoop {
            header,
            blocks,
            parent: None,
        })
        .collect::<Vec<_>>();
    loops.sort_by_key(|natural_loop| (natural_loop.blocks.len(), natural_loop.header));
    for child in 0..loops.len() {
        loops[child].parent = (child + 1..loops.len())
            .filter(|parent| loops[*parent].blocks.is_superset(&loops[child].blocks))
            .min_by_key(|parent| loops[*parent].blocks.len());
    }
    loops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{PhiNode, SpillDesc, VRegAllocator};

    #[test]
    fn splits_critical_edge_and_rewrites_phi_predecessor() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let left = vregs.alloc();
        let right = vregs.alloc();
        let merged = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: left,
            value: 2,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut other = MBlock::new(BlockId(1));
        other.push(MInst::LoadImm {
            dst: right,
            value: 3,
        });
        other.push(MInst::Jump { target: BlockId(3) });
        let mut critical_pred = MBlock::new(BlockId(2));
        critical_pred.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(3),
            false_bb: BlockId(4),
        });
        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: merged,
            sources: vec![(BlockId(1), right), (BlockId(2), left)],
        });
        join.push(MInst::Return);
        let mut exit = MBlock::new(BlockId(4));
        exit.push(MInst::Return);
        func.blocks = vec![entry, other, critical_pred, join, exit];

        let cfg = normalize(&mut func);
        assert_eq!(func.blocks.len(), 9);
        let join = &func.blocks[cfg.block_index[&BlockId(3)]];
        let split_predecessor = join.phis[0]
            .sources
            .iter()
            .find(|(_, value)| *value == left)
            .unwrap()
            .0;
        assert_ne!(split_predecessor, BlockId(2));
        assert_eq!(cfg.predecessors[cfg.block_index[&BlockId(3)]].len(), 2);
    }
}
