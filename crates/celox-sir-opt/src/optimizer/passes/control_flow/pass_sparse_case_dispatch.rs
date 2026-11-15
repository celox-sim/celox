use super::pass_manager::ExecutionUnitPass;
use super::shared::def_reg;
use crate::PassOptions;
use crate::ir::cfg::SirCfg;
use crate::ir::{
    AbsoluteAddr, BasicBlock, BinaryOp, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId,
    RegisterType, SIRInstruction, SIROffset, SIRSwitchCase, SIRTerminator, STABLE_REGION, UnaryOp,
};
use crate::{HashMap, HashSet};
use num_bigint::BigUint;
use num_traits::{One, Zero};
#[expect(
    clippy::disallowed_types,
    reason = "single-threaded: lazy dominance and ID-limit caches local to one sequential discovery call"
)]
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Turns a prioritized chain of exact, same-selector muxes into one sparse
/// dispatch. Narrow selectors remain a first-class multi-way branch through
/// backend lowering; wider selectors use a balanced comparison tree.
#[derive(Clone, Default)]
pub(in crate::optimizer) struct SparseCaseDispatchPass {
    stable_alias_class: HashMap<AbsoluteAddr, AbsoluteAddr>,
}

impl SparseCaseDispatchPass {
    pub(in crate::optimizer) fn new(address_aliases: &HashMap<AbsoluteAddr, AbsoluteAddr>) -> Self {
        let mut adjacency = HashMap::<AbsoluteAddr, Vec<AbsoluteAddr>>::default();
        for (&alias, &canonical) in address_aliases {
            adjacency.entry(alias).or_default().push(canonical);
            adjacency.entry(canonical).or_default().push(alias);
        }

        let mut stable_alias_class = HashMap::default();
        let mut addresses = adjacency.keys().copied().collect::<Vec<_>>();
        addresses.sort_unstable();
        for address in addresses {
            if stable_alias_class.contains_key(&address) {
                continue;
            }
            let mut worklist = vec![address];
            while let Some(member) = worklist.pop() {
                if stable_alias_class.contains_key(&member) {
                    continue;
                }
                stable_alias_class.insert(member, address);
                worklist.extend(adjacency.get(&member).into_iter().flatten().copied());
            }
        }
        Self { stable_alias_class }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct DefSite {
    block: BlockId,
    index: usize,
}

#[derive(Clone)]
struct CaseStage {
    mux_index: usize,
    keys: Vec<BigUint>,
    value: RegisterId,
}

#[derive(Clone)]
struct DispatchArm {
    value: RegisterId,
    sink_defs: Vec<usize>,
}

#[derive(Clone)]
struct Boundary {
    threshold: BigUint,
    right_arm: usize,
}

#[derive(Clone)]
struct SparseCasePlan {
    block_id: BlockId,
    root_index: usize,
    result: RegisterId,
    selector: RegisterId,
    stages: Vec<CaseStage>,
    arms: Vec<DispatchArm>,
    cases: Vec<(BigUint, usize)>,
    direct_switch: bool,
    initial_arm: usize,
    boundaries: Vec<Boundary>,
    reachable_arms: Vec<usize>,
    dead_defs: HashSet<usize>,
    profitability: SparseCaseProfitability,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SparseCaseProfitability {
    removed_always_cost: u128,
    sunk_total_cost: u128,
    retained_arm_cost: u128,
    introduced_cost: u128,
}

impl SparseCaseProfitability {
    fn avoided_cost(self) -> u128 {
        self.removed_always_cost
            .saturating_add(self.sunk_total_cost.saturating_sub(self.retained_arm_cost))
    }

    fn proves_worst_case_benefit(self) -> bool {
        self.avoided_cost() > self.introduced_cost
    }
}

impl ExecutionUnitPass for SparseCaseDispatchPass {
    fn name(&self) -> &'static str {
        "sparse_case_dispatch"
    }

    fn handles_calls(&self) -> bool {
        true
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        // EqWildcard is an exact equality only for two-state values.  In
        // four-state mode its RHS wildcard mask and Mux X/Z-condition behavior
        // cannot be represented by an ordinary SIR Branch.
        if options.four_state {
            return;
        }

        // Every successful rewrite removes at least one mux.  Re-discovery is
        // intentional: splitting one block can expose another independent
        // chain in its head or merge block.  Termination follows from the
        // strictly decreasing number of SIR Mux instructions; there is no
        // iteration or function-size cap.
        let mut changed = false;
        let diagnostics = &options.optimize_options.diagnostics;
        let stats = diagnostics.branchify_stats;
        let mut applied = 0usize;
        let Some(mut next_block) = eu
            .blocks
            .keys()
            .map(|id| id.0)
            .max()
            .unwrap_or(0)
            .checked_add(1)
        else {
            return;
        };
        let Some(mut next_register) = eu
            .register_map
            .keys()
            .map(|id| id.0)
            .max()
            .unwrap_or(0)
            .checked_add(1)
        else {
            return;
        };
        let mut candidate_blocks: Option<HashSet<BlockId>> = None;
        loop {
            // Applying a batch normally leaves no newly split block to
            // inspect. Avoid rebuilding whole-EU use and definition indices
            // merely to discover that the restricted candidate set is empty.
            if candidate_blocks.as_ref().is_some_and(HashSet::is_empty) {
                break;
            }
            let plans =
                find_sparse_case_plans(eu, &self.stable_alias_class, candidate_blocks.as_ref());
            if plans.is_empty() {
                break;
            }
            // Recognition proves capacity for each plan separately. Reserve
            // the complete batch before allocating any IDs.
            let mut block_limit = next_block;
            let mut register_limit = next_register;
            let mut batches = BTreeMap::<BlockId, Vec<SparseCasePlan>>::new();
            for plan in plans {
                let decisions = if plan.direct_switch {
                    0
                } else {
                    plan.boundaries.len()
                };
                let Some(block_end) = block_limit
                    .checked_add(plan.reachable_arms.len())
                    .and_then(|n| n.checked_add(decisions))
                    .and_then(|n| n.checked_add(1))
                else {
                    continue;
                };
                let Some(register_end) = decisions
                    .checked_mul(2)
                    .and_then(|n| register_limit.checked_add(n))
                else {
                    continue;
                };
                block_limit = block_end;
                register_limit = register_end;
                batches.entry(plan.block_id).or_default().push(plan);
            }
            if batches.is_empty() {
                break;
            }
            let mut next_candidates = HashSet::default();
            for plans in batches.into_values() {
                applied += plans.len();
                next_candidates.extend(apply_sparse_case_batch(
                    eu,
                    plans,
                    &mut next_block,
                    &mut next_register,
                ));
                changed = true;
            }
            if stats {
                tracing::debug!("[branchify-stats] sparse_case applied={applied}");
            }
            candidate_blocks = Some(next_candidates);
        }
        if stats || diagnostics.pass_timing {
            tracing::debug!("[branchify-stats] sparse_case done applied={applied}");
        }
        if changed {
            // A condition can be defined in a dominating predecessor left by
            // an earlier CFG rewrite.  Prune those newly dead pure DAGs once,
            // after plan discovery is complete, rather than rescanning the
            // whole EU after every independently rewritten chain.
            prune_dead_pure_instructions(eu);
        }
    }
}

fn find_sparse_case_plans(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    stable_alias_class: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    candidate_blocks: Option<&HashSet<BlockId>>,
) -> Vec<SparseCasePlan> {
    let use_counts = count_uses(eu);
    let def_sites = definition_sites(eu);
    let mut block_ids = eu
        .blocks
        .keys()
        .copied()
        .filter(|block| candidate_blocks.is_none_or(|candidates| candidates.contains(block)))
        .collect::<Vec<_>>();
    block_ids.sort_unstable();
    let mut planned_blocks = HashSet::default();
    let mut plans = Vec::new();
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: lazily built dominator tree shared by the candidates of one sequential discovery call"
    )]
    let dominance = OnceCell::<Option<SirCfg>>::new();
    // Discovery borrows the EU immutably. All candidates therefore share the
    // same ID limits; compute them only if a profitable candidate needs them.
    // A new discovery call after applying plans gets a fresh cache.
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: lazily computed register/block ID limits shared by the candidates of one sequential discovery call"
    )]
    let max_ids = OnceCell::<(usize, usize)>::new();

    // The first sweep plans only maximal same-selector spines.  This avoids
    // repeating global-use cloning, local DCE, and arm-DAG collection for all
    // O(n) prefixes of the common profitable long-chain case.  If every
    // maximal candidate fails its proof, a second sweep considers the
    // deferred prefixes, so the optimization does not depend on monotonicity
    // of the profitability model.
    for maximal_only in [true, false] {
        for &block_id in &block_ids {
            if planned_blocks.contains(&block_id) {
                continue;
            }
            let block = &eu.blocks[&block_id];
            let local_defs = local_definition_positions(block);
            // Unchanged zero-use definitions are shared by all candidates in
            // this immutable block. Candidate-specific zeros come from the
            // sparse use-count overrides, not a rescan of every definition.
            let initially_unused = local_defs
                .keys()
                .copied()
                .filter(|reg| use_counts.get(reg).copied().unwrap_or(0) == 0)
                .collect::<Vec<_>>();
            let deferred =
                nonmaximal_same_selector_muxes(eu, block, &local_defs, &def_sites, &use_counts);
            let dense_lookup_indices =
                dense_constant_lookup_mux_indices(eu, block, &local_defs, &def_sites, &deferred);
            let mut candidates = Vec::new();
            for (root_index, inst) in block.instructions.iter().enumerate() {
                if !matches!(inst, SIRInstruction::Mux(..))
                    || dense_lookup_indices.contains(&root_index)
                    || (maximal_only && deferred.contains(&root_index))
                    || (!maximal_only && !deferred.contains(&root_index))
                {
                    continue;
                }
                let Some(plan) = recognize_sparse_case_chain(
                    eu,
                    block,
                    root_index,
                    &local_defs,
                    &def_sites,
                    &use_counts,
                    stable_alias_class,
                    &dominance,
                    &max_ids,
                    &initially_unused,
                ) else {
                    continue;
                };
                candidates.push(plan);
            }
            if !candidates.is_empty() {
                planned_blocks.insert(block_id);
                plans.extend(disjoint_sparse_case_plans(candidates));
            }
        }
    }
    plans
}

