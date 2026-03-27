//! Register allocator: Braun & Hack (2009) extended MIN algorithm.
//!
//! Spilling phase reduces register pressure to ≤ k (physical register count),
//! then assignment phase colors the SSA interference graph in linear time.

mod analysis;
pub mod assignment;
mod spilling;

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
fn verify_assignment(
    func: &MFunction,
    analysis: &analysis::AnalysisResult,
    assignment: &assignment::AssignmentMap,
) {
    use std::collections::BTreeMap;
    use super::mir::VReg;
    use assignment::PhysReg;

    for (bi, block) in func.blocks.iter().enumerate() {
        // Track live VRegs and their PhysRegs at each program point
        let mut live: BTreeMap<VReg, PhysReg> = BTreeMap::new();

        // Initialize from entry_distances
        for &vreg in analysis.entry_distances[bi].keys() {
            if let Some(preg) = assignment.get(vreg) {
                if let Some((&existing_vreg, _)) = live.iter().find(|&(_, &p)| p == preg) {
                    panic!(
                        "regalloc conflict: block {bi} entry: {vreg} and {existing_vreg} both assigned to {preg}"
                    );
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

            // Remove dead VRegs
            let dead: Vec<VReg> = live.keys().copied().filter(|&v| {
                analysis::next_use_at(func, analysis, bi, inst_idx + 1, v) == u32::MAX
            }).collect();
            for v in dead {
                live.remove(&v);
            }

            // Add def
            if let Some(def) = inst.def() {
                if let Some(preg) = assignment.get(def) {
                    // Check for conflict with existing live VRegs
                    for (&other_vreg, &other_preg) in &live {
                        if other_vreg != def && other_preg == preg {
                            panic!(
                                "regalloc conflict: block {bi} inst {inst_idx}: def {def} and live {other_vreg} both at {preg} | inst: {inst}"
                            );
                        }
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
    // Pre-spilling live-range splits.
    assignment::split_live_ranges_at_clobbers(func);
    let _ = assignment::split_live_ranges_at_fixed_constraints(func);

    let analysis = analysis::analyze(func);
    let mut spill_frame_size = spilling::spill(func, &analysis, NUM_REGS - 1);

    // Post-spilling re-split: spilling may have inserted reloads between
    // the Mov and the shift, breaking the 1-instruction lifetime.
    assignment::split_live_ranges_at_fixed_constraints(func);

    // Iterate split + re-spill until stable: each spilling pass may insert
    // reloads that break the 1-instruction lifetime of split Movs. Re-split
    // and re-spill until no new splits are needed.
    loop {
        let changed = assignment::split_live_ranges_at_fixed_constraints(func);
        if !changed { break; }
        let analysis = analysis::analyze(func);
        spill_frame_size += spilling::spill(func, &analysis, NUM_REGS - 1);
    }

    let analysis = analysis::analyze(func);
    let assignment = assignment::assign(func, &analysis);

    if std::env::var("CELOX_DUMP_MIR").is_ok() {
        for (bi, block) in func.blocks.iter().enumerate() {
            for (ii, inst) in block.insts.iter().enumerate() {
                let r = inst.def().and_then(|d| assignment.get(d));
                eprintln!("  [{ii:3}] {inst}  => {r:?}");
            }
            let _ = bi;
        }
    }

    // Verify: no two simultaneously-live VRegs share a PhysReg.
    #[cfg(debug_assertions)]
    verify_assignment(func, &analysis, &assignment);

    RegallocResult {
        assignment,
        spill_frame_size,
    }
}
