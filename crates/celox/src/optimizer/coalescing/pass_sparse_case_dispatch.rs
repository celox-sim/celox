use super::pass_manager::ExecutionUnitPass;
use super::shared::def_reg;
use crate::ir::{
    AbsoluteAddr, BasicBlock, BinaryOp, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId,
    RegisterType, SIRInstruction, SIROffset, SIRTerminator, STABLE_REGION, UnaryOp,
};
use crate::optimizer::PassOptions;
use crate::{HashMap, HashSet};
use num_bigint::BigUint;
use num_traits::{One, Zero};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Turns a prioritized chain of exact, same-selector muxes into one balanced
/// sparse dispatch.  This remains separate from `BranchifyMuxPass`: the latter
/// makes a local decision for one mux, while this pass proves a whole-chain
/// replacement before changing the CFG.
#[derive(Clone, Default)]
pub(super) struct SparseCaseDispatchPass {
    stable_alias_class: HashMap<AbsoluteAddr, AbsoluteAddr>,
}

impl SparseCaseDispatchPass {
    pub(super) fn new(address_aliases: &HashMap<AbsoluteAddr, AbsoluteAddr>) -> Self {
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
        while let Some(plan) = find_best_sparse_case_plan(eu, &self.stable_alias_class) {
            apply_sparse_case_plan(eu, plan);
            changed = true;
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

fn find_best_sparse_case_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    stable_alias_class: &HashMap<AbsoluteAddr, AbsoluteAddr>,
) -> Option<SparseCasePlan> {
    let use_counts = count_uses(eu);
    let def_sites = definition_sites(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable();

    // The first sweep plans only maximal same-selector spines.  This avoids
    // repeating global-use cloning, local DCE, and arm-DAG collection for all
    // O(n) prefixes of the common profitable long-chain case.  If every
    // maximal candidate fails its proof, a second sweep considers the
    // deferred prefixes, so the optimization does not depend on monotonicity
    // of the profitability model.
    for maximal_only in [true, false] {
        let mut best: Option<SparseCasePlan> = None;
        for &block_id in &block_ids {
            let block = &eu.blocks[&block_id];
            let local_defs = local_definition_positions(block);
            let dense_lookup_indices =
                dense_constant_lookup_mux_indices(eu, block, &local_defs, &def_sites);
            let deferred =
                nonmaximal_same_selector_muxes(eu, block, &local_defs, &def_sites, &use_counts);
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
                ) else {
                    continue;
                };
                let replace = best.as_ref().is_none_or(|current| {
                    case_key_count(&plan.stages) > case_key_count(&current.stages)
                        || (case_key_count(&plan.stages) == case_key_count(&current.stages)
                            && plan.stages.len() > current.stages.len())
                        || (case_key_count(&plan.stages) == case_key_count(&current.stages)
                            && plan.stages.len() == current.stages.len()
                            && plan.profitability.avoided_cost()
                                > current.profitability.avoided_cost())
                });
                if replace {
                    best = Some(plan);
                }
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
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
) -> HashSet<usize> {
    let mut protected = HashSet::default();

    for inst in &block.instructions {
        let SIRInstruction::Mux(result, _, _, _) = inst else {
            continue;
        };
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

fn recognize_sparse_case_chain(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    root_index: usize,
    local_defs: &HashMap<RegisterId, usize>,
    def_sites: &HashMap<RegisterId, DefSite>,
    use_counts: &HashMap<RegisterId, usize>,
    stable_alias_class: &HashMap<AbsoluteAddr, AbsoluteAddr>,
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
    for (key, value) in effective {
        let arm = *arm_by_value.entry(value).or_insert_with(|| {
            let arm = arms.len();
            arms.push(DispatchArm {
                value,
                sink_defs: Vec::new(),
            });
            arm
        });
        changes.insert(key.clone(), arm);
        let next = &key + BigUint::one();
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

    let mut reachable = HashSet::default();
    reachable.insert(initial_arm);
    reachable.extend(boundaries.iter().map(|boundary| boundary.right_arm));
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
        boundaries.len(),
        &arms,
        &reachable_arms,
        use_counts,
        local_defs,
        &occupied_sink_defs,
    )?;
    let cross_block_dead_defs = cross_block_exact_dead_defs_after_rewrite(
        eu,
        block,
        &stages,
        selector,
        boundaries.len(),
        &arms,
        &reachable_arms,
        use_counts,
        def_sites,
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
    );
    if !profitability.proves_worst_case_benefit() {
        return None;
    }

    let additional_blocks = reachable_arms
        .len()
        .checked_add(boundaries.len())?
        .checked_add(1)?;
    let additional_registers = boundaries.len().checked_mul(2)?;
    let max_block = eu.blocks.keys().map(|id| id.0).max().unwrap_or(0);
    let max_register = eu.register_map.keys().map(|id| id.0).max().unwrap_or(0);
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
            SIRInstruction::Unary(dst, UnaryOp::Ident, inner) => {
                if eu.register_map.get(dst)?.width() == 0
                    || eu.register_map.get(inner)?.width() == 0
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
        op @ (BinaryOp::Eq | BinaryOp::EqWildcard),
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
        BinaryOp::Eq => match (lhs_constant, rhs_constant) {
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
    match inst {
        SIRInstruction::Store(addr, offset, width, _, _, _) => Some(MemAccess {
            addr,
            offset: static_offset(offset),
            width: *width,
        }),
        SIRInstruction::Commit(_, addr, offset, width, _) => Some(MemAccess {
            addr,
            offset: static_offset(offset),
            width: *width,
        }),
        _ => None,
    }
}

fn static_offset(offset: &SIROffset) -> Option<usize> {
    match offset {
        SIROffset::Static(value) => Some(*value),
        SIROffset::Dynamic(_) => None,
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
) -> Option<HashSet<usize>> {
    let chain_indices = stages
        .iter()
        .map(|stage| stage.mux_index)
        .collect::<HashSet<_>>();
    let mut remaining = use_counts.clone();
    for &index in &chain_indices {
        for operand in instruction_uses(&block.instructions[index]) {
            decrement_count(&mut remaining, operand)?;
        }
    }
    let selector_uses = remaining.entry(selector).or_default();
    *selector_uses = selector_uses.checked_add(boundary_count)?;
    for &arm_index in reachable_arms {
        let arm_uses = remaining.entry(arms[arm_index].value).or_default();
        *arm_uses = arm_uses.checked_add(1)?;
    }

    let mut queue = VecDeque::new();
    for (&reg, &index) in local_defs {
        if index <= root_index
            && !chain_indices.contains(&index)
            && !protected_defs.contains(&index)
            && remaining.get(&reg).copied().unwrap_or(0) == 0
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
            decrement_count(&mut remaining, operand)?;
            if remaining.get(&operand).copied().unwrap_or(0) == 0
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

    let mut dominance = HashMap::<BlockId, bool>::default();
    for &reg in &candidates {
        let site = def_sites.get(&reg)?;
        if site.block != block.id
            && !*dominance
                .entry(site.block)
                .or_insert_with(|| block_dominates(eu, site.block, block.id))
        {
            // A verifier-valid producer must dominate its use.  Refusing the
            // credit here keeps this proof conservative even for malformed IR
            // presented to the pass without verification.
            return Some(HashSet::default());
        }
    }

    let mut remaining = use_counts.clone();
    for stage in stages {
        for operand in instruction_uses(&block.instructions[stage.mux_index]) {
            decrement_count(&mut remaining, operand)?;
        }
    }
    let selector_uses = remaining.entry(selector).or_default();
    *selector_uses = selector_uses.checked_add(boundary_count)?;
    for &arm_index in reachable_arms {
        let arm_uses = remaining.entry(arms[arm_index].value).or_default();
        *arm_uses = arm_uses.checked_add(1)?;
    }

    let mut queue = candidates
        .iter()
        .copied()
        .filter(|reg| remaining.get(reg).copied().unwrap_or(0) == 0)
        .collect::<VecDeque<_>>();
    let mut dead = HashSet::<RegisterId>::default();
    while let Some(reg) = queue.pop_front() {
        if !dead.insert(reg) {
            continue;
        }
        let inst = instruction_defining(eu, def_sites, reg)?;
        for operand in instruction_uses(inst) {
            decrement_count(&mut remaining, operand)?;
            if candidates.contains(&operand) && remaining.get(&operand).copied().unwrap_or(0) == 0 {
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
                BinaryOp::Eq | BinaryOp::EqWildcard | BinaryOp::LogicOr | BinaryOp::Or,
                _,
            )
            | SIRInstruction::Unary(_, UnaryOp::Ident, _)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
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
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

fn decrement_count(counts: &mut HashMap<RegisterId, usize>, reg: RegisterId) -> Option<()> {
    let count = counts.get_mut(&reg)?;
    *count = count.checked_sub(1)?;
    if *count == 0 {
        counts.remove(&reg);
    }
    Some(())
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
    let depth = balanced_tree_depth(boundary_count) as u128;
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
                BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor
                | BinaryOp::LogicAnd
                | BinaryOp::LogicOr => n,
                BinaryOp::Add | BinaryOp::Sub => 3u128.saturating_mul(n),
                BinaryOp::Mul => 5u128.saturating_mul(n.saturating_mul(n)),
                BinaryOp::Div | BinaryOp::Rem => 12u128.saturating_mul(n.saturating_mul(n)),
                BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => 4u128.saturating_mul(n),
                BinaryOp::Eq
                | BinaryOp::Ne
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
            .saturating_add(3 * u128::from(matches!(offset, SIROffset::Dynamic(_)))),
        SIRInstruction::Concat(dst, args) => chunks(width(*dst)) + args.len() as u128,
        SIRInstruction::Slice(dst, _, _, _) => 2 * chunks(width(*dst)),
        SIRInstruction::Mux(dst, _, true_value, false_value) => {
            chunks(width(*dst).max(width(*true_value)).max(width(*false_value)))
        }
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => 0,
    }
}

fn apply_sparse_case_plan(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, plan: SparseCasePlan) {
    let original = eu.blocks[&plan.block_id].clone();
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

    let mut next_block = eu.blocks.keys().map(|id| id.0).max().unwrap_or(0) + 1;
    let mut next_register = eu.register_map.keys().map(|id| id.0).max().unwrap_or(0) + 1;
    let merge_id = fresh_block(&mut next_block);

    let mut generated = HashMap::<BlockId, BasicBlock<RegionedAbsoluteAddr>>::default();
    let mut arm_blocks = HashMap::<usize, BlockId>::default();
    for &arm_index in &plan.reachable_arms {
        let id = fresh_block(&mut next_block);
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

    let selector_type = eu.register_map[&plan.selector].clone();
    let decision_root = build_decision_tree(
        &plan.boundaries,
        plan.initial_arm,
        &arm_blocks,
        plan.selector,
        &selector_type,
        &mut next_block,
        &mut next_register,
        &mut eu.register_map,
        &mut generated,
    );
    let root_decision = generated
        .remove(&decision_root)
        .expect("non-empty boundary tree must have a decision root");

    let mut head_instructions = original
        .instructions
        .iter()
        .enumerate()
        .take(plan.root_index)
        .filter(|(index, _)| !removed.contains(index))
        .map(|(_, inst)| inst.clone())
        .collect::<Vec<_>>();
    head_instructions.extend(root_decision.instructions);
    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_instructions,
        terminator: root_decision.terminator,
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

    eu.blocks.insert(plan.block_id, head);
    eu.blocks.insert(merge_id, merge);
    eu.blocks.extend(generated);
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
    match inst {
        SIRInstruction::Imm(_, _) => Vec::new(),
        SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
        SIRInstruction::Unary(_, _, src) => vec![*src],
        SIRInstruction::Load(_, _, SIROffset::Dynamic(offset), _) => vec![*offset],
        SIRInstruction::Load(_, _, SIROffset::Static(_), _) => Vec::new(),
        SIRInstruction::Store(_, SIROffset::Dynamic(offset), _, src, _, _) => {
            vec![*offset, *src]
        }
        SIRInstruction::Store(_, SIROffset::Static(_), _, src, _, _) => vec![*src],
        SIRInstruction::Commit(_, _, SIROffset::Dynamic(offset), _, _) => vec![*offset],
        SIRInstruction::Commit(_, _, SIROffset::Static(_), _, _) => Vec::new(),
        SIRInstruction::Concat(_, args) => args.clone(),
        SIRInstruction::Slice(_, src, _, _) => vec![*src],
        SIRInstruction::Mux(_, cond, true_value, false_value) => {
            vec![*cond, *true_value, *false_value]
        }
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
            let mut result = vec![*cond];
            result.extend(true_block.1.iter().copied());
            result.extend(false_block.1.iter().copied());
            result
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{InstanceId, SIRValue};
    use veryl_analyzer::ir::VarId;

    struct FixtureBuilder {
        next_register: usize,
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        register_map: HashMap<RegisterId, RegisterType>,
    }

    impl FixtureBuilder {
        fn new() -> Self {
            Self {
                next_register: 0,
                instructions: Vec::new(),
                register_map: HashMap::default(),
            }
        }

        fn register(&mut self, width: usize) -> RegisterId {
            let reg = RegisterId(self.next_register);
            self.next_register += 1;
            self.register_map.insert(
                reg,
                RegisterType::Bit {
                    width,
                    signed: false,
                },
            );
            reg
        }

        fn immediate(&mut self, width: usize, value: u64) -> RegisterId {
            self.immediate_value(width, SIRValue::new(value))
        }

        fn immediate_value(&mut self, width: usize, value: SIRValue) -> RegisterId {
            let dst = self.register(width);
            self.instructions.push(SIRInstruction::Imm(dst, value));
            dst
        }

        fn binary(
            &mut self,
            width: usize,
            lhs: RegisterId,
            op: BinaryOp,
            rhs: RegisterId,
        ) -> RegisterId {
            let dst = self.register(width);
            self.instructions
                .push(SIRInstruction::Binary(dst, lhs, op, rhs));
            dst
        }

        fn exact_condition(&mut self, selector: RegisterId, key: u64, op: BinaryOp) -> RegisterId {
            let width = self.register_map[&selector].width();
            let key = self.immediate(width, key);
            self.binary(1, selector, op, key)
        }

        fn exact_condition_with_zero_extended_key(
            &mut self,
            selector: RegisterId,
            key_width: usize,
            key: u64,
            op: BinaryOp,
        ) -> RegisterId {
            let selector_width = self.register_map[&selector].width();
            assert!(key_width < selector_width);
            let high = self.immediate(selector_width - key_width, 0);
            let low = self.immediate(key_width, key);
            let key = self.concat(vec![high, low]);
            self.binary(1, selector, op, key)
        }

        fn grouped_condition(&mut self, conditions: &[RegisterId]) -> RegisterId {
            assert!(!conditions.is_empty());
            conditions[1..].iter().fold(conditions[0], |lhs, &rhs| {
                self.binary(1, lhs, BinaryOp::LogicOr, rhs)
            })
        }

        fn concat(&mut self, args: Vec<RegisterId>) -> RegisterId {
            let width = args.iter().map(|arg| self.register_map[arg].width()).sum();
            let dst = self.register(width);
            self.instructions.push(SIRInstruction::Concat(dst, args));
            dst
        }

        fn slice(&mut self, source: RegisterId, offset: usize, width: usize) -> RegisterId {
            let dst = self.register(width);
            self.instructions
                .push(SIRInstruction::Slice(dst, source, offset, width));
            dst
        }

        fn zero_extend(&mut self, source: RegisterId, extension: usize) -> RegisterId {
            assert!(extension > 0);
            let zero = self.immediate(extension, 0);
            self.concat(vec![zero, source])
        }

        fn expensive_value(&mut self, seed: u64, factor: RegisterId) -> RegisterId {
            let mut value = self.immediate(64, seed);
            for _ in 0..6 {
                value = self.binary(64, value, BinaryOp::Mul, factor);
            }
            value
        }

        fn mux(
            &mut self,
            cond: RegisterId,
            true_value: RegisterId,
            false_value: RegisterId,
        ) -> RegisterId {
            let width = self.register_map[&true_value].width();
            let dst = self.register(width);
            self.instructions
                .push(SIRInstruction::Mux(dst, cond, true_value, false_value));
            dst
        }

        fn ident(&mut self, source: RegisterId) -> RegisterId {
            let dst = self.register(self.register_map[&source].width());
            self.instructions
                .push(SIRInstruction::Unary(dst, UnaryOp::Ident, source));
            dst
        }

        fn observe(&mut self, source: RegisterId) {
            self.instructions.push(SIRInstruction::Store(
                address(usize::MAX),
                SIROffset::Static(0),
                self.register_map[&source].width(),
                source,
                Vec::new(),
                Vec::new(),
            ));
        }

        fn finish(self, params: Vec<RegisterId>) -> ExecutionUnit<RegionedAbsoluteAddr> {
            let block = BasicBlock {
                id: BlockId(0),
                params,
                instructions: self.instructions,
                terminator: SIRTerminator::Return,
            };
            ExecutionUnit {
                entry_block_id: BlockId(0),
                blocks: [(BlockId(0), block)].into_iter().collect(),
                register_map: self.register_map,
            }
        }
    }

    fn address(instance: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(instance),
            var_id: VarId::default(),
        }
    }

    fn expensive_duplicate_fixture() -> (ExecutionUnit<RegionedAbsoluteAddr>, RegisterId, RegisterId)
    {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(4);
        let factor = builder.immediate(64, 3);
        let mut previous = builder.expensive_value(17, factor);
        // The final stage is outermost and deliberately shadows key 1.
        for (stage, key) in [0, 2, 5, 7, 9, 11, 15, 2].into_iter().enumerate() {
            let cond = builder.exact_condition(selector, key, BinaryOp::EqWildcard);
            let value = builder.expensive_value(30 + stage as u64, factor);
            previous = builder.mux(cond, value, previous);
        }
        let output = builder.ident(previous);
        builder.observe(output);
        (builder.finish(vec![selector]), selector, output)
    }

    fn cross_block_case_fixture(
        case_count: usize,
        keep_first_condition_live: bool,
    ) -> (
        ExecutionUnit<RegionedAbsoluteAddr>,
        RegisterId,
        RegisterId,
        Vec<RegisterId>,
    ) {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(7);
        let default_value = builder.immediate(64, 0xf0);
        let mut conditions = Vec::with_capacity(case_count);
        let mut values = Vec::with_capacity(case_count);
        for key in 0..case_count {
            conditions.push(builder.exact_condition(selector, key as u64, BinaryOp::EqWildcard));
            values.push(builder.immediate(64, 0x100 + key as u64));
        }
        if keep_first_condition_live {
            builder.instructions.push(SIRInstruction::Store(
                address(usize::MAX - 1),
                SIROffset::Static(0),
                1,
                conditions[0],
                Vec::new(),
                Vec::new(),
            ));
        }

        let split = builder.instructions.len();
        let mut previous = default_value;
        for (&condition, &value) in conditions.iter().zip(&values) {
            previous = builder.mux(condition, value, previous);
        }
        let output = builder.ident(previous);
        builder.observe(output);
        let mut eu = builder.finish(vec![selector]);
        let successor_instructions = eu
            .blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions
            .split_off(split);
        eu.blocks.get_mut(&BlockId(0)).unwrap().terminator =
            SIRTerminator::Jump(BlockId(1), Vec::new());
        eu.blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: successor_instructions,
                terminator: SIRTerminator::Return,
            },
        );
        (eu, selector, output, conditions)
    }

    fn has_definition(eu: &ExecutionUnit<RegionedAbsoluteAddr>, reg: RegisterId) -> bool {
        eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| def_reg(inst) == Some(reg))
        })
    }

    #[test]
    fn lowers_prioritized_duplicates_to_balanced_semantic_dispatch() {
        let (mut eu, selector, output) = expensive_duplicate_fixture();
        eu.verify();
        let expected = (0..16)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        eu.verify();
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
        }));
        assert!(maximum_branch_depth(&eu, eu.entry_block_id) <= 4);
        let actual = (0..16)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        // The outer duplicate is stage 7 (seed 37), rather than stage 0.
        assert_eq!(actual[2], BigUint::from(37u8) * BigUint::from(3u8).pow(6));
    }

    #[test]
    fn lowers_grouped_keys_across_exact_zero_extensions() {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(4);
        let selector_5 = builder.zero_extend(selector, 1);
        let selector_6 = builder.zero_extend(selector, 2);
        let factor = builder.immediate(64, 3);
        let mut previous = builder.expensive_value(10, factor);

        let condition_13 = [
            builder.exact_condition(selector_5, 1, BinaryOp::EqWildcard),
            builder.exact_condition(selector_5, 3, BinaryOp::EqWildcard),
        ];
        let condition_13 = builder.grouped_condition(&condition_13);
        let value_13 = builder.expensive_value(20, factor);
        previous = builder.mux(condition_13, value_13, previous);

        let condition_246 = [
            builder.exact_condition_with_zero_extended_key(selector_6, 4, 2, BinaryOp::EqWildcard),
            builder.exact_condition_with_zero_extended_key(selector_6, 4, 4, BinaryOp::EqWildcard),
            builder.exact_condition_with_zero_extended_key(selector_6, 4, 6, BinaryOp::EqWildcard),
        ];
        let condition_246 = builder.grouped_condition(&condition_246);
        let value_246 = builder.expensive_value(30, factor);
        previous = builder.mux(condition_246, value_246, previous);

        let condition_89 = [
            builder.exact_condition(selector, 8, BinaryOp::Eq),
            builder.exact_condition(selector, 9, BinaryOp::Eq),
        ];
        let condition_89 = builder.grouped_condition(&condition_89);
        let value_89 = builder.expensive_value(40, factor);
        previous = builder.mux(condition_89, value_89, previous);

        let output = builder.ident(previous);
        builder.observe(output);
        let mut eu = builder.finish(vec![selector]);
        eu.verify();
        let expected = (0..16)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        eu.verify();
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
        }));
        let actual = (0..16)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn grouped_duplicates_and_overlaps_preserve_outer_priority() {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(4);
        let factor = builder.immediate(64, 3);
        let mut previous = builder.expensive_value(10, factor);

        for (keys, seed) in [
            (&[1, 2, 2][..], 20),
            (&[2, 3, 3][..], 30),
            (&[4, 5][..], 40),
        ] {
            let leaves = keys
                .iter()
                .map(|&key| builder.exact_condition(selector, key, BinaryOp::EqWildcard))
                .collect::<Vec<_>>();
            let condition = builder.grouped_condition(&leaves);
            let value = builder.expensive_value(seed, factor);
            previous = builder.mux(condition, value, previous);
        }
        let output = builder.ident(previous);
        builder.observe(output);
        let mut eu = builder.finish(vec![selector]);
        eu.verify();
        let expected = (0..16)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        eu.verify();
        let actual = (0..16)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        assert_eq!(actual[2], BigUint::from(30u8) * BigUint::from(3u8).pow(6));
    }

    #[test]
    fn lowers_bitwise_or_with_exact_concat_and_slice_keys() {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(4);
        let sliced_selector = builder.slice(selector, 0, 4);

        let key_high = builder.immediate(2, 0);
        let key_low = builder.immediate(2, 1);
        let concat_key = builder.concat(vec![key_high, key_low]);
        let concat_condition = builder.binary(1, sliced_selector, BinaryOp::Eq, concat_key);

        let key_source = builder.immediate(8, 0b10_10_01);
        let slice_key = builder.slice(key_source, 2, 4);
        let slice_condition = builder.binary(1, selector, BinaryOp::Eq, slice_key);
        let condition = builder.binary(1, concat_condition, BinaryOp::Or, slice_condition);

        let factor = builder.immediate(64, 3);
        let make_value = |builder: &mut FixtureBuilder, seed| {
            let mut value = builder.immediate(64, seed);
            for _ in 0..16 {
                value = builder.binary(64, value, BinaryOp::Mul, factor);
            }
            value
        };
        let default_value = make_value(&mut builder, 10);
        let case_value = make_value(&mut builder, 20);
        let result = builder.mux(condition, case_value, default_value);
        let output = builder.ident(result);
        builder.observe(output);
        let mut eu = builder.finish(vec![selector]);
        eu.verify();
        let expected = (0..16)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        eu.verify();
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
        }));
        let actual = (0..16)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn rejects_grouped_mixed_selectors_and_nonzero_extensions() {
        let mut mixed = FixtureBuilder::new();
        let selector_a = mixed.register(4);
        let selector_b = mixed.register(4);
        let factor = mixed.immediate(64, 3);
        let mut previous = mixed.expensive_value(1, factor);
        for key in 0..4 {
            let leaves = [
                mixed.exact_condition(selector_a, key, BinaryOp::EqWildcard),
                mixed.exact_condition(selector_b, key + 4, BinaryOp::EqWildcard),
            ];
            let condition = mixed.grouped_condition(&leaves);
            let value = mixed.expensive_value(20 + key, factor);
            previous = mixed.mux(condition, value, previous);
        }
        mixed.ident(previous);
        let mut mixed = mixed.finish(vec![selector_a, selector_b]);
        mixed.verify();
        let original = mixed.clone();

        SparseCaseDispatchPass::default().run(&mut mixed, &PassOptions::default());

        assert_eq!(mixed.blocks, original.blocks);
        assert_eq!(mixed.register_map, original.register_map);

        let mut extended = FixtureBuilder::new();
        let selector = extended.register(4);
        let zero = extended.immediate(1, 0);
        let one = extended.immediate(1, 1);
        let zero_extended = extended.concat(vec![zero, selector]);
        let nonzero_extended = extended.concat(vec![one, selector]);
        let factor = extended.immediate(64, 3);
        let mut previous = extended.expensive_value(1, factor);
        for key in 0..4 {
            let leaves = [
                extended.exact_condition(zero_extended, key, BinaryOp::Eq),
                extended.exact_condition(nonzero_extended, 16 + key, BinaryOp::Eq),
            ];
            let condition = extended.grouped_condition(&leaves);
            let value = extended.expensive_value(30 + key, factor);
            previous = extended.mux(condition, value, previous);
        }
        extended.ident(previous);
        let mut extended = extended.finish(vec![selector]);
        extended.verify();
        let original = extended.clone();

        SparseCaseDispatchPass::default().run(&mut extended, &PassOptions::default());

        assert_eq!(extended.blocks, original.blocks);
        assert_eq!(extended.register_map, original.register_map);
    }

    #[test]
    fn rejects_grouped_signed_width_changing_identity_casts() {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(4);
        builder.register_map.insert(
            selector,
            RegisterType::Bit {
                width: 4,
                signed: true,
            },
        );
        let cast = builder.register(8);
        builder.register_map.insert(
            cast,
            RegisterType::Bit {
                width: 8,
                signed: true,
            },
        );
        builder
            .instructions
            .push(SIRInstruction::Unary(cast, UnaryOp::Ident, selector));
        let factor = builder.immediate(64, 3);
        let mut previous = builder.expensive_value(1, factor);
        for key in 0..4 {
            let leaves = [
                builder.exact_condition(selector, key, BinaryOp::Eq),
                builder.exact_condition(cast, key, BinaryOp::Eq),
            ];
            let condition = builder.grouped_condition(&leaves);
            let value = builder.expensive_value(40 + key, factor);
            previous = builder.mux(condition, value, previous);
        }
        builder.ident(previous);
        let mut eu = builder.finish(vec![selector]);
        eu.verify();
        let original = eu.clone();

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks, original.blocks);
        assert_eq!(eu.register_map, original.register_map);
    }

