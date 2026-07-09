//! Liveness analysis and global next-use distance computation.
//!
//! Implements the augmented liveness analysis from Braun & Hack Section 4.1:
//! instead of computing live-in/live-out sets, we compute next-use distance
//! maps. The join operation takes the minimum distance per variable.

use std::collections::{HashMap, VecDeque};

use crate::backend::native::mir::*;

/// Large constant used as edge length for loop-exit edges.
/// Ensures that uses behind loops have larger distances than uses inside loops.
const LOOP_EXIT_LENGTH: u32 = 100_000;

/// Analysis result for the entire function.
pub struct AnalysisResult {
    /// For each block, the next-use distances at block entry.
    /// `entry_distances[block_idx][vreg] = distance`
    pub entry_distances: Vec<HashMap<VReg, u32>>,
    /// For each block, the next-use distances at block exit.
    pub exit_distances: Vec<HashMap<VReg, u32>>,
    /// Predecessor list for each block (by index).
    pub predecessors: Vec<Vec<usize>>,
    /// Successor indices that are real DFS backedges for each block.
    pub backedge_successors: Vec<Vec<usize>>,
}

/// Compute liveness and next-use distances for the entire MFunction.
pub fn analyze(func: &MFunction) -> AnalysisResult {
    let num_blocks = func.blocks.len();

    // Build block_order and block_index
    let block_order: Vec<BlockId> = func.blocks.iter().map(|b| b.id).collect();
    let mut block_index = HashMap::new();
    for (i, &bid) in block_order.iter().enumerate() {
        block_index.insert(bid, i);
    }

    // Build predecessor and successor lists
    let mut predecessors = vec![Vec::new(); num_blocks];
    let mut successors = vec![Vec::new(); num_blocks];
    for (i, block) in func.blocks.iter().enumerate() {
        for succ_bid in block.successors() {
            if let Some(&succ_idx) = block_index.get(&succ_bid) {
                successors[i].push(succ_idx);
                predecessors[succ_idx].push(i);
            }
        }
    }
    let backedge_successors = compute_backedge_successors(&successors);

    // Initialize next-use distance maps
    let mut entry_distances: Vec<HashMap<VReg, u32>> = vec![HashMap::new(); num_blocks];
    let mut exit_distances: Vec<HashMap<VReg, u32>> = vec![HashMap::new(); num_blocks];

    let mut worklist: VecDeque<usize> = (0..num_blocks).rev().collect();
    let mut in_worklist = vec![true; num_blocks];
    while let Some(bi) = worklist.pop_front() {
        in_worklist[bi] = false;
        let (new_entry, new_exit) = compute_block_distances(
            func,
            bi,
            &block_order,
            &successors,
            &backedge_successors,
            &entry_distances,
        );

        if new_entry != entry_distances[bi] || new_exit != exit_distances[bi] {
            entry_distances[bi] = new_entry;
            exit_distances[bi] = new_exit;
            for &pred in &predecessors[bi] {
                if !in_worklist[pred] {
                    worklist.push_back(pred);
                    in_worklist[pred] = true;
                }
            }
        }
    }

    AnalysisResult {
        entry_distances,
        exit_distances,
        predecessors,
        backedge_successors,
    }
}

