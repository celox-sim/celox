//! Recover control dependence after expression lowering has eagerly
//! materialized a shared mux arm.
//!
//! A single-output mux pass cannot sink a DAG shared by several guarded
//! outputs: every individual root appears to have external uses.  This pass
//! treats all values owned by one already-existing CFG edge as a region.  It
//! distributes stores whose values are selected by the branch condition and
//! moves the closed, pure true-edge region behind that branch.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{def_reg, normalize_branch_condition};
use crate::ir::*;
use crate::optimizer::PassOptions;
use crate::{HashMap, HashSet};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) struct GuardedRegionSinkingPass;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UseSite {
    Instruction { block: BlockId, index: usize },
    BranchCondition { block: BlockId },
    TrueEdgeArgument { block: BlockId },
    FalseEdgeArgument { block: BlockId },
    JumpArgument { block: BlockId },
}

impl UseSite {
    fn block(self) -> BlockId {
        match self {
            Self::Instruction { block, .. }
            | Self::BranchCondition { block }
            | Self::TrueEdgeArgument { block }
            | Self::FalseEdgeArgument { block }
            | Self::JumpArgument { block } => block,
        }
    }
}

#[derive(Clone)]
struct DistributedStore {
    index: usize,
    mux_index: usize,
    mux_result: RegisterId,
    true_value: RegisterId,
    false_value: RegisterId,
}

#[derive(Clone)]
struct GuardedRegionPlan {
    block_id: BlockId,
    condition: RegisterId,
    true_target: (BlockId, Vec<RegisterId>),
    false_target: (BlockId, Vec<RegisterId>),
    moved: HashSet<usize>,
    distributed: Vec<DistributedStore>,
    removable_muxes: HashSet<usize>,
}

/// One reverse-if-conversion of a pure, single-block same-predicate region.
///
/// `true_owned` and `false_owned` are closed backwards slices which are used
/// only by the corresponding Mux arms. `cofactor` is the forward slice which
/// depends on at least one removed Mux result. It is rebuilt once on each edge,
/// so only `live_outs` cross the merge instead of every individual Mux result.
#[derive(Clone)]
struct SamePredicatePlan {
    block_id: BlockId,
    segment_start: usize,
    segment_end: usize,
    condition: RegisterId,
    muxes: HashSet<usize>,
    true_muxes: HashSet<usize>,
    false_muxes: HashSet<usize>,
    true_owned: HashSet<usize>,
    false_owned: HashSet<usize>,
    cofactor: HashSet<usize>,
    true_cofactor: HashSet<usize>,
    false_cofactor: HashSet<usize>,
    live_outs: Vec<RegisterId>,
    net_benefit_scaled: u128,
}

impl ExecutionUnitPass for GuardedRegionSinkingPass {
    fn name(&self) -> &'static str {
        "guarded_region_sinking"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        // A four-state Mux bitwise-merges its arms for an X/Z condition, while
        // control flow selects one edge. No structural proof below authorizes
        // that conversion in four-state mode.
        if options.four_state || eu.verify_result().is_err() {
            return;
        }

        // First recover a branch shared by all Muxes with the same predicate
        // in a pure SIR region. This is deliberately planned from the input
        // CFG and applies at most one best region per input block. Generated
        // blocks are not candidates in this run, so termination needs neither
        // an iteration limit nor a function-size budget.
        form_same_predicate_regions(eu);

        // Recompute CFG facts after region formation. The existing edge
        // sinking transform remains independent and can consume either an
        // original branch or a branch introduced above.

        let predecessors = predecessor_map(eu);
        let dominators = Dominators::compute(eu, &predecessors);
        let uses = collect_uses(eu);
        let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
        block_ids.sort_unstable_by_key(|id| id.0);

        // Plans are built only from the input CFG.  Blocks generated below are
        // intentionally not revisited in this run, which makes termination
        // independent of function size or any iteration budget.
        let plans = block_ids
            .into_iter()
            .filter_map(|block_id| plan_block(eu, block_id, &predecessors, &dominators, &uses))
            .collect::<Vec<_>>();
        if plans.is_empty() {
            return;
        }
        // Reserve the complete block-id range before mutating the EU.  An ID
        // overflow therefore leaves the input byte-for-byte unchanged.
        let Some(additional_blocks) = plans.len().checked_mul(2) else {
            return;
        };
        let max_block = eu.blocks.keys().map(|id| id.0).max().unwrap_or(0);
        let Some(first_new_block) = max_block.checked_add(1) else {
            return;
        };
        let Some(last_new_block) = max_block.checked_add(additional_blocks) else {
            return;
        };
        if last_new_block > u32::MAX as usize {
            return;
        }
        if std::env::var_os("CELOX_PASS_TIMING").is_some() {
            let moved = plans.iter().map(|plan| plan.moved.len()).sum::<usize>();
            let stores = plans
                .iter()
                .map(|plan| plan.distributed.len())
                .sum::<usize>();
            eprintln!(
                "[guarded-region-sinking] regions={} moved_instructions={moved} distributed_stores={stores}",
                plans.len(),
            );
        }

        let mut reg_counter = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
        for (ordinal, plan) in plans.into_iter().enumerate() {
            let true_id = BlockId(first_new_block + ordinal * 2);
            let false_id = BlockId(first_new_block + ordinal * 2 + 1);
            apply_plan(eu, plan, true_id, false_id, &mut reg_counter);
        }
    }
}

fn form_same_predicate_regions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let uses = collect_uses(eu);
    let mut input_blocks = eu.blocks.keys().copied().collect::<Vec<_>>();
    input_blocks.sort_unstable_by_key(|id| id.0);
    let plans = input_blocks
        .into_iter()
        .filter_map(|block| best_same_predicate_plan(eu, block, &uses))
        .collect::<Vec<_>>();
    if plans.is_empty() {
        return;
    }

    // A region adds true, false, and merge blocks. Reserve all IDs before
    // changing the EU so overflow leaves the input untouched.
    let Some(additional_blocks) = plans.len().checked_mul(3) else {
        return;
    };
    let max_block = eu.blocks.keys().map(|id| id.0).max().unwrap_or(0);
    let Some(first_new_block) = max_block.checked_add(1) else {
        return;
    };
    let Some(last_new_block) = max_block.checked_add(additional_blocks) else {
        return;
    };
    if last_new_block > u32::MAX as usize {
        return;
    }

    // Every moved or cofactored instruction is rebuilt with one edge-local
    // destination. Branch normalization can allocate at most two more values
    // per plan. Check the complete register range before applying any plan.
    let additional_registers = plans.iter().try_fold(0usize, |total, plan| {
        total
            .checked_add(plan.true_owned.len())?
            .checked_add(plan.false_owned.len())?
            .checked_add(plan.true_cofactor.len())?
            .checked_add(plan.false_cofactor.len())?
            .checked_add(2)
    });
    let Some(additional_registers) = additional_registers else {
        return;
    };
    let max_register = eu.register_map.keys().map(|id| id.0).max().unwrap_or(0);
    if max_register.checked_add(additional_registers).is_none() {
        return;
    }

    if std::env::var_os("CELOX_PASS_TIMING").is_some() {
        let muxes = plans.iter().map(|plan| plan.muxes.len()).sum::<usize>();
        let true_owned = plans
            .iter()
            .map(|plan| plan.true_owned.len())
            .sum::<usize>();
        let false_owned = plans
            .iter()
            .map(|plan| plan.false_owned.len())
            .sum::<usize>();
        let live_outs = plans.iter().map(|plan| plan.live_outs.len()).sum::<usize>();
        eprintln!(
            "[same-predicate-regions] regions={} muxes={muxes} true_owned={true_owned} false_owned={false_owned} live_outs={live_outs}",
            plans.len(),
        );
        for plan in &plans {
            eprintln!(
                "[same-predicate-region] block={} cond=r{} segment={}..{} muxes={} true_owned={} false_owned={} true_cofactor={} false_cofactor={} live_outs={} benefit_scaled={}",
                plan.block_id.0,
                plan.condition.0,
                plan.segment_start,
                plan.segment_end,
                plan.muxes.len(),
                plan.true_owned.len(),
                plan.false_owned.len(),
                plan.true_cofactor.len(),
                plan.false_cofactor.len(),
                plan.live_outs.len(),
                plan.net_benefit_scaled,
            );
        }
    }

    let mut reg_counter = max_register;
    for (ordinal, plan) in plans.into_iter().enumerate() {
        let true_id = BlockId(first_new_block + ordinal * 3);
        let false_id = BlockId(first_new_block + ordinal * 3 + 1);
        let merge_id = BlockId(first_new_block + ordinal * 3 + 2);
        apply_same_predicate_plan(eu, plan, true_id, false_id, merge_id, &mut reg_counter);
    }
}

