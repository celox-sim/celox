//! Cross-block mux groups and priority-chain planning and rewriting.

use super::*;

pub(super) fn find_cross_block_priority_chain_plans(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    use_counts: &HashMap<RegisterId, usize>,
    multiple: bool,
) -> Option<Vec<CrossBlockPriorityChainPlan>> {
    let cfg = SirDominance::analyze(eu).ok()?;
    let locations = instruction_def_locations(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);
    let mut selection = CrossBlockBatch::default();
    let mut plans = Vec::new();

    for block_id in block_ids {
        if selection.sources.contains(&block_id) {
            continue;
        }
        let block = &eu.blocks[&block_id];
        let suffix_chunks = std::cell::OnceCell::new();
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
                let Some(defs) = collect_cross_condition_defs(
                    eu,
                    &cfg,
                    use_counts,
                    &locations,
                    block_id,
                    first_mux_idx,
                    mux.cond,
                ) else {
                    valid = false;
                    break;
                };
                // Moving only the cross-block prefix of a condition DAG is
                // not a closed rewrite.  If the root (or an intermediate)
                // remains in the Mux block, it still uses that prefix before
                // the newly created decision blocks execute.
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

            let head = &block.instructions[..first_mux_idx];
            let outer_condition_defs = condition_defs.last().expect("chain has an outer condition");
            if moved_defs_insertion_index(head, outer_condition_defs).is_none() {
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
            let chunks_for = |value: RegisterId| {
                eu.register_map
                    .get(&value)
                    .map(|register| register.width().div_ceil(64).max(1))
                    .unwrap_or(1) as u128
            };
            let live_through_cost = suffix_chunks
                .get_or_init(|| mux_live_through_chunks(block, &eu.register_map))
                [muxes.last().expect("chain has a first mux").mux_idx];
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

            let plan = CrossBlockPriorityChainPlan {
                block_id,
                first_mux_idx,
                muxes,
                condition_defs,
                arm_defs,
            };
            if selection.reserve(
                block_id,
                plan.condition_defs
                    .iter()
                    .flatten()
                    .chain(plan.arm_defs.iter().flatten()),
            ) {
                plans.push(plan);
                if !multiple {
                    return Some(plans);
                }
                break;
            }
        }
    }
    (!plans.is_empty()).then_some(plans)
}

pub(super) fn apply_cross_block_priority_chain(
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

// Summarize local single-use DAGs once. A cross-block leaf is relevant only
// when the arm collector could move that leaf itself; loads and shared values
// therefore stop propagation. Dominance is deliberately left to the existing
// planner, making this a conservative rejection filter rather than a new
// movement rule.
pub(super) fn movable_cross_block_inputs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    use_counts: &HashMap<RegisterId, usize>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
) -> HashSet<RegisterId> {
    let mut cross_inputs = HashSet::default();
    for block in eu.blocks.values() {
        for (index, instruction) in block.instructions.iter().enumerate() {
            let Some(root) = def_reg(instruction) else {
                continue;
            };
            if use_counts.get(&root).copied().unwrap_or(0) != 1
                || !is_cross_block_sinkable_input(instruction)
            {
                continue;
            }
            let mut crosses = false;
            visit_instruction_uses(instruction, |operand| {
                let Some(&(source_block, source_index)) = locations.get(&operand) else {
                    return;
                };
                if use_counts.get(&operand).copied().unwrap_or(0) != 1
                    || !is_cross_block_sinkable_input(
                        &eu.blocks[&source_block].instructions[source_index],
                    )
                {
                    return;
                }
                crosses |= source_block != block.id
                    || source_index >= index
                    || cross_inputs.contains(&operand);
            });
            if crosses {
                cross_inputs.insert(root);
            }
        }
    }
    cross_inputs
}

