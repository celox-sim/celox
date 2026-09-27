//! Selector-guarded predicate recognition and dispatch rewriting.

use super::*;

pub(super) fn branchify_selector_guarded_predicates(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) -> usize {
    let def_locations = instruction_def_locations(eu);
    let use_locations = register_use_locations(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|block| block.0);
    let plans = block_ids
        .into_iter()
        .filter_map(|block| find_selector_predicate_plan(eu, &def_locations, &use_locations, block))
        .collect::<Vec<_>>();
    let applied = plans.len();
    for plan in plans {
        apply_selector_predicate_plan(eu, plan, next_block_id, reg_counter);
    }
    applied
}

fn find_selector_predicate_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    use_locations: &HashMap<RegisterId, Vec<UseLocation>>,
    block_id: BlockId,
) -> Option<SelectorPredicatePlan> {
    let block = eu.blocks.get(&block_id)?;
    let SIRTerminator::Branch {
        cond,
        true_block,
        false_block,
    } = &block.terminator
    else {
        return None;
    };
    if true_block.0 == false_block.0
        || block.instructions.iter().any(|instruction| {
            !matches!(
                instruction,
                SIRInstruction::Imm(..)
                    | SIRInstruction::Load(..)
                    | SIRInstruction::Binary(..)
                    | SIRInstruction::Unary(..)
                    | SIRInstruction::Concat(..)
                    | SIRInstruction::Slice(..)
                    | SIRInstruction::Mux(..)
            )
        })
    {
        // Delaying a Load is valid across pure computation, but not across an
        // observable write, commit, or runtime event in the source block.
        return None;
    }

    let mut root_removed = HashSet::default();
    let root = peel_selector_boolean_alias(eu, def_locations, block_id, *cond, &mut root_removed);
    let &(root_block, root_index) = def_locations.get(&root)?;
    if root_block != block_id {
        return None;
    }
    let SIRInstruction::Binary(_, lhs, crate::ir::BinaryOp::LogicAnd, rhs) =
        &block.instructions[root_index]
    else {
        return None;
    };
    root_removed.insert(root_index);

    for (common_condition, selector_expression) in [(*lhs, *rhs), (*rhs, *lhs)] {
        let mut removed = root_removed.clone();
        let Some(mut arms) = parse_selector_sum(
            eu,
            def_locations,
            block_id,
            selector_expression,
            &mut removed,
        ) else {
            continue;
        };

        let edge_arguments = true_block
            .1
            .iter()
            .chain(&false_block.1)
            .copied()
            .collect::<HashSet<_>>();
        let removed_is_closed = removed.iter().all(|&index| {
            let Some(dst) = def_reg(&block.instructions[index]) else {
                return false;
            };
            if edge_arguments.contains(&dst) {
                return false;
            }
            use_locations
                .get(&dst)
                .into_iter()
                .flatten()
                .all(|location| {
                    location.block == block_id
                        && match location.instruction {
                            Some(use_index) => removed.contains(&use_index),
                            None => dst == *cond,
                        }
                })
        });
        if !removed_is_closed {
            continue;
        }

        for arm in &mut arms {
            collect_selector_motion_closure(
                eu,
                def_locations,
                block_id,
                arm.selector_condition,
                &removed,
                &mut arm.selector_defs,
            );
            collect_selector_motion_closure(
                eu,
                def_locations,
                block_id,
                arm.payload_condition,
                &removed,
                &mut arm.payload_defs,
            );
        }

        // An instruction reachable from more than one selector arm must stay
        // in the head.  This keeps the transformation linear and avoids
        // duplicating shared address or expected-value computation.
        let mut membership = HashMap::<usize, (usize, usize)>::default();
        for (arm_index, arm) in arms.iter().enumerate() {
            let mut all = arm.selector_defs.clone();
            all.extend(arm.payload_defs.iter().copied());
            for index in all {
                membership
                    .entry(index)
                    .and_modify(|(owner, count)| {
                        if *owner != arm_index {
                            *count += 1;
                        }
                    })
                    .or_insert((arm_index, 1));
            }
        }
        let mut movable = membership
            .iter()
            .filter_map(|(&index, &(_, count))| {
                (count == 1 && !removed.contains(&index)).then_some(index)
            })
            .collect::<HashSet<_>>();

        // Close the selected move set under uses.  If one candidate definition
        // is also consumed by a head instruction, edge argument, or another
        // block, retain it in the head and transitively retain its operands.
        let mut reject = VecDeque::new();
        for &index in &movable {
            if !selector_definition_uses_are_closed(
                block,
                use_locations,
                block_id,
                index,
                &movable,
                &removed,
            ) {
                reject.push_back(index);
            }
        }
        while let Some(index) = reject.pop_front() {
            if !movable.remove(&index) {
                continue;
            }
            for operand in inst_uses(&block.instructions[index]) {
                let Some(&(definition_block, definition_index)) = def_locations.get(&operand)
                else {
                    continue;
                };
                if definition_block == block_id
                    && movable.contains(&definition_index)
                    && !selector_definition_uses_are_closed(
                        block,
                        use_locations,
                        block_id,
                        definition_index,
                        &movable,
                        &removed,
                    )
                {
                    reject.push_back(definition_index);
                }
            }
        }

        let arms_with_delayed_load = arms
            .iter()
            .enumerate()
            .filter(|(arm_index, _)| {
                let arm_index = *arm_index;
                movable.iter().any(|&index| {
                    membership.get(&index) == Some(&(arm_index, 1))
                        && matches!(block.instructions[index], SIRInstruction::Load(..))
                })
            })
            .count();
        if arms_with_delayed_load < 2 {
            // The extra selector and payload branches must skip real memory
            // work on at least two mutually exclusive paths.
            continue;
        }

        let planned_arms = arms
            .into_iter()
            .enumerate()
            .map(|(arm_index, arm)| {
                let mut owned = movable
                    .iter()
                    .filter_map(|&index| {
                        (membership.get(&index) == Some(&(arm_index, 1))).then_some(index)
                    })
                    .collect::<Vec<_>>();
                owned.sort_unstable();
                let (decision_defs, payload_defs) = owned
                    .into_iter()
                    .partition(|index| arm.selector_defs.contains(index));
                SelectorPredicateArmPlan {
                    selector_condition: arm.selector_condition,
                    payload_condition: arm.payload_condition,
                    decision_defs,
                    payload_defs,
                }
            })
            .collect::<Vec<_>>();
        let mut removed_defs = removed.into_iter().collect::<Vec<_>>();
        removed_defs.sort_unstable();
        return Some(SelectorPredicatePlan {
            block_id,
            common_condition,
            true_target: true_block.clone(),
            false_target: false_block.clone(),
            removed_defs,
            arms: planned_arms,
        });
    }
    None
}