fn best_same_predicate_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> Option<SamePredicatePlan> {
    let block = eu.blocks.get(&block_id)?;
    let mut best: Option<SamePredicatePlan> = None;
    let mut segment_start = 0usize;
    while segment_start < block.instructions.len() {
        while segment_start < block.instructions.len()
            && !instruction_is_same_predicate_region_value(&block.instructions[segment_start])
        {
            segment_start += 1;
        }
        if segment_start == block.instructions.len() {
            break;
        }
        let mut segment_end = segment_start;
        while segment_end < block.instructions.len()
            && instruction_is_same_predicate_region_value(&block.instructions[segment_end])
        {
            segment_end += 1;
        }

        let mut groups = BTreeMap::<RegisterId, Vec<usize>>::new();
        for index in segment_start..segment_end {
            if let SIRInstruction::Mux(_, condition, _, _) = block.instructions[index] {
                groups.entry(condition).or_default().push(index);
            }
        }
        for (condition, muxes) in groups {
            // One Mux is already handled by ordinary cost-directed lowering;
            // this transform exists to share one branch across a region.
            if muxes.len() < 2 {
                continue;
            }
            let Some(candidate) = plan_same_predicate_region(
                eu,
                block_id,
                segment_start,
                segment_end,
                condition,
                &muxes,
                uses,
            ) else {
                continue;
            };
            let replace = best.as_ref().is_none_or(|current| {
                candidate.net_benefit_scaled > current.net_benefit_scaled
                    || candidate.net_benefit_scaled == current.net_benefit_scaled
                        && (candidate.segment_start, candidate.condition)
                            < (current.segment_start, current.condition)
            });
            if replace {
                best = Some(candidate);
            }
        }
        segment_start = segment_end.saturating_add(1);
    }
    best
}

#[allow(clippy::too_many_arguments)]
fn plan_same_predicate_region(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    segment_start: usize,
    segment_end: usize,
    condition: RegisterId,
    mux_indices: &[usize],
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> Option<SamePredicatePlan> {
    let block = eu.blocks.get(&block_id)?;
    if eu.register_map.get(&condition).map(RegisterType::width) != Some(1) {
        return None;
    }
    let local_defs = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, inst)| def_reg(inst).map(|reg| (reg, index)))
        .collect::<HashMap<_, _>>();
    let mut muxes = mux_indices.iter().copied().collect::<HashSet<_>>();

    // Forward-cofactor every pure value which depends on a group Mux. This is
    // what turns many scalar Mux outputs which later reconverge in a Concat
    // into a small live-out frontier instead of one block parameter per Mux.
    let mut affected = HashSet::<RegisterId>::default();
    let mut cofactor = HashSet::<usize>::default();
    for index in segment_start..segment_end {
        let inst = &block.instructions[index];
        let dst = def_reg(inst)?;
        if muxes.contains(&index) {
            affected.insert(dst);
        } else if instruction_uses(inst)
            .into_iter()
            .any(|operand| affected.contains(&operand))
        {
            affected.insert(dst);
            cofactor.insert(index);
        }
    }

    let mut live_outs = affected
        .iter()
        .copied()
        .filter(|value| {
            uses.get(value).into_iter().flatten().any(|site| {
                !matches!(
                    site,
                    UseSite::Instruction { block, index }
                        if *block == block_id
                            && (muxes.contains(index) || cofactor.contains(index))
                )
            })
        })
        .collect::<Vec<_>>();
    live_outs.sort_unstable();
    live_outs.dedup();
    if live_outs.is_empty() {
        return None;
    }

    // Specialize the forward slice separately on each edge. At a group Mux,
    // follow only that edge's selected operand. This both removes dead source
    // Muxes and avoids cloning a nested unselected cofactor chain.
    let (true_muxes, true_cofactor) =
        specialize_region_needed(&live_outs, true, &muxes, &cofactor, &local_defs, block);
    let (false_muxes, false_cofactor) =
        specialize_region_needed(&live_outs, false, &muxes, &cofactor, &local_defs, block);
    muxes = true_muxes
        .union(&false_muxes)
        .copied()
        .collect::<HashSet<_>>();
    cofactor = true_cofactor
        .union(&false_cofactor)
        .copied()
        .collect::<HashSet<_>>();
    if muxes.len() < 2 {
        return None;
    }

    let mut true_roots = Vec::with_capacity(true_muxes.len());
    let mut false_roots = Vec::with_capacity(false_muxes.len());
    for &index in &muxes {
        let SIRInstruction::Mux(_, mux_condition, true_value, false_value) =
            block.instructions[index]
        else {
            return None;
        };
        if mux_condition != condition {
            return None;
        }
        if true_muxes.contains(&index) {
            true_roots.push(true_value);
        }
        if false_muxes.contains(&index) {
            false_roots.push(false_value);
        }
    }
    let true_reachable = collect_region_reachable_defs(
        &true_roots,
        segment_start,
        segment_end,
        &local_defs,
        block,
        &muxes,
    );
    let false_reachable = collect_region_reachable_defs(
        &false_roots,
        segment_start,
        segment_end,
        &local_defs,
        block,
        &muxes,
    );
    let shared = true_reachable
        .intersection(&false_reachable)
        .copied()
        .collect::<HashSet<_>>();
    let removed_forward = muxes
        .iter()
        .chain(cofactor.iter())
        .copied()
        .collect::<HashSet<_>>();
    let mut true_owned = true_reachable
        .difference(&shared)
        .copied()
        .filter(|index| !removed_forward.contains(index))
        .collect::<HashSet<_>>();
    let mut false_owned = false_reachable
        .difference(&shared)
        .copied()
        .filter(|index| !removed_forward.contains(index))
        .collect::<HashSet<_>>();
    close_region_arm(&mut true_owned, true, block_id, block, &true_muxes, uses);
    close_region_arm(&mut false_owned, false, block_id, block, &false_muxes, uses);
    if !true_owned.is_disjoint(&false_owned) {
        return None;
    }

    let plan = SamePredicatePlan {
        block_id,
        segment_start,
        segment_end,
        condition,
        muxes,
        true_muxes,
        false_muxes,
        true_owned,
        false_owned,
        cofactor,
        true_cofactor,
        false_cofactor,
        live_outs,
        net_benefit_scaled: 0,
    };
    if !same_predicate_arm_is_closed(block, &plan, true)
        || !same_predicate_arm_is_closed(block, &plan, false)
    {
        return None;
    }
    let net_benefit_scaled = same_predicate_net_benefit(eu, block, &plan)?;
    Some(SamePredicatePlan {
        net_benefit_scaled,
        ..plan
    })
}

fn collect_region_reachable_defs(
    roots: &[RegisterId],
    segment_start: usize,
    segment_end: usize,
    local_defs: &HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    source_muxes: &HashSet<usize>,
) -> HashSet<usize> {
    let mut result = HashSet::default();
    let mut work = roots.to_vec();
    let mut visited = HashSet::default();
    while let Some(value) = work.pop() {
        if !visited.insert(value) {
            continue;
        }
        let Some(&index) = local_defs.get(&value) else {
            continue;
        };
        if index < segment_start || index >= segment_end || source_muxes.contains(&index) {
            continue;
        }
        if result.insert(index) {
            work.extend(instruction_uses(&block.instructions[index]));
        }
    }
    result
}

fn specialize_region_needed(
    live_outs: &[RegisterId],
    true_arm: bool,
    source_muxes: &HashSet<usize>,
    cofactor: &HashSet<usize>,
    local_defs: &HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
) -> (HashSet<usize>, HashSet<usize>) {
    let mut needed_muxes = HashSet::default();
    let mut needed_cofactor = HashSet::default();
    let mut work = live_outs.to_vec();
    let mut visited = HashSet::default();
    while let Some(value) = work.pop() {
        if !visited.insert(value) {
            continue;
        }
        let Some(&index) = local_defs.get(&value) else {
            continue;
        };
        if source_muxes.contains(&index) {
            needed_muxes.insert(index);
            let SIRInstruction::Mux(_, _, true_value, false_value) = block.instructions[index]
            else {
                continue;
            };
            work.push(if true_arm { true_value } else { false_value });
        } else if cofactor.contains(&index) && needed_cofactor.insert(index) {
            work.extend(instruction_uses(&block.instructions[index]));
        }
    }
    (needed_muxes, needed_cofactor)
}

fn close_region_arm(
    owned: &mut HashSet<usize>,
    true_arm: bool,
    block_id: BlockId,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    source_muxes: &HashSet<usize>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) {
    loop {
        let rejected = owned
            .iter()
            .copied()
            .filter(|index| {
                let Some(dst) = def_reg(&block.instructions[*index]) else {
                    return true;
                };
                uses.get(&dst)
                    .into_iter()
                    .flatten()
                    .any(|site| match *site {
                        UseSite::Instruction {
                            block: use_block,
                            index: use_index,
                        } if use_block == block_id => {
                            if owned.contains(&use_index) {
                                return false;
                            }
                            if !source_muxes.contains(&use_index) {
                                return true;
                            }
                            match block.instructions[use_index] {
                                SIRInstruction::Mux(_, condition, true_value, false_value) => {
                                    condition == dst
                                        || (if true_arm { true_value } else { false_value }) != dst
                                }
                                _ => true,
                            }
                        }
                        _ => true,
                    })
            })
            .collect::<Vec<_>>();
        if rejected.is_empty() {
            break;
        }
        for index in rejected {
            owned.remove(&index);
        }
    }
}