    #[test]
    fn rejects_mixed_selectors_and_masked_or_reversed_wildcards() {
        let mut mixed = FixtureBuilder::new();
        let selector_a = mixed.register(4);
        let selector_b = mixed.register(4);
        let factor = mixed.immediate(64, 3);
        let mut previous = mixed.expensive_value(1, factor);
        for key in 0..8 {
            let selector = if key % 2 == 0 { selector_a } else { selector_b };
            let cond = mixed.exact_condition(selector, key, BinaryOp::EqWildcard);
            let value = mixed.expensive_value(10 + key, factor);
            previous = mixed.mux(cond, value, previous);
        }
        mixed.ident(previous);
        let mut mixed = mixed.finish(vec![selector_a, selector_b]);
        let original = mixed.clone();
        SparseCaseDispatchPass::default().run(&mut mixed, &PassOptions::default());
        assert_eq!(mixed.blocks, original.blocks);

        let mut inexact = FixtureBuilder::new();
        let selector = inexact.register(4);
        let factor = inexact.immediate(64, 3);
        let mut previous = inexact.expensive_value(1, factor);
        for key in 0..4 {
            let key_reg = inexact.immediate_value(
                4,
                SIRValue::new_four_state(key, if key % 2 == 0 { 1u8 } else { 0u8 }),
            );
            let cond = if key % 2 == 0 {
                inexact.binary(1, selector, BinaryOp::EqWildcard, key_reg)
            } else {
                // A constant on the wildcard LHS is not an exact case key.
                inexact.binary(1, key_reg, BinaryOp::EqWildcard, selector)
            };
            let value = inexact.expensive_value(20 + key, factor);
            previous = inexact.mux(cond, value, previous);
        }
        inexact.ident(previous);
        let mut inexact = inexact.finish(vec![selector]);
        let original = inexact.clone();
        SparseCaseDispatchPass::default().run(&mut inexact, &PassOptions::default());
        assert_eq!(inexact.blocks, original.blocks);
    }

