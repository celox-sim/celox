//! Liveness analysis and global next-use distance computation.
//!
//! Implements the augmented liveness analysis from Braun & Hack Section 4.1:
//! instead of computing live-in/live-out sets, we compute next-use distance
//! maps. The join operation takes the minimum distance per variable.

use std::collections::{BTreeMap, BTreeSet};

use crate::backend::native::mir::*;

/// Large constant used as edge length for loop-exit edges.
/// Ensures that uses behind loops have larger distances than uses inside loops.
const LOOP_EXIT_LENGTH: u32 = 100_000;

/// Analysis result for the entire function.
pub struct AnalysisResult {
    /// For each block, the next-use distances at block entry.
    /// `entry_distances[block_idx][vreg] = distance`
    pub entry_distances: Vec<BTreeMap<VReg, u32>>,
    /// For each block, the next-use distances at block exit.
    pub exit_distances: Vec<BTreeMap<VReg, u32>>,
    /// Block layout order (index → BlockId).
    pub block_order: Vec<BlockId>,
    /// Reverse map: BlockId → index in block_order.
    pub block_index: BTreeMap<BlockId, usize>,
    /// Predecessor list for each block (by index).
    pub predecessors: Vec<Vec<usize>>,
    /// Successor list for each block (by index).
    pub successors: Vec<Vec<usize>>,
    /// Maximum register pressure within each block.
    pub max_pressure: Vec<usize>,
}

/// Compute liveness and next-use distances for the entire MFunction.
pub fn analyze(func: &MFunction) -> AnalysisResult {
    let num_blocks = func.blocks.len();

    // Build block_order and block_index
    let block_order: Vec<BlockId> = func.blocks.iter().map(|b| b.id).collect();
    let mut block_index = BTreeMap::new();
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

    // Initialize next-use distance maps
    let mut entry_distances: Vec<BTreeMap<VReg, u32>> =
        vec![BTreeMap::new(); num_blocks];
    let mut exit_distances: Vec<BTreeMap<VReg, u32>> =
        vec![BTreeMap::new(); num_blocks];

    // Iterative data-flow analysis (backward)
    // Compute next-use distances from block exits to entries.
    //
    // Transfer function (Braun & Hack Section 4.1):
    //   For variable v with use in block B at position ν_B(v):
    //     f_B(a)(v) = ℓ_B + ν_B(v)
    //   For variable v with no use in B:
    //     f_B(a)(v) = ℓ_B + |B| + a(v)
    //
    // Join: min of successors' entry distances (+ edge length)

    let mut changed = true;
    let mut iteration = 0;
    while changed {
        changed = false;
        iteration += 1;
        if iteration > 100 {
            // Safety valve — should converge quickly for reducible CFGs
            break;
        }

        // Process blocks in reverse post order (we iterate backwards)
        for bi in (0..num_blocks).rev() {
            let block = &func.blocks[bi];
            let block_len = block.insts.len() as u32;

            // Compute exit distances by joining successor entry distances
            let mut new_exit: BTreeMap<VReg, u32> = BTreeMap::new();
            let my_block_id = block_order[bi];
            for &succ_idx in &successors[bi] {
                // Edge length: 0 for normal edges, LOOP_EXIT_LENGTH for back edges
                let edge_len = if succ_idx <= bi {
                    LOOP_EXIT_LENGTH // back edge (loop)
                } else {
                    0
                };
                for (&vreg, &dist) in &entry_distances[succ_idx] {
                    let new_dist = dist.saturating_add(edge_len);
                    let entry = new_exit.entry(vreg).or_insert(u32::MAX);
                    *entry = (*entry).min(new_dist);
                }
                // Phi sources: if the successor has phi nodes, the source VRegs
                // from this predecessor are "used" at the edge (distance 0 from
                // successor entry).
                let succ_block = &func.blocks[succ_idx];
                for phi in &succ_block.phis {
                    for (pred_id, src_vreg) in &phi.sources {
                        if *pred_id == my_block_id {
                            let new_dist = 0u32.saturating_add(edge_len);
                            let entry = new_exit.entry(*src_vreg).or_insert(u32::MAX);
                            *entry = (*entry).min(new_dist);
                        }
                    }
                }
            }

            // Compute entry distances via transfer function
            // Walk instructions backwards to find first use of each vreg
            let mut new_entry: BTreeMap<VReg, u32> = BTreeMap::new();

            // Start with exit distances (for variables not used in this block)
            for (&vreg, &dist) in &new_exit {
                new_entry.insert(vreg, block_len + dist);
            }

            // Walk backwards, updating with actual use positions
            for (inst_idx, inst) in block.insts.iter().enumerate().rev() {
                let pos = inst_idx as u32;

                // Uses: set distance to this position
                for vreg in inst.uses() {
                    new_entry.insert(vreg, pos);
                }

                // Defs: remove from map (definition point, no earlier use needed)
                if let Some(def) = inst.def() {
                    new_entry.remove(&def);
                }
            }

            // Phi defs: phi dst VRegs are defined at block entry
            for phi in &block.phis {
                new_entry.remove(&phi.dst);
            }

            // Check if anything changed
            if new_entry != entry_distances[bi] || new_exit != exit_distances[bi] {
                entry_distances[bi] = new_entry;
                exit_distances[bi] = new_exit;
                changed = true;
            }
        }
    }

    // Compute max register pressure per block
    let mut max_pressure = vec![0usize; num_blocks];
    for (bi, block) in func.blocks.iter().enumerate() {
        let mut live: std::collections::BTreeSet<VReg> = BTreeSet::new();

        // Start with live-in (variables with entry distance < infinity)
        for &vreg in entry_distances[bi].keys() {
            live.insert(vreg);
        }

        let mut max_p = live.len();

        for inst in &block.insts {
            // Add defs
            if let Some(def) = inst.def() {
                live.insert(def);
            }
            max_p = max_p.max(live.len());

            // Remove dead values (last use in this instruction)
            // A vreg is dead after this instruction if its next use distance
            // from the *next* instruction is infinity
            // For simplicity, we just track the max of |live| which is an
            // upper bound on pressure
        }

        max_pressure[bi] = max_p;
    }

    AnalysisResult {
        entry_distances,
        exit_distances,
        block_order,
        block_index,
        predecessors,
        successors,
        max_pressure,
    }
}

/// Get the next-use distance of `vreg` at instruction `inst_idx` within `block_idx`.
/// This walks forward from `inst_idx` to find the next use, then falls back
/// to the block's exit distance.
pub fn next_use_at(
    func: &MFunction,
    analysis: &AnalysisResult,
    block_idx: usize,
    inst_idx: usize,
    vreg: VReg,
) -> u32 {
    let block = &func.blocks[block_idx];

    // Search forward from inst_idx for the next use
    for i in inst_idx..block.insts.len() {
        if block.insts[i].uses().contains(&vreg) {
            return (i - inst_idx) as u32;
        }
    }

    // Not used in remaining block — use exit distance
    let remaining = (block.insts.len() - inst_idx) as u32;
    analysis.exit_distances[block_idx]
        .get(&vreg)
        .map(|d| remaining + d)
        .unwrap_or(u32::MAX)
}
