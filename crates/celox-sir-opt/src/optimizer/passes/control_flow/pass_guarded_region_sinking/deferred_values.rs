//! Sinking deferred value regions and diamonds.

use super::*;

/// Move an acyclic pure SESE decision region to the only block which consumes
/// its merge value.  This is the multi-block form of deferred code motion: a
/// scheduler may have placed normal-path normalization before exception
/// priority checks even though the whole decision region and its phi are only
/// needed by the final normal leaf.
pub(super) fn sink_deferred_value_regions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let Ok(cfg) = SirCfg::analyze_structure(eu) else {
        return;
    };
    let uses = collect_uses(eu);
    let mut plans = Vec::new();
    let mut claimed = HashSet::default();

    for &head_id in &cfg.block_ids {
        let head = &eu.blocks[&head_id];
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &head.terminator
        else {
            continue;
        };
        if !true_block.1.is_empty() || !false_block.1.is_empty() || true_block.0 == false_block.0 {
            continue;
        }
        let Some(merge_id) = cfg.common_postdominator(true_block.0, false_block.0) else {
            continue;
        };
        let Some(merge) = eu.blocks.get(&merge_id) else {
            continue;
        };
        if merge.params.is_empty() || merge_id == head_id {
            continue;
        }

        let mut region = HashSet::default();
        let mut work = vec![true_block.0, false_block.0];
        let mut valid = true;
        while let Some(block_id) = work.pop() {
            if block_id == merge_id || !region.insert(block_id) {
                continue;
            }
            let Some(index) = cfg.block_index(block_id) else {
                valid = false;
                break;
            };
            let block = &eu.blocks[&block_id];
            if cfg.sccs[cfg.scc_for_block[index]].cyclic
                || block
                    .instructions
                    .iter()
                    .any(|instruction| !instruction_is_movable(instruction))
            {
                valid = false;
                break;
            }
            match &block.terminator {
                SIRTerminator::Jump(target, _) => work.push(*target),
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => {
                    work.push(true_block.0);
                    work.push(false_block.0);
                }
                SIRTerminator::Switch { cases, default, .. } => {
                    work.extend(cases.iter().map(|case| case.target));
                    work.push(*default);
                }
                SIRTerminator::Return | SIRTerminator::Error(_) => {
                    valid = false;
                    break;
                }
            }
        }
        if !valid || region.is_empty() || region.contains(&head_id) {
            continue;
        }
        let merge_index = cfg.block_index(merge_id).unwrap();
        if cfg.predecessors[merge_index]
            .iter()
            .any(|&predecessor| !region.contains(&cfg.block_ids[predecessor]))
        {
            continue;
        }
        for &block_id in &region {
            let index = cfg.block_index(block_id).unwrap();
            if cfg.predecessors[index].iter().any(|&predecessor| {
                let predecessor = cfg.block_ids[predecessor];
                predecessor != head_id && !region.contains(&predecessor)
            }) || eu.blocks[&block_id].instructions.iter().any(|instruction| {
                def_reg(instruction).is_some_and(|definition| {
                    uses.get(&definition)
                        .into_iter()
                        .flatten()
                        .any(|site| !region.contains(&site.block()))
                })
            }) {
                valid = false;
                break;
            }
            match &eu.blocks[&block_id].terminator {
                SIRTerminator::Jump(target, arguments) if *target == merge_id => {
                    valid &= arguments.len() == merge.params.len();
                }
                SIRTerminator::Jump(target, _) => valid &= region.contains(target),
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => {
                    valid &= region.contains(&true_block.0) && region.contains(&false_block.0);
                }
                SIRTerminator::Switch { cases, default, .. } => {
                    valid &= cases.iter().all(|case| region.contains(&case.target))
                        && region.contains(default);
                }
                SIRTerminator::Return | SIRTerminator::Error(_) => valid = false,
            }
        }
        if !valid {
            continue;
        }

        let mut use_block = None;
        for &param in &merge.params {
            let Some(param_uses) = uses.get(&param) else {
                valid = false;
                break;
            };
            for site in param_uses {
                let block = site.block();
                if [head_id, merge_id].contains(&block) || region.contains(&block) {
                    valid = false;
                    break;
                }
                if use_block.is_some_and(|expected| expected != block) {
                    valid = false;
                    break;
                }
                use_block = Some(block);
            }
        }
        let Some(use_block) = use_block.filter(|_| valid) else {
            continue;
        };
        if !cfg.dominates(merge_id, use_block)
            || cfg.postdominates(use_block, merge_id)
            || claimed.contains(&head_id)
            || claimed.contains(&merge_id)
            || claimed.contains(&use_block)
            || region.iter().any(|block| claimed.contains(block))
        {
            continue;
        }

        let mut moved_head = HashSet::default();
        for index in (0..head.instructions.len()).rev() {
            let Some(dst) = def_reg(&head.instructions[index]) else {
                continue;
            };
            if !instruction_is_movable(&head.instructions[index]) {
                continue;
            }
            let owned = uses
                .get(&dst)
                .into_iter()
                .flatten()
                .all(|site| match *site {
                    UseSite::Instruction {
                        block,
                        index: use_index,
                    } if block == head_id => moved_head.contains(&use_index),
                    UseSite::BranchCondition { block } if block == head_id => true,
                    _ => region.contains(&site.block()) || site.block() == use_block,
                });
            if owned {
                moved_head.insert(index);
            }
        }
        if moved_head.is_empty() {
            continue;
        }

        claimed.insert(head_id);
        claimed.insert(merge_id);
        claimed.insert(use_block);
        claimed.extend(region.iter().copied());
        plans.push(DeferredValueRegionPlan {
            head: head_id,
            merge: merge_id,
            region,
            use_block,
            moved_head,
        });
    }

    let max_block = eu.blocks.keys().map(|block| block.0).max().unwrap_or(0);
    let Some(first_new_block) = max_block.checked_add(1) else {
        return;
    };
    let Some(last_new_block) = max_block.checked_add(plans.len()) else {
        return;
    };
    if last_new_block > u32::MAX as usize {
        return;
    }
    for (ordinal, plan) in plans.into_iter().enumerate() {
        apply_deferred_value_region(eu, plan, BlockId(first_new_block + ordinal));
    }
}

