use super::pass_manager::ExecutionUnitPass;
use super::placement_analysis::{PlacementAnalysis, ValueId, ValueOrigin, ValueSafety, ValueUse};
use super::shared::{def_reg, normalize_branch_condition};
use crate::PassOptions;
use crate::ir::cfg::SirCfg;
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
        && let Some(plan) = find_cross_block_priority_chain_plan(eu, &use_counts)
    {
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
        && let Some(plan) = find_cross_block_branchify_plan(eu, &use_counts)
    {
        apply_cross_block_branchify(eu, plan, &mut next_block_id, &mut reg_counter);
        applied += 1;
        cross_mux_applied += 1;
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

fn branchify_selector_guarded_predicates(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) -> usize {
    let def_locations = instruction_def_locations(eu);
    let use_locations = register_use_locations(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|block| block.0);
    let plans = block_ids
        .into_iter()
        .filter_map(|block| find_selector_predicate_plan(eu, &def_locations, &use_locations, block))
        .collect::<Vec<_>>();
    let applied = plans.len();
    for plan in plans {
        apply_selector_predicate_plan(eu, plan, next_block_id, reg_counter);
    }
    applied
}

fn find_selector_predicate_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    use_locations: &HashMap<RegisterId, Vec<UseLocation>>,
    block_id: BlockId,
) -> Option<SelectorPredicatePlan> {
    let block = eu.blocks.get(&block_id)?;
    let SIRTerminator::Branch {
        cond,
        true_block,
        false_block,
    } = &block.terminator
    else {
        return None;
    };
    if true_block.0 == false_block.0
        || block.instructions.iter().any(|instruction| {
            !matches!(
                instruction,
                SIRInstruction::Imm(..)
                    | SIRInstruction::Load(..)
                    | SIRInstruction::Binary(..)
                    | SIRInstruction::Unary(..)
                    | SIRInstruction::Concat(..)
                    | SIRInstruction::Slice(..)
                    | SIRInstruction::Mux(..)
            )
        })
    {
        // Delaying a Load is valid across pure computation, but not across an
        // observable write, commit, or runtime event in the source block.
        return None;
    }

    let mut root_removed = HashSet::default();
    let root = peel_selector_boolean_alias(eu, def_locations, block_id, *cond, &mut root_removed);
    let &(root_block, root_index) = def_locations.get(&root)?;
    if root_block != block_id {
        return None;
    }
    let SIRInstruction::Binary(_, lhs, crate::ir::BinaryOp::LogicAnd, rhs) =
        &block.instructions[root_index]
    else {
        return None;
    };
    root_removed.insert(root_index);

    for (common_condition, selector_expression) in [(*lhs, *rhs), (*rhs, *lhs)] {
        let mut removed = root_removed.clone();
        let Some(mut arms) = parse_selector_sum(
            eu,
            def_locations,
            block_id,
            selector_expression,
            &mut removed,
        ) else {
            continue;
        };

        let edge_arguments = true_block
            .1
            .iter()
            .chain(&false_block.1)
            .copied()
            .collect::<HashSet<_>>();
        let removed_is_closed = removed.iter().all(|&index| {
            let Some(dst) = def_reg(&block.instructions[index]) else {
                return false;
            };
            if edge_arguments.contains(&dst) {
                return false;
            }
            use_locations
                .get(&dst)
                .into_iter()
                .flatten()
                .all(|location| {
                    location.block == block_id
                        && match location.instruction {
                            Some(use_index) => removed.contains(&use_index),
                            None => dst == *cond,
                        }
                })
        });
        if !removed_is_closed {
            continue;
        }

        for arm in &mut arms {
            collect_selector_motion_closure(
                eu,
                def_locations,
                block_id,
                arm.selector_condition,
                &removed,
                &mut arm.selector_defs,
            );
            collect_selector_motion_closure(
                eu,
                def_locations,
                block_id,
                arm.payload_condition,
                &removed,
                &mut arm.payload_defs,
            );
        }

        // An instruction reachable from more than one selector arm must stay
        // in the head.  This keeps the transformation linear and avoids
        // duplicating shared address or expected-value computation.
        let mut membership = HashMap::<usize, (usize, usize)>::default();
        for (arm_index, arm) in arms.iter().enumerate() {
            let mut all = arm.selector_defs.clone();
            all.extend(arm.payload_defs.iter().copied());
            for index in all {
                membership
                    .entry(index)
                    .and_modify(|(owner, count)| {
                        if *owner != arm_index {
                            *count += 1;
                        }
                    })
                    .or_insert((arm_index, 1));
            }
        }
        let mut movable = membership
            .iter()
            .filter_map(|(&index, &(_, count))| {
                (count == 1 && !removed.contains(&index)).then_some(index)
            })
            .collect::<HashSet<_>>();

        // Close the selected move set under uses.  If one candidate definition
        // is also consumed by a head instruction, edge argument, or another
        // block, retain it in the head and transitively retain its operands.
        let mut reject = VecDeque::new();
        for &index in &movable {
            if !selector_definition_uses_are_closed(
                block,
                use_locations,
                block_id,
                index,
                &movable,
                &removed,
            ) {
                reject.push_back(index);
            }
        }
        while let Some(index) = reject.pop_front() {
            if !movable.remove(&index) {
                continue;
            }
            for operand in inst_uses(&block.instructions[index]) {
                let Some(&(definition_block, definition_index)) = def_locations.get(&operand)
                else {
                    continue;
                };
                if definition_block == block_id
                    && movable.contains(&definition_index)
                    && !selector_definition_uses_are_closed(
                        block,
                        use_locations,
                        block_id,
                        definition_index,
                        &movable,
                        &removed,
                    )
                {
                    reject.push_back(definition_index);
                }
            }
        }

        let arms_with_delayed_load = arms
            .iter()
            .enumerate()
            .filter(|(arm_index, _)| {
                let arm_index = *arm_index;
                movable.iter().any(|&index| {
                    membership.get(&index) == Some(&(arm_index, 1))
                        && matches!(block.instructions[index], SIRInstruction::Load(..))
                })
            })
            .count();
        if arms_with_delayed_load < 2 {
            // The extra selector and payload branches must skip real memory
            // work on at least two mutually exclusive paths.
            continue;
        }

        let planned_arms = arms
            .into_iter()
            .enumerate()
            .map(|(arm_index, arm)| {
                let mut owned = movable
                    .iter()
                    .filter_map(|&index| {
                        (membership.get(&index) == Some(&(arm_index, 1))).then_some(index)
                    })
                    .collect::<Vec<_>>();
                owned.sort_unstable();
                let (decision_defs, payload_defs) = owned
                    .into_iter()
                    .partition(|index| arm.selector_defs.contains(index));
                SelectorPredicateArmPlan {
                    selector_condition: arm.selector_condition,
                    payload_condition: arm.payload_condition,
                    decision_defs,
                    payload_defs,
                }
            })
            .collect::<Vec<_>>();
        let mut removed_defs = removed.into_iter().collect::<Vec<_>>();
        removed_defs.sort_unstable();
        return Some(SelectorPredicatePlan {
            block_id,
            common_condition,
            true_target: true_block.clone(),
            false_target: false_block.clone(),
            removed_defs,
            arms: planned_arms,
        });
    }
    None
}

fn peel_selector_boolean_alias(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    block_id: BlockId,
    mut register: RegisterId,
    removed: &mut HashSet<usize>,
) -> RegisterId {
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let Some(&(definition_block, index)) = def_locations.get(&register) else {
            break;
        };
        if definition_block != block_id {
            break;
        }
        let instruction = &eu.blocks[&block_id].instructions[index];
        let source = match instruction {
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => Some(*source),
            SIRInstruction::Unary(_, crate::ir::UnaryOp::And | crate::ir::UnaryOp::Or, source)
                if eu
                    .register_map
                    .get(source)
                    .is_some_and(|register| register.width() == 1) =>
            {
                Some(*source)
            }
            _ => None,
        };
        let Some(source) = source else {
            break;
        };
        removed.insert(index);
        register = source;
    }
    register
}

fn parse_selector_sum(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    block_id: BlockId,
    expression: RegisterId,
    removed: &mut HashSet<usize>,
) -> Option<Vec<ParsedSelectorArm>> {
    let mut terms = Vec::new();
    let mut seen = HashSet::default();
    if !flatten_selector_or(
        eu,
        def_locations,
        block_id,
        expression,
        removed,
        &mut seen,
        &mut terms,
    ) || !(2..=8).contains(&terms.len())
    {
        return None;
    }

    let mut operands = Vec::with_capacity(terms.len());
    for term in terms {
        let term = peel_selector_boolean_alias(eu, def_locations, block_id, term, removed);
        let &(definition_block, index) = def_locations.get(&term)?;
        if definition_block != block_id {
            return None;
        }
        let SIRInstruction::Binary(_, lhs, crate::ir::BinaryOp::LogicAnd, rhs) =
            &eu.blocks[&block_id].instructions[index]
        else {
            return None;
        };
        removed.insert(index);
        operands.push([*lhs, *rhs]);
    }

    for first_selector_side in 0..2 {
        let Some((selector, first_constant)) =
            selector_guard_key(eu, def_locations, operands[0][first_selector_side])
        else {
            continue;
        };
        if selector_guard_key(eu, def_locations, operands[0][1 - first_selector_side])
            .is_some_and(|(other_selector, _)| other_selector == selector)
        {
            continue;
        }
        let mut constants = HashSet::default();
        constants.insert(first_constant);
        let mut arms = vec![ParsedSelectorArm {
            selector_condition: operands[0][first_selector_side],
            payload_condition: operands[0][1 - first_selector_side],
            selector_defs: HashSet::default(),
            payload_defs: HashSet::default(),
        }];
        let mut valid = true;
        for pair in operands.iter().skip(1) {
            let matches = (0..2)
                .filter_map(|side| {
                    let (candidate_selector, constant) =
                        selector_guard_key(eu, def_locations, pair[side])?;
                    (candidate_selector == selector).then_some((side, constant))
                })
                .collect::<Vec<_>>();
            if matches.len() != 1 || !constants.insert(matches[0].1.clone()) {
                valid = false;
                break;
            }
            let side = matches[0].0;
            arms.push(ParsedSelectorArm {
                selector_condition: pair[side],
                payload_condition: pair[1 - side],
                selector_defs: HashSet::default(),
                payload_defs: HashSet::default(),
            });
        }
        if valid {
            return Some(arms);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn flatten_selector_or(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    block_id: BlockId,
    expression: RegisterId,
    removed: &mut HashSet<usize>,
    seen: &mut HashSet<RegisterId>,
    terms: &mut Vec<RegisterId>,
) -> bool {
    let expression = peel_selector_boolean_alias(eu, def_locations, block_id, expression, removed);
    if !seen.insert(expression) {
        return false;
    }
    let Some(&(definition_block, index)) = def_locations.get(&expression) else {
        terms.push(expression);
        return true;
    };
    if definition_block != block_id {
        terms.push(expression);
        return true;
    }
    let SIRInstruction::Binary(_, lhs, crate::ir::BinaryOp::LogicOr, rhs) =
        &eu.blocks[&block_id].instructions[index]
    else {
        terms.push(expression);
        return true;
    };
    removed.insert(index);
    flatten_selector_or(eu, def_locations, block_id, *lhs, removed, seen, terms)
        && flatten_selector_or(eu, def_locations, block_id, *rhs, removed, seen, terms)
}

fn selector_guard_key(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    condition: RegisterId,
) -> Option<(RegisterId, Vec<u64>)> {
    let (key, inverted) = predicate_key(eu, def_locations, condition)?;
    if inverted || key.kind != PredicateKind::Equal {
        return None;
    }
    let PredicateRhs::Constant(payload, mask) = key.rhs else {
        return None;
    };
    mask.iter()
        .all(|word| *word == 0)
        .then_some((key.lhs, payload))
}

fn collect_selector_motion_closure(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    block_id: BlockId,
    root: RegisterId,
    removed: &HashSet<usize>,
    result: &mut HashSet<usize>,
) {
    let Some(&(definition_block, index)) = def_locations.get(&root) else {
        return;
    };
    if definition_block != block_id || removed.contains(&index) || !result.insert(index) {
        return;
    }
    let instruction = &eu.blocks[&block_id].instructions[index];
    if !matches!(
        instruction,
        SIRInstruction::Imm(..)
            | SIRInstruction::Load(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..)
    ) {
        result.remove(&index);
        return;
    }
    for operand in inst_uses(instruction) {
        collect_selector_motion_closure(eu, def_locations, block_id, operand, removed, result);
    }
}

fn selector_definition_uses_are_closed(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    use_locations: &HashMap<RegisterId, Vec<UseLocation>>,
    block_id: BlockId,
    index: usize,
    movable: &HashSet<usize>,
    removed: &HashSet<usize>,
) -> bool {
    let Some(dst) = def_reg(&block.instructions[index]) else {
        return false;
    };
    use_locations
        .get(&dst)
        .into_iter()
        .flatten()
        .all(|location| {
            location.block == block_id
                && location.instruction.is_some_and(|use_index| {
                    movable.contains(&use_index) || removed.contains(&use_index)
                })
        })
}

fn apply_selector_predicate_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: SelectorPredicatePlan,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) {
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("selector-predicate source block must exist");
    let removed = plan.removed_defs.iter().copied().collect::<HashSet<_>>();
    let moved = plan
        .arms
        .iter()
        .flat_map(|arm| arm.decision_defs.iter().chain(&arm.payload_defs))
        .copied()
        .collect::<HashSet<_>>();
    debug_assert!(removed.is_disjoint(&moved));

    let arm_blocks = (0..plan.arms.len())
        .map(|_| {
            let decision = BlockId(*next_block_id);
            let payload = BlockId(*next_block_id + 1);
            *next_block_id += 2;
            (decision, payload)
        })
        .collect::<Vec<_>>();
    let mut head_instructions = original
        .instructions
        .iter()
        .enumerate()
        .filter(|(index, _)| !removed.contains(index) && !moved.contains(index))
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let common_condition = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_instructions,
        plan.common_condition,
        reg_counter,
    );
    let head = BasicBlock {
        id: original.id,
        params: original.params.clone(),
        instructions: head_instructions,
        terminator: SIRTerminator::Branch {
            cond: common_condition,
            true_block: (arm_blocks[0].0, Vec::new()),
            false_block: plan.false_target.clone(),
        },
    };
    eu.blocks.insert(head.id, head);

    for (arm_index, (arm, &(decision_id, payload_id))) in
        plan.arms.iter().zip(&arm_blocks).enumerate()
    {
        let decision_false = arm_blocks
            .get(arm_index + 1)
            .map_or_else(|| plan.false_target.clone(), |next| (next.0, Vec::new()));
        let mut decision_instructions = arm
            .decision_defs
            .iter()
            .map(|&index| original.instructions[index].clone())
            .collect::<Vec<_>>();
        let selector_condition = normalize_branch_condition(
            &mut eu.register_map,
            &mut decision_instructions,
            arm.selector_condition,
            reg_counter,
        );
        eu.blocks.insert(
            decision_id,
            BasicBlock {
                id: decision_id,
                params: Vec::new(),
                instructions: decision_instructions,
                terminator: SIRTerminator::Branch {
                    cond: selector_condition,
                    true_block: (payload_id, Vec::new()),
                    false_block: decision_false,
                },
            },
        );
        let mut payload_instructions = arm
            .payload_defs
            .iter()
            .map(|&index| original.instructions[index].clone())
            .collect::<Vec<_>>();
        let payload_condition = normalize_branch_condition(
            &mut eu.register_map,
            &mut payload_instructions,
            arm.payload_condition,
            reg_counter,
        );
        eu.blocks.insert(
            payload_id,
            BasicBlock {
                id: payload_id,
                params: Vec::new(),
                instructions: payload_instructions,
                terminator: SIRTerminator::Branch {
                    cond: payload_condition,
                    true_block: plan.true_target.clone(),
                    false_block: plan.false_target.clone(),
                },
            },
        );
    }
}

fn branchify_coupled_state_updates(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    use_counts: &HashMap<RegisterId, usize>,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) -> usize {
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|block| block.0);
    let mut applied = 0usize;

    for block_id in block_ids {
        let plans = plan_coupled_state_updates_in_block(eu, block_id, use_counts);
        if plans.is_empty() {
            continue;
        }
        applied += plans.len();
        apply_coupled_state_update_batch(eu, plans, next_block_id, reg_counter);
    }

    applied
}

fn branchify_coupled_priority_chains(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    use_counts: &HashMap<RegisterId, usize>,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) -> usize {
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|block| block.0);
    let mut worklist = VecDeque::from(block_ids);
    let mut applied = 0usize;

    while let Some(block_id) = worklist.pop_front() {
        let Some(plan) = find_coupled_priority_chain_in_block(eu, block_id, use_counts) else {
            continue;
        };
        let merge = apply_coupled_priority_chain(eu, plan, next_block_id, reg_counter);
        applied += 1;
        worklist.push_front(merge);
    }
    applied
}

fn find_coupled_priority_chain_in_block(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    use_counts: &HashMap<RegisterId, usize>,
) -> Option<CoupledPriorityChainPlan> {
    let block = eu.blocks.get(&block_id)?;
    let mut best = None;
    let mut muxes = Vec::new();
    let mut def_pos = HashMap::default();
    for (mux_idx, instruction) in block.instructions.iter().enumerate() {
        if let Some(register) = def_reg(instruction) {
            def_pos.insert(register, mux_idx);
        }
        if let SIRInstruction::Mux(dst, cond, true_val, false_val) = instruction {
            muxes.push(PriorityChainMux {
                mux_idx,
                dst: *dst,
                cond: *cond,
                true_val: *true_val,
                false_val: *false_val,
            });
        }
    }
    let mut groups = HashMap::<RegisterId, Vec<PriorityChainMux>>::default();
    for mux in &muxes {
        groups.entry(mux.cond).or_default().push(mux.clone());
    }
    for group in groups.values_mut() {
        group.sort_unstable_by_key(|mux| mux.mux_idx);
    }

    for inner in groups.values().filter(|group| group.len() >= 2) {
        let mut levels = vec![CoupledPriorityLevel {
            cond: inner[0].cond,
            muxes: inner.clone(),
        }];
        loop {
            let current = levels.last().unwrap();
            let current_outputs = current.muxes.iter().map(|mux| mux.dst).collect::<Vec<_>>();
            let mut successors = groups
                .iter()
                .filter(|(cond, _)| **cond != current.cond)
                .filter_map(|(&cond, group)| {
                    let mapped = current_outputs
                        .iter()
                        .map(|output| group.iter().find(|mux| mux.false_val == *output).cloned())
                        .collect::<Option<Vec<_>>>()?;
                    mapped
                        .iter()
                        .zip(&current.muxes)
                        .all(|(next, previous)| next.mux_idx > previous.mux_idx)
                        .then_some(CoupledPriorityLevel {
                            cond,
                            muxes: mapped,
                        })
                })
                .collect::<Vec<_>>();
            successors.sort_unstable_by_key(|level| {
                level
                    .muxes
                    .iter()
                    .map(|mux| mux.mux_idx)
                    .min()
                    .unwrap_or(usize::MAX)
            });
            let Some(next) = successors.into_iter().next() else {
                break;
            };
            if next
                .muxes
                .iter()
                .zip(&current.muxes)
                .any(|(next, previous)| {
                    use_counts.get(&previous.dst).copied() != Some(1)
                        || next.false_val != previous.dst
                })
            {
                break;
            }
            levels.push(next);
        }
        if levels.len() < 2 {
            continue;
        }

        let chain_outputs = levels
            .iter()
            .flat_map(|level| level.muxes.iter().map(|mux| mux.dst))
            .collect::<HashSet<_>>();
        let first_mux_idx = levels
            .iter()
            .flat_map(|level| level.muxes.iter().map(|mux| mux.mux_idx))
            .min()?;
        let roots = levels
            .iter()
            .flat_map(|level| {
                std::iter::once(level.cond).chain(level.muxes.iter().map(|mux| mux.true_val))
            })
            .chain(levels[0].muxes.iter().map(|mux| mux.false_val));
        if roots.into_iter().any(|root| {
            chain_outputs.contains(&root)
                || def_pos
                    .get(&root)
                    .is_some_and(|&index| index >= first_mux_idx)
        }) {
            continue;
        }
        let candidate = CoupledPriorityChainPlan {
            block_id,
            first_mux_idx,
            levels,
        };
        if best
            .as_ref()
            .is_none_or(|current: &CoupledPriorityChainPlan| {
                candidate.levels.len() > current.levels.len()
                    || candidate.levels.len() == current.levels.len()
                        && (candidate.block_id, candidate.first_mux_idx)
                            < (current.block_id, current.first_mux_idx)
            })
        {
            best = Some(candidate);
        }
    }
    best
}

fn apply_coupled_priority_chain(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: CoupledPriorityChainPlan,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) -> BlockId {
    let depth = plan.levels.len();
    let base = *next_block_id;
    let decision_ids = (0..depth)
        .map(|level| {
            if level + 1 == depth {
                plan.block_id
            } else {
                BlockId(base + level)
            }
        })
        .collect::<Vec<_>>();
    let leaf_base = base + depth - 1;
    let leaf_ids = (0..=depth)
        .map(|leaf| BlockId(leaf_base + leaf))
        .collect::<Vec<_>>();
    let merge_id = BlockId(leaf_base + depth + 1);
    *next_block_id = merge_id.0 + 1;

    let original = eu.blocks.remove(&plan.block_id).unwrap();
    let removed = plan
        .levels
        .iter()
        .flat_map(|level| level.muxes.iter().map(|mux| mux.mux_idx))
        .collect::<HashSet<_>>();
    let head = original.instructions[..plan.first_mux_idx].to_vec();
    let continuation = original
        .instructions
        .into_iter()
        .enumerate()
        .skip(plan.first_mux_idx)
        .filter_map(|(index, instruction)| (!removed.contains(&index)).then_some(instruction))
        .collect::<Vec<_>>();

    for level in (0..depth).rev() {
        let mut instructions = if level + 1 == depth {
            head.clone()
        } else {
            Vec::new()
        };
        let cond = normalize_branch_condition(
            &mut eu.register_map,
            &mut instructions,
            plan.levels[level].cond,
            reg_counter,
        );
        eu.blocks.insert(
            decision_ids[level],
            BasicBlock {
                id: decision_ids[level],
                params: if level + 1 == depth {
                    original.params.clone()
                } else {
                    Vec::new()
                },
                instructions,
                terminator: SIRTerminator::Branch {
                    cond,
                    true_block: (leaf_ids[level + 1], Vec::new()),
                    false_block: if level == 0 {
                        (leaf_ids[0], Vec::new())
                    } else {
                        (decision_ids[level - 1], Vec::new())
                    },
                },
            },
        );
    }
    for (leaf, &leaf_id) in leaf_ids.iter().enumerate() {
        let values = if leaf == 0 {
            plan.levels[0]
                .muxes
                .iter()
                .map(|mux| mux.false_val)
                .collect()
        } else {
            plan.levels[leaf - 1]
                .muxes
                .iter()
                .map(|mux| mux.true_val)
                .collect()
        };
        eu.blocks.insert(
            leaf_id,
            BasicBlock {
                id: leaf_id,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(merge_id, values),
            },
        );
    }
    eu.blocks.insert(
        merge_id,
        BasicBlock {
            id: merge_id,
            params: plan
                .levels
                .last()
                .unwrap()
                .muxes
                .iter()
                .map(|mux| mux.dst)
                .collect(),
            instructions: continuation,
            terminator: original.terminator,
        },
    );
    debug_assert_eq!(eu.verify_result(), Ok(()));
    merge_id
}

fn plan_coupled_state_updates_in_block(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    use_counts: &HashMap<RegisterId, usize>,
) -> Vec<CoupledStateUpdatePlan> {
    let Some(block) = eu.blocks.get(&block_id) else {
        return Vec::new();
    };
    let mut all_muxes = Vec::new();
    let mut def_pos = HashMap::default();
    for (mux_idx, instruction) in block.instructions.iter().enumerate() {
        if let Some(register) = def_reg(instruction) {
            def_pos.insert(register, mux_idx);
        }
        if let SIRInstruction::Mux(dst, cond, true_val, false_val) = instruction {
            all_muxes.push(PriorityChainMux {
                mux_idx,
                dst: *dst,
                cond: *cond,
                true_val: *true_val,
                false_val: *false_val,
            });
        }
    }
    if all_muxes.len() < 2 {
        return Vec::new();
    }
    let mut by_condition = HashMap::<RegisterId, Vec<usize>>::default();
    let mut false_consumers = HashMap::<RegisterId, Vec<usize>>::default();
    for (index, mux) in all_muxes.iter().enumerate() {
        by_condition.entry(mux.cond).or_default().push(index);
        false_consumers
            .entry(mux.false_val)
            .or_default()
            .push(index);
    }
    let mut condition_groups = by_condition.into_iter().collect::<Vec<_>>();
    condition_groups.sort_unstable_by_key(|(cond, muxes)| {
        (
            muxes
                .first()
                .map(|&index| all_muxes[index].mux_idx)
                .unwrap_or(usize::MAX),
            cond.0,
        )
    });

    let mut plans = Vec::new();
    let mut start_index = 0usize;
    let mut removed = HashSet::default();
    let mut params = block.params.iter().copied().collect::<HashSet<_>>();
    while let Some(plan) = find_coupled_state_update_in_source_block(
        eu,
        block,
        block_id,
        &all_muxes,
        &condition_groups,
        &false_consumers,
        &def_pos,
        use_counts,
        start_index,
        &removed,
        &params,
    ) {
        start_index = plan.first_mux_idx + 1;
        params = plan.muxes.iter().map(|mux| mux.dst).collect();
        removed.extend(plan.muxes.iter().map(|mux| mux.mux_idx));
        removed.extend(plan.hoisted_defs.iter().copied());
        removed.extend(
            plan.short_circuit
                .iter()
                .flat_map(|short| short.removed_defs.iter().copied()),
        );
        plans.push(plan);
    }
    plans
}

#[allow(clippy::too_many_arguments)]
fn find_coupled_state_update_in_source_block(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    block_id: BlockId,
    all_muxes: &[PriorityChainMux],
    condition_groups: &[(RegisterId, Vec<usize>)],
    false_consumers: &HashMap<RegisterId, Vec<usize>>,
    def_pos: &HashMap<RegisterId, usize>,
    use_counts: &HashMap<RegisterId, usize>,
    start_index: usize,
    removed: &HashSet<usize>,
    params: &HashSet<RegisterId>,
) -> Option<CoupledStateUpdatePlan> {
    for &(cond, ref group_indices) in condition_groups {
        let group = group_indices
            .iter()
            .map(|&index| &all_muxes[index])
            .filter(|mux| mux.mux_idx >= start_index && !removed.contains(&mux.mux_idx))
            .collect::<Vec<_>>();
        if group.len() < 2 {
            continue;
        }

        // A non-final update is recognized by at least two state components
        // flowing to Muxes controlled by the same next predicate. After an
        // update has been recovered, its merge parameters identify the final
        // update in the sequence as well.
        let mut successor_links = HashMap::<RegisterId, HashSet<RegisterId>>::default();
        for mux in &group {
            for &consumer_index in false_consumers.get(&mux.dst).into_iter().flatten() {
                let consumer = &all_muxes[consumer_index];
                if consumer.mux_idx < start_index
                    || removed.contains(&consumer.mux_idx)
                    || consumer.mux_idx <= mux.mux_idx
                    || consumer.cond == cond
                {
                    continue;
                }
                successor_links
                    .entry(consumer.cond)
                    .or_default()
                    .insert(mux.dst);
            }
        }
        let successor = successor_links
            .into_iter()
            .filter(|(_, outputs)| outputs.len() >= 2)
            .max_by_key(|(next_cond, outputs)| (outputs.len(), Reverse(next_cond.0)));
        let mut selected = if let Some((_, outputs)) = successor {
            group
                .iter()
                .filter(|mux| outputs.contains(&mux.dst))
                .map(|mux| (*mux).clone())
                .collect::<Vec<_>>()
        } else {
            group
                .iter()
                .filter(|mux| params.contains(&mux.false_val))
                .map(|mux| (*mux).clone())
                .collect::<Vec<_>>()
        };
        if selected.len() < 2 {
            continue;
        }
        selected.sort_unstable_by_key(|mux| mux.mux_idx);
        let first_mux_idx = selected[0].mux_idx;
        let selected_outputs = selected.iter().map(|mux| mux.dst).collect::<HashSet<_>>();
        let selected_locations = selected
            .iter()
            .map(|mux| mux.mux_idx)
            .collect::<HashSet<_>>();
        let mut hoisted_defs = HashSet::default();
        let mut visiting = HashSet::default();
        let available = selected.iter().all(|mux| {
            [mux.true_val, mux.false_val].into_iter().all(|root| {
                collect_coupled_update_hoists(
                    block,
                    def_pos,
                    start_index,
                    removed,
                    first_mux_idx,
                    &selected_outputs,
                    &selected_locations,
                    root,
                    &mut visiting,
                    &mut hoisted_defs,
                )
            })
        });
        if !available {
            continue;
        }
        let mut hoisted_defs = hoisted_defs.into_iter().collect::<Vec<_>>();
        hoisted_defs.sort_unstable();
        let short_circuit = find_coupled_short_circuit(
            eu,
            block,
            def_pos,
            use_counts,
            start_index,
            removed,
            cond,
            &selected_locations,
        );

        return Some(CoupledStateUpdatePlan {
            block_id,
            first_mux_idx,
            cond,
            muxes: selected,
            hoisted_defs,
            short_circuit,
        });
    }
    None
}

