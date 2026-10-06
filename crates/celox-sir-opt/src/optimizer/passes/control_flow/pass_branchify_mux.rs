//! Branchify profitable mux computations while preserving control and memory dependencies.
//!
//! This module owns pass ordering, shared plans, CFG analysis, and use/definition
//! bookkeeping. Child modules implement the individual rewrite strategies,
//! profitability estimates, diagnostics, and cleanup.

mod cleanup;
mod controlled_joins;
mod coupled;
mod cross_block;
mod diagnostics;
mod local_mux;
mod path_facts;
mod placement;
mod profitability;
mod selector;

use cleanup::inline_param_only_jump_blocks;
use controlled_joins::eliminate_controlled_join_muxes;
use coupled::{branchify_coupled_priority_chains, branchify_coupled_state_updates};
use cross_block::{
    apply_cross_block_branchify, apply_cross_block_group_branchify,
    apply_cross_block_priority_chain, find_cross_block_branchify_plans,
    find_cross_block_group_branchify_plan, find_cross_block_priority_chain_plans,
    is_cross_block_sinkable_input,
};
use diagnostics::{trace_reg_branchify_plan, trace_reg_in_new_block, trace_reg_in_original};
use local_mux::{
    apply_branchify_mux, find_branchify_mux_in_block, is_memory_barrier, memory_write,
    removable_defs_after_head_restore,
};
use path_facts::{
    facts_on_edge, indexed_incoming_edges, known_condition_truth, predicate_key,
    resolve_boolean_alias,
};
use placement::{
    apply_atomic_priority_placement, apply_existing_cfg_placement, find_atomic_priority_placement,
    find_existing_cfg_placement,
};
use profitability::{branch_is_profitable, branchified_instruction_cost, static_true_probability};
use selector::branchify_selector_guarded_predicates;

use super::pass_manager::ExecutionUnitPass;
use super::placement_analysis::{PlacementAnalysis, ValueId, ValueOrigin, ValueSafety, ValueUse};
use super::shared::{def_reg, normalize_branch_condition};
use crate::PassOptions;
use crate::ir::analysis::{visit_instruction_uses, visit_terminator_uses};
use crate::ir::cfg::{SirCfg, SirDominance};
use crate::ir::{
    BasicBlock, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId, SIRInstruction,
    SIROffset, SIRTerminator,
};
use crate::{HashMap, HashSet};
use std::cmp::Reverse;
use std::collections::{BTreeSet, VecDeque};

pub(in crate::optimizer) struct BranchifyMuxPass;

#[derive(Clone)]
struct BranchifyPlan {
    block_id: BlockId,
    mux_idx: usize,
    dst: RegisterId,
    cond: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
    true_defs: Vec<usize>,
    false_defs: Vec<usize>,
    distributed_store: Option<DistributedStore>,
    preserve_result: bool,
}

#[derive(Clone)]
struct DistributedStore {
    idx: usize,
    true_inst: SIRInstruction<RegionedAbsoluteAddr>,
    false_inst: SIRInstruction<RegionedAbsoluteAddr>,
}

/// CFG facts used by BranchifyMux.  The old implementation looked only at the
/// block containing a Mux, which made it blind to the normal SSA shape
/// produced by lowering:
///
/// ```text
///             branch p
///             /       \
///       compute t   compute f
///             \       /
///              join: Mux(p, t, f)
/// ```
///
/// In that shape the arm work is already control-dependent, but the Mux still
/// survives as a branchless select.  The analysis below is deliberately
/// function-wide: it uses the complete predecessor graph, dominators and a
/// post-dominator tree to identify the controlled join in linear-ish time.
struct CfgAnalysis {
    graph: SirCfg,
    incoming_edges: Vec<Vec<(BlockId, Option<bool>)>>,
    path_facts: PathFacts,
}

#[derive(Clone)]
struct BranchInfo {
    source: BlockId,
    true_target: BlockId,
    false_target: BlockId,
}

#[derive(Clone)]
struct ControlledMuxPlan {
    join: BlockId,
    mux_idx: usize,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
    /// Each incoming edge is classified by the original branch's truth value.
    incoming: Vec<ControlledIncomingEdge>,
    /// Join-local single-use definitions moved to the selected predecessor.
    /// Moving and Mux removal are published together, so the intermediate
    /// non-dominating SSA shape is never observable by another pass.
    moved: Vec<ControlledMovedInstruction>,
}