fn peel_selector_boolean_alias(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    block_id: BlockId,
    mut register: RegisterId,
    removed: &mut HashSet<usize>,
) -> RegisterId {
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let Some(&(definition_block, index)) = def_locations.get(&register) else {
            break;
        };
        if definition_block != block_id {
            break;
        }
        let instruction = &eu.blocks[&block_id].instructions[index];
        let source = match instruction {
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => Some(*source),
            SIRInstruction::Unary(_, crate::ir::UnaryOp::And | crate::ir::UnaryOp::Or, source)
                if eu
                    .register_map
                    .get(source)
                    .is_some_and(|register| register.width() == 1) =>
            {
                Some(*source)
            }
            _ => None,
        };
        let Some(source) = source else {
            break;
        };
        removed.insert(index);
        register = source;
    }
    register
}

fn parse_selector_sum(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    block_id: BlockId,
    expression: RegisterId,
    removed: &mut HashSet<usize>,
) -> Option<Vec<ParsedSelectorArm>> {
    let mut terms = Vec::new();
    let mut seen = HashSet::default();
    if !flatten_selector_or(
        eu,
        def_locations,
        block_id,
        expression,
        removed,
        &mut seen,
        &mut terms,
    ) || !(2..=8).contains(&terms.len())
    {
        return None;
    }

    let mut operands = Vec::with_capacity(terms.len());
    for term in terms {
        let term = peel_selector_boolean_alias(eu, def_locations, block_id, term, removed);
        let &(definition_block, index) = def_locations.get(&term)?;
        if definition_block != block_id {
            return None;
        }
        let SIRInstruction::Binary(_, lhs, crate::ir::BinaryOp::LogicAnd, rhs) =
            &eu.blocks[&block_id].instructions[index]
        else {
            return None;
        };
        removed.insert(index);
        operands.push([*lhs, *rhs]);
    }

    for first_selector_side in 0..2 {
        let Some((selector, first_constant)) =
            selector_guard_key(eu, def_locations, operands[0][first_selector_side])
        else {
            continue;
        };
        if selector_guard_key(eu, def_locations, operands[0][1 - first_selector_side])
            .is_some_and(|(other_selector, _)| other_selector == selector)
        {
            continue;
        }
        let mut constants = HashSet::default();
        constants.insert(first_constant);
        let mut arms = vec![ParsedSelectorArm {
            selector_condition: operands[0][first_selector_side],
            payload_condition: operands[0][1 - first_selector_side],
            selector_defs: HashSet::default(),
            payload_defs: HashSet::default(),
        }];
        let mut valid = true;
        for pair in operands.iter().skip(1) {
            let matches = (0..2)
                .filter_map(|side| {
                    let (candidate_selector, constant) =
                        selector_guard_key(eu, def_locations, pair[side])?;
                    (candidate_selector == selector).then_some((side, constant))
                })
                .collect::<Vec<_>>();
            if matches.len() != 1 || !constants.insert(matches[0].1.clone()) {
                valid = false;
                break;
            }
            let side = matches[0].0;
            arms.push(ParsedSelectorArm {
                selector_condition: pair[side],
                payload_condition: pair[1 - side],
                selector_defs: HashSet::default(),
                payload_defs: HashSet::default(),
            });
        }
        if valid {
            return Some(arms);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn flatten_selector_or(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    block_id: BlockId,
    expression: RegisterId,
    removed: &mut HashSet<usize>,
    seen: &mut HashSet<RegisterId>,
    terms: &mut Vec<RegisterId>,
) -> bool {
    let expression = peel_selector_boolean_alias(eu, def_locations, block_id, expression, removed);
    if !seen.insert(expression) {
        return false;
    }
    let Some(&(definition_block, index)) = def_locations.get(&expression) else {
        terms.push(expression);
        return true;
    };
    if definition_block != block_id {
        terms.push(expression);
        return true;
    }
    let SIRInstruction::Binary(_, lhs, crate::ir::BinaryOp::LogicOr, rhs) =
        &eu.blocks[&block_id].instructions[index]
    else {
        terms.push(expression);
        return true;
    };
    removed.insert(index);
    flatten_selector_or(eu, def_locations, block_id, *lhs, removed, seen, terms)
        && flatten_selector_or(eu, def_locations, block_id, *rhs, removed, seen, terms)
}

fn selector_guard_key(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    condition: RegisterId,
) -> Option<(RegisterId, Vec<u64>)> {
    let (key, inverted) = predicate_key(eu, def_locations, condition)?;
    if inverted || key.kind != PredicateKind::Equal {
        return None;
    }
    let PredicateRhs::Constant(payload, mask) = key.rhs else {
        return None;
    };
    mask.iter()
        .all(|word| *word == 0)
        .then_some((key.lhs, payload))
}

fn collect_selector_motion_closure(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    block_id: BlockId,
    root: RegisterId,
    removed: &HashSet<usize>,
    result: &mut HashSet<usize>,
) {
    let Some(&(definition_block, index)) = def_locations.get(&root) else {
        return;
    };
    if definition_block != block_id || removed.contains(&index) || !result.insert(index) {
        return;
    }
    let instruction = &eu.blocks[&block_id].instructions[index];
    if !matches!(
        instruction,
        SIRInstruction::Imm(..)
            | SIRInstruction::Load(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..)
    ) {
        result.remove(&index);
        return;
    }
    visit_instruction_uses(instruction, |operand| {
        collect_selector_motion_closure(eu, def_locations, block_id, operand, removed, result);
    });
}

fn selector_definition_uses_are_closed(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    use_locations: &HashMap<RegisterId, Vec<UseLocation>>,
    block_id: BlockId,
    index: usize,
    movable: &HashSet<usize>,
    removed: &HashSet<usize>,
) -> bool {
    let Some(dst) = def_reg(&block.instructions[index]) else {
        return false;
    };
    use_locations
        .get(&dst)
        .into_iter()
        .flatten()
        .all(|location| {
            location.block == block_id
                && location.instruction.is_some_and(|use_index| {
                    movable.contains(&use_index) || removed.contains(&use_index)
                })
        })
}

fn apply_selector_predicate_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: SelectorPredicatePlan,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) {
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("selector-predicate source block must exist");
    let removed = plan.removed_defs.iter().copied().collect::<HashSet<_>>();
    let moved = plan
        .arms
        .iter()
        .flat_map(|arm| arm.decision_defs.iter().chain(&arm.payload_defs))
        .copied()
        .collect::<HashSet<_>>();
    debug_assert!(removed.is_disjoint(&moved));

    let arm_blocks = (0..plan.arms.len())
        .map(|_| {
            let decision = BlockId(*next_block_id);
            let payload = BlockId(*next_block_id + 1);
            *next_block_id += 2;
            (decision, payload)
        })
        .collect::<Vec<_>>();
    let mut head_instructions = original
        .instructions
        .iter()
        .enumerate()
        .filter(|(index, _)| !removed.contains(index) && !moved.contains(index))
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let common_condition = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_instructions,
        plan.common_condition,
        reg_counter,
    );
    let head = BasicBlock {
        id: original.id,
        params: original.params.clone(),
        instructions: head_instructions,
        terminator: SIRTerminator::Branch {
            cond: common_condition,
            true_block: (arm_blocks[0].0, Vec::new()),
            false_block: plan.false_target.clone(),
        },
    };
    eu.blocks.insert(head.id, head);

    for (arm_index, (arm, &(decision_id, payload_id))) in
        plan.arms.iter().zip(&arm_blocks).enumerate()
    {
        let decision_false = arm_blocks
            .get(arm_index + 1)
            .map_or_else(|| plan.false_target.clone(), |next| (next.0, Vec::new()));
        let mut decision_instructions = arm
            .decision_defs
            .iter()
            .map(|&index| original.instructions[index].clone())
            .collect::<Vec<_>>();
        let selector_condition = normalize_branch_condition(
            &mut eu.register_map,
            &mut decision_instructions,
            arm.selector_condition,
            reg_counter,
        );
        eu.blocks.insert(
            decision_id,
            BasicBlock {
                id: decision_id,
                params: Vec::new(),
                instructions: decision_instructions,
                terminator: SIRTerminator::Branch {
                    cond: selector_condition,
                    true_block: (payload_id, Vec::new()),
                    false_block: decision_false,
                },
            },
        );
        let mut payload_instructions = arm
            .payload_defs
            .iter()
            .map(|&index| original.instructions[index].clone())
            .collect::<Vec<_>>();
        let payload_condition = normalize_branch_condition(
            &mut eu.register_map,
            &mut payload_instructions,
            arm.payload_condition,
            reg_counter,
        );
        eu.blocks.insert(
            payload_id,
            BasicBlock {
                id: payload_id,
                params: Vec::new(),
                instructions: payload_instructions,
                terminator: SIRTerminator::Branch {
                    cond: payload_condition,
                    true_block: plan.true_target.clone(),
                    false_block: plan.false_target.clone(),
                },
            },
        );
    }
}
