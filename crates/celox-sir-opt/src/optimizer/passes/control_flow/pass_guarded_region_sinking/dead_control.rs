//! Pruning phi operands, block parameters and regions that control flow leaves dead.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum IncomingEdgeKind {
    Jump,
    True,
    False,
}

#[derive(Clone)]
pub(super) struct IncomingPhiEdge {
    pub(super) predecessor: BlockId,
    pub(super) kind: IncomingEdgeKind,
    pub(super) arguments: Vec<RegisterId>,
    pub(super) facts: Vec<(RegisterId, bool)>,
}

/// Remove phi operands which cannot reach any use of that phi result.
///
/// Priority regions commonly merge both their selected result and the
/// predicates needed by a later priority region.  On an early-exit edge the
/// later payload is unobservable, but ordinary SSA liveness still keeps its
/// producer alive.  The branch facts on the consuming side and the facts on
/// each incoming edge are sufficient to prove those operands dead without
/// enumerating paths.
pub(super) fn prune_control_dead_phi_operands(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) -> bool {
    let Ok(cfg) = SirCfg::analyze_forward_structure(eu) else {
        return false;
    };
    let uses = collect_uses(eu);
    let block_facts = predicate_facts(eu, &cfg);
    let mut constants = HashMap::<RegisterId, bool>::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            if let SIRInstruction::Imm(dst, value) = instruction
                && value.mask.to_u64_digits().is_empty()
            {
                constants.insert(*dst, !value.payload.to_u64_digits().is_empty());
            }
        }
    }

    let mut replacements = Vec::<(BlockId, IncomingEdgeKind, usize, RegisterId)>::new();
    let mut replacement_params = HashSet::<RegisterId>::default();
    for &merge_id in &cfg.block_ids {
        let merge = &eu.blocks[&merge_id];
        if merge.params.is_empty() {
            continue;
        }
        let mut incoming = Vec::new();
        let merge_index = cfg.block_index(merge_id).unwrap();
        for &predecessor_index in &cfg.predecessors[merge_index] {
            let predecessor_id = cfg.block_ids[predecessor_index];
            let predecessor = &eu.blocks[&predecessor_id];
            match &predecessor.terminator {
                SIRTerminator::Jump(target, arguments) if *target == merge_id => {
                    incoming.push(IncomingPhiEdge {
                        predecessor: predecessor_id,
                        kind: IncomingEdgeKind::Jump,
                        arguments: arguments.clone(),
                        facts: block_facts[predecessor_index].clone(),
                    });
                }
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    if true_block.0 == merge_id {
                        let mut facts = block_facts[predecessor_index].clone();
                        facts.push((*cond, true));
                        incoming.push(IncomingPhiEdge {
                            predecessor: predecessor_id,
                            kind: IncomingEdgeKind::True,
                            arguments: true_block.1.clone(),
                            facts,
                        });
                    }
                    if false_block.0 == merge_id {
                        let mut facts = block_facts[predecessor_index].clone();
                        facts.push((*cond, false));
                        incoming.push(IncomingPhiEdge {
                            predecessor: predecessor_id,
                            kind: IncomingEdgeKind::False,
                            arguments: false_block.1.clone(),
                            facts,
                        });
                    }
                }
                _ => {}
            }
        }
        if incoming.len() < 2
            || incoming
                .iter()
                .any(|edge| edge.arguments.len() != merge.params.len())
        {
            continue;
        }
        let parameter_indices = merge
            .params
            .iter()
            .enumerate()
            .map(|(index, &parameter)| (parameter, index))
            .collect::<HashMap<_, _>>();

        for edge in incoming {
            let known_on_edge = |register: RegisterId| {
                edge.facts
                    .iter()
                    .rev()
                    .find_map(|&(condition, value)| (condition == register).then_some(value))
                    .or_else(|| constants.get(&register).copied())
            };
            for (parameter_index, &parameter) in merge.params.iter().enumerate() {
                if constants.contains_key(&edge.arguments[parameter_index]) {
                    continue;
                }
                let Some(parameter_uses) = uses.get(&parameter) else {
                    continue;
                };
                let all_uses_unreachable = parameter_uses.iter().all(|site| {
                    let use_block = site.block();
                    if use_block == merge_id || !cfg.dominates(merge_id, use_block) {
                        return false;
                    }
                    block_facts[cfg.block_index(use_block).unwrap()].iter().any(
                        |&(condition, required)| {
                            let Some(&condition_index) = parameter_indices.get(&condition) else {
                                return false;
                            };
                            known_on_edge(edge.arguments[condition_index])
                                .is_some_and(|actual| actual != required)
                        },
                    )
                });
                if all_uses_unreachable {
                    replacements.push((edge.predecessor, edge.kind, parameter_index, parameter));
                    replacement_params.insert(parameter);
                }
            }
        }
    }
    if replacements.is_empty() {
        return false;
    }

    let mut next_register = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
    let Some(last_register) = next_register.checked_add(replacement_params.len()) else {
        return false;
    };
    let entry = cfg.block_ids[0];
    let mut zero_for_param = HashMap::default();
    let mut params = replacement_params.into_iter().collect::<Vec<_>>();
    params.sort_unstable();
    for parameter in params {
        next_register += 1;
        let zero = RegisterId(next_register);
        eu.register_map
            .insert(zero, eu.register_map[&parameter].clone());
        eu.blocks
            .get_mut(&entry)
            .unwrap()
            .instructions
            .push(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        zero_for_param.insert(parameter, zero);
    }
    debug_assert_eq!(next_register, last_register);

    for (predecessor, kind, parameter_index, parameter) in replacements {
        let zero = zero_for_param[&parameter];
        match (
            &mut eu.blocks.get_mut(&predecessor).unwrap().terminator,
            kind,
        ) {
            (SIRTerminator::Jump(_, arguments), IncomingEdgeKind::Jump) => {
                arguments[parameter_index] = zero;
            }
            (SIRTerminator::Branch { true_block, .. }, IncomingEdgeKind::True) => {
                true_block.1[parameter_index] = zero;
            }
            (SIRTerminator::Branch { false_block, .. }, IncomingEdgeKind::False) => {
                false_block.1[parameter_index] = zero;
            }
            _ => unreachable!(),
        }
    }
    debug_assert_eq!(eu.verify_result(), Ok(()));
    true
}

