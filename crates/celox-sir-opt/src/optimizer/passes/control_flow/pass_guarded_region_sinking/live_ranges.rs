//! Splitting branch live ranges, sinking pure values and repairing predicated live-outs.

use super::*;

#[derive(Clone)]
pub(super) struct BranchLiveRangeSplitPlan {
    pub(super) source: BlockId,
    pub(super) arms: [BlockId; 2],
    pub(super) placements: Vec<u8>,
}

/// Split a pure cone which is live from a branch head into both exclusive
/// successor regions.
///
/// Both successors must have the head as their sole predecessor. Therefore an
/// edge-local copy executes exactly once whenever that edge is taken, even if
/// the branch is inside a loop. Loads and effects stay in the head and form
/// the materialization frontier.
pub(super) fn split_branch_live_ranges(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let Ok(cfg) = SirCfg::analyze_forward_structure(eu) else {
        return;
    };
    let uses = collect_uses(eu);
    let mut plans = Vec::new();

    for &source_id in &cfg.block_ids {
        let source = &eu.blocks[&source_id];
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &source.terminator
        else {
            continue;
        };
        let arms = [true_block.0, false_block.0];
        let (Some(source_index), Some(true_index), Some(false_index)) = (
            cfg.block_index(source_id),
            cfg.block_index(arms[0]),
            cfg.block_index(arms[1]),
        ) else {
            continue;
        };
        if arms[0] == arms[1]
            || cfg.predecessors[true_index].as_slice() != [source_index]
            || cfg.predecessors[false_index].as_slice() != [source_index]
        {
            continue;
        }

        // Bit 0 is the true edge and bit 1 is the false edge. Reverse order
        // lets a producer inherit the exact edge set of a local pure user.
        let mut placements = vec![0u8; source.instructions.len()];
        for (index, instruction) in source.instructions.iter().enumerate().rev() {
            if !instruction_is_movable(instruction) {
                continue;
            }
            let Some(definition) = def_reg(instruction) else {
                continue;
            };
            let Some(value_uses) = uses.get(&definition).filter(|uses| !uses.is_empty()) else {
                continue;
            };
            let mut edges = 0u8;
            let mut legal = true;
            for site in value_uses {
                let use_edges = match *site {
                    UseSite::Instruction { block, index: user } if block == source_id => {
                        placements.get(user).copied().unwrap_or(0)
                    }
                    _ if site.block() == source_id => 0,
                    _ => {
                        let block = site.block();
                        match (cfg.dominates(arms[0], block), cfg.dominates(arms[1], block)) {
                            (true, false) => 1,
                            (false, true) => 2,
                            _ => 0,
                        }
                    }
                };
                if use_edges == 0 {
                    legal = false;
                    break;
                }
                edges |= use_edges;
            }
            if legal {
                placements[index] = edges;
            }
        }

        // One-edge values are already handled by ordinary sinking. This pass
        // only applies when it removes a genuinely shared branch live range.
        if placements.contains(&3) {
            plans.push(BranchLiveRangeSplitPlan {
                source: source_id,
                arms,
                placements,
            });
        }
    }

    if plans.is_empty() {
        return;
    }
    // Inner branches first. An outer split may rewrite their operands, but it
    // cannot invalidate their source instruction indices.
    plans.sort_unstable_by_key(|plan| {
        std::cmp::Reverse(dominator_depth(&cfg, cfg.block_index(plan.source).unwrap()))
    });

    let mut reg_counter = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
    let Some(additional_registers) = plans.iter().try_fold(0usize, |total, plan| {
        plan.placements.iter().try_fold(total, |total, placement| {
            total.checked_add(placement.count_ones() as usize)
        })
    }) else {
        return;
    };
    if reg_counter.checked_add(additional_registers).is_none() {
        return;
    }

    for plan in plans {
        let original = eu.blocks[&plan.source].instructions.clone();
        let mut replacements = [HashMap::default(), HashMap::default()];
        let mut cloned = [Vec::new(), Vec::new()];

        for (index, instruction) in original.iter().enumerate() {
            let placement = plan.placements[index];
            if placement == 0 {
                continue;
            }
            let old = def_reg(instruction).expect("split plan contains only value definitions");
            for arm in 0..2 {
                if placement & (1 << arm) == 0 {
                    continue;
                }
                reg_counter += 1;
                let new = RegisterId(reg_counter);
                eu.register_map.insert(new, eu.register_map[&old].clone());
                cloned[arm].push(
                    clone_pure_instruction(instruction, new, &replacements[arm])
                        .expect("split plan contains only pure instructions"),
                );
                replacements[arm].insert(old, new);
            }
        }

        for arm in 0..2 {
            for &block_id in &cfg.block_ids {
                if !cfg.dominates(plan.arms[arm], block_id) {
                    continue;
                }
                let block = eu.blocks.get_mut(&block_id).unwrap();
                for (&old, &new) in &replacements[arm] {
                    for instruction in &mut block.instructions {
                        replace_register_uses_in_instruction(instruction, old, new);
                    }
                    replace_register_uses_in_terminator(&mut block.terminator, old, new);
                }
            }
            cloned[arm].append(&mut eu.blocks.get_mut(&plan.arms[arm]).unwrap().instructions);
            eu.blocks.get_mut(&plan.arms[arm]).unwrap().instructions =
                std::mem::take(&mut cloned[arm]);
        }

        let source = eu.blocks.get_mut(&plan.source).unwrap();
        source.instructions = std::mem::take(&mut source.instructions)
            .into_iter()
            .enumerate()
            .filter_map(|(index, instruction)| (plan.placements[index] == 0).then_some(instruction))
            .collect();
        for (index, instruction) in original.iter().enumerate() {
            if plan.placements[index] != 0
                && let Some(old) = def_reg(instruction)
            {
                eu.register_map.remove(&old);
            }
        }
    }

    debug_assert_eq!(eu.verify_result(), Ok(()));
}