#[derive(Clone, Copy)]
struct ControlledIncomingEdge {
    predecessor: BlockId,
    select_true: bool,
    /// `Some(true)`/`Some(false)` identifies a branch edge. `None` is a jump.
    edge_truth: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ControlledMovedInstruction {
    predecessor: BlockId,
    index: usize,
}

struct PathFacts {
    entry_facts: HashMap<BlockId, HashMap<PathFactKey, bool>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum PathFactKey {
    Register(RegisterId),
    Predicate(PredicateKey),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PredicateKey {
    lhs: RegisterId,
    kind: PredicateKind,
    rhs: PredicateRhs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PredicateKind {
    Equal,
    NotEqual,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum PredicateRhs {
    Register(RegisterId),
    Constant(Vec<u64>, Vec<u64>),
}

#[derive(Clone)]
struct LocatedInstruction {
    block: BlockId,
    index: usize,
    instruction: SIRInstruction<RegionedAbsoluteAddr>,
}

#[derive(Default)]
struct CrossBlockBatch {
    targets: HashSet<BlockId>,
    sources: HashSet<BlockId>,
    definitions: HashSet<(BlockId, usize)>,
}

impl CrossBlockBatch {
    fn reserve<'a>(
        &mut self,
        target: BlockId,
        definitions: impl Iterator<Item = &'a LocatedInstruction>,
    ) -> bool {
        if self.targets.contains(&target) || self.sources.contains(&target) {
            return false;
        }
        let locations = definitions.map(located_instruction_key).collect::<Vec<_>>();
        if locations.iter().any(|&(block, index)| {
            self.targets.contains(&block) || self.definitions.contains(&(block, index))
        }) {
            return false;
        }
        self.targets.insert(target);
        self.sources.extend(
            locations
                .iter()
                .map(|&(block, _)| block)
                .filter(|&block| block != target),
        );
        self.definitions.extend(locations);
        true
    }
}

/// Plans in a batch may remove distinct definitions from the same source
/// block, but never split another plan's source or target. Prefix counts map
/// their original indices to the indices after preceding removals. Storage
/// is linear in the affected source instructions, not in the number of plans.
#[derive(Default)]
struct CrossBlockOffsets {
    removed: HashMap<BlockId, Vec<usize>>,
}

impl CrossBlockOffsets {
    fn remap<'a>(
        &mut self,
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        target: BlockId,
        definitions: impl Iterator<Item = &'a mut LocatedInstruction>,
    ) {
        let mut removed = Vec::new();
        for definition in definitions {
            if definition.block == target {
                continue;
            }
            let counts = self
                .removed
                .entry(definition.block)
                .or_insert_with(|| vec![0; eu.blocks[&definition.block].instructions.len() + 1]);
            let original = definition.index;
            let mut position = original;
            while position != 0 {
                definition.index -= counts[position];
                position &= position - 1;
            }
            removed.push((definition.block, original));
        }
        // All definitions of this plan use the same pre-application indices.
        for (block, original) in removed {
            let counts = self
                .removed
                .get_mut(&block)
                .expect("source has prefix counts");
            let mut position = original + 1;
            while position < counts.len() {
                counts[position] += 1;
                position += position.isolate_lowest_one();
            }
        }
    }
}

#[derive(Clone)]
struct CrossBlockBranchifyPlan {
    block_id: BlockId,
    mux_idx: usize,
    dst: RegisterId,
    cond: RegisterId,
    condition_defs: Vec<LocatedInstruction>,
    true_val: RegisterId,
    false_val: RegisterId,
    true_defs: Vec<LocatedInstruction>,
    false_defs: Vec<LocatedInstruction>,
}

#[derive(Clone)]
struct PriorityChainMux {
    mux_idx: usize,
    dst: RegisterId,
    cond: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
}

#[derive(Clone)]
struct CrossBlockPriorityChainPlan {
    block_id: BlockId,
    first_mux_idx: usize,
    muxes: Vec<PriorityChainMux>,
    condition_defs: Vec<Vec<LocatedInstruction>>,
    /// Closed, single-use pure DAGs for the default value followed by each
    /// Mux's true value.  Keeping these separate from the decision DAG is
    /// essential: a case dispatch which evaluates its payload before testing
    /// the selector has removed Muxes without removing any dynamic work.
    arm_defs: Vec<Vec<LocatedInstruction>>,
}

#[derive(Clone)]
struct CrossGroupMux {
    mux_idx: usize,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
    condition_inverted: bool,
}

#[derive(Clone)]
struct CrossBlockGroupBranchifyPlan {
    block_id: BlockId,
    first_mux_idx: usize,
    branch_cond: RegisterId,
    muxes: Vec<CrossGroupMux>,
    true_defs: Vec<LocatedInstruction>,
    false_defs: Vec<LocatedInstruction>,
}

#[derive(Clone)]
struct CoupledStateUpdatePlan {
    block_id: BlockId,
    first_mux_idx: usize,
    cond: RegisterId,
    muxes: Vec<PriorityChainMux>,
    hoisted_defs: Vec<usize>,
    short_circuit: Option<CoupledShortCircuit>,
}

#[derive(Clone)]
struct CoupledPriorityLevel {
    cond: RegisterId,
    muxes: Vec<PriorityChainMux>,
}

#[derive(Clone)]
struct CoupledPriorityChainPlan {
    block_id: BlockId,
    first_mux_idx: usize,
    levels: Vec<CoupledPriorityLevel>,
}

#[derive(Clone)]
struct CoupledShortCircuit {
    guard: RegisterId,
    delayed: RegisterId,
    removed_defs: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PriorityPlacementSite {
    Decision(usize),
    Leaf(usize),
}
#[derive(Clone)]
struct PriorityPlacedInstruction {
    block: BlockId,
    index: usize,
    site: PriorityPlacementSite,
    instruction: SIRInstruction<RegionedAbsoluteAddr>,
}

#[derive(Clone)]
struct WholePriorityChainPlan {
    block_id: BlockId,
    first_mux_idx: usize,
    muxes: Vec<PriorityChainMux>,
    placed: Vec<PriorityPlacedInstruction>,
}

struct WholePriorityChainCandidate {
    plan: WholePriorityChainPlan,
    benefit_scaled: u128,
    depth: usize,
    assigned_values: HashSet<ValueId>,
}

struct AtomicPriorityPlacementPlan {
    regions: Vec<WholePriorityChainPlan>,
}

#[derive(Clone)]
struct ExistingCfgPlacedInstruction {
    value: ValueId,
    source_block: BlockId,
    source_index: usize,
    target_block: BlockId,
    topological_rank: usize,
    instruction: SIRInstruction<RegionedAbsoluteAddr>,
}

struct ExistingCfgPlacementPlan {
    placements: Vec<ExistingCfgPlacedInstruction>,
}

#[derive(Clone)]
struct SelectorPredicateArmPlan {
    selector_condition: RegisterId,
    payload_condition: RegisterId,
    decision_defs: Vec<usize>,
    payload_defs: Vec<usize>,
}

#[derive(Clone)]
struct SelectorPredicatePlan {
    block_id: BlockId,
    common_condition: RegisterId,
    true_target: (BlockId, Vec<RegisterId>),
    false_target: (BlockId, Vec<RegisterId>),
    removed_defs: Vec<usize>,
    arms: Vec<SelectorPredicateArmPlan>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct UseLocation {
    block: BlockId,
    instruction: Option<usize>,
}

impl ExecutionUnitPass for BranchifyMuxPass {
    fn name(&self) -> &'static str {
        "branchify_mux"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        run_branchify_mux(eu, options, None, true, true);
    }
}

pub(in crate::optimizer) fn run_late_branchify_mux(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    options: &PassOptions,
    previous_max_block: usize,
) {
    run_branchify_mux(eu, options, Some(previous_max_block), true, true);
}

fn run_branchify_mux(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    options: &PassOptions,
    controlled_join_after: Option<usize>,
    recover_controlled_joins: bool,
    enable_whole_function_rewrites: bool,
) {
    let diagnostics = &options.optimize_options.diagnostics;
    let stage_timing = diagnostics.pass_timing || diagnostics.branchify_stats;
    let mut previous_stage = stage_timing.then(crate::timing::now);
    let mut report_stage = |stage: &'static str| {
        if let Some(previous) = previous_stage.as_mut() {
            tracing::debug!("[branchify-timing] {stage}: {:?}", previous.elapsed());
            *previous = crate::timing::now();
        }
    };
    let verify_stage = |eu: &ExecutionUnit<RegionedAbsoluteAddr>, stage: &'static str| {
        if diagnostics.verify_passes
            && let Err(error) = eu.verify_result()
        {
            panic!("during branchify_mux {stage}: {error}");
        }
    };
    // A four-state Mux bitwise-merges its arms for an X/Z condition.
    // Control flow selects only one arm, so branchification cannot preserve
    // that behavior.
    if options.four_state {
        return;
    }
    // First consume Muxes whose arms are already guarded by an existing
    // branch.  This is the CFG case the old block-local pass missed: no
    // new control flow is needed, so the selected value can be carried as
    // a block parameter and the branchless Mux can be deleted outright.
    // Plan all such rewrites from one CFG snapshot; do not repeatedly
    // rescan the whole function for each Mux.
    if recover_controlled_joins {
        eliminate_controlled_join_muxes(eu, controlled_join_after);
    }
    verify_stage(eu, "controlled-join elimination");
    report_stage("controlled-join elimination");

    let stats = diagnostics.branchify_stats;
    let stats_start = stats.then(crate::timing::now);
    let trace_reg = diagnostics.branchify_trace_reg.map(RegisterId);
    let mut next_block_id = eu.blocks.keys().map(|id| id.0).max().unwrap_or(0) + 1;
    let mut reg_counter = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
    let mut applied = 0usize;
    let mut cross_priority_applied = 0usize;
    let mut cross_group_applied = 0usize;
    let mut cross_mux_applied = 0usize;
    let mut use_counts = count_uses(eu);

    // Recover a source-level conditional state update before treating its
    // Muxes independently. RTL lowering commonly separates correlated
    // updates into distant recurrence chains:
    //
    //   next_pri = Mux(c, candidate_pri, pri)
    //   ...
    //   next_id  = Mux(c, candidate_id, id)
    //
    // Keeping those as two selects extends `c` across the intervening
    // dataflow and prevents the backend from representing the update as
    // one branch carrying a state tuple. Process each resulting merge as
    // a worklist item so a sequence is recovered in source order without
    // repeatedly rescanning the whole execution unit.
    if enable_whole_function_rewrites {
        let priority_start = stage_timing.then(crate::timing::now);
        applied += branchify_coupled_priority_chains(
            eu,
            &use_counts,
            &mut next_block_id,
            &mut reg_counter,
        );
        if let Some(start) = priority_start {
            tracing::debug!(
                "[branchify-timing] coupled priority chains: {:?}",
                start.elapsed()
            );
        }
        let state_start = stage_timing.then(crate::timing::now);
        applied +=
            branchify_coupled_state_updates(eu, &use_counts, &mut next_block_id, &mut reg_counter);
        if let Some(start) = state_start {
            tracing::debug!(
                "[branchify-timing] coupled state updates: {:?}",
                start.elapsed()
            );
        }
        verify_stage(eu, "coupled updates");
    }
    report_stage("coupled updates");
    use_counts = count_uses(eu);

    // A priority spine is one short-circuit expression, not a collection
    // of independent selects.  Handle the whole spine before the
    // single-Mux motion below so later conditions and their pure compare
    // DAGs are evaluated only on the fall-through path.
    while enable_whole_function_rewrites
        && let Some(plans) = find_cross_block_priority_chain_plans(eu, &use_counts, true)
    {
        let mut offsets = CrossBlockOffsets::default();
        for mut plan in plans {
            offsets.remap(
                eu,
                plan.block_id,
                plan.condition_defs
                    .iter_mut()
                    .flatten()
                    .chain(plan.arm_defs.iter_mut().flatten()),
            );
            if let Some(register) = trace_reg {
                tracing::debug!(
                    "[branchify-trace] selected cross-block priority plan source=b{} first_mux={} muxes={} r{} uses={}",
                    plan.block_id.0,
                    plan.first_mux_idx,
                    plan.muxes.len(),
                    register.0,
                    use_counts.get(&register).copied().unwrap_or(0)
                );
                for (kind, group, definitions) in plan
                    .condition_defs
                    .iter()
                    .enumerate()
                    .map(|(index, definitions)| ("condition", index, definitions))
                    .chain(
                        plan.arm_defs
                            .iter()
                            .enumerate()
                            .map(|(index, definitions)| ("arm", index, definitions)),
                    )
                {
                    for definition in definitions {
                        if def_reg(&definition.instruction) == Some(register)
                            || inst_uses(&definition.instruction).contains(&register)
                        {
                            tracing::debug!(
                                "[branchify-trace] plan {kind}[{group}] b{} i{}: {}",
                                definition.block.0,
                                definition.index,
                                definition.instruction
                            );
                        }
                    }
                }
                for candidate in eu.blocks.values() {
                    trace_reg_in_new_block(candidate, register);
                }
            }
            let trace_plan = trace_reg.map(|register| {
                (
                    register,
                    plan.block_id,
                    plan.first_mux_idx,
                    plan.muxes.len(),
                )
            });
            apply_cross_block_priority_chain(eu, plan, &mut next_block_id, &mut reg_counter);
            if let Some((register, block, first_mux, muxes)) = trace_plan
                && let Err(error) = eu.verify_result()
            {
                tracing::debug!(
                    "[branchify-trace] invalid cross-block priority plan source=b{} first_mux={} muxes={}: {error}",
                    block.0,
                    first_mux,
                    muxes
                );
                for candidate in eu.blocks.values() {
                    trace_reg_in_new_block(candidate, register);
                }
                panic!("cross-block priority rewrite produced invalid SIR");
            }
            applied += 1;
            cross_priority_applied += 1;
        }
        use_counts = count_uses(eu);
    }
    verify_stage(eu, "cross-block priority chains");
    report_stage("cross-block priority chains");

    // The local transform below can only move definitions from one basic
    // block.  Before using it, repeatedly consume the existing
    // conservative cross-block plans: every moved instruction must be
    // pure, its defining block must dominate the Mux block, and every
    // moved definition must have exactly one use in the selected arm.
    // Preserve these already-proved short-circuit regions before the
    // whole-unit placement pass considers the residual Mux graph.
    while enable_whole_function_rewrites
        && let Some(plan) = find_cross_block_group_branchify_plan(eu)
    {
        apply_cross_block_group_branchify(eu, plan, &mut next_block_id, &mut reg_counter);
        applied += 1;
        cross_group_applied += 1;
        use_counts = count_uses(eu);
    }
    verify_stage(eu, "cross-block groups");
    report_stage("cross-block groups");
    while enable_whole_function_rewrites
        && let Some(plans) = find_cross_block_branchify_plans(eu, &use_counts, true)
    {
        let mut offsets = CrossBlockOffsets::default();
        for mut plan in plans {
            offsets.remap(
                eu,
                plan.block_id,
                plan.condition_defs
                    .iter_mut()
                    .chain(&mut plan.true_defs)
                    .chain(&mut plan.false_defs),
            );
            apply_cross_block_branchify(eu, plan, &mut next_block_id, &mut reg_counter);
            applied += 1;
            cross_mux_applied += 1;
        }
        use_counts = count_uses(eu);
    }
    verify_stage(eu, "cross-block muxes");
    report_stage("cross-block muxes");
    let mut def_blocks = instruction_def_blocks(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_by_key(|id| id.0);
    let mut worklist = VecDeque::from(block_ids);
    let mut queued = HashSet::default();
    queued.extend(worklist.iter().copied());
    while let Some(block_id) = worklist.pop_front() {
        queued.remove(&block_id);
        if !eu.blocks.contains_key(&block_id) {
            continue;
        }
        while let Some(plan) = find_branchify_mux_in_block(eu, block_id, &use_counts, &def_blocks) {
            let new_blocks = apply_branchify_mux(
                eu,
                plan,
                &mut use_counts,
                &mut def_blocks,
                &mut next_block_id,
                &mut reg_counter,
                trace_reg,
            );
            applied += 1;
            if stats && applied.is_multiple_of(1000) {
                let insts = eu
                    .blocks
                    .values()
                    .map(|block| block.instructions.len())
                    .sum::<usize>();
                tracing::debug!(
                    "[branchify-stats] applied={applied} blocks={} insts={} worklist={} elapsed={:?}",
                    eu.blocks.len(),
                    insts,
                    worklist.len(),
                    stats_start.unwrap().elapsed()
                );
            }
            for new_block in new_blocks {
                if queued.insert(new_block) {
                    worklist.push_back(new_block);
                }
            }
        }
    }
    verify_stage(eu, "local muxes");
    report_stage("local muxes");

    // The leaf fixed point has now exposed the residual nested Mux spines.
    // Select complete priority regions from one occurrence-aware snapshot
    // and apply the non-overlapping whole-unit plan atomically.  Running
    // here preserves every existing CFG/block-number decision and appends
    // only complete regions which the leaf transforms could not consume.
    let mut placement = enable_whole_function_rewrites
        .then(|| PlacementAnalysis::analyze(eu).ok())
        .flatten();
    let mut placement_stale = false;
    if let Some(placement) = &placement
        && let Some(plan) = find_atomic_priority_placement(eu, placement)
    {
        let regions =
            apply_atomic_priority_placement(eu, plan, &mut next_block_id, &mut reg_counter);
        applied += regions;
        placement_stale = regions != 0;
    }
    verify_stage(eu, "atomic priority placement");
    report_stage("atomic priority placement");

    // Rebuild placement facts after the atomic CFG rewrite, then perform
    // ordinary whole-unit ScheduleLate on the existing branch forest.
    // This catches pure/state-versioned DAGs which feed only one existing
    // control arm even when no Mux remains at the use site.  The complete
    // connected move is selected and preflighted before any block changes.
    if placement_stale {
        placement = PlacementAnalysis::analyze(eu).ok();
    }
    if let Some(placement) = &placement
        && let Some(plan) = find_existing_cfg_placement(eu, placement)
    {
        applied += apply_existing_cfg_placement(eu, plan);
    }
    verify_stage(eu, "existing CFG placement");
    report_stage("existing CFG placement");

    // A selector-disjoint Boolean sum is control flow, not a reason to
    // evaluate every payload shape eagerly:
    //
    //   common && ((kind == A && payload_a) ||
    //              (kind == B && payload_b) || ...)
    //
    // Lower the complete predicate after the ordinary placement pass so
    // only the selected payload DAG executes.  Planning is whole-EU and
    // each source block is rewritten once; newly added decision blocks do
    // not trigger a repeated global scan.
    if enable_whole_function_rewrites {
        applied += branchify_selector_guarded_predicates(eu, &mut next_block_id, &mut reg_counter);
    }
    verify_stage(eu, "selector predicates");
    report_stage("selector predicates");

    if stats {
        tracing::debug!(
            "[branchify-stats] before_pre_repair_inline applied={applied} blocks={} elapsed={:?}",
            eu.blocks.len(),
            stats_start.unwrap().elapsed()
        );
    }
    inline_param_only_jump_blocks(eu);
    verify_stage(eu, "first parameter-block inline");
    report_stage("first parameter-block inline");
    inline_param_only_jump_blocks(eu);
    verify_stage(eu, "second parameter-block inline");
    report_stage("second parameter-block inline");
    if stats {
        let insts = eu
            .blocks
            .values()
            .map(|block| block.instructions.len())
            .sum::<usize>();
        tracing::debug!(
            "[branchify-stats] done applied={applied} cross_priority={cross_priority_applied} cross_group={cross_group_applied} cross_mux={cross_mux_applied} blocks={} insts={} elapsed={:?}",
            eu.blocks.len(),
            insts,
            stats_start.unwrap().elapsed()
        );
    }
    if diagnostics.branchify_verify {
        verify_all_uses_have_defs(eu);
    }
}

#[derive(Clone)]
struct ParsedSelectorArm {
    selector_condition: RegisterId,
    payload_condition: RegisterId,
    selector_defs: HashSet<usize>,
    payload_defs: HashSet<usize>,
}

fn dominator_depth(cfg: &SirCfg, block: BlockId) -> usize {
    let Some(mut block) = cfg.block_index(block) else {
        return 0;
    };
    let mut depth = 0usize;
    while let Some(parent) = cfg.dominators.idom[block] {
        depth += 1;
        block = parent;
    }
    depth
}

fn register_use_locations(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, Vec<UseLocation>> {
    let mut uses = HashMap::<RegisterId, Vec<UseLocation>>::default();
    for block in eu.blocks.values() {
        for (register, locations) in block_register_use_locations(block) {
            uses.entry(register).or_default().extend(locations);
        }
    }
    uses
}

fn block_register_use_locations(
    block: &BasicBlock<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, Vec<UseLocation>> {
    let mut uses = HashMap::<RegisterId, Vec<UseLocation>>::default();
    for (index, instruction) in block.instructions.iter().enumerate() {
        for register in inst_uses(instruction) {
            uses.entry(register).or_default().push(UseLocation {
                block: block.id,
                instruction: Some(index),
            });
        }
    }
    for register in terminator_uses(&block.terminator) {
        uses.entry(register).or_default().push(UseLocation {
            block: block.id,
            instruction: None,
        });
    }
    uses
}

fn instruction_locations(instructions: &[LocatedInstruction]) -> HashSet<(BlockId, usize)> {
    instructions.iter().map(located_instruction_key).collect()
}

fn located_instruction_key(instruction: &LocatedInstruction) -> (BlockId, usize) {
    (instruction.block, instruction.index)
}

fn all_def_blocks(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, BlockId> {
    let mut defs = HashMap::default();
    for block in eu.blocks.values() {
        for &param in &block.params {
            defs.insert(param, block.id);
        }
        for inst in &block.instructions {
            if let Some(def) = def_reg(inst) {
                defs.insert(def, block.id);
            }
        }
    }
    defs
}

fn instruction_def_locations(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, (BlockId, usize)> {
    let mut defs = HashMap::default();
    for block in eu.blocks.values() {
        for (idx, inst) in block.instructions.iter().enumerate() {
            if let Some(def) = def_reg(inst) {
                defs.insert(def, (block.id, idx));
            }
        }
    }
    defs
}

impl CfgAnalysis {
    fn compute(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Option<Self> {
        let stats = tracing::enabled!(tracing::Level::DEBUG);
        // Controlled-join recovery needs predecessor, dominance, and
        // post-dominance queries, but not the potentially dense dominance
        // frontiers or control-dependence tables of the full analysis.
        let graph = SirCfg::analyze_structure(eu).ok()?;
        if stats {
            tracing::debug!("[branchify-stats] controlled_join cfg");
        }
        let def_locations = instruction_def_locations(eu);
        if stats {
            tracing::debug!("[branchify-stats] controlled_join defs");
        }
        let incoming_edges = indexed_incoming_edges(eu, &graph);
        if stats {
            tracing::debug!("[branchify-stats] controlled_join incoming");
        }
        let path_facts = PathFacts::compute(eu, &def_locations, &graph, &incoming_edges);
        if stats {
            tracing::debug!("[branchify-stats] controlled_join path_facts");
        }

        Some(Self {
            graph,
            incoming_edges,
            path_facts,
        })
    }

    fn incoming_edges(&self, target: BlockId) -> Option<&[(BlockId, Option<bool>)]> {
        self.graph
            .block_index(target)
            .and_then(|block| self.incoming_edges.get(block))
            .map(Vec::as_slice)
    }
}

impl PathFacts {
    fn compute(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        def_locations: &HashMap<RegisterId, (BlockId, usize)>,
        graph: &SirCfg,
        indexed_incoming: &[Vec<(BlockId, Option<bool>)>],
    ) -> Self {
        let mut entry_facts = HashMap::<BlockId, HashMap<PathFactKey, bool>>::default();
        for &block_id in &graph.block_ids {
            entry_facts.insert(block_id, HashMap::default());
        }

        let mut incoming = HashMap::<BlockId, Vec<(BlockId, Option<bool>)>>::default();
        let mut successors = HashMap::<BlockId, Vec<BlockId>>::default();
        for (block, &block_id) in graph.block_ids.iter().enumerate() {
            incoming.insert(block_id, indexed_incoming[block].clone());
            successors.insert(
                block_id,
                graph.successors[block]
                    .iter()
                    .map(|&successor| graph.block_ids[successor])
                    .collect(),
            );
        }

        let mut worklist = VecDeque::from_iter(graph.block_ids.iter().copied());
        while let Some(predecessor) = worklist.pop_front() {
            let targets = successors.get(&predecessor).cloned().unwrap_or_default();
            for target in targets {
                if target == eu.entry_block_id {
                    continue;
                }
                let mut intersection: Option<HashMap<PathFactKey, bool>> = None;
                for &(edge_predecessor, edge_truth) in &incoming[&target] {
                    let facts = &entry_facts[&edge_predecessor];
                    let Some(edge_facts) = facts_on_edge(
                        eu,
                        def_locations,
                        edge_predecessor,
                        target,
                        edge_truth,
                        facts,
                    ) else {
                        continue;
                    };
                    if let Some(current) = intersection.as_mut() {
                        current.retain(|register, value| edge_facts.get(register) == Some(value));
                    } else {
                        intersection = Some(edge_facts);
                    }
                }
                let Some(next) = intersection else {
                    continue;
                };
                if entry_facts[&target] != next {
                    entry_facts.insert(target, next);
                    worklist.push_back(target);
                }
            }
        }

        Self { entry_facts }
    }

    fn facts_on_edge(
        &self,
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        def_locations: &HashMap<RegisterId, (BlockId, usize)>,
        predecessor: BlockId,
        target: BlockId,
        edge_truth: Option<bool>,
    ) -> Option<HashMap<PathFactKey, bool>> {
        let facts = self.entry_facts.get(&predecessor)?;
        facts_on_edge(eu, def_locations, predecessor, target, edge_truth, facts)
    }
}

// Native and Cranelift both eventually turn a SIR branch into a conditional
// transfer, an executed arm-to-merge transfer, and (when the mux result is
// preserved) phi copies. With no profile, equality-to-constant decoder tests
// use the same 20/80 prior as cost-directed SLT lowering and other conditions
// use 50/50. A modern x86 misprediction is roughly 16 cycles.
//
// This is a local proof of expected benefit, not an iteration or function-size
// budget: the work expected to be skipped must strictly exceed every modeled
// downstream cost introduced by this particular transformation.
const BRANCH_CONTROL_COST: u128 = 3;
const MISPREDICT_COST: u128 = 16;
const PHI_COPY_COST_PER_CHUNK: u128 = 2;
const LIVE_THROUGH_COST_PER_CHUNK: u128 = 1;
// Cross-block motion adds three CFG blocks and extends the live range to a
// join.  The profitability proof below accounts for that cost directly; do
// not impose a separate compile-time or arbitrary work threshold here.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StaticBranchProbability {
    true_weight: u128,
    total_weight: u128,
}

impl StaticBranchProbability {
    const EVEN: Self = Self {
        true_weight: 1,
        total_weight: 2,
    };

    const EQUALITY_TO_CONSTANT: Self = Self {
        true_weight: 1,
        total_weight: 5,
    };

    fn inverted(self) -> Self {
        Self {
            true_weight: self.total_weight - self.true_weight,
            total_weight: self.total_weight,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BranchProfitability {
    true_arm_cost: u128,
    false_arm_cost: u128,
    removed_mux_cost: u128,
    probability: StaticBranchProbability,
    control_cost: u128,
    phi_copy_cost: u128,
    live_through_cost: u128,
}

impl BranchProfitability {
    fn expected_saved_scaled(self) -> u128 {
        let false_weight = self.probability.total_weight - self.probability.true_weight;
        false_weight
            .saturating_mul(self.true_arm_cost)
            .saturating_add(
                self.probability
                    .true_weight
                    .saturating_mul(self.false_arm_cost),
            )
            .saturating_add(
                self.probability
                    .total_weight
                    .saturating_mul(self.removed_mux_cost),
            )
    }

    fn introduced_cost_scaled(self) -> u128 {
        let false_weight = self.probability.total_weight - self.probability.true_weight;
        self.probability
            .total_weight
            .saturating_mul(
                self.control_cost
                    .saturating_add(self.phi_copy_cost)
                    .saturating_add(self.live_through_cost),
            )
            .saturating_add(
                self.probability
                    .true_weight
                    .min(false_weight)
                    .saturating_mul(MISPREDICT_COST),
            )
    }

    fn proves_expected_benefit(self) -> bool {
        self.expected_saved_scaled() > self.introduced_cost_scaled()
    }
}

#[derive(Clone, Copy)]
struct MemAccess<'a> {
    addr: &'a RegionedAbsoluteAddr,
    offset: Option<usize>,
    width: usize,
}

fn instruction_defs_in(head_insts: &[SIRInstruction<RegionedAbsoluteAddr>]) -> HashSet<RegisterId> {
    let mut defs = HashSet::default();
    for inst in head_insts {
        if let Some(def) = def_reg(inst) {
            defs.insert(def);
        }
    }
    defs
}

fn instruction_def_blocks(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, BlockId> {
    let mut defs = HashMap::default();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            if let Some(def) = def_reg(inst) {
                defs.insert(def, block.id);
            }
        }
    }
    defs
}

fn head_restore_defs(
    original: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    remove_defs: &HashSet<usize>,
    def_blocks: &HashMap<RegisterId, BlockId>,
) -> HashSet<usize> {
    let mut head_insts = Vec::new();
    for (idx, inst) in original.instructions.iter().enumerate().take(plan.mux_idx) {
        if !remove_defs.contains(&idx) {
            head_insts.push(inst.clone());
        }
    }
    let head_defs = instruction_defs_in(&head_insts);

    let mut suffix = Vec::new();
    for (idx, inst) in original
        .instructions
        .iter()
        .enumerate()
        .skip(plan.mux_idx + 1)
    {
        if !remove_defs.contains(&idx) {
            suffix.push(inst.clone());
        }
    }

    let mut merge_live_ins = block_live_ins(&suffix, &terminator_uses(&original.terminator));
    if plan.preserve_result {
        merge_live_ins.retain(|reg| *reg != plan.dst);
    }
    merge_live_ins.retain(|reg| {
        !head_defs.contains(reg)
            && def_blocks
                .get(reg)
                .is_none_or(|def_block| *def_block >= plan.block_id)
    });

    let mut true_args = if plan.preserve_result {
        vec![plan.true_val]
    } else {
        Vec::new()
    };
    true_args.extend(merge_live_ins.iter().copied());
    let mut false_args = if plan.preserve_result {
        vec![plan.false_val]
    } else {
        Vec::new()
    };
    false_args.extend(merge_live_ins.iter().copied());

    let true_insts = plan
        .true_defs
        .iter()
        .filter(|idx| remove_defs.contains(idx))
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    let false_insts = plan
        .false_defs
        .iter()
        .filter(|idx| remove_defs.contains(idx))
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    let true_live_ins = block_live_ins(&true_insts, &true_args);
    let false_live_ins = block_live_ins(&false_insts, &false_args);

    let mut needed = HashSet::default();
    needed.insert(plan.cond);
    needed.extend(true_live_ins);
    needed.extend(false_live_ins);
    collect_removed_defs_needed_by_head(original, remove_defs, needed)
}

fn collect_removed_defs_needed_by_head(
    original: &BasicBlock<RegionedAbsoluteAddr>,
    remove_defs: &HashSet<usize>,
    needed: HashSet<RegisterId>,
) -> HashSet<usize> {
    let mut removed_def_pos = HashMap::default();
    for &idx in remove_defs {
        if let Some(def) = def_reg(&original.instructions[idx]) {
            removed_def_pos.insert(def, idx);
        }
    }

    let mut restore = HashSet::default();
    let mut queue = VecDeque::from_iter(needed);
    let mut seen = HashSet::default();
    while let Some(reg) = queue.pop_front() {
        if !seen.insert(reg) {
            continue;
        }
        let Some(&idx) = removed_def_pos.get(&reg) else {
            continue;
        };
        if restore.insert(idx) {
            for use_reg in inst_uses(&original.instructions[idx]) {
                queue.push_back(use_reg);
            }
        }
    }
    restore
}

// A backward scan gives every Mux's exact suffix cost. The planners ask for
// this table lazily per block, instead of cloning and scanning the same long
// suffix for each candidate. The Mux result travels through its phi and is
// charged separately, so exclude it from the live-through total.
fn mux_live_through_chunks(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    register_map: &HashMap<RegisterId, crate::ir::RegisterType>,
) -> Vec<u128> {
    mux_suffix_live_through_chunks(block, register_map, false)
}

// A distributed store moves with the arms; its address and source uses do not
// belong to the remaining suffix. Sample liveness before visiting that store.
fn mux_store_live_through_chunks(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    register_map: &HashMap<RegisterId, crate::ir::RegisterType>,
) -> Vec<u128> {
    mux_suffix_live_through_chunks(block, register_map, true)
}

fn mux_suffix_live_through_chunks(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    register_map: &HashMap<RegisterId, crate::ir::RegisterType>,
    skip_store: bool,
) -> Vec<u128> {
    let chunks_for = |value: RegisterId| {
        register_map
            .get(&value)
            .map(|register| register.width().div_ceil(64).max(1))
            .unwrap_or(1) as u128
    };
    let mut live = HashSet::default();
    let mut chunks = 0u128;
    visit_terminator_uses(&block.terminator, |value| {
        if live.insert(value) {
            chunks += chunks_for(value);
        }
    });
    let mut costs = vec![0; block.instructions.len()];
    for (index, instruction) in block.instructions.iter().enumerate().rev() {
        let mux_index = if skip_store {
            if matches!(instruction, SIRInstruction::Store(..)) {
                index.checked_sub(1)
            } else {
                None
            }
        } else {
            Some(index)
        };
        if let Some(mux_index) = mux_index
            && let SIRInstruction::Mux(destination, ..) = &block.instructions[mux_index]
        {
            costs[mux_index] = chunks
                - if live.contains(destination) {
                    chunks_for(*destination)
                } else {
                    0
                };
        }
        if let Some(definition) = def_reg(instruction)
            && live.remove(&definition)
        {
            chunks -= chunks_for(definition);
        }
        visit_instruction_uses(instruction, |value| {
            if live.insert(value) {
                chunks += chunks_for(value);
            }
        });
    }
    costs
}

fn block_live_ins(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    terminator_args: &[RegisterId],
) -> Vec<RegisterId> {
    let mut defs = HashSet::default();
    let mut live_ins = Vec::new();
    let mut seen = HashSet::default();

    for inst in instructions {
        visit_instruction_uses(inst, |reg| {
            if !defs.contains(&reg) && seen.insert(reg) {
                live_ins.push(reg);
            }
        });
        if let Some(def) = def_reg(inst) {
            defs.insert(def);
        }
    }
    for &reg in terminator_args {
        if !defs.contains(&reg) && seen.insert(reg) {
            live_ins.push(reg);
        }
    }

    live_ins
}

fn verify_all_uses_have_defs(eu: &ExecutionUnit<RegionedAbsoluteAddr>) {
    let mut defs = HashSet::default();
    for block in eu.blocks.values() {
        defs.extend(block.params.iter().copied());
        for inst in &block.instructions {
            if let Some(def) = def_reg(inst) {
                defs.insert(def);
            }
        }
    }

    for block in eu.blocks.values() {
        for (idx, inst) in block.instructions.iter().enumerate() {
            for reg in inst_uses(inst) {
                assert!(
                    defs.contains(&reg),
                    "branchify verify: r{} used without def/param in b{} inst {}: {}",
                    reg.0,
                    block.id.0,
                    idx,
                    inst
                );
            }
        }
        for reg in terminator_uses(&block.terminator) {
            assert!(
                defs.contains(&reg),
                "branchify verify: r{} used without def/param in b{} terminator: {}",
                reg.0,
                block.id.0,
                block.terminator
            );
        }
    }
}

fn count_uses(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, usize> {
    let mut counts = HashMap::default();
    for block in eu.blocks.values() {
        add_block_uses(&mut counts, block);
    }
    counts
}

fn add_block_uses(
    counts: &mut HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
) {
    for inst in &block.instructions {
        visit_instruction_uses(inst, |reg| {
            *counts.entry(reg).or_default() += 1;
        });
    }
    visit_terminator_uses(&block.terminator, |reg| {
        *counts.entry(reg).or_default() += 1;
    });
}

fn remove_block_uses(
    counts: &mut HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
) {
    for inst in &block.instructions {
        visit_instruction_uses(inst, |reg| decrement_use(counts, reg));
    }
    visit_terminator_uses(&block.terminator, |reg| decrement_use(counts, reg));
}

fn decrement_use(counts: &mut HashMap<RegisterId, usize>, reg: RegisterId) {
    let Some(count) = counts.get_mut(&reg) else {
        return;
    };
    *count -= 1;
    if *count == 0 {
        counts.remove(&reg);
    }
}

fn inst_uses(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
    let mut uses = Vec::new();
    inst.for_each_use(|register| uses.push(register));
    uses
}

fn terminator_uses(term: &SIRTerminator) -> Vec<RegisterId> {
    match term {
        SIRTerminator::Jump(_, args) => args.clone(),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            let mut uses = Vec::with_capacity(1 + true_block.1.len() + false_block.1.len());
            uses.push(*cond);
            uses.extend(true_block.1.iter().copied());
            uses.extend(false_block.1.iter().copied());
            uses
        }
        SIRTerminator::Switch { selector, .. } => vec![*selector],
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests;