/// Remove control dependence which has no remaining value or effect after
/// rooted dead-store elimination. This is the CFG half of ADCE: first prune
/// dead phi parameters, then bypass pure SESE regions whose merge carries no
/// live value.
pub(in crate::optimizer) fn eliminate_dead_control_regions(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) {
    loop {
        prune_dead_block_parameters(eu);
        super::super::pass_vectorize_concat::remove_dead_definitions(eu);
        let Ok(cfg) = SirCfg::analyze_structure(eu) else {
            return;
        };
        let uses = collect_uses(eu);
        let mut candidates = Vec::new();
        for &head_id in &cfg.block_ids {
            let SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } = &eu.blocks[&head_id].terminator
            else {
                continue;
            };
            let Some(merge_id) = cfg.common_postdominator(true_block.0, false_block.0) else {
                continue;
            };
            if merge_id == head_id || !eu.blocks[&merge_id].params.is_empty() {
                continue;
            }
            let mut region = HashSet::default();
            let mut work = vec![true_block.0, false_block.0];
            let mut valid = true;
            while let Some(block_id) = work.pop() {
                if block_id == merge_id || !region.insert(block_id) {
                    continue;
                }
                let index = cfg.block_index(block_id).unwrap();
                let block = &eu.blocks[&block_id];
                if cfg.sccs[cfg.scc_for_block[index]].cyclic
                    || block.instructions.iter().any(instruction_has_effect)
                {
                    valid = false;
                    break;
                }
                match &block.terminator {
                    SIRTerminator::Jump(target, arguments) => {
                        if *target == merge_id && !arguments.is_empty() {
                            valid = false;
                            break;
                        }
                        work.push(*target);
                    }
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
            if !valid || region.is_empty() {
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
            }
            if valid {
                candidates.push((head_id, merge_id, region));
            }
        }
        if candidates.is_empty() {
            break;
        }

        // One CFG/use analysis can prove several independent dead regions.
        // Prefer the largest region when candidates nest, and reserve its
        // head, body, and merge so no other rewrite in this batch can mutate
        // or remove one of those blocks. Re-analyze only after the maximal
        // non-overlapping batch has been removed.
        candidates.sort_unstable_by(|lhs, rhs| {
            rhs.2
                .len()
                .cmp(&lhs.2.len())
                .then_with(|| lhs.0.0.cmp(&rhs.0.0))
        });
        let mut claimed = HashSet::default();
        let mut selected = Vec::new();
        for (head, merge, region) in candidates {
            if claimed.contains(&head)
                || claimed.contains(&merge)
                || region.iter().any(|block| claimed.contains(block))
            {
                continue;
            }
            claimed.insert(head);
            claimed.insert(merge);
            claimed.extend(region.iter().copied());
            selected.push((head, merge, region));
        }
        debug_assert!(!selected.is_empty());
        for (head, merge, region) in selected {
            eu.blocks.get_mut(&head).unwrap().terminator = SIRTerminator::Jump(merge, Vec::new());
            for block in region {
                eu.blocks.remove(&block);
            }
        }
    }
    prune_dead_block_parameters(eu);
    super::super::pass_vectorize_concat::remove_dead_definitions(eu);
    debug_assert_eq!(eu.verify_result(), Ok(()));
}

pub(super) fn prune_dead_block_parameters(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    loop {
        let uses = collect_uses(eu);
        let mut dead = BTreeMap::<BlockId, Vec<usize>>::new();
        for block in eu.blocks.values() {
            for (index, parameter) in block.params.iter().enumerate() {
                if !uses.contains_key(parameter) {
                    dead.entry(block.id).or_default().push(index);
                }
            }
        }
        if dead.is_empty() {
            break;
        }
        for (&block, indices) in &dead {
            for &index in indices.iter().rev() {
                eu.blocks.get_mut(&block).unwrap().params.remove(index);
            }
        }
        for predecessor in eu.blocks.values_mut() {
            remove_dead_edge_arguments(&mut predecessor.terminator, &dead);
        }
    }
}

pub(super) fn remove_dead_edge_arguments(
    terminator: &mut SIRTerminator,
    dead: &BTreeMap<BlockId, Vec<usize>>,
) {
    let remove = |target: BlockId, arguments: &mut Vec<RegisterId>| {
        let Some(indices) = dead.get(&target) else {
            return;
        };
        for &index in indices.iter().rev() {
            arguments.remove(index);
        }
    };
    match terminator {
        SIRTerminator::Jump(target, arguments) => {
            remove(*target, arguments);
        }
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => {
            remove(true_block.0, &mut true_block.1);
            remove(false_block.0, &mut false_block.1);
        }
        _ => {}
    }
}
