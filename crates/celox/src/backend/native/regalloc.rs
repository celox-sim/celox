//! Register allocator: Braun & Hack (2009) extended MIN algorithm.
//!
//! Spilling phase reduces register pressure to ≤ k (physical register count),
//! then assignment phase colors the SSA interference graph in linear time.

mod analysis;
pub mod assignment;
mod spilling;
#[cfg(test)]
mod tests;
mod unified;

use super::mir::MFunction;
pub use assignment::AssignmentMap;

/// Number of available general-purpose registers for allocation.
/// x86-64: 16 GPRs - RSP - RBP - SimState base = 13
pub const NUM_REGS: usize = 13;

/// Result of register allocation: assignment map + spill frame size.
pub struct RegallocResult {
    pub assignment: AssignmentMap,
    /// Bytes of stack frame needed for spill slots.
    pub spill_frame_size: u32,
}

/// Verify that no two simultaneously-live VRegs share a PhysReg.
#[cfg(any(debug_assertions, test))]
fn verify_assignment(
    func: &MFunction,
    analysis: &analysis::AnalysisResult,
    assignment: &assignment::AssignmentMap,
) {
    use super::mir::VReg;
    use assignment::PhysReg;
    use std::collections::HashMap;

    for (bi, block) in func.blocks.iter().enumerate() {
        // Track live VRegs and their PhysRegs at each program point
        let mut live: HashMap<VReg, PhysReg> = HashMap::new();

        // Pre-compute use positions for O(log n) dead check
        let mut use_positions: HashMap<VReg, Vec<usize>> = HashMap::new();
        for (i, inst) in block.insts.iter().enumerate() {
            for vreg in inst.uses() {
                use_positions.entry(vreg).or_default().push(i);
            }
        }

        // Initialize from entry_distances.
        // Only include VRegs that have assignments AND don't conflict.
        // VRegs that were spilled in a predecessor may still appear in
        // entry_distances (for cross-block liveness) but no longer
        // occupy a register.
        for &vreg in analysis.entry_distances[bi].keys() {
            if !use_positions.contains_key(&vreg) {
                continue;
            }
            if let Some(preg) = assignment.get(vreg) {
                // Skip if this PhysReg is already claimed by another VReg
                if live.values().any(|&p| p == preg) {
                    continue;
                }
                live.insert(vreg, preg);
            }
        }

        for (inst_idx, inst) in block.insts.iter().enumerate() {
            // Check uses: all used VRegs should be live and have unique PhysRegs
            for use_vreg in inst.uses() {
                if let Some(preg) = assignment.get(use_vreg) {
                    // Check if another live VReg has the same PhysReg
                    for (&other_vreg, &other_preg) in &live {
                        if other_vreg != use_vreg && other_preg == preg {
                            panic!(
                                "regalloc conflict: block {bi} inst {inst_idx}: use {use_vreg} and live {other_vreg} both at {preg} | inst: {inst}"
                            );
                        }
                    }
                }
            }

            // Remove dead VRegs (O(log n) per VReg via binary search)
            let dead: Vec<VReg> = live
                .keys()
                .copied()
                .filter(|&v| {
                    let from = inst_idx + 1;
                    let has_future_use = if let Some(positions) = use_positions.get(&v) {
                        match positions.binary_search(&from) {
                            Ok(_) => true,
                            Err(idx) => idx < positions.len(),
                        }
                    } else {
                        false
                    };
                    !has_future_use && !analysis.exit_distances[bi].contains_key(&v)
                })
                .collect();
            for v in dead {
                live.remove(&v);
            }

            // Add def
            if let Some(def) = inst.def() {
                if let Some(preg) = assignment.get(def) {
                    // In the unified allocator, a spilled VReg may still
                    // have a global assignment but no longer occupies the
                    // register. If a new def claims the same PhysReg,
                    // evict the stale entry from live.
                    let stale: Vec<VReg> = live
                        .iter()
                        .filter(|(v, p)| **v != def && **p == preg)
                        .map(|(v, _)| *v)
                        .collect();
                    for v in stale {
                        live.remove(&v);
                    }
                    live.insert(def, preg);
                }
            }
        }
    }
}

/// Run the full register allocation pipeline on an MFunction.
/// Returns the assignment map and required spill frame size.
pub fn run_regalloc(func: &mut MFunction) -> RegallocResult {
    // Unified single-pass: simultaneous spilling + assignment.
    // No separate analysis → spill → re-analyze → assign pipeline.
    // No k-1 hack — uses k = NUM_REGS directly.
    let analysis = analysis::analyze(func);
    let (assignment, spill_frame_size) = unified::unified_alloc(func, &analysis);

    #[cfg(debug_assertions)]
    {
        let analysis = analysis::analyze(func);
        verify_assignment(func, &analysis, &assignment);
    }

    RegallocResult {
        assignment,
        spill_frame_size,
    }
}