// Include every instruction that application can remove or move. Disjoint
// intervals leave the other plan's prefix indices and retained definitions
// intact. Overlapping plans are rediscovered after the selected rewrites.
fn sparse_case_start(plan: &SparseCasePlan) -> usize {
    plan.stages
        .iter()
        .map(|stage| stage.mux_index)
        .chain(
            plan.dead_defs
                .iter()
                .copied()
                .filter(|&i| i <= plan.root_index),
        )
        .chain(
            plan.reachable_arms
                .iter()
                .flat_map(|&arm| plan.arms[arm].sink_defs.iter().copied()),
        )
        .min()
        .unwrap_or(plan.root_index)
}

fn disjoint_sparse_case_plans(mut candidates: Vec<SparseCasePlan>) -> Vec<SparseCasePlan> {
    // Preserve the old best-plan preference before considering additional
    // non-overlapping rewrites. Source order breaks otherwise equal ties.
    candidates.sort_by(|a, b| {
        case_key_count(&b.stages)
            .cmp(&case_key_count(&a.stages))
            .then_with(|| b.stages.len().cmp(&a.stages.len()))
            .then_with(|| {
                b.profitability
                    .avoided_cost()
                    .cmp(&a.profitability.avoided_cost())
            })
            .then_with(|| a.root_index.cmp(&b.root_index))
    });
    let mut selected = BTreeMap::<usize, SparseCasePlan>::new();
    for plan in candidates {
        let start = sparse_case_start(&plan);
        if selected
            .range(..=start)
            .next_back()
            .is_some_and(|(_, prior)| prior.root_index >= start)
            || selected
                .range(start..)
                .next()
                .is_some_and(|(&next, _)| next <= plan.root_index)
        {
            continue;
        }
        selected.insert(start, plan);
    }
    selected.into_values().collect()
}

