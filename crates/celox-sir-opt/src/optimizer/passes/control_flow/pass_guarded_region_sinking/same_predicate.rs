//! Forming regions of values guarded by the same predicate.

use super::*;

pub(super) fn form_same_predicate_regions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
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
            // One false-edge zero is shared by every implicit gated-And in a
            // region. Explicit-Mux-only regions leave this reservation unused.
            .checked_add(1)?
            .checked_add(2)
    });
    let Some(additional_registers) = additional_registers else {
        return;
    };
    let max_register = eu.register_map.keys().map(|id| id.0).max().unwrap_or(0);
    if max_register.checked_add(additional_registers).is_none() {
        return;
    }

    if tracing::enabled!(tracing::Level::DEBUG) {
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
        tracing::debug!(
            "[same-predicate-regions] regions={} muxes={muxes} true_owned={true_owned} false_owned={false_owned} live_outs={live_outs}",
            plans.len(),
        );
        for plan in &plans {
            tracing::debug!(
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

pub(super) fn best_same_predicate_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> Option<SamePredicatePlan> {
    let block = eu.blocks.get(&block_id)?;
    let mut best: Option<SamePredicatePlan> = None;
    // Every candidate reads the same immutable input block. Build its def
    // index once, and only if a predicate has enough sources to plan.
    let mut block_definitions = None;
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
            for condition in conditional_source_conditions(eu, &block.instructions[index]) {
                groups.entry(condition).or_default().push(index);
            }
        }
        for (condition, muxes) in groups {
            // One Mux is already handled by ordinary cost-directed lowering;
            // this transform exists to share one branch across a region.
            if muxes.len() < 2 {
                continue;
            }
            let local_defs = block_definitions.get_or_insert_with(|| {
                block
                    .instructions
                    .iter()
                    .enumerate()
                    .filter_map(|(index, inst)| def_reg(inst).map(|reg| (reg, index)))
                    .collect::<HashMap<_, _>>()
            });
            let Some(candidate) = plan_same_predicate_region(
                eu,
                block_id,
                segment_start,
                segment_end,
                condition,
                &muxes,
                uses,
                local_defs,
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
pub(super) fn plan_same_predicate_region(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    segment_start: usize,
    segment_end: usize,
    condition: RegisterId,
    mux_indices: &[usize],
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    local_defs: &HashMap<RegisterId, usize>,
) -> Option<SamePredicatePlan> {
    let block = eu.blocks.get(&block_id)?;
    if eu.register_map.get(&condition).map(RegisterType::width) != Some(1) {
        return None;
    }
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

    // Specialize the forward slice separately on each edge. Besides explicit
    // Muxes, a one-bit `condition & payload` is an implicit
    // `Mux(condition, payload, 0)` in two-state mode. Recognizing it here
    // recovers the control dependence without first materializing another
    // select instruction.
    let (true_muxes, true_cofactor) = specialize_region_needed(
        eu, condition, &live_outs, true, &muxes, &cofactor, local_defs, block,
    );
    let (false_muxes, false_cofactor) = specialize_region_needed(
        eu, condition, &live_outs, false, &muxes, &cofactor, local_defs, block,
    );
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
        let source = conditional_source(eu, &block.instructions[index], condition)?;
        if source.condition != condition {
            return None;
        }
        if true_muxes.contains(&index) {
            if let ConditionalArm::Value(value) = source.true_arm {
                true_roots.push(value);
            }
        }
        if false_muxes.contains(&index) {
            if let ConditionalArm::Value(value) = source.false_arm {
                false_roots.push(value);
            }
        }
    }
    let true_reachable = collect_region_reachable_defs(
        &true_roots,
        segment_start,
        segment_end,
        local_defs,
        block,
        &muxes,
    );
    let false_reachable = collect_region_reachable_defs(
        &false_roots,
        segment_start,
        segment_end,
        local_defs,
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
    close_region_arm(
        eu,
        condition,
        &mut true_owned,
        true,
        block_id,
        block,
        &true_muxes,
        uses,
    );
    close_region_arm(
        eu,
        condition,
        &mut false_owned,
        false,
        block_id,
        block,
        &false_muxes,
        uses,
    );
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
    if !same_predicate_arm_is_closed(eu, block, &plan, true)
        || !same_predicate_arm_is_closed(eu, block, &plan, false)
    {
        return None;
    }
    let net_benefit_scaled = same_predicate_net_benefit(eu, block, &plan)?;
    Some(SamePredicatePlan {
        net_benefit_scaled,
        ..plan
    })
}

pub(super) fn collect_region_reachable_defs(
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

pub(super) fn specialize_region_needed(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    condition: RegisterId,
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
            let Some(source) = conditional_source(eu, &block.instructions[index], condition) else {
                continue;
            };
            if let ConditionalArm::Value(value) = source.selected(true_arm) {
                work.push(value);
            }
        } else if cofactor.contains(&index) && needed_cofactor.insert(index) {
            work.extend(instruction_uses(&block.instructions[index]));
        }
    }
    (needed_muxes, needed_cofactor)
}

pub(super) fn close_region_arm(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    condition: RegisterId,
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
                            let Some(source) = conditional_source(
                                eu,
                                &block.instructions[use_index],
                                condition,
                            ) else {
                                return true;
                            };
                            source.condition == dst
                                || !matches!(source.selected(true_arm), ConditionalArm::Value(value) if value == dst)
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

pub(super) fn same_predicate_arm_is_closed(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
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
            let Some(source) = conditional_source(eu, &block.instructions[index], plan.condition)
            else {
                return false;
            };
            if let ConditionalArm::Value(selected) = source.selected(true_arm) {
                if removed.contains(&selected) && !mapped.contains(&selected) {
                    return false;
                }
            }
            mapped.insert(source.dst);
        }
    }
    plan.live_outs.iter().all(|value| mapped.contains(value))
}

pub(super) fn register_chunks(
    register_map: &HashMap<RegisterId, RegisterType>,
    value: RegisterId,
) -> u128 {
    register_map
        .get(&value)
        .map(|ty| ty.width().div_ceil(64).max(1) as u128)
        .unwrap_or(1)
}

pub(super) fn same_predicate_net_benefit(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &SamePredicatePlan,
) -> Option<u128> {
    const BRANCH_CONTROL_COST: u128 = 3;
    const MISPREDICT_COST: u128 = 16;
    const PHI_COPY_COST_PER_CHUNK: u128 = 2;
    const LIVE_THROUGH_COST_PER_CHUNK: u128 = 1;

    let instruction_cost = |index: usize| {
        super::super::cost_model::estimate_clif_cost(
            &block.instructions[index],
            &eu.register_map,
            false,
        ) as u128
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

pub(super) fn apply_same_predicate_plan(
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

pub(super) fn build_same_predicate_arm(
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
    let mut zero = None;
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
            let source = conditional_source(eu, inst, plan.condition)
                .expect("same-predicate source must remain conditional");
            let selected = match source.selected(true_arm) {
                ConditionalArm::Value(value) => replacements.get(&value).copied().unwrap_or(value),
                ConditionalArm::Zero => *zero.get_or_insert_with(|| {
                    *reg_counter += 1;
                    let value = RegisterId(*reg_counter);
                    eu.register_map
                        .insert(value, eu.register_map[&source.dst].clone());
                    instructions.push(SIRInstruction::Imm(value, SIRValue::new(0u8)));
                    value
                }),
            };
            replacements.insert(source.dst, selected);
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

pub(super) fn clone_pure_instruction(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    dst: RegisterId,
    replacements: &HashMap<RegisterId, RegisterId>,
) -> Option<SIRInstruction<RegionedAbsoluteAddr>> {
    if inst.has_side_effects() {
        return None;
    }
    let mut clone = inst.clone();
    clone.for_each_use_mut(|value| {
        if let Some(&replacement) = replacements.get(value) {
            *value = replacement;
        }
    });
    *clone.defined_register_mut()? = dst;
    Some(clone)
}

pub(super) fn instruction_is_same_predicate_region_value(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
) -> bool {
    instruction_is_movable(inst) || matches!(inst, SIRInstruction::Load(..))
}
