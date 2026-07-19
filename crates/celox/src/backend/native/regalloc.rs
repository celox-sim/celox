//! Verified SSA register allocator based on Braun & Hack's extended MIN.
//!
//! The pipeline schedules pure DAG regions, constructs CSSA, plans spilling,
//! reconstructs strict SSA, materializes late full-live Perm boundaries, and
//! colors chordal SSA live ranges without an explicit interference graph.

#[allow(dead_code)]
mod allocation_constraints;
#[allow(dead_code)]
mod allocation_expand;
#[allow(dead_code)]
mod allocation_ir;
#[allow(dead_code)]
mod allocation_lower;
#[allow(dead_code)]
mod allocation_reallocate;
#[allow(dead_code)]
mod allocation_split;
mod analysis;
pub mod assignment;
mod cfg;
mod color;
mod constraints;
#[allow(dead_code)]
mod cssa;
mod greedy;
#[allow(dead_code)]
mod home_graph;
#[allow(dead_code)]
mod home_verify;
#[allow(dead_code)]
mod interval_allocator;
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
mod spiller;
#[cfg(test)]
mod spilling;
mod ssa;
#[allow(dead_code)]
mod stack_color;
#[cfg(test)]
mod tests;
#[cfg(test)]
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

/// Enable allocator-internal exhaustive consistency checks.
///
/// Publication always performs an independent whole-function rebuild in
/// `allocation_lower`; this switch is for additional checks at intermediate
/// split-session boundaries.  Repeating those whole-session proofs after
/// every symbolic split is intentionally kept out of optimized compilation.
pub(super) fn exhaustive_verification_enabled() -> bool {
    cfg!(debug_assertions) || std::env::var_os("CELOX_REGALLOC_VERIFY").is_some()
}

/// Result of register allocation: assignment map + spill frame size.
pub struct RegallocResult {
    pub assignment: AssignmentMap,
    /// Bytes of stack frame needed for spill slots.
    pub spill_frame_size: u32,
    /// Complete, independently verified phi-edge parallel-copy plan.
    pub(crate) ssa_destruction: super::ssa_destroy::SsaDestructionPlan,
}

#[derive(Default)]
pub(crate) struct RegallocTrace {
    pub mir_after_scheduling: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegallocImplementation {
    Ssa,
    /// Build and verify the replacement, then publish the established
    /// production allocator's result from the same scheduled/CSSA input.
    IntervalDiagnostic,
    /// Publish the replacement allocator's atomically lowered result.
    Interval,
}

impl RegallocImplementation {
    fn parse(requested: &str) -> Option<Self> {
        match requested {
            "auto" | "ssa" => Some(Self::Ssa),
            "interval-diagnostic" => Some(Self::IntervalDiagnostic),
            "interval" => Some(Self::Interval),
            _ => None,
        }
    }