fn nonmaximal_same_selector_muxes(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    local_defs: &HashMap<RegisterId, usize>,
    def_sites: &HashMap<RegisterId, DefSite>,
    use_counts: &HashMap<RegisterId, usize>,
) -> HashSet<usize> {
    let selectors = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, inst)| match inst {
            SIRInstruction::Mux(_, cond, _, _) => match_exact_case_condition(eu, def_sites, *cond)
                .map(|condition| (index, condition.selector)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut deferred = HashSet::default();
    for (parent_index, inst) in block.instructions.iter().enumerate() {
        let SIRInstruction::Mux(parent_dst, _, parent_true, false_value) = inst else {
            continue;
        };
        if use_counts.get(false_value).copied() != Some(1) {
            continue;
        }
        let Some(&child_index) = local_defs.get(false_value) else {
            continue;
        };
        let Some(SIRInstruction::Mux(child_dst, _, child_true, child_false)) =
            block.instructions.get(child_index)
        else {
            continue;
        };
        let Some(result_width) = eu.register_map.get(parent_dst).map(RegisterType::width) else {
            continue;
        };
        if eu.register_map.get(parent_true).map(RegisterType::width) != Some(result_width)
            || eu.register_map.get(false_value).map(RegisterType::width) != Some(result_width)
            || eu.register_map.get(child_dst).map(RegisterType::width) != Some(result_width)
            || eu.register_map.get(child_true).map(RegisterType::width) != Some(result_width)
            || eu.register_map.get(child_false).map(RegisterType::width) != Some(result_width)
        {
            continue;
        }
        if selectors.get(&parent_index) == selectors.get(&child_index)
            && selectors.contains_key(&parent_index)
        {
            deferred.insert(child_index);
        }
    }
    deferred
}

fn dense_constant_lookup_mux_indices(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    local_defs: &HashMap<RegisterId, usize>,
    def_sites: &HashMap<RegisterId, DefSite>,
    nonmaximal_same_selector: &HashSet<usize>,
) -> HashSet<usize> {
    let mut protected = HashSet::default();

    for (root_index, inst) in block.instructions.iter().enumerate() {
        let SIRInstruction::Mux(result, _, _, _) = inst else {
            continue;
        };
        // Every stage in a same-selector spine was previously used as a root,
        // making dense-lookup recognition quadratic in chain length. The
        // maximal root reaches and protects the complete spine.
        if nonmaximal_same_selector.contains(&root_index) {
            continue;
        }
        let Some(result_width) = eu.register_map.get(result).map(RegisterType::width) else {
            continue;
        };
        let mut selector = None;
        let mut cursor = *result;
        let mut stages = Vec::new();
        let mut seen_muxes = HashSet::default();
        while let Some(&mux_index) = local_defs.get(&cursor) {
            if !seen_muxes.insert(mux_index) {
                stages.clear();
                break;
            }
            let SIRInstruction::Mux(_, cond, true_value, false_value) =
                &block.instructions[mux_index]
            else {
                break;
            };
            if eu.register_map.get(true_value).map(RegisterType::width) != Some(result_width)
                || eu.register_map.get(false_value).map(RegisterType::width) != Some(result_width)
            {
                stages.clear();
                break;
            }
            let Some(condition) = match_exact_case_condition(eu, def_sites, *cond) else {
                break;
            };
            if selector.is_some_and(|expected| expected != condition.selector) {
                break;
            }
            selector = Some(condition.selector);
            stages.push(CaseStage {
                mux_index,
                keys: condition.keys,
                value: *true_value,
            });
            cursor = *false_value;
        }

        let Some(selector_width) = selector
            .and_then(|selector| eu.register_map.get(&selector))
            .map(RegisterType::width)
        else {
            continue;
        };
        let mut effective = BTreeMap::new();
        for stage in &stages {
            for key in &stage.keys {
                effective.entry(key.clone()).or_insert(stage.value);
            }
        }
        if is_dense_constant_lookup(
            eu,
            def_sites,
            selector_width,
            result_width,
            &stages,
            &effective,
        ) {
            protected.extend(stages.iter().map(|stage| stage.mux_index));
        }
    }
    protected
}

#[expect(
    clippy::disallowed_types,
    reason = "single-threaded: borrows the discovery call's lazy dominance and ID-limit caches"
)]
fn recognize_sparse_case_chain(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    root_index: usize,
    local_defs: &HashMap<RegisterId, usize>,
    def_sites: &HashMap<RegisterId, DefSite>,
    use_counts: &HashMap<RegisterId, usize>,
    stable_alias_class: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    dominance: &OnceCell<Option<SirCfg>>,
    max_ids: &OnceCell<(usize, usize)>,
    initially_unused: &[RegisterId],
) -> Option<SparseCasePlan> {
    let SIRInstruction::Mux(result, _, _, _) = &block.instructions[root_index] else {
        return None;
    };
    if use_counts.get(result).copied().unwrap_or(0) == 0 {
        return None;
    }
    let result_width = eu.register_map.get(result)?.width();

    let mut stages = Vec::new();
    let mut selector = None;
    let mut cursor = *result;
    while let Some(&mux_index) = local_defs.get(&cursor) {
        if mux_index > root_index {
            return None;
        }
        if !stages.is_empty() && use_counts.get(&cursor).copied().unwrap_or(0) != 1 {
            // This mux result is observable outside the outer chain.  It is a
            // valid default for the dispatch prefix, but cannot itself be
            // removed by that prefix's rewrite.
            break;
        }
        let SIRInstruction::Mux(dst, cond, true_value, false_value) =
            &block.instructions[mux_index]
        else {
            break;
        };
        if *dst != cursor
            || eu.register_map.get(true_value)?.width() != result_width
            || eu.register_map.get(false_value)?.width() != result_width
        {
            return None;
        }
        let Some(condition) = match_exact_case_condition(eu, def_sites, *cond) else {
            break;
        };
        if selector.is_some_and(|expected| expected != condition.selector) {
            break;
        }
        selector = Some(condition.selector);
        stages.push(CaseStage {
            mux_index,
            keys: condition.keys,
            value: *true_value,
        });
        cursor = *false_value;
    }

    if stages.is_empty() {
        return None;
    }
    let selector = selector?;
    let selector_width = eu.register_map.get(&selector)?.width();
    if selector_width == 0 || eu.register_map.get(&cursor)?.width() != result_width {
        return None;
    }

    // Stages are visited outermost first.  Therefore the first occurrence of
    // a duplicate key is the value selected by the original prioritized mux
    // chain; inner duplicates are unreachable and deliberately omitted.
    let mut effective = BTreeMap::<BigUint, RegisterId>::new();
    for stage in &stages {
        for key in &stage.keys {
            effective.entry(key.clone()).or_insert(stage.value);
        }
    }
    // One exact key is an ordinary branchification problem, not a sparse
    // case-dispatch problem.  A grouped condition can make one mux a genuine
    // multi-key dispatch, so count semantic keys rather than mux stages.
    if effective.len() < 2 {
        return None;
    }
    // Full-domain constant lookups have a denser indexed-table lowering when
    // their root is eligible.  Never partially branchify such a chain: doing
    // so would destroy that lowering opportunity and turn one lookup into a
    // large CFG.  The block prepass protects every stage of the full shape.
    if is_dense_constant_lookup(
        eu,
        def_sites,
        selector_width,
        result_width,
        &stages,
        &effective,
    ) {
        return None;
    }

    let mut arms = Vec::with_capacity(effective.len() + 1);
    arms.push(DispatchArm {
        value: cursor,
        sink_defs: Vec::new(),
    });
    let mut arm_by_value = HashMap::default();
    arm_by_value.insert(cursor, 0usize);
    let mut changes = BTreeMap::<BigUint, usize>::new();
    let mut cases = Vec::with_capacity(effective.len());
    for (key, &value) in &effective {
        let arm = *arm_by_value.entry(value).or_insert_with(|| {
            let arm = arms.len();
            arms.push(DispatchArm {
                value,
                sink_defs: Vec::new(),
            });
            arm
        });
        if arm != 0 {
            cases.push((key.clone(), arm));
        }
        changes.insert(key.clone(), arm);
        let next = key + BigUint::one();
        if value_fits_width(&next, selector_width) {
            // A following consecutive key overwrites this transition, so the
            // tree contains no empty default interval between adjacent cases.
            changes.insert(next, 0);
        }
    }

    let initial_arm = changes.remove(&BigUint::zero()).unwrap_or(0);
    let mut current_arm = initial_arm;
    let boundaries = changes
        .into_iter()
        .filter_map(|(threshold, right_arm)| {
            if right_arm == current_arm {
                None
            } else {
                current_arm = right_arm;
                Some(Boundary {
                    threshold,
                    right_arm,
                })
            }
        })
        .collect::<Vec<_>>();
    if boundaries.is_empty() {
        return None;
    }

    let direct_switch = selector_width <= 8;
    let mut reachable = HashSet::default();
    if direct_switch {
        let domain_size = 1usize.checked_shl(selector_width as u32)?;
        // An exhaustive direct switch has no semantic default value. Do not
        // manufacture an orphan block for the Mux spine's initial value:
        // native jump-table lowering correctly has no table entry which can
        // reach it.
        if cases.len() < domain_size {
            reachable.insert(0);
        }
        reachable.extend(cases.iter().map(|(_, arm)| *arm));
    } else {
        reachable.insert(initial_arm);
        reachable.extend(boundaries.iter().map(|boundary| boundary.right_arm));
    }
    let mut reachable_arms = reachable.into_iter().collect::<Vec<_>>();
    reachable_arms.sort_unstable();

    let chain_indices = stages
        .iter()
        .map(|stage| stage.mux_index)
        .collect::<HashSet<_>>();
    let mut occupied_sink_defs = HashSet::default();
    for &arm_index in &reachable_arms {
        let mut defs = HashSet::default();
        collect_sinkable_defs(
            block,
            local_defs,
            use_counts,
            root_index,
            root_index,
            arms[arm_index].value,
            &mut defs,
            stable_alias_class,
        );
        if !defs.is_disjoint(&chain_indices) || !defs.is_disjoint(&occupied_sink_defs) {
            return None;
        }
        occupied_sink_defs.extend(defs.iter().copied());
        let mut defs = defs.into_iter().collect::<Vec<_>>();
        defs.sort_unstable();
        arms[arm_index].sink_defs = defs;
    }

    let dead_defs = dead_defs_after_rewrite(
        block,
        root_index,
        &stages,
        selector,
        if direct_switch { 1 } else { boundaries.len() },
        &arms,
        &reachable_arms,
        use_counts,
        local_defs,
        &occupied_sink_defs,
        initially_unused,
    )?;
    let cross_block_dead_defs = cross_block_exact_dead_defs_after_rewrite(
        eu,
        block,
        &stages,
        selector,
        if direct_switch { 1 } else { boundaries.len() },
        &arms,
        &reachable_arms,
        use_counts,
        def_sites,
        dominance,
    )?;
    let profitability = sparse_case_profitability(
        eu,
        block,
        &stages,
        selector,
        result_width,
        &arms,
        &reachable_arms,
        &dead_defs,
        &cross_block_dead_defs,
        boundaries.len(),
        direct_switch,
    );
    if !profitability.proves_worst_case_benefit() {
        return None;
    }

    let additional_blocks = reachable_arms
        .len()
        .checked_add(if direct_switch { 0 } else { boundaries.len() })?
        .checked_add(1)?;
    let additional_registers = if direct_switch {
        0
    } else {
        boundaries.len().checked_mul(2)?
    };
    let &(max_block, max_register) = max_ids.get_or_init(|| {
        (
            eu.blocks.keys().map(|id| id.0).max().unwrap_or(0),
            eu.register_map.keys().map(|id| id.0).max().unwrap_or(0),
        )
    });
    // `fresh_*` maintains a one-past-the-last sentinel after returning an ID,
    // so prove that sentinel representable as well as every generated ID.
    max_block.checked_add(additional_blocks)?.checked_add(1)?;
    max_register
        .checked_add(additional_registers)?
        .checked_add(1)?;

    Some(SparseCasePlan {
        block_id: block.id,
        root_index,
        result: *result,
        selector,
        stages,
        arms,
        cases,
        direct_switch,
        initial_arm,
        boundaries,
        reachable_arms,
        dead_defs,
        profitability,
    })
}

