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

/// Verify the final allocation's coverage and machine constraints.
///
/// Residency conflicts are verified against `RegFile` during allocation;
/// `AssignmentMap` alone intentionally does not encode residency intervals.
fn verify_assignment(
    func: &MFunction,
    _analysis: &analysis::AnalysisResult,
    assignment: &assignment::AssignmentMap,
) {
    use assignment::RegConstraint;

    for block in &func.blocks {
        for phi in &block.phis {
            assert!(
                assignment.get(phi.dst).is_some() || assignment.edge_spill_slot(phi.dst).is_some(),
                "regalloc verify: phi dst {} has neither physical assignment nor edge spill slot at bb{}",
                phi.dst,
                block.id
            );
            for (pred, src) in &phi.sources {
                assert!(
                    assignment.edge_location(*pred, *src).is_some()
                        || assignment.get(*src).is_some()
                        || assignment.edge_spill_slot(*src).is_some(),
                    "regalloc verify: phi source {src} has no edge location or fallback assignment at bb{} from bb{} to dst {}",
                    block.id,
                    pred,
                    phi.dst
                );
            }
        }

        for (inst_idx, inst) in block.insts.iter().enumerate() {
            let uses = inst.uses();
            let constraints = assignment::use_constraints(inst);
            assert_eq!(
                uses.len(),
                constraints.len(),
                "regalloc verify: constraint arity mismatch at bb{} inst {}: {}",
                block.id,
                inst_idx,
                inst
            );
            for (use_vreg, constraint) in uses.into_iter().zip(constraints) {
                assert!(
                    assignment.get(use_vreg).is_some(),
                    "regalloc verify: use {use_vreg} has no physical assignment at bb{} inst {}: {}",
                    block.id,
                    inst_idx,
                    inst
                );
                if let RegConstraint::Fixed(required) = constraint {
                    assert_eq!(
                        assignment.get(use_vreg),
                        Some(required),
                        "regalloc verify: use {use_vreg} must occupy {required} at bb{} inst {}: {}",
                        block.id,
                        inst_idx,
                        inst
                    );
                }
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
    reorder_blocks_rpo(func);
    let timing = std::env::var_os("CELOX_REGALLOC_TIMING").is_some()
        || std::env::var_os("CELOX_PHASE_TIMING").is_some();
    let total_start = timing.then(crate::timing::now);
    let stats_start = timing.then(crate::timing::now);
    let before_stats = std::env::var_os("CELOX_REGALLOC_STATS")
        .is_some()
        .then(|| collect_regalloc_block_stats(func));
    if let Some(start) = stats_start {
        eprintln!(
            "[regalloc-timing] label={label} collect_before_stats elapsed={:?}",
            start.elapsed()
        );
    }

    // Unified single-pass: simultaneous spilling + assignment.
    // No separate analysis → spill → re-analyze → assign pipeline.
    // No k-1 hack — uses k = NUM_REGS directly.
    let analysis_start = timing.then(crate::timing::now);
    let analysis = analysis::analyze(func);
    if let Some(start) = analysis_start {
        eprintln!(
            "[regalloc-timing] label={label} analysis blocks={} insts={} elapsed={:?}",
            func.blocks.len(),
            func.blocks
                .iter()
                .map(|block| block.insts.len())
                .sum::<usize>(),
            start.elapsed()
        );
    }
    let alloc_start = timing.then(crate::timing::now);
    let (assignment, spill_frame_size) = unified::unified_alloc_with_label(func, &analysis, label);
    if let Some(start) = alloc_start {
        eprintln!(
            "[regalloc-timing] label={label} unified_alloc blocks={} insts={} vregs={} spill_frame={} elapsed={:?}",
            func.blocks.len(),
            func.blocks
                .iter()
                .map(|block| block.insts.len())
                .sum::<usize>(),
            func.vregs.count(),
            spill_frame_size,
            start.elapsed()
        );
    }

    if cfg!(debug_assertions) || std::env::var_os("CELOX_REGALLOC_VERIFY").is_some() {
        let verify_start = timing.then(crate::timing::now);
        let analysis = analysis::analyze(func);
        verify_assignment(func, &analysis, &assignment);
        if let Some(start) = verify_start {
            eprintln!(
                "[regalloc-timing] label={label} verify elapsed={:?}",
                start.elapsed()
            );
        }
    }

    if let Some(before) = before_stats {
        let stats_start = timing.then(crate::timing::now);
        log_regalloc_stats(label, func, &before, spill_frame_size);
        if let Some(start) = stats_start {
            eprintln!(
                "[regalloc-timing] label={label} log_stats elapsed={:?}",
                start.elapsed()
            );
        }
    }
    if let Some(start) = total_start {
        eprintln!(
            "[regalloc-timing] label={label} total elapsed={:?}",
            start.elapsed()
        );
    }

    RegallocResult {
        assignment,
        spill_frame_size,
    }
}

/// Normalize block layout to reverse postorder before the single forward
/// allocation walk. ISel may append CFG-lowering blocks after their logical
/// successors (for example runtime-event blocks), so numeric/block-vector
/// order is not a valid way to distinguish forward edges from backedges.
fn reorder_blocks_rpo(func: &mut MFunction) {
    use super::mir::BlockId;
    use std::collections::{HashMap, HashSet};

    let Some(entry) = func.blocks.first().map(|block| block.id) else {
        return;
    };
    let successors = func
        .blocks
        .iter()
        .map(|block| (block.id, block.successors()))
        .collect::<HashMap<_, _>>();
    let mut visited = HashSet::new();
    let mut postorder = Vec::with_capacity(func.blocks.len());
    let mut stack: Vec<(BlockId, usize)> = vec![(entry, 0)];
    visited.insert(entry);

    while let Some((block, next_successor)) = stack.last_mut() {
        let succs = &successors[block];
        if *next_successor < succs.len() {
            let successor = succs[*next_successor];
            *next_successor += 1;
            if visited.insert(successor) {
                stack.push((successor, 0));
            }
        } else {
            postorder.push(*block);
            stack.pop();
        }
    }
    postorder.reverse();

    // MIR verification rejects unreachable blocks, but retain them
    // deterministically here so this normalization is total on raw inputs.
    let mut remaining = func
        .blocks
        .iter()
        .map(|block| block.id)
        .filter(|id| !visited.contains(id))
        .collect::<Vec<_>>();
    remaining.sort();
    postorder.extend(remaining);

    let mut blocks = std::mem::take(&mut func.blocks)
        .into_iter()
        .map(|block| (block.id, block))
        .collect::<HashMap<_, _>>();
    func.blocks = postorder
        .into_iter()
        .map(|id| {
            blocks
                .remove(&id)
                .expect("RPO contains every MIR block once")
        })
        .collect();
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
