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

/// Run the full register allocation pipeline on an MFunction.
/// Returns the assignment map and required spill frame size.
pub fn run_regalloc(func: &mut MFunction) -> RegallocResult {
    let analysis = analysis::analyze(func);
    let spill_frame_size = spilling::spill(func, &analysis, NUM_REGS);
    // Re-analyze after spilling (spill/reload instructions change liveness)
    let analysis = analysis::analyze(func);
    let assignment = assignment::assign(func, &analysis);
    RegallocResult {
        assignment,
        spill_frame_size,
    }
}