fn is_dense_constant_lookup(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_sites: &HashMap<RegisterId, DefSite>,
    selector_width: usize,
    result_width: usize,
    stages: &[CaseStage],
    effective: &BTreeMap<BigUint, RegisterId>,
) -> bool {
    if result_width == 0
        || result_width > 64
        || selector_width == 0
        || selector_width >= usize::BITS as usize
    {
        return false;
    }
    let Some(domain_size) = 1usize.checked_shl(selector_width as u32) else {
        return false;
    };
    if domain_size < 4
        || stages.len() != domain_size
        || stages.iter().any(|stage| stage.keys.len() != 1)
        || effective.len() != domain_size
    {
        return false;
    }
    if !effective
        .keys()
        .enumerate()
        .all(|(expected, key)| key == &BigUint::from(expected))
    {
        return false;
    }
    effective
        .values()
        .all(|&value| is_direct_definite_u64_constant(eu, def_sites, value))
}

fn case_key_count(stages: &[CaseStage]) -> usize {
    stages.iter().fold(0usize, |count, stage| {
        count.saturating_add(stage.keys.len())
    })
}

fn is_direct_definite_u64_constant(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_sites: &HashMap<RegisterId, DefSite>,
    reg: RegisterId,
) -> bool {
    let Some(SIRInstruction::Imm(_, value)) = instruction_defining(eu, def_sites, reg) else {
        return false;
    };
    value.mask.is_zero() && value.payload.to_u64_digits().len() <= 1
}

struct ExactCaseCondition {
    selector: RegisterId,
    keys: Vec<BigUint>,
}

fn match_exact_case_condition(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_sites: &HashMap<RegisterId, DefSite>,
    cond: RegisterId,
) -> Option<ExactCaseCondition> {
    match_exact_case_condition_inner(eu, def_sites, cond, &mut HashSet::default())
}

fn match_exact_case_condition_inner(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_sites: &HashMap<RegisterId, DefSite>,
    cond: RegisterId,
    active: &mut HashSet<RegisterId>,
) -> Option<ExactCaseCondition> {
    if !active.insert(cond) {
        return None;
    }
    let result = match_exact_case_condition_inner_impl(eu, def_sites, cond, active);
    active.remove(&cond);
    result
}

fn match_exact_case_condition_inner_impl(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_sites: &HashMap<RegisterId, DefSite>,
    cond: RegisterId,
    active: &mut HashSet<RegisterId>,
) -> Option<ExactCaseCondition> {
    let mut cursor = cond;
    let mut seen = HashSet::default();
    while seen.insert(cursor) {
        let inst = instruction_defining(eu, def_sites, cursor)?;
        match inst {
            SIRInstruction::Unary(
                dst,
                UnaryOp::Ident | UnaryOp::Or | UnaryOp::ToTwoState,
                inner,
            ) => {
                let destination_width = eu.register_map.get(dst)?.width();
                let source_width = eu.register_map.get(inner)?.width();
                if destination_width == 0
                    || source_width == 0
                    || (!matches!(inst, SIRInstruction::Unary(_, UnaryOp::Ident, _))
                        && (destination_width != 1 || source_width != 1))
                {
                    return None;
                }
                cursor = *inner;
            }
            SIRInstruction::Concat(dst, args) if !args.is_empty() => {
                let (&low, high) = args.split_last()?;
                if eu.register_map.get(dst)?.width() == 0
                    || eu.register_map.get(&low)?.width() != 1
                    || high.iter().any(|reg| {
                        exact_constant(eu, def_sites, *reg).is_none_or(|value| !value.is_zero())
                    })
                {
                    return None;
                }
                cursor = low;
            }
            _ => break,
        }
    }

    if let SIRInstruction::Binary(result, lhs, BinaryOp::LogicOr | BinaryOp::Or, rhs) =
        instruction_defining(eu, def_sites, cursor)?
    {
        if eu.register_map.get(result)?.width() != 1
            || eu.register_map.get(lhs)?.width() != 1
            || eu.register_map.get(rhs)?.width() != 1
        {
            return None;
        }
        let lhs = match_exact_case_condition_inner(eu, def_sites, *lhs, active)?;
        let rhs = match_exact_case_condition_inner(eu, def_sites, *rhs, active)?;
        if lhs.selector != rhs.selector {
            return None;
        }
        let mut keys = lhs.keys.into_iter().collect::<BTreeSet<_>>();
        keys.extend(rhs.keys);
        if keys.is_empty() {
            return None;
        }
        return Some(ExactCaseCondition {
            selector: lhs.selector,
            keys: keys.into_iter().collect(),
        });
    }

    let SIRInstruction::Binary(
        compare_result,
        lhs,
        op @ (BinaryOp::Eq | BinaryOp::EqCase | BinaryOp::EqWildcard),
        rhs,
    ) = instruction_defining(eu, def_sites, cursor)?
    else {
        return None;
    };
    if eu.register_map.get(compare_result)?.width() != 1 {
        // Backends widen comparison operands to their common width, which
        // includes the declared result width.  A wider result can therefore
        // change signed-extension behavior, rather than merely zero-extending
        // a canonical one-bit predicate.
        return None;
    }
    let lhs_constant = exact_constant(eu, def_sites, *lhs);
    let rhs_constant = exact_constant(eu, def_sites, *rhs);
    let (selector, key_reg, key) = match op {
        BinaryOp::Eq | BinaryOp::EqCase => match (lhs_constant, rhs_constant) {
            (None, Some(key)) => (*lhs, *rhs, key),
            (Some(key), None) => (*rhs, *lhs, key),
            _ => return None,
        },
        BinaryOp::EqWildcard => {
            // Wildcard equality is directional.  Even though this pass only
            // runs in two-state mode, accepting only a definite RHS immediate
            // keeps the recognition valid if the IR's provenance changes.
            match (lhs_constant, rhs_constant) {
                (None, Some(key)) => (*lhs, *rhs, key),
                _ => return None,
            }
        }
        _ => unreachable!(),
    };
    let compare_width = eu.register_map.get(&selector)?.width();
    if compare_width == 0 || eu.register_map.get(&key_reg)?.width() != compare_width {
        return None;
    }
    let key = truncate_to_width(key, compare_width);
    let selector = canonical_case_selector(eu, def_sites, selector);
    let selector_width = eu.register_map.get(&selector)?.width();
    // A zero-extension wrapper is removable only for keys whose high part is
    // zero.  A non-fitting key makes that equality constantly false; reject
    // the whole condition instead of silently dropping one OR alternative.
    if selector_width == 0 || !value_fits_width(&key, selector_width) {
        return None;
    }
    Some(ExactCaseCondition {
        selector,
        keys: vec![key],
    })
}