fn same_predicate_arm_is_closed(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &SamePredicatePlan,
    true_arm: bool,
) -> bool {
    let owned = if true_arm {
        &plan.true_owned
    } else {
        &plan.false_owned
    };
    let arm_muxes = if true_arm {
        &plan.true_muxes
    } else {
        &plan.false_muxes
    };
    let arm_cofactor = if true_arm {
        &plan.true_cofactor
    } else {
        &plan.false_cofactor
    };
    let removed = plan
        .true_owned
        .iter()
        .chain(plan.false_owned.iter())
        .chain(plan.muxes.iter())
        .chain(plan.cofactor.iter())
        .filter_map(|index| def_reg(&block.instructions[*index]))
        .collect::<HashSet<_>>();
    let mut mapped = HashSet::default();
    for index in plan.segment_start..plan.segment_end {
        if owned.contains(&index) || arm_cofactor.contains(&index) {
            if instruction_uses(&block.instructions[index])
                .into_iter()
                .any(|operand| removed.contains(&operand) && !mapped.contains(&operand))
            {
                return false;
            }
            if let Some(dst) = def_reg(&block.instructions[index]) {
                mapped.insert(dst);
            }
        } else if arm_muxes.contains(&index) {
            let SIRInstruction::Mux(_, _, true_value, false_value) = block.instructions[index]
            else {
                return false;
            };
            let selected = if true_arm { true_value } else { false_value };
            if removed.contains(&selected) && !mapped.contains(&selected) {
                return false;
            }
            if let Some(dst) = def_reg(&block.instructions[index]) {
                mapped.insert(dst);
            }
        }
    }
    plan.live_outs.iter().all(|value| mapped.contains(value))
}

fn register_chunks(register_map: &HashMap<RegisterId, RegisterType>, value: RegisterId) -> u128 {
    register_map
        .get(&value)
        .map(|ty| ty.width().div_ceil(64).max(1) as u128)
        .unwrap_or(1)
}

fn same_predicate_net_benefit(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &SamePredicatePlan,
) -> Option<u128> {
    const BRANCH_CONTROL_COST: u128 = 3;
    const MISPREDICT_COST: u128 = 16;
    const PHI_COPY_COST_PER_CHUNK: u128 = 2;
    const LIVE_THROUGH_COST_PER_CHUNK: u128 = 1;

    let instruction_cost = |index: usize| {
        super::cost_model::estimate_clif_cost(&block.instructions[index], &eu.register_map, false)
            as u128
    };
    let true_cost = plan
        .true_owned
        .iter()
        .copied()
        .map(instruction_cost)
        .fold(0u128, u128::saturating_add);
    let false_cost = plan
        .false_owned
        .iter()
        .copied()
        .map(instruction_cost)
        .fold(0u128, u128::saturating_add);
    let mux_cost = plan
        .muxes
        .iter()
        .copied()
        .map(instruction_cost)
        .fold(0u128, u128::saturating_add);

    let region_defs = plan
        .true_owned
        .iter()
        .chain(plan.false_owned.iter())
        .chain(plan.muxes.iter())
        .chain(plan.cofactor.iter())
        .filter_map(|index| def_reg(&block.instructions[*index]))
        .collect::<HashSet<_>>();
    let mut live_through = HashSet::default();
    for &index in plan
        .true_owned
        .iter()
        .chain(plan.false_owned.iter())
        .chain(plan.cofactor.iter())
    {
        for operand in instruction_uses(&block.instructions[index]) {
            if !region_defs.contains(&operand) {
                live_through.insert(operand);
            }
        }
    }
    let live_through_chunks = live_through
        .into_iter()
        .map(|value| register_chunks(&eu.register_map, value))
        .fold(0u128, u128::saturating_add);
    let phi_chunks = plan
        .live_outs
        .iter()
        .copied()
        .map(|value| register_chunks(&eu.register_map, value))
        .fold(0u128, u128::saturating_add);

    // Exact 50/50 integer expected-cost comparison. All values are scaled by
    // two: each arm is skipped on one edge, every removed Mux is saved on both,
    // and one of the two equally-likely outcomes pays the modeled miss.
    let saved_scaled = true_cost
        .saturating_add(false_cost)
        .saturating_add(mux_cost.saturating_mul(2));
    let introduced_scaled = BRANCH_CONTROL_COST
        .saturating_add(phi_chunks.saturating_mul(PHI_COPY_COST_PER_CHUNK))
        .saturating_add(live_through_chunks.saturating_mul(LIVE_THROUGH_COST_PER_CHUNK))
        .saturating_mul(2)
        .saturating_add(MISPREDICT_COST);
    (saved_scaled > introduced_scaled).then(|| saved_scaled - introduced_scaled)
}

fn apply_same_predicate_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: SamePredicatePlan,
    true_id: BlockId,
    false_id: BlockId,
    merge_id: BlockId,
    reg_counter: &mut usize,
) {
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("planned same-predicate block must remain present");
    let mut head_instructions = original.instructions[..plan.segment_start].to_vec();
    for index in plan.segment_start..plan.segment_end {
        if !plan.true_owned.contains(&index)
            && !plan.false_owned.contains(&index)
            && !plan.muxes.contains(&index)
            && !plan.cofactor.contains(&index)
        {
            head_instructions.push(original.instructions[index].clone());
        }
    }
    let merge_instructions = original.instructions[plan.segment_end..].to_vec();

    // These original SSA definitions are replaced by edge-local clones or by
    // merge parameters. Keeping the dead IDs in register_map is not benign:
    // native isel allocates a VReg for every entry before it sees any use.
    let live_outs = plan.live_outs.iter().copied().collect::<HashSet<_>>();
    let dead_originals = plan
        .true_owned
        .iter()
        .chain(plan.false_owned.iter())
        .chain(plan.muxes.iter())
        .chain(plan.cofactor.iter())
        .filter_map(|index| def_reg(&original.instructions[*index]))
        .filter(|value| !live_outs.contains(value))
        .collect::<HashSet<_>>();

    let (true_instructions, true_arguments) =
        build_same_predicate_arm(eu, &original, &plan, true, reg_counter);
    let (false_instructions, false_arguments) =
        build_same_predicate_arm(eu, &original, &plan, false, reg_counter);
    let branch_condition = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_instructions,
        plan.condition,
        reg_counter,
    );

    eu.blocks.insert(
        plan.block_id,
        BasicBlock {
            id: plan.block_id,
            params: original.params,
            instructions: head_instructions,
            terminator: SIRTerminator::Branch {
                cond: branch_condition,
                true_block: (true_id, Vec::new()),
                false_block: (false_id, Vec::new()),
            },
        },
    );
    eu.blocks.insert(
        true_id,
        BasicBlock {
            id: true_id,
            params: Vec::new(),
            instructions: true_instructions,
            terminator: SIRTerminator::Jump(merge_id, true_arguments),
        },
    );
    eu.blocks.insert(
        false_id,
        BasicBlock {
            id: false_id,
            params: Vec::new(),
            instructions: false_instructions,
            terminator: SIRTerminator::Jump(merge_id, false_arguments),
        },
    );
    eu.blocks.insert(
        merge_id,
        BasicBlock {
            id: merge_id,
            params: plan.live_outs,
            instructions: merge_instructions,
            terminator: original.terminator,
        },
    );
    for value in dead_originals {
        eu.register_map.remove(&value);
    }
}

fn build_same_predicate_arm(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    original: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &SamePredicatePlan,
    true_arm: bool,
    reg_counter: &mut usize,
) -> (Vec<SIRInstruction<RegionedAbsoluteAddr>>, Vec<RegisterId>) {
    let owned = if true_arm {
        &plan.true_owned
    } else {
        &plan.false_owned
    };
    let arm_muxes = if true_arm {
        &plan.true_muxes
    } else {
        &plan.false_muxes
    };
    let arm_cofactor = if true_arm {
        &plan.true_cofactor
    } else {
        &plan.false_cofactor
    };
    let mut instructions = Vec::new();
    let mut replacements = HashMap::<RegisterId, RegisterId>::default();
    for index in plan.segment_start..plan.segment_end {
        let inst = &original.instructions[index];
        if owned.contains(&index) || arm_cofactor.contains(&index) {
            let old_dst = def_reg(inst).expect("pure region instruction must define a value");
            *reg_counter += 1;
            let new_dst = RegisterId(*reg_counter);
            eu.register_map
                .insert(new_dst, eu.register_map[&old_dst].clone());
            instructions.push(
                clone_pure_instruction(inst, new_dst, &replacements)
                    .expect("planned region contains only pure instructions"),
            );
            replacements.insert(old_dst, new_dst);
        } else if arm_muxes.contains(&index) {
            let SIRInstruction::Mux(dst, _, true_value, false_value) = *inst else {
                unreachable!("same-predicate source must remain a Mux")
            };
            let selected = if true_arm { true_value } else { false_value };
            replacements.insert(
                dst,
                replacements.get(&selected).copied().unwrap_or(selected),
            );
        }
    }
    let arguments = plan
        .live_outs
        .iter()
        .map(|value| {
            *replacements
                .get(value)
                .expect("closed cofactor must define every live-out on both edges")
        })
        .collect();
    (instructions, arguments)
}