pub(super) fn collect_cross_condition_defs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirDominance,
    use_counts: &HashMap<RegisterId, usize>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mux_block: BlockId,
    mux_idx: usize,
    root: RegisterId,
) -> Option<Vec<LocatedInstruction>> {
    if let Some(&(block, index)) = locations.get(&root)
        && block == mux_block
    {
        // A movable local root necessarily appears in its collected slice,
        // which the closure rule below discards in its entirety. A shared or
        // immovable local root already produces an empty slice. Preserve the
        // original availability rejection without walking either local DAG.
        return (index < mux_idx && cfg.dominates(block, mux_block)).then(Vec::new);
    }
    let mut seen = HashSet::default();
    collect_cross_arm_defs(
        eu, cfg, use_counts, locations, mux_block, mux_idx, root, true, &mut seen,
    )
    .map(|defs| closed_cross_block_condition_slice(defs, mux_block))
}

pub(super) fn find_cross_block_branchify_plans(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    use_counts: &HashMap<RegisterId, usize>,
    multiple: bool,
) -> Option<Vec<CrossBlockBranchifyPlan>> {
    let cfg = SirDominance::analyze(eu).ok()?;
    let def_locations = instruction_def_locations(eu);
    let cross_inputs = movable_cross_block_inputs(eu, use_counts, &def_locations);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);
    let mut selection = CrossBlockBatch::default();
    let mut plans = Vec::new();

    for block_id in block_ids {
        if selection.sources.contains(&block_id) {
            continue;
        }
        let block = &eu.blocks[&block_id];
        let suffix_chunks = std::cell::OnceCell::new();
        for (mux_idx, inst) in block.instructions.iter().enumerate() {
            let SIRInstruction::Mux(dst, cond, true_val, false_val) = inst else {
                continue;
            };
            let could_cross = |root: RegisterId| {
                let Some(&(definition_block, index)) = def_locations.get(&root) else {
                    return false;
                };
                use_counts.get(&root).copied().unwrap_or(0) == 1
                    && is_cross_block_sinkable_input(
                        &eu.blocks[&definition_block].instructions[index],
                    )
                    && (definition_block != block_id || cross_inputs.contains(&root))
            };
            // The later local planner owns these Muxes. Avoid repeatedly
            // cloning their entire single-use DAG merely to rediscover that
            // none of its definitions can move from another block.
            let cross_condition = def_locations
                .get(cond)
                .is_some_and(|&(definition_block, _)| definition_block != block_id)
                && could_cross(*cond);
            if !cross_condition && !could_cross(*true_val) && !could_cross(*false_val) {
                continue;
            }
            let Some(condition_defs) = collect_cross_condition_defs(
                eu,
                &cfg,
                use_counts,
                &def_locations,
                block_id,
                mux_idx,
                *cond,
            ) else {
                continue;
            };
            // Do not sever a cross-block producer from a condition node that
            // remains in the Mux block.  Such a prefix is not independently
            // movable: the local node still executes before the new branch.
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
            let live_through_chunks = suffix_chunks
                .get_or_init(|| mux_live_through_chunks(block, &eu.register_map))[mux_idx];
            if cross_block_branch_is_profitable(eu, &plan, live_through_chunks)
                && selection.reserve(
                    block_id,
                    plan.condition_defs
                        .iter()
                        .chain(&plan.true_defs)
                        .chain(&plan.false_defs),
                )
            {
                plans.push(plan);
                if !multiple {
                    return Some(plans);
                }
                break;
            }
        }
    }
    (!plans.is_empty()).then_some(plans)
}

/// Return the only insertion point that keeps a moved condition DAG in SSA
/// order.  Its external operands must already be defined in the target head,
/// while every use of a moved result must remain after the inserted DAG.
pub(super) fn moved_defs_insertion_index(
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

    let mut external_operands = HashSet::default();
    for definition in moved {
        visit_instruction_uses(&definition.instruction, |operand| {
            if !moved_registers.contains(&operand) {
                external_operands.insert(operand);
            }
        });
    }
    if external_operands.is_empty() {
        return Some(0);
    }

    // Scan the head once instead of restarting a definition search for every
    // operand of a potentially large moved DAG. Removing each found operand
    // also preserves the old first-definition behavior for malformed heads.
    let mut first_use = head.len();
    let mut insertion = 0usize;
    for (index, instruction) in head.iter().enumerate() {
        if first_use == head.len() {
            visit_instruction_uses(instruction, |operand| {
                if moved_registers.contains(&operand) {
                    first_use = index;
                }
            });
        }
        if let Some(defined) = def_reg(instruction)
            && external_operands.remove(&defined)
        {
            insertion = index + 1;
            if insertion > first_use {
                return None;
            }
        }
        if external_operands.is_empty() {
            return Some(insertion);
        }
    }
    (insertion <= first_use).then_some(insertion)
}