fn canonical_case_selector(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_sites: &HashMap<RegisterId, DefSite>,
    mut reg: RegisterId,
) -> RegisterId {
    let mut seen = HashSet::default();
    while seen.insert(reg) {
        match instruction_defining(eu, def_sites, reg) {
            Some(SIRInstruction::Unary(dst, UnaryOp::Ident, inner))
                if identity_preserves_bits(eu, *dst, *inner) =>
            {
                reg = *inner;
            }
            Some(SIRInstruction::Concat(dst, args)) if !args.is_empty() => {
                let (&low, high) = match args.split_last() {
                    Some(parts) => parts,
                    None => break,
                };
                let Some(low_width) = eu.register_map.get(&low).map(RegisterType::width) else {
                    break;
                };
                let Some(dst_width) = eu.register_map.get(dst).map(RegisterType::width) else {
                    break;
                };
                let Some(total_width) = high.iter().try_fold(low_width, |sum, high| {
                    sum.checked_add(eu.register_map.get(high)?.width())
                }) else {
                    break;
                };
                if low_width == 0
                    || dst_width != total_width
                    || high.iter().any(|high| {
                        exact_constant(eu, def_sites, *high).is_none_or(|value| !value.is_zero())
                    })
                    || (dst_width == low_width && !identity_preserves_bits(eu, *dst, low))
                {
                    break;
                }
                reg = low;
            }
            Some(SIRInstruction::Slice(dst, src, 0, width))
                if *width
                    == eu
                        .register_map
                        .get(src)
                        .map(RegisterType::width)
                        .unwrap_or(0)
                    && identity_preserves_bits(eu, *dst, *src) =>
            {
                reg = *src;
            }
            _ => break,
        }
    }
    reg
}

fn exact_constant(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_sites: &HashMap<RegisterId, DefSite>,
    reg: RegisterId,
) -> Option<BigUint> {
    exact_constant_inner(eu, def_sites, reg, &mut HashSet::default())
}

fn exact_constant_inner(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_sites: &HashMap<RegisterId, DefSite>,
    reg: RegisterId,
    active: &mut HashSet<RegisterId>,
) -> Option<BigUint> {
    if !active.insert(reg) {
        return None;
    }
    let width = eu.register_map.get(&reg)?.width();
    let result = match instruction_defining(eu, def_sites, reg)? {
        SIRInstruction::Imm(_, value) if value.mask.is_zero() => {
            Some(truncate_to_width(value.payload.clone(), width))
        }
        SIRInstruction::Unary(dst, UnaryOp::Ident, inner)
            if identity_preserves_bits(eu, *dst, *inner) =>
        {
            exact_constant_inner(eu, def_sites, *inner, active)
        }
        SIRInstruction::Concat(dst, args) if !args.is_empty() => {
            let mut value = BigUint::zero();
            let mut total_width = 0usize;
            for &arg in args {
                let arg_width = eu.register_map.get(&arg)?.width();
                total_width = total_width.checked_add(arg_width)?;
                value <<= arg_width;
                value |= exact_constant_inner(eu, def_sites, arg, active)?;
            }
            if total_width != eu.register_map.get(dst)?.width() {
                None
            } else {
                Some(truncate_to_width(value, width))
            }
        }
        SIRInstruction::Slice(dst, src, offset, slice_width) => {
            let end = offset.checked_add(*slice_width)?;
            if *slice_width != eu.register_map.get(dst)?.width()
                || end > eu.register_map.get(src)?.width()
            {
                None
            } else {
                let value = exact_constant_inner(eu, def_sites, *src, active)? >> offset;
                Some(truncate_to_width(value, *slice_width))
            }
        }
        _ => None,
    };
    active.remove(&reg);
    result
}

fn identity_preserves_bits(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    dst: RegisterId,
    src: RegisterId,
) -> bool {
    matches!(
        (eu.register_map.get(&dst), eu.register_map.get(&src)),
        (Some(dst_type), Some(src_type)) if dst_type == src_type
    )
}

fn instruction_defining<'a>(
    eu: &'a ExecutionUnit<RegionedAbsoluteAddr>,
    def_sites: &HashMap<RegisterId, DefSite>,
    reg: RegisterId,
) -> Option<&'a SIRInstruction<RegionedAbsoluteAddr>> {
    let site = def_sites.get(&reg)?;
    eu.blocks.get(&site.block)?.instructions.get(site.index)
}

fn truncate_to_width(value: BigUint, width: usize) -> BigUint {
    if value_fits_width(&value, width) {
        value
    } else {
        value & ((BigUint::one() << width) - BigUint::one())
    }
}

fn value_fits_width(value: &BigUint, width: usize) -> bool {
    value.bits() <= u64::try_from(width).unwrap_or(u64::MAX)
}

fn collect_sinkable_defs(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    def_pos: &HashMap<RegisterId, usize>,
    use_counts: &HashMap<RegisterId, usize>,
    user_index: usize,
    memory_barrier_index: usize,
    root: RegisterId,
    defs: &mut HashSet<usize>,
    stable_alias_class: &HashMap<AbsoluteAddr, AbsoluteAddr>,
) {
    let mut worklist = vec![(root, user_index)];
    while let Some((value, user_index)) = worklist.pop() {
        if use_counts.get(&value).copied().unwrap_or(0) != 1 {
            continue;
        }
        let Some(&index) = def_pos.get(&value) else {
            continue;
        };
        if index >= user_index || defs.contains(&index) {
            continue;
        }
        let inst = &block.instructions[index];
        if !is_removable_pure(inst) {
            continue;
        }
        if let Some(load) = memory_read(inst)
            && has_intervening_memory_conflict(
                block,
                index + 1,
                memory_barrier_index,
                load,
                stable_alias_class,
            )
        {
            continue;
        }

        defs.insert(index);
        worklist.extend(
            instruction_uses(inst)
                .into_iter()
                .map(|operand| (operand, index)),
        );
    }
}

fn is_removable_pure(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    !inst.has_side_effects()
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
            offset: static_offset(offset),
            width: *width,
        }),
        _ => None,
    }
}

fn memory_write(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<MemAccess<'_>> {
    inst.memory_write().map(|write| MemAccess {
        addr: write.addr,
        offset: static_offset(write.offset),
        width: write.width,
    })
}