fn clone_pure_instruction(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    dst: RegisterId,
    replacements: &HashMap<RegisterId, RegisterId>,
) -> Option<SIRInstruction<RegionedAbsoluteAddr>> {
    let mapped = |value: RegisterId| replacements.get(&value).copied().unwrap_or(value);
    Some(match inst {
        SIRInstruction::Imm(_, value) => SIRInstruction::Imm(dst, value.clone()),
        SIRInstruction::Binary(_, lhs, op, rhs) => {
            SIRInstruction::Binary(dst, mapped(*lhs), *op, mapped(*rhs))
        }
        SIRInstruction::Unary(_, op, source) => SIRInstruction::Unary(dst, *op, mapped(*source)),
        SIRInstruction::Concat(_, args) => {
            SIRInstruction::Concat(dst, args.iter().copied().map(mapped).collect())
        }
        SIRInstruction::Slice(_, source, lsb, width) => {
            SIRInstruction::Slice(dst, mapped(*source), *lsb, *width)
        }
        SIRInstruction::Mux(_, condition, true_value, false_value) => SIRInstruction::Mux(
            dst,
            mapped(*condition),
            mapped(*true_value),
            mapped(*false_value),
        ),
        SIRInstruction::Load(_, address, offset, width) => SIRInstruction::Load(
            dst,
            *address,
            match offset {
                SIROffset::Static(offset) => SIROffset::Static(*offset),
                SIROffset::Dynamic(offset) => SIROffset::Dynamic(mapped(*offset)),
                SIROffset::Element {
                    index,
                    element_width,
                    bit_offset,
                    dynamic_bit_offset,
                } => SIROffset::Element {
                    index: mapped(*index),
                    element_width: *element_width,
                    bit_offset: *bit_offset,
                    dynamic_bit_offset: dynamic_bit_offset.map(mapped),
                },
            },
            *width,
        ),
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => return None,
    })
}

fn instruction_is_same_predicate_region_value(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    instruction_is_movable(inst) || matches!(inst, SIRInstruction::Load(..))
}

fn plan_block(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    predecessors: &BTreeMap<BlockId, BTreeSet<BlockId>>,
    dominators: &Dominators,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> Option<GuardedRegionPlan> {
    let block = eu.blocks.get(&block_id)?;
    let SIRTerminator::Branch {
        cond,
        true_block,
        false_block,
    } = &block.terminator
    else {
        return None;
    };
    if eu.register_map.get(cond).map(RegisterType::width) != Some(1)
        || true_block.0 == false_block.0
        || true_block.0 == eu.entry_block_id
        || dominators.dominates(true_block.0, block_id)
        || predecessors
            .get(&true_block.0)?
            .iter()
            .copied()
            .ne([block_id])
    {
        return None;
    }

    let local_defs = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, inst)| def_reg(inst).map(|reg| (reg, index)))
        .collect::<HashMap<_, _>>();
    let mut distributed = Vec::new();
    for (index, inst) in block.instructions.iter().enumerate() {
        let SIRInstruction::Store(_, offset, width, source, _, _) = inst else {
            continue;
        };
        let Some(&mux_index) = local_defs.get(source) else {
            continue;
        };
        let SIRInstruction::Mux(result, mux_cond, true_value, false_value) =
            block.instructions[mux_index]
        else {
            continue;
        };
        if result == *source
            && mux_cond == *cond
            && mux_index < index
            && *width != 0
            && eu
                .register_map
                .get(&true_value)
                .is_some_and(|ty| ty.width() >= *width)
            && eu
                .register_map
                .get(&false_value)
                .is_some_and(|ty| ty.width() >= *width)
            && !offset
                .dynamic_registers()
                .into_iter()
                .flatten()
                .any(|dynamic| dynamic == result)
        {
            distributed.push(DistributedStore {
                index,
                mux_index,
                mux_result: result,
                true_value,
                false_value,
            });
        }
    }
    if distributed.is_empty() {
        return None;
    }
    distributed.sort_unstable_by_key(|store| store.index);
    let distributed_indices = distributed
        .iter()
        .map(|store| store.index)
        .collect::<HashSet<_>>();
    let first_distributed = distributed.first()?.index;
    if block
        .instructions
        .iter()
        .enumerate()
        .skip(first_distributed + 1)
        .any(|(index, inst)| {
            matches!(inst, SIRInstruction::Load(..))
                || instruction_has_effect(inst) && !distributed_indices.contains(&index)
        })
    {
        return None;
    }

    let removable_muxes = distributed
        .iter()
        .map(|store| store.mux_index)
        .collect::<HashSet<_>>();
    for store in &distributed {
        if uses
            .get(&store.mux_result)
            .into_iter()
            .flatten()
            .any(|site| {
                !matches!(
                    site,
                    UseSite::Instruction { block, index }
                        if *block == block_id
                            && distributed_indices.contains(index)
                            && store_source_is(
                                &eu.blocks[block].instructions[*index],
                                store.mux_result,
                            )
                )
            })
        {
            return None;
        }
    }

    let safe_to_move = block
        .instructions
        .iter()
        .map(instruction_is_movable)
        .collect::<Vec<_>>();
    let can_move = compute_moveable_definitions(
        eu,
        block,
        *cond,
        true_block.0,
        &local_defs,
        uses,
        dominators,
        &distributed,
        &removable_muxes,
        &safe_to_move,
    );

    let mut seeds = VecDeque::new();
    for store in &distributed {
        if local_defs.contains_key(&store.true_value) {
            seeds.push_back(store.true_value);
        }
    }
    seeds.extend(
        true_block
            .1
            .iter()
            .copied()
            .filter(|reg| local_defs.contains_key(reg)),
    );
    for &reg in local_defs.keys() {
        if uses.get(&reg).into_iter().flatten().any(|site| {
            site.block() != block_id && dominators.dominates(true_block.0, site.block())
        }) {
            seeds.push_back(reg);
        }
    }

    let mut moved = HashSet::default();
    while let Some(reg) = seeds.pop_front() {
        if reg == *cond {
            continue;
        }
        let Some(&index) = local_defs.get(&reg) else {
            continue;
        };
        if removable_muxes.contains(&index) || !can_move[index] || !moved.insert(index) {
            continue;
        }

        for operand in instruction_uses(&block.instructions[index]) {
            if operand != *cond
                && let Some(&operand_index) = local_defs.get(&operand)
                && can_move[operand_index]
            {
                seeds.push_back(operand);
            }
        }
        for site in uses.get(&reg).into_iter().flatten() {
            if let UseSite::Instruction {
                block: use_block,
                index: use_index,
            } = *site
                && use_block == block_id
                && !removable_muxes.contains(&use_index)
                && let Some(user) = def_reg(&block.instructions[use_index])
                && can_move[use_index]
            {
                seeds.push_back(user);
            }
        }
    }
    if moved.is_empty() {
        return None;
    }

    // The false store value and dynamic store offset must remain available on
    // both edges.  The source mux itself is the only store operand removed.
    for store in &distributed {
        if local_defs
            .get(&store.false_value)
            .is_some_and(|index| moved.contains(index))
        {
            return None;
        }
        if let SIRInstruction::Store(_, offset, _, _, _, _) = &block.instructions[store.index] {
            for offset in offset.dynamic_registers().into_iter().flatten() {
                if local_defs
                    .get(&offset)
                    .is_some_and(|index| moved.contains(index) || removable_muxes.contains(index))
                {
                    return None;
                }
            }
        }
    }

    // Recheck the selected closed region explicitly.  This is deliberately
    // redundant with `can_move`: it keeps application independent from the
    // fixed-point implementation and makes every external use proof local to
    // the completed plan.
    for &index in &moved {
        let Some(dst) = def_reg(&block.instructions[index]) else {
            return None;
        };
        if uses.get(&dst).into_iter().flatten().any(|site| {
            !use_is_owned_by_true_edge(
                *site,
                dst,
                block_id,
                true_block.0,
                block,
                &moved,
                &distributed,
                &removable_muxes,
                dominators,
            )
        }) {
            return None;
        }
    }

    Some(GuardedRegionPlan {
        block_id,
        condition: *cond,
        true_target: true_block.clone(),
        false_target: false_block.clone(),
        moved,
        distributed,
        removable_muxes,
    })
}