pub(super) fn closed_cross_block_condition_slice(
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
pub(super) fn find_cross_block_group_branchify_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> Option<CrossBlockGroupBranchifyPlan> {
    let cfg = SirDominance::analyze(eu).ok()?;
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

        for ((branch_cond, condition_inverted), muxes) in groups {
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

            // The branch uses the resolved predicate, while the collected
            // definitions still follow the original Mux condition. Inverted
            // groups must swap the computations along with their edge values.
            let (true_defs, false_defs) = if condition_inverted {
                (false_defs, true_defs)
            } else {
                (true_defs, false_defs)
            };
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

fn collect_cross_group_defs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirDominance,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mux_block: BlockId,
    first_mux_idx: usize,
    roots: &[RegisterId],
) -> Vec<LocatedInstruction> {
    fn visit(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        cfg: &SirDominance,
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
    cfg: &SirDominance,
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
pub(super) fn collect_cross_arm_defs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirDominance,
    use_counts: &HashMap<RegisterId, usize>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mux_block: BlockId,
    mux_idx: usize,
    root: RegisterId,
    root_required: bool,
    seen: &mut HashSet<(BlockId, usize)>,
) -> Option<Vec<LocatedInstruction>> {
    let mut result = Vec::new();
    collect_cross_arm_defs_into(
        eu,
        cfg,
        use_counts,
        locations,
        mux_block,
        mux_idx,
        root,
        root_required,
        seen,
        &mut result,
    )
    .then_some(result)
}

// Append in dependency order; rebuilding a subtree vector at every recursion
// level repeatedly moves the dependencies of a long single-use chain.
fn collect_cross_arm_defs_into(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirDominance,
    use_counts: &HashMap<RegisterId, usize>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mux_block: BlockId,
    mux_idx: usize,
    root: RegisterId,
    root_required: bool,
    seen: &mut HashSet<(BlockId, usize)>,
    result: &mut Vec<LocatedInstruction>,
) -> bool {
    let Some(&(block_id, index)) = locations.get(&root) else {
        return root_required;
    };
    if block_id == mux_block && index >= mux_idx {
        return false;
    }
    if !cfg.dominates(block_id, mux_block) {
        return false;
    }
    if use_counts.get(&root).copied().unwrap_or(0) != 1 {
        return root_required;
    }
    let instruction = &eu.blocks[&block_id].instructions[index];
    if !is_cross_block_sinkable_input(instruction) {
        return root_required;
    }
    if !seen.insert((block_id, index)) {
        return true;
    }

    visit_instruction_uses(instruction, |operand| {
        let can_attempt_move =
            locations
                .get(&operand)
                .is_some_and(|&(operand_block, operand_idx)| {
                    (operand_block != mux_block || operand_idx < mux_idx)
                        && cfg.dominates(operand_block, mux_block)
                });
        if can_attempt_move && use_counts.get(&operand).copied().unwrap_or(0) == 1 {
            collect_cross_arm_defs_into(
                eu, cfg, use_counts, locations, mux_block, mux_idx, operand, false, seen, result,
            );
        }
    });
    result.push(LocatedInstruction {
        block: block_id,
        index,
        instruction: instruction.clone(),
    });
    true
}

pub(super) fn is_cross_block_sinkable_input(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
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
    live_through_chunks: u128,
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
    let chunks_for = |value: RegisterId| {
        eu.register_map
            .get(&value)
            .map(|register| register.width().div_ceil(64).max(1))
            .unwrap_or(1) as u128
    };
    let phi_copy_cost = chunks_for(plan.dst).saturating_mul(PHI_COPY_COST_PER_CHUNK);
    let live_through_cost = live_through_chunks.saturating_mul(LIVE_THROUGH_COST_PER_CHUNK);
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

pub(super) fn apply_cross_block_group_branchify(
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

pub(super) fn apply_cross_block_branchify(
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