fn static_offset(offset: &SIROffset) -> Option<usize> {
    match offset {
        SIROffset::Static(value) => Some(*value),
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
    stable_alias_class: &HashMap<AbsoluteAddr, AbsoluteAddr>,
) -> bool {
    block.instructions[start..end].iter().any(|inst| {
        is_memory_barrier(inst)
            || memory_write(inst)
                .is_some_and(|write| memory_may_alias(read, write, stable_alias_class))
            // A call may write any part of the objects its effects list.
            || inst.opaque_written_objects().is_none_or(|mut objects| {
                objects.any(|addr| {
                    let write = MemAccess {
                        addr,
                        offset: None,
                        width: 0,
                    };
                    memory_may_alias(read, write, stable_alias_class)
                })
            })
    })
}

fn is_memory_barrier(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    // State accesses are not moved across host interactions, so the host
    // observes them in program order.
    inst.is_host_interaction()
}

fn memory_may_alias(
    lhs: MemAccess<'_>,
    rhs: MemAccess<'_>,
    stable_alias_class: &HashMap<AbsoluteAddr, AbsoluteAddr>,
) -> bool {
    if lhs.addr.region != rhs.addr.region {
        return false;
    }
    let lhs_absolute = lhs.addr.absolute_addr();
    let rhs_absolute = rhs.addr.absolute_addr();
    let same_storage = lhs_absolute == rhs_absolute
        || (lhs.addr.region == STABLE_REGION
            && stable_alias_class
                .get(&lhs_absolute)
                .zip(stable_alias_class.get(&rhs_absolute))
                .is_some_and(|(lhs_class, rhs_class)| lhs_class == rhs_class));
    if !same_storage {
        return false;
    }
    match (lhs.offset, rhs.offset) {
        (Some(lhs_offset), Some(rhs_offset)) => {
            let (Some(lhs_end), Some(rhs_end)) = (
                lhs_offset.checked_add(lhs.width),
                rhs_offset.checked_add(rhs.width),
            ) else {
                // An overflowing access range is malformed for concrete
                // memory, but SIR verification does not currently reject it.
                // Conservatively treating it as aliasing keeps this pass
                // sound for such input.
                return true;
            };
            lhs_offset < rhs_end && rhs_offset < lhs_end
        }
        _ => true,
    }
}

#[allow(clippy::too_many_arguments)]
fn dead_defs_after_rewrite(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    root_index: usize,
    stages: &[CaseStage],
    selector: RegisterId,
    boundary_count: usize,
    arms: &[DispatchArm],
    reachable_arms: &[usize],
    use_counts: &HashMap<RegisterId, usize>,
    local_defs: &HashMap<RegisterId, usize>,
    protected_defs: &HashSet<usize>,
    initially_unused: &[RegisterId],
) -> Option<HashSet<usize>> {
    let chain_indices = stages
        .iter()
        .map(|stage| stage.mux_index)
        .collect::<HashSet<_>>();
    let mut remaining = SparseUseCounts::new(use_counts);
    for &index in &chain_indices {
        for operand in instruction_uses(&block.instructions[index]) {
            remaining.decrement(operand)?;
        }
    }
    remaining.increment(selector, boundary_count)?;
    for &arm_index in reachable_arms {
        remaining.increment(arms[arm_index].value, 1)?;
    }

    let mut queue = VecDeque::new();
    // A count can become zero only if this rewrite changed it, or if it
    // was zero already. The recursive queue below handles newly dead users'
    // operands. Keep pre-existing dead definitions for the same profitability
    // accounting as the full local-definition scan.
    for reg in initially_unused
        .iter()
        .copied()
        .chain(remaining.overrides.keys().copied())
    {
        let Some(&index) = local_defs.get(&reg) else {
            continue;
        };
        if index <= root_index
            && !chain_indices.contains(&index)
            && !protected_defs.contains(&index)
            && remaining.get(reg) == 0
            && is_removable_pure(&block.instructions[index])
        {
            queue.push_back(index);
        }
    }

    let mut dead = HashSet::default();
    while let Some(index) = queue.pop_front() {
        if !dead.insert(index) {
            continue;
        }
        for operand in instruction_uses(&block.instructions[index]) {
            remaining.decrement(operand)?;
            if remaining.get(operand) == 0
                && local_defs.get(&operand).is_some_and(|&operand_index| {
                    operand_index <= root_index
                        && !chain_indices.contains(&operand_index)
                        && !protected_defs.contains(&operand_index)
                        && !dead.contains(&operand_index)
                        && is_removable_pure(&block.instructions[operand_index])
                })
            {
                queue.push_back(local_defs[&operand]);
            }
        }
    }
    Some(dead)
}

#[allow(clippy::too_many_arguments)]
#[expect(
    clippy::disallowed_types,
    reason = "single-threaded: borrows the discovery call's lazy dominator-tree cache"
)]
fn cross_block_exact_dead_defs_after_rewrite(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    stages: &[CaseStage],
    selector: RegisterId,
    boundary_count: usize,
    arms: &[DispatchArm],
    reachable_arms: &[usize],
    use_counts: &HashMap<RegisterId, usize>,
    def_sites: &HashMap<RegisterId, DefSite>,
    dominance: &OnceCell<Option<SirCfg>>,
) -> Option<HashSet<DefSite>> {
    // Matching proves the shape of every condition, but the definitions can
    // live in a predecessor after an earlier CFG rewrite.  Restrict the
    // additional profitability credit to that exact predicate/key DAG.  In
    // particular, do not turn this into speculative global DCE credit for an
    // unrelated pure expression which happens to be dead already.
    let mut candidates = HashSet::<RegisterId>::default();
    for stage in stages {
        collect_exact_condition_dag_defs(
            eu,
            def_sites,
            stage_condition(block, stage)?,
            selector,
            &mut candidates,
        );
    }

    for &reg in &candidates {
        let site = def_sites.get(&reg)?;
        if site.block != block.id && !cached_block_dominates(eu, site.block, block.id, dominance) {
            // A verifier-valid producer must dominate its use.  Refusing the
            // credit here keeps this proof conservative even for malformed IR
            // presented to the pass without verification.
            return Some(HashSet::default());
        }
    }

    let mut remaining = SparseUseCounts::new(use_counts);
    for stage in stages {
        for operand in instruction_uses(&block.instructions[stage.mux_index]) {
            remaining.decrement(operand)?;
        }
    }
    remaining.increment(selector, boundary_count)?;
    for &arm_index in reachable_arms {
        remaining.increment(arms[arm_index].value, 1)?;
    }

    let mut queue = candidates
        .iter()
        .copied()
        .filter(|&reg| remaining.get(reg) == 0)
        .collect::<VecDeque<_>>();
    let mut dead = HashSet::<RegisterId>::default();
    while let Some(reg) = queue.pop_front() {
        if !dead.insert(reg) {
            continue;
        }
        let inst = instruction_defining(eu, def_sites, reg)?;
        for operand in instruction_uses(inst) {
            remaining.decrement(operand)?;
            if candidates.contains(&operand) && remaining.get(operand) == 0 {
                queue.push_back(operand);
            }
        }
    }

    Some(
        dead.into_iter()
            .filter_map(|reg| def_sites.get(&reg).copied())
            .filter(|site| site.block != block.id)
            .collect(),
    )
}

fn stage_condition(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    stage: &CaseStage,
) -> Option<RegisterId> {
    match block.instructions.get(stage.mux_index)? {
        SIRInstruction::Mux(_, cond, _, _) => Some(*cond),
        _ => None,
    }
}

fn collect_exact_condition_dag_defs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_sites: &HashMap<RegisterId, DefSite>,
    reg: RegisterId,
    selector: RegisterId,
    out: &mut HashSet<RegisterId>,
) {
    if reg == selector || out.contains(&reg) {
        return;
    }
    let Some(inst) = instruction_defining(eu, def_sites, reg) else {
        // Block parameters and external inputs are never removable defs.
        return;
    };
    if !is_exact_condition_dag_instruction(inst) {
        return;
    }
    out.insert(reg);
    for operand in instruction_uses(inst) {
        collect_exact_condition_dag_defs(eu, def_sites, operand, selector, out);
    }
}

fn is_exact_condition_dag_instruction(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        inst,
        SIRInstruction::Imm(..)
            | SIRInstruction::Binary(
                _,
                _,
                BinaryOp::Eq
                    | BinaryOp::EqCase
                    | BinaryOp::EqWildcard
                    | BinaryOp::LogicOr
                    | BinaryOp::Or,
                _,
            )
            | SIRInstruction::Unary(_, UnaryOp::Ident, _)
            | SIRInstruction::Unary(_, UnaryOp::Or | UnaryOp::ToTwoState, _)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
    )
}

/// Build the forward dominator tree only if a cross-block proof is needed.
/// Discovery leaves the CFG unchanged, so all candidates share this linear-
/// space analysis. Invalid CFGs retain the conservative original path proof.
#[expect(
    clippy::disallowed_types,
    reason = "single-threaded: fills the discovery call's lazy dominator-tree cache on first use"
)]
fn cached_block_dominates(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    dominator: BlockId,
    target: BlockId,
    cache: &OnceCell<Option<SirCfg>>,
) -> bool {
    cache
        .get_or_init(|| SirCfg::analyze_forward_structure(eu).ok())
        .as_ref()
        .map_or_else(
            || block_dominates(eu, dominator, target),
            |cfg| cfg.dominates(dominator, target),
        )
}

fn block_dominates(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    dominator: BlockId,
    target: BlockId,
) -> bool {
    if dominator == target || dominator == eu.entry_block_id {
        return eu.blocks.contains_key(&dominator) && eu.blocks.contains_key(&target);
    }
    if !eu.blocks.contains_key(&dominator)
        || !eu.blocks.contains_key(&target)
        || !eu.blocks.contains_key(&eu.entry_block_id)
    {
        return false;
    }

    // `dominator` dominates `target` exactly when removing it makes `target`
    // unreachable from the entry.  This one-target proof runs only for the
    // distinct external producer blocks of an otherwise profitable-looking
    // case chain, avoiding a whole-EU dominator matrix in the common path.
    let mut seen = HashSet::default();
    let mut queue = VecDeque::from([eu.entry_block_id]);
    while let Some(block_id) = queue.pop_front() {
        if block_id == dominator || !seen.insert(block_id) {
            continue;
        }
        if block_id == target {
            return false;
        }
        let Some(block) = eu.blocks.get(&block_id) else {
            return false;
        };
        queue.extend(terminator_successors(&block.terminator));
    }
    true
}

