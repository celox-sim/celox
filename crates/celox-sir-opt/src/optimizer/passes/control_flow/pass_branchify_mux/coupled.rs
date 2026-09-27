//! Coupled state-update and priority-chain branchification.

use super::*;

pub(super) fn branchify_coupled_state_updates(
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

pub(super) fn branchify_coupled_priority_chains(
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