#[allow(clippy::too_many_arguments)]
fn compute_moveable_definitions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    condition: RegisterId,
    true_target: BlockId,
    local_defs: &HashMap<RegisterId, usize>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    dominators: &Dominators,
    distributed: &[DistributedStore],
    removable_muxes: &HashSet<usize>,
    safe_to_move: &[bool],
) -> Vec<bool> {
    let mut result = vec![false; block.instructions.len()];
    for index in (0..block.instructions.len()).rev() {
        let Some(dst) = def_reg(&block.instructions[index]) else {
            continue;
        };
        if dst == condition || removable_muxes.contains(&index) || !safe_to_move[index] {
            continue;
        }
        result[index] = uses.get(&dst).into_iter().flatten().all(|site| {
            use_can_follow_true_edge(
                eu,
                *site,
                dst,
                block.id,
                true_target,
                local_defs,
                &result,
                distributed,
                removable_muxes,
                dominators,
            )
        });
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn use_can_follow_true_edge(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    site: UseSite,
    value: RegisterId,
    source_block: BlockId,
    true_target: BlockId,
    _local_defs: &HashMap<RegisterId, usize>,
    moveable: &[bool],
    distributed: &[DistributedStore],
    removable_muxes: &HashSet<usize>,
    dominators: &Dominators,
) -> bool {
    match site {
        UseSite::Instruction { block, index } if block == source_block => {
            if removable_muxes.contains(&index) {
                return removable_mux_true_value(eu, source_block, index) == Some(value);
            }
            def_reg(&eu.blocks[&block].instructions[index])
                .is_some_and(|_| moveable.get(index).copied().unwrap_or(false))
                || distributed.iter().any(|store| {
                    store.index == index && store.true_value == value && store.false_value != value
                })
        }
        UseSite::TrueEdgeArgument { block } if block == source_block => true,
        UseSite::BranchCondition { block }
        | UseSite::FalseEdgeArgument { block }
        | UseSite::JumpArgument { block }
            if block == source_block =>
        {
            false
        }
        _ => dominators.dominates(true_target, site.block()),
    }
}

#[allow(clippy::too_many_arguments)]
fn use_is_owned_by_true_edge(
    site: UseSite,
    value: RegisterId,
    source_block: BlockId,
    true_target: BlockId,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    moved: &HashSet<usize>,
    distributed: &[DistributedStore],
    removable_muxes: &HashSet<usize>,
    dominators: &Dominators,
) -> bool {
    match site {
        UseSite::Instruction {
            block: use_block,
            index,
        } if use_block == source_block => {
            moved.contains(&index)
                || removable_muxes.contains(&index)
                    && removable_mux_true_value_in_block(block, index) == Some(value)
                || distributed.iter().any(|store| {
                    store.index == index && store.true_value == value && store.false_value != value
                })
        }
        UseSite::TrueEdgeArgument { block } if block == source_block => true,
        UseSite::BranchCondition { block }
        | UseSite::FalseEdgeArgument { block }
        | UseSite::JumpArgument { block }
            if block == source_block =>
        {
            false
        }
        _ => dominators.dominates(true_target, site.block()),
    }
}

fn removable_mux_true_value(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: BlockId,
    index: usize,
) -> Option<RegisterId> {
    removable_mux_true_value_in_block(eu.blocks.get(&block)?, index)
}

fn removable_mux_true_value_in_block(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    index: usize,
) -> Option<RegisterId> {
    match block.instructions.get(index)? {
        SIRInstruction::Mux(_, _, true_value, _) => Some(*true_value),
        _ => None,
    }
}

fn apply_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: GuardedRegionPlan,
    true_id: BlockId,
    false_id: BlockId,
    reg_counter: &mut usize,
) {
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("verified guarded-region source block must exist");
    let distributed = plan
        .distributed
        .iter()
        .map(|store| (store.index, store))
        .collect::<HashMap<_, _>>();
    let mut head_instructions = Vec::new();
    let mut true_instructions = Vec::new();
    let mut false_instructions = Vec::new();

    for (index, inst) in original.instructions.into_iter().enumerate() {
        if plan.moved.contains(&index) {
            true_instructions.push(inst);
        } else if let Some(store) = distributed.get(&index) {
            true_instructions.push(store_with_source(&inst, store.true_value));
            false_instructions.push(store_with_source(&inst, store.false_value));
        } else if !plan.removable_muxes.contains(&index) {
            head_instructions.push(inst);
        }
    }
    let branch_condition = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_instructions,
        plan.condition,
        reg_counter,
    );

    eu.blocks.insert(
        plan.block_id,
        BasicBlock {
            id: plan.block_id,
            params: original.params,
            instructions: head_instructions,
            terminator: SIRTerminator::Branch {
                cond: branch_condition,
                true_block: (true_id, Vec::new()),
                false_block: (false_id, Vec::new()),
            },
        },
    );
    eu.blocks.insert(
        true_id,
        BasicBlock {
            id: true_id,
            params: Vec::new(),
            instructions: true_instructions,
            terminator: SIRTerminator::Jump(plan.true_target.0, plan.true_target.1),
        },
    );
    eu.blocks.insert(
        false_id,
        BasicBlock {
            id: false_id,
            params: Vec::new(),
            instructions: false_instructions,
            terminator: SIRTerminator::Jump(plan.false_target.0, plan.false_target.1),
        },
    );
}

fn store_with_source(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    source: RegisterId,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    let SIRInstruction::Store(addr, offset, width, _, triggers, capture_sites) = inst else {
        unreachable!("distributed store plan refers to a non-store")
    };
    SIRInstruction::Store(
        *addr,
        offset.clone(),
        *width,
        source,
        triggers.clone(),
        capture_sites.clone(),
    )
}

fn store_source_is(inst: &SIRInstruction<RegionedAbsoluteAddr>, source: RegisterId) -> bool {
    matches!(inst, SIRInstruction::Store(_, _, _, actual, _, _) if *actual == source)
}

fn instruction_is_movable(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
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

fn instruction_has_effect(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        inst,
        SIRInstruction::Store(..)
            | SIRInstruction::Commit(..)
            | SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. }
    )
}