fn terminator_successors(term: &SIRTerminator) -> Vec<BlockId> {
    match term {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|case| case.target)
            .chain(std::iter::once(*default))
            .collect(),
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

struct SparseUseCounts<'a> {
    base: &'a HashMap<RegisterId, usize>,
    overrides: HashMap<RegisterId, usize>,
}

impl<'a> SparseUseCounts<'a> {
    fn new(base: &'a HashMap<RegisterId, usize>) -> Self {
        Self {
            base,
            overrides: HashMap::default(),
        }
    }

    fn get(&self, reg: RegisterId) -> usize {
        self.overrides
            .get(&reg)
            .copied()
            .or_else(|| self.base.get(&reg).copied())
            .unwrap_or(0)
    }

    fn decrement(&mut self, reg: RegisterId) -> Option<()> {
        let count = self.get(reg).checked_sub(1)?;
        self.overrides.insert(reg, count);
        Some(())
    }

    fn increment(&mut self, reg: RegisterId, amount: usize) -> Option<()> {
        let count = self.get(reg).checked_add(amount)?;
        self.overrides.insert(reg, count);
        Some(())
    }
}

const BRANCH_CONTROL_COST: u128 = 3;
const WORST_CASE_MISPREDICT_COST: u128 = 16;
const ARM_TO_MERGE_JUMP_COST: u128 = 1;
const PHI_COPY_COST_PER_CHUNK: u128 = 2;

#[allow(clippy::too_many_arguments)]
fn sparse_case_profitability(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    stages: &[CaseStage],
    selector: RegisterId,
    result_width: usize,
    arms: &[DispatchArm],
    reachable_arms: &[usize],
    dead_defs: &HashSet<usize>,
    cross_block_dead_defs: &HashSet<DefSite>,
    boundary_count: usize,
    direct_switch: bool,
) -> SparseCaseProfitability {
    let chain_cost = saturating_sum(stages.iter().map(|stage| {
        runtime_instruction_cost(&block.instructions[stage.mux_index], &eu.register_map)
    }));
    let dead_cost = saturating_sum(
        dead_defs
            .iter()
            .map(|&index| runtime_instruction_cost(&block.instructions[index], &eu.register_map)),
    )
    .saturating_add(saturating_sum(cross_block_dead_defs.iter().filter_map(
        |site| {
            eu.blocks
                .get(&site.block)
                .and_then(|block| block.instructions.get(site.index))
                .map(|inst| runtime_instruction_cost(inst, &eu.register_map))
        },
    )));
    let arm_costs = reachable_arms
        .iter()
        .map(|&arm_index| {
            saturating_sum(arms[arm_index].sink_defs.iter().map(|&index| {
                runtime_instruction_cost(&block.instructions[index], &eu.register_map)
            }))
        })
        .collect::<Vec<_>>();
    let sunk_total_cost = saturating_sum(arm_costs.iter().copied());
    let retained_arm_cost = arm_costs.iter().copied().max().unwrap_or(0);

    let selector_chunks = eu.register_map[&selector].width().div_ceil(64).max(1) as u128;
    let decision_cost = selector_chunks
        .saturating_add(3u128.saturating_mul(selector_chunks))
        .saturating_add(BRANCH_CONTROL_COST)
        .saturating_add(WORST_CASE_MISPREDICT_COST);
    let depth = if direct_switch {
        1
    } else {
        balanced_tree_depth(boundary_count)
    } as u128;
    let result_chunks = result_width.div_ceil(64).max(1) as u128;
    let introduced_cost = depth
        .saturating_mul(decision_cost)
        .saturating_add(ARM_TO_MERGE_JUMP_COST)
        .saturating_add(result_chunks.saturating_mul(PHI_COPY_COST_PER_CHUNK));

    SparseCaseProfitability {
        removed_always_cost: chain_cost.saturating_add(dead_cost),
        sunk_total_cost,
        retained_arm_cost,
        introduced_cost,
    }
}

fn saturating_sum(values: impl IntoIterator<Item = u128>) -> u128 {
    values
        .into_iter()
        .fold(0, |sum, value| sum.saturating_add(value))
}

fn balanced_tree_depth(boundary_count: usize) -> usize {
    if boundary_count == 0 {
        0
    } else {
        usize::BITS as usize - boundary_count.leading_zeros() as usize
    }
}

fn runtime_instruction_cost(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    register_map: &HashMap<RegisterId, RegisterType>,
) -> u128 {
    let width = |reg: RegisterId| register_map.get(&reg).map_or(64, RegisterType::width);
    let chunks = |bits: usize| bits.div_ceil(64).max(1) as u128;
    match inst {
        SIRInstruction::Imm(dst, _) => chunks(width(*dst)),
        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            let n = chunks(width(*dst).max(width(*lhs)).max(width(*rhs)));
            match op {
                BinaryOp::Float { .. } => 32,
                BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor
                | BinaryOp::LogicAnd
                | BinaryOp::LogicOr => n,
                BinaryOp::Add | BinaryOp::Sub => 3u128.saturating_mul(n),
                BinaryOp::Mul => 5u128.saturating_mul(n.saturating_mul(n)),
                BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
                    12u128.saturating_mul(n.saturating_mul(n))
                }
                BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => 4u128.saturating_mul(n),
                BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::EqCase
                | BinaryOp::NeCase
                | BinaryOp::EqWildcard
                | BinaryOp::NeWildcard
                | BinaryOp::LtU
                | BinaryOp::LtS
                | BinaryOp::LeU
                | BinaryOp::LeS
                | BinaryOp::GtU
                | BinaryOp::GtS
                | BinaryOp::GeU
                | BinaryOp::GeS => 3u128.saturating_mul(n),
            }
        }
        SIRInstruction::Unary(dst, op, src) => {
            let n = chunks(width(*dst).max(width(*src)));
            match op {
                UnaryOp::PopCount => 2u128.saturating_mul(n).saturating_add(1),
                UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
                    3u128.saturating_mul(n).saturating_add(1)
                }
                _ => 2u128.saturating_mul(n),
            }
        }
        SIRInstruction::Load(_, _, offset, width) => 3u128
            .saturating_mul(chunks(*width))
            .saturating_add(3 * u128::from(offset.is_dynamic())),
        SIRInstruction::Concat(dst, args) => chunks(width(*dst)) + args.len() as u128,
        SIRInstruction::Slice(dst, _, _, _) => 2 * chunks(width(*dst)),
        SIRInstruction::Mux(dst, _, true_value, false_value) => {
            chunks(width(*dst).max(width(*true_value)).max(width(*false_value)))
        }
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. }
        | SIRInstruction::ExternCall { .. }
        | SIRInstruction::Call { .. } => 0,
    }
}

fn apply_sparse_case_batch(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plans: Vec<SparseCasePlan>,
    next_block: &mut usize,
    next_register: &mut usize,
) -> Vec<BlockId> {
    let original = eu
        .blocks
        .remove(&plans[0].block_id)
        .expect("sparse case batch must reference an existing block");
    let mut instructions = original.instructions.into_iter();
    let mut head_id = original.id;
    let mut params = original.params;
    let mut offset = 0;
    let mut touched = Vec::new();
    for mut plan in plans {
        // Partition once in source order instead of cloning a shrinking
        // whole-block prefix for every plan. The previous merge becomes this
        // plan's head, preserving the result parameter and dominance chain.
        debug_assert!(sparse_case_start(&plan) >= offset);
        let end = plan.root_index + 1;
        let segment = instructions.by_ref().take(end - offset).collect();
        plan.block_id = head_id;
        plan.root_index -= offset;
        for stage in &mut plan.stages {
            stage.mux_index -= offset;
        }
        // Definitions after the root are retained by application, even when
        // local DCE also identified them as unused.
        plan.dead_defs = plan
            .dead_defs
            .into_iter()
            .filter(|&i| i < end)
            .map(|i| i - offset)
            .collect();
        for &arm in &plan.reachable_arms {
            for index in &mut plan.arms[arm].sink_defs {
                *index -= offset;
            }
        }
        eu.blocks.insert(
            head_id,
            BasicBlock {
                id: head_id,
                params,
                instructions: segment,
                terminator: SIRTerminator::Return,
            },
        );
        let (changed, merge_id) = apply_sparse_case_plan(eu, plan, next_block, next_register);
        touched.extend(changed);
        let merge = eu
            .blocks
            .remove(&merge_id)
            .expect("application creates a merge block");
        debug_assert!(merge.instructions.is_empty());
        head_id = merge_id;
        params = merge.params;
        offset = end;
    }
    eu.blocks.insert(
        head_id,
        BasicBlock {
            id: head_id,
            params,
            instructions: instructions.collect(),
            terminator: original.terminator,
        },
    );
    touched
}