pub(super) fn apply_deferred_value_region(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: DeferredValueRegionPlan,
    continuation_id: BlockId,
) {
    let original_use = eu
        .blocks
        .remove(&plan.use_block)
        .expect("planned deferred region use must exist");
    let head = eu
        .blocks
        .get_mut(&plan.head)
        .expect("planned deferred region head must exist");
    let original_branch = std::mem::replace(
        &mut head.terminator,
        SIRTerminator::Jump(plan.merge, Vec::new()),
    );
    let mut moved = Vec::new();
    let mut retained = Vec::new();
    for (index, instruction) in std::mem::take(&mut head.instructions)
        .into_iter()
        .enumerate()
    {
        if plan.moved_head.contains(&index) {
            moved.push(instruction);
        } else {
            retained.push(instruction);
        }
    }
    head.instructions = retained;

    let params = std::mem::take(
        &mut eu
            .blocks
            .get_mut(&plan.merge)
            .expect("planned deferred region merge must exist")
            .params,
    );
    for &block_id in &plan.region {
        let block = eu.blocks.get_mut(&block_id).unwrap();
        if let SIRTerminator::Jump(target, _) = &mut block.terminator
            && *target == plan.merge
        {
            *target = continuation_id;
        }
    }
    eu.blocks.insert(
        plan.use_block,
        BasicBlock {
            id: plan.use_block,
            params: original_use.params,
            instructions: moved,
            terminator: original_branch,
        },
    );
    eu.blocks.insert(
        continuation_id,
        BasicBlock {
            id: continuation_id,
            params,
            instructions: original_use.instructions,
            terminator: original_use.terminator,
        },
    );
}