fn find_coupled_short_circuit(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    def_pos: &HashMap<RegisterId, usize>,
    use_counts: &HashMap<RegisterId, usize>,
    start_index: usize,
    previously_removed: &HashSet<usize>,
    cond: RegisterId,
    selected_locations: &HashSet<usize>,
) -> Option<CoupledShortCircuit> {
    let mut current = cond;
    let mut removed_defs = Vec::new();
    let (guard, delayed) = loop {
        let &index = def_pos.get(&current)?;
        if index < start_index || previously_removed.contains(&index) {
            return None;
        }
        match &block.instructions[index] {
            SIRInstruction::Unary(
                dst,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) if *dst == current => {
                removed_defs.push(index);
                current = *source;
            }
            SIRInstruction::Unary(dst, crate::ir::UnaryOp::Or, source)
                if *dst == current
                    && eu
                        .register_map
                        .get(source)
                        .is_some_and(|register| register.width() == 1) =>
            {
                removed_defs.push(index);
                current = *source;
            }
            SIRInstruction::Binary(dst, lhs, crate::ir::BinaryOp::LogicAnd, rhs)
                if *dst == current =>
            {
                removed_defs.push(index);
                break (*lhs, *rhs);
            }
            _ => return None,
        }
    };

    let removed_locations = removed_defs.iter().copied().collect::<HashSet<_>>();
    let allowed_locations = removed_locations
        .iter()
        .chain(selected_locations)
        .copied()
        .collect::<HashSet<_>>();
    for &index in &removed_defs {
        let register = def_reg(&block.instructions[index])?;
        let allowed_uses = allowed_locations
            .iter()
            .map(|&use_index| {
                inst_uses(&block.instructions[use_index])
                    .into_iter()
                    .filter(|used| *used == register)
                    .count()
            })
            .sum::<usize>();
        if use_counts.get(&register).copied().unwrap_or(0) != allowed_uses {
            return None;
        }
    }
    removed_defs.sort_unstable();
    Some(CoupledShortCircuit {
        guard,
        delayed,
        removed_defs,
    })
}

#[allow(clippy::too_many_arguments)]
fn collect_coupled_update_hoists(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    def_pos: &HashMap<RegisterId, usize>,
    start_index: usize,
    previously_removed: &HashSet<usize>,
    first_mux_idx: usize,
    selected_outputs: &HashSet<RegisterId>,
    selected_locations: &HashSet<usize>,
    register: RegisterId,
    visiting: &mut HashSet<RegisterId>,
    hoisted_defs: &mut HashSet<usize>,
) -> bool {
    if selected_outputs.contains(&register) {
        return false;
    }
    let Some(&index) = def_pos.get(&register) else {
        return true;
    };
    if index < start_index || previously_removed.contains(&index) {
        return true;
    }
    if index < first_mux_idx || hoisted_defs.contains(&index) {
        return true;
    }
    if selected_locations.contains(&index) || !visiting.insert(register) {
        return false;
    }
    let instruction = &block.instructions[index];
    let movable = matches!(
        instruction,
        SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
    );
    let valid = movable
        && inst_uses(instruction).into_iter().all(|operand| {
            collect_coupled_update_hoists(
                block,
                def_pos,
                start_index,
                previously_removed,
                first_mux_idx,
                selected_outputs,
                selected_locations,
                operand,
                visiting,
                hoisted_defs,
            )
        });
    visiting.remove(&register);
    if valid {
        hoisted_defs.insert(index);
    }
    valid
}

fn apply_coupled_state_update_batch(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plans: Vec<CoupledStateUpdatePlan>,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) {
    let source_block_id = plans[0].block_id;
    let original = eu
        .blocks
        .remove(&source_block_id)
        .expect("coupled state-update block must exist");
    let removed = plans
        .iter()
        .flat_map(|plan| {
            plan.muxes
                .iter()
                .map(|mux| mux.mux_idx)
                .chain(plan.hoisted_defs.iter().copied())
                .chain(
                    plan.short_circuit
                        .iter()
                        .flat_map(|short| short.removed_defs.iter().copied()),
                )
        })
        .collect::<HashSet<_>>();
    let mut current_block_id = source_block_id;
    let mut current_params = original.params;
    let mut segment_start = 0usize;

    for plan in &plans {
        let guard_block_id = plan.short_circuit.as_ref().map(|_| BlockId(*next_block_id));
        let edge_base = *next_block_id + usize::from(guard_block_id.is_some());
        let true_block_id = BlockId(edge_base);
        let false_block_id = BlockId(edge_base + 1);
        let merge_block_id = BlockId(edge_base + 2);
        *next_block_id = merge_block_id.0 + 1;

        let mut head_instructions = original.instructions[segment_start..plan.first_mux_idx]
            .iter()
            .enumerate()
            .filter(|(relative_index, _)| !removed.contains(&(segment_start + relative_index)))
            .map(|(_, instruction)| instruction.clone())
            .collect::<Vec<_>>();
        head_instructions.extend(
            plan.hoisted_defs
                .iter()
                .map(|&index| original.instructions[index].clone()),
        );
        let head_cond = normalize_branch_condition(
            &mut eu.register_map,
            &mut head_instructions,
            plan.short_circuit
                .as_ref()
                .map_or(plan.cond, |short| short.guard),
            reg_counter,
        );
        eu.blocks.insert(
            current_block_id,
            BasicBlock {
                id: current_block_id,
                params: current_params,
                instructions: head_instructions,
                terminator: SIRTerminator::Branch {
                    cond: head_cond,
                    true_block: (guard_block_id.unwrap_or(true_block_id), Vec::new()),
                    false_block: (false_block_id, Vec::new()),
                },
            },
        );
        if let (Some(guard_block_id), Some(short_circuit)) =
            (guard_block_id, plan.short_circuit.as_ref())
        {
            let mut instructions = Vec::new();
            let delayed = normalize_branch_condition(
                &mut eu.register_map,
                &mut instructions,
                short_circuit.delayed,
                reg_counter,
            );
            eu.blocks.insert(
                guard_block_id,
                BasicBlock {
                    id: guard_block_id,
                    params: Vec::new(),
                    instructions,
                    terminator: SIRTerminator::Branch {
                        cond: delayed,
                        true_block: (true_block_id, Vec::new()),
                        false_block: (false_block_id, Vec::new()),
                    },
                },
            );
        }
        eu.blocks.insert(
            true_block_id,
            BasicBlock {
                id: true_block_id,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(
                    merge_block_id,
                    plan.muxes.iter().map(|mux| mux.true_val).collect(),
                ),
            },
        );
        eu.blocks.insert(
            false_block_id,
            BasicBlock {
                id: false_block_id,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(
                    merge_block_id,
                    plan.muxes.iter().map(|mux| mux.false_val).collect(),
                ),
            },
        );
        current_block_id = merge_block_id;
        current_params = plan.muxes.iter().map(|mux| mux.dst).collect();
        segment_start = plan.first_mux_idx + 1;
    }

    eu.blocks.insert(
        current_block_id,
        BasicBlock {
            id: current_block_id,
            params: current_params,
            instructions: original
                .instructions
                .into_iter()
                .enumerate()
                .skip(segment_start)
                .filter(|(index, _)| !removed.contains(index))
                .map(|(_, instruction)| instruction)
                .collect(),
            terminator: original.terminator,
        },
    );
    debug_assert_eq!(eu.verify_result(), Ok(()));
}

fn find_cross_block_priority_chain_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    use_counts: &HashMap<RegisterId, usize>,
) -> Option<CrossBlockPriorityChainPlan> {
    let cfg = SirCfg::analyze_forward_structure(eu).ok()?;
    let locations = instruction_def_locations(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);

    for block_id in block_ids {
        let block = &eu.blocks[&block_id];
        for first_mux_idx in 0..block.instructions.len() {
            let SIRInstruction::Mux(dst, cond, true_val, false_val) =
                &block.instructions[first_mux_idx]
            else {
                continue;
            };
            let mut muxes = vec![PriorityChainMux {
                mux_idx: first_mux_idx,
                dst: *dst,
                cond: *cond,
                true_val: *true_val,
                false_val: *false_val,
            }];
            while let Some(index) = first_mux_idx.checked_add(muxes.len()) {
                let Some(SIRInstruction::Mux(dst, cond, true_val, false_val)) =
                    block.instructions.get(index)
                else {
                    break;
                };
                if *false_val != muxes.last().expect("chain has a first mux").dst {
                    break;
                }
                muxes.push(PriorityChainMux {
                    mux_idx: index,
                    dst: *dst,
                    cond: *cond,
                    true_val: *true_val,
                    false_val: *false_val,
                });
            }
            if muxes.len() < 2
                || muxes
                    .iter()
                    .take(muxes.len() - 1)
                    .any(|mux| use_counts.get(&mux.dst).copied().unwrap_or(0) != 1)
            {
                continue;
            }

            let mut condition_defs = Vec::with_capacity(muxes.len());
            let mut moved_locations = HashSet::default();
            let mut valid = true;
            for mux in &muxes {
                let mut seen = HashSet::default();
                let Some(defs) = collect_cross_arm_defs(
                    eu,
                    &cfg,
                    use_counts,
                    &locations,
                    block_id,
                    first_mux_idx,
                    mux.cond,
                    true,
                    &mut seen,
                ) else {
                    valid = false;
                    break;
                };
                // Moving only the cross-block prefix of a condition DAG is
                // not a closed rewrite.  If the root (or an intermediate)
                // remains in the Mux block, it still uses that prefix before
                // the newly created decision blocks execute.
                let defs = closed_cross_block_condition_slice(defs, block_id);
                for def in &defs {
                    if !moved_locations.insert((def.block, def.index)) {
                        valid = false;
                        break;
                    }
                }
                if !valid {
                    break;
                }
                condition_defs.push(defs);
            }
            if !valid || moved_locations.is_empty() || condition_defs.iter().all(Vec::is_empty) {
                continue;
            }

            // The old cross-block priority rewrite moved only condition DAGs.
            // It then passed every already-computed payload directly from a
            // decision block to the merge.  Recover the disjoint arm slices
            // while the Mux chain still records which case owns each value.
            // Single-use closure makes the total walk linear in the number of
            // collected def-use edges across all arms.
            let arm_roots =
                std::iter::once(muxes[0].false_val).chain(muxes.iter().map(|mux| mux.true_val));
            let mut arm_defs = Vec::with_capacity(muxes.len() + 1);
            for root in arm_roots {
                let mut seen = HashSet::default();
                let Some(defs) = collect_cross_arm_defs(
                    eu,
                    &cfg,
                    use_counts,
                    &locations,
                    block_id,
                    first_mux_idx,
                    root,
                    true,
                    &mut seen,
                ) else {
                    valid = false;
                    break;
                };
                for def in &defs {
                    if !moved_locations.insert((def.block, def.index)) {
                        valid = false;
                        break;
                    }
                }
                if !valid {
                    break;
                }
                arm_defs.push(defs);
            }
            if !valid || arm_defs.len() != muxes.len() + 1 {
                continue;
            }

            let head = block
                .instructions
                .iter()
                .enumerate()
                .take(first_mux_idx)
                .map(|(_, instruction)| instruction.clone())
                .collect::<Vec<_>>();
            let outer_condition_defs = condition_defs.last().expect("chain has an outer condition");
            if moved_defs_insertion_index(&head, outer_condition_defs).is_none() {
                continue;
            }

            let condition_cost = condition_defs
                .iter()
                .flatten()
                .map(|def| branchified_instruction_cost(&def.instruction, &eu.register_map))
                .sum::<u128>();
            let arm_costs = arm_defs
                .iter()
                .map(|defs| {
                    defs.iter()
                        .map(|def| branchified_instruction_cost(&def.instruction, &eu.register_map))
                        .sum::<u128>()
                })
                .collect::<Vec<_>>();
            let avoided_arm_cost = arm_costs
                .iter()
                .copied()
                .sum::<u128>()
                .saturating_sub(arm_costs.iter().copied().max().unwrap_or(0));
            let removed_mux_cost = muxes
                .iter()
                .map(|mux| {
                    branchified_instruction_cost(&block.instructions[mux.mux_idx], &eu.register_map)
                })
                .sum::<u128>();
            let suffix = block
                .instructions
                .iter()
                .skip(muxes.last().expect("chain has a first mux").mux_idx + 1)
                .cloned()
                .collect::<Vec<_>>();
            let live_through = block_live_ins(&suffix, &terminator_uses(&block.terminator));
            let chunks_for = |value: RegisterId| {
                eu.register_map
                    .get(&value)
                    .map(|register| register.width().div_ceil(64).max(1))
                    .unwrap_or(1) as u128
            };
            let live_through_cost = live_through
                .into_iter()
                .filter(|value| *value != muxes.last().expect("chain has a first mux").dst)
                .map(chunks_for)
                .sum::<u128>();
            let introduced_cost = (muxes.len() as u128)
                .saturating_mul(BRANCH_CONTROL_COST)
                .saturating_add(
                    chunks_for(muxes.last().expect("chain has a first mux").dst)
                        .saturating_mul(PHI_COPY_COST_PER_CHUNK),
                )
                .saturating_add(live_through_cost)
                // At most one selected payload leaf executes, hence at most
                // one additional arm-to-merge transfer is dynamic.
                .saturating_add(1);
            if condition_cost
                .saturating_add(removed_mux_cost)
                .saturating_add(avoided_arm_cost)
                <= introduced_cost
            {
                continue;
            }

            return Some(CrossBlockPriorityChainPlan {
                block_id,
                first_mux_idx,
                muxes,
                condition_defs,
                arm_defs,
            });
        }
    }
    None
}

fn apply_cross_block_priority_chain(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: CrossBlockPriorityChainPlan,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) {
    let mux_count = plan.muxes.len();
    let decision_ids = (0..plan.muxes.len() - 1)
        .map(|index| BlockId(*next_block_id + index))
        .collect::<Vec<_>>();
    let leaf_base = *next_block_id + decision_ids.len();
    let leaf_ids = (0..=mux_count)
        .map(|index| BlockId(leaf_base + index))
        .collect::<Vec<_>>();
    let merge_id = BlockId(leaf_base + mux_count + 1);
    *next_block_id = merge_id.0 + 1;

    let removed_locations = plan
        .condition_defs
        .iter()
        .flatten()
        .chain(plan.arm_defs.iter().flatten())
        .map(located_instruction_key)
        .chain(plan.muxes.iter().map(|mux| (plan.block_id, mux.mux_idx)))
        .collect::<HashSet<_>>();
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("priority chain target block must exist");
    remove_instructions_at_locations(eu, &removed_locations, plan.block_id);

    let outer_index = plan.muxes.len() - 1;
    let outer = &plan.muxes[outer_index];
    let mut head_insts = original
        .instructions
        .iter()
        .enumerate()
        .take(plan.first_mux_idx)
        .filter(|(index, _)| !removed_locations.contains(&(plan.block_id, *index)))
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let insertion = moved_defs_insertion_index(&head_insts, &plan.condition_defs[outer_index])
        .expect("priority-chain condition definitions must have an SSA insertion point");
    head_insts.splice(
        insertion..insertion,
        plan.condition_defs[outer_index]
            .iter()
            .map(|def| def.instruction.clone()),
    );
    let head_cond = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_insts,
        outer.cond,
        reg_counter,
    );
    let head_false = if outer_index == 0 {
        (leaf_ids[0], Vec::new())
    } else {
        (decision_ids[outer_index - 1], Vec::new())
    };
    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_insts,
        terminator: SIRTerminator::Branch {
            cond: head_cond,
            true_block: (leaf_ids[outer_index + 1], Vec::new()),
            false_block: head_false,
        },
    };
    eu.blocks.insert(plan.block_id, head);

    for index in (0..outer_index).rev() {
        let mux = &plan.muxes[index];
        let mut instructions = plan.condition_defs[index]
            .iter()
            .map(|def| def.instruction.clone())
            .collect::<Vec<_>>();
        let cond = normalize_branch_condition(
            &mut eu.register_map,
            &mut instructions,
            mux.cond,
            reg_counter,
        );
        let false_target = if index == 0 {
            (leaf_ids[0], Vec::new())
        } else {
            (decision_ids[index - 1], Vec::new())
        };
        let true_target = (leaf_ids[index + 1], Vec::new());
        eu.blocks.insert(
            decision_ids[index],
            BasicBlock {
                id: decision_ids[index],
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Branch {
                    cond,
                    true_block: true_target,
                    false_block: false_target,
                },
            },
        );
    }

    for (arm, leaf_id) in leaf_ids.into_iter().enumerate() {
        let value = if arm == 0 {
            plan.muxes[0].false_val
        } else {
            plan.muxes[arm - 1].true_val
        };
        eu.blocks.insert(
            leaf_id,
            BasicBlock {
                id: leaf_id,
                params: Vec::new(),
                instructions: plan.arm_defs[arm]
                    .iter()
                    .map(|def| def.instruction.clone())
                    .collect(),
                terminator: SIRTerminator::Jump(merge_id, vec![value]),
            },
        );
    }

    let suffix = original
        .instructions
        .iter()
        .enumerate()
        .skip(plan.muxes.last().expect("chain has a first mux").mux_idx + 1)
        .filter(|(index, _)| !removed_locations.contains(&(plan.block_id, *index)))
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    eu.blocks.insert(
        merge_id,
        BasicBlock {
            id: merge_id,
            params: vec![outer.dst],
            instructions: suffix,
            terminator: original.terminator,
        },
    );
}

fn find_cross_block_branchify_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    use_counts: &HashMap<RegisterId, usize>,
) -> Option<CrossBlockBranchifyPlan> {
    let cfg = SirCfg::analyze_forward_structure(eu).ok()?;
    let def_locations = instruction_def_locations(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);

    for block_id in block_ids {
        let block = &eu.blocks[&block_id];
        for (mux_idx, inst) in block.instructions.iter().enumerate() {
            let SIRInstruction::Mux(dst, cond, true_val, false_val) = inst else {
                continue;
            };
            let mut condition_seen = HashSet::default();
            let Some(condition_defs) = collect_cross_arm_defs(
                eu,
                &cfg,
                use_counts,
                &def_locations,
                block_id,
                mux_idx,
                *cond,
                true,
                &mut condition_seen,
            ) else {
                continue;
            };
            // Do not sever a cross-block producer from a condition node that
            // remains in the Mux block.  Such a prefix is not independently
            // movable: the local node still executes before the new branch.
            let condition_defs = closed_cross_block_condition_slice(condition_defs, block_id);
            if moved_defs_insertion_index(&block.instructions[..mux_idx], &condition_defs).is_none()
            {
                continue;
            }
            let mut true_seen = HashSet::default();
            let mut false_seen = HashSet::default();
            let Some(true_defs) = collect_cross_arm_defs(
                eu,
                &cfg,
                use_counts,
                &def_locations,
                block_id,
                mux_idx,
                *true_val,
                true,
                &mut true_seen,
            ) else {
                continue;
            };
            let Some(false_defs) = collect_cross_arm_defs(
                eu,
                &cfg,
                use_counts,
                &def_locations,
                block_id,
                mux_idx,
                *false_val,
                true,
                &mut false_seen,
            ) else {
                continue;
            };
            if condition_defs.is_empty() && true_defs.is_empty() && false_defs.is_empty() {
                continue;
            }
            let condition_locations = condition_defs
                .iter()
                .map(|def| (def.block, def.index))
                .collect::<HashSet<_>>();
            let true_locations = true_defs
                .iter()
                .map(|def| (def.block, def.index))
                .collect::<HashSet<_>>();
            let false_locations = false_defs
                .iter()
                .map(|def| (def.block, def.index))
                .collect::<HashSet<_>>();
            if condition_locations
                .intersection(&true_locations)
                .next()
                .is_some()
                || condition_locations
                    .intersection(&false_locations)
                    .next()
                    .is_some()
            {
                continue;
            }
            if false_defs
                .iter()
                .any(|def| true_locations.contains(&(def.block, def.index)))
            {
                continue;
            }
            if !condition_defs
                .iter()
                .chain(true_defs.iter())
                .chain(false_defs.iter())
                .any(|def| def.block != block_id)
            {
                // The existing block-local planner has a more precise memory
                // and live-through model for this case.
                continue;
            }

            let plan = CrossBlockBranchifyPlan {
                block_id,
                mux_idx,
                dst: *dst,
                cond: *cond,
                condition_defs,
                true_val: *true_val,
                false_val: *false_val,
                true_defs,
                false_defs,
            };
            if cross_block_branch_is_profitable(eu, &plan) {
                return Some(plan);
            }
        }
    }
    None
}

/// Return the only insertion point that keeps a moved condition DAG in SSA
/// order.  Its external operands must already be defined in the target head,
/// while every use of a moved result must remain after the inserted DAG.
fn moved_defs_insertion_index(
    head: &[SIRInstruction<RegionedAbsoluteAddr>],
    moved: &[LocatedInstruction],
) -> Option<usize> {
    if moved.is_empty() {
        return Some(head.len());
    }
    let moved_registers = moved
        .iter()
        .filter_map(|def| def_reg(&def.instruction))
        .collect::<HashSet<_>>();
    if moved_registers.len() != moved.len() {
        return None;
    }

    let first_use = head
        .iter()
        .position(|instruction| {
            inst_uses(instruction)
                .iter()
                .any(|reg| moved_registers.contains(reg))
        })
        .unwrap_or(head.len());
    let mut insertion = 0usize;
    for definition in moved {
        for operand in inst_uses(&definition.instruction) {
            if moved_registers.contains(&operand) {
                continue;
            }
            if let Some(index) = head
                .iter()
                .position(|instruction| def_reg(instruction) == Some(operand))
            {
                insertion = insertion.max(index + 1);
            }
        }
    }
    (insertion <= first_use).then_some(insertion)
}

fn closed_cross_block_condition_slice(
    definitions: Vec<LocatedInstruction>,
    mux_block: BlockId,
) -> Vec<LocatedInstruction> {
    if definitions
        .iter()
        .any(|definition| definition.block == mux_block)
    {
        Vec::new()
    } else {
        definitions
    }
}

/// Find a group of selects driven by the same predicate.  Treating each Mux
/// independently misses the important case where several selected values
/// share one arm DAG:
///
/// ```text
///   t = expensive(...)
///   a = Mux(p, t, a0)
///   b = Mux(p, t, b0)
/// ```
///
/// `t` has two uses, so a single-use walk rejects it even though it is safe to
/// compute it once in the true arm and pass both selected results through one
/// merge.  The group analysis below classifies all uses of the candidate DAG,
/// so a definition is moved only when every use is on the same arm or is
/// another Mux in this group.
fn find_cross_block_group_branchify_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> Option<CrossBlockGroupBranchifyPlan> {
    let cfg = SirCfg::analyze_forward_structure(eu).ok()?;
    let def_locations = instruction_def_locations(eu);
    let def_blocks = all_def_blocks(eu);
    let use_locations = register_use_locations(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);

    for block_id in block_ids {
        let block = &eu.blocks[&block_id];
        let mut groups = HashMap::<(RegisterId, bool), Vec<CrossGroupMux>>::default();
        for (mux_idx, inst) in block.instructions.iter().enumerate() {
            let SIRInstruction::Mux(dst, condition, true_val, false_val) = inst else {
                continue;
            };
            let (root, condition_inverted) = resolve_boolean_alias(eu, &def_locations, *condition);
            groups
                .entry((root, condition_inverted))
                .or_default()
                .push(CrossGroupMux {
                    mux_idx,
                    dst: *dst,
                    true_val: *true_val,
                    false_val: *false_val,
                    condition_inverted,
                });
        }

        let mut groups = groups
            .into_iter()
            .filter(|(_, muxes)| muxes.len() >= 2)
            .collect::<Vec<_>>();
        groups.sort_unstable_by_key(|((root, inverted), muxes)| {
            (muxes[0].mux_idx, root.0, *inverted as u8)
        });

        for ((branch_cond, _), muxes) in groups {
            let first_mux_idx = muxes[0].mux_idx;
            if !cross_group_value_available(
                &cfg,
                &def_blocks,
                &def_locations,
                block_id,
                first_mux_idx,
                branch_cond,
                &HashSet::default(),
            ) {
                continue;
            }

            let true_roots = muxes.iter().map(|mux| mux.true_val).collect::<Vec<_>>();
            let false_roots = muxes.iter().map(|mux| mux.false_val).collect::<Vec<_>>();
            let true_all = collect_cross_group_defs(
                eu,
                &cfg,
                &def_locations,
                block_id,
                first_mux_idx,
                &true_roots,
            );
            let false_all = collect_cross_group_defs(
                eu,
                &cfg,
                &def_locations,
                block_id,
                first_mux_idx,
                &false_roots,
            );
            if true_all.is_empty() && false_all.is_empty() {
                continue;
            }

            let true_all_locations = instruction_locations(&true_all);
            let false_all_locations = instruction_locations(&false_all);
            let true_movable = filter_cross_group_defs(
                eu,
                block_id,
                &true_all,
                &false_all_locations,
                true,
                &muxes,
                &use_locations,
            );
            let false_movable = filter_cross_group_defs(
                eu,
                block_id,
                &false_all,
                &true_all_locations,
                false,
                &muxes,
                &use_locations,
            );
            if true_movable.is_empty() && false_movable.is_empty() {
                continue;
            }

            let true_defs = true_all
                .into_iter()
                .filter(|def| true_movable.contains(&located_instruction_key(def)))
                .collect::<Vec<_>>();
            let false_defs = false_all
                .into_iter()
                .filter(|def| false_movable.contains(&located_instruction_key(def)))
                .collect::<Vec<_>>();

            if muxes.iter().any(|mux| {
                !cross_group_value_available(
                    &cfg,
                    &def_blocks,
                    &def_locations,
                    block_id,
                    first_mux_idx,
                    mux.true_val,
                    &true_movable,
                ) || !cross_group_value_available(
                    &cfg,
                    &def_blocks,
                    &def_locations,
                    block_id,
                    first_mux_idx,
                    mux.false_val,
                    &false_movable,
                )
            }) {
                continue;
            }

            let plan = CrossBlockGroupBranchifyPlan {
                block_id,
                first_mux_idx,
                branch_cond,
                muxes,
                true_defs,
                false_defs,
            };
            if cross_group_branch_is_profitable(eu, &plan) {
                return Some(plan);
            }
        }
    }
    None
}

