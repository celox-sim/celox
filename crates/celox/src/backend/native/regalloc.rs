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

/// Run the full register allocation pipeline on an MFunction.
/// Returns the assignment map (VReg → PhysReg).
pub fn run_regalloc(func: &mut MFunction) -> AssignmentMap {
    let analysis = analysis::analyze(func);
    spilling::spill(func, &analysis, NUM_REGS);
    // Re-analyze after spilling (spill/reload instructions change liveness)
    let analysis = analysis::analyze(func);
    assignment::assign(func, &analysis)
}