fn compute_block_distances(
    func: &MFunction,
    bi: usize,
    block_order: &[BlockId],
    successors: &[Vec<usize>],
    backedge_successors: &[Vec<usize>],
    entry_distances: &[HashMap<VReg, u32>],
) -> (HashMap<VReg, u32>, HashMap<VReg, u32>) {
    let block = &func.blocks[bi];
    let block_len = block.insts.len() as u32;

    // Join successor entry distances. Phi sources are edge uses from this
    // predecessor into the successor, so they are live at this block's exit.
    let mut new_exit: HashMap<VReg, u32> = HashMap::new();
    let my_block_id = block_order[bi];
    for &succ_idx in &successors[bi] {
        let edge_len = if backedge_successors[bi].contains(&succ_idx) {
            LOOP_EXIT_LENGTH
        } else {
            0
        };
        for (&vreg, &dist) in &entry_distances[succ_idx] {
            let new_dist = dist.saturating_add(edge_len);
            let entry = new_exit.entry(vreg).or_insert(u32::MAX);
            *entry = (*entry).min(new_dist);
        }
        let succ_block = &func.blocks[succ_idx];
        for phi in &succ_block.phis {
            for (pred_id, src_vreg) in &phi.sources {
                if *pred_id == my_block_id {
                    let entry = new_exit.entry(*src_vreg).or_insert(u32::MAX);
                    *entry = (*entry).min(edge_len);
                }
            }
        }
    }

    // Transfer function (Braun & Hack Section 4.1):
    // start with successor uses shifted by this block length, then override
    // with the first in-block use seen while walking backward.
    let mut new_entry: HashMap<VReg, u32> = HashMap::new();
    for (&vreg, &dist) in &new_exit {
        new_entry.insert(vreg, block_len.saturating_add(dist));
    }
    for (inst_idx, inst) in block.insts.iter().enumerate().rev() {
        let pos = inst_idx as u32;
        for vreg in inst.uses() {
            new_entry.insert(vreg, pos);
        }
        if let Some(def) = inst.def() {
            new_entry.remove(&def);
        }
    }
    for phi in &block.phis {
        new_entry.remove(&phi.dst);
    }

    (new_entry, new_exit)
}

fn compute_backedge_successors(successors: &[Vec<usize>]) -> Vec<Vec<usize>> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    fn dfs(
        node: usize,
        successors: &[Vec<usize>],
        colors: &mut [Color],
        backedges: &mut [Vec<usize>],
    ) {
        colors[node] = Color::Gray;
        for &succ in &successors[node] {
            match colors[succ] {
                Color::White => dfs(succ, successors, colors, backedges),
                Color::Gray => backedges[node].push(succ),
                Color::Black => {}
            }
        }
        colors[node] = Color::Black;
    }

    let mut colors = vec![Color::White; successors.len()];
    let mut backedges = vec![Vec::new(); successors.len()];
    for node in 0..successors.len() {
        if colors[node] == Color::White {
            dfs(node, successors, &mut colors, &mut backedges);
        }
    }
    backedges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_func(blocks: Vec<MBlock>) -> MFunction {
        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        func.blocks = blocks;
        func
    }

    #[test]
    fn lower_index_successor_is_not_backedge_in_dag() {
        let mut b0 = MBlock::new(BlockId(0));
        b0.push(MInst::Jump { target: BlockId(1) });

        let mut b2 = MBlock::new(BlockId(2));
        b2.push(MInst::Return);

        let mut b1 = MBlock::new(BlockId(1));
        b1.push(MInst::Jump { target: BlockId(2) });

        // Layout order is intentionally not topological: b1 jumps to b2,
        // whose layout index is lower. This must not be treated as a loop.
        let analysis = analyze(&empty_func(vec![b0, b2, b1]));
        assert!(analysis.backedge_successors[2].is_empty());
    }

    #[test]
    fn dfs_gray_edge_is_backedge() {
        let mut b0 = MBlock::new(BlockId(0));
        b0.push(MInst::Jump { target: BlockId(1) });

        let mut b1 = MBlock::new(BlockId(1));
        b1.push(MInst::Jump { target: BlockId(0) });

        let analysis = analyze(&empty_func(vec![b0, b1]));
        assert_eq!(analysis.backedge_successors[1], vec![0]);
    }

    #[test]
    fn long_reverse_layout_dag_converges_without_iteration_cap() {
        let block_count = 160usize;
        let live = VReg(0);
        let mut blocks = Vec::new();

        for i in (0..block_count).rev() {
            let mut block = MBlock::new(BlockId(i as u32));
            if i + 1 == block_count {
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: live,
                    size: OpSize::S64,
                });
                block.push(MInst::Return);
            } else {
                block.push(MInst::Jump {
                    target: BlockId((i + 1) as u32),
                });
            }
            blocks.push(block);
        }

        let analysis = analyze(&empty_func(blocks));
        assert!(
            analysis.entry_distances[block_count - 1].contains_key(&live),
            "live value should propagate to BlockId(0) through the long DAG"
        );
    }
}