fn find_atomic_priority_placement(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    placement: &PlacementAnalysis,
) -> Option<AtomicPriorityPlacementPlan> {
    let locations = instruction_def_locations(eu);
    let use_locations = register_use_locations(eu);
    let mut candidates = Vec::<WholePriorityChainCandidate>::new();
    for &block_id in &placement.cfg.block_ids {
        let block = &eu.blocks[&block_id];
        let mut index = 0usize;
        while index < block.instructions.len() {
            let SIRInstruction::Mux(dst, cond, true_val, false_val) = &block.instructions[index]
            else {
                index += 1;
                continue;
            };
            let mut muxes = vec![PriorityChainMux {
                mux_idx: index,
                dst: *dst,
                cond: *cond,
                true_val: *true_val,
                false_val: *false_val,
            }];
            let mut next = index + 1;
            while let Some(SIRInstruction::Mux(dst, cond, true_val, false_val)) =
                block.instructions.get(next)
            {
                if *false_val != muxes.last().expect("chain has a first Mux").dst {
                    break;
                }
                muxes.push(PriorityChainMux {
                    mux_idx: next,
                    dst: *dst,
                    cond: *cond,
                    true_val: *true_val,
                    false_val: *false_val,
                });
                next += 1;
            }
            if muxes.len() >= 2
                && let Some(candidate) = build_whole_priority_chain_candidate(
                    eu,
                    placement,
                    &locations,
                    &use_locations,
                    block_id,
                    index,
                    muxes,
                )
            {
                candidates.push(candidate);
            }
            index = next;
        }
    }

    candidates.sort_unstable_by_key(|candidate| {
        (
            Reverse(candidate.depth),
            Reverse(candidate.benefit_scaled),
            candidate.plan.block_id,
            candidate.plan.first_mux_idx,
        )
    });

    let mut touched_blocks = HashSet::<BlockId>::default();
    let mut assigned_values = HashSet::<ValueId>::default();
    let mut regions = Vec::new();
    for candidate in candidates {
        if touched_blocks.contains(&candidate.plan.block_id)
            || !candidate.assigned_values.is_disjoint(&assigned_values)
        {
            continue;
        }
        touched_blocks.insert(candidate.plan.block_id);
        assigned_values.extend(candidate.assigned_values);
        regions.push(candidate.plan);
    }
    (!regions.is_empty()).then_some(AtomicPriorityPlacementPlan { regions })
}

/// Schedule movable SSA occurrences as late as the existing CFG permits.
///
/// The target of a producer depends on the eventual targets of its users, so
/// this is deliberately a whole-unit reverse-topological computation.  It is
/// not a block-local "all uses happen to be in one arm" scan.  Parameters and
/// observable instructions break the value DAG and remain fixed anchors.
fn find_existing_cfg_placement(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    placement: &PlacementAnalysis,
) -> Option<ExistingCfgPlacementPlan> {
    let mut instruction_values = HashMap::<(BlockId, usize), ValueId>::default();
    let mut candidates = BTreeSet::<ValueId>::new();
    for occurrence in &placement.values {
        let ValueOrigin::Instruction { block, index } = occurrence.origin else {
            continue;
        };
        let instruction = eu
            .blocks
            .get(&block)
            .and_then(|block| block.instructions.get(index))?;
        if def_reg(instruction) != Some(occurrence.register) {
            return None;
        }
        instruction_values.insert((block, index), occurrence.id);
        let movable = match occurrence.safety {
            ValueSafety::Pure => is_cross_block_sinkable_input(instruction),
            ValueSafety::StateRead(_) => matches!(instruction, SIRInstruction::Load(..)),
            ValueSafety::Pinned(_) => false,
        };
        if movable {
            candidates.insert(occurrence.id);
        }
    }

    // Producer -> user edges. Repeated operands are one dependency edge, not
    // a cycle or an inflated indegree.
    let mut indegree = candidates
        .iter()
        .copied()
        .map(|value| (value, 0usize))
        .collect::<HashMap<_, _>>();
    let mut users = HashMap::<ValueId, Vec<ValueId>>::default();
    for &user in &candidates {
        let dependencies = placement.values[user.0]
            .operands
            .iter()
            .copied()
            .filter(|operand| candidates.contains(operand))
            .collect::<BTreeSet<_>>();
        indegree.insert(user, dependencies.len());
        for dependency in dependencies {
            users.entry(dependency).or_default().push(user);
        }
    }
    for value_users in users.values_mut() {
        value_users.sort_unstable();
        value_users.dedup();
    }

    let mut ready = indegree
        .iter()
        .filter_map(|(&value, &degree)| (degree == 0).then_some(value))
        .collect::<BTreeSet<_>>();
    let mut topological = Vec::with_capacity(candidates.len());
    while let Some(&value) = ready.iter().next() {
        ready.remove(&value);
        topological.push(value);
        for &user in users.get(&value).into_iter().flatten() {
            let degree = indegree.get_mut(&user)?;
            *degree = degree.checked_sub(1)?;
            if *degree == 0 {
                ready.insert(user);
            }
        }
    }
    // Valid SIR SSA is acyclic after block parameters cut loop-carried value
    // edges. Refuse the complete plan if that invariant is not represented.
    if topological.len() != candidates.len() {
        return None;
    }

    let mut targets = HashMap::<ValueId, BlockId>::default();
    for &value in topological.iter().rev() {
        let occurrence = &placement.values[value.0];
        let origin = occurrence.origin.block();
        let use_blocks = occurrence.uses.iter().map(|site| match *site {
            ValueUse::Instruction { block, index, .. } => instruction_values
                .get(&(block, index))
                .and_then(|user| targets.get(user))
                .copied()
                .unwrap_or(block),
            ValueUse::BranchCondition { block } => block,
            ValueUse::EdgeArgument { predecessor, .. } => predecessor,
        });
        let target = placement
            .sink_bounds_for_use_blocks(value, use_blocks)
            .map(|bounds| bounds.latest)
            .filter(|&target| target != origin)
            .unwrap_or(origin);
        targets.insert(value, target);
    }

    let moved = candidates
        .iter()
        .copied()
        .filter(|value| {
            let origin = placement.values[value.0].origin.block();
            targets.get(value).is_some_and(|target| *target != origin)
        })
        .collect::<BTreeSet<_>>();
    if moved.is_empty() {
        return None;
    }

    // Profitability belongs to the connected move, not to a cheap Mux or
    // arithmetic node viewed in isolation.  Compare work skipped on the
    // untaken half of an existing branch with the worst-case increase in
    // values crossing the control boundary.  No new control transfer or phi
    // is introduced by this transform.
    let mut neighbors = HashMap::<ValueId, Vec<ValueId>>::default();
    for &user in &moved {
        for &operand in &placement.values[user.0].operands {
            if !moved.contains(&operand) {
                continue;
            }
            neighbors.entry(user).or_default().push(operand);
            neighbors.entry(operand).or_default().push(user);
        }
    }
    for adjacent in neighbors.values_mut() {
        adjacent.sort_unstable();
        adjacent.dedup();
    }

    let mut accepted = HashSet::<ValueId>::default();
    let mut unvisited = moved.clone();
    while let Some(&root) = unvisited.iter().next() {
        let mut component = BTreeSet::new();
        let mut worklist = VecDeque::from([root]);
        unvisited.remove(&root);
        while let Some(value) = worklist.pop_front() {
            component.insert(value);
            for &neighbor in neighbors.get(&value).into_iter().flatten() {
                if unvisited.remove(&neighbor) {
                    worklist.push_back(neighbor);
                }
            }
        }
        if existing_cfg_component_is_profitable(
            eu,
            placement,
            &instruction_values,
            &targets,
            &component,
        ) {
            accepted.extend(component);
        }
    }
    if accepted.is_empty() {
        return None;
    }

    let placements = topological
        .into_iter()
        .enumerate()
        .filter_map(|(topological_rank, value)| {
            if !accepted.contains(&value) {
                return None;
            }
            let occurrence = &placement.values[value.0];
            let ValueOrigin::Instruction {
                block: source_block,
                index: source_index,
            } = occurrence.origin
            else {
                return None;
            };
            let target_block = targets[&value];
            Some(ExistingCfgPlacedInstruction {
                value,
                source_block,
                source_index,
                target_block,
                topological_rank,
                instruction: eu.blocks[&source_block].instructions[source_index].clone(),
            })
        })
        .collect::<Vec<_>>();
    (!placements.is_empty()).then_some(ExistingCfgPlacementPlan { placements })
}

fn existing_cfg_component_is_profitable(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    placement: &PlacementAnalysis,
    instruction_values: &HashMap<(BlockId, usize), ValueId>,
    targets: &HashMap<ValueId, BlockId>,
    component: &BTreeSet<ValueId>,
) -> bool {
    let mut inputs = BTreeSet::<ValueId>::new();
    let mut outputs = BTreeSet::<ValueId>::new();
    let mut moved_cost = 0u128;

    for &value in component {
        let occurrence = &placement.values[value.0];
        let ValueOrigin::Instruction { block, index } = occurrence.origin else {
            return false;
        };
        let Some(instruction) = eu
            .blocks
            .get(&block)
            .and_then(|block| block.instructions.get(index))
        else {
            return false;
        };
        moved_cost =
            moved_cost.saturating_add(branchified_instruction_cost(instruction, &eu.register_map));
        inputs.extend(
            occurrence
                .operands
                .iter()
                .copied()
                .filter(|operand| !component.contains(operand)),
        );
        if occurrence.uses.iter().any(|site| match *site {
            ValueUse::Instruction { block, index, .. } => instruction_values
                .get(&(block, index))
                .is_none_or(|user| !component.contains(user)),
            ValueUse::BranchCondition { .. } | ValueUse::EdgeArgument { .. } => true,
        }) {
            outputs.insert(value);
        }
    }

    let chunks = |value: ValueId| {
        eu.register_map
            .get(&placement.values[value.0].register)
            .map(|register| register.width().div_ceil(64).max(1))
            .unwrap_or(1) as u128
    };
    let input_chunks = inputs.into_iter().map(chunks).sum::<u128>();
    let output_chunks = outputs.into_iter().map(chunks).sum::<u128>();
    let added_live_chunks = input_chunks.saturating_sub(output_chunks);

    // ScheduleLate into a post-dominator does not skip dynamic work. It is
    // still profitable when it replaces at least as much live output state as
    // the inputs it carries forward. This is the ordinary live-range case,
    // distinct from control-dependent sinking below; do not use instruction
    // cost as if the moved operation became conditional.
    let has_postdominating_move = component.iter().any(|value| {
        let origin = placement.values[value.0].origin.block();
        targets
            .get(value)
            .is_some_and(|&target| placement.cfg.postdominates(target, origin))
    });
    if has_postdominating_move {
        return input_chunks <= output_chunks;
    }

    // Scale by two: under the same profile-free even prior used by branch
    // selection, moving C units into one arm skips C/2 expected work, while a
    // newly live boundary chunk is charged on the complete path.
    moved_cost
        > added_live_chunks
            .saturating_mul(LIVE_THROUGH_COST_PER_CHUNK)
            .saturating_mul(2)
}

fn apply_existing_cfg_placement(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: ExistingCfgPlacementPlan,
) -> usize {
    let mut values = HashSet::<ValueId>::default();
    let mut definitions = HashSet::<RegisterId>::default();
    let mut source_locations = HashSet::<(BlockId, usize)>::default();
    for placed in &plan.placements {
        let Some(source) = eu.blocks.get(&placed.source_block) else {
            return 0;
        };
        if placed.source_block == placed.target_block
            || !eu.blocks.contains_key(&placed.target_block)
            || source.instructions.get(placed.source_index) != Some(&placed.instruction)
            || !values.insert(placed.value)
            || !source_locations.insert((placed.source_block, placed.source_index))
            || def_reg(&placed.instruction).is_none_or(|register| !definitions.insert(register))
        {
            return 0;
        }
    }

    let mut removals = HashMap::<BlockId, BTreeSet<usize>>::default();
    let mut insertions =
        HashMap::<BlockId, Vec<(usize, SIRInstruction<RegionedAbsoluteAddr>)>>::default();
    let mut touched = BTreeSet::<BlockId>::new();
    for placed in &plan.placements {
        removals
            .entry(placed.source_block)
            .or_default()
            .insert(placed.source_index);
        insertions
            .entry(placed.target_block)
            .or_default()
            .push((placed.topological_rank, placed.instruction.clone()));
        touched.insert(placed.source_block);
        touched.insert(placed.target_block);
    }

    // Construct every touched block from the preflighted snapshot first. No
    // partially rewritten CFG is observable if validation above fails.
    let mut replacements = touched
        .iter()
        .map(|&block| (block, eu.blocks[&block].clone()))
        .collect::<HashMap<_, _>>();
    for (block, indices) in removals {
        let replacement = replacements
            .get_mut(&block)
            .expect("preflighted source block must have a replacement");
        replacement.instructions = replacement
            .instructions
            .drain(..)
            .enumerate()
            .filter_map(|(index, instruction)| (!indices.contains(&index)).then_some(instruction))
            .collect();
    }
    for (block, mut instructions) in insertions {
        instructions.sort_unstable_by_key(|(rank, _)| *rank);
        replacements
            .get_mut(&block)
            .expect("preflighted target block must have a replacement")
            .instructions
            .splice(
                0..0,
                instructions.into_iter().map(|(_, instruction)| instruction),
            );
    }
    for block in touched {
        *eu.blocks
            .get_mut(&block)
            .expect("preflighted touched block must remain present") = replacements
            .remove(&block)
            .expect("preflighted touched block must have a replacement");
    }
    debug_assert_eq!(eu.verify_result(), Ok(()));
    plan.placements.len()
}

#[allow(clippy::too_many_arguments)]
fn mark_priority_dependency(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    placement: &PlacementAnalysis,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    block_id: BlockId,
    first_mux_idx: usize,
    leaf_count: usize,
    register: RegisterId,
    leaf: usize,
    masks: &mut HashMap<(BlockId, usize), Vec<bool>>,
    seen: &mut HashSet<(RegisterId, usize)>,
) {
    if !seen.insert((register, leaf)) {
        return;
    }
    let Some(&(definition_block, index)) = locations.get(&register) else {
        return;
    };
    if definition_block == block_id && index >= first_mux_idx {
        return;
    }
    let instruction = &eu.blocks[&definition_block].instructions[index];
    let Some(value) = placement.value_for_register(register) else {
        return;
    };
    let sinkable = match placement.value(value).map(|value| value.safety) {
        Some(ValueSafety::Pure) => is_cross_block_sinkable_input(instruction),
        Some(ValueSafety::StateRead(_)) => matches!(instruction, SIRInstruction::Load(..)),
        Some(ValueSafety::Pinned(_)) | None => false,
    };
    if !sinkable || !placement.can_sink_to_edge(value, block_id) {
        return;
    }

    masks
        .entry((definition_block, index))
        .or_insert_with(|| vec![false; leaf_count])[leaf] = true;
    for operand in inst_uses(instruction) {
        mark_priority_dependency(
            eu,
            placement,
            locations,
            block_id,
            first_mux_idx,
            leaf_count,
            operand,
            leaf,
            masks,
            seen,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn build_whole_priority_chain_candidate(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    placement: &PlacementAnalysis,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    use_locations: &HashMap<RegisterId, Vec<UseLocation>>,
    block_id: BlockId,
    first_mux_idx: usize,
    muxes: Vec<PriorityChainMux>,
) -> Option<WholePriorityChainCandidate> {
    for (index, mux) in muxes.iter().enumerate().take(muxes.len() - 1) {
        let uses = use_locations.get(&mux.dst)?;
        if uses.len() != 1
            || uses[0].block != block_id
            || uses[0].instruction != Some(muxes[index + 1].mux_idx)
        {
            return None;
        }
    }

    let chain_outputs = muxes.iter().map(|mux| mux.dst).collect::<HashSet<_>>();
    if muxes
        .iter()
        .any(|mux| chain_outputs.contains(&mux.cond) || chain_outputs.contains(&mux.true_val))
        || chain_outputs.contains(&muxes[0].false_val)
    {
        return None;
    }

    let leaf_count = muxes.len() + 1;
    let mut masks = HashMap::<(BlockId, usize), Vec<bool>>::default();
    let mut seen = HashSet::<(RegisterId, usize)>::default();
    mark_priority_dependency(
        eu,
        placement,
        locations,
        block_id,
        first_mux_idx,
        leaf_count,
        muxes[0].false_val,
        0,
        &mut masks,
        &mut seen,
    );
    for (index, mux) in muxes.iter().enumerate() {
        mark_priority_dependency(
            eu,
            placement,
            locations,
            block_id,
            first_mux_idx,
            leaf_count,
            mux.true_val,
            index + 1,
            &mut masks,
            &mut seen,
        );
        for leaf in 0..=index + 1 {
            mark_priority_dependency(
                eu,
                placement,
                locations,
                block_id,
                first_mux_idx,
                leaf_count,
                mux.cond,
                leaf,
                &mut masks,
                &mut seen,
            );
        }
    }

    let chain_locations = muxes
        .iter()
        .map(|mux| (block_id, mux.mux_idx))
        .collect::<HashSet<_>>();
    let mut movable = masks
        .iter()
        .filter(|(_, mask)| !mask.iter().all(|needed| *needed))
        .map(|(&location, _)| location)
        .collect::<HashSet<_>>();
    loop {
        let remove = movable
            .iter()
            .copied()
            .filter(|&(definition_block, index)| {
                let register = def_reg(&eu.blocks[&definition_block].instructions[index])
                    .expect("priority placement input defines a value");
                use_locations
                    .get(&register)
                    .into_iter()
                    .flatten()
                    .any(|location| {
                        location.instruction.is_none_or(|user| {
                            let user = (location.block, user);
                            !movable.contains(&user) && !chain_locations.contains(&user)
                        })
                    })
            })
            .collect::<Vec<_>>();
        if remove.is_empty() {
            break;
        }
        for location in remove {
            movable.remove(&location);
        }
    }
    if movable.is_empty() {
        return None;
    }

    let placed = order_priority_placements(eu, placement, locations, &masks, &movable)?;
    let plan = WholePriorityChainPlan {
        block_id,
        first_mux_idx,
        muxes,
        placed,
    };
    let benefit_scaled = whole_priority_chain_benefit(eu, &plan)?;
    let assigned_values = plan
        .placed
        .iter()
        .filter_map(|placed| def_reg(&placed.instruction))
        .filter_map(|register| placement.value_for_register(register))
        .chain(
            plan.muxes
                .iter()
                .filter_map(|mux| placement.value_for_register(mux.dst)),
        )
        .collect::<HashSet<_>>();
    Some(WholePriorityChainCandidate {
        depth: dominator_depth(&placement.cfg, block_id),
        plan,
        benefit_scaled,
        assigned_values,
    })
}

fn order_priority_placements(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    placement: &PlacementAnalysis,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    masks: &HashMap<(BlockId, usize), Vec<bool>>,
    movable: &HashSet<(BlockId, usize)>,
) -> Option<Vec<PriorityPlacedInstruction>> {
    let mut indegree = movable
        .iter()
        .copied()
        .map(|location| (location, 0usize))
        .collect::<HashMap<_, _>>();
    let mut users = HashMap::<(BlockId, usize), Vec<(BlockId, usize)>>::default();
    for &(block, index) in movable {
        let mut dependencies = HashSet::default();
        for operand in inst_uses(&eu.blocks[&block].instructions[index]) {
            let Some(&dependency) = locations.get(&operand) else {
                continue;
            };
            if movable.contains(&dependency) && dependencies.insert(dependency) {
                *indegree.get_mut(&(block, index))? += 1;
                users.entry(dependency).or_default().push((block, index));
            }
        }
    }
    for dependents in users.values_mut() {
        dependents.sort_unstable();
        dependents.dedup();
    }

    let order_key =
        |(block, index): (BlockId, usize)| Some((placement.cfg.block_index(block)?, index));
    let mut ready = BTreeSet::<(usize, usize)>::new();
    for (&location, &degree) in &indegree {
        if degree == 0 {
            ready.insert(order_key(location)?);
        }
    }

    let mut ordered = Vec::with_capacity(movable.len());
    while let Some(key) = ready.first().copied() {
        ready.remove(&key);
        let location = (placement.cfg.block_ids[key.0], key.1);
        ordered.push(location);
        for &user in users.get(&location).into_iter().flatten() {
            let degree = indegree.get_mut(&user)?;
            *degree = degree.checked_sub(1)?;
            if *degree == 0 {
                ready.insert(order_key(user)?);
            }
        }
    }
    if ordered.len() != movable.len() {
        return None;
    }

    ordered
        .into_iter()
        .map(|(block, index)| {
            let leaves = masks[&(block, index)]
                .iter()
                .enumerate()
                .filter_map(|(leaf, needed)| needed.then_some(leaf))
                .collect::<Vec<_>>();
            let site = if leaves.len() == 1 {
                PriorityPlacementSite::Leaf(leaves[0])
            } else {
                PriorityPlacementSite::Decision(
                    leaves.iter().copied().max().expect("non-empty use mask") - 1,
                )
            };
            Some(PriorityPlacedInstruction {
                block,
                index,
                site,
                instruction: eu.blocks.get(&block)?.instructions.get(index)?.clone(),
            })
        })
        .collect()
}

fn whole_priority_chain_benefit(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    plan: &WholePriorityChainPlan,
) -> Option<u128> {
    const SCALE: u128 = 1 << 32;
    let block = &eu.blocks[&plan.block_id];
    let def_pos = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| def_reg(instruction).map(|register| (register, index)))
        .collect::<HashMap<_, _>>();
    let probabilities = plan
        .muxes
        .iter()
        .map(|mux| static_true_probability(block, &def_pos, mux.cond))
        .collect::<Vec<_>>();
    let mut decision_weights = vec![0u128; plan.muxes.len()];
    let mut leaf_weights = vec![0u128; plan.muxes.len() + 1];
    let mut reach = SCALE;
    for index in (0..plan.muxes.len()).rev() {
        let probability = probabilities[index];
        decision_weights[index] = reach;
        leaf_weights[index + 1] =
            reach.saturating_mul(probability.true_weight) / probability.total_weight;
        reach = reach.saturating_mul(probability.total_weight - probability.true_weight)
            / probability.total_weight;
    }
    leaf_weights[0] = reach;

    let instruction_cost = |instruction: &SIRInstruction<RegionedAbsoluteAddr>| {
        branchified_instruction_cost(instruction, &eu.register_map)
    };
    let original_placed_cost = plan
        .placed
        .iter()
        .map(|placed| instruction_cost(&placed.instruction))
        .sum::<u128>()
        .saturating_mul(SCALE);
    let new_placed_cost = plan
        .placed
        .iter()
        .map(|placed| {
            let weight = match placed.site {
                PriorityPlacementSite::Decision(index) => decision_weights[index],
                PriorityPlacementSite::Leaf(index) => leaf_weights[index],
            };
            instruction_cost(&placed.instruction).saturating_mul(weight)
        })
        .sum::<u128>();
    let removed_mux_cost = plan
        .muxes
        .iter()
        .map(|mux| instruction_cost(&block.instructions[mux.mux_idx]))
        .sum::<u128>()
        .saturating_mul(SCALE);

    let mut introduced = BRANCH_CONTROL_COST.saturating_mul(SCALE);
    for (index, probability) in probabilities.iter().copied().enumerate() {
        let reach = decision_weights[index];
        introduced = introduced
            .saturating_add(BRANCH_CONTROL_COST.saturating_mul(reach))
            .saturating_add(
                MISPREDICT_COST.saturating_mul(reach).saturating_mul(
                    probability
                        .true_weight
                        .min(probability.total_weight - probability.true_weight),
                ) / probability.total_weight,
            );
    }
    let outer = plan.muxes.last().expect("priority chain is non-empty");
    let chunks_for = |value: RegisterId| {
        eu.register_map
            .get(&value)
            .map(|register| register.width().div_ceil(64).max(1))
            .unwrap_or(1) as u128
    };
    introduced = introduced.saturating_add(
        chunks_for(outer.dst)
            .saturating_mul(PHI_COPY_COST_PER_CHUNK)
            .saturating_mul(SCALE),
    );
    // Values used by the suffix were already live from their definitions to
    // that suffix before this rewrite.  The priority region merely lies on
    // the same path; it does not introduce a new live range for those values.
    // The closed-placement proof above rejects any arm value with an external
    // suffix use, so the only new region output is the outer Mux result, whose
    // phi-copy cost is charged above.

    original_placed_cost
        .saturating_add(removed_mux_cost)
        .checked_sub(new_placed_cost.saturating_add(introduced))
        .filter(|benefit| *benefit != 0)
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

fn collect_cross_group_defs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mux_block: BlockId,
    first_mux_idx: usize,
    roots: &[RegisterId],
) -> Vec<LocatedInstruction> {
    fn visit(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        cfg: &SirCfg,
        locations: &HashMap<RegisterId, (BlockId, usize)>,
        mux_block: BlockId,
        first_mux_idx: usize,
        register: RegisterId,
        seen: &mut HashSet<(BlockId, usize)>,
        result: &mut Vec<LocatedInstruction>,
    ) {
        let Some(&(block, index)) = locations.get(&register) else {
            return;
        };
        if block == mux_block && index >= first_mux_idx {
            return;
        }
        if !cfg.dominates(block, mux_block) {
            return;
        }
        let instruction = eu.blocks[&block].instructions[index].clone();
        if !is_cross_block_sinkable_input(&instruction) || !seen.insert((block, index)) {
            return;
        }
        for operand in inst_uses(&instruction) {
            visit(
                eu,
                cfg,
                locations,
                mux_block,
                first_mux_idx,
                operand,
                seen,
                result,
            );
        }
        result.push(LocatedInstruction {
            block,
            index,
            instruction,
        });
    }

    let mut seen = HashSet::default();
    let mut result = Vec::new();
    for &root in roots {
        visit(
            eu,
            cfg,
            locations,
            mux_block,
            first_mux_idx,
            root,
            &mut seen,
            &mut result,
        );
    }
    result
}

fn filter_cross_group_defs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    mux_block: BlockId,
    candidates: &[LocatedInstruction],
    other_side: &HashSet<(BlockId, usize)>,
    true_side: bool,
    muxes: &[CrossGroupMux],
    use_locations: &HashMap<RegisterId, Vec<UseLocation>>,
) -> HashSet<(BlockId, usize)> {
    let mut movable = candidates
        .iter()
        .map(located_instruction_key)
        .filter(|location| !other_side.contains(location))
        .collect::<HashSet<_>>();

    loop {
        let remove = movable
            .iter()
            .copied()
            .filter(|location| {
                let instruction = &eu.blocks[&location.0].instructions[location.1];
                let Some(definition) = def_reg(instruction) else {
                    return true;
                };
                use_locations
                    .get(&definition)
                    .into_iter()
                    .flatten()
                    .any(|use_location| {
                        if use_location
                            .instruction
                            .is_some_and(|index| movable.contains(&(use_location.block, index)))
                        {
                            return false;
                        }
                        let Some(index) = use_location.instruction else {
                            return true;
                        };
                        if use_location.block != mux_block {
                            return true;
                        }
                        let Some(_mux) = muxes.iter().find(|mux| mux.mux_idx == index) else {
                            return true;
                        };
                        let SIRInstruction::Mux(_, condition, true_val, false_val) =
                            &eu.blocks[&mux_block].instructions[index]
                        else {
                            return true;
                        };
                        if *condition == definition {
                            return true;
                        }
                        if true_side {
                            *true_val != definition || *false_val == definition
                        } else {
                            *false_val != definition || *true_val == definition
                        }
                    })
            })
            .collect::<Vec<_>>();
        if remove.is_empty() {
            break;
        }
        for location in remove {
            movable.remove(&location);
        }
    }
    movable
}

fn cross_group_value_available(
    cfg: &SirCfg,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    mux_block: BlockId,
    first_mux_idx: usize,
    register: RegisterId,
    moved: &HashSet<(BlockId, usize)>,
) -> bool {
    if let Some(&(block, index)) = def_locations.get(&register) {
        if moved.contains(&(block, index)) {
            return true;
        }
        return cfg.dominates(block, mux_block) && (block != mux_block || index < first_mux_idx);
    }
    def_blocks
        .get(&register)
        .is_some_and(|block| cfg.dominates(*block, mux_block))
}

fn cross_group_branch_is_profitable(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    plan: &CrossBlockGroupBranchifyPlan,
) -> bool {
    let block = &eu.blocks[&plan.block_id];
    let true_arm_cost = plan
        .true_defs
        .iter()
        .map(|def| branchified_instruction_cost(&def.instruction, &eu.register_map))
        .sum::<u128>();
    let false_arm_cost = plan
        .false_defs
        .iter()
        .map(|def| branchified_instruction_cost(&def.instruction, &eu.register_map))
        .sum::<u128>();
    let group_indices = plan
        .muxes
        .iter()
        .map(|mux| mux.mux_idx)
        .collect::<HashSet<_>>();
    let moved = plan
        .true_defs
        .iter()
        .chain(plan.false_defs.iter())
        .map(located_instruction_key)
        .collect::<HashSet<_>>();
    let suffix = block
        .instructions
        .iter()
        .enumerate()
        .skip(plan.first_mux_idx + 1)
        .filter(|(index, _)| {
            !group_indices.contains(index) && !moved.contains(&(plan.block_id, *index))
        })
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let mut live_through = block_live_ins(&suffix, &terminator_uses(&block.terminator));
    live_through.retain(|value| !plan.muxes.iter().any(|mux| mux.dst == *value));
    live_through.sort_unstable();
    live_through.dedup();
    let chunks_for = |value: RegisterId| {
        eu.register_map
            .get(&value)
            .map(|register| register.width().div_ceil(64).max(1))
            .unwrap_or(1) as u128
    };
    let phi_copy_cost = plan
        .muxes
        .iter()
        .map(|mux| chunks_for(mux.dst).saturating_mul(PHI_COPY_COST_PER_CHUNK))
        .sum::<u128>();
    let live_through_cost = live_through
        .into_iter()
        .map(chunks_for)
        .sum::<u128>()
        .saturating_mul(LIVE_THROUGH_COST_PER_CHUNK);
    let removed_mux_cost = plan
        .muxes
        .iter()
        .map(|mux| block.instructions[mux.mux_idx].clone())
        .map(|instruction| branchified_instruction_cost(&instruction, &eu.register_map))
        .sum::<u128>();
    BranchProfitability {
        true_arm_cost,
        false_arm_cost,
        removed_mux_cost,
        probability: StaticBranchProbability::EVEN,
        control_cost: BRANCH_CONTROL_COST,
        phi_copy_cost,
        live_through_cost,
    }
    .proves_expected_benefit()
}

/// Collect a closed, pure, single-use slice which can be delayed from a
/// dominating block until the Mux's branch arm. A non-movable operand is kept
/// as a live-in; SSA dominance guarantees that it is available at the Mux.
fn collect_cross_arm_defs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    use_counts: &HashMap<RegisterId, usize>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mux_block: BlockId,
    mux_idx: usize,
    root: RegisterId,
    root_required: bool,
    seen: &mut HashSet<(BlockId, usize)>,
) -> Option<Vec<LocatedInstruction>> {
    let Some(&(block_id, index)) = locations.get(&root) else {
        return root_required.then(Vec::new);
    };
    if block_id == mux_block && index >= mux_idx {
        return None;
    }
    if !cfg.dominates(block_id, mux_block) {
        return None;
    }
    if use_counts.get(&root).copied().unwrap_or(0) != 1 {
        return root_required.then(Vec::new);
    }
    let instruction = eu.blocks[&block_id].instructions[index].clone();
    if !is_cross_block_sinkable_input(&instruction) {
        return root_required.then(Vec::new);
    }
    if !seen.insert((block_id, index)) {
        return Some(Vec::new());
    }

    let mut result = Vec::new();
    for operand in inst_uses(&instruction) {
        let can_attempt_move =
            locations
                .get(&operand)
                .is_some_and(|&(operand_block, operand_idx)| {
                    (operand_block != mux_block || operand_idx < mux_idx)
                        && cfg.dominates(operand_block, mux_block)
                });
        if can_attempt_move
            && use_counts.get(&operand).copied().unwrap_or(0) == 1
            && let Some(operand_defs) = collect_cross_arm_defs(
                eu, cfg, use_counts, locations, mux_block, mux_idx, operand, false, seen,
            )
        {
            result.extend(operand_defs);
        }
    }
    result.push(LocatedInstruction {
        block: block_id,
        index,
        instruction,
    });
    Some(result)
}

