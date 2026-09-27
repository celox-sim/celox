//! Atomic, whole-priority-chain, and existing-CFG instruction placement.

use super::*;

pub(super) fn find_atomic_priority_placement(
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
pub(super) fn find_existing_cfg_placement(
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

pub(super) fn apply_existing_cfg_placement(
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

pub(super) fn apply_atomic_priority_placement(
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