    #[test]
    fn rejects_width_changing_constant_casts_and_wide_comparison_results() {
        let mut cast_key = FixtureBuilder::new();
        let selector = cast_key.register(8);
        let factor = cast_key.immediate(64, 3);
        let mut previous = cast_key.expensive_value(1, factor);
        for raw_key in 8..12 {
            let key_source = cast_key.immediate(4, raw_key);
            cast_key.register_map.insert(
                key_source,
                RegisterType::Bit {
                    width: 4,
                    signed: true,
                },
            );
            let key_cast = cast_key.register(8);
            cast_key
                .instructions
                .push(SIRInstruction::Unary(key_cast, UnaryOp::Ident, key_source));
            let cond = cast_key.binary(1, selector, BinaryOp::Eq, key_cast);
            let value = cast_key.expensive_value(20 + raw_key, factor);
            previous = cast_key.mux(cond, value, previous);
        }
        cast_key.ident(previous);
        let mut cast_key = cast_key.finish(vec![selector]);
        cast_key.verify();
        let original = cast_key.clone();

        SparseCaseDispatchPass::default().run(&mut cast_key, &PassOptions::default());

        assert_eq!(cast_key.blocks, original.blocks);

        let mut cast_selector = FixtureBuilder::new();
        let selector_source = cast_selector.register(4);
        cast_selector.register_map.insert(
            selector_source,
            RegisterType::Bit {
                width: 4,
                signed: true,
            },
        );
        let selector_cast = cast_selector.register(8);
        cast_selector.instructions.push(SIRInstruction::Unary(
            selector_cast,
            UnaryOp::Ident,
            selector_source,
        ));
        let factor = cast_selector.immediate(64, 3);
        let mut previous = cast_selector.expensive_value(1, factor);
        for key in 8..12 {
            // Verifier-valid today: the backend compares at the 8-bit common
            // width, while incorrectly stripping selector_cast would make the
            // matcher compare the 4-bit source against this 4-bit key.
            let key_reg = cast_selector.immediate(4, key);
            let cond = cast_selector.binary(1, selector_cast, BinaryOp::Eq, key_reg);
            let value = cast_selector.expensive_value(20 + key, factor);
            previous = cast_selector.mux(cond, value, previous);
        }
        cast_selector.ident(previous);
        let mut cast_selector = cast_selector.finish(vec![selector_source]);
        cast_selector.verify();
        let original = cast_selector.clone();

        SparseCaseDispatchPass::default().run(&mut cast_selector, &PassOptions::default());

        assert_eq!(cast_selector.blocks, original.blocks);

        let mut wide_compare = FixtureBuilder::new();
        let selector = wide_compare.register(4);
        wide_compare.register_map.insert(
            selector,
            RegisterType::Bit {
                width: 4,
                signed: true,
            },
        );
        let factor = wide_compare.immediate(64, 3);
        let mut previous = wide_compare.expensive_value(1, factor);
        for key in 8..12 {
            let key_reg = wide_compare.immediate(4, key);
            // The backend's common comparison width is 8 here, so the signed
            // selector is extended before equality is evaluated.
            let cond = wide_compare.binary(8, selector, BinaryOp::Eq, key_reg);
            let value = wide_compare.expensive_value(30 + key, factor);
            previous = wide_compare.mux(cond, value, previous);
        }
        wide_compare.ident(previous);
        let mut wide_compare = wide_compare.finish(vec![selector]);
        wide_compare.verify();
        let original = wide_compare.clone();

        SparseCaseDispatchPass::default().run(&mut wide_compare, &PassOptions::default());

        assert_eq!(wide_compare.blocks, original.blocks);
    }

