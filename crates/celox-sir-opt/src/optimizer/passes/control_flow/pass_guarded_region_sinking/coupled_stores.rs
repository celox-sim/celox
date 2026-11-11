//! Forming regions of coupled stores.

use super::*;

pub(super) fn form_coupled_store_regions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    form_coupled_store_regions_verified_by(eu, verify_coupled_store_batch);
}

/// [`form_coupled_store_regions`] with the batch check passed in, so tests
/// can observe how often it runs.
pub(super) fn form_coupled_store_regions_verified_by(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    verify: impl FnOnce(&ExecutionUnit<RegionedAbsoluteAddr>),
) {
    let uses = collect_uses(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);
    let plans = block_ids
        .into_iter()
        .filter_map(|block| best_coupled_store_plan(eu, block, &uses))
        .collect::<Vec<_>>();
    if plans.is_empty() {
        return;
    }

    let Some(additional_blocks) = plans.iter().try_fold(0usize, |total, plan| {
        total.checked_add(plan.conditions.len().checked_mul(2)?.checked_add(1)?)
    }) else {
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
    let max_register = eu.register_map.keys().map(|id| id.0).max().unwrap_or(0);
    let Some(additional_registers) = plans.iter().try_fold(0usize, |total, plan| {
        total.checked_add(plan.conditions.len().checked_mul(2)?)
    }) else {
        return;
    };
    if max_register.checked_add(additional_registers).is_none() {
        return;
    }

    if tracing::enabled!(tracing::Level::DEBUG) {
        for plan in &plans {
            tracing::debug!(
                "[coupled-store-region] block={} depth={} stores={} owned={} benefit_scaled={}",
                plan.block_id.0,
                plan.conditions.len(),
                plan.stores.len(),
                plan.placements.len(),
                plan.net_benefit_scaled,
            );
        }
    }

    let mut next_block = first_new_block;
    let mut reg_counter = max_register;
    for plan in plans {
        let consumed = plan.conditions.len() * 2 + 1;
        apply_coupled_store_plan(eu, plan, next_block, &mut reg_counter);
        next_block += consumed;
    }
    // Plans belong to distinct original blocks and were collected before any
    // mutation. Check the completed batch once rather than rescanning every
    // unrelated definition after each local rewrite.
    verify(eu);
}

pub(super) fn verify_coupled_store_batch(eu: &ExecutionUnit<RegionedAbsoluteAddr>) {
    debug_assert_eq!(eu.verify_result(), Ok(()));
}

pub(super) fn best_coupled_store_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> Option<CoupledStorePlan> {
    let block = eu.blocks.get(&block_id)?;
    let local_defs = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| def_reg(instruction).map(|value| (value, index)))
        .collect::<HashMap<_, _>>();
    let mut groups = BTreeMap::<RegisterId, Vec<DistributedStore>>::new();
    for (index, instruction) in block.instructions.iter().enumerate() {
        let SIRInstruction::Store(_, offset, width, source, _, _) = instruction else {
            continue;
        };
        let Some(&mux_index) = local_defs.get(source) else {
            continue;
        };
        let SIRInstruction::Mux(result, condition, true_value, false_value) =
            block.instructions[mux_index]
        else {
            continue;
        };
        if result != *source
            || mux_index >= index
            || *width == 0
            || eu.register_map.get(&condition).map(RegisterType::width) != Some(1)
            || eu
                .register_map
                .get(&true_value)
                .is_none_or(|ty| ty.width() < *width)
            || eu
                .register_map
                .get(&false_value)
                .is_none_or(|ty| ty.width() < *width)
            || offset
                .dynamic_registers()
                .into_iter()
                .flatten()
                .any(|dynamic| dynamic == result)
        {
            continue;
        }
        groups.entry(condition).or_default().push(DistributedStore {
            index,
            mux_index,
            mux_result: result,
            true_value,
            false_value,
        });
    }

    let mut best: Option<CoupledStorePlan> = None;
    for (condition, stores) in groups {
        if stores.len() < 2 {
            continue;
        }
        let Some(candidate) =
            plan_coupled_store_region(eu, block_id, condition, stores, &local_defs, uses)
        else {
            continue;
        };
        if best.as_ref().is_none_or(|current| {
            candidate.net_benefit_scaled > current.net_benefit_scaled
                || candidate.net_benefit_scaled == current.net_benefit_scaled
                    && candidate.conditions < current.conditions
        }) {
            best = Some(candidate);
        }
    }
    best
}