fn is_cross_block_sinkable_input(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        inst,
        SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..)
    )
}

fn cross_block_branch_is_profitable(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    plan: &CrossBlockBranchifyPlan,
) -> bool {
    let block = &eu.blocks[&plan.block_id];
    let true_arm_cost = plan
        .true_defs
        .iter()
        .map(|def| branchified_instruction_cost(&def.instruction, &eu.register_map))
        .sum::<u128>();
    let false_arm_cost = plan
        .false_defs
        .iter()
        .map(|def| branchified_instruction_cost(&def.instruction, &eu.register_map))
        .sum::<u128>();
    let suffix = block
        .instructions
        .iter()
        .skip(plan.mux_idx + 1)
        .cloned()
        .collect::<Vec<_>>();
    let mut live_through = block_live_ins(&suffix, &terminator_uses(&block.terminator));
    live_through.retain(|value| *value != plan.dst);
    live_through.sort_unstable();
    live_through.dedup();
    let chunks_for = |value: RegisterId| {
        eu.register_map
            .get(&value)
            .map(|register| register.width().div_ceil(64).max(1))
            .unwrap_or(1) as u128
    };
    let phi_copy_cost = chunks_for(plan.dst).saturating_mul(PHI_COPY_COST_PER_CHUNK);
    let live_through_cost = live_through
        .into_iter()
        .map(chunks_for)
        .sum::<u128>()
        .saturating_mul(LIVE_THROUGH_COST_PER_CHUNK);
    BranchProfitability {
        true_arm_cost,
        false_arm_cost,
        removed_mux_cost: branchified_instruction_cost(
            &block.instructions[plan.mux_idx],
            &eu.register_map,
        ),
        probability: StaticBranchProbability::EVEN,
        control_cost: BRANCH_CONTROL_COST,
        phi_copy_cost,
        live_through_cost,
    }
    .proves_expected_benefit()
}

fn apply_atomic_priority_placement(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: AtomicPriorityPlacementPlan,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) -> usize {
    let mut additional_blocks = 0usize;
    for region in &plan.regions {
        let Some(target) = eu.blocks.get(&region.block_id) else {
            return 0;
        };
        if region
            .muxes
            .iter()
            .any(|mux| target.instructions.get(mux.mux_idx).and_then(def_reg) != Some(mux.dst))
            || region.placed.iter().any(|placed| {
                eu.blocks
                    .get(&placed.block)
                    .and_then(|block| block.instructions.get(placed.index))
                    .and_then(def_reg)
                    != def_reg(&placed.instruction)
            })
        {
            return 0;
        }
        let Some(region_blocks) = region
            .muxes
            .len()
            .checked_mul(2)
            .and_then(|blocks| blocks.checked_add(1))
        else {
            return 0;
        };
        let Some(total) = additional_blocks.checked_add(region_blocks) else {
            return 0;
        };
        additional_blocks = total;
    }
    let Some(reserved_end) = next_block_id.checked_add(additional_blocks) else {
        return 0;
    };

    let regions = plan.regions.len();
    for region in plan.regions {
        apply_whole_priority_chain(eu, region, next_block_id, reg_counter);
    }
    debug_assert_eq!(*next_block_id, reserved_end);
    regions
}

fn apply_whole_priority_chain(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: WholePriorityChainPlan,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) {
    let mux_count = plan.muxes.len();
    let base = *next_block_id;
    let decision_ids = (0..mux_count)
        .map(|index| {
            if index + 1 == mux_count {
                plan.block_id
            } else {
                BlockId(base + index)
            }
        })
        .collect::<Vec<_>>();
    let leaf_base = base + mux_count - 1;
    let leaf_ids = (0..=mux_count)
        .map(|index| BlockId(leaf_base + index))
        .collect::<Vec<_>>();
    let merge_id = BlockId(leaf_base + mux_count + 1);
    *next_block_id = merge_id.0 + 1;

    let moved_registers = plan
        .placed
        .iter()
        .filter_map(|placed| def_reg(&placed.instruction))
        .collect::<HashSet<_>>();
    for block in eu.blocks.values_mut() {
        if block.id != plan.block_id {
            block.instructions.retain(|instruction| {
                def_reg(instruction).is_none_or(|register| !moved_registers.contains(&register))
            });
        }
    }
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("whole priority target block must exist");
    let first_mux_position = original
        .instructions
        .iter()
        .position(|instruction| def_reg(instruction) == Some(plan.muxes[0].dst))
        .expect("whole priority first Mux must remain in its target block");
    let outer = plan
        .muxes
        .last()
        .expect("whole priority chain is non-empty");
    let outer_mux_position = original
        .instructions
        .iter()
        .position(|instruction| def_reg(instruction) == Some(outer.dst))
        .expect("whole priority outer Mux must remain in its target block");
    let removed_registers = moved_registers
        .iter()
        .copied()
        .chain(plan.muxes.iter().map(|mux| mux.dst))
        .collect::<HashSet<_>>();
    let placed_for = |site| {
        plan.placed
            .iter()
            .filter(|placed| placed.site == site)
            .map(|placed| placed.instruction.clone())
            .collect::<Vec<_>>()
    };

    for index in (0..mux_count).rev() {
        let mux = &plan.muxes[index];
        let mut instructions = if index + 1 == mux_count {
            let mut head = original
                .instructions
                .iter()
                .take(first_mux_position)
                .filter(|instruction| {
                    def_reg(instruction)
                        .is_none_or(|register| !removed_registers.contains(&register))
                })
                .cloned()
                .collect::<Vec<_>>();
            head.extend(placed_for(PriorityPlacementSite::Decision(index)));
            head
        } else {
            placed_for(PriorityPlacementSite::Decision(index))
        };
        let cond = normalize_branch_condition(
            &mut eu.register_map,
            &mut instructions,
            mux.cond,
            reg_counter,
        );
        let false_block = if index == 0 {
            leaf_ids[0]
        } else {
            decision_ids[index - 1]
        };
        eu.blocks.insert(
            decision_ids[index],
            BasicBlock {
                id: decision_ids[index],
                params: if index + 1 == mux_count {
                    original.params.clone()
                } else {
                    Vec::new()
                },
                instructions,
                terminator: SIRTerminator::Branch {
                    cond,
                    true_block: (leaf_ids[index + 1], Vec::new()),
                    false_block: (false_block, Vec::new()),
                },
            },
        );
    }

    for (leaf, &leaf_id) in leaf_ids.iter().enumerate() {
        let value = if leaf == 0 {
            plan.muxes[0].false_val
        } else {
            plan.muxes[leaf - 1].true_val
        };
        eu.blocks.insert(
            leaf_id,
            BasicBlock {
                id: leaf_id,
                params: Vec::new(),
                instructions: placed_for(PriorityPlacementSite::Leaf(leaf)),
                terminator: SIRTerminator::Jump(merge_id, vec![value]),
            },
        );
    }

    let suffix = original
        .instructions
        .iter()
        .skip(outer_mux_position + 1)
        .filter(|instruction| {
            def_reg(instruction).is_none_or(|register| !removed_registers.contains(&register))
        })
        .cloned()
        .collect::<Vec<_>>();
    eu.blocks.insert(
        merge_id,
        BasicBlock {
            id: merge_id,
            params: vec![outer.dst],
            instructions: suffix,
            terminator: original.terminator,
        },
    );
}

fn apply_cross_block_group_branchify(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: CrossBlockGroupBranchifyPlan,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) {
    let true_id = BlockId(*next_block_id);
    let false_id = BlockId(*next_block_id + 1);
    let merge_id = BlockId(*next_block_id + 2);
    *next_block_id += 3;

    let mux_indices = plan
        .muxes
        .iter()
        .map(|mux| mux.mux_idx)
        .collect::<HashSet<_>>();
    let removed_locations = plan
        .true_defs
        .iter()
        .chain(plan.false_defs.iter())
        .map(located_instruction_key)
        .chain(plan.muxes.iter().map(|mux| (plan.block_id, mux.mux_idx)))
        .collect::<HashSet<_>>();
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("cross-group branchify target block must exist");
    remove_instructions_at_locations(eu, &removed_locations, plan.block_id);

    let mut head_insts = original
        .instructions
        .iter()
        .enumerate()
        .take(plan.first_mux_idx)
        .filter(|(index, _)| !removed_locations.contains(&(plan.block_id, *index)))
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let branch_cond = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_insts,
        plan.branch_cond,
        reg_counter,
    );
    let suffix = original
        .instructions
        .iter()
        .enumerate()
        .skip(plan.first_mux_idx + 1)
        .filter(|(index, _)| {
            !mux_indices.contains(index) && !removed_locations.contains(&(plan.block_id, *index))
        })
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let true_insts = plan
        .true_defs
        .iter()
        .map(|def| def.instruction.clone())
        .collect::<Vec<_>>();
    let false_insts = plan
        .false_defs
        .iter()
        .map(|def| def.instruction.clone())
        .collect::<Vec<_>>();
    let true_args = plan
        .muxes
        .iter()
        .map(|mux| {
            if mux.condition_inverted {
                mux.false_val
            } else {
                mux.true_val
            }
        })
        .collect::<Vec<_>>();
    let false_args = plan
        .muxes
        .iter()
        .map(|mux| {
            if mux.condition_inverted {
                mux.true_val
            } else {
                mux.false_val
            }
        })
        .collect::<Vec<_>>();
    let merge_params = plan.muxes.iter().map(|mux| mux.dst).collect::<Vec<_>>();

    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_insts,
        terminator: SIRTerminator::Branch {
            cond: branch_cond,
            true_block: (true_id, Vec::new()),
            false_block: (false_id, Vec::new()),
        },
    };
    let true_block = BasicBlock {
        id: true_id,
        params: Vec::new(),
        instructions: true_insts,
        terminator: SIRTerminator::Jump(merge_id, true_args),
    };
    let false_block = BasicBlock {
        id: false_id,
        params: Vec::new(),
        instructions: false_insts,
        terminator: SIRTerminator::Jump(merge_id, false_args),
    };
    let merge_block = BasicBlock {
        id: merge_id,
        params: merge_params,
        instructions: suffix,
        terminator: original.terminator,
    };
    eu.blocks.insert(plan.block_id, head);
    eu.blocks.insert(true_id, true_block);
    eu.blocks.insert(false_id, false_block);
    eu.blocks.insert(merge_id, merge_block);
}

fn apply_cross_block_branchify(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: CrossBlockBranchifyPlan,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) {
    let true_id = BlockId(*next_block_id);
    let false_id = BlockId(*next_block_id + 1);
    let merge_id = BlockId(*next_block_id + 2);
    *next_block_id += 3;

    let removed_locations = plan
        .condition_defs
        .iter()
        .chain(plan.true_defs.iter())
        .chain(plan.false_defs.iter())
        .map(|def| (def.block, def.index))
        .collect::<HashSet<_>>();
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("cross-block branchify target block must exist");
    remove_instructions_at_locations(eu, &removed_locations, plan.block_id);

    let mut head_insts = original
        .instructions
        .iter()
        .enumerate()
        .take(plan.mux_idx)
        .filter(|(index, _)| !removed_locations.contains(&(plan.block_id, *index)))
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let insertion = moved_defs_insertion_index(&head_insts, &plan.condition_defs)
        .expect("cross-block condition definitions must have an SSA insertion point");
    head_insts.splice(
        insertion..insertion,
        plan.condition_defs
            .iter()
            .map(|def| def.instruction.clone()),
    );
    let branch_cond = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_insts,
        plan.cond,
        reg_counter,
    );
    let suffix = original
        .instructions
        .iter()
        .enumerate()
        .skip(plan.mux_idx + 1)
        .filter(|(index, _)| !removed_locations.contains(&(plan.block_id, *index)))
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let true_insts = plan
        .true_defs
        .iter()
        .map(|def| def.instruction.clone())
        .collect::<Vec<_>>();
    let false_insts = plan
        .false_defs
        .iter()
        .map(|def| def.instruction.clone())
        .collect::<Vec<_>>();

    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_insts,
        terminator: SIRTerminator::Branch {
            cond: branch_cond,
            true_block: (true_id, Vec::new()),
            false_block: (false_id, Vec::new()),
        },
    };
    let true_block = BasicBlock {
        id: true_id,
        params: Vec::new(),
        instructions: true_insts,
        terminator: SIRTerminator::Jump(merge_id, vec![plan.true_val]),
    };
    let false_block = BasicBlock {
        id: false_id,
        params: Vec::new(),
        instructions: false_insts,
        terminator: SIRTerminator::Jump(merge_id, vec![plan.false_val]),
    };
    let merge_block = BasicBlock {
        id: merge_id,
        params: vec![plan.dst],
        instructions: suffix,
        terminator: original.terminator,
    };
    eu.blocks.insert(plan.block_id, head);
    eu.blocks.insert(true_id, true_block);
    eu.blocks.insert(false_id, false_block);
    eu.blocks.insert(merge_id, merge_block);
}

fn remove_instructions_at_locations(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    removed: &HashSet<(BlockId, usize)>,
    removed_block: BlockId,
) {
    let mut affected = removed
        .iter()
        .map(|&(block, _)| block)
        .filter(|&block| block != removed_block)
        .collect::<Vec<_>>();
    affected.sort_unstable_by_key(|block| block.0);
    affected.dedup();
    for block_id in affected {
        let block = eu
            .blocks
            .get_mut(&block_id)
            .expect("moved cross-block definition must remain in the execution unit");
        let mut index = 0usize;
        block.instructions.retain(|_| {
            let keep = !removed.contains(&(block_id, index));
            index += 1;
            keep
        });
    }
}

fn eliminate_controlled_join_muxes(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    controlled_join_after: Option<usize>,
) {
    let Some(cfg) = CfgAnalysis::compute(eu) else {
        return;
    };
    let def_blocks = all_def_blocks(eu);
    let def_locations = instruction_def_locations(eu);
    let use_counts = count_uses(eu);
    let first_effect = eu
        .blocks
        .iter()
        .map(|(&block_id, block)| {
            let index = block
                .instructions
                .iter()
                .position(|instruction| {
                    memory_write(instruction).is_some() || is_memory_barrier(instruction)
                })
                .unwrap_or(block.instructions.len());
            (block_id, index)
        })
        .collect::<HashMap<_, _>>();
    let mut branches_by_root = HashMap::<RegisterId, Vec<BranchInfo>>::default();

    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);
    for block_id in block_ids.iter().copied() {
        let block = &eu.blocks[&block_id];
        let SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &block.terminator
        else {
            continue;
        };
        let (root, _) = resolve_boolean_alias(eu, &def_locations, *cond);
        branches_by_root.entry(root).or_default().push(BranchInfo {
            source: block_id,
            true_target: true_block.0,
            false_target: false_block.0,
        });
    }
    if branches_by_root.is_empty() {
        return;
    }

    let mut plans = Vec::new();
    for block_id in block_ids {
        if controlled_join_after.is_some_and(|watermark| block_id.0 <= watermark) {
            continue;
        }
        let block = &eu.blocks[&block_id];
        for (mux_idx, inst) in block.instructions.iter().enumerate() {
            let SIRInstruction::Mux(dst, condition, true_val, false_val) = inst else {
                continue;
            };
            let (root, _) = resolve_boolean_alias(eu, &def_locations, *condition);
            let plan = branches_by_root
                .get(&root)
                .into_iter()
                .flatten()
                .find_map(|branch| {
                    plan_controlled_join_mux(
                        eu,
                        &cfg,
                        &def_blocks,
                        &def_locations,
                        branch,
                        block_id,
                        mux_idx,
                        *condition,
                        *dst,
                        *true_val,
                        *false_val,
                        &use_counts,
                        &first_effect,
                    )
                })
                .or_else(|| {
                    plan_path_conditioned_join_mux(
                        eu,
                        &cfg,
                        &def_blocks,
                        &def_locations,
                        block_id,
                        mux_idx,
                        *dst,
                        *true_val,
                        *false_val,
                    )
                });
            let Some(plan) = plan else {
                continue;
            };
            plans.push(plan);
        }
    }

    apply_controlled_join_mux_plans(eu, plans);
}

fn apply_controlled_join_mux_plans(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plans: Vec<ControlledMuxPlan>,
) {
    let mut by_join = HashMap::<BlockId, Vec<ControlledMuxPlan>>::default();
    for plan in plans {
        by_join.entry(plan.join).or_default().push(plan);
    }
    let mut joins = by_join.keys().copied().collect::<Vec<_>>();
    joins.sort_unstable_by_key(|block| block.0);

    for join_id in joins {
        let Some(mut plans) = by_join.remove(&join_id) else {
            continue;
        };
        plans.sort_unstable_by_key(|plan| plan.mux_idx);
        let Some(original) = eu.blocks.get(&join_id).cloned() else {
            continue;
        };

        let mut removed = BTreeSet::new();
        let mut moved_by_predecessor = HashMap::<BlockId, BTreeSet<usize>>::default();
        let mut valid = true;
        for plan in &plans {
            if !matches!(
                original.instructions.get(plan.mux_idx),
                Some(SIRInstruction::Mux(dst, ..)) if *dst == plan.dst
            ) || !removed.insert(plan.mux_idx)
            {
                valid = false;
                break;
            }
            for moved in &plan.moved {
                if removed.contains(&moved.index)
                    || !moved_by_predecessor
                        .entry(moved.predecessor)
                        .or_default()
                        .insert(moved.index)
                {
                    valid = false;
                    break;
                }
                removed.insert(moved.index);
            }
            if !valid {
                break;
            }
        }
        if !valid
            || moved_by_predecessor.iter().any(|(&predecessor, _)| {
                !matches!(
                    eu.blocks.get(&predecessor).map(|block| &block.terminator),
                    Some(SIRTerminator::Jump(target, _)) if *target == join_id
                )
            })
        {
            continue;
        }

        let mut predecessors = moved_by_predecessor.keys().copied().collect::<Vec<_>>();
        predecessors.sort_unstable_by_key(|block| block.0);
        for predecessor in predecessors {
            let instructions = moved_by_predecessor[&predecessor]
                .iter()
                .map(|&index| original.instructions[index].clone())
                .collect::<Vec<_>>();
            eu.blocks
                .get_mut(&predecessor)
                .expect("preflighted controlled predecessor must remain present")
                .instructions
                .extend(instructions);
        }

        {
            let join = eu
                .blocks
                .get_mut(&join_id)
                .expect("controlled join must remain present");
            join.instructions = original
                .instructions
                .into_iter()
                .enumerate()
                .filter_map(|(index, instruction)| {
                    (!removed.contains(&index)).then_some(instruction)
                })
                .collect();
            join.params.extend(plans.iter().map(|plan| plan.dst));
        }

        // Parameter order and edge-argument order are both the ascending Mux
        // order above.  This publishes the edge-sunk definitions and the
        // select-to-phi rewrite as one valid SSA change.
        for plan in plans {
            for edge in plan.incoming {
                let value = if edge.select_true {
                    plan.true_val
                } else {
                    plan.false_val
                };
                append_controlled_edge_argument(
                    eu,
                    edge.predecessor,
                    plan.join,
                    edge.edge_truth,
                    value,
                );
            }
        }
    }
}

fn plan_controlled_join_mux(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &CfgAnalysis,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    branch: &BranchInfo,
    join: BlockId,
    mux_idx: usize,
    condition: RegisterId,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
    use_counts: &HashMap<RegisterId, usize>,
    first_effect: &HashMap<BlockId, usize>,
) -> Option<ControlledMuxPlan> {
    if branch.source == join
        || !cfg.graph.dominates(branch.source, join)
        || !cfg.graph.postdominates(join, branch.true_target)
        || !cfg.graph.postdominates(join, branch.false_target)
    {
        return None;
    }

    let block = eu.blocks.get(&join)?;
    if block.params.contains(&dst)
        || !matches!(
            block.instructions.get(mux_idx),
            Some(SIRInstruction::Mux(..))
        )
    {
        return None;
    }

    let incoming_edges = cfg.incoming_edges(join)?.to_vec();
    if incoming_edges.is_empty() {
        return None;
    }

    let mut incoming = Vec::with_capacity(incoming_edges.len());
    let mut seen_predecessors = HashSet::default();
    let mut moved = HashMap::<usize, BlockId>::default();
    for (predecessor, edge_truth) in incoming_edges {
        // A block with two edges to the same join has no unambiguous edge
        // classification for this transform.  Leave it to the general
        // branchifier instead of guessing.
        if !seen_predecessors.insert(predecessor) || predecessor == join {
            return None;
        }

        // A predicate can be branched on repeatedly.  Dominance alone cannot
        // identify which occurrence controls this edge: a CFG-only walk sees
        // infeasible paths that flip the same SSA boolean later.  Derive the
        // Mux's truth value from the actual incoming edge facts instead.
        let facts =
            cfg.path_facts
                .facts_on_edge(eu, def_locations, predecessor, join, edge_truth)?;
        let selected = known_condition_truth(eu, def_locations, &facts, condition)?;
        let selected_value = if selected { true_val } else { false_val };
        for definition in controlled_value_availability(
            eu,
            cfg,
            def_blocks,
            def_locations,
            use_counts,
            first_effect,
            join,
            mux_idx,
            predecessor,
            selected_value,
        )? {
            if moved
                .insert(definition.index, definition.predecessor)
                .is_some_and(|owner| owner != definition.predecessor)
            {
                // One SSA definition cannot be moved to two predecessor
                // blocks without cloning and renaming its complete DAG.
                return None;
            }
        }

        incoming.push(ControlledIncomingEdge {
            predecessor,
            select_true: selected,
            edge_truth,
        });
    }

    Some(ControlledMuxPlan {
        join,
        mux_idx,
        dst,
        true_val,
        false_val,
        incoming,
        moved: moved
            .into_iter()
            .map(|(index, predecessor)| ControlledMovedInstruction { predecessor, index })
            .collect(),
    })
}

#[allow(clippy::too_many_arguments)]
fn controlled_value_availability(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &CfgAnalysis,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    use_counts: &HashMap<RegisterId, usize>,
    first_effect: &HashMap<BlockId, usize>,
    join: BlockId,
    mux_idx: usize,
    predecessor: BlockId,
    value: RegisterId,
) -> Option<Vec<ControlledMovedInstruction>> {
    let &definition_block = def_blocks.get(&value)?;
    if cfg.graph.dominates(definition_block, predecessor) {
        return Some(Vec::new());
    }
    if definition_block != join
        || !matches!(
            eu.blocks.get(&predecessor).map(|block| &block.terminator),
            Some(SIRTerminator::Jump(target, _)) if *target == join
        )
    {
        return None;
    }

    let block = eu.blocks.get(&join)?;
    let mut definitions = HashSet::default();
    collect_controlled_edge_defs(
        block,
        join,
        def_locations,
        use_counts,
        mux_idx,
        value,
        &mut definitions,
    );
    let &(_, root_index) = def_locations.get(&value)?;
    if !definitions.contains(&root_index) {
        return None;
    }

    let moved_values = definitions
        .iter()
        .filter_map(|&index| def_reg(&block.instructions[index]))
        .collect::<HashSet<_>>();
    for &index in &definitions {
        let instruction = &block.instructions[index];
        // A state read can move from the join entry to the predecessor edge
        // only if it crosses no write or runtime-observation point.  The first
        // effect index is one scalar per block, avoiding a dense per-load
        // prefix table on very large EUs.
        if matches!(instruction, SIRInstruction::Load(..))
            && index >= first_effect.get(&join).copied().unwrap_or(0)
        {
            return None;
        }
        for operand in inst_uses(instruction) {
            if moved_values.contains(&operand) {
                continue;
            }
            let &operand_block = def_blocks.get(&operand)?;
            if !cfg.graph.dominates(operand_block, predecessor) {
                return None;
            }
        }
    }

    let mut definitions = definitions.into_iter().collect::<Vec<_>>();
    definitions.sort_unstable();
    Some(
        definitions
            .into_iter()
            .map(|index| ControlledMovedInstruction { predecessor, index })
            .collect(),
    )
}