/// Sink pure SSA definitions to the nearest common dominator of all uses.
///
/// Definitions are considered in reverse instruction order, so a producer can
/// follow a consumer which was itself sunk out of the source block.  The
/// destination must remain in an acyclic region: sinking a loop-invariant
/// value into a loop would trade one evaluation for one per iteration.
pub(super) fn sink_pure_values_to_use_dominators(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let Ok(cfg) = SirCfg::analyze_forward_structure(eu) else {
        return;
    };
    let uses = collect_uses(eu);
    let mut placements = HashMap::<(BlockId, usize), BlockId>::default();

    for &source_id in &cfg.block_ids {
        let source = &eu.blocks[&source_id];
        for (index, instruction) in source.instructions.iter().enumerate().rev() {
            if !instruction_is_movable(instruction) {
                continue;
            }
            let Some(definition) = def_reg(instruction) else {
                continue;
            };
            let Some(definition_uses) = uses.get(&definition).filter(|uses| !uses.is_empty())
            else {
                continue;
            };

            let mut destination = None;
            for site in definition_uses {
                let use_block = match *site {
                    UseSite::Instruction {
                        block,
                        index: use_index,
                    } if block == source_id => placements
                        .get(&(source_id, use_index))
                        .copied()
                        .unwrap_or(source_id),
                    _ => site.block(),
                };
                let Some(use_index) = cfg.block_index(use_block) else {
                    destination = None;
                    break;
                };
                destination = Some(match destination {
                    None => use_index,
                    Some(current) => cfg.dominators.lca(current, use_index).unwrap_or(0),
                });
                // Once a retained use pins the value to its definition block,
                // no later use can make a deeper placement legal.
                if destination.is_some_and(|index| cfg.block_ids[index] == source_id) {
                    break;
                }
            }

            let Some(destination) = destination.map(|index| cfg.block_ids[index]) else {
                continue;
            };
            let destination_index = cfg.block_index(destination).unwrap();
            if destination != source_id
                && cfg.dominates(source_id, destination)
                && !cfg.sccs[cfg.scc_for_block[destination_index]].cyclic
            {
                placements.insert((source_id, index), destination);
            }
        }
    }

    if placements.is_empty() {
        return;
    }

    // Source blocks are in RPO, hence definitions from an outer dominator are
    // prepended before definitions from an inner dominator when both arrive at
    // the same destination.
    let mut incoming = HashMap::<BlockId, Vec<SIRInstruction<RegionedAbsoluteAddr>>>::default();
    for &source_id in &cfg.block_ids {
        let source = eu.blocks.get_mut(&source_id).unwrap();
        let mut retained = Vec::with_capacity(source.instructions.len());
        for (index, instruction) in std::mem::take(&mut source.instructions)
            .into_iter()
            .enumerate()
        {
            if let Some(&destination) = placements.get(&(source_id, index)) {
                incoming.entry(destination).or_default().push(instruction);
            } else {
                retained.push(instruction);
            }
        }
        source.instructions = retained;
    }
    for (destination, mut instructions) in incoming {
        let block = eu.blocks.get_mut(&destination).unwrap();
        instructions.append(&mut block.instructions);
        block.instructions = instructions;
    }
}