fn apply_sparse_case_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: SparseCasePlan,
    next_block: &mut usize,
    next_register: &mut usize,
) -> (Vec<BlockId>, BlockId) {
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("sparse case plan must reference an existing block");
    let chain_indices = plan
        .stages
        .iter()
        .map(|stage| stage.mux_index)
        .collect::<HashSet<_>>();
    let sink_indices = plan
        .reachable_arms
        .iter()
        .flat_map(|&arm| plan.arms[arm].sink_defs.iter().copied())
        .collect::<HashSet<_>>();
    let mut removed = chain_indices;
    removed.extend(plan.dead_defs.iter().copied());
    removed.extend(sink_indices);

    let merge_id = fresh_block(next_block);

    let mut generated = HashMap::<BlockId, BasicBlock<RegionedAbsoluteAddr>>::default();
    let mut arm_blocks = HashMap::<usize, BlockId>::default();
    for &arm_index in &plan.reachable_arms {
        let id = fresh_block(next_block);
        let instructions = plan.arms[arm_index]
            .sink_defs
            .iter()
            .map(|&index| original.instructions[index].clone())
            .collect();
        generated.insert(
            id,
            BasicBlock {
                id,
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Jump(merge_id, vec![plan.arms[arm_index].value]),
            },
        );
        arm_blocks.insert(arm_index, id);
    }

    let (decision_instructions, decision_terminator) = if plan.direct_switch {
        let default_arm = if arm_blocks.contains_key(&0) {
            0
        } else {
            // Every selector value has an explicit case. SIR retains a
            // syntactic default target, but it is semantically unreachable;
            // point it at an already reachable arm instead of creating an
            // orphan CFG block which jump-table lowering cannot target.
            plan.cases
                .first()
                .map(|(_, arm)| *arm)
                .expect("a direct sparse dispatch has at least two cases")
        };
        (
            Vec::new(),
            SIRTerminator::Switch {
                selector: plan.selector,
                cases: plan
                    .cases
                    .iter()
                    .map(|(value, arm)| SIRSwitchCase {
                        value: value.clone(),
                        target: arm_blocks[arm],
                    })
                    .collect(),
                default: arm_blocks[&default_arm],
            },
        )
    } else {
        let selector_type = eu.register_map[&plan.selector].clone();
        let decision_root = build_decision_tree(
            &plan.boundaries,
            plan.initial_arm,
            &arm_blocks,
            plan.selector,
            &selector_type,
            next_block,
            next_register,
            &mut eu.register_map,
            &mut generated,
        );
        let root_decision = generated
            .remove(&decision_root)
            .expect("non-empty boundary tree must have a decision root");
        (root_decision.instructions, root_decision.terminator)
    };

    let mut head_instructions = original
        .instructions
        .iter()
        .enumerate()
        .take(plan.root_index)
        .filter(|(index, _)| !removed.contains(index))
        .map(|(_, inst)| inst.clone())
        .collect::<Vec<_>>();
    head_instructions.extend(decision_instructions);
    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_instructions,
        terminator: decision_terminator,
    };
    let merge = BasicBlock {
        id: merge_id,
        params: vec![plan.result],
        instructions: original
            .instructions
            .into_iter()
            .skip(plan.root_index + 1)
            .collect(),
        terminator: original.terminator,
    };

    let mut touched = generated.keys().copied().collect::<Vec<_>>();
    touched.push(plan.block_id);
    touched.push(merge_id);
    eu.blocks.insert(plan.block_id, head);
    eu.blocks.insert(merge_id, merge);
    eu.blocks.extend(generated);
    (touched, merge_id)
}

fn prune_dead_pure_instructions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let defs = definition_sites(eu);
    let mut work = Vec::new();
    for block in eu.blocks.values() {
        work.extend(terminator_uses(&block.terminator));
        for inst in &block.instructions {
            if !is_removable_pure(inst) {
                work.extend(instruction_uses(inst));
            }
        }
    }

    let mut live = HashSet::default();
    while let Some(reg) = work.pop() {
        if !live.insert(reg) {
            continue;
        }
        if let Some(inst) = instruction_defining(eu, &defs, reg) {
            work.extend(instruction_uses(inst));
        }
    }

    for block in eu.blocks.values_mut() {
        block.instructions.retain(|inst| {
            !is_removable_pure(inst) || def_reg(inst).is_some_and(|dst| live.contains(&dst))
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn build_decision_tree(
    boundaries: &[Boundary],
    initial_arm: usize,
    arm_blocks: &HashMap<usize, BlockId>,
    selector: RegisterId,
    selector_type: &RegisterType,
    next_block: &mut usize,
    next_register: &mut usize,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    generated: &mut HashMap<BlockId, BasicBlock<RegionedAbsoluteAddr>>,
) -> BlockId {
    if boundaries.is_empty() {
        return arm_blocks[&initial_arm];
    }
    let midpoint = boundaries.len() / 2;
    let pivot = &boundaries[midpoint];
    let left = build_decision_tree(
        &boundaries[..midpoint],
        initial_arm,
        arm_blocks,
        selector,
        selector_type,
        next_block,
        next_register,
        register_map,
        generated,
    );
    let right = build_decision_tree(
        &boundaries[midpoint + 1..],
        pivot.right_arm,
        arm_blocks,
        selector,
        selector_type,
        next_block,
        next_register,
        register_map,
        generated,
    );

    let id = fresh_block(next_block);
    let key = fresh_register(next_register);
    let condition = fresh_register(next_register);
    register_map.insert(key, selector_type.clone());
    register_map.insert(
        condition,
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    generated.insert(
        id,
        BasicBlock {
            id,
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Imm(key, crate::ir::SIRValue::new(pivot.threshold.clone())),
                SIRInstruction::Binary(condition, selector, BinaryOp::LtU, key),
            ],
            terminator: SIRTerminator::Branch {
                cond: condition,
                true_block: (left, Vec::new()),
                false_block: (right, Vec::new()),
            },
        },
    );
    id
}

fn fresh_block(next: &mut usize) -> BlockId {
    let result = BlockId(*next);
    *next += 1;
    result
}

fn fresh_register(next: &mut usize) -> RegisterId {
    let result = RegisterId(*next);
    *next += 1;
    result
}

fn definition_sites(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, DefSite> {
    let mut result = HashMap::default();
    for block in eu.blocks.values() {
        for (index, inst) in block.instructions.iter().enumerate() {
            if let Some(reg) = def_reg(inst) {
                result.insert(
                    reg,
                    DefSite {
                        block: block.id,
                        index,
                    },
                );
            }
        }
    }
    result
}

fn local_definition_positions(
    block: &BasicBlock<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, usize> {
    block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, inst)| def_reg(inst).map(|reg| (reg, index)))
        .collect()
}

fn count_uses(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, usize> {
    let mut result = HashMap::default();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            for reg in instruction_uses(inst) {
                *result.entry(reg).or_default() += 1;
            }
        }
        for reg in terminator_uses(&block.terminator) {
            *result.entry(reg).or_default() += 1;
        }
    }
    result
}

fn instruction_uses(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
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
            let mut result = vec![*cond];
            result.extend(true_block.1.iter().copied());
            result.extend(false_block.1.iter().copied());
            result
        }
        SIRTerminator::Switch { selector, .. } => vec![*selector],
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests;