pub(super) fn plan_coupled_store_region(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    condition: RegisterId,
    mut stores: Vec<DistributedStore>,
    local_defs: &HashMap<RegisterId, usize>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> Option<CoupledStorePlan> {
    let block = eu.blocks.get(&block_id)?;
    stores.sort_unstable_by_key(|store| store.index);
    let first_store = stores.first()?.index;
    let last_store = stores.last()?.index;
    let store_indices = stores
        .iter()
        .map(|store| store.index)
        .collect::<HashSet<_>>();
    if block
        .instructions
        .iter()
        .enumerate()
        .take(last_store + 1)
        .skip(first_store + 1)
        .any(|(index, instruction)| {
            matches!(instruction, SIRInstruction::Load(..))
                || instruction_has_effect(instruction) && !store_indices.contains(&index)
        })
    {
        return None;
    }

    let output_values = stores
        .iter()
        .map(|store| store.mux_result)
        .collect::<HashSet<_>>();
    if output_values.len() != stores.len() {
        return None;
    }
    for store in &stores {
        if uses
            .get(&store.mux_result)
            .into_iter()
            .flatten()
            .any(|site| {
                !matches!(
                    site,
                    UseSite::Instruction { block, index }
                        if *block == block_id
                            && store_indices.contains(index)
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

    // Peel the common outer-to-inner priority spine from all store values at
    // once.  Every selected leaf is a tuple, so a definition shared by result
    // and flags remains owned by that leaf instead of escaping through the
    // other output.
    let mut roots = stores
        .iter()
        .map(|store| store.mux_result)
        .collect::<Vec<_>>();
    let mut conditions = Vec::new();
    let mut leaf_values = Vec::new();
    let mut removable_muxes = HashSet::default();
    loop {
        let mut level_condition = None;
        let mut selected = Vec::with_capacity(roots.len());
        let mut fallthrough = Vec::with_capacity(roots.len());
        let mut level_muxes = Vec::with_capacity(roots.len());
        for root in &roots {
            let Some(&index) = local_defs.get(root) else {
                level_muxes.clear();
                break;
            };
            let SIRInstruction::Mux(dst, mux_condition, true_value, false_value) =
                block.instructions[index]
            else {
                level_muxes.clear();
                break;
            };
            if dst != *root
                || eu.register_map.get(&mux_condition).map(RegisterType::width) != Some(1)
                || level_condition.is_some_and(|expected| expected != mux_condition)
            {
                level_muxes.clear();
                break;
            }
            level_condition = Some(mux_condition);
            level_muxes.push(index);
            selected.push(true_value);
            fallthrough.push(false_value);
        }
        if level_muxes.len() != roots.len() {
            break;
        }
        let level_condition = level_condition?;
        if conditions.is_empty() && level_condition != condition {
            return None;
        }
        conditions.push(level_condition);
        leaf_values.push(selected);
        removable_muxes.extend(level_muxes);
        roots = fallthrough;
    }
    if conditions.is_empty() {
        return None;
    }
    leaf_values.push(roots);

    if removable_muxes.iter().any(|index| {
        let Some(value) = def_reg(&block.instructions[*index]) else {
            return true;
        };
        uses.get(&value).into_iter().flatten().any(|site| {
            !matches!(
                site,
                UseSite::Instruction { block, index }
                    if *block == block_id
                        && (removable_muxes.contains(index)
                            || store_indices.contains(index))
            )
        })
    }) {
        return None;
    }

    let mut masks = HashMap::<usize, Vec<bool>>::default();
    for (leaf, values) in leaf_values.iter().enumerate() {
        mark_coupled_store_leaf_defs(
            values,
            leaf..leaf + 1,
            leaf_values.len(),
            last_store,
            local_defs,
            block,
            &removable_muxes,
            &mut masks,
        );
    }
    // Every store template is emitted in every leaf.  Its dynamic address
    // operands therefore have to dominate the complete region, even when the
    // same calculation is also reachable from one leaf's value cone.  Without
    // this all-leaf mask, that shared calculation could be sunk into only that
    // leaf and leave the duplicated stores in the other leaves with a
    // non-dominating operand.
    for store in &stores {
        let SIRInstruction::Store(_, offset, _, _, _, _) = &block.instructions[store.index] else {
            unreachable!("coupled-store plan must refer to a Store")
        };
        let dynamic_offsets = offset
            .dynamic_registers()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        mark_coupled_store_leaf_defs(
            &dynamic_offsets,
            0..leaf_values.len(),
            leaf_values.len(),
            last_store,
            local_defs,
            block,
            &removable_muxes,
            &mut masks,
        );
    }
    // A condition at level `n` executes on every path which reaches that
    // decision, namely leaves n..=depth.  Include its defining slice in the
    // same path masks as data values so the nearest-common-dominator
    // placement does not incorrectly classify it as a final-leaf value and
    // then reject the entire dependency chain as a forward branch use.
    for (level, condition) in conditions.iter().copied().enumerate().skip(1) {
        mark_coupled_store_leaf_defs(
            std::slice::from_ref(&condition),
            level..leaf_values.len(),
            leaf_values.len(),
            last_store,
            local_defs,
            block,
            &removable_muxes,
            &mut masks,
        );
    }
    let mut placements = HashMap::default();
    for (index, mask) in masks {
        let mut leaves = mask
            .iter()
            .enumerate()
            .filter_map(|(leaf, needed)| needed.then_some(leaf));
        let Some(leaf) = leaves.next() else {
            continue;
        };
        let site = if leaves.next().is_none() {
            CoupledPlacementSite::Leaf(leaf)
        } else if leaf > 0 {
            // A priority chain's decision `leaf` is the nearest common
            // dominator of every remaining leaf.  Placing the definition
            // there avoids evaluating it on earlier exits without cloning it.
            CoupledPlacementSite::Decision(leaf)
        } else {
            // Every path can need this definition, so it belongs in the head.
            continue;
        };
        placements.insert(index, site);
    }
    close_coupled_store_placements(
        eu,
        block_id,
        &conditions,
        &mut placements,
        &removable_muxes,
        uses,
    );
    if placements.is_empty() {
        return None;
    }

    let mut plan = CoupledStorePlan {
        block_id,
        last_store,
        stores,
        conditions,
        leaf_values,
        placements,
        removable_muxes,
        net_benefit_scaled: 0,
    };
    plan.net_benefit_scaled = coupled_store_net_benefit(eu, block, &plan)?;
    Some(plan)
}

pub(super) fn coupled_site_dominates(
    definition: CoupledPlacementSite,
    use_site: CoupledPlacementSite,
) -> bool {
    match (definition, use_site) {
        (CoupledPlacementSite::Decision(def), CoupledPlacementSite::Decision(use_)) => use_ >= def,
        (CoupledPlacementSite::Decision(def), CoupledPlacementSite::Leaf(use_)) => use_ >= def,
        (CoupledPlacementSite::Leaf(def), CoupledPlacementSite::Leaf(use_)) => def == use_,
        (CoupledPlacementSite::Leaf(_), CoupledPlacementSite::Decision(_)) => false,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn mark_coupled_store_leaf_defs(
    roots: &[RegisterId],
    needed_leaves: std::ops::Range<usize>,
    leaf_count: usize,
    last_store: usize,
    local_defs: &HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    removable_muxes: &HashSet<usize>,
    masks: &mut HashMap<usize, Vec<bool>>,
) {
    let mut visited = HashSet::default();
    let mut worklist = roots.to_vec();
    while let Some(value) = worklist.pop() {
        if !visited.insert(value) {
            continue;
        }
        let Some(&index) = local_defs.get(&value) else {
            continue;
        };
        if index > last_store
            || removable_muxes.contains(&index)
            || !instruction_is_movable(&block.instructions[index])
        {
            continue;
        }
        let mask = masks
            .entry(index)
            .or_insert_with(|| vec![false; leaf_count]);
        mask[needed_leaves.clone()].fill(true);
        worklist.extend(instruction_uses(&block.instructions[index]));
    }
}

pub(super) fn close_coupled_store_placements(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    conditions: &[RegisterId],
    placements: &mut HashMap<usize, CoupledPlacementSite>,
    removable_muxes: &HashSet<usize>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) {
    let block = &eu.blocks[&block_id];
    let mut condition_levels = HashMap::default();
    for (level, condition) in conditions.iter().copied().enumerate() {
        condition_levels
            .entry(condition)
            .and_modify(|earliest: &mut usize| *earliest = (*earliest).min(level))
            .or_insert(level);
    }
    loop {
        let mut rejected = Vec::new();
        for (&index, &definition_site) in placements.iter() {
            let Some(value) = def_reg(&block.instructions[index]) else {
                rejected.push(index);
                continue;
            };
            let escapes = uses
                .get(&value)
                .into_iter()
                .flatten()
                .any(|site| match *site {
                    UseSite::Instruction {
                        block: use_block,
                        index: use_index,
                    } if use_block == block_id => {
                        if removable_muxes.contains(&use_index) {
                            match &block.instructions[use_index] {
                                SIRInstruction::Mux(_, condition, _, _) if *condition == value => {
                                    condition_levels.get(condition).is_none_or(|&level| {
                                        !coupled_site_dominates(
                                            definition_site,
                                            CoupledPlacementSite::Decision(level),
                                        )
                                    })
                                }
                                _ => false,
                            }
                        } else if let Some(&use_site) = placements.get(&use_index) {
                            !coupled_site_dominates(definition_site, use_site)
                        } else {
                            true
                        }
                    }
                    _ => true,
                });
            if escapes {
                rejected.push(index);
            }
        }
        if rejected.is_empty() {
            break;
        }
        for index in rejected {
            placements.remove(&index);
        }
    }
}

pub(super) fn coupled_store_net_benefit(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &CoupledStorePlan,
) -> Option<u128> {
    const BRANCH_CONTROL_COST: u128 = 3;
    const MISPREDICT_COST: u128 = 16;
    let cost = |index: usize| {
        super::super::cost_model::estimate_clif_cost(
            &block.instructions[index],
            &eu.register_map,
            false,
        ) as u128
    };
    let arm_cost = plan
        .placements
        .keys()
        .copied()
        .map(cost)
        .fold(0u128, u128::saturating_add);
    let mux_cost = plan
        .removable_muxes
        .iter()
        .copied()
        .map(cost)
        .fold(0u128, u128::saturating_add);
    let saved_scaled = arm_cost.saturating_add(mux_cost.saturating_mul(2));
    let introduced_scaled = BRANCH_CONTROL_COST
        .saturating_mul(plan.conditions.len() as u128)
        .saturating_mul(2)
        .saturating_add(MISPREDICT_COST.saturating_mul(plan.conditions.len() as u128));
    (saved_scaled > introduced_scaled).then(|| saved_scaled - introduced_scaled)
}

pub(super) fn apply_coupled_store_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: CoupledStorePlan,
    first_new_block: usize,
    reg_counter: &mut usize,
) {
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("planned coupled-store block must remain present");
    let dead_mux_values = plan
        .removable_muxes
        .iter()
        .filter_map(|index| def_reg(&original.instructions[*index]))
        .collect::<Vec<_>>();
    let store_templates = plan
        .stores
        .iter()
        .map(|store| original.instructions[store.index].clone())
        .collect::<Vec<_>>();
    let mut head = Vec::new();
    let mut leaves = vec![Vec::new(); plan.leaf_values.len()];
    let mut decisions = vec![Vec::new(); plan.conditions.len()];
    let mut continuation = Vec::new();
    for (index, instruction) in original.instructions.into_iter().enumerate() {
        if index > plan.last_store {
            continuation.push(instruction);
        } else if let Some(site) = plan.placements.get(&index) {
            match *site {
                CoupledPlacementSite::Decision(level) => decisions[level].push(instruction),
                CoupledPlacementSite::Leaf(leaf) => leaves[leaf].push(instruction),
            }
        } else if !plan.stores.iter().any(|store| store.index == index)
            && !plan.removable_muxes.contains(&index)
        {
            head.push(instruction);
        }
    }
    for (leaf, values) in leaves.iter_mut().zip(&plan.leaf_values) {
        leaf.extend(
            store_templates
                .iter()
                .zip(values)
                .map(|(store, value)| store_with_source(store, *value)),
        );
    }
    let mut conditions = Vec::with_capacity(plan.conditions.len());
    for (level, condition) in plan.conditions.iter().copied().enumerate() {
        let instructions = if level == 0 {
            &mut head
        } else {
            &mut decisions[level]
        };
        conditions.push(normalize_branch_condition(
            &mut eu.register_map,
            instructions,
            condition,
            reg_counter,
        ));
    }

    let depth = conditions.len();
    let decision_ids = std::iter::once(plan.block_id)
        .chain((0..depth.saturating_sub(1)).map(|index| BlockId(first_new_block + index)))
        .collect::<Vec<_>>();
    let leaf_base = first_new_block + depth.saturating_sub(1);
    let leaf_ids = (0..=depth)
        .map(|index| BlockId(leaf_base + index))
        .collect::<Vec<_>>();
    let merge_id = BlockId(leaf_base + depth + 1);

    eu.blocks.insert(
        plan.block_id,
        BasicBlock {
            id: plan.block_id,
            params: original.params,
            instructions: head,
            terminator: SIRTerminator::Branch {
                cond: conditions[0],
                true_block: (leaf_ids[0], Vec::new()),
                false_block: if depth == 1 {
                    (leaf_ids[1], Vec::new())
                } else {
                    (decision_ids[1], Vec::new())
                },
            },
        },
    );
    for level in 1..depth {
        eu.blocks.insert(
            decision_ids[level],
            BasicBlock {
                id: decision_ids[level],
                params: Vec::new(),
                instructions: std::mem::take(&mut decisions[level]),
                terminator: SIRTerminator::Branch {
                    cond: conditions[level],
                    true_block: (leaf_ids[level], Vec::new()),
                    false_block: if level + 1 == depth {
                        (leaf_ids[depth], Vec::new())
                    } else {
                        (decision_ids[level + 1], Vec::new())
                    },
                },
            },
        );
    }
    for (leaf, instructions) in leaves.into_iter().enumerate() {
        eu.blocks.insert(
            leaf_ids[leaf],
            BasicBlock {
                id: leaf_ids[leaf],
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Jump(merge_id, Vec::new()),
            },
        );
    }
    eu.blocks.insert(
        merge_id,
        BasicBlock {
            id: merge_id,
            params: Vec::new(),
            instructions: continuation,
            terminator: original.terminator,
        },
    );
    for value in dead_mux_values {
        eu.register_map.remove(&value);
    }
}