fn instruction_uses(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
    match inst {
        SIRInstruction::Imm(..) => Vec::new(),
        SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
            vec![*source]
        }
        SIRInstruction::Load(_, _, offset, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Store(_, offset, _, source, _, _) => offset
            .dynamic_registers()
            .into_iter()
            .flatten()
            .chain(std::iter::once(*source))
            .collect(),
        SIRInstruction::Commit(_, _, offset, _, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Concat(_, args)
        | SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => args.clone(),
        SIRInstruction::Mux(_, cond, true_value, false_value) => {
            vec![*cond, *true_value, *false_value]
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => vec![*old, *new],
    }
}

fn collect_uses(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, Vec<UseSite>> {
    let mut result = HashMap::<RegisterId, Vec<UseSite>>::default();
    for block in eu.blocks.values() {
        for (index, inst) in block.instructions.iter().enumerate() {
            for reg in instruction_uses(inst) {
                result.entry(reg).or_default().push(UseSite::Instruction {
                    block: block.id,
                    index,
                });
            }
        }
        match &block.terminator {
            SIRTerminator::Jump(_, args) => {
                for &reg in args {
                    result
                        .entry(reg)
                        .or_default()
                        .push(UseSite::JumpArgument { block: block.id });
                }
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                result
                    .entry(*cond)
                    .or_default()
                    .push(UseSite::BranchCondition { block: block.id });
                for &reg in &true_block.1 {
                    result
                        .entry(reg)
                        .or_default()
                        .push(UseSite::TrueEdgeArgument { block: block.id });
                }
                for &reg in &false_block.1 {
                    result
                        .entry(reg)
                        .or_default()
                        .push(UseSite::FalseEdgeArgument { block: block.id });
                }
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    result
}

fn predecessor_map<A>(eu: &ExecutionUnit<A>) -> BTreeMap<BlockId, BTreeSet<BlockId>> {
    let mut predecessors = eu
        .blocks
        .keys()
        .copied()
        .map(|id| (id, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for block in eu.blocks.values() {
        for successor in successors(&block.terminator) {
            if let Some(entries) = predecessors.get_mut(&successor) {
                entries.insert(block.id);
            }
        }
    }
    predecessors
}

fn successors(terminator: &SIRTerminator) -> Vec<BlockId> {
    match terminator {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

struct Dominators {
    index: HashMap<BlockId, usize>,
    enter: Vec<usize>,
    exit: Vec<usize>,
}

impl Dominators {
    fn compute<A>(
        eu: &ExecutionUnit<A>,
        predecessors: &BTreeMap<BlockId, BTreeSet<BlockId>>,
    ) -> Self {
        let mut successor_map = eu
            .blocks
            .keys()
            .copied()
            .map(|id| (id, successors(&eu.blocks[&id].terminator)))
            .collect::<BTreeMap<_, _>>();
        for entries in successor_map.values_mut() {
            entries.sort_unstable_by_key(|id| id.0);
        }

        let entry = eu.entry_block_id;
        let mut visited = BTreeSet::new();
        let mut postorder = Vec::with_capacity(eu.blocks.len());
        visited.insert(entry);
        let mut stack = vec![(entry, 0usize)];
        while let Some((block, next)) = stack.last_mut() {
            if *next == successor_map[block].len() {
                postorder.push(*block);
                stack.pop();
                continue;
            }
            let successor = successor_map[block][*next];
            *next += 1;
            if visited.insert(successor) {
                stack.push((successor, 0));
            }
        }
        postorder.reverse();
        let index = postorder
            .iter()
            .enumerate()
            .map(|(index, &block)| (block, index))
            .collect::<HashMap<_, _>>();
        let mut immediate = vec![None; postorder.len()];
        immediate[0] = Some(0);
        let intersect = |mut left: usize, mut right: usize, idom: &[Option<usize>]| {
            while left != right {
                while left > right {
                    left = idom[left].expect("verified predecessor must be processed");
                }
                while right > left {
                    right = idom[right].expect("verified predecessor must be processed");
                }
            }
            left
        };
        loop {
            let mut changed = false;
            for block_index in 1..postorder.len() {
                let block = postorder[block_index];
                let mut processed = predecessors[&block]
                    .iter()
                    .filter_map(|predecessor| index.get(predecessor).copied())
                    .filter(|predecessor| immediate[*predecessor].is_some());
                let Some(first) = processed.next() else {
                    continue;
                };
                let next = processed.fold(first, |current, predecessor| {
                    intersect(current, predecessor, &immediate)
                });
                if immediate[block_index] != Some(next) {
                    immediate[block_index] = Some(next);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let mut children = vec![Vec::new(); postorder.len()];
        for (block, parent) in immediate.iter().enumerate().skip(1) {
            children[parent.expect("verified reachable block must have an idom")].push(block);
        }
        let mut enter = vec![0usize; postorder.len()];
        let mut exit = vec![0usize; postorder.len()];
        let mut time = 0usize;
        let mut events = vec![(0usize, false)];
        while let Some((block, leaving)) = events.pop() {
            if leaving {
                exit[block] = time;
                time += 1;
            } else {
                enter[block] = time;
                time += 1;
                events.push((block, true));
                events.extend(children[block].iter().rev().map(|child| (*child, false)));
            }
        }
        Self { index, enter, exit }
    }

    fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        let (Some(&dominator), Some(&block)) = (self.index.get(&dominator), self.index.get(&block))
        else {
            return false;
        };
        self.enter[dominator] <= self.enter[block] && self.exit[block] <= self.exit[dominator]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{DomainKind, InstanceId, SIRValue, TriggerIdWithKind};
    use veryl_analyzer::ir::VarId;

    fn address(id: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(id),
            var_id: VarId::default(),
        }
    }

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn insert_block(
        blocks: &mut HashMap<BlockId, BasicBlock<RegionedAbsoluteAddr>>,
        id: usize,
        params: Vec<RegisterId>,
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        terminator: SIRTerminator,
    ) {
        blocks.insert(
            BlockId(id),
            BasicBlock {
                id: BlockId(id),
                params,
                instructions,
                terminator,
            },
        );
    }

    fn shared_dag_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        for reg in 1..=8 {
            register_map.insert(RegisterId(reg), bit(8));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1)],
            vec![
                SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
                SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Add, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(4), RegisterId(3), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(5), RegisterId(3), BinaryOp::Or, RegisterId(4)),
                SIRInstruction::Mux(RegisterId(6), RegisterId(0), RegisterId(5), RegisterId(2)),
                SIRInstruction::Store(
                    address(10),
                    SIROffset::Static(0),
                    8,
                    RegisterId(6),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), vec![RegisterId(5)]),
                false_block: (BlockId(2), vec![RegisterId(2)]),
            },
        );
        insert_block(
            &mut blocks,
            1,
            vec![RegisterId(7)],
            vec![SIRInstruction::Store(
                address(11),
                SIROffset::Static(0),
                8,
                RegisterId(7),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Jump(BlockId(3), Vec::new()),
        );
        insert_block(
            &mut blocks,
            2,
            vec![RegisterId(8)],
            vec![SIRInstruction::Store(
                address(12),
                SIROffset::Static(0),
                8,
                RegisterId(8),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Jump(BlockId(3), Vec::new()),
        );
        insert_block(
            &mut blocks,
            3,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Return,
        );
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    fn repeated_predicate_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        for reg in 1..=11 {
            register_map.insert(RegisterId(reg), bit(8));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            vec![
                // Two closed arm DAGs. The selected values are postprocessed
                // and reconverge before their one external Store.
                SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(4), RegisterId(2), BinaryOp::Add, RegisterId(2)),
                SIRInstruction::Mux(RegisterId(5), RegisterId(0), RegisterId(3), RegisterId(4)),
                SIRInstruction::Binary(RegisterId(6), RegisterId(5), BinaryOp::And, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(7), RegisterId(3), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(8), RegisterId(4), BinaryOp::Add, RegisterId(2)),
                SIRInstruction::Mux(RegisterId(9), RegisterId(0), RegisterId(7), RegisterId(8)),
                SIRInstruction::Binary(RegisterId(10), RegisterId(9), BinaryOp::Or, RegisterId(2)),
                SIRInstruction::Binary(RegisterId(11), RegisterId(6), BinaryOp::Or, RegisterId(10)),
                SIRInstruction::Store(
                    address(40),
                    SIROffset::Static(0),
                    8,
                    RegisterId(11),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Return,
        );
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ExecutionTrace {
        stores: Vec<(RegionedAbsoluteAddr, usize, usize, u64)>,
    }

    fn execute(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        condition: u64,
        input: u64,
    ) -> ExecutionTrace {
        let mut registers = HashMap::default();
        registers.insert(RegisterId(0), condition);
        registers.insert(RegisterId(1), input);
        registers.insert(RegisterId(2), input);
        let mut memory = HashMap::<(RegionedAbsoluteAddr, usize), u64>::default();
        let mut stores = Vec::new();
        let mut current = eu.entry_block_id;
        let mut entered = HashSet::default();
        loop {
            assert!(
                entered.insert(current),
                "test fixture unexpectedly contains a loop"
            );
            let block = &eu.blocks[&current];
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Imm(dst, value) => {
                        let digits = value.payload.to_u64_digits();
                        registers.insert(*dst, digits.first().copied().unwrap_or(0));
                    }
                    SIRInstruction::Binary(dst, lhs, op, rhs) => {
                        let lhs = registers[lhs];
                        let rhs = registers[rhs];
                        let value = match op {
                            BinaryOp::Add => lhs.wrapping_add(rhs),
                            BinaryOp::Mul => lhs.wrapping_mul(rhs),
                            BinaryOp::Or | BinaryOp::LogicOr => lhs | rhs,
                            BinaryOp::And | BinaryOp::LogicAnd => lhs & rhs,
                            other => panic!("unsupported test binary op {other:?}"),
                        };
                        let width = eu.register_map[dst].width();
                        let mask = if width >= 64 {
                            u64::MAX
                        } else {
                            (1u64 << width) - 1
                        };
                        registers.insert(*dst, value & mask);
                    }
                    SIRInstruction::Unary(dst, op, source) => {
                        let width = eu.register_map[dst].width();
                        let mask = if width >= 64 {
                            u64::MAX
                        } else {
                            (1u64 << width) - 1
                        };
                        let value = match op {
                            UnaryOp::Ident => registers[source],
                            UnaryOp::BitNot => !registers[source],
                            UnaryOp::Minus => registers[source].wrapping_neg(),
                            other => panic!("unsupported test unary op {other:?}"),
                        };
                        registers.insert(*dst, value & mask);
                    }
                    SIRInstruction::Load(dst, addr, offset, _) => {
                        let offset = match offset {
                            SIROffset::Static(offset) => *offset,
                            SIROffset::Dynamic(offset) => registers[offset] as usize,
                            SIROffset::Element {
                                index,
                                element_width,
                                bit_offset,
                                dynamic_bit_offset,
                            } => {
                                registers[index] as usize * element_width
                                    + bit_offset
                                    + dynamic_bit_offset
                                        .map(|register| registers[&register] as usize)
                                        .unwrap_or(0)
                            }
                        };
                        registers.insert(*dst, memory.get(&(*addr, offset)).copied().unwrap_or(0));
                    }
                    SIRInstruction::Mux(dst, cond, true_value, false_value) => {
                        let selected = if registers[cond] != 0 {
                            registers[true_value]
                        } else {
                            registers[false_value]
                        };
                        registers.insert(*dst, selected);
                    }
                    SIRInstruction::Store(addr, SIROffset::Static(offset), width, source, _, _) => {
                        let value = registers[source];
                        memory.insert((*addr, *offset), value);
                        stores.push((*addr, *offset, *width, value));
                    }
                    other => panic!("unsupported test instruction {other:?}"),
                }
            }
            let (target, arguments) = match &block.terminator {
                SIRTerminator::Jump(target, args) => (*target, args.clone()),
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    if registers[cond] != 0 {
                        (true_block.0, true_block.1.clone())
                    } else {
                        (false_block.0, false_block.1.clone())
                    }
                }
                SIRTerminator::Return => return ExecutionTrace { stores },
                SIRTerminator::Error(code) => panic!("unexpected test error {code}"),
            };
            let values = arguments
                .iter()
                .map(|argument| registers[argument])
                .collect::<Vec<_>>();
            for (&param, value) in eu.blocks[&target].params.iter().zip(values) {
                registers.insert(param, value);
            }
            current = target;
        }
    }

    fn assert_unchanged(
        before: &ExecutionUnit<RegionedAbsoluteAddr>,
        after: &ExecutionUnit<RegionedAbsoluteAddr>,
    ) {
        assert_eq!(after.entry_block_id, before.entry_block_id);
        assert_eq!(after.register_map, before.register_map);
        assert_eq!(after.blocks, before.blocks);
    }

    #[test]
    fn sinks_a_shared_true_edge_dag_and_preserves_both_guard_outcomes() {
        let mut eu = shared_dag_unit();
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert_eq!(execute(&before, 0, 7), execute(&eu, 0, 7));
        assert_eq!(execute(&before, 1, 7), execute(&eu, 1, 7));
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &eu.blocks[&BlockId(0)].terminator
        else {
            panic!("guard must remain a branch");
        };
        let head = &eu.blocks[&BlockId(0)];
        assert!(!head.instructions.iter().any(|inst| {
            matches!(
                def_reg(inst),
                Some(RegisterId(3) | RegisterId(4) | RegisterId(5))
            )
        }));
        let true_shim = &eu.blocks[&true_block.0];
        assert!(
            true_shim
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(3), ..)) })
        );
        assert!(
            true_shim
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(4), ..)) })
        );
        assert!(
            true_shim
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(5), ..)) })
        );
        assert!(
            true_shim.instructions.iter().any(|inst| {
                matches!(inst, SIRInstruction::Store(_, _, 8, RegisterId(5), _, _))
            })
        );
        let false_shim = &eu.blocks[&false_block.0];
        assert!(
            false_shim.instructions.iter().any(|inst| {
                matches!(inst, SIRInstruction::Store(_, _, 8, RegisterId(2), _, _))
            })
        );
    }

    #[test]
    fn one_branch_specializes_a_repeated_predicate_and_merges_only_the_live_out() {
        let mut eu = repeated_predicate_unit();
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        for condition in [0, 1] {
            for input in [3, 5, 7, 11] {
                assert_eq!(
                    execute(&before, condition, input),
                    execute(&eu, condition, input),
                );
            }
        }
        let head = &eu.blocks[&BlockId(0)];
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &head.terminator
        else {
            panic!("the repeated predicate must form one shared branch");
        };
        assert!(
            eu.blocks[&true_block.0]
                .instructions
                .iter()
                .all(|inst| !matches!(inst, SIRInstruction::Mux(_, RegisterId(0), ..)))
        );
        assert!(
            eu.blocks[&false_block.0]
                .instructions
                .iter()
                .all(|inst| !matches!(inst, SIRInstruction::Mux(_, RegisterId(0), ..)))
        );
        let merge = match &eu.blocks[&true_block.0].terminator {
            SIRTerminator::Jump(merge, args) => {
                assert_eq!(
                    args.len(),
                    1,
                    "only the reconverged result crosses the edge"
                );
                *merge
            }
            other => panic!("unexpected true-edge terminator: {other:?}"),
        };
        assert_eq!(eu.blocks[&merge].params, vec![RegisterId(11)]);
        assert!(matches!(
            eu.blocks[&merge].instructions.as_slice(),
            [SIRInstruction::Store(_, _, 8, RegisterId(11), _, _)]
        ));
        for dead in 3..=10 {
            assert!(
                !eu.register_map.contains_key(&RegisterId(dead)),
                "replaced original r{dead} must not allocate a native VReg"
            );
        }
        assert!(
            eu.register_map.contains_key(&RegisterId(11)),
            "the merge parameter keeps its original register type"
        );
    }

    #[test]
    fn same_predicate_region_moves_static_and_dynamic_loads_with_their_arms() {
        let mut eu = repeated_predicate_unit();
        eu.register_map.insert(RegisterId(12), bit(8));
        eu.register_map.insert(RegisterId(13), bit(8));
        let block = eu.blocks.get_mut(&BlockId(0)).unwrap();
        block.instructions.insert(
            0,
            SIRInstruction::Load(RegisterId(12), address(50), SIROffset::Static(0), 8),
        );
        block.instructions.insert(
            1,
            SIRInstruction::Load(
                RegisterId(13),
                address(51),
                SIROffset::Dynamic(RegisterId(2)),
                8,
            ),
        );
        let SIRInstruction::Binary(_, true_lhs, _, _) = &mut block.instructions[2] else {
            panic!("fixture true arm must start with a binary operation");
        };
        *true_lhs = RegisterId(12);
        let SIRInstruction::Binary(_, false_lhs, _, _) = &mut block.instructions[3] else {
            panic!("fixture false arm must start with a binary operation");
        };
        *false_lhs = RegisterId(13);
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        for condition in [0, 1] {
            assert_eq!(execute(&before, condition, 7), execute(&eu, condition, 7));
        }
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &eu.blocks[&BlockId(0)].terminator
        else {
            panic!("load-bearing repeated predicate must branch");
        };
        assert!(
            eu.blocks[&true_block.0]
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Load(_, _, SIROffset::Static(0), 8)))
        );
        assert!(
            eu.blocks[&false_block.0].instructions.iter().any(|inst| {
                matches!(inst, SIRInstruction::Load(_, _, SIROffset::Dynamic(_), 8))
            })
        );
    }

    #[test]
    fn nested_same_predicate_mux_drops_the_unselected_cofactor() {
        let mut eu = repeated_predicate_unit();
        for reg in 12..=14 {
            eu.register_map.insert(RegisterId(reg), bit(8));
        }
        let block = eu.blocks.get_mut(&BlockId(0)).unwrap();
        let store = block.instructions.pop().unwrap();
        block.instructions.extend([
            SIRInstruction::Unary(RegisterId(12), UnaryOp::BitNot, RegisterId(11)),
            SIRInstruction::Unary(RegisterId(13), UnaryOp::Minus, RegisterId(11)),
            SIRInstruction::Mux(
                RegisterId(14),
                RegisterId(0),
                RegisterId(12),
                RegisterId(13),
            ),
        ]);
        let SIRInstruction::Store(address, offset, width, _, triggers, sites) = store else {
            panic!("fixture must end in a Store");
        };
        block.instructions.push(SIRInstruction::Store(
            address,
            offset,
            width,
            RegisterId(14),
            triggers,
            sites,
        ));
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        for condition in [0, 1] {
            assert_eq!(execute(&before, condition, 9), execute(&eu, condition, 9));
        }
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &eu.blocks[&BlockId(0)].terminator
        else {
            panic!("nested repeated predicate must branch");
        };
        assert!(
            eu.blocks[&true_block.0]
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Unary(_, UnaryOp::BitNot, _)) })
        );
        assert!(
            !eu.blocks[&true_block.0]
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Unary(_, UnaryOp::Minus, _)) })
        );
        assert!(
            eu.blocks[&false_block.0]
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Unary(_, UnaryOp::Minus, _)) })
        );
        assert!(
            !eu.blocks[&false_block.0]
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Unary(_, UnaryOp::BitNot, _)) })
        );
    }

    #[test]
    fn keeps_a_locally_defined_predicate_before_the_recovered_branch() {
        let mut register_map = HashMap::default();
        for reg in 0..=14 {
            register_map.insert(RegisterId(reg), bit(1));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            vec![
                SIRInstruction::Unary(RegisterId(3), UnaryOp::Ident, RegisterId(0)),
                SIRInstruction::Binary(RegisterId(4), RegisterId(1), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(5), RegisterId(4), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(6), RegisterId(5), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(7), RegisterId(6), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(8), RegisterId(2), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(RegisterId(9), RegisterId(8), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(RegisterId(10), RegisterId(9), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(
                    RegisterId(11),
                    RegisterId(10),
                    BinaryOp::Mul,
                    RegisterId(2),
                ),
                // The predicate is also selected as data. Moving r3 into the
                // true arm would leave the newly introduced Branch undefined.
                SIRInstruction::Mux(RegisterId(12), RegisterId(3), RegisterId(3), RegisterId(7)),
                SIRInstruction::Mux(RegisterId(13), RegisterId(3), RegisterId(3), RegisterId(11)),
                SIRInstruction::Binary(
                    RegisterId(14),
                    RegisterId(12),
                    BinaryOp::Or,
                    RegisterId(13),
                ),
                SIRInstruction::Store(
                    address(42),
                    SIROffset::Static(0),
                    1,
                    RegisterId(14),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        for condition in [0, 1] {
            for input in [0, 1] {
                assert_eq!(
                    execute(&before, condition, input),
                    execute(&eu, condition, input),
                );
            }
        }
        let head = &eu.blocks[&BlockId(0)];
        assert!(head.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Unary(RegisterId(3), UnaryOp::Ident, RegisterId(0))
            )
        }));
        assert!(matches!(
            head.terminator,
            SIRTerminator::Branch {
                cond: RegisterId(3),
                ..
            }
        ));
    }

    #[test]
    fn rejects_a_value_used_from_the_false_region() {
        let mut eu = shared_dag_unit();
        eu.blocks
            .get_mut(&BlockId(2))
            .unwrap()
            .instructions
            .push(SIRInstruction::Store(
                address(13),
                SIROffset::Static(0),
                8,
                RegisterId(5),
                Vec::new(),
                Vec::new(),
            ));
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn an_aliasing_write_keeps_the_load_before_the_guard() {
        let mut eu = shared_dag_unit();
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        head.instructions = vec![
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
            SIRInstruction::Load(RegisterId(5), address(20), SIROffset::Static(0), 8),
            SIRInstruction::Store(
                address(20),
                SIROffset::Static(0),
                8,
                RegisterId(1),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Mux(RegisterId(6), RegisterId(0), RegisterId(5), RegisterId(2)),
            SIRInstruction::Store(
                address(10),
                SIROffset::Static(0),
                8,
                RegisterId(6),
                Vec::new(),
                Vec::new(),
            ),
        ];
        if let SIRTerminator::Branch { true_block, .. } = &mut head.terminator {
            true_block.1 = vec![RegisterId(5)];
        }
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn rejects_a_load_after_the_first_distributed_store() {
        let mut eu = shared_dag_unit();
        eu.register_map.insert(RegisterId(9), bit(8));
        eu.blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions
            .push(SIRInstruction::Load(
                RegisterId(9),
                address(30),
                SIROffset::Static(0),
                8,
            ));
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn rejects_an_effect_after_the_first_distributed_store() {
        let mut eu = shared_dag_unit();
        eu.blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions
            .push(SIRInstruction::RuntimeEvent {
                site_id: 7,
                args: vec![RegisterId(1)],
            });
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn preserves_multiple_distributed_store_order() {
        let mut eu = shared_dag_unit();
        eu.register_map.insert(RegisterId(9), bit(8));
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        head.instructions.push(SIRInstruction::Mux(
            RegisterId(9),
            RegisterId(0),
            RegisterId(4),
            RegisterId(2),
        ));
        head.instructions.push(SIRInstruction::Store(
            address(14),
            SIROffset::Static(0),
            8,
            RegisterId(9),
            Vec::new(),
            Vec::new(),
        ));
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert_eq!(execute(&before, 0, 3), execute(&eu, 0, 3));
        assert_eq!(execute(&before, 1, 3), execute(&eu, 1, 3));
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &eu.blocks[&BlockId(0)].terminator
        else {
            panic!("expected guard branch");
        };
        let addresses = |block: &BasicBlock<RegionedAbsoluteAddr>| {
            block
                .instructions
                .iter()
                .filter_map(|inst| match inst {
                    SIRInstruction::Store(address, ..) => Some(address.instance_id),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            addresses(&eu.blocks[&true_block.0]),
            vec![InstanceId(10), InstanceId(14)]
        );
        assert_eq!(
            addresses(&eu.blocks[&false_block.0]),
            vec![InstanceId(10), InstanceId(14)]
        );
    }

    #[test]
    fn four_state_mode_is_non_destructive() {
        let mut eu = shared_dag_unit();
        let before = eu.clone();
        let mut options = PassOptions::default();
        options.four_state = true;

        GuardedRegionSinkingPass.run(&mut eu, &options);

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn trigger_only_zero_width_store_is_non_destructive() {
        let mut eu = shared_dag_unit();
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        let SIRInstruction::Store(_, _, width, _, triggers, _) = &mut head.instructions[5] else {
            panic!("fixture must end in a store");
        };
        *width = 0;
        *triggers = vec![TriggerIdWithKind {
            kind: DomainKind::ClockPosedge,
            id: 9,
        }];
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn a_second_run_does_not_rewrite_generated_regions() {
        let mut eu = shared_dag_unit();
        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());
        eu.verify_result().unwrap();
        let once = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&once, &eu);
    }

    #[test]
    fn multi_bit_branch_guard_is_rejected() {
        let mut eu = shared_dag_unit();
        eu.register_map.insert(RegisterId(0), bit(8));
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "TYPE.BRANCH_CONDITION"
        );
    }

    #[test]
    fn narrow_mux_arm_is_not_connected_directly_to_a_wider_store() {
        let mut eu = shared_dag_unit();
        eu.register_map.insert(RegisterId(2), bit(4));
        eu.register_map.insert(RegisterId(8), bit(4));
        eu.register_map.insert(RegisterId(9), bit(8));
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        head.instructions.insert(
            4,
            SIRInstruction::Unary(RegisterId(9), UnaryOp::Ident, RegisterId(2)),
        );
        let SIRInstruction::Mux(_, _, _, false_value) = &mut head.instructions[5] else {
            panic!("fixture must contain the distributed mux");
        };
        *false_value = RegisterId(9);
        let false_block = eu.blocks.get_mut(&BlockId(2)).unwrap();
        let SIRInstruction::Store(_, _, width, _, _, _) = &mut false_block.instructions[0] else {
            panic!("fixture false block must store its parameter");
        };
        *width = 4;
        eu.verify_result().unwrap();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(
            eu.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .all(|instruction| {
                    let SIRInstruction::Store(_, _, width, value, _, _) = instruction else {
                        return true;
                    };
                    *width == eu.register_map[value].width()
                })
        );
    }

    #[test]
    fn removable_mux_cannot_also_supply_a_dynamic_store_offset() {
        let mut eu = shared_dag_unit();
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        let SIRInstruction::Store(_, offset, _, _, _, _) = &mut head.instructions[5] else {
            panic!("fixture must end in the distributed store");
        };
        *offset = SIROffset::Dynamic(RegisterId(6));
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn one_removable_mux_cannot_supply_another_store_offset() {
        let mut eu = shared_dag_unit();
        eu.register_map.insert(RegisterId(9), bit(8));
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        head.instructions.insert(
            5,
            SIRInstruction::Mux(RegisterId(9), RegisterId(0), RegisterId(4), RegisterId(2)),
        );
        let SIRInstruction::Store(_, first_offset, _, _, _, _) = &mut head.instructions[6] else {
            panic!("fixture first store must remain a store");
        };
        *first_offset = SIROffset::Dynamic(RegisterId(9));
        head.instructions.push(SIRInstruction::Store(
            address(14),
            SIROffset::Static(0),
            8,
            RegisterId(9),
            Vec::new(),
            Vec::new(),
        ));
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn malformed_input_is_non_destructive() {
        let mut eu = shared_dag_unit();
        eu.blocks.remove(&BlockId(2));
        assert!(eu.verify_result().is_err());
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn block_id_overflow_is_non_destructive() {
        let mut eu = shared_dag_unit();
        let terminal = eu.blocks.remove(&BlockId(3)).unwrap();
        let max = BlockId(usize::MAX);
        eu.blocks.insert(
            max,
            BasicBlock {
                id: max,
                ..terminal
            },
        );
        for id in [BlockId(1), BlockId(2)] {
            eu.blocks.get_mut(&id).unwrap().terminator = SIRTerminator::Jump(max, Vec::new());
        }
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn native_block_id_overflow_is_non_destructive() {
        let mut eu = shared_dag_unit();
        let terminal = eu.blocks.remove(&BlockId(3)).unwrap();
        let max = BlockId(u32::MAX as usize);
        eu.blocks.insert(
            max,
            BasicBlock {
                id: max,
                ..terminal
            },
        );
        for id in [BlockId(1), BlockId(2)] {
            eu.blocks.get_mut(&id).unwrap().terminator = SIRTerminator::Jump(max, Vec::new());
        }
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }
}
