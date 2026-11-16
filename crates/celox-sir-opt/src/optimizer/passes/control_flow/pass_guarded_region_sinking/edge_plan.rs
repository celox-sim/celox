//! Planning and applying the sinking of a block along one edge.

use super::*;

pub(super) fn plan_block(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    cfg: &SirCfg,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> Option<GuardedRegionPlan> {
    let block = &eu.blocks[&block_id];
    let cost = |plan: &GuardedRegionPlan| {
        plan.moved
            .iter()
            .map(|&index| {
                super::super::cost_model::estimate_clif_cost(
                    &block.instructions[index],
                    &eu.register_map,
                    false,
                ) as u128
            })
            .fold(0u128, u128::saturating_add)
    };
    // Every rewrite inserts one edge block on the executed path.  A lone
    // constant/copy cannot repay that control transfer even though it forms a
    // technically closed region.
    let true_plan =
        plan_block_for_edge(eu, block_id, cfg, uses, true).filter(|plan| cost(plan) > 1);
    let false_plan =
        plan_block_for_edge(eu, block_id, cfg, uses, false).filter(|plan| cost(plan) > 1);
    match (true_plan, false_plan) {
        (Some(true_plan), Some(false_plan)) => {
            if cost(&false_plan) > cost(&true_plan) {
                Some(false_plan)
            } else {
                Some(true_plan)
            }
        }
        (Some(plan), None) | (None, Some(plan)) => Some(plan),
        (None, None) => None,
    }
}

pub(super) fn plan_block_for_edge(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    cfg: &SirCfg,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    selected_true: bool,
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
    let block_index = cfg.block_index(block_id)?;
    let selected_target = if selected_true {
        true_block
    } else {
        false_block
    };
    let selected_index = cfg.block_index(selected_target.0)?;
    if eu.register_map.get(cond).map(RegisterType::width) != Some(1)
        || true_block.0 == false_block.0
        || selected_target.0 == eu.entry_block_id
        || cfg.dominates(selected_target.0, block_id)
        || cfg.predecessors[selected_index].as_slice() != [block_index]
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
        selected_target.0,
        selected_true,
        &local_defs,
        uses,
        cfg,
        &distributed,
        &removable_muxes,
        &safe_to_move,
    );

    let mut seeds = VecDeque::new();
    for store in &distributed {
        let selected_value = if selected_true {
            store.true_value
        } else {
            store.false_value
        };
        if local_defs.contains_key(&selected_value) {
            seeds.push_back(selected_value);
        }
    }
    seeds.extend(
        selected_target
            .1
            .iter()
            .copied()
            .filter(|reg| local_defs.contains_key(reg)),
    );
    for &reg in local_defs.keys() {
        if uses
            .get(&reg)
            .into_iter()
            .flatten()
            .any(|site| site.block() != block_id && cfg.dominates(selected_target.0, site.block()))
        {
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

    // The unselected store value and dynamic store offset must remain available on
    // both edges.  The source mux itself is the only store operand removed.
    for store in &distributed {
        let unselected_value = if selected_true {
            store.false_value
        } else {
            store.true_value
        };
        if local_defs
            .get(&unselected_value)
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
        let dst = def_reg(&block.instructions[index])?;
        if uses.get(&dst).into_iter().flatten().any(|site| {
            !use_is_owned_by_selected_edge(
                *site,
                dst,
                block_id,
                selected_target.0,
                selected_true,
                block,
                &moved,
                &distributed,
                &removable_muxes,
                cfg,
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
        selected_true,
        moved,
        distributed,
        removable_muxes,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compute_moveable_definitions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    condition: RegisterId,
    selected_target: BlockId,
    selected_true: bool,
    local_defs: &HashMap<RegisterId, usize>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    cfg: &SirCfg,
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
            use_can_follow_selected_edge(
                eu,
                *site,
                dst,
                block.id,
                selected_target,
                selected_true,
                local_defs,
                &result,
                distributed,
                removable_muxes,
                cfg,
            )
        });
    }
    result
}

#[allow(clippy::too_many_arguments)]
pub(super) fn use_can_follow_selected_edge(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    site: UseSite,
    value: RegisterId,
    source_block: BlockId,
    selected_target: BlockId,
    selected_true: bool,
    _local_defs: &HashMap<RegisterId, usize>,
    moveable: &[bool],
    distributed: &[DistributedStore],
    removable_muxes: &HashSet<usize>,
    cfg: &SirCfg,
) -> bool {
    match site {
        UseSite::Instruction { block, index } if block == source_block => {
            if removable_muxes.contains(&index) {
                return removable_mux_selected_value(eu, source_block, index, selected_true)
                    == Some(value);
            }
            def_reg(&eu.blocks[&block].instructions[index])
                .is_some_and(|_| moveable.get(index).copied().unwrap_or(false))
                || distributed.iter().any(|store| {
                    let selected_value = if selected_true {
                        store.true_value
                    } else {
                        store.false_value
                    };
                    let unselected_value = if selected_true {
                        store.false_value
                    } else {
                        store.true_value
                    };
                    store.index == index && selected_value == value && unselected_value != value
                })
        }
        UseSite::TrueEdgeArgument { block } if block == source_block => selected_true,
        UseSite::FalseEdgeArgument { block } if block == source_block => !selected_true,
        UseSite::BranchCondition { block } | UseSite::JumpArgument { block }
            if block == source_block =>
        {
            false
        }
        _ => cfg.dominates(selected_target, site.block()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn use_is_owned_by_selected_edge(
    site: UseSite,
    value: RegisterId,
    source_block: BlockId,
    selected_target: BlockId,
    selected_true: bool,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    moved: &HashSet<usize>,
    distributed: &[DistributedStore],
    removable_muxes: &HashSet<usize>,
    cfg: &SirCfg,
) -> bool {
    match site {
        UseSite::Instruction {
            block: use_block,
            index,
        } if use_block == source_block => {
            moved.contains(&index)
                || removable_muxes.contains(&index)
                    && removable_mux_selected_value_in_block(block, index, selected_true)
                        == Some(value)
                || distributed.iter().any(|store| {
                    let selected_value = if selected_true {
                        store.true_value
                    } else {
                        store.false_value
                    };
                    let unselected_value = if selected_true {
                        store.false_value
                    } else {
                        store.true_value
                    };
                    store.index == index && selected_value == value && unselected_value != value
                })
        }
        UseSite::TrueEdgeArgument { block } if block == source_block => selected_true,
        UseSite::FalseEdgeArgument { block } if block == source_block => !selected_true,
        UseSite::BranchCondition { block } | UseSite::JumpArgument { block }
            if block == source_block =>
        {
            false
        }
        _ => cfg.dominates(selected_target, site.block()),
    }
}

pub(super) fn removable_mux_selected_value(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: BlockId,
    index: usize,
    selected_true: bool,
) -> Option<RegisterId> {
    removable_mux_selected_value_in_block(eu.blocks.get(&block)?, index, selected_true)
}

pub(super) fn removable_mux_selected_value_in_block(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    index: usize,
    selected_true: bool,
) -> Option<RegisterId> {
    match block.instructions.get(index)? {
        SIRInstruction::Mux(_, _, true_value, false_value) => Some(if selected_true {
            *true_value
        } else {
            *false_value
        }),
        _ => None,
    }
}

pub(super) fn apply_plan(
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
            if plan.selected_true {
                true_instructions.push(inst);
            } else {
                false_instructions.push(inst);
            }
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

pub(super) fn store_with_source(
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

pub(super) fn store_source_is(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    source: RegisterId,
) -> bool {
    matches!(inst, SIRInstruction::Store(_, _, _, actual, _, _) if *actual == source)
}

pub(super) fn instruction_is_movable(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
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

pub(super) fn instruction_has_effect(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    inst.has_side_effects()
}