    fn runs_interval(self) -> bool {
        matches!(self, Self::IntervalDiagnostic | Self::Interval)
    }
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

fn home_graph_error(phase: &'static str, error: home_graph::HomeGraphError) -> RegallocError {
    RegallocError::new(
        phase,
        error.rule,
        error.block,
        error.instruction,
        error.values,
        error.message,
    )
}

fn allocation_expand_error(
    phase: &'static str,
    error: allocation_expand::AllocationExpandError,
) -> RegallocError {
    let identity = match (error.root, error.use_id) {
        (Some(root), Some(use_id)) => format!(" root={root:?} use={use_id:?}"),
        (Some(root), None) => format!(" root={root:?}"),
        (None, Some(use_id)) => format!(" use={use_id:?}"),
        (None, None) => String::new(),
    };
    RegallocError::new(
        phase,
        error.rule,
        error.block,
        None,
        Vec::new(),
        format!("{}{identity}", error.message),
    )
}

fn allocation_split_error(
    phase: &'static str,
    error: allocation_split::AllocationSplitError,
) -> RegallocError {
    let root = error
        .root
        .map(|root| format!(" root={root:?}"))
        .unwrap_or_default();
    RegallocError::new(
        phase,
        error.rule,
        error.block,
        None,
        error.value.into_iter().collect(),
        format!("{}{root}", error.message),
    )
}

fn allocation_lower_error(
    phase: &'static str,
    error: allocation_lower::AllocationLowerError,
) -> RegallocError {
    RegallocError::new(
        phase,
        error.rule,
        error.block,
        error.instruction,
        error.values,
        error.message,
    )
}

fn allocation_perm_error(phase: &'static str, error: legalize::PermError) -> RegallocError {
    RegallocError::new(
        phase,
        error.rule,
        error.block,
        error.instruction,
        error.values,
        error.message,
    )
}

fn ssa_destruction_error(
    phase: &'static str,
    error: super::ssa_destroy::SsaDestructionError,
) -> RegallocError {
    let mut values = Vec::with_capacity(2);
    if let Some(destination) = error.phi_destination {
        values.push(destination);
    }
    if let Some(source) = error.source_value {
        values.push(source);
    }
    let edge = match (error.predecessor, error.successor) {
        (Some(predecessor), Some(successor)) => format!(" on {predecessor} -> {successor}"),
        (None, Some(successor)) => format!(" at {successor}"),
        _ => String::new(),
    };
    RegallocError::new(
        phase,
        error.rule,
        error.successor.or(error.predecessor),
        None,
        values,
        format!("{}{edge}", error.message),
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
    let requested = std::env::var("CELOX_REGALLOC_IMPL").unwrap_or_else(|_| "auto".into());
    let Some(implementation) = RegallocImplementation::parse(&requested) else {
        return Err(RegallocError::new(
            "configuration",
            "CONFIG.IMPLEMENTATION",
            None,
            None,
            Vec::new(),
            format!(
                "unknown CELOX_REGALLOC_IMPL={requested:?}; expected auto, ssa, interval-diagnostic, or interval"
            ),
        ));
    };

    // Build the complete result privately. A structured error cannot expose
    // CFG/scheduling/SSA mutations from a failed phase to the caller.
    let mut working = func.clone();
    let allocation = run_regalloc_in_place(&mut working, label, trace, implementation)?;
    *func = working;
    Ok(allocation)
}

fn run_regalloc_in_place(
    func: &mut MFunction,
    label: &str,
    trace: Option<&mut RegallocTrace>,
    implementation: RegallocImplementation,
) -> Result<RegallocResult, RegallocError> {
    let timing = std::env::var_os("CELOX_REGALLOC_TIMING").is_some()
        || std::env::var_os("CELOX_PHASE_TIMING").is_some();
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

    let scheduling_constraints = constraints::ConstraintModel::build(func, &normalized_cfg)
        .map_err(|error| constraint_error("scheduling constraint construction", error))?;
    scheduling_constraints
        .verify(func)
        .map_err(|error| constraint_error("scheduling constraint verification", error))?;
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
            error.rule,
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
    if let Some(trace) = trace {
        trace.mir_after_scheduling = func.to_string();
    }
    let cssa_start = timing.then(crate::timing::now);
    let cssa = cssa::normalize_to_cssa(func, &normalized_cfg)
        .map_err(|error| cssa_error("CSSA normalization", error))?;
    if let Some(start) = cssa_start {
        eprintln!(
            "[regalloc-timing] label={label} cssa_normalize elapsed={:?}",
            start.elapsed()
        );
    }
    let cssa_verify_start = timing.then(crate::timing::now);
    cssa::verify_cssa(func, &normalized_cfg, &cssa)
        .map_err(|error| cssa_error("CSSA verification", error))?;
    func.verify_result()
        .map_err(|error| RegallocError::mir("CSSA structural verification", error))?;
    if let Some(start) = cssa_verify_start {
        eprintln!(
            "[regalloc-timing] label={label} cssa_verify elapsed={:?}",
            start.elapsed()
        );
    }
    if implementation.runs_interval() {
        let interval_start = timing.then(crate::timing::now);
        let constraint_perm_start = timing.then(crate::timing::now);
        let mut interval_func = func.clone();
        legalize::materialize_allocation_fixed_use_fragments(&mut interval_func).map_err(
            |error| allocation_perm_error("interval fixed-use fragment construction", error),
        )?;
        let interval_cfg = &normalized_cfg;
        if let Some(start) = constraint_perm_start {
            eprintln!(
                "[regalloc-timing] label={label} interval_constraint_fragments elapsed={:?}",
                start.elapsed()
            );
        }
        let home_start = timing.then(crate::timing::now);
        let homes = home_graph::build(&interval_func, interval_cfg)
            .map_err(|error| home_graph_error("interval HomeGraph construction", error))?;
        if let Some(start) = home_start {
            eprintln!(
                "[regalloc-timing] label={label} interval_home_graph elapsed={:?}",
                start.elapsed()
            );
        }
        let seed_start = timing.then(crate::timing::now);
        let mut expanded =
            allocation_expand::expand_unallocated(&interval_func, interval_cfg, &homes).map_err(
                |error| allocation_expand_error("interval unallocated SSA construction", error),
            )?;
        if let Some(start) = seed_start {
            eprintln!(
                "[regalloc-timing] label={label} interval_unallocated_seed elapsed={:?}",
                start.elapsed()
            );
        }
        let reallocation_start = timing.then(crate::timing::now);
        let allocation = allocation_split::allocate_with_splitting(
            &mut expanded,
            &homes,
            interval_cfg,
            assignment::ALLOCATABLE_REGS,
        )
        .map_err(|error| allocation_split_error("interval joint reallocation", error))?;
        if let Some(start) = reallocation_start {
            eprintln!(
                "[regalloc-timing] label={label} interval_joint_reallocation elapsed={:?}",
                start.elapsed()
            );
        }
        let lowering_start = timing.then(crate::timing::now);
        let lowered = allocation_lower::lower(
            &interval_func,
            interval_cfg,
            &homes,
            &expanded,
            &allocation,
            assignment::ALLOCATABLE_REGS,
        )
        .map_err(|error| allocation_lower_error("interval atomic MIR lowering", error))?;
        if let Some(start) = lowering_start {
            eprintln!(
                "[regalloc-timing] label={label} interval_atomic_lowering elapsed={:?}",
                start.elapsed()
            );
        }
        if let Some(start) = interval_start {
            eprintln!(
                "[regalloc-timing] label={label} interval_diagnostic elapsed={:?}",
                start.elapsed()
            );
        }
        if implementation == RegallocImplementation::Interval {
            let allocation_lower::LoweredAllocation {
                function,
                assignment,
                spill_frame_size,
                ssa_destruction,
                ..
            } = lowered;
            *func = function;
            let verify_start = timing.then(crate::timing::now);
            verify_assignment(func, &assignment)?;
            ssa_destruction
                .verify(func, &assignment, spill_frame_size)
                .map_err(|error| {
                    ssa_destruction_error("interval SSA destruction verification", error)
                })?;
            if let Some(start) = verify_start {
                eprintln!(
                    "[regalloc-timing] label={label} interval_publish_verify elapsed={:?}",
                    start.elapsed()
                );
            }
            if let Some(before) = &before_stats {
                let stats_start = timing.then(crate::timing::now);
                log_regalloc_stats(label, func, before, spill_frame_size);
                if let Some(start) = stats_start {
                    eprintln!(
                        "[regalloc-timing] label={label} log_stats elapsed={:?}",
                        start.elapsed()
                    );
                }
            }
            if let Some(start) = total_start {
                eprintln!(
                    "[regalloc-timing] label={label} implementation=interval total elapsed={:?}",
                    start.elapsed()
                );
            }
            return Ok(RegallocResult {
                assignment,
                spill_frame_size,
                ssa_destruction,
            });
        }
    }
    let constraint_start = timing.then(crate::timing::now);
    let constraints = constraints::ConstraintModel::build(func, &normalized_cfg)
        .map_err(|error| constraint_error("allocation constraint construction", error))?;
    constraints
        .verify(func)
        .map_err(|error| constraint_error("allocation constraint verification", error))?;
    if let Some(start) = constraint_start {
        eprintln!(
            "[regalloc-timing] label={label} allocation_constraints elapsed={:?}",
            start.elapsed()
        );
    }
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
    let allocation = ssa::allocate(func, &normalized_cfg, &next_use, &planning_recipes)?;
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
    let ssa_destruction = super::ssa_destroy::SsaDestructionPlan::build(func, &assignment)
        .map_err(|error| ssa_destruction_error("SSA destruction planning", error))?;
    ssa_destruction
        .verify(func, &assignment, spill_frame_size)
        .map_err(|error| ssa_destruction_error("SSA destruction verification", error))?;
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
        ssa_destruction,
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