    #[test]
    fn profitability_rejects_a_cheap_case_chain() {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(2);
        let mut previous = builder.immediate(64, 100);
        for key in 0..4 {
            let cond = builder.exact_condition(selector, key, BinaryOp::Eq);
            let value = builder.immediate(64, 10 + key);
            previous = builder.mux(cond, value, previous);
        }
        builder.ident(previous);
        let mut eu = builder.finish(vec![selector]);
        let original = eu.clone();

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks, original.blocks);
    }

    #[test]
    fn counts_and_prunes_dead_cross_block_exact_conditions() {
        let (mut eu, selector, output, conditions) = cross_block_case_fixture(71, false);
        eu.verify();
        let expected = (0..128)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        eu.verify();
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
        }));
        assert!(
            conditions
                .iter()
                .all(|condition| !has_definition(&eu, *condition))
        );
        let actual = (0..128)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn retains_a_cross_block_condition_with_an_external_effect_use() {
        let (mut eu, selector, output, conditions) = cross_block_case_fixture(71, true);
        eu.verify();
        let expected = (0..128)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();
        let shared = conditions[0];

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        eu.verify();
        assert!(has_definition(&eu, shared));
        assert!(
            conditions[1..]
                .iter()
                .all(|condition| !has_definition(&eu, *condition))
        );
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(
                |inst| matches!(inst, SIRInstruction::Store(_, _, _, src, _, _) if *src == shared),
            )
        }));
        let actual = (0..128)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn cross_block_dead_cost_does_not_force_an_unprofitable_dispatch() {
        let (mut eu, selector, output, _) = cross_block_case_fixture(2, false);
        eu.verify();
        let expected = (0..8)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();
        let original = eu.clone();

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        eu.verify();
        assert_eq!(eu.blocks, original.blocks);
        assert_eq!(eu.register_map, original.register_map);
        let actual = (0..8)
            .map(|value| evaluate(&eu, selector, value, output))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn does_not_partially_rewrite_a_full_domain_constant_lookup() {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(7);
        let guard = builder.register(1);
        let mut previous = builder.immediate(8, 0xee);
        let mut shared_middle = None;
        for key in 0..128 {
            let cond = builder.exact_condition(selector, key, BinaryOp::EqWildcard);
            let value = builder.immediate(8, (key * 3) & 0xff);
            previous = builder.mux(cond, value, previous);
            if key == 63 {
                shared_middle = Some(previous);
            }
        }
        // Embed the dense chain under a non-case outer mux and give one
        // intermediate stage an outside use.  Protecting only the observable
        // root, or stopping the protection walk at shared intermediates,
        // would let an inner sparse prefix destroy the table shape.
        let outer_value = builder.immediate(8, 0xa5);
        let outer = builder.mux(guard, outer_value, previous);
        builder.ident(outer);
        builder.ident(shared_middle.unwrap());
        let mut eu = builder.finish(vec![selector, guard]);
        eu.verify();
        let original = eu.clone();

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks, original.blocks);
        assert_eq!(eu.register_map, original.register_map);
    }

    #[test]
    fn rejects_predicate_casts_through_zero_width() {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(4);
        let factor = builder.immediate(64, 3);
        let mut previous = builder.expensive_value(1, factor);
        for key in 0..4 {
            let raw = builder.exact_condition(selector, key, BinaryOp::Eq);
            let zero_width = builder.register(0);
            builder
                .instructions
                .push(SIRInstruction::Unary(zero_width, UnaryOp::Ident, raw));
            let cond = builder.register(1);
            builder
                .instructions
                .push(SIRInstruction::Unary(cond, UnaryOp::Ident, zero_width));
            let value = builder.expensive_value(20 + key, factor);
            previous = builder.mux(cond, value, previous);
        }
        builder.ident(previous);
        let mut eu = builder.finish(vec![selector]);
        let original = eu.clone();

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks, original.blocks);
        assert_eq!(eu.register_map, original.register_map);
    }

    #[test]
    fn keeps_shared_and_alias_sensitive_arm_definitions_in_the_head() {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(4);
        let factor = builder.immediate(64, 3);
        let shared = builder.expensive_value(9, factor);
        let loaded = builder.register(64);
        builder.instructions.push(SIRInstruction::Load(
            loaded,
            address(0),
            SIROffset::Static(usize::MAX),
            64,
        ));
        let alias_sensitive = builder.binary(64, loaded, BinaryOp::Add, factor);
        let stored = builder.immediate(64, 44);
        builder.instructions.push(SIRInstruction::Store(
            address(0),
            SIROffset::Static(usize::MAX),
            64,
            stored,
            Vec::new(),
            Vec::new(),
        ));
        let mut previous = builder.expensive_value(1, factor);
        for key in 0..8 {
            let cond = builder.exact_condition(selector, key * 2, BinaryOp::Eq);
            let value = match key {
                0 => shared,
                1 => alias_sensitive,
                _ => builder.expensive_value(20 + key, factor),
            };
            previous = builder.mux(cond, value, previous);
        }
        let selected = builder.ident(previous);
        // The second use makes `shared` ineligible for arm sinking.
        builder.binary(64, shared, BinaryOp::Add, selected);
        let mut eu = builder.finish(vec![selector]);

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        eu.verify();
        let head = &eu.blocks[&BlockId(0)];
        assert!(
            head.instructions
                .iter()
                .any(|inst| def_reg(inst) == Some(shared))
        );
        assert!(
            head.instructions
                .iter()
                .any(|inst| def_reg(inst) == Some(loaded))
        );
        assert!(matches!(head.terminator, SIRTerminator::Branch { .. }));
    }

    #[test]
    fn preserves_arm_definitions_with_uses_in_a_successor_block() {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(4);
        let factor = builder.immediate(64, 3);
        let shared = builder.expensive_value(9, factor);
        let mut previous = builder.expensive_value(1, factor);
        for key in 0..8 {
            let cond = builder.exact_condition(selector, key * 2, BinaryOp::Eq);
            let value = if key == 0 {
                shared
            } else {
                builder.expensive_value(20 + key, factor)
            };
            previous = builder.mux(cond, value, previous);
        }
        let external_result = builder.register(64);
        let mut eu = builder.finish(vec![selector]);
        eu.blocks.get_mut(&BlockId(0)).unwrap().terminator =
            SIRTerminator::Jump(BlockId(1), Vec::new());
        eu.blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Binary(external_result, shared, BinaryOp::Add, previous),
                    SIRInstruction::Store(
                        address(usize::MAX),
                        SIROffset::Static(0),
                        64,
                        external_result,
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        eu.verify();

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        eu.verify();
        assert!(
            eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|inst| def_reg(inst) == Some(shared))
        );
        assert!(
            eu.blocks[&BlockId(1)]
                .instructions
                .iter()
                .any(|inst| def_reg(inst) == Some(external_result))
        );
    }

    #[test]
    fn anticipates_stable_storage_aliases_when_sinking_loads() {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(4);
        let factor = builder.immediate(64, 3);
        let address_a = address(10);
        let address_b = address(11);
        let loaded_b = builder.register(64);
        builder.instructions.push(SIRInstruction::Load(
            loaded_b,
            address_b,
            SIROffset::Static(0),
            64,
        ));
        let alias_sensitive = builder.binary(64, loaded_b, BinaryOp::Add, factor);
        let duplicate_value = builder.immediate(64, 44);
        builder.instructions.push(SIRInstruction::Store(
            address_a,
            SIROffset::Static(0),
            64,
            duplicate_value,
            Vec::new(),
            Vec::new(),
        ));

        let mut previous = builder.expensive_value(1, factor);
        for key in 0..8 {
            let cond = builder.exact_condition(selector, key * 2, BinaryOp::Eq);
            let value = if key == 0 {
                alias_sensitive
            } else {
                builder.expensive_value(20 + key, factor)
            };
            previous = builder.mux(cond, value, previous);
        }
        // This store is after the mux root, so exact-address checking alone
        // does not stop the B load from crossing Store(A).  Layout later
        // removes Store(B) if its precomputed B -> A alias is validated.
        builder.instructions.push(SIRInstruction::Store(
            address_b,
            SIROffset::Static(0),
            64,
            duplicate_value,
            Vec::new(),
            Vec::new(),
        ));
        builder.ident(previous);
        let mut eu = builder.finish(vec![selector]);
        let aliases = [(address_b.absolute_addr(), address_a.absolute_addr())]
            .into_iter()
            .collect();
        let pass = SparseCaseDispatchPass::new(&aliases);

        pass.run(&mut eu, &PassOptions::default());

        eu.verify();
        assert!(
            eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|inst| def_reg(inst) == Some(loaded_b))
        );
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch { .. }
        ));
    }

    #[test]
    fn stable_alias_classes_are_transitive_but_do_not_cross_regions() {
        let stable_a = address(20);
        let stable_b = address(21);
        let stable_c = address(22);
        let aliases = [
            (stable_b.absolute_addr(), stable_a.absolute_addr()),
            (stable_c.absolute_addr(), stable_b.absolute_addr()),
        ]
        .into_iter()
        .collect();
        let pass = SparseCaseDispatchPass::new(&aliases);
        fn access(addr: &RegionedAbsoluteAddr) -> MemAccess<'_> {
            MemAccess {
                addr,
                offset: Some(0),
                width: 1,
            }
        }
        assert!(memory_may_alias(
            access(&stable_a),
            access(&stable_c),
            &pass.stable_alias_class,
        ));

        let mut working_a = stable_a;
        working_a.region = crate::ir::WORKING_REGION;
        let mut working_c = stable_c;
        working_c.region = crate::ir::WORKING_REGION;
        assert!(!memory_may_alias(
            access(&working_a),
            access(&working_c),
            &pass.stable_alias_class,
        ));
    }

    #[test]
    fn leaves_exact_chains_unchanged_in_four_state_mode() {
        let (mut eu, _, _) = expensive_duplicate_fixture();
        let original = eu.clone();
        let options = PassOptions {
            four_state: true,
            ..PassOptions::default()
        };

        SparseCaseDispatchPass::default().run(&mut eu, &options);

        assert_eq!(eu.blocks, original.blocks);
    }

    #[test]
    fn id_exhaustion_rejects_the_rewrite_without_panicking_or_mutating() {
        let (mut block_ids, _, _) = expensive_duplicate_fixture();
        let mut block = block_ids.blocks.remove(&BlockId(0)).unwrap();
        block.id = BlockId(usize::MAX);
        block_ids.entry_block_id = block.id;
        block_ids.blocks.insert(block.id, block);
        let original = block_ids.clone();

        SparseCaseDispatchPass::default().run(&mut block_ids, &PassOptions::default());

        assert_eq!(block_ids.blocks, original.blocks);
        assert_eq!(block_ids.register_map, original.register_map);

        let (mut register_ids, _, _) = expensive_duplicate_fixture();
        register_ids.register_map.insert(
            RegisterId(usize::MAX),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        let original = register_ids.clone();

        SparseCaseDispatchPass::default().run(&mut register_ids, &PassOptions::default());

        assert_eq!(register_ids.blocks, original.blocks);
        assert_eq!(register_ids.register_map, original.register_map);
    }

    #[test]
    fn enormous_width_profitability_saturates_instead_of_overflowing() {
        let mut builder = FixtureBuilder::new();
        let selector = builder.register(2);
        let factor = builder.immediate(usize::MAX, 3);
        let make_value = |builder: &mut FixtureBuilder, seed| {
            let mut value = builder.immediate(usize::MAX, seed);
            for _ in 0..100 {
                value = builder.binary(usize::MAX, value, BinaryOp::Div, factor);
            }
            value
        };
        let mut previous = make_value(&mut builder, 17);
        for key in 0..2 {
            let cond = builder.exact_condition(selector, key, BinaryOp::Eq);
            let value = make_value(&mut builder, 30 + key);
            previous = builder.mux(cond, value, previous);
        }
        builder.ident(previous);
        let mut eu = builder.finish(vec![selector]);
        eu.verify();

        SparseCaseDispatchPass::default().run(&mut eu, &PassOptions::default());

        eu.verify();
        assert!(eu.blocks.len() > 1);
    }

    fn evaluate(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        selector: RegisterId,
        selector_value: u64,
        output: RegisterId,
    ) -> BigUint {
        let mut values = HashMap::<RegisterId, BigUint>::default();
        values.insert(selector, BigUint::from(selector_value));
        let mut block_id = eu.entry_block_id;

        for _ in 0..10_000 {
            let block = &eu.blocks[&block_id];
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Imm(dst, value) => {
                        assert!(value.mask.is_zero());
                        values.insert(
                            *dst,
                            truncate_to_width(value.payload.clone(), eu.register_map[dst].width()),
                        );
                    }
                    SIRInstruction::Binary(dst, lhs, op, rhs) => {
                        let lhs = values[lhs].clone();
                        let rhs = values[rhs].clone();
                        let value = match op {
                            BinaryOp::Eq | BinaryOp::EqWildcard => {
                                BigUint::from(u8::from(lhs == rhs))
                            }
                            BinaryOp::LogicOr => {
                                BigUint::from(u8::from(!lhs.is_zero() || !rhs.is_zero()))
                            }
                            BinaryOp::Or => lhs | rhs,
                            BinaryOp::LtU => BigUint::from(u8::from(lhs < rhs)),
                            BinaryOp::Mul => lhs * rhs,
                            BinaryOp::Add => lhs + rhs,
                            _ => panic!("unsupported test operation {op:?}"),
                        };
                        values.insert(*dst, truncate_to_width(value, eu.register_map[dst].width()));
                    }
                    SIRInstruction::Unary(dst, UnaryOp::Ident, src) => {
                        values.insert(*dst, values[src].clone());
                    }
                    SIRInstruction::Concat(dst, args) => {
                        let mut value = BigUint::zero();
                        for arg in args {
                            value <<= eu.register_map[arg].width();
                            value |= values[arg].clone();
                        }
                        values.insert(*dst, value);
                    }
                    SIRInstruction::Slice(dst, src, offset, width) => {
                        let value = values[src].clone() >> offset;
                        values.insert(*dst, truncate_to_width(value, *width));
                    }
                    SIRInstruction::Mux(dst, cond, true_value, false_value) => {
                        let selected = if values[cond].is_zero() {
                            false_value
                        } else {
                            true_value
                        };
                        values.insert(*dst, values[selected].clone());
                    }
                    SIRInstruction::Store(_, _, _, src, _, _) => {
                        assert!(values.contains_key(src));
                    }
                    other => panic!("unsupported test instruction {other:?}"),
                }
            }
            match &block.terminator {
                SIRTerminator::Jump(target, args) => {
                    assign_edge_params(eu, &mut values, *target, args);
                    block_id = *target;
                }
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    let edge = if values[cond].is_zero() {
                        false_block
                    } else {
                        true_block
                    };
                    assign_edge_params(eu, &mut values, edge.0, &edge.1);
                    block_id = edge.0;
                }
                SIRTerminator::Return => return values[&output].clone(),
                SIRTerminator::Error(code) => panic!("unexpected error {code}"),
            }
        }
        panic!("test evaluator did not terminate")
    }

    fn assign_edge_params(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        values: &mut HashMap<RegisterId, BigUint>,
        target: BlockId,
        args: &[RegisterId],
    ) {
        let incoming = args
            .iter()
            .map(|arg| values[arg].clone())
            .collect::<Vec<_>>();
        for (&param, value) in eu.blocks[&target].params.iter().zip(incoming) {
            values.insert(param, value);
        }
    }

    fn maximum_branch_depth(eu: &ExecutionUnit<RegionedAbsoluteAddr>, block_id: BlockId) -> usize {
        fn visit(
            eu: &ExecutionUnit<RegionedAbsoluteAddr>,
            block_id: BlockId,
            memo: &mut HashMap<BlockId, usize>,
        ) -> usize {
            if let Some(&depth) = memo.get(&block_id) {
                return depth;
            }
            let depth = match &eu.blocks[&block_id].terminator {
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => 1 + visit(eu, true_block.0, memo).max(visit(eu, false_block.0, memo)),
                SIRTerminator::Jump(target, _) => visit(eu, *target, memo),
                SIRTerminator::Return | SIRTerminator::Error(_) => 0,
            };
            memo.insert(block_id, depth);
            depth
        }
        visit(eu, block_id, &mut HashMap::default())
    }
}