fn collect_controlled_edge_defs(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    block_id: BlockId,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    use_counts: &HashMap<RegisterId, usize>,
    user_index: usize,
    value: RegisterId,
    definitions: &mut HashSet<usize>,
) {
    if use_counts.get(&value).copied().unwrap_or(0) != 1 {
        return;
    }
    let Some(&(definition_block, index)) = def_locations.get(&value) else {
        return;
    };
    if definition_block != block_id || index >= user_index || definitions.contains(&index) {
        return;
    }
    let instruction = &block.instructions[index];
    if !matches!(
        instruction,
        SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Load(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
    ) {
        return;
    }

    definitions.insert(index);
    for operand in inst_uses(instruction) {
        collect_controlled_edge_defs(
            block,
            block_id,
            def_locations,
            use_counts,
            index,
            operand,
            definitions,
        );
    }
}

fn plan_path_conditioned_join_mux(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &CfgAnalysis,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    join: BlockId,
    mux_idx: usize,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
) -> Option<ControlledMuxPlan> {
    let block = eu.blocks.get(&join)?;
    if block.params.contains(&dst)
        || !matches!(
            block.instructions.get(mux_idx),
            Some(SIRInstruction::Mux(..))
        )
    {
        return None;
    }
    let incoming_edges = cfg.incoming_edges(join)?.to_vec();
    if incoming_edges.is_empty() {
        return None;
    }
    let Some(SIRInstruction::Mux(_, condition, ..)) = block.instructions.get(mux_idx) else {
        return None;
    };
    let (_, condition_inverted) = resolve_boolean_alias(eu, def_locations, *condition);
    let mut incoming = Vec::with_capacity(incoming_edges.len());
    let mut seen_predecessors = HashSet::default();
    for (predecessor, edge_truth) in incoming_edges {
        if !seen_predecessors.insert(predecessor) || predecessor == join {
            return None;
        }
        let facts =
            cfg.path_facts
                .facts_on_edge(eu, def_locations, predecessor, join, edge_truth)?;
        let condition_truth = known_condition_truth(eu, def_locations, &facts, *condition)?;
        let select_true = condition_truth ^ condition_inverted;
        let selected_value = if select_true { true_val } else { false_val };
        let def_block = def_blocks.get(&selected_value)?;
        if !cfg.graph.dominates(*def_block, predecessor) {
            return None;
        }
        incoming.push(ControlledIncomingEdge {
            predecessor,
            select_true,
            edge_truth,
        });
    }
    Some(ControlledMuxPlan {
        join,
        mux_idx,
        dst,
        true_val,
        false_val,
        incoming,
        moved: Vec::new(),
    })
}

fn append_controlled_edge_argument(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    predecessor: BlockId,
    target: BlockId,
    edge_truth: Option<bool>,
    value: RegisterId,
) {
    let Some(block) = eu.blocks.get_mut(&predecessor) else {
        return;
    };
    match &mut block.terminator {
        SIRTerminator::Jump(destination, args) if *destination == target => args.push(value),
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => match edge_truth {
            Some(true) if true_block.0 == target => true_block.1.push(value),
            Some(false) if false_block.0 == target => false_block.1.push(value),
            _ => {}
        },
        _ => {}
    }
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

fn resolve_boolean_alias(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> (RegisterId, bool) {
    let mut inverted = false;
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let Some(&(block_id, idx)) = locations.get(&register) else {
            break;
        };
        match &eu.blocks[&block_id].instructions[idx] {
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => {
                register = *source;
            }
            SIRInstruction::Unary(_, crate::ir::UnaryOp::LogicNot, source) => {
                register = *source;
                inverted = !inverted;
            }
            _ => break,
        }
    }
    (register, inverted)
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

fn indexed_incoming_edges(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    graph: &SirCfg,
) -> Vec<Vec<(BlockId, Option<bool>)>> {
    let mut incoming = vec![Vec::new(); graph.block_ids.len()];
    for (target, predecessors) in graph.predecessors.iter().enumerate() {
        let target_id = graph.block_ids[target];
        for &predecessor in predecessors {
            let predecessor_id = graph.block_ids[predecessor];
            match &eu.blocks[&predecessor_id].terminator {
                SIRTerminator::Jump(destination, _) if *destination == target_id => {
                    incoming[target].push((predecessor_id, None));
                }
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => {
                    if true_block.0 == target_id {
                        incoming[target].push((predecessor_id, Some(true)));
                    }
                    if false_block.0 == target_id {
                        incoming[target].push((predecessor_id, Some(false)));
                    }
                }
                _ => {}
            }
        }
    }
    incoming
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

fn facts_on_edge(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    predecessor: BlockId,
    target: BlockId,
    edge_truth: Option<bool>,
    facts: &HashMap<PathFactKey, bool>,
) -> Option<HashMap<PathFactKey, bool>> {
    let mut result = facts.clone();
    let block = eu.blocks.get(&predecessor)?;
    match (&block.terminator, edge_truth) {
        (SIRTerminator::Jump(destination, _), None) if *destination == target => {}
        (
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            },
            Some(truth),
        ) if (truth && true_block.0 == target) || (!truth && false_block.0 == target) => {
            let (root, inverted) = resolve_boolean_alias(eu, def_locations, *cond);
            let root_truth = truth ^ inverted;
            let register_key = PathFactKey::Register(root);
            if result
                .get(&register_key)
                .is_some_and(|known| *known != root_truth)
            {
                return None;
            }
            result.insert(register_key, root_truth);
            if let Some((predicate, predicate_inverted)) = predicate_key(eu, def_locations, *cond) {
                let predicate_truth = truth ^ predicate_inverted;
                let key = PathFactKey::Predicate(predicate);
                if result
                    .get(&key)
                    .is_some_and(|known| *known != predicate_truth)
                {
                    return None;
                }
                result.insert(key, predicate_truth);
            }
        }
        _ => return None,
    }
    Some(result)
}

fn known_condition_truth(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    facts: &HashMap<PathFactKey, bool>,
    condition: RegisterId,
) -> Option<bool> {
    let (root, inverted) = resolve_boolean_alias(eu, def_locations, condition);
    if let Some(value) = facts.get(&PathFactKey::Register(root)) {
        return Some(*value ^ inverted);
    }
    let (predicate, predicate_inverted) = predicate_key(eu, def_locations, condition)?;
    let value = known_predicate_truth(facts, &predicate)?;
    Some(value ^ predicate_inverted)
}

fn known_predicate_truth(facts: &HashMap<PathFactKey, bool>, query: &PredicateKey) -> Option<bool> {
    if let Some(value) = facts.get(&PathFactKey::Predicate(query.clone())) {
        return Some(*value);
    }
    let same_lhs = |key: &PredicateKey| key.lhs == query.lhs;
    for (fact, &value) in facts {
        let PathFactKey::Predicate(fact) = fact else {
            continue;
        };
        if !same_lhs(fact) {
            continue;
        }
        if fact.kind == query.kind
            && fact.kind == PredicateKind::Equal
            && different_constants(&fact.rhs, &query.rhs)
            && value
        {
            return Some(false);
        }
        if fact.rhs == query.rhs && fact.kind != query.kind && value {
            return Some(false);
        }
        if fact.rhs == query.rhs && fact.kind != query.kind && !value {
            return Some(true);
        }
    }
    None
}

fn different_constants(left: &PredicateRhs, right: &PredicateRhs) -> bool {
    match (left, right) {
        (
            PredicateRhs::Constant(left_payload, left_mask),
            PredicateRhs::Constant(right_payload, right_mask),
        ) => {
            left_mask.iter().all(|word| *word == 0)
                && right_mask.iter().all(|word| *word == 0)
                && left_payload != right_payload
        }
        _ => false,
    }
}

fn predicate_key(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> Option<(PredicateKey, bool)> {
    let mut inverted = false;
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let &(block, index) = locations.get(&register)?;
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Unary(_, crate::ir::UnaryOp::LogicNot, source) => {
                register = *source;
                inverted = !inverted;
            }
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => register = *source,
            SIRInstruction::Binary(_, lhs, op, rhs)
                if matches!(
                    op,
                    crate::ir::BinaryOp::Eq
                        | crate::ir::BinaryOp::EqWildcard
                        | crate::ir::BinaryOp::Ne
                        | crate::ir::BinaryOp::NeWildcard
                ) =>
            {
                let kind = match op {
                    crate::ir::BinaryOp::Eq | crate::ir::BinaryOp::EqWildcard => {
                        PredicateKind::Equal
                    }
                    crate::ir::BinaryOp::Ne | crate::ir::BinaryOp::NeWildcard => {
                        PredicateKind::NotEqual
                    }
                    _ => unreachable!(),
                };
                let lhs = canonical_identity_register(eu, locations, *lhs);
                let rhs = if let Some(value) = immediate_value(eu, locations, *rhs) {
                    PredicateRhs::Constant(value.0, value.1)
                } else {
                    PredicateRhs::Register(canonical_identity_register(eu, locations, *rhs))
                };
                return Some((PredicateKey { lhs, kind, rhs }, inverted));
            }
            _ => return None,
        }
    }
    None
}

fn canonical_identity_register(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> RegisterId {
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let Some(&(block, index)) = locations.get(&register) else {
            break;
        };
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => register = *source,
            _ => break,
        }
    }
    register
}

fn immediate_value(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> Option<(Vec<u64>, Vec<u64>)> {
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let &(block, index) = locations.get(&register)?;
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Imm(_, value) => {
                return Some((value.payload.to_u64_digits(), value.mask.to_u64_digits()));
            }
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => register = *source,
            _ => return None,
        }
    }
    None
}

fn find_branchify_mux_in_block(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    use_counts: &HashMap<RegisterId, usize>,
    def_blocks: &HashMap<RegisterId, BlockId>,
) -> Option<BranchifyPlan> {
    let block = eu.blocks.get(&block_id)?;
    let mut def_pos = HashMap::default();
    for (idx, inst) in block.instructions.iter().enumerate() {
        if let Some(def) = def_reg(inst) {
            def_pos.insert(def, idx);
        }
    }

    for (mux_idx, inst) in block.instructions.iter().enumerate() {
        let SIRInstruction::Mux(dst, cond, true_val, false_val) = inst else {
            continue;
        };

        if use_counts.get(dst).copied().unwrap_or(0) > block_use_count(block, *dst) {
            continue;
        }

        let immediate_store = find_distributed_store(block, mux_idx, *dst, *true_val, *false_val);
        let preserve_result =
            immediate_store.is_none() || use_counts.get(dst).copied().unwrap_or(0) > 1;
        let memory_barrier_idx = if preserve_result {
            mux_idx
        } else {
            immediate_store
                .as_ref()
                .expect("single-use store mux should have a store")
                .idx
                + 1
        };

        let mut true_defs = HashSet::default();
        let mut false_defs = HashSet::default();
        collect_sinkable_defs(
            block,
            &def_pos,
            use_counts,
            mux_idx,
            memory_barrier_idx,
            *true_val,
            &mut true_defs,
        );
        collect_sinkable_defs(
            block,
            &def_pos,
            use_counts,
            mux_idx,
            memory_barrier_idx,
            *false_val,
            &mut false_defs,
        );
        if !true_defs.is_disjoint(&false_defs) {
            continue;
        }
        if !terminator_uses(&block.terminator).contains(dst)
            && true_defs
                .iter()
                .chain(false_defs.iter())
                .all(|idx| is_trivial_select_input(&block.instructions[*idx]))
        {
            continue;
        }

        let mut true_defs = true_defs.into_iter().collect::<Vec<_>>();
        let mut false_defs = false_defs.into_iter().collect::<Vec<_>>();
        true_defs.sort_unstable();
        false_defs.sort_unstable();
        let plan = BranchifyPlan {
            block_id,
            mux_idx,
            dst: *dst,
            cond: *cond,
            true_val: *true_val,
            false_val: *false_val,
            true_defs,
            false_defs,
            distributed_store: if preserve_result {
                None
            } else {
                immediate_store
            },
            preserve_result,
        };
        if !branch_is_profitable(eu, block, &plan, def_blocks, &def_pos) {
            continue;
        }
        return Some(plan);
    }

    None
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

fn branch_is_profitable(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_pos: &HashMap<RegisterId, usize>,
) -> bool {
    branch_profitability(eu, block, plan, def_blocks, def_pos).proves_expected_benefit()
}

fn branch_profitability(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_pos: &HashMap<RegisterId, usize>,
) -> BranchProfitability {
    let remove_defs = removable_defs_after_head_restore(block, plan, def_blocks);
    let arm_cost = |defs: &[usize]| {
        defs.iter()
            .filter(|idx| remove_defs.contains(idx))
            .map(|&idx| branchified_instruction_cost(&block.instructions[idx], &eu.register_map))
            .sum::<u128>()
    };
    let suffix = block
        .instructions
        .iter()
        .enumerate()
        .skip(plan.mux_idx + 1)
        .filter(|(idx, _)| !remove_defs.contains(idx))
        .map(|(_, inst)| inst.clone())
        .collect::<Vec<_>>();
    let mut live_through = block_live_ins(&suffix, &terminator_uses(&block.terminator));
    live_through.retain(|value| *value != plan.dst);
    live_through.sort_unstable();
    live_through.dedup();

    let chunks_for = |value: RegisterId| {
        eu.register_map
            .get(&value)
            .map(|register| register.width().div_ceil(64).max(1))
            .unwrap_or(1) as u128
    };
    let result_chunks = if plan.preserve_result {
        chunks_for(plan.dst)
    } else {
        0
    };
    let live_through_chunks = live_through.into_iter().map(chunks_for).sum::<u128>();

    BranchProfitability {
        true_arm_cost: arm_cost(&plan.true_defs),
        false_arm_cost: arm_cost(&plan.false_defs),
        removed_mux_cost: branchified_instruction_cost(
            &block.instructions[plan.mux_idx],
            &eu.register_map,
        ),
        probability: static_true_probability(block, def_pos, plan.cond),
        control_cost: BRANCH_CONTROL_COST,
        phi_copy_cost: result_chunks.saturating_mul(PHI_COPY_COST_PER_CHUNK),
        live_through_cost: live_through_chunks.saturating_mul(LIVE_THROUGH_COST_PER_CHUNK),
    }
}

fn static_true_probability(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    def_pos: &HashMap<RegisterId, usize>,
    cond: RegisterId,
) -> StaticBranchProbability {
    let mut current = cond;
    let mut inverted = false;
    let mut seen = HashSet::default();

    while seen.insert(current) {
        let Some(&idx) = def_pos.get(&current) else {
            break;
        };
        match &block.instructions[idx] {
            SIRInstruction::Unary(_, crate::ir::UnaryOp::LogicNot, inner) => {
                inverted = !inverted;
                current = *inner;
            }
            SIRInstruction::Unary(_, crate::ir::UnaryOp::Ident, inner) => {
                current = *inner;
            }
            SIRInstruction::Binary(
                _,
                lhs,
                op @ (crate::ir::BinaryOp::Eq
                | crate::ir::BinaryOp::Ne
                | crate::ir::BinaryOp::EqWildcard
                | crate::ir::BinaryOp::NeWildcard),
                rhs,
            ) if register_is_immediate(block, def_pos, *lhs)
                || register_is_immediate(block, def_pos, *rhs) =>
            {
                let equality = matches!(
                    op,
                    crate::ir::BinaryOp::Eq | crate::ir::BinaryOp::EqWildcard
                );
                let probability = if equality != inverted {
                    StaticBranchProbability::EQUALITY_TO_CONSTANT
                } else {
                    StaticBranchProbability::EQUALITY_TO_CONSTANT.inverted()
                };
                return probability;
            }
            _ => break,
        }
    }

    if inverted {
        StaticBranchProbability::EVEN.inverted()
    } else {
        StaticBranchProbability::EVEN
    }
}

fn register_is_immediate(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    def_pos: &HashMap<RegisterId, usize>,
    register: RegisterId,
) -> bool {
    let mut current = register;
    let mut seen = HashSet::default();
    while seen.insert(current) {
        let Some(&idx) = def_pos.get(&current) else {
            return false;
        };
        match &block.instructions[idx] {
            SIRInstruction::Imm(..) => return true,
            SIRInstruction::Unary(_, crate::ir::UnaryOp::Ident, inner) => current = *inner,
            _ => return false,
        }
    }
    false
}

/// Estimated dynamic target work for an instruction that can be moved into a
/// branch arm.  This deliberately follows the same width/chunk model as
/// cost-directed SLT mux lowering instead of the CLIF-size estimator: the
/// decision is about runtime work skipped, not compiler IR expansion.
fn branchified_instruction_cost(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    register_map: &HashMap<RegisterId, crate::ir::RegisterType>,
) -> u128 {
    let register_width = |register: RegisterId| {
        register_map
            .get(&register)
            .map(crate::ir::RegisterType::width)
            .unwrap_or(64)
    };
    let chunks = |width: usize| width.div_ceil(64).max(1) as u128;

    match inst {
        SIRInstruction::Imm(dst, _) => chunks(register_width(*dst)),
        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            let operand_chunks = chunks(
                register_width(*dst)
                    .max(register_width(*lhs))
                    .max(register_width(*rhs)),
            );
            match op {
                crate::ir::BinaryOp::And
                | crate::ir::BinaryOp::Or
                | crate::ir::BinaryOp::Xor
                | crate::ir::BinaryOp::LogicAnd
                | crate::ir::BinaryOp::LogicOr => operand_chunks,
                crate::ir::BinaryOp::Add | crate::ir::BinaryOp::Sub => 3 * operand_chunks,
                crate::ir::BinaryOp::Mul => 5 * operand_chunks.saturating_mul(operand_chunks),
                crate::ir::BinaryOp::DivU
                | crate::ir::BinaryOp::DivS
                | crate::ir::BinaryOp::RemU
                | crate::ir::BinaryOp::RemS => 12 * operand_chunks.saturating_mul(operand_chunks),
                crate::ir::BinaryOp::Shl | crate::ir::BinaryOp::Shr | crate::ir::BinaryOp::Sar => {
                    4 * operand_chunks
                }
                crate::ir::BinaryOp::Eq
                | crate::ir::BinaryOp::Ne
                | crate::ir::BinaryOp::EqWildcard
                | crate::ir::BinaryOp::NeWildcard
                | crate::ir::BinaryOp::LtU
                | crate::ir::BinaryOp::LtS
                | crate::ir::BinaryOp::LeU
                | crate::ir::BinaryOp::LeS
                | crate::ir::BinaryOp::GtU
                | crate::ir::BinaryOp::GtS
                | crate::ir::BinaryOp::GeU
                | crate::ir::BinaryOp::GeS => 3 * operand_chunks,
            }
        }
        SIRInstruction::Unary(dst, op, src) => {
            let operand_chunks = chunks(register_width(*dst).max(register_width(*src)));
            match op {
                crate::ir::UnaryOp::PopCount => 2 * operand_chunks + 1,
                crate::ir::UnaryOp::CountLeadingZeros | crate::ir::UnaryOp::CountTrailingZeros => {
                    3 * operand_chunks + 1
                }
                _ => 2 * operand_chunks,
            }
        }
        SIRInstruction::Load(_, _, offset, width) => {
            3 * chunks(*width) + 3 * u128::from(offset.is_dynamic())
        }
        SIRInstruction::Concat(dst, args) => chunks(register_width(*dst)) + args.len() as u128,
        SIRInstruction::Slice(dst, _, _, _) => 2 * chunks(register_width(*dst)),
        SIRInstruction::Mux(dst, _, true_value, false_value) => chunks(
            register_width(*dst)
                .max(register_width(*true_value))
                .max(register_width(*false_value)),
        ),
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => 0,
    }
}

fn find_distributed_store(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    mux_idx: usize,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
) -> Option<DistributedStore> {
    let store_idx = mux_idx + 1;
    let store = block.instructions.get(store_idx)?;
    match store {
        SIRInstruction::Store(addr, offset, width, src, triggers, sites) if *src == dst => {
            Some(DistributedStore {
                idx: store_idx,
                true_inst: SIRInstruction::Store(
                    *addr,
                    offset.clone(),
                    *width,
                    true_val,
                    triggers.clone(),
                    sites.clone(),
                ),
                false_inst: SIRInstruction::Store(
                    *addr,
                    offset.clone(),
                    *width,
                    false_val,
                    triggers.clone(),
                    sites.clone(),
                ),
            })
        }
        _ => None,
    }
}

fn collect_sinkable_defs(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    def_pos: &HashMap<RegisterId, usize>,
    use_counts: &HashMap<RegisterId, usize>,
    user_idx: usize,
    memory_barrier_idx: usize,
    root: RegisterId,
    defs: &mut HashSet<usize>,
) {
    if use_counts.get(&root).copied().unwrap_or(0) != 1 {
        return;
    }
    let Some(&idx) = def_pos.get(&root) else {
        return;
    };
    if idx >= user_idx || defs.contains(&idx) {
        return;
    }
    let inst = &block.instructions[idx];
    if !is_sinkable_input(inst) {
        return;
    }
    if let Some(load) = memory_read(inst)
        && has_intervening_memory_conflict(block, idx + 1, memory_barrier_idx, load)
    {
        return;
    }

    defs.insert(idx);
    for use_reg in inst_uses(inst) {
        collect_sinkable_defs(
            block,
            def_pos,
            use_counts,
            idx,
            memory_barrier_idx,
            use_reg,
            defs,
        );
    }
}

fn is_sinkable_input(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        inst,
        SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Load(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..)
    )
}

fn is_trivial_select_input(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(inst, SIRInstruction::Imm(..))
}

#[derive(Clone, Copy)]
struct MemAccess<'a> {
    addr: &'a RegionedAbsoluteAddr,
    offset: Option<usize>,
    width: usize,
}

fn memory_read(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<MemAccess<'_>> {
    match inst {
        SIRInstruction::Load(_, addr, offset, width) => Some(MemAccess {
            addr,
            offset: offset_static(offset),
            width: *width,
        }),
        _ => None,
    }
}

fn memory_write(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<MemAccess<'_>> {
    match inst {
        SIRInstruction::Store(addr, offset, width, _, _, _) => Some(MemAccess {
            addr,
            offset: offset_static(offset),
            width: *width,
        }),
        SIRInstruction::Commit(_, dst, offset, width, _) => Some(MemAccess {
            addr: dst,
            offset: offset_static(offset),
            width: *width,
        }),
        _ => None,
    }
}

fn offset_static(offset: &SIROffset) -> Option<usize> {
    match offset {
        SIROffset::Static(offset) => Some(*offset),
        SIROffset::Dynamic(_) | SIROffset::Element { .. } | SIROffset::PackedElements { .. } => {
            None
        }
    }
}

fn has_intervening_memory_conflict(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    start: usize,
    end: usize,
    read: MemAccess<'_>,
) -> bool {
    block.instructions[start..end].iter().any(|inst| {
        is_memory_barrier(inst)
            || memory_write(inst).is_some_and(|write| mem_may_alias(read, write))
    })
}

fn is_memory_barrier(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        inst,
        SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. }
    )
}

fn mem_may_alias(a: MemAccess<'_>, b: MemAccess<'_>) -> bool {
    if a.addr != b.addr {
        return false;
    }
    match (a.offset, b.offset) {
        (Some(a_off), Some(b_off)) => a_off < b_off + b.width && b_off < a_off + a.width,
        _ => true,
    }
}

fn apply_branchify_mux(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: BranchifyPlan,
    use_counts: &mut HashMap<RegisterId, usize>,
    def_blocks: &mut HashMap<RegisterId, BlockId>,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
    trace_reg: Option<RegisterId>,
) -> [BlockId; 3] {
    let true_id = BlockId(*next_block_id);
    let false_id = BlockId(*next_block_id + 1);
    let merge_id = BlockId(*next_block_id + 2);
    *next_block_id += 3;

    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("branchify target block must exist");
    if let Some(reg) = trace_reg {
        trace_reg_in_original(&original, &plan, reg);
    }
    remove_block_uses(use_counts, &original);
    let remove_defs = removable_defs_after_head_restore(&original, &plan, def_blocks);
    if let Some(reg) = trace_reg {
        trace_reg_branchify_plan(&original, &plan, &remove_defs, reg);
    }

    let mut head_insts = Vec::new();
    for (idx, inst) in original.instructions.iter().enumerate().take(plan.mux_idx) {
        if !remove_defs.contains(&idx) {
            head_insts.push(inst.clone());
        }
    }
    let branch_cond = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_insts,
        plan.cond,
        reg_counter,
    );
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

    let mut true_insts = plan
        .true_defs
        .iter()
        .filter(|idx| remove_defs.contains(idx))
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    let mut false_insts = plan
        .false_defs
        .iter()
        .filter(|idx| remove_defs.contains(idx))
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    if let Some(store) = &plan.distributed_store {
        true_insts.push(store.true_inst.clone());
        false_insts.push(store.false_inst.clone());
    }
    let true_args = if plan.preserve_result {
        vec![plan.true_val]
    } else {
        Vec::new()
    };
    let false_args = if plan.preserve_result {
        vec![plan.false_val]
    } else {
        Vec::new()
    };
    let merge_params = if plan.preserve_result {
        vec![plan.dst]
    } else {
        Vec::new()
    };

    let merge_terminator = original.terminator;

    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_insts,
        terminator: SIRTerminator::Branch {
            cond: branch_cond,
            true_block: (true_id, Vec::new()),
            false_block: (false_id, Vec::new()),
        },
    };
    let true_block = BasicBlock {
        id: true_id,
        params: Vec::new(),
        instructions: true_insts,
        terminator: SIRTerminator::Jump(merge_id, true_args),
    };
    let false_block = BasicBlock {
        id: false_id,
        params: Vec::new(),
        instructions: false_insts,
        terminator: SIRTerminator::Jump(merge_id, false_args),
    };
    let merge_block = BasicBlock {
        id: merge_id,
        params: merge_params,
        instructions: suffix,
        terminator: merge_terminator,
    };

    add_block_uses(use_counts, &head);
    add_block_uses(use_counts, &true_block);
    add_block_uses(use_counts, &false_block);
    add_block_uses(use_counts, &merge_block);

    eu.blocks.insert(plan.block_id, head);
    eu.blocks.insert(true_id, true_block);
    eu.blocks.insert(false_id, false_block);
    eu.blocks.insert(merge_id, merge_block);

    for block_id in [plan.block_id, true_id, false_id, merge_id] {
        for inst in &eu.blocks[&block_id].instructions {
            if let Some(def) = def_reg(inst) {
                def_blocks.insert(def, block_id);
            }
        }
    }

    if let Some(reg) = trace_reg {
        for block_id in [plan.block_id, true_id, false_id, merge_id] {
            if let Some(block) = eu.blocks.get(&block_id) {
                trace_reg_in_new_block(block, reg);
            }
        }
    }

    [true_id, false_id, merge_id]
}