pub(in crate::optimizer) fn sink_pure_values_with_predicate_repair(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) {
    sink_pure_values_to_use_dominators(eu);
    loop {
        let pruned = prune_control_dead_phi_operands(eu);
        let repaired = repair_predicated_live_outs(eu);
        if !pruned && !repaired {
            break;
        }
        sink_pure_values_to_use_dominators(eu);
    }
}

#[derive(Clone)]
pub(super) struct PredicatedLiveOutPlan {
    pub(super) value: RegisterId,
    pub(super) source: BlockId,
    pub(super) candidate: BlockId,
    pub(super) merge: BlockId,
    pub(super) facts: Vec<(RegisterId, bool)>,
}

/// Repair SSA for a value which is available under the same predicate facts
/// in two separate priority regions.  A plain dominator LCA places such a
/// value before both regions.  Passing it through the first region's merge
/// makes its conditional availability explicit, after which ordinary sinking
/// can place the complete producer DAG below the shared guards.
pub(super) fn repair_predicated_live_outs(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    let Ok(cfg) = SirCfg::analyze_structure(eu) else {
        return false;
    };
    let uses = collect_uses(eu);
    let facts = predicate_facts(eu, &cfg);
    // Eligibility of an ancestor's merge is independent of the value being
    // repaired. Index it once instead of walking long straight-line dominator
    // chains for every definition/use pair and rejecting the same blocks.
    let mut repair_merges = vec![None; cfg.block_ids.len()];
    for (candidate, merge_slot) in repair_merges.iter_mut().enumerate() {
        if facts[candidate].is_empty() || cfg.sccs[cfg.scc_for_block[candidate]].cyclic {
            continue;
        }
        let Some(merge) = cfg.immediate_postdominator(candidate) else {
            continue;
        };
        if !cfg.dominators.dominates(candidate, merge)
            && !cfg.sccs[cfg.scc_for_block[merge]].cyclic
            && merge_accepts_phi(&cfg, eu, cfg.block_ids[merge])
        {
            *merge_slot = Some(merge);
        }
    }
    let mut nearest_candidate = vec![None; cfg.block_ids.len()];
    let mut pending = vec![(0usize, None)];
    while let Some((block, ancestor)) = pending.pop() {
        let nearest = if repair_merges[block].is_some() {
            Some(block)
        } else {
            ancestor
        };
        nearest_candidate[block] = nearest;
        pending.extend(
            cfg.dominators.children[block]
                .iter()
                .map(|&child| (child, nearest)),
        );
    }
    let mut definitions = HashMap::default();
    for &block_id in &cfg.block_ids {
        for instruction in &eu.blocks[&block_id].instructions {
            if instruction_is_movable(instruction)
                && let Some(value) = def_reg(instruction)
            {
                definitions.insert(value, block_id);
            }
        }
    }

    let mut plans = Vec::new();
    let mut claimed = HashSet::default();
    for (&value, &source) in &definitions {
        let Some(value_uses) = uses.get(&value).filter(|uses| uses.len() >= 2) else {
            continue;
        };
        let mut use_blocks = value_uses
            .iter()
            .map(|site| site.block())
            .collect::<Vec<_>>();
        use_blocks.sort_unstable();
        use_blocks.dedup();
        if use_blocks.len() < 2 {
            continue;
        }
        let mut common_facts = facts[cfg.block_index(use_blocks[0]).unwrap()].clone();
        common_facts.retain(|fact| {
            use_blocks
                .iter()
                .skip(1)
                .all(|block| facts[cfg.block_index(*block).unwrap()].contains(fact))
        });
        if common_facts.is_empty() {
            continue;
        }

        let mut candidates = HashSet::default();
        for &use_block in &use_blocks {
            let mut next = nearest_candidate[cfg.block_index(use_block).unwrap()];
            while let Some(block) = next {
                if cfg.block_ids[block] == source || !cfg.dominates(source, cfg.block_ids[block]) {
                    break;
                }
                // All uses share the same definition. A previously visited
                // ancestor already contributed its entire path to that source.
                if !candidates.insert(block) {
                    break;
                }
                next = cfg.dominators.idom[block].and_then(|parent| nearest_candidate[parent]);
            }
        }
        let mut best = None;
        for candidate_index in candidates {
            let candidate = cfg.block_ids[candidate_index];
            let candidate_facts = &facts[candidate_index];
            if candidate_facts.is_empty()
                || candidate_facts
                    .iter()
                    .any(|fact| !common_facts.contains(fact) || fact.0 == value)
                || !cfg.dominates(source, candidate)
                || cfg.sccs[cfg.scc_for_block[candidate_index]].cyclic
            {
                continue;
            }
            let dominated_uses = use_blocks
                .iter()
                .filter(|&&block| cfg.dominates(candidate, block))
                .count();
            if dominated_uses == 0 || dominated_uses == use_blocks.len() {
                continue;
            }
            let merge_index = repair_merges[candidate_index]
                .expect("indexed repair candidates have an admissible merge");
            let merge = cfg.block_ids[merge_index];
            // Predicate facts describe necessary branch outcomes, not the
            // complete path to the candidate (in particular, Switch arms are
            // not represented). A dummy phi operand is safe only if every
            // edge bypassing the candidate contradicts its required facts.
            if cfg.predecessors[merge_index].iter().any(|&predecessor| {
                if cfg.dominators.dominates(candidate_index, predecessor) {
                    return false;
                }
                let contradicts = |edge_fact: Option<(RegisterId, bool)>| {
                    candidate_facts.iter().any(|&(condition, required)| {
                        facts[predecessor].contains(&(condition, !required))
                            || edge_fact == Some((condition, !required))
                    })
                };
                match &eu.blocks[&cfg.block_ids[predecessor]].terminator {
                    SIRTerminator::Jump(..) => !contradicts(None),
                    SIRTerminator::Branch {
                        cond,
                        true_block,
                        false_block,
                    } => {
                        (true_block.0 == merge && !contradicts(Some((*cond, true))))
                            || (false_block.0 == merge && !contradicts(Some((*cond, false))))
                    }
                    _ => true,
                }
            }) {
                continue;
            }
            if use_blocks.iter().any(|&block| {
                !cfg.dominates(candidate, block)
                    && (!cfg.dominates(merge, block)
                        || candidate_facts
                            .iter()
                            .any(|fact| !facts[cfg.block_index(block).unwrap()].contains(fact)))
            }) {
                continue;
            }
            let score = (
                candidate_facts.len(),
                dominator_depth(&cfg, candidate_index),
            );
            if best.as_ref().is_none_or(
                |(best_score, _, _): &((usize, usize), BlockId, BlockId)| score > *best_score,
            ) {
                best = Some((score, candidate, merge));
            }
        }
        let Some((_, candidate, merge)) = best else {
            continue;
        };
        if claimed.insert(value) {
            plans.push(PredicatedLiveOutPlan {
                value,
                source,
                candidate,
                merge,
                facts: facts[cfg.block_index(candidate).unwrap()].clone(),
            });
        }
    }
    if plans.is_empty() {
        return false;
    }
    plans.sort_unstable_by_key(|plan| (plan.merge.0, plan.value.0));

    let mut next_register = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
    let Some(_) = plans
        .len()
        .checked_mul(2)
        .and_then(|additional| next_register.checked_add(additional))
    else {
        return false;
    };
    for plan in plans {
        let dummy_id = next_register + 1;
        let phi_id = dummy_id + 1;
        next_register = phi_id;
        let Some(ty) = eu.register_map.get(&plan.value).cloned() else {
            continue;
        };
        let dummy = RegisterId(dummy_id);
        let phi = RegisterId(phi_id);
        eu.register_map.insert(dummy, ty.clone());
        eu.register_map.insert(phi, ty);
        eu.blocks
            .get_mut(&plan.source)
            .unwrap()
            .instructions
            .push(SIRInstruction::Imm(dummy, SIRValue::new(0u8)));

        for &block_id in &cfg.block_ids {
            if cfg.dominates(plan.merge, block_id)
                && !cfg.dominates(plan.candidate, block_id)
                && plan
                    .facts
                    .iter()
                    .all(|fact| facts[cfg.block_index(block_id).unwrap()].contains(fact))
            {
                let block = eu.blocks.get_mut(&block_id).unwrap();
                for instruction in &mut block.instructions {
                    replace_register_uses_in_instruction(instruction, plan.value, phi);
                }
                replace_register_uses_in_terminator(&mut block.terminator, plan.value, phi);
            }
        }

        let merge_index = cfg.block_index(plan.merge).unwrap();
        for &predecessor_index in &cfg.predecessors[merge_index] {
            let predecessor = cfg.block_ids[predecessor_index];
            let argument = if cfg.dominates(plan.candidate, predecessor) {
                plan.value
            } else {
                dummy
            };
            append_edge_argument(
                &mut eu.blocks.get_mut(&predecessor).unwrap().terminator,
                plan.merge,
                argument,
            );
        }
        eu.blocks.get_mut(&plan.merge).unwrap().params.push(phi);
    }
    debug_assert_eq!(eu.verify_result(), Ok(()));
    true
}

