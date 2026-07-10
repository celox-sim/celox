//! Liveness analysis and global next-use distance computation.
//!
//! Implements the augmented liveness analysis from Braun & Hack Section 4.1:
//! instead of computing live-in/live-out sets, we compute next-use distance
//! maps. The join operation takes the minimum distance per variable.

use std::collections::VecDeque;

use crate::backend::native::mir::*;
use crate::{HashMap, HashSet};

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
    analyze_ignoring_phi_sources(func, &HashSet::default())
}

pub(super) fn analyze_ignoring_phi_sources(
    func: &MFunction,
    ignored_phi_sources: &HashSet<VReg>,
) -> AnalysisResult {
    let num_blocks = func.blocks.len();

    // Build block_order and block_index
    let block_order: Vec<BlockId> = func.blocks.iter().map(|b| b.id).collect();
    let mut block_index = HashMap::default();
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
    let backedge_successor_edges =
        compute_backedge_successor_edges(&successors, &backedge_successors);
    let phi_edge_uses = compute_phi_edge_uses(func, &block_order, &successors, ignored_phi_sources);
    let block_transfers = compute_block_transfers(func);

    // Initialize next-use distance maps
    let mut entry_distances: Vec<HashMap<VReg, u32>> = vec![HashMap::default(); num_blocks];
    let mut exit_distances: Vec<HashMap<VReg, u32>> = vec![HashMap::default(); num_blocks];

    let mut worklist: VecDeque<usize> = (0..num_blocks).rev().collect();
    let mut in_worklist = vec![true; num_blocks];
    while let Some(bi) = worklist.pop_front() {
        in_worklist[bi] = false;
        let (new_entry, new_exit) = compute_block_distances(
            bi,
            &successors,
            &backedge_successor_edges,
            &phi_edge_uses,
            &block_transfers,
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
    bi: usize,
    successors: &[Vec<usize>],
    backedge_successor_edges: &[Vec<bool>],
    phi_edge_uses: &[Vec<Vec<VReg>>],
    block_transfers: &[BlockTransfer],
    entry_distances: &[HashMap<VReg, u32>],
) -> (HashMap<VReg, u32>, HashMap<VReg, u32>) {
    let transfer = &block_transfers[bi];

    // Join successor entry distances. Phi sources are edge uses from this
    // predecessor into the successor, so they are live at this block's exit.
    let mut new_exit: HashMap<VReg, u32> = HashMap::default();
    let exit_capacity = successors[bi]
        .iter()
        .map(|&succ_idx| entry_distances[succ_idx].len())
        .sum::<usize>()
        + phi_edge_uses[bi]
            .iter()
            .map(|sources| sources.len())
            .sum::<usize>();
    new_exit.reserve(exit_capacity);
    for (edge_idx, &succ_idx) in successors[bi].iter().enumerate() {
        let edge_len = if backedge_successor_edges[bi][edge_idx] {
            LOOP_EXIT_LENGTH
        } else {
            0
        };
        for (&vreg, &dist) in &entry_distances[succ_idx] {
            let new_dist = dist.saturating_add(edge_len);
            let entry = new_exit.entry(vreg).or_insert(u32::MAX);
            *entry = (*entry).min(new_dist);
        }
        for &src_vreg in &phi_edge_uses[bi][edge_idx] {
            let entry = new_exit.entry(src_vreg).or_insert(u32::MAX);
            *entry = (*entry).min(edge_len);
        }
    }

    let mut new_entry: HashMap<VReg, u32> = HashMap::default();
    new_entry.reserve(new_exit.len() + transfer.local_uses.len());
    for (&vreg, &dist) in &new_exit {
        if !transfer.defs.contains(&vreg) {
            new_entry.insert(vreg, transfer.block_len.saturating_add(dist));
        }
    }
    for &(vreg, pos) in &transfer.local_uses {
        new_entry.insert(vreg, pos);
    }

    (new_entry, new_exit)
}

struct BlockTransfer {
    block_len: u32,
    defs: HashSet<VReg>,
    local_uses: Vec<(VReg, u32)>,
}

fn compute_block_transfers(func: &MFunction) -> Vec<BlockTransfer> {
    func.blocks
        .iter()
        .map(|block| {
            let mut defs = HashSet::default();
            defs.reserve(block.phis.len() + block.insts.len());
            for phi in &block.phis {
                defs.insert(phi.dst);
            }

            let mut local_uses: HashMap<VReg, u32> = HashMap::default();
            for (inst_idx, inst) in block.insts.iter().enumerate() {
                if let Some(def) = inst.def() {
                    defs.insert(def);
                }
                let pos = inst_idx as u32;
                for vreg in inst.uses() {
                    if !defs.contains(&vreg) {
                        local_uses.entry(vreg).or_insert(pos);
                    }
                }
            }

            let mut local_uses = local_uses.into_iter().collect::<Vec<_>>();
            local_uses.sort_by_key(|(vreg, _)| *vreg);
            BlockTransfer {
                block_len: block.insts.len() as u32,
                defs,
                local_uses,
            }
        })
        .collect()
}

fn compute_backedge_successor_edges(
    successors: &[Vec<usize>],
    backedge_successors: &[Vec<usize>],
) -> Vec<Vec<bool>> {
    successors
        .iter()
        .enumerate()
        .map(|(idx, succs)| {
            succs
                .iter()
                .map(|succ| backedge_successors[idx].contains(succ))
                .collect()
        })
        .collect()
}

fn compute_phi_edge_uses(
    func: &MFunction,
    block_order: &[BlockId],
    successors: &[Vec<usize>],
    ignored: &HashSet<VReg>,
) -> Vec<Vec<Vec<VReg>>> {
    let mut phi_edge_uses = successors
        .iter()
        .map(|succs| vec![Vec::new(); succs.len()])
        .collect::<Vec<_>>();

    for (pred_idx, succs) in successors.iter().enumerate() {
        let pred_id = block_order[pred_idx];
        for (edge_idx, &succ_idx) in succs.iter().enumerate() {
            let succ_block = &func.blocks[succ_idx];
            for phi in &succ_block.phis {
                for (source_pred, source) in &phi.sources {
                    if *source_pred == pred_id && !ignored.contains(source) {
                        phi_edge_uses[pred_idx][edge_idx].push(*source);
                    }
                }
            }
        }
    }

    phi_edge_uses
}

fn compute_backedge_successors(successors: &[Vec<usize>]) -> Vec<Vec<usize>> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    let mut colors = vec![Color::White; successors.len()];
    let mut backedges = vec![Vec::new(); successors.len()];
    for root in 0..successors.len() {
        if colors[root] != Color::White {
            continue;
        }
        colors[root] = Color::Gray;
        let mut stack = vec![(root, 0usize)];
        while let Some((node, next_successor)) = stack.last_mut() {
            if *next_successor == successors[*node].len() {
                colors[*node] = Color::Black;
                stack.pop();
                continue;
            }
            let successor = successors[*node][*next_successor];
            *next_successor += 1;
            match colors[successor] {
                Color::White => {
                    colors[successor] = Color::Gray;
                    stack.push((successor, 0));
                }
                Color::Gray => backedges[*node].push(successor),
                Color::Black => {}
            }
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
