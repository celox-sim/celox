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

use super::mir::{BaseReg, MFunction, MInst};
pub use assignment::AssignmentMap;

/// Number of available general-purpose registers for allocation.
/// x86-64: 16 GPRs - RSP - SimState base = 14.
///
/// RBP is callee-saved, but the native backend does not use it as a frame
/// pointer; spill slots are addressed relative to RSP after the prologue.
pub const NUM_REGS: usize = 14;

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
    use super::mir::VReg;
    use assignment::PhysReg;
    use std::collections::HashMap;

    for (bi, block) in func.blocks.iter().enumerate() {
        for phi in &block.phis {
            assert!(
                assignment.get(phi.dst).is_some(),
                "regalloc verify: phi dst {} has no physical assignment at bb{}",
                phi.dst,
                block.id
            );
            for (pred, src) in &phi.sources {
                assert!(
                    assignment.get(*src).is_some(),
                    "regalloc verify: phi source {src} has no physical assignment at bb{} from bb{} to dst {}",
                    block.id,
                    pred,
                    phi.dst
                );
            }
        }

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
            for use_vreg in inst.uses() {
                assert!(
                    assignment.get(use_vreg).is_some(),
                    "regalloc verify: use {use_vreg} has no physical assignment at bb{} inst {}: {}",
                    block.id,
                    inst_idx,
                    inst
                );
            }
            if let Some(def) = inst.def() {
                assert!(
                    assignment.get(def).is_some(),
                    "regalloc verify: def {def} has no physical assignment at bb{} inst {}: {}",
                    block.id,
                    inst_idx,
                    inst
                );
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
    run_regalloc_with_label(func, "unknown")
}

/// Run register allocation and optionally log per-block allocation deltas.
pub fn run_regalloc_with_label(func: &mut MFunction, label: &str) -> RegallocResult {
    let before_stats = std::env::var_os("CELOX_REGALLOC_STATS")
        .is_some()
        .then(|| collect_regalloc_block_stats(func));

    // Unified single-pass: simultaneous spilling + assignment.
    // No separate analysis → spill → re-analyze → assign pipeline.
    // No k-1 hack — uses k = NUM_REGS directly.
    let analysis = analysis::analyze(func);
    let (assignment, spill_frame_size) = unified::unified_alloc(func, &analysis);

    if cfg!(debug_assertions) || std::env::var_os("CELOX_REGALLOC_VERIFY").is_some() {
        let analysis = analysis::analyze(func);
        verify_assignment(func, &analysis, &assignment);
    }

    if let Some(before) = before_stats {
        log_regalloc_stats(label, func, &before, spill_frame_size);
    }

    RegallocResult {
        assignment,
        spill_frame_size,
    }
}

#[derive(Clone, Copy, Default)]
struct RegallocBlockStats {
    insts: usize,
    mov: usize,
    load_stack: usize,
    store_stack: usize,
    load_imm: usize,
}

fn collect_regalloc_block_stats(
    func: &MFunction,
) -> Vec<(super::mir::BlockId, RegallocBlockStats)> {
    func.blocks
        .iter()
        .map(|block| {
            let mut stats = RegallocBlockStats {
                insts: block.insts.len(),
                ..RegallocBlockStats::default()
            };
            for inst in &block.insts {
                match inst {
                    MInst::Mov { .. } => stats.mov += 1,
                    MInst::LoadImm { .. } => stats.load_imm += 1,
                    MInst::Load {
                        base: BaseReg::StackFrame,
                        ..
                    } => stats.load_stack += 1,
                    MInst::Store {
                        base: BaseReg::StackFrame,
                        ..
                    } => stats.store_stack += 1,
                    _ => {}
                }
            }
            (block.id, stats)
        })
        .collect()
}

fn log_regalloc_stats(
    label: &str,
    func: &MFunction,
    before: &[(super::mir::BlockId, RegallocBlockStats)],
    spill_frame_size: u32,
) {
    let after = collect_regalloc_block_stats(func);
    let before_by_block = before
        .iter()
        .copied()
        .collect::<std::collections::HashMap<_, _>>();
    let mut rows = Vec::new();
    let mut total = RegallocBlockStats::default();
    let mut total_delta = RegallocBlockStats::default();

    for (block_id, after_stats) in after {
        let before_stats = before_by_block.get(&block_id).copied().unwrap_or_default();
        total.insts += after_stats.insts;
        total.mov += after_stats.mov;
        total.load_stack += after_stats.load_stack;
        total.store_stack += after_stats.store_stack;
        total.load_imm += after_stats.load_imm;

        let delta = RegallocBlockStats {
            insts: after_stats.insts.saturating_sub(before_stats.insts),
            mov: after_stats.mov.saturating_sub(before_stats.mov),
            load_stack: after_stats
                .load_stack
                .saturating_sub(before_stats.load_stack),
            store_stack: after_stats
                .store_stack
                .saturating_sub(before_stats.store_stack),
            load_imm: after_stats.load_imm.saturating_sub(before_stats.load_imm),
        };
        total_delta.insts += delta.insts;
        total_delta.mov += delta.mov;
        total_delta.load_stack += delta.load_stack;
        total_delta.store_stack += delta.store_stack;
        total_delta.load_imm += delta.load_imm;
        rows.push((
            delta.load_stack + delta.store_stack + delta.mov + delta.load_imm,
            block_id,
            before_stats,
            after_stats,
            delta,
        ));
    }

    eprintln!(
        "[regalloc-stats] label={label} spill_frame={spill_frame_size} total_insts={} delta_insts={} total_mov={} delta_mov={} total_load_stack={} delta_load_stack={} total_store_stack={} delta_store_stack={} total_load_imm={} delta_load_imm={}",
        total.insts,
        total_delta.insts,
        total.mov,
        total_delta.mov,
        total.load_stack,
        total_delta.load_stack,
        total.store_stack,
        total_delta.store_stack,
        total.load_imm,
        total_delta.load_imm,
    );

    rows.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    for (rank, (_score, block_id, before_stats, after_stats, delta)) in
        rows.into_iter().take(12).enumerate()
    {
        eprintln!(
            "[regalloc-block-stats] label={label} rank={} block={} before_insts={} after_insts={} delta_insts={} delta_mov={} delta_load_stack={} delta_store_stack={} delta_load_imm={}",
            rank + 1,
            block_id.0,
            before_stats.insts,
            after_stats.insts,
            delta.insts,
            delta.mov,
            delta.load_stack,
            delta.store_stack,
            delta.load_imm,
        );
    }
}