/// Distribute a merge block's leading Stores when their operands are block
/// parameters.  Every accepted predecessor is an unconditional incoming edge,
/// so each dynamic execution still performs each Store exactly once and in the
/// same order.  Backedges are excluded: this normalization is for acyclic CFG
/// joins, not loop-header effect motion.
pub(super) fn distribute_prefix_phi_stores(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    #[derive(Clone)]
    struct Plan {
        merge: BlockId,
        count: usize,
        incoming: Vec<(BlockId, Vec<RegisterId>)>,
    }

    let Ok(cfg) = SirCfg::analyze_forward_structure(eu) else {
        return false;
    };
    let mut plans = Vec::new();
    for &merge_id in &cfg.block_ids {
        let merge = &eu.blocks[&merge_id];
        if merge.params.is_empty() {
            continue;
        }
        let params = merge.params.iter().copied().collect::<HashSet<_>>();
        let count = merge
            .instructions
            .iter()
            .take_while(|inst| {
                matches!(inst, SIRInstruction::Store(_, _, _, source, _, _)
                    if params.contains(source))
            })
            .count();
        if count == 0 {
            continue;
        }

        let Some(merge_index) = cfg.block_index(merge_id) else {
            continue;
        };
        let mut incoming = Vec::new();
        let mut valid = cfg.predecessors[merge_index].len() >= 2;
        for &pred_index in &cfg.predecessors[merge_index] {
            let pred_id = cfg.block_ids[pred_index];
            if cfg.dominates(merge_id, pred_id) {
                valid = false;
                break;
            }
            match &eu.blocks[&pred_id].terminator {
                SIRTerminator::Jump(target, args)
                    if *target == merge_id && args.len() == merge.params.len() =>
                {
                    incoming.push((pred_id, args.clone()));
                }
                _ => {
                    valid = false;
                    break;
                }
            }
        }
        if valid {
            plans.push(Plan {
                merge: merge_id,
                count,
                incoming,
            });
        }
    }

    for plan in &plans {
        let merge = &eu.blocks[&plan.merge];
        let params = merge.params.clone();
        let stores = merge.instructions[..plan.count].to_vec();
        for (pred_id, args) in &plan.incoming {
            let mut edge_stores = stores.clone();
            for (&param, &argument) in params.iter().zip(args) {
                for store in &mut edge_stores {
                    replace_register_uses_in_instruction(store, param, argument);
                }
            }
            eu.blocks
                .get_mut(pred_id)
                .expect("planned predecessor must still exist")
                .instructions
                .extend(edge_stores);
        }
        eu.blocks
            .get_mut(&plan.merge)
            .expect("planned merge must still exist")
            .instructions
            .drain(..plan.count);
    }
    let uses = collect_uses(eu);
    for plan in &plans {
        let dead = eu.blocks[&plan.merge]
            .params
            .iter()
            .enumerate()
            .filter_map(|(index, param)| (!uses.contains_key(param)).then_some(index))
            .collect::<Vec<_>>();
        for &index in dead.iter().rev() {
            eu.blocks
                .get_mut(&plan.merge)
                .expect("planned merge must still exist")
                .params
                .remove(index);
            for (pred_id, _) in &plan.incoming {
                let SIRTerminator::Jump(target, arguments) =
                    &mut eu.blocks.get_mut(pred_id).unwrap().terminator
                else {
                    unreachable!("planned incoming edge must remain a Jump")
                };
                debug_assert_eq!(*target, plan.merge);
                arguments.remove(index);
            }
        }
    }
    !plans.is_empty()
}

/// Merge one disjoint layer of `pred -> successor` pairs where successor has
/// exactly one predecessor.  One layer is intentional: the edge blocks just
/// introduced by this pass are the desired predecessors, and a global fixed
/// point would needlessly reshape unrelated CFG.
pub(super) fn merge_single_predecessor_jump_blocks(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let Ok(cfg) = SirCfg::analyze_forward_structure(eu) else {
        return;
    };
    let mut claimed = HashSet::default();
    let mut pairs = Vec::new();
    for &pred_id in &cfg.block_ids {
        let SIRTerminator::Jump(successor, arguments) = &eu.blocks[&pred_id].terminator else {
            continue;
        };
        let Some(successor_index) = cfg.block_index(*successor) else {
            continue;
        };
        if *successor == eu.entry_block_id
            || cfg.predecessors[successor_index].as_slice()
                != [cfg.block_index(pred_id).expect("CFG contains predecessor")]
            || arguments.len() != eu.blocks[successor].params.len()
            || claimed.contains(&pred_id)
            || claimed.contains(successor)
        {
            continue;
        }
        claimed.insert(pred_id);
        claimed.insert(*successor);
        pairs.push((pred_id, *successor));
    }

    for (pred_id, successor_id) in pairs {
        // Read the arguments now: an earlier merge may have replaced a
        // parameter that this edge passes on.
        let SIRTerminator::Jump(_, arguments) = &eu.blocks[&pred_id].terminator else {
            unreachable!("planned predecessor still ends in its jump");
        };
        let arguments = arguments.clone();
        let mut successor = eu
            .blocks
            .remove(&successor_id)
            .expect("planned successor must still exist");
        for (&param, &argument) in successor.params.iter().zip(&arguments) {
            for instruction in &mut successor.instructions {
                replace_register_uses_in_instruction(instruction, param, argument);
            }
            replace_register_uses_in_terminator(&mut successor.terminator, param, argument);
            replace_register_uses(eu, param, argument);
        }
        let pred = eu
            .blocks
            .get_mut(&pred_id)
            .expect("planned predecessor must still exist");
        pred.instructions.extend(successor.instructions);
        pred.terminator = successor.terminator;
    }
}

