//! Verified SSA register allocator based on Braun & Hack's extended MIN.
//!
//! The pipeline schedules pure DAG regions, plans explicit per-value homes and
//! phi-edge transfers, reconstructs strict SSA, materializes late full-live
//! Perm boundaries, and colors chordal SSA live ranges without an explicit
//! interference graph.

#[allow(dead_code)]
mod allocation_ir;
mod analysis;
pub mod assignment;
mod cfg;
mod color;
mod constraints;
mod cost;
#[allow(dead_code)]
mod cssa;
#[allow(dead_code)]
mod home_graph;
#[allow(dead_code)]
mod home_verify;
#[allow(dead_code)]
mod interval_union;
mod legalize;
#[allow(dead_code)]
mod live_interval;
mod next_use;
mod pressure;
mod reconstruct;
mod reload;
mod schedule;
mod spill_plan;
#[cfg(test)]
mod spilling;
mod ssa;
mod ssa_state_home;
mod stack_color;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod unified;
mod verify;

use std::fmt;

use super::mir::{BaseReg, BlockId, MFunction, MInst, VReg};
pub use assignment::AssignmentMap;

/// Maximum number of available general-purpose registers for allocation.
/// x86-64: 16 GPRs - architectural stack pointer = 15. R15 is excluded at
/// runtime when the host cannot use GS-base instructions.
pub const NUM_REGS: usize = 15;

/// Enable allocator-internal exhaustive consistency checks.
///
/// Repeating whole-session proofs after every incremental liveness edit is
/// intentionally kept out of optimized compilation.
pub(super) fn exhaustive_verification_enabled() -> bool {
    cfg!(debug_assertions) || std::env::var_os("CELOX_REGALLOC_VERIFY").is_some()
}

/// Result of register allocation: assignment map + spill frame size.
pub struct RegallocResult {
    pub assignment: AssignmentMap,
    /// Bytes of stack frame needed for spill slots.
    pub spill_frame_size: u32,
}