pub(super) fn dominator_depth(cfg: &SirCfg, block: usize) -> usize {
    cfg.dominators.depth(block).unwrap_or(0)
}

pub(super) fn merge_accepts_phi(
    cfg: &SirCfg,
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    merge: BlockId,
) -> bool {
    let Some(merge_index) = cfg.block_index(merge) else {
        return false;
    };
    cfg.predecessors[merge_index].iter().all(|&predecessor| {
        let predecessor = cfg.block_ids[predecessor];
        match &eu.blocks[&predecessor].terminator {
            SIRTerminator::Jump(target, _) => *target == merge,
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => true_block.0 == merge || false_block.0 == merge,
            SIRTerminator::Switch { .. } => false,
            SIRTerminator::Return | SIRTerminator::Error(_) => false,
        }
    })
}

pub(super) fn append_edge_argument(
    terminator: &mut SIRTerminator,
    target: BlockId,
    argument: RegisterId,
) {
    match terminator {
        SIRTerminator::Jump(actual, arguments) if *actual == target => arguments.push(argument),
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => {
            if true_block.0 == target {
                true_block.1.push(argument);
            }
            if false_block.0 == target {
                false_block.1.push(argument);
            }
        }
        _ => unreachable!("validated merge predecessor must target the merge"),
    }
}
