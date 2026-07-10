//! Verified SSA register allocator based on Braun & Hack's extended MIN.
//!
//! The pipeline schedules pure DAG regions, constructs CSSA, plans spilling,
//! reconstructs strict SSA, materializes late full-live Perm boundaries, and
//! colors chordal SSA live ranges without an explicit interference graph.

mod analysis;
pub mod assignment;
mod cfg;
mod color;
mod constraints;
#[allow(dead_code)]
mod cssa;
#[allow(dead_code)]
mod home_verify;
mod legalize;
mod next_use;
mod pressure;
mod reconstruct;
mod schedule;
mod spill_plan;
mod spilling;
mod ssa;
#[cfg(test)]
mod tests;
mod unified;
mod verify;

use std::fmt;

use super::mir::{BaseReg, BlockId, MFunction, MInst, VReg};
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

/// Structured failure from a verified register-allocation phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegallocError {
    pub phase: &'static str,
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl RegallocError {
    fn new(
        phase: &'static str,
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        values: Vec<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            phase,
            rule,
            block,
            instruction,
            values,
            message: message.into(),
        }
    }

    fn mir(phase: &'static str, error: super::mir_verify::MirVerifyError) -> Self {
        Self::new(
            phase,
            error.invariant,
            error.block,
            error.instruction,
            Vec::new(),
            error.message,
        )
    }
}

impl fmt::Display for RegallocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "register allocation {} [{}]", self.phase, self.rule)?;
        if let Some(block) = self.block {
            write!(f, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(f, "/i{instruction}")?;
        }
        if !self.values.is_empty() {
            write!(f, " values={:?}", self.values)?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for RegallocError {}

fn verify_assignment(
    func: &MFunction,
    analysis: &analysis::AnalysisResult,
    assignment: &assignment::AssignmentMap,
) -> Result<(), RegallocError> {
    verify::verify(func, analysis, assignment).map_err(|error| {
        RegallocError::new(
            "completed-assignment verification",
            "ASSIGNMENT.INVALID",
            Some(error.block),
            error.instruction,
            Vec::new(),
            error.message,
        )
    })
}

/// Run the full register allocation pipeline on an MFunction.
/// Returns the assignment map and required spill frame size.
pub fn run_regalloc(func: &mut MFunction) -> Result<RegallocResult, RegallocError> {
    run_regalloc_with_label(func, "unknown")
}

/// Run register allocation and optionally log per-block allocation deltas.
pub fn run_regalloc_with_label(
    func: &mut MFunction,
    label: &str,
) -> Result<RegallocResult, RegallocError> {
    let requested = std::env::var("CELOX_REGALLOC_IMPL").unwrap_or_else(|_| "auto".into());
    if !matches!(requested.as_str(), "auto" | "ssa" | "unified") {
        return Err(RegallocError::new(
            "configuration",
            "CONFIG.IMPLEMENTATION",
            None,
            None,
            Vec::new(),
            format!("unknown CELOX_REGALLOC_IMPL={requested:?}; expected auto, ssa, or unified"),
        ));
    }

    // Build the complete result privately. A structured error cannot expose
    // CFG/scheduling/SSA mutations from a failed phase to the caller.
    let mut working = func.clone();
    let allocation = run_regalloc_in_place(&mut working, label, &requested)?;
    *func = working;
    Ok(allocation)
}

fn run_regalloc_in_place(
    func: &mut MFunction,
    label: &str,
    requested: &str,
) -> Result<RegallocResult, RegallocError> {
    func.verify_result()
        .map_err(|error| RegallocError::mir("input MIR verification", error))?;
    let normalized_cfg = cfg::normalize(func);
    normalized_cfg.verify(func);
    func.verify_result()
        .map_err(|error| RegallocError::mir("CFG normalization verification", error))?;
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

    let scheduling_constraints = constraints::ConstraintModel::build(func, &normalized_cfg);
    scheduling_constraints.verify(func);
    let schedule_analysis = analysis::analyze(func);
    let schedule_start = timing.then(crate::timing::now);
    let schedule_stats = schedule::schedule_for_pressure(
        func,
        &normalized_cfg,
        &scheduling_constraints,
        &schedule_analysis,
    )
    .map_err(|error| {
        RegallocError::new(
            "pressure scheduling",
            "SCHEDULE.DEPENDENCY_ORDER",
            Some(error.block),
            None,
            Vec::new(),
            error.reason,
        )
    })?;
    if let Some(start) = schedule_start {
        eprintln!(
            "[regalloc-timing] label={label} pressure_schedule changed_blocks={} max_before={} max_after={} elapsed={:?}",
            schedule_stats.changed_blocks,
            schedule_stats.maximum_before,
            schedule_stats.maximum_after,
            start.elapsed()
        );
    }
    func.verify_result()
        .map_err(|error| RegallocError::mir("pressure scheduling verification", error))?;
    let cssa = cssa::normalize_to_cssa(func, &normalized_cfg);
    cssa::verify_cssa(func, &normalized_cfg, &cssa).map_err(|error| {
        RegallocError::new(
            "CSSA verification",
            error.invariant,
            error.block,
            error.instruction,
            error
                .values
                .into_iter()
                .flat_map(|pair| [pair.0, pair.1])
                .collect(),
            error.message,
        )
    })?;
    func.verify_result()
        .map_err(|error| RegallocError::mir("CSSA structural verification", error))?;
    let constraints = constraints::ConstraintModel::build(func, &normalized_cfg);
    constraints.verify(func);
    let next_use = next_use::analyze(func, &normalized_cfg);
    next_use.verify(func, &normalized_cfg);
    let alloc_start = timing.then(crate::timing::now);
    let (assignment, spill_frame_size, implementation) = if requested == "unified" {
        let analysis_start = timing.then(crate::timing::now);
        let analysis = analysis::analyze(func);
        if let Some(start) = analysis_start {
            eprintln!(
                "[regalloc-timing] label={label} unified_analysis blocks={} insts={} elapsed={:?}",
                func.blocks.len(),
                func.blocks
                    .iter()
                    .map(|block| block.insts.len())
                    .sum::<usize>(),
                start.elapsed()
            );
        }
        let (assignment, frame_size) = unified::unified_alloc_with_label(func, &analysis, label);
        (assignment, frame_size, "unified-spill")
    } else {
        let allocation = ssa::allocate(func, &normalized_cfg, &next_use)?;
        (
            allocation.assignment,
            allocation.spill_frame_size,
            "ssa-split-color",
        )
    };
    if let Some(start) = alloc_start {
        eprintln!(
            "[regalloc-timing] label={label} implementation={implementation} blocks={} insts={} vregs={} spill_frame={} elapsed={:?}",
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

    let verify_start = timing.then(crate::timing::now);
    let analysis = analysis::analyze(func);
    verify_assignment(func, &analysis, &assignment)?;
    if let Some(start) = verify_start {
        eprintln!(
            "[regalloc-timing] label={label} verify elapsed={:?}",
            start.elapsed()
        );
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

    Ok(RegallocResult {
        assignment,
        spill_frame_size,
    })
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