#[derive(Default)]
pub(crate) struct RegallocTrace {
    pub mir_after_late_memory_folds: String,
    pub mir_after_scheduling: String,
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

pub(crate) fn verify_assignment(
    func: &MFunction,
    assignment: &assignment::AssignmentMap,
) -> Result<(), RegallocError> {
    let analysis = analysis::analyze_for_assignment(func, assignment);
    verify::verify(func, &analysis, assignment).map_err(|error| {
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

fn constraint_error(phase: &'static str, error: constraints::ConstraintError) -> RegallocError {
    RegallocError::new(
        phase,
        error.rule,
        error.block,
        error.instruction,
        error.values,
        error.message,
    )
}

fn cfg_error(phase: &'static str, error: cfg::CfgError) -> RegallocError {
    RegallocError::new(
        phase,
        error.rule,
        error.block,
        None,
        Vec::new(),
        error.message,
    )
}

fn next_use_error(phase: &'static str, error: next_use::NextUseError) -> RegallocError {
    RegallocError::new(
        phase,
        error.rule,
        error.block,
        error.instruction,
        error.values,
        error.message,
    )
}

fn reload_recipe_error(phase: &'static str, error: reload::ReloadRecipeError) -> RegallocError {
    RegallocError::new(
        phase,
        error.rule,
        error.block,
        error.instruction,
        error.value.into_iter().collect(),
        error.message,
    )
}

#[cfg(test)]
fn cssa_error(phase: &'static str, error: cssa::CssaError) -> RegallocError {
    RegallocError::new(
        phase,
        error.rule,
        error.block,
        error.instruction,
        error.values,
        error.message,
    )
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
    run_regalloc_with_label_and_trace(func, label, None)
}

pub(crate) fn run_regalloc_with_label_and_trace(
    func: &mut MFunction,
    label: &str,
    trace: Option<&mut RegallocTrace>,
) -> Result<RegallocResult, RegallocError> {
    // Build the complete result privately. A structured error cannot expose
    // CFG/scheduling/SSA mutations from a failed phase to the caller.
    let mut working = func.clone();
    let allocation = run_regalloc_in_place(&mut working, label, trace)?;
    *func = working;
    Ok(allocation)
}

fn run_regalloc_in_place(
    func: &mut MFunction,
    label: &str,
    mut trace: Option<&mut RegallocTrace>,
) -> Result<RegallocResult, RegallocError> {
    let timing = std::env::var_os("CELOX_REGALLOC_TIMING").is_some()
        || std::env::var_os("CELOX_PHASE_TIMING").is_some();
    // Allocation must never depend on callers having run the optional MIR
    // optimization pipeline. Select flag-consuming register branches at the
    // allocation boundary so their unmaterialized boolean result cannot
    // acquire a live range.
    super::mir_opt::fold_register_branch_predicates(func);
    func.verify_result()
        .map_err(|error| RegallocError::mir("input MIR verification", error))?;
    let cfg_start = timing.then(crate::timing::now);
    let normalized_cfg =
        cfg::normalize(func).map_err(|error| cfg_error("CFG normalization", error))?;
    normalized_cfg
        .verify(func)
        .map_err(|error| cfg_error("CFG normalization verification", error))?;
    func.verify_result()
        .map_err(|error| RegallocError::mir("CFG normalization verification", error))?;
    if let Some(start) = cfg_start {
        eprintln!(
            "[regalloc-timing] label={label} cfg_normalize blocks={} elapsed={:?}",
            func.blocks.len(),
            start.elapsed()
        );
    }
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

    let late_memory_fold_start = timing.then(crate::timing::now);
    // These folds run before pressure scheduling. They only remove local
    // instructions and never replace a load with a new cross-block VReg.
    super::mir_opt::eliminate_redundant_local_stores(func);
    let folded_direct_immediate_stores = super::mir_opt::fold_direct_immediate_stores(func);
    let folded_memory_branches = super::mir_opt::fold_memory_branch_predicates(func);
    func.verify_result()
        .map_err(|error| RegallocError::mir("late memory-fold verification", error))?;
    if let Some(trace) = trace.as_deref_mut() {
        trace.mir_after_late_memory_folds = func.to_string();
    }
    if let Some(start) = late_memory_fold_start {
        eprintln!(
            "[regalloc-timing] label={label} late_memory_fold folded_direct_immediate_stores={folded_direct_immediate_stores} folded_memory_branches={folded_memory_branches} elapsed={:?}",
            start.elapsed()
        );
    }
    let allocation_constraints = constraints::ConstraintModel::build(func, &normalized_cfg)
        .map_err(|error| constraint_error("placement constraint construction", error))?;
    allocation_constraints
        .verify(func)
        .map_err(|error| constraint_error("placement constraint verification", error))?;
    // W/S planning owns independent homes, explicit phi-edge transfers, and
    // the one authoritative dependency-ready instruction order. Inserting
    // CSSA snapshots before that walk would lengthen the ranges it is meant
    // to split and would make the snapshot order independently authoritative.
    let reload_recipe_start = timing.then(crate::timing::now);
    let planning_recipes = reload::analyze_for_planning(func, &normalized_cfg)
        .map_err(|error| reload_recipe_error("reload-recipe planning analysis", error))?;
    if let Some(start) = reload_recipe_start {
        eprintln!(
            "[regalloc-timing] label={label} reload_recipe_plan_analyze elapsed={:?}",
            start.elapsed()
        );
    }
    let next_use_start = timing.then(crate::timing::now);
    let next_use = next_use::analyze(func, &normalized_cfg)
        .map_err(|error| next_use_error("next-use analysis", error))?;
    if let Some(start) = next_use_start {
        eprintln!(
            "[regalloc-timing] label={label} next_use_analyze elapsed={:?}",
            start.elapsed()
        );
    }
    let next_use_verify_start = timing.then(crate::timing::now);
    next_use
        .verify(func, &normalized_cfg)
        .map_err(|error| next_use_error("next-use verification", error))?;
    if let Some(start) = next_use_verify_start {
        eprintln!(
            "[regalloc-timing] label={label} next_use_verify elapsed={:?}",
            start.elapsed()
        );
    }
    let alloc_start = timing.then(crate::timing::now);
    let allocation = ssa::allocate(
        func,
        &normalized_cfg,
        &next_use,
        &planning_recipes,
        &allocation_constraints,
        trace,
    )?;
    let assignment = allocation.assignment;
    let spill_frame_size = allocation.spill_frame_size;
    if let Some(start) = alloc_start {
        eprintln!(
            "[regalloc-timing] label={label} implementation=ssa-split-color blocks={} insts={} vregs={} spill_frame={} elapsed={:?}",
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
    verify_assignment(func, &assignment)?;
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
fn reorder_blocks_rpo(func: &mut MFunction) -> Result<(), String> {
    use super::mir::BlockId;
    use std::collections::{HashMap, HashSet};

    let Some(entry) = func.blocks.first().map(|block| block.id) else {
        return Ok(());
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

    let positions = postorder
        .into_iter()
        .enumerate()
        .map(|(position, id)| (id, position))
        .collect::<HashMap<_, _>>();
    if positions.len() != func.blocks.len()
        || func
            .blocks
            .iter()
            .any(|block| !positions.contains_key(&block.id))
    {
        return Err("reverse-postorder layout is not a bijection over MIR blocks".into());
    }
    func.blocks
        .sort_by_key(|block| positions.get(&block.id).copied().unwrap_or(usize::MAX));
    Ok(())
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

    rows.sort_unstable_by_key(|row| std::cmp::Reverse(row.0));
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