pub(super) fn replace_register_uses(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    old: RegisterId,
    new: RegisterId,
) {
    for block in eu.blocks.values_mut() {
        for instruction in &mut block.instructions {
            replace_register_uses_in_instruction(instruction, old, new);
        }
        replace_register_uses_in_terminator(&mut block.terminator, old, new);
    }
}

pub(super) fn replace_register_uses_in_instruction(
    instruction: &mut SIRInstruction<RegionedAbsoluteAddr>,
    old: RegisterId,
    new: RegisterId,
) {
    instruction.for_each_use_mut(|register| {
        if *register == old {
            *register = new;
        }
    });
}

pub(super) fn replace_register_uses_in_terminator(
    terminator: &mut SIRTerminator,
    old: RegisterId,
    new: RegisterId,
) {
    let replace = |register: &mut RegisterId| {
        if *register == old {
            *register = new;
        }
    };
    match terminator {
        SIRTerminator::Jump(_, args) => {
            for arg in args {
                replace(arg);
            }
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            replace(cond);
            for arg in true_block.1.iter_mut().chain(&mut false_block.1) {
                replace(arg);
            }
        }
        SIRTerminator::Switch { selector, .. } => replace(selector),
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

pub(super) fn sink_deferred_value_diamonds(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let Ok(cfg) = SirCfg::analyze_structure(eu) else {
        return;
    };
    let uses = collect_uses(eu);
    let mut plans = Vec::<DeferredValueDiamondPlan>::new();
    let mut removed_arms = HashSet::<BlockId>::default();
    let mut selected_use_blocks = HashSet::<BlockId>::default();
    let mut selected_boundaries = HashSet::<BlockId>::default();

    for &head_id in &cfg.block_ids {
        let head = &eu.blocks[&head_id];
        let SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &head.terminator
        else {
            continue;
        };
        if !true_block.1.is_empty() || !false_block.1.is_empty() || true_block.0 == false_block.0 {
            continue;
        }
        let (true_id, false_id) = (true_block.0, false_block.0);
        let (Some(true_arm), Some(false_arm)) = (eu.blocks.get(&true_id), eu.blocks.get(&false_id))
        else {
            continue;
        };
        if !true_arm.params.is_empty()
            || !false_arm.params.is_empty()
            || true_arm
                .instructions
                .iter()
                .any(|inst| !instruction_is_movable(inst))
            || false_arm
                .instructions
                .iter()
                .any(|inst| !instruction_is_movable(inst))
        {
            continue;
        }
        let (
            SIRTerminator::Jump(true_merge, true_args),
            SIRTerminator::Jump(false_merge, false_args),
        ) = (&true_arm.terminator, &false_arm.terminator)
        else {
            continue;
        };
        if true_merge != false_merge || true_args.len() != 1 || false_args.len() != 1 {
            continue;
        }
        let merge_id = *true_merge;
        let Some(merge) = eu.blocks.get(&merge_id) else {
            continue;
        };
        if merge.params.len() != 1
            || !has_exact_predecessors(&cfg, true_id, &[head_id])
            || !has_exact_predecessors(&cfg, false_id, &[head_id])
            || !has_exact_predecessors(&cfg, merge_id, &[true_id, false_id])
        {
            continue;
        }

        let result = merge.params[0];
        let Some(result_uses) = uses.get(&result) else {
            continue;
        };
        let Some(use_block) = result_uses.first().map(|site| site.block()) else {
            continue;
        };
        if result_uses.iter().any(|site| site.block() != use_block)
            || [head_id, true_id, false_id, merge_id].contains(&use_block)
        {
            continue;
        }
        let Some(use_body) = eu.blocks.get(&use_block) else {
            continue;
        };
        if !use_body.params.is_empty()
            || use_body.instructions.iter().any(instruction_has_effect)
            || !cfg.dominates(merge_id, use_block)
            // A postdominating use executes whenever the original diamond
            // does, so moving the branch would save no dynamic work.
            || cfg.postdominates(use_block, merge_id)
        {
            continue;
        }
        let (Some(head_index), Some(merge_index), Some(use_index)) = (
            cfg.block_index(head_id),
            cfg.block_index(merge_id),
            cfg.block_index(use_block),
        ) else {
            continue;
        };
        if cfg.sccs[cfg.scc_for_block[head_index]].cyclic
            || cfg.sccs[cfg.scc_for_block[merge_index]].cyclic
            || cfg.sccs[cfg.scc_for_block[use_index]].cyclic
            || removed_arms.contains(&true_id)
            || removed_arms.contains(&false_id)
            || removed_arms.contains(&use_block)
            || selected_boundaries.contains(&use_block)
            || selected_use_blocks.contains(&use_block)
            || selected_use_blocks.contains(&true_id)
            || selected_use_blocks.contains(&false_id)
            || selected_use_blocks.contains(&head_id)
            || selected_use_blocks.contains(&merge_id)
        {
            continue;
        }

        // Serial diamonds may share a boundary (`previous.merge ==
        // next.head`), but their arm blocks and selected leaves are disjoint.
        // This admits DivS/DivU/RemS/RemU sequences in one linear plan.
        removed_arms.insert(true_id);
        removed_arms.insert(false_id);
        selected_use_blocks.insert(use_block);
        selected_boundaries.insert(head_id);
        selected_boundaries.insert(merge_id);
        plans.push(DeferredValueDiamondPlan {
            head: head_id,
            true_arm: true_id,
            false_arm: false_id,
            merge: merge_id,
            condition: *cond,
            result,
            true_value: true_args[0],
            false_value: false_args[0],
            use_block,
        });
    }

    if plans.is_empty() {
        return;
    }
    let Some(additional_blocks) = plans.len().checked_mul(3) else {
        return;
    };
    let max_block = eu.blocks.keys().map(|id| id.0).max().unwrap_or(0);
    let Some(first_new_block) = max_block.checked_add(1) else {
        return;
    };
    let Some(end_sentinel) = first_new_block.checked_add(additional_blocks) else {
        return;
    };
    if end_sentinel > u32::MAX as usize {
        return;
    }

    for (ordinal, plan) in plans.into_iter().enumerate() {
        let true_id = BlockId(first_new_block + ordinal * 3);
        let false_id = BlockId(first_new_block + ordinal * 3 + 1);
        let continuation_id = BlockId(first_new_block + ordinal * 3 + 2);
        apply_deferred_value_diamond(eu, plan, true_id, false_id, continuation_id);
    }
    debug_assert_eq!(eu.verify_result(), Ok(()));
}

pub(super) fn has_exact_predecessors(cfg: &SirCfg, block: BlockId, expected: &[BlockId]) -> bool {
    let Some(block) = cfg.block_index(block) else {
        return false;
    };
    let mut actual = cfg.predecessors[block]
        .iter()
        .map(|&predecessor| cfg.block_ids[predecessor])
        .collect::<Vec<_>>();
    let mut expected = expected.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    actual == expected
}

pub(super) fn apply_deferred_value_diamond(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: DeferredValueDiamondPlan,
    true_id: BlockId,
    false_id: BlockId,
    continuation_id: BlockId,
) {
    let true_arm = eu
        .blocks
        .remove(&plan.true_arm)
        .expect("planned deferred true arm must exist");
    let false_arm = eu
        .blocks
        .remove(&plan.false_arm)
        .expect("planned deferred false arm must exist");
    let original_use = eu
        .blocks
        .remove(&plan.use_block)
        .expect("planned deferred use block must exist");

    eu.blocks
        .get_mut(&plan.head)
        .expect("planned deferred head must exist")
        .terminator = SIRTerminator::Jump(plan.merge, Vec::new());
    eu.blocks
        .get_mut(&plan.merge)
        .expect("planned deferred merge must exist")
        .params
        .clear();

    eu.blocks.insert(
        plan.use_block,
        BasicBlock {
            id: plan.use_block,
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Branch {
                cond: plan.condition,
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
            instructions: true_arm.instructions,
            terminator: SIRTerminator::Jump(continuation_id, vec![plan.true_value]),
        },
    );
    eu.blocks.insert(
        false_id,
        BasicBlock {
            id: false_id,
            params: Vec::new(),
            instructions: false_arm.instructions,
            terminator: SIRTerminator::Jump(continuation_id, vec![plan.false_value]),
        },
    );
    eu.blocks.insert(
        continuation_id,
        BasicBlock {
            id: continuation_id,
            params: vec![plan.result],
            instructions: original_use.instructions,
            terminator: original_use.terminator,
        },
    );
}