fn removable_defs_after_head_restore(
    original: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    def_blocks: &HashMap<RegisterId, BlockId>,
) -> HashSet<usize> {
    let mut remove_defs = plan
        .true_defs
        .iter()
        .chain(plan.false_defs.iter())
        .copied()
        .collect::<HashSet<_>>();
    remove_defs.insert(plan.mux_idx);
    if let Some(store) = &plan.distributed_store {
        remove_defs.insert(store.idx);
    }
    let restore_defs = head_restore_defs(original, plan, &remove_defs, def_blocks);
    for idx in restore_defs {
        remove_defs.remove(&idx);
    }
    remove_defs
}

fn trace_reg_in_original(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    reg: RegisterId,
) {
    let defines = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(idx, inst)| (def_reg(inst) == Some(reg)).then_some((idx, inst)))
        .collect::<Vec<_>>();
    let inst_uses = block
        .instructions
        .iter()
        .enumerate()
        .filter(|(_, inst)| inst_uses(inst).contains(&reg))
        .collect::<Vec<_>>();
    let term_uses = terminator_uses(&block.terminator).contains(&reg);
    if defines.is_empty() && inst_uses.is_empty() && !term_uses && !block.params.contains(&reg) {
        return;
    }
    tracing::debug!(
        "[branchify-trace] original block=b{} mux_idx={} dst=r{} cond=r{} true=r{} false=r{} params={} term_uses={} true_defs={:?} false_defs={:?}",
        block.id.0,
        plan.mux_idx,
        plan.dst.0,
        plan.cond.0,
        plan.true_val.0,
        plan.false_val.0,
        block.params.contains(&reg),
        term_uses,
        plan.true_defs,
        plan.false_defs
    );
    for (idx, inst) in defines {
        tracing::debug!(
            "[branchify-trace] original defines r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    for (idx, inst) in inst_uses {
        tracing::debug!(
            "[branchify-trace] original uses r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    if term_uses {
        tracing::debug!(
            "[branchify-trace] original terminator uses r{}: {}",
            reg.0,
            block.terminator
        );
    }
}

fn trace_reg_branchify_plan(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    remove_defs: &HashSet<usize>,
    reg: RegisterId,
) {
    for (idx, inst) in block.instructions.iter().enumerate() {
        if def_reg(inst) == Some(reg) {
            tracing::debug!(
                "[branchify-trace] after restore decision block=b{} r{} def_idx={idx} removed={} inst={inst}",
                block.id.0,
                reg.0,
                remove_defs.contains(&idx)
            );
        }
    }
    if plan.cond == reg || plan.true_val == reg || plan.false_val == reg || plan.dst == reg {
        tracing::debug!(
            "[branchify-trace] plan directly references r{} block=b{} mux_idx={} dst=r{} cond=r{} true=r{} false=r{}",
            reg.0,
            block.id.0,
            plan.mux_idx,
            plan.dst.0,
            plan.cond.0,
            plan.true_val.0,
            plan.false_val.0
        );
    }
}

fn trace_reg_in_new_block(block: &BasicBlock<RegionedAbsoluteAddr>, reg: RegisterId) {
    let term_uses = terminator_uses(&block.terminator).contains(&reg);
    let inst_uses = block
        .instructions
        .iter()
        .enumerate()
        .filter(|(_, inst)| inst_uses(inst).contains(&reg))
        .collect::<Vec<_>>();
    let defines = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(idx, inst)| (def_reg(inst) == Some(reg)).then_some((idx, inst)))
        .collect::<Vec<_>>();
    if !block.params.contains(&reg) && !term_uses && inst_uses.is_empty() && defines.is_empty() {
        return;
    }
    tracing::debug!(
        "[branchify-trace] new block=b{} params={} term_uses={} insts={} defs={}",
        block.id.0,
        block.params.contains(&reg),
        term_uses,
        inst_uses.len(),
        defines.len()
    );
    for (idx, inst) in defines {
        tracing::debug!(
            "[branchify-trace] new defines r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    for (idx, inst) in inst_uses {
        tracing::debug!(
            "[branchify-trace] new uses r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    if term_uses {
        tracing::debug!(
            "[branchify-trace] new terminator uses r{}: {}",
            reg.0,
            block.terminator
        );
    }
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

fn block_live_ins(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    terminator_args: &[RegisterId],
) -> Vec<RegisterId> {
    let mut defs = HashSet::default();
    let mut live_ins = Vec::new();
    let mut seen = HashSet::default();

    for inst in instructions {
        for reg in inst_uses(inst) {
            if !defs.contains(&reg) && seen.insert(reg) {
                live_ins.push(reg);
            }
        }
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

fn inline_param_only_jump_blocks(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    loop {
        let (pred_counts, jump_preds) = predecessor_info(eu);
        let use_blocks = register_use_blocks(eu);
        let mut eligible = eu
            .blocks
            .keys()
            .copied()
            .filter(|&block_id| block_id != eu.entry_block_id)
            .filter(|block_id| param_only_replacement(eu, *block_id, &use_blocks).is_some())
            .filter(|block_id| {
                let jump_count = jump_preds.get(block_id).map_or(0, Vec::len);
                jump_count > 0 && pred_counts.get(block_id).copied().unwrap_or(0) == jump_count
            })
            .collect::<Vec<_>>();
        eligible.sort();

        if eligible.is_empty() {
            break;
        }

        // Do not remove adjacent candidates from the same predecessor
        // snapshot.  Given A -> B, removing A can create new predecessors of
        // B which are absent from `jump_preds`; removing B afterwards would
        // then leave those predecessors targeting a deleted block.  A greedy
        // independent set still removes a constant fraction of a long chain,
        // so the number of whole-CFG rebuilds remains logarithmic.
        let mut selected = HashSet::default();
        let eligible = eligible
            .into_iter()
            .filter(|block_id| {
                let adjacent_to_selected_predecessor = jump_preds
                    .get(block_id)
                    .is_some_and(|preds| preds.iter().any(|pred| selected.contains(pred)));
                let adjacent_to_selected_successor = match &eu.blocks[block_id].terminator {
                    SIRTerminator::Jump(target, _) => selected.contains(target),
                    SIRTerminator::Branch {
                        true_block,
                        false_block,
                        ..
                    } => selected.contains(&true_block.0) || selected.contains(&false_block.0),
                    SIRTerminator::Switch { cases, default, .. } => {
                        selected.contains(default)
                            || cases.iter().any(|case| selected.contains(&case.target))
                    }
                    SIRTerminator::Return | SIRTerminator::Error(_) => false,
                };
                if adjacent_to_selected_predecessor || adjacent_to_selected_successor {
                    false
                } else {
                    selected.insert(*block_id);
                    true
                }
            })
            .collect::<Vec<_>>();

        for block_id in eligible {
            if !eu.blocks.contains_key(&block_id) {
                continue;
            }
            let Some(replacement) = param_only_replacement(eu, block_id, &use_blocks) else {
                continue;
            };
            let Some(preds) = jump_preds.get(&block_id) else {
                continue;
            };
            let params = eu.blocks[&block_id].params.clone();
            for &pred_id in preds {
                if !eu.blocks.contains_key(&pred_id) {
                    continue;
                }
                let pred_args = match &eu.blocks[&pred_id].terminator {
                    SIRTerminator::Jump(target, args) if *target == block_id => args.clone(),
                    _ => continue,
                };
                let map = params
                    .iter()
                    .copied()
                    .zip(pred_args)
                    .collect::<HashMap<_, _>>();
                eu.blocks.get_mut(&pred_id).unwrap().terminator =
                    substitute_terminator(&replacement, &map);
            }
            eu.blocks.remove(&block_id);
        }
    }
}

fn param_only_replacement(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    use_blocks: &HashMap<RegisterId, HashSet<BlockId>>,
) -> Option<SIRTerminator> {
    let block = eu.blocks.get(&block_id)?;
    if !block.instructions.is_empty() || block.params.is_empty() {
        return None;
    }
    if block.params.iter().any(|param| {
        use_blocks
            .get(param)
            .is_some_and(|uses| uses.iter().any(|use_block| *use_block != block_id))
    }) {
        return None;
    }
    match &block.terminator {
        SIRTerminator::Jump(_, _) | SIRTerminator::Branch { .. } | SIRTerminator::Switch { .. } => {
            Some(block.terminator.clone())
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => None,
    }
}

fn register_use_blocks(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, HashSet<BlockId>> {
    let mut result = HashMap::<RegisterId, HashSet<BlockId>>::default();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            for value in inst_uses(inst) {
                result.entry(value).or_default().insert(block.id);
            }
        }
        for value in terminator_uses(&block.terminator) {
            result.entry(value).or_default().insert(block.id);
        }
    }
    result
}

fn predecessor_info(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> (HashMap<BlockId, usize>, HashMap<BlockId, Vec<BlockId>>) {
    let mut pred_counts = HashMap::default();
    let mut jump_preds: HashMap<BlockId, Vec<BlockId>> = HashMap::default();
    for block in eu.blocks.values() {
        match &block.terminator {
            SIRTerminator::Jump(dst, _) => {
                *pred_counts.entry(*dst).or_default() += 1;
                jump_preds.entry(*dst).or_default().push(block.id);
            }
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                *pred_counts.entry(true_block.0).or_default() += 1;
                *pred_counts.entry(false_block.0).or_default() += 1;
            }
            SIRTerminator::Switch { cases, default, .. } => {
                for case in cases {
                    *pred_counts.entry(case.target).or_default() += 1;
                }
                *pred_counts.entry(*default).or_default() += 1;
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    for preds in jump_preds.values_mut() {
        preds.sort();
    }
    (pred_counts, jump_preds)
}

fn substitute_terminator(
    term: &SIRTerminator,
    map: &HashMap<RegisterId, RegisterId>,
) -> SIRTerminator {
    let replace = |reg: RegisterId| map.get(&reg).copied().unwrap_or(reg);
    match term {
        SIRTerminator::Jump(target, args) => {
            SIRTerminator::Jump(*target, args.iter().copied().map(replace).collect())
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => SIRTerminator::Branch {
            cond: replace(*cond),
            true_block: (
                true_block.0,
                true_block.1.iter().copied().map(replace).collect(),
            ),
            false_block: (
                false_block.0,
                false_block.1.iter().copied().map(replace).collect(),
            ),
        },
        SIRTerminator::Switch {
            selector,
            cases,
            default,
        } => SIRTerminator::Switch {
            selector: replace(*selector),
            cases: cases.clone(),
            default: *default,
        },
        SIRTerminator::Return => SIRTerminator::Return,
        SIRTerminator::Error(code) => SIRTerminator::Error(*code),
    }
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

fn block_use_count(block: &BasicBlock<RegionedAbsoluteAddr>, reg: RegisterId) -> usize {
    let inst_uses = block
        .instructions
        .iter()
        .map(|inst| {
            inst_uses(inst)
                .into_iter()
                .filter(|use_reg| *use_reg == reg)
                .count()
        })
        .sum::<usize>();
    let term_uses = terminator_uses(&block.terminator)
        .into_iter()
        .filter(|use_reg| *use_reg == reg)
        .count();
    inst_uses + term_uses
}

fn add_block_uses(
    counts: &mut HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
) {
    for inst in &block.instructions {
        for reg in inst_uses(inst) {
            *counts.entry(reg).or_default() += 1;
        }
    }
    for reg in terminator_uses(&block.terminator) {
        *counts.entry(reg).or_default() += 1;
    }
}

fn remove_block_uses(
    counts: &mut HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
) {
    for inst in &block.instructions {
        for reg in inst_uses(inst) {
            decrement_use(counts, reg);
        }
    }
    for reg in terminator_uses(&block.terminator) {
        decrement_use(counts, reg);
    }
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
    match inst {
        SIRInstruction::Imm(_, _) => Vec::new(),
        SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
        SIRInstruction::Unary(_, _, src) => vec![*src],
        SIRInstruction::Load(_, _, offset, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Store(_, offset, _, src, _, _) => offset
            .dynamic_registers()
            .into_iter()
            .flatten()
            .chain(std::iter::once(*src))
            .collect(),
        SIRInstruction::Commit(_, _, offset, _, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Concat(_, args) => args.clone(),
        SIRInstruction::Slice(_, src, _, _) => vec![*src],
        SIRInstruction::Mux(_, cond, true_val, false_val) => vec![*cond, *true_val, *false_val],
        SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => args.clone(),
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => vec![*old, *new],
    }
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
mod tests {
    use super::*;
    use crate::ir::{InstanceId, RegisterType, SIRValue};
    use celox_design::StateObjectId as VarId;
    use num_bigint::BigUint;

    fn addr(id: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(id),
            var_id: VarId::default(),
        }
    }

    fn unit(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        for reg in 0..26 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        );
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    fn imm(dst: usize, value: u64) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Imm(RegisterId(dst), SIRValue::new(BigUint::from(value)))
    }

    #[test]
    fn does_not_sever_cross_block_condition_prefix_from_local_user() {
        let definitions = vec![
            LocatedInstruction {
                block: BlockId(0),
                index: 0,
                instruction: imm(0, 1),
            },
            LocatedInstruction {
                block: BlockId(1),
                index: 0,
                instruction: SIRInstruction::Unary(
                    RegisterId(1),
                    crate::ir::UnaryOp::Ident,
                    RegisterId(0),
                ),
            },
        ];

        assert!(
            closed_cross_block_condition_slice(definitions, BlockId(1)).is_empty(),
            "a producer cannot move below the local condition node which still uses it"
        );
    }

    fn append_mul_chain(
        instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        initial: usize,
        factor: usize,
        outputs: &[usize],
    ) {
        let mut lhs = RegisterId(initial);
        for &output in outputs {
            instructions.push(SIRInstruction::Binary(
                RegisterId(output),
                lhs,
                crate::ir::BinaryOp::Mul,
                RegisterId(factor),
            ));
            lhs = RegisterId(output);
        }
    }

    fn cfg_unit(
        register_count: usize,
        one_bit_registers: &[usize],
        blocks: Vec<BasicBlock<RegionedAbsoluteAddr>>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let one_bit_registers = one_bit_registers.iter().copied().collect::<HashSet<_>>();
        let register_map = (0..register_count)
            .map(|register| {
                (
                    RegisterId(register),
                    RegisterType::Bit {
                        width: if one_bit_registers.contains(&register) {
                            1
                        } else {
                            64
                        },
                        signed: false,
                    },
                )
            })
            .collect();
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
            register_map,
        }
    }

    fn store(instance: usize, source: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            addr(instance),
            SIROffset::Static(0),
            64,
            RegisterId(source),
            Vec::new(),
            Vec::new(),
        )
    }

    fn profitability(
        true_arm_cost: u128,
        false_arm_cost: u128,
        phi_copy_cost: u128,
        live_through_cost: u128,
    ) -> BranchProfitability {
        profitability_with_probability(
            true_arm_cost,
            false_arm_cost,
            phi_copy_cost,
            live_through_cost,
            StaticBranchProbability::EVEN,
        )
    }

    fn profitability_with_probability(
        true_arm_cost: u128,
        false_arm_cost: u128,
        phi_copy_cost: u128,
        live_through_cost: u128,
        probability: StaticBranchProbability,
    ) -> BranchProfitability {
        BranchProfitability {
            true_arm_cost,
            false_arm_cost,
            removed_mux_cost: 1,
            probability,
            control_cost: BRANCH_CONTROL_COST,
            phi_copy_cost,
            live_through_cost,
        }
    }

    #[test]
    fn one_expensive_arm_must_pay_for_its_unselected_half() {
        // Expected savings: 24 / 2 + 1 = 13. Introduced cost: 11 + 2 = 13.
        // Equality is deliberately rejected because it does not prove a win.
        assert!(!profitability(24, 0, 2, 0).proves_expected_benefit());
    }

    #[test]
    fn work_on_both_arms_can_prove_expected_benefit() {
        // Expected savings: (20 + 20) / 2 + 1 = 21. Introduced cost: 13.
        assert!(profitability(20, 20, 2, 0).proves_expected_benefit());
    }

    #[test]
    fn live_through_cost_can_turn_a_candidate_into_a_rejection() {
        assert!(profitability(20, 10, 2, 0).proves_expected_benefit());
        // Expected savings and introduced cost are now both 16.
        assert!(!profitability(20, 10, 2, 3).proves_expected_benefit());
    }

    #[test]
    fn decoder_probability_can_prove_a_local_expected_win() {
        assert!(!profitability(10, 0, 0, 0).proves_expected_benefit());
        assert!(
            profitability_with_probability(
                10,
                0,
                0,
                0,
                StaticBranchProbability::EQUALITY_TO_CONSTANT,
            )
            .proves_expected_benefit()
        );
    }

    #[test]
    fn static_probability_tracks_constant_equality_and_inversion() {
        let eu = unit(vec![
            imm(1, 7),
            SIRInstruction::Unary(RegisterId(5), crate::ir::UnaryOp::Ident, RegisterId(1)),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(0),
                crate::ir::BinaryOp::EqWildcard,
                RegisterId(5),
            ),
            SIRInstruction::Unary(RegisterId(3), crate::ir::UnaryOp::LogicNot, RegisterId(2)),
            SIRInstruction::Binary(
                RegisterId(4),
                RegisterId(0),
                crate::ir::BinaryOp::Ne,
                RegisterId(1),
            ),
        ]);
        let block = &eu.blocks[&BlockId(0)];
        let def_pos = block
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(idx, inst)| def_reg(inst).map(|register| (register, idx)))
            .collect::<HashMap<_, _>>();

        assert_eq!(
            static_true_probability(block, &def_pos, RegisterId(2)),
            StaticBranchProbability::EQUALITY_TO_CONSTANT,
        );
        assert_eq!(
            static_true_probability(block, &def_pos, RegisterId(3)),
            StaticBranchProbability::EQUALITY_TO_CONSTANT.inverted(),
        );
        assert_eq!(
            static_true_probability(block, &def_pos, RegisterId(4)),
            StaticBranchProbability::EQUALITY_TO_CONSTANT.inverted(),
        );
        assert_eq!(
            static_true_probability(block, &def_pos, RegisterId(0)),
            StaticBranchProbability::EVEN,
        );
    }

    #[test]
    fn runtime_work_cost_scales_with_width_and_operation() {
        let mut register_map = HashMap::default();
        for register in [RegisterId(1), RegisterId(2)] {
            register_map.insert(
                register,
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mul = SIRInstruction::Binary(
            RegisterId(2),
            RegisterId(1),
            crate::ir::BinaryOp::Mul,
            RegisterId(1),
        );
        let div = SIRInstruction::Binary(
            RegisterId(2),
            RegisterId(1),
            crate::ir::BinaryOp::DivU,
            RegisterId(1),
        );
        assert_eq!(branchified_instruction_cost(&mul, &register_map), 5);
        assert_eq!(branchified_instruction_cost(&div, &register_map), 12);

        for register in [RegisterId(1), RegisterId(2)] {
            register_map.insert(
                register,
                RegisterType::Bit {
                    width: 128,
                    signed: false,
                },
            );
        }
        assert_eq!(branchified_instruction_cost(&mul, &register_map), 20);
        assert_eq!(branchified_instruction_cost(&div, &register_map), 48);
    }

    fn selector_predicate_unit(
        duplicate_last_selector: bool,
        add_store: bool,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut instructions = vec![imm(3, 0), imm(4, 1), imm(5, 2)];
        if duplicate_last_selector {
            instructions[2] = imm(5, 1);
        }
        instructions.extend([
            SIRInstruction::Binary(
                RegisterId(6),
                RegisterId(1),
                crate::ir::BinaryOp::Eq,
                RegisterId(3),
            ),
            SIRInstruction::Binary(
                RegisterId(7),
                RegisterId(1),
                crate::ir::BinaryOp::Eq,
                RegisterId(4),
            ),
            SIRInstruction::Binary(
                RegisterId(8),
                RegisterId(1),
                crate::ir::BinaryOp::Eq,
                RegisterId(5),
            ),
            SIRInstruction::Load(RegisterId(9), addr(10), SIROffset::Static(0), 64),
            SIRInstruction::Load(RegisterId(10), addr(11), SIROffset::Static(0), 64),
            SIRInstruction::Load(RegisterId(11), addr(12), SIROffset::Static(0), 64),
            SIRInstruction::Binary(
                RegisterId(15),
                RegisterId(9),
                crate::ir::BinaryOp::Eq,
                RegisterId(12),
            ),
            SIRInstruction::Binary(
                RegisterId(16),
                RegisterId(10),
                crate::ir::BinaryOp::Eq,
                RegisterId(13),
            ),
            SIRInstruction::Binary(
                RegisterId(17),
                RegisterId(11),
                crate::ir::BinaryOp::Eq,
                RegisterId(14),
            ),
            SIRInstruction::Binary(
                RegisterId(18),
                RegisterId(6),
                crate::ir::BinaryOp::LogicAnd,
                RegisterId(15),
            ),
            SIRInstruction::Binary(
                RegisterId(19),
                RegisterId(7),
                crate::ir::BinaryOp::LogicAnd,
                RegisterId(16),
            ),
            SIRInstruction::Binary(
                RegisterId(20),
                RegisterId(8),
                crate::ir::BinaryOp::LogicAnd,
                RegisterId(17),
            ),
            SIRInstruction::Binary(
                RegisterId(21),
                RegisterId(18),
                crate::ir::BinaryOp::LogicOr,
                RegisterId(19),
            ),
            SIRInstruction::Binary(
                RegisterId(22),
                RegisterId(21),
                crate::ir::BinaryOp::LogicOr,
                RegisterId(20),
            ),
            SIRInstruction::Binary(
                RegisterId(23),
                RegisterId(0),
                crate::ir::BinaryOp::LogicAnd,
                RegisterId(22),
            ),
            SIRInstruction::Unary(
                RegisterId(24),
                crate::ir::UnaryOp::ToTwoState,
                RegisterId(23),
            ),
        ]);
        if add_store {
            instructions.push(store(20, 12));
        }
        cfg_unit(
            25,
            &[0, 6, 7, 8, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24],
            vec![
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions,
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(24),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Return,
                },
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Return,
                },
            ],
        )
    }

    #[test]
    fn selector_disjoint_payload_loads_become_control_dependent() {
        let mut eu = selector_predicate_unit(false, false);
        let mut next_block_id = 3;
        let mut reg_counter = 24;

        assert_eq!(
            branchify_selector_guarded_predicates(&mut eu, &mut next_block_id, &mut reg_counter,),
            1
        );

        let head = &eu.blocks[&BlockId(0)];
        assert!(
            !head
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Load(..)))
        );
        let SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &head.terminator
        else {
            panic!("expected common guard branch");
        };
        assert_eq!(*cond, RegisterId(0));
        assert_eq!(true_block.0, BlockId(3));
        assert_eq!(false_block.0, BlockId(2));

        let mut decision = true_block.0;
        let mut payload_loads = Vec::new();
        for expected_selector in [RegisterId(6), RegisterId(7), RegisterId(8)] {
            let block = &eu.blocks[&decision];
            let SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } = &block.terminator
            else {
                panic!("expected selector decision");
            };
            assert_eq!(*cond, expected_selector);
            let payload = &eu.blocks[&true_block.0];
            payload_loads.extend(payload.instructions.iter().filter_map(|instruction| {
                let SIRInstruction::Load(dst, ..) = instruction else {
                    return None;
                };
                Some(*dst)
            }));
            decision = false_block.0;
        }
        assert_eq!(
            payload_loads,
            vec![RegisterId(9), RegisterId(10), RegisterId(11)]
        );
        assert_eq!(decision, BlockId(2));
    }

    #[test]
    fn selector_dispatch_rejects_overlapping_selector_values() {
        let mut eu = selector_predicate_unit(true, false);
        let mut next_block_id = 3;
        let mut reg_counter = 24;

        assert_eq!(
            branchify_selector_guarded_predicates(&mut eu, &mut next_block_id, &mut reg_counter,),
            0
        );
        assert_eq!(eu.blocks.len(), 3);
        assert_eq!(
            eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction, SIRInstruction::Load(..)))
                .count(),
            3
        );
    }

    #[test]
    fn selector_dispatch_normalizes_each_logic_condition() {
        let mut eu = selector_predicate_unit(false, false);
        for register in [0, 6, 7, 8, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24] {
            eu.register_map
                .insert(RegisterId(register), RegisterType::Logic { width: 1 });
        }
        let mut next_block_id = 3;
        let mut reg_counter = 24;

        assert_eq!(
            branchify_selector_guarded_predicates(&mut eu, &mut next_block_id, &mut reg_counter,),
            1
        );
        for block in eu.blocks.values() {
            let SIRTerminator::Branch { cond, .. } = block.terminator else {
                continue;
            };
            assert_eq!(
                eu.register_map[&cond],
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                }
            );
        }
    }

    #[test]
    fn selector_dispatch_does_not_delay_loads_across_a_store() {
        let mut eu = selector_predicate_unit(false, true);
        let mut next_block_id = 3;
        let mut reg_counter = 24;

        assert_eq!(
            branchify_selector_guarded_predicates(&mut eu, &mut next_block_id, &mut reg_counter,),
            0
        );
        assert_eq!(eu.blocks.len(), 3);
    }

    #[test]
    fn branchifies_single_use_mux_arm_work_when_expected_savings_pay_cost() {
        let mut eu = unit(vec![
            imm(1, 3),
            imm(4, 5),
            SIRInstruction::Binary(
                RegisterId(5),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Binary(
                RegisterId(6),
                RegisterId(5),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Binary(
                RegisterId(7),
                RegisterId(6),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(7),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        let head = &eu.blocks[&BlockId(0)];
        assert!(matches!(head.terminator, SIRTerminator::Branch { .. }));
        assert!(eu.blocks.values().any(|block| {
            block.params.is_empty() && matches!(block.terminator, SIRTerminator::Return)
        }));
        assert!(eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(2), _, _)))
        }));
        let SIRTerminator::Branch { false_block, .. } = &head.terminator else {
            panic!("expected mux to become branch");
        };
        assert!(false_block.1.is_empty());
        let false_block = &eu.blocks[&false_block.0];
        assert!(
            false_block.instructions.iter().any(|inst| {
                matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(4), _, _))
            })
        );
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(2), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(!head.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(2), _, crate::ir::BinaryOp::Mul, _)
            )
        }));
    }

    #[test]
    fn keeps_a_single_cheap_mul_arm_as_a_mux() {
        let mut eu = unit(vec![
            imm(1, 3),
            imm(4, 5),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 1);
        assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4))
            )
        }));
    }

    #[test]
    fn branchifies_a_decoder_biased_arm_with_expected_benefit() {
        let mut instructions = vec![
            imm(1, 3),
            imm(4, 5),
            imm(13, 7),
            SIRInstruction::Binary(
                RegisterId(14),
                RegisterId(0),
                crate::ir::BinaryOp::Eq,
                RegisterId(13),
            ),
        ];
        append_mul_chain(&mut instructions, 1, 1, &[5, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(14), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);
        let mut eu = unit(instructions);
        eu.register_map.insert(
            RegisterId(14),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch {
                cond: RegisterId(14),
                ..
            }
        ));
    }

    #[test]
    fn keeps_muxes_in_four_state_mode() {
        let mut instructions = vec![imm(1, 3), imm(4, 5)];
        append_mul_chain(&mut instructions, 1, 1, &[5, 6, 7, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);
        let mut eu = unit(instructions);
        let options = PassOptions {
            four_state: true,
            ..Default::default()
        };

        BranchifyMuxPass.run(&mut eu, &options);

        assert_eq!(eu.blocks.len(), 1);
        assert!(
            eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(3), _, _, _)))
        );
    }

    #[test]
    fn keeps_shared_mux_input_hoisted() {
        let mut eu = unit(vec![
            imm(1, 3),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(2)),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 1);
    }

    #[test]
    fn keeps_cheap_select_as_mux() {
        let mut eu = unit(vec![
            imm(1, 3),
            SIRInstruction::Unary(RegisterId(2), crate::ir::UnaryOp::BitNot, RegisterId(1)),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 1);
        assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4))
            )
        }));
    }

    #[test]
    fn branchifies_non_store_mux_with_arm_work() {
        let mut instructions = vec![imm(1, 3)];
        append_mul_chain(&mut instructions, 1, 1, &[8, 10, 2]);
        append_mul_chain(&mut instructions, 1, 1, &[9, 4]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Unary(RegisterId(5), crate::ir::UnaryOp::BitNot, RegisterId(3)),
        ]);
        let mut eu = unit(instructions);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 4);
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(3), _, _, _)))
        }));
        assert!(
            eu.blocks
                .values()
                .any(|block| block.params == vec![RegisterId(3)])
        );
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(2), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(4), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
    }

    #[test]
    fn does_not_branchify_mux_with_external_uses() {
        let mut eu = unit(vec![
            imm(1, 3),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
        ]);
        eu.blocks.get_mut(&BlockId(0)).unwrap().terminator =
            SIRTerminator::Jump(BlockId(1), Vec::new());
        eu.blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Unary(
                    RegisterId(5),
                    crate::ir::UnaryOp::BitNot,
                    RegisterId(3),
                )],
                terminator: SIRTerminator::Return,
            },
        );

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 2);
        assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4))
            )
        }));
    }

    #[test]
    fn removes_mux_at_cfg_controlled_join() {
        let mut register_map = HashMap::default();
        for reg in 0..8 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg == 0 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: vec![imm(1, 3), imm(2, 5)],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Binary(
                    RegisterId(3),
                    RegisterId(1),
                    crate::ir::BinaryOp::Mul,
                    RegisterId(1),
                )],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Binary(
                    RegisterId(4),
                    RegisterId(2),
                    crate::ir::BinaryOp::Mul,
                    RegisterId(2),
                )],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
        );
        blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Mux(RegisterId(5), RegisterId(0), RegisterId(3), RegisterId(4)),
                    SIRInstruction::Unary(RegisterId(6), crate::ir::UnaryOp::BitNot, RegisterId(5)),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert_eq!(eu.blocks[&BlockId(3)].params, vec![RegisterId(5)]);
        assert!(
            !eu.blocks[&BlockId(3)]
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Mux(RegisterId(5), ..)) })
        );
        assert!(matches!(
            &eu.blocks[&BlockId(1)].terminator,
            SIRTerminator::Jump(BlockId(3), args) if args == &vec![RegisterId(3)]
        ));
        assert!(matches!(
            &eu.blocks[&BlockId(2)].terminator,
            SIRTerminator::Jump(BlockId(3), args) if args == &vec![RegisterId(4)]
        ));
    }

    #[test]
    fn controlled_join_sinks_multiple_join_loads_to_the_selected_predecessor() {
        let element = |index| SIROffset::Element {
            index: RegisterId(index),
            element_width: 64,
            bit_offset: 0,
            dynamic_bit_offset: None,
        };
        let blocks = vec![
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2), RegisterId(3)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Load(RegisterId(4), addr(0), element(1), 64),
                    SIRInstruction::Load(RegisterId(6), addr(1), element(1), 64),
                    SIRInstruction::Mux(RegisterId(5), RegisterId(0), RegisterId(4), RegisterId(2)),
                    SIRInstruction::Mux(RegisterId(7), RegisterId(0), RegisterId(6), RegisterId(3)),
                    SIRInstruction::Concat(RegisterId(8), vec![RegisterId(5), RegisterId(7)]),
                ],
                terminator: SIRTerminator::Return,
            },
        ];
        let mut eu = cfg_unit(9, &[0], blocks);
        eu.register_map.insert(
            RegisterId(8),
            RegisterType::Bit {
                width: 128,
                signed: false,
            },
        );

        eliminate_controlled_join_muxes(&mut eu, None);

        assert_eq!(eu.verify_result(), Ok(()));
        assert_eq!(
            eu.blocks[&BlockId(3)].params,
            vec![RegisterId(5), RegisterId(7)]
        );
        assert!(
            eu.blocks[&BlockId(3)]
                .instructions
                .iter()
                .all(|instruction| !matches!(
                    instruction,
                    SIRInstruction::Load(..) | SIRInstruction::Mux(..)
                ))
        );
        assert!(matches!(
            &eu.blocks[&BlockId(1)].instructions[..],
            [
                SIRInstruction::Load(RegisterId(4), ..),
                SIRInstruction::Load(RegisterId(6), ..)
            ]
        ));
        assert!(eu.blocks[&BlockId(2)].instructions.is_empty());
        assert!(matches!(
            &eu.blocks[&BlockId(1)].terminator,
            SIRTerminator::Jump(BlockId(3), args)
                if args == &vec![RegisterId(4), RegisterId(6)]
        ));
        assert!(matches!(
            &eu.blocks[&BlockId(2)].terminator,
            SIRTerminator::Jump(BlockId(3), args)
                if args == &vec![RegisterId(2), RegisterId(3)]
        ));
    }

    #[test]
    fn controlled_join_does_not_move_a_load_before_a_join_write() {
        let blocks = vec![
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![
                    store(0, 1),
                    SIRInstruction::Load(RegisterId(3), addr(0), SIROffset::Static(0), 64),
                    SIRInstruction::Mux(RegisterId(4), RegisterId(0), RegisterId(3), RegisterId(2)),
                    store(1, 4),
                ],
                terminator: SIRTerminator::Return,
            },
        ];
        let mut eu = cfg_unit(5, &[0], blocks);

        eliminate_controlled_join_muxes(&mut eu, None);

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(eu.blocks[&BlockId(1)].instructions.is_empty());
        assert!(
            eu.blocks[&BlockId(3)]
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(
                        instruction,
                        SIRInstruction::Mux(
                            RegisterId(4),
                            RegisterId(0),
                            RegisterId(3),
                            RegisterId(2)
                        )
                    )
                })
        );
    }

    #[test]
    fn uses_per_edge_path_facts_for_reconvergent_mux() {
        let mut register_map = HashMap::default();
        for reg in 0..6 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg < 2 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: vec![imm(2, 3), imm(3, 5)],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(1),
                    true_block: (BlockId(3), Vec::new()),
                    false_block: (BlockId(4), Vec::new()),
                },
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(1),
                    true_block: (BlockId(3), Vec::new()),
                    false_block: (BlockId(5), Vec::new()),
                },
            },
        );
        blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Mux(
                    RegisterId(4),
                    RegisterId(1),
                    RegisterId(2),
                    RegisterId(3),
                )],
                terminator: SIRTerminator::Return,
            },
        );
        for block_id in [BlockId(4), BlockId(5)] {
            blocks.insert(
                block_id,
                BasicBlock {
                    id: block_id,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Return,
                },
            );
        }
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert_eq!(eu.blocks[&BlockId(3)].params, vec![RegisterId(4)]);
        assert!(
            !eu.blocks[&BlockId(3)]
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Mux(RegisterId(4), ..)) })
        );
        assert!(matches!(
            &eu.blocks[&BlockId(1)].terminator,
            SIRTerminator::Branch { true_block: (_, args), .. } if args == &vec![RegisterId(2)]
        ));
        assert!(matches!(
            &eu.blocks[&BlockId(2)].terminator,
            SIRTerminator::Branch { true_block: (_, args), .. } if args == &vec![RegisterId(2)]
        ));
    }

    #[test]
    fn repeated_predicate_sinks_the_join_value_to_the_actual_selected_edge() {
        // `b0` and `b2` branch on the same SSA predicate.  Structurally, b2
        // dominates b3, so an ancestor-only arm classification would label
        // b3 as b0's false arm.  But b3 is reached on b2's true edge, where
        // the Mux must select r6.  The join-local definition of r6 must move
        // to b3, never to the infeasible ancestor classification.
        let mut register_map = HashMap::default();
        for reg in 0..8 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg == 0 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(2), Vec::new()),
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(3), Vec::new()),
                    false_block: (BlockId(4), Vec::new()),
                },
            },
        );
        for block_id in [BlockId(3), BlockId(4)] {
            blocks.insert(
                block_id,
                BasicBlock {
                    id: block_id,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(5), Vec::new()),
                },
            );
        }
        blocks.insert(
            BlockId(5),
            BasicBlock {
                id: BlockId(5),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Unary(RegisterId(6), crate::ir::UnaryOp::BitNot, RegisterId(1)),
                    SIRInstruction::Mux(RegisterId(7), RegisterId(0), RegisterId(6), RegisterId(1)),
                    SIRInstruction::Store(
                        addr(0),
                        SIROffset::Static(0),
                        64,
                        RegisterId(7),
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        eliminate_controlled_join_muxes(&mut eu, None);

        assert_eq!(eu.verify_result(), Ok(()));
        assert_eq!(eu.blocks[&BlockId(5)].params, vec![RegisterId(7)]);
        assert!(
            !eu.blocks[&BlockId(5)]
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(instruction, SIRInstruction::Mux(RegisterId(7), ..))
                })
        );
        assert!(
            eu.blocks[&BlockId(3)]
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(
                        instruction,
                        SIRInstruction::Unary(
                            RegisterId(6),
                            crate::ir::UnaryOp::BitNot,
                            RegisterId(1)
                        )
                    )
                })
        );
        assert!(eu.blocks[&BlockId(4)].instructions.is_empty());
        assert!(matches!(
            &eu.blocks[&BlockId(3)].terminator,
            SIRTerminator::Jump(BlockId(5), args) if args == &vec![RegisterId(6)]
        ));
        assert!(matches!(
            &eu.blocks[&BlockId(4)].terminator,
            SIRTerminator::Jump(BlockId(5), args) if args == &vec![RegisterId(1)]
        ));
    }

    #[test]
    fn short_circuits_a_cross_block_priority_chain() {
        let mut register_map = HashMap::default();
        for reg in 0..21 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if matches!(reg, 6 | 8 | 10) { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![
                    RegisterId(0),
                    RegisterId(1),
                    RegisterId(2),
                    RegisterId(3),
                    RegisterId(12),
                ],
                instructions: vec![
                    imm(5, 1),
                    SIRInstruction::Binary(
                        RegisterId(6),
                        RegisterId(0),
                        crate::ir::BinaryOp::Eq,
                        RegisterId(5),
                    ),
                    imm(7, 2),
                    SIRInstruction::Binary(
                        RegisterId(8),
                        RegisterId(0),
                        crate::ir::BinaryOp::Eq,
                        RegisterId(7),
                    ),
                    imm(9, 3),
                    SIRInstruction::Binary(
                        RegisterId(10),
                        RegisterId(0),
                        crate::ir::BinaryOp::Eq,
                        RegisterId(9),
                    ),
                ],
                terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Unary(
                        RegisterId(17),
                        crate::ir::UnaryOp::BitNot,
                        RegisterId(12),
                    ),
                    SIRInstruction::Unary(
                        RegisterId(18),
                        crate::ir::UnaryOp::BitNot,
                        RegisterId(1),
                    ),
                    SIRInstruction::Unary(
                        RegisterId(19),
                        crate::ir::UnaryOp::BitNot,
                        RegisterId(2),
                    ),
                    SIRInstruction::Unary(
                        RegisterId(20),
                        crate::ir::UnaryOp::BitNot,
                        RegisterId(3),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(13),
                        RegisterId(6),
                        RegisterId(18),
                        RegisterId(17),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(14),
                        RegisterId(8),
                        RegisterId(19),
                        RegisterId(13),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(15),
                        RegisterId(10),
                        RegisterId(20),
                        RegisterId(14),
                    ),
                    SIRInstruction::Unary(
                        RegisterId(16),
                        crate::ir::UnaryOp::BitNot,
                        RegisterId(15),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
        }));
        assert!(eu.blocks.values().any(|block| {
            matches!(block.terminator, SIRTerminator::Branch { .. })
                && block
                    .instructions
                    .iter()
                    .any(|inst| matches!(inst, SIRInstruction::Binary(RegisterId(10), ..)))
        }));
        assert!(!eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(6) | RegisterId(8) | RegisterId(10), ..)
            )
        }));
        let cfg = SirCfg::analyze(&eu).unwrap();
        for payload in 17..=20 {
            let payload_block = eu
                .blocks
                .values()
                .find(|block| {
                    block
                        .instructions
                        .iter()
                        .any(|instruction| def_reg(instruction) == Some(RegisterId(payload)))
                })
                .expect("each selected payload must retain one definition");
            assert!(matches!(payload_block.terminator, SIRTerminator::Jump(..)));
            assert!(
                !cfg.controllers[cfg.block_index(payload_block.id).unwrap()].is_empty(),
                "payload r{payload} must execute only below its selector edge"
            );
        }
    }

    #[test]
    fn atomic_priority_does_not_charge_preexisting_suffix_live_ranges() {
        let mut register_map = HashMap::default();
        for reg in 0..60 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mut instructions = vec![imm(7, 3)];
        append_mul_chain(&mut instructions, 6, 7, &[8, 9, 10, 11, 12, 13, 14, 15]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(16), RegisterId(2), RegisterId(5), RegisterId(15)),
            SIRInstruction::Mux(RegisterId(17), RegisterId(1), RegisterId(4), RegisterId(16)),
            SIRInstruction::Mux(RegisterId(18), RegisterId(0), RegisterId(3), RegisterId(17)),
            SIRInstruction::Unary(RegisterId(19), crate::ir::UnaryOp::BitNot, RegisterId(18)),
        ]);
        // These values are live from entry to the suffix both before and
        // after priority-region formation.  They must not be treated as
        // newly introduced live-through pressure.
        instructions.extend((20..60).map(|register| {
            SIRInstruction::Store(
                addr(register),
                SIROffset::Static(0),
                64,
                RegisterId(register),
                Vec::new(),
                Vec::new(),
            )
        }));
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: (0..=6).chain(20..60).map(RegisterId).collect(),
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map,
        };

        let placement = PlacementAnalysis::analyze(&eu).unwrap();
        let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
        assert_eq!(plan.regions.len(), 1);
        assert_eq!(plan.regions[0].muxes.len(), 3);
        let mut next_block_id = 1;
        let mut reg_counter = 19;
        assert_eq!(
            apply_atomic_priority_placement(&mut eu, plan, &mut next_block_id, &mut reg_counter,),
            1
        );

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
        }));
        assert_eq!(
            eu.blocks
                .values()
                .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
                .count(),
            3
        );
        assert!(
            !eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(
                        instruction,
                        SIRInstruction::Binary(_, _, crate::ir::BinaryOp::Mul, _)
                    )
                })
        );
    }

    #[test]
    fn branchifies_coupled_state_updates_with_interleaved_conditions() {
        let mut eu = cfg_unit(
            23,
            &[2, 4, 6, 8, 9, 11, 12, 14, 15],
            vec![BasicBlock {
                id: BlockId(0),
                params: (0..=7).map(RegisterId).collect(),
                instructions: vec![
                    SIRInstruction::Binary(
                        RegisterId(8),
                        RegisterId(3),
                        crate::ir::BinaryOp::GtU,
                        RegisterId(0),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(9),
                        RegisterId(2),
                        crate::ir::BinaryOp::LogicAnd,
                        RegisterId(8),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(10),
                        RegisterId(9),
                        RegisterId(3),
                        RegisterId(0),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(11),
                        RegisterId(5),
                        crate::ir::BinaryOp::GtU,
                        RegisterId(10),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(12),
                        RegisterId(4),
                        crate::ir::BinaryOp::LogicAnd,
                        RegisterId(11),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(13),
                        RegisterId(12),
                        RegisterId(5),
                        RegisterId(10),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(14),
                        RegisterId(7),
                        crate::ir::BinaryOp::GtU,
                        RegisterId(13),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(15),
                        RegisterId(6),
                        crate::ir::BinaryOp::LogicAnd,
                        RegisterId(14),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(16),
                        RegisterId(15),
                        RegisterId(7),
                        RegisterId(13),
                    ),
                    imm(17, 1),
                    imm(18, 2),
                    imm(19, 3),
                    SIRInstruction::Mux(
                        RegisterId(20),
                        RegisterId(9),
                        RegisterId(17),
                        RegisterId(1),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(21),
                        RegisterId(12),
                        RegisterId(18),
                        RegisterId(20),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(22),
                        RegisterId(15),
                        RegisterId(19),
                        RegisterId(21),
                    ),
                    store(0, 16),
                    store(1, 22),
                ],
                terminator: SIRTerminator::Return,
            }],
        );

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
        }));
        assert_eq!(
            eu.blocks
                .values()
                .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
                .count(),
            6
        );
        for (guard, delayed) in [(2, 8), (4, 11), (6, 14)] {
            let guard_block = eu
                .blocks
                .values()
                .find(|block| {
                    matches!(
                        block.terminator,
                        SIRTerminator::Branch {
                            cond,
                            ..
                        } if cond == RegisterId(guard)
                    )
                })
                .expect("eligibility guard must become the first branch");
            let delayed_block_id = match &guard_block.terminator {
                SIRTerminator::Branch { true_block, .. } => true_block.0,
                _ => unreachable!(),
            };
            let delayed_block = &eu.blocks[&delayed_block_id];
            assert!(delayed_block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Binary(dst, _, crate::ir::BinaryOp::GtU, _)
                        if *dst == RegisterId(delayed)
                )
            }));
            assert!(matches!(
                delayed_block.terminator,
                SIRTerminator::Branch {
                    cond,
                    ..
                } if cond == RegisterId(delayed)
            ));
        }
        for outputs in [
            [RegisterId(10), RegisterId(20)],
            [RegisterId(13), RegisterId(21)],
            [RegisterId(16), RegisterId(22)],
        ] {
            assert!(eu.blocks.values().any(|block| block.params == outputs));
        }
    }

    #[test]
    fn branchifies_a_coupled_priority_chain_outermost_first() {
        let mut eu = cfg_unit(
            10,
            &[0, 1],
            vec![BasicBlock {
                id: BlockId(0),
                params: (0..=5).map(RegisterId).collect(),
                instructions: vec![
                    SIRInstruction::Mux(RegisterId(6), RegisterId(0), RegisterId(4), RegisterId(2)),
                    SIRInstruction::Mux(RegisterId(7), RegisterId(1), RegisterId(4), RegisterId(6)),
                    store(0, 7),
                    SIRInstruction::Mux(RegisterId(8), RegisterId(0), RegisterId(5), RegisterId(3)),
                    SIRInstruction::Mux(RegisterId(9), RegisterId(1), RegisterId(5), RegisterId(8)),
                    store(1, 9),
                ],
                terminator: SIRTerminator::Return,
            }],
        );

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch {
                cond: RegisterId(1),
                ..
            }
        ));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
        }));
        assert!(
            eu.blocks
                .values()
                .any(|block| block.params == [RegisterId(7), RegisterId(9)])
        );
    }

    #[test]
    fn atomic_priority_moves_cross_block_occurrences_with_valid_state_versions() {
        let mut register_map = HashMap::default();
        for reg in 0..24 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if matches!(reg, 4 | 6) { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut source = vec![
            imm(3, 1),
            SIRInstruction::Binary(
                RegisterId(4),
                RegisterId(0),
                crate::ir::BinaryOp::Eq,
                RegisterId(3),
            ),
            imm(5, 2),
            SIRInstruction::Binary(
                RegisterId(6),
                RegisterId(0),
                crate::ir::BinaryOp::Eq,
                RegisterId(5),
            ),
            SIRInstruction::Load(RegisterId(7), addr(0), SIROffset::Static(0), 64),
            imm(8, 3),
        ];
        append_mul_chain(
            &mut source,
            7,
            8,
            &[9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20],
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                (
                    BlockId(0),
                    BasicBlock {
                        id: BlockId(0),
                        params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                        instructions: source,
                        terminator: SIRTerminator::Jump(BlockId(1), vec![]),
                    },
                ),
                (
                    BlockId(1),
                    BasicBlock {
                        id: BlockId(1),
                        params: vec![],
                        instructions: vec![
                            SIRInstruction::Mux(
                                RegisterId(21),
                                RegisterId(4),
                                RegisterId(20),
                                RegisterId(1),
                            ),
                            SIRInstruction::Mux(
                                RegisterId(22),
                                RegisterId(6),
                                RegisterId(2),
                                RegisterId(21),
                            ),
                            SIRInstruction::Unary(
                                RegisterId(23),
                                crate::ir::UnaryOp::BitNot,
                                RegisterId(22),
                            ),
                        ],
                        terminator: SIRTerminator::Return,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            register_map,
        };

        let placement = PlacementAnalysis::analyze(&eu).unwrap();
        let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
        assert_eq!(plan.regions.len(), 1);
        let moved_load = plan.regions[0]
            .placed
            .iter()
            .find(|placed| def_reg(&placed.instruction) == Some(RegisterId(7)))
            .unwrap();
        assert_eq!(moved_load.block, BlockId(0));
        assert_eq!(moved_load.site, PriorityPlacementSite::Leaf(1));
        let moved_inner_condition = plan.regions[0]
            .placed
            .iter()
            .find(|placed| def_reg(&placed.instruction) == Some(RegisterId(4)))
            .unwrap();
        assert_eq!(
            moved_inner_condition.site,
            PriorityPlacementSite::Decision(0)
        );

        let mut next_block_id = 2;
        let mut reg_counter = 23;
        assert_eq!(
            apply_atomic_priority_placement(&mut eu, plan, &mut next_block_id, &mut reg_counter),
            1
        );
        assert_eq!(eu.verify_result(), Ok(()));
        assert!(
            !eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(instruction, SIRInstruction::Load(RegisterId(7), ..))
                        || def_reg(instruction) == Some(RegisterId(4))
                })
        );
        let load_block = eu
            .blocks
            .values()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction, SIRInstruction::Load(RegisterId(7), ..))
                })
            })
            .unwrap();
        let cfg = SirCfg::analyze(&eu).unwrap();
        assert!(!cfg.controllers[cfg.block_index(load_block.id).unwrap()].is_empty());
        let condition_block = eu
            .blocks
            .values()
            .find(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| def_reg(instruction) == Some(RegisterId(4)))
            })
            .unwrap();
        assert!(matches!(
            condition_block.terminator,
            SIRTerminator::Branch {
                cond: RegisterId(4),
                ..
            }
        ));
    }

    #[test]
    fn whole_priority_places_a_shared_descendant_once_at_its_lca() {
        let mut register_map = HashMap::default();
        for reg in 0..48 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg <= 2 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut instructions = vec![imm(3, 3), imm(4, 5), imm(5, 7), imm(6, 11)];
        let shared_outputs = (7..=38).collect::<Vec<_>>();
        append_mul_chain(&mut instructions, 3, 4, &shared_outputs);
        instructions.extend([
            SIRInstruction::Binary(
                RegisterId(39),
                RegisterId(38),
                crate::ir::BinaryOp::Add,
                RegisterId(5),
            ),
            SIRInstruction::Binary(
                RegisterId(40),
                RegisterId(38),
                crate::ir::BinaryOp::Sub,
                RegisterId(6),
            ),
            SIRInstruction::Mux(RegisterId(41), RegisterId(2), RegisterId(39), RegisterId(3)),
            SIRInstruction::Mux(
                RegisterId(42),
                RegisterId(1),
                RegisterId(40),
                RegisterId(41),
            ),
            SIRInstruction::Mux(RegisterId(43), RegisterId(0), RegisterId(4), RegisterId(42)),
            SIRInstruction::Unary(RegisterId(44), crate::ir::UnaryOp::BitNot, RegisterId(43)),
        ]);
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map,
        };

        let placement = PlacementAnalysis::analyze(&eu).unwrap();
        let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
        let shared = plan.regions[0]
            .placed
            .iter()
            .find(|placed| def_reg(&placed.instruction) == Some(RegisterId(38)))
            .unwrap();
        assert_eq!(shared.site, PriorityPlacementSite::Decision(1));

        let mut next_block_id = 1;
        let mut reg_counter = 47;
        apply_atomic_priority_placement(&mut eu, plan, &mut next_block_id, &mut reg_counter);
        assert_eq!(eu.verify_result(), Ok(()));
        let definition_blocks = eu
            .blocks
            .values()
            .filter(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| def_reg(instruction) == Some(RegisterId(38)))
            })
            .collect::<Vec<_>>();
        assert_eq!(definition_blocks.len(), 1);
        assert!(matches!(
            definition_blocks[0].terminator,
            SIRTerminator::Branch {
                cond: RegisterId(1),
                ..
            }
        ));
    }

    #[test]
    fn whole_priority_pins_a_definition_with_an_external_use() {
        let mut register_map = HashMap::default();
        for reg in 0..40 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg <= 2 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut instructions = vec![imm(3, 3), imm(4, 5), imm(5, 7), imm(6, 11)];
        append_mul_chain(&mut instructions, 3, 4, &(7..=18).collect::<Vec<_>>());
        append_mul_chain(&mut instructions, 5, 6, &(19..=30).collect::<Vec<_>>());
        instructions.extend([
            SIRInstruction::Mux(
                RegisterId(31),
                RegisterId(2),
                RegisterId(30),
                RegisterId(18),
            ),
            SIRInstruction::Mux(RegisterId(32), RegisterId(1), RegisterId(4), RegisterId(31)),
            SIRInstruction::Mux(RegisterId(33), RegisterId(0), RegisterId(3), RegisterId(32)),
            SIRInstruction::Binary(
                RegisterId(34),
                RegisterId(33),
                crate::ir::BinaryOp::Add,
                RegisterId(18),
            ),
        ]);
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map,
        };

        let placement = PlacementAnalysis::analyze(&eu).unwrap();
        let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
        assert!(
            !plan.regions[0]
                .placed
                .iter()
                .any(|placed| def_reg(&placed.instruction) == Some(RegisterId(18)))
        );

        let mut next_block_id = 1;
        let mut reg_counter = 39;
        apply_atomic_priority_placement(&mut eu, plan, &mut next_block_id, &mut reg_counter);
        assert_eq!(eu.verify_result(), Ok(()));
        assert!(
            eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| def_reg(instruction) == Some(RegisterId(18)))
        );
    }

    #[test]
    fn whole_priority_delays_an_inner_condition_dag_until_fallthrough() {
        let mut register_map = HashMap::default();
        for reg in 0..30 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg <= 1 || reg == 22 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut instructions = vec![imm(2, 3), imm(3, 5), imm(4, 7), imm(5, 11)];
        append_mul_chain(&mut instructions, 2, 3, &(6..=21).collect::<Vec<_>>());
        instructions.extend([
            SIRInstruction::Binary(
                RegisterId(22),
                RegisterId(21),
                crate::ir::BinaryOp::Eq,
                RegisterId(4),
            ),
            SIRInstruction::Mux(RegisterId(23), RegisterId(22), RegisterId(4), RegisterId(5)),
            SIRInstruction::Mux(RegisterId(24), RegisterId(1), RegisterId(2), RegisterId(23)),
            SIRInstruction::Mux(RegisterId(25), RegisterId(0), RegisterId(3), RegisterId(24)),
            SIRInstruction::Unary(RegisterId(26), crate::ir::UnaryOp::BitNot, RegisterId(25)),
        ]);
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1)],
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map,
        };

        let placement = PlacementAnalysis::analyze(&eu).unwrap();
        let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
        let condition = plan.regions[0]
            .placed
            .iter()
            .find(|placed| def_reg(&placed.instruction) == Some(RegisterId(22)))
            .unwrap();
        assert_eq!(condition.site, PriorityPlacementSite::Decision(0));

        let mut next_block_id = 1;
        let mut reg_counter = 29;
        apply_atomic_priority_placement(&mut eu, plan, &mut next_block_id, &mut reg_counter);
        assert_eq!(eu.verify_result(), Ok(()));
        let condition_block = eu
            .blocks
            .values()
            .find(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| def_reg(instruction) == Some(RegisterId(22)))
            })
            .unwrap();
        assert_ne!(condition_block.id, BlockId(0));
        assert!(matches!(
            condition_block.terminator,
            SIRTerminator::Branch {
                cond: RegisterId(22),
                ..
            }
        ));
        let middle_block = eu
            .blocks
            .values()
            .find(|block| {
                matches!(
                    block.terminator,
                    SIRTerminator::Branch {
                        cond: RegisterId(1),
                        ..
                    }
                )
            })
            .unwrap();
        let SIRTerminator::Branch {
            false_block: middle_fallthrough,
            ..
        } = &middle_block.terminator
        else {
            unreachable!()
        };
        assert_eq!(middle_fallthrough.0, condition_block.id);
        let SIRTerminator::Branch {
            cond: RegisterId(0),
            false_block: outer_fallthrough,
            ..
        } = &eu.blocks[&BlockId(0)].terminator
        else {
            panic!("expected the outer priority decision")
        };
        assert_eq!(outer_fallthrough.0, middle_block.id);
    }

    #[test]
    fn moves_pure_arm_dags_from_dominating_blocks() {
        let mut register_map = HashMap::default();
        for reg in 0..26 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg == 0 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        let mut preheader_insts = vec![imm(1, 3), imm(2, 5)];
        append_mul_chain(
            &mut preheader_insts,
            1,
            1,
            &[3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        );
        append_mul_chain(
            &mut preheader_insts,
            2,
            2,
            &[13, 14, 15, 16, 17, 18, 19, 20, 21, 22],
        );
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: preheader_insts,
                terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Mux(
                        RegisterId(23),
                        RegisterId(0),
                        RegisterId(12),
                        RegisterId(22),
                    ),
                    SIRInstruction::Unary(
                        RegisterId(24),
                        crate::ir::UnaryOp::BitNot,
                        RegisterId(23),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(23), ..)))
        }));
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(12), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(22), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(
            eu.blocks
                .values()
                .any(|block| { block.params == vec![RegisterId(23)] })
        );
        assert!(!eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(
                    RegisterId(12) | RegisterId(22),
                    _,
                    crate::ir::BinaryOp::Mul,
                    _
                )
            )
        }));
    }

    #[test]
    fn branches_once_for_multiple_muxes_sharing_an_arm_dag() {
        let mut register_map = HashMap::default();
        for reg in 0..26 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg == 0 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        let mut preheader_insts = vec![imm(1, 3), imm(2, 5)];
        append_mul_chain(
            &mut preheader_insts,
            1,
            1,
            &[3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        );
        append_mul_chain(
            &mut preheader_insts,
            2,
            2,
            &[13, 14, 15, 16, 17, 18, 19, 20, 21, 22],
        );
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: preheader_insts,
                terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Mux(
                        RegisterId(23),
                        RegisterId(0),
                        RegisterId(12),
                        RegisterId(22),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(24),
                        RegisterId(0),
                        RegisterId(12),
                        RegisterId(22),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(25),
                        RegisterId(23),
                        crate::ir::BinaryOp::Add,
                        RegisterId(24),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(!eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Mux(RegisterId(23) | RegisterId(24), ..)
                )
            })
        }));
        assert!(
            eu.blocks
                .values()
                .any(|block| { block.params == vec![RegisterId(23), RegisterId(24)] })
        );
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(12), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(22), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(!eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(
                    RegisterId(12) | RegisterId(22),
                    _,
                    crate::ir::BinaryOp::Mul,
                    _
                )
            )
        }));
    }

    #[test]
    fn existing_cfg_moves_a_state_read_only_with_memoryssa_proof() {
        let mut register_map = HashMap::default();
        for reg in 0..17 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg == 0 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut source = vec![
            SIRInstruction::Load(RegisterId(1), addr(0), SIROffset::Static(0), 64),
            imm(2, 3),
            imm(13, 5),
        ];
        append_mul_chain(&mut source, 1, 2, &[3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: source,
                terminator: SIRTerminator::Jump(BlockId(1), vec![]),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Mux(
                        RegisterId(14),
                        RegisterId(0),
                        RegisterId(12),
                        RegisterId(13),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(15),
                        RegisterId(0),
                        RegisterId(12),
                        RegisterId(13),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(16),
                        RegisterId(14),
                        crate::ir::BinaryOp::Add,
                        RegisterId(15),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        let load_blocks = eu
            .blocks
            .values()
            .filter(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction, SIRInstruction::Load(RegisterId(1), ..))
                })
            })
            .map(|block| block.id)
            .collect::<Vec<_>>();
        assert_eq!(load_blocks.len(), 1);
        let cfg = SirCfg::analyze(&eu).unwrap();
        let load_block = cfg.block_index(load_blocks[0]).unwrap();
        assert!(
            !cfg.controllers[load_block].is_empty(),
            "an unchanged versioned state read should execute only in its selected arm"
        );
    }

    #[test]
    fn atomic_placement_keeps_a_load_before_a_reaching_write() {
        let mut register_map = HashMap::default();
        for reg in 0..17 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg == 0 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut source = vec![
            SIRInstruction::Load(RegisterId(1), addr(0), SIROffset::Static(0), 64),
            imm(2, 3),
            imm(13, 5),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(13),
                vec![],
                vec![],
            ),
        ];
        append_mul_chain(&mut source, 1, 2, &[3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: source,
                terminator: SIRTerminator::Jump(BlockId(1), vec![]),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Mux(
                        RegisterId(14),
                        RegisterId(0),
                        RegisterId(12),
                        RegisterId(13),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(15),
                        RegisterId(0),
                        RegisterId(12),
                        RegisterId(13),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(16),
                        RegisterId(14),
                        crate::ir::BinaryOp::Add,
                        RegisterId(15),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        let placement = PlacementAnalysis::analyze(&eu).unwrap();
        if let Some(plan) = find_atomic_priority_placement(&eu, &placement) {
            assert!(
                !plan
                    .regions
                    .iter()
                    .flat_map(|region| &region.placed)
                    .any(|placed| matches!(
                        placed.instruction,
                        SIRInstruction::Load(RegisterId(1), ..)
                    ))
            );
        }

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(
            eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(instruction, SIRInstruction::Load(RegisterId(1), ..))
                })
        );
    }

    #[test]
    fn one_atomic_plan_selects_disjoint_regions_bottom_up() {
        let mut register_map = HashMap::default();
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(1),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        for reg in 2..32 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mut true_instructions = vec![imm(2, 3), imm(3, 5), imm(12, 7)];
        append_mul_chain(&mut true_instructions, 2, 3, &[4, 5, 6, 7, 8, 9, 10, 11]);
        true_instructions.extend([
            SIRInstruction::Mux(
                RegisterId(13),
                RegisterId(1),
                RegisterId(11),
                RegisterId(12),
            ),
            SIRInstruction::Mux(
                RegisterId(14),
                RegisterId(1),
                RegisterId(12),
                RegisterId(13),
            ),
            SIRInstruction::Binary(
                RegisterId(15),
                RegisterId(12),
                crate::ir::BinaryOp::Add,
                RegisterId(14),
            ),
        ]);
        let mut false_instructions = vec![imm(16, 11), imm(17, 13), imm(26, 17)];
        append_mul_chain(
            &mut false_instructions,
            16,
            17,
            &[18, 19, 20, 21, 22, 23, 24, 25],
        );
        false_instructions.extend([
            SIRInstruction::Mux(
                RegisterId(27),
                RegisterId(1),
                RegisterId(25),
                RegisterId(26),
            ),
            SIRInstruction::Mux(
                RegisterId(28),
                RegisterId(1),
                RegisterId(26),
                RegisterId(27),
            ),
            SIRInstruction::Binary(
                RegisterId(29),
                RegisterId(26),
                crate::ir::BinaryOp::Add,
                RegisterId(28),
            ),
        ]);
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                (
                    BlockId(0),
                    BasicBlock {
                        id: BlockId(0),
                        params: vec![RegisterId(0), RegisterId(1)],
                        instructions: vec![],
                        terminator: SIRTerminator::Branch {
                            cond: RegisterId(0),
                            true_block: (BlockId(1), vec![]),
                            false_block: (BlockId(2), vec![]),
                        },
                    },
                ),
                (
                    BlockId(1),
                    BasicBlock {
                        id: BlockId(1),
                        params: vec![],
                        instructions: true_instructions,
                        terminator: SIRTerminator::Return,
                    },
                ),
                (
                    BlockId(2),
                    BasicBlock {
                        id: BlockId(2),
                        params: vec![],
                        instructions: false_instructions,
                        terminator: SIRTerminator::Return,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            register_map,
        };
        let placement = PlacementAnalysis::analyze(&eu).unwrap();
        let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
        let mut heads = plan
            .regions
            .iter()
            .map(|region| region.block_id)
            .collect::<Vec<_>>();
        heads.sort_unstable();

        assert_eq!(heads, vec![BlockId(1), BlockId(2)]);
    }

    #[test]
    fn atomic_apply_handles_disjoint_regions_sharing_a_definition_block() {
        let mut register_map = HashMap::default();
        for reg in 0..30 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg <= 2 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut source = vec![imm(4, 3), imm(5, 5)];
        append_mul_chain(&mut source, 4, 5, &[6, 7, 8, 9, 10, 11, 12, 13]);
        source.extend([imm(14, 7), imm(15, 11)]);
        append_mul_chain(&mut source, 14, 15, &[16, 17, 18, 19, 20, 21, 22, 23]);
        let priority_block = |id, inner, outer, result, value| BasicBlock {
            id: BlockId(id),
            params: vec![],
            instructions: vec![
                SIRInstruction::Mux(
                    RegisterId(inner),
                    RegisterId(2),
                    RegisterId(value),
                    RegisterId(3),
                ),
                SIRInstruction::Mux(
                    RegisterId(outer),
                    RegisterId(1),
                    RegisterId(3),
                    RegisterId(inner),
                ),
                SIRInstruction::Unary(
                    RegisterId(result),
                    crate::ir::UnaryOp::BitNot,
                    RegisterId(outer),
                ),
            ],
            terminator: SIRTerminator::Return,
        };
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                (
                    BlockId(0),
                    BasicBlock {
                        id: BlockId(0),
                        params: vec![RegisterId(0), RegisterId(1), RegisterId(2), RegisterId(3)],
                        instructions: source,
                        terminator: SIRTerminator::Branch {
                            cond: RegisterId(0),
                            true_block: (BlockId(1), vec![]),
                            false_block: (BlockId(2), vec![]),
                        },
                    },
                ),
                (BlockId(1), priority_block(1, 24, 25, 26, 13)),
                (BlockId(2), priority_block(2, 27, 28, 29, 23)),
            ]
            .into_iter()
            .collect(),
            register_map,
        };

        let placement = PlacementAnalysis::analyze(&eu).unwrap();
        let plan = find_atomic_priority_placement(&eu, &placement).unwrap();
        assert_eq!(plan.regions.len(), 2);
        let mut next_block_id = 3;
        let mut reg_counter = 29;
        assert_eq!(
            apply_atomic_priority_placement(&mut eu, plan, &mut next_block_id, &mut reg_counter),
            2
        );

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
        }));
        assert!(
            !eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| {
                    def_reg(instruction).is_some_and(|register| (4..=23).contains(&register.0))
                })
        );
        for register in [RegisterId(13), RegisterId(23)] {
            assert_eq!(
                eu.blocks
                    .values()
                    .flat_map(|block| &block.instructions)
                    .filter(|instruction| def_reg(instruction) == Some(register))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn existing_cfg_places_complete_dags_in_their_used_arms() {
        let mut eu = cfg_unit(
            10,
            &[0],
            vec![
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                    instructions: vec![
                        SIRInstruction::Binary(
                            RegisterId(3),
                            RegisterId(1),
                            crate::ir::BinaryOp::Mul,
                            RegisterId(2),
                        ),
                        SIRInstruction::Binary(
                            RegisterId(4),
                            RegisterId(3),
                            crate::ir::BinaryOp::Mul,
                            RegisterId(2),
                        ),
                        SIRInstruction::Binary(
                            RegisterId(5),
                            RegisterId(1),
                            crate::ir::BinaryOp::Mul,
                            RegisterId(2),
                        ),
                        SIRInstruction::Binary(
                            RegisterId(6),
                            RegisterId(5),
                            crate::ir::BinaryOp::Mul,
                            RegisterId(2),
                        ),
                        SIRInstruction::Binary(
                            RegisterId(7),
                            RegisterId(1),
                            crate::ir::BinaryOp::Add,
                            RegisterId(2),
                        ),
                    ],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![store(10, 4), store(11, 7)],
                    terminator: SIRTerminator::Return,
                },
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![store(12, 6), store(13, 7)],
                    terminator: SIRTerminator::Return,
                },
            ],
        );
        assert_eq!(eu.verify_result(), Ok(()));

        let placement = PlacementAnalysis::analyze(&eu).unwrap();
        let plan = find_existing_cfg_placement(&eu, &placement).unwrap();
        assert_eq!(apply_existing_cfg_placement(&mut eu, plan), 4);

        let definitions = |block: BlockId| {
            eu.blocks[&block]
                .instructions
                .iter()
                .filter_map(def_reg)
                .collect::<Vec<_>>()
        };
        assert_eq!(definitions(BlockId(0)), vec![RegisterId(7)]);
        assert_eq!(definitions(BlockId(1)), vec![RegisterId(3), RegisterId(4)]);
        assert_eq!(definitions(BlockId(2)), vec![RegisterId(5), RegisterId(6)]);
        assert_eq!(eu.verify_result(), Ok(()));
    }

    #[test]
    fn existing_cfg_schedules_pure_dags_into_a_postdominating_use_block() {
        let mut eu = cfg_unit(
            6,
            &[1],
            vec![
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1)],
                    instructions: vec![
                        SIRInstruction::Unary(
                            RegisterId(2),
                            crate::ir::UnaryOp::BitNot,
                            RegisterId(0),
                        ),
                        SIRInstruction::Binary(
                            RegisterId(3),
                            RegisterId(2),
                            crate::ir::BinaryOp::Add,
                            RegisterId(0),
                        ),
                    ],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(1),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
                BasicBlock {
                    id: BlockId(3),
                    params: vec![],
                    instructions: vec![store(0, 3)],
                    terminator: SIRTerminator::Return,
                },
            ],
        );
        let placement = PlacementAnalysis::analyze(&eu).unwrap();
        let plan = find_existing_cfg_placement(&eu, &placement).unwrap();

        assert_eq!(apply_existing_cfg_placement(&mut eu, plan), 2);
        assert!(eu.blocks[&BlockId(0)].instructions.is_empty());
        assert_eq!(
            eu.blocks[&BlockId(3)]
                .instructions
                .iter()
                .filter_map(def_reg)
                .collect::<Vec<_>>(),
            vec![RegisterId(2), RegisterId(3)]
        );
        assert_eq!(eu.verify_result(), Ok(()));
    }

    #[test]
    fn existing_cfg_sinks_a_dynamic_load_within_the_same_loop_iteration() {
        let mut eu = cfg_unit(
            5,
            &[1, 2],
            vec![
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![imm(0, 0), imm(1, 1), imm(2, 0)],
                    terminator: SIRTerminator::Jump(BlockId(1), vec![]),
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![SIRInstruction::Load(
                        RegisterId(3),
                        addr(0),
                        SIROffset::Dynamic(RegisterId(0)),
                        64,
                    )],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(1),
                        true_block: (BlockId(2), vec![]),
                        false_block: (BlockId(3), vec![]),
                    },
                },
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![SIRInstruction::Unary(
                        RegisterId(4),
                        crate::ir::UnaryOp::Ident,
                        RegisterId(3),
                    )],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
                BasicBlock {
                    id: BlockId(3),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(2),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(4), vec![]),
                    },
                },
                BasicBlock {
                    id: BlockId(4),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Return,
                },
            ],
        );
        let placement = PlacementAnalysis::analyze(&eu).unwrap();
        let plan = find_existing_cfg_placement(&eu, &placement).unwrap();

        assert_eq!(apply_existing_cfg_placement(&mut eu, plan), 1);
        assert!(eu.blocks[&BlockId(1)].instructions.is_empty());
        assert!(matches!(
            eu.blocks[&BlockId(2)].instructions.first(),
            Some(SIRInstruction::Load(RegisterId(3), _, _, 64))
        ));
        assert_eq!(eu.verify_result(), Ok(()));
    }

    #[test]
    fn existing_cfg_uses_edge_arguments_and_preserves_dependency_order() {
        let mut eu = cfg_unit(
            5,
            &[0],
            vec![
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1)],
                    instructions: vec![
                        SIRInstruction::Binary(
                            RegisterId(2),
                            RegisterId(1),
                            crate::ir::BinaryOp::Mul,
                            RegisterId(1),
                        ),
                        SIRInstruction::Binary(
                            RegisterId(3),
                            RegisterId(2),
                            crate::ir::BinaryOp::Mul,
                            RegisterId(2),
                        ),
                    ],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![RegisterId(3)]),
                },
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Return,
                },
                BasicBlock {
                    id: BlockId(3),
                    params: vec![RegisterId(4)],
                    instructions: vec![store(0, 4)],
                    terminator: SIRTerminator::Return,
                },
            ],
        );
        let placement = PlacementAnalysis::analyze(&eu).unwrap();
        let plan = find_existing_cfg_placement(&eu, &placement).unwrap();

        assert_eq!(apply_existing_cfg_placement(&mut eu, plan), 2);
        assert_eq!(
            eu.blocks[&BlockId(1)]
                .instructions
                .iter()
                .filter_map(def_reg)
                .collect::<Vec<_>>(),
            vec![RegisterId(2), RegisterId(3)]
        );
        assert_eq!(eu.verify_result(), Ok(()));
    }

    #[test]
    fn existing_cfg_sinks_only_loads_with_the_same_state_version() {
        let make_unit = |intervening_write: bool| {
            let mut instructions = vec![
                SIRInstruction::Load(RegisterId(3), addr(0), SIROffset::Static(0), 64),
                SIRInstruction::Binary(
                    RegisterId(4),
                    RegisterId(3),
                    crate::ir::BinaryOp::Mul,
                    RegisterId(1),
                ),
            ];
            if intervening_write {
                instructions.push(store(0, 2));
            }
            cfg_unit(
                5,
                &[0],
                vec![
                    BasicBlock {
                        id: BlockId(0),
                        params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                        instructions,
                        terminator: SIRTerminator::Branch {
                            cond: RegisterId(0),
                            true_block: (BlockId(1), vec![]),
                            false_block: (BlockId(2), vec![]),
                        },
                    },
                    BasicBlock {
                        id: BlockId(1),
                        params: vec![],
                        instructions: vec![],
                        terminator: SIRTerminator::Return,
                    },
                    BasicBlock {
                        id: BlockId(2),
                        params: vec![],
                        instructions: vec![store(1, 4)],
                        terminator: SIRTerminator::Return,
                    },
                ],
            )
        };

        let mut unchanged = make_unit(false);
        let placement = PlacementAnalysis::analyze(&unchanged).unwrap();
        let plan = find_existing_cfg_placement(&unchanged, &placement).unwrap();
        assert_eq!(apply_existing_cfg_placement(&mut unchanged, plan), 2);
        assert!(matches!(
            unchanged.blocks[&BlockId(2)].instructions.first(),
            Some(SIRInstruction::Load(RegisterId(3), _, _, _))
        ));
        assert_eq!(unchanged.verify_result(), Ok(()));

        let mut changed = make_unit(true);
        let placement = PlacementAnalysis::analyze(&changed).unwrap();
        let plan = find_existing_cfg_placement(&changed, &placement).unwrap();
        assert_eq!(apply_existing_cfg_placement(&mut changed, plan), 1);
        assert!(
            changed.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| matches!(
                    instruction,
                    SIRInstruction::Load(RegisterId(3), _, _, _)
                ))
        );
        assert!(
            changed.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Store(_, _, _, _, _, _)))
        );
        assert_eq!(changed.verify_result(), Ok(()));
    }

    #[test]
    fn existing_cfg_rejects_cyclic_targets_and_unprofitable_fan_in() {
        let loop_unit = cfg_unit(
            6,
            &[0, 5],
            vec![
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1), RegisterId(2), RegisterId(5)],
                    instructions: vec![SIRInstruction::Binary(
                        RegisterId(3),
                        RegisterId(1),
                        crate::ir::BinaryOp::Mul,
                        RegisterId(2),
                    )],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(3), vec![]),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![store(0, 3)],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(5),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Return,
                },
                BasicBlock {
                    id: BlockId(3),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Return,
                },
            ],
        );
        let placement = PlacementAnalysis::analyze(&loop_unit).unwrap();
        assert!(find_existing_cfg_placement(&loop_unit, &placement).is_none());

        let cheap = cfg_unit(
            4,
            &[0],
            vec![
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                    instructions: vec![SIRInstruction::Binary(
                        RegisterId(3),
                        RegisterId(1),
                        crate::ir::BinaryOp::And,
                        RegisterId(2),
                    )],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![store(0, 3)],
                    terminator: SIRTerminator::Return,
                },
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Return,
                },
            ],
        );
        let placement = PlacementAnalysis::analyze(&cheap).unwrap();
        assert!(find_existing_cfg_placement(&cheap, &placement).is_none());
    }

    #[test]
    fn does_not_sink_load_across_aliasing_store() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(1), addr(0), SIROffset::Static(0), 64),
            imm(9, 3),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(4),
                Vec::new(),
                Vec::new(),
            ),
        ];
        append_mul_chain(&mut instructions, 1, 9, &[6, 7, 8, 10, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(5)),
            SIRInstruction::Store(
                addr(1),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);
        let mut eu = unit(instructions);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        let head = &eu.blocks[&BlockId(0)];
        assert!(matches!(head.terminator, SIRTerminator::Branch { .. }));
        assert!(
            head.instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Load(RegisterId(1), _, _, _)) })
        );
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(2), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
    }

    #[test]
    fn sunk_arm_uses_dominating_live_in_directly() {
        let mut instructions = vec![imm(1, 3), imm(4, 5)];
        append_mul_chain(&mut instructions, 7, 1, &[5, 6, 8, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);
        let mut eu = unit(instructions);
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(7)];

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        let head = &eu.blocks[&BlockId(0)];
        let SIRTerminator::Branch {
            true_block: true_edge,
            false_block: false_edge,
            ..
        } = &head.terminator
        else {
            panic!("expected mux to become branch");
        };
        let true_block = &eu.blocks[&true_edge.0];
        let false_block = &eu.blocks[&false_edge.0];
        assert!(true_edge.1.is_empty());
        assert!(false_edge.1.is_empty());
        assert!(true_block.params.is_empty());
        assert!(false_block.params.is_empty());
        assert!(true_block.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(dst, lhs, crate::ir::BinaryOp::Mul, _)
                    if *dst == RegisterId(5) && *lhs == RegisterId(7)
            )
        }));
    }

    #[test]
    fn branchifies_when_suffix_uses_dominating_live_in() {
        let mut instructions = vec![imm(1, 3), imm(6, 11)];
        append_mul_chain(&mut instructions, 1, 1, &[7, 8, 9, 10, 11, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Binary(
                RegisterId(5),
                RegisterId(6),
                crate::ir::BinaryOp::Add,
                RegisterId(3),
            ),
        ]);
        let mut eu = unit(instructions);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 4);
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(
                        RegisterId(5),
                        RegisterId(6),
                        crate::ir::BinaryOp::Add,
                        RegisterId(3)
                    )
                )
            })
        }));
    }

    #[test]
    fn merge_uses_dominating_param_directly() {
        let mut instructions = vec![imm(1, 3)];
        append_mul_chain(&mut instructions, 1, 1, &[8, 9, 10, 11, 12, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Binary(
                RegisterId(5),
                RegisterId(7),
                crate::ir::BinaryOp::Add,
                RegisterId(3),
            ),
        ]);
        let mut eu = unit(instructions);
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(7)];

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        let merge = eu
            .blocks
            .values()
            .find(|block| {
                block
                    .params
                    .first()
                    .is_some_and(|param| *param == RegisterId(3))
            })
            .expect("expected merge block with mux result param");
        assert_eq!(merge.params, vec![RegisterId(3)]);
        assert!(merge.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(5), lhs, crate::ir::BinaryOp::Add, RegisterId(3))
                    if *lhs == RegisterId(7)
            )
        }));
        assert!(eu.blocks.values().any(|block| {
            matches!(
                &block.terminator,
                SIRTerminator::Jump(target, args)
                    if *target == merge.id && args.len() == 1
            )
        }));
    }

    #[test]
    fn inlines_param_only_branch_blocks_from_jump_predecessors() {
        let mut register_map = HashMap::default();
        for reg in 0..8 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![imm(1, 3)],
                terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(1)]),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![RegisterId(2)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(2), vec![RegisterId(2)]),
                    false_block: (BlockId(3), vec![RegisterId(2)]),
                },
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: vec![RegisterId(4)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: vec![RegisterId(5)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        inline_param_only_jump_blocks(&mut eu);

        assert!(!eu.blocks.contains_key(&BlockId(1)));
        assert!(matches!(
            &eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } if true_block.1 == vec![RegisterId(1)] && false_block.1 == vec![RegisterId(1)]
        ));
    }

    #[test]
    fn inlines_chained_param_only_blocks_without_dangling_targets() {
        let mut register_map = HashMap::default();
        for reg in 0..4 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![imm(0, 3)],
                terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(0)]),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![RegisterId(1)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(2), vec![RegisterId(1)]),
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: vec![RegisterId(2)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), vec![RegisterId(2)]),
            },
        );
        blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: vec![RegisterId(3)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        inline_param_only_jump_blocks(&mut eu);

        assert!(!eu.blocks.contains_key(&BlockId(1)));
        assert!(!eu.blocks.contains_key(&BlockId(2)));
        assert!(matches!(
            &eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Jump(target, args)
                if *target == BlockId(3) && args == &vec![RegisterId(0)]
        ));
        eu.verify();
    }

    #[test]
    fn keeps_param_only_branch_when_descendant_uses_parameter_directly() {
        let mut register_map = HashMap::default();
        for reg in 0..6 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        register_map.insert(
            RegisterId(5),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![imm(1, 3)],
                terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(1)]),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![RegisterId(2)],
                instructions: vec![SIRInstruction::Imm(RegisterId(5), SIRValue::new(1u8))],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(5),
                    true_block: (BlockId(2), Vec::new()),
                    false_block: (BlockId(3), Vec::new()),
                },
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Unary(
                    RegisterId(4),
                    crate::ir::UnaryOp::BitNot,
                    RegisterId(2),
                )],
                terminator: SIRTerminator::Return,
            },
        );
        blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        inline_param_only_jump_blocks(&mut eu);

        assert!(eu.blocks.contains_key(&BlockId(1)));
        eu.verify();
    }

    #[test]
    fn keeps_cheap_mux_feeding_jump_args() {
        let mut register_map = HashMap::default();
        for reg in 0..8 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![imm(1, 1), imm(2, 2), imm(3, 3)],
                terminator: SIRTerminator::Jump(
                    BlockId(1),
                    vec![RegisterId(1), RegisterId(2), RegisterId(3)],
                ),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![RegisterId(4), RegisterId(5), RegisterId(6)],
                instructions: vec![SIRInstruction::Mux(
                    RegisterId(7),
                    RegisterId(4),
                    RegisterId(5),
                    RegisterId(6),
                )],
                terminator: SIRTerminator::Jump(BlockId(2), vec![RegisterId(7)]),
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: vec![RegisterId(7)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert!(eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(7), _, _, _)))
        }));
    }

    #[test]
    fn preserves_mux_result_through_merge_when_used_after_store() {
        let mut instructions = vec![imm(1, 3)];
        append_mul_chain(&mut instructions, 1, 1, &[5, 6, 7, 8, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr(1),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);
        let mut eu = unit(instructions);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 4);
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(3), _, _, _)))
        }));
        assert!(
            eu.blocks
                .values()
                .any(|block| block.params == vec![RegisterId(3)])
        );
        assert!(eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(3), _, _)))
        }));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(2), _, _)))
        }));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(4), _, _)))
        }));
    }
}
