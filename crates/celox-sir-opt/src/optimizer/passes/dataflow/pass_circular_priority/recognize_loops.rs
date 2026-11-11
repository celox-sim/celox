//! Recognizing bit map loops, circular priority loops and their orientation.

use super::*;

pub(super) fn recognize_bit_map_loop(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    use_blocks: &HashMap<RegisterId, HashSet<BlockId>>,
) -> Option<BitMapLoopPlan> {
    let block = &eu.blocks[&header];
    if block.params.len() != 3 {
        return None;
    }
    if block
        .params
        .iter()
        .copied()
        .chain(block.instructions.iter().filter_map(def_reg))
        .any(|value| {
            use_blocks
                .get(&value)
                .is_some_and(|users| users.iter().any(|user| !loop_blocks.contains(user)))
        })
    {
        return None;
    }
    if block
        .instructions
        .iter()
        .any(|instruction| instruction.has_side_effects())
    {
        return None;
    }
    let header_index = cfg.block_index(header)?;
    let outside = cfg.predecessors[header_index]
        .iter()
        .map(|&index| cfg.block_ids[index])
        .filter(|block| *block != header)
        .collect::<Vec<_>>();
    let [preheader] = outside.as_slice() else {
        return None;
    };
    let preheader = *preheader;
    let SIRTerminator::Jump(target, entry_arguments) = &eu.blocks[&preheader].terminator else {
        return None;
    };
    if *target != header || entry_arguments.len() != block.params.len() {
        return None;
    }
    let (condition, true_target, false_target) = branch_targets(&block.terminator)?;
    let (exit, loop_when_true) = if true_target == header && false_target != header {
        (false_target, true)
    } else if false_target == header && true_target != header {
        (true_target, false)
    } else {
        return None;
    };
    let backedge = edge_arguments(&block.terminator, header)?;
    let exit_arguments = edge_arguments(&block.terminator, exit)?;
    let (count_position, lanes) = match_count_recurrence(
        eu,
        definitions,
        block,
        entry_arguments,
        backedge,
        condition,
        loop_when_true,
    )?;
    if lanes != 16 {
        return None;
    }
    let index_position = match_index_recurrence(
        eu,
        definitions,
        block,
        entry_arguments,
        backedge,
        count_position,
    )?;
    let accumulator_position = (0..block.params.len())
        .find(|position| *position != count_position && *position != index_position)?;
    let current = block.params[accumulator_position];
    let accumulator_type = eu.register_map.get(&current)?.clone();
    if accumulator_type.width() < lanes {
        return None;
    }
    let update = backedge[accumulator_position];
    let (shift, bit) = match_inserted_bit(eu, definitions, current, update)?;
    let index = block.params[index_position];
    if !shift_is_index(eu, definitions, shift, index) {
        return None;
    }
    let exit_result_position = exit_arguments.iter().position(|&value| value == update)?;
    if exit_arguments.iter().enumerate().any(|(position, value)| {
        position != exit_result_position
            && definitions
                .get(value)
                .is_some_and(|definition| definition.block() == header)
    }) {
        return None;
    }
    let dependencies = collect_bit_dependencies(
        eu,
        cfg,
        definitions,
        header,
        preheader,
        bit,
        index,
        &[block.params[count_position], current],
    )?;
    Some(BitMapLoopPlan {
        preheader,
        header,
        exit,
        lanes,
        accumulator_type,
        initial_accumulator: entry_arguments[accumulator_position],
        index,
        bit,
        dependency_indices: dependencies,
        exit_arguments: exit_arguments.to_vec(),
        exit_result_position,
    })
}

pub(super) fn common_jump_target(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: BlockId,
) -> Option<BlockId> {
    let SIRTerminator::Jump(target, _) = &eu.blocks.get(&block)?.terminator else {
        return None;
    };
    Some(*target)
}

pub(super) fn is_add_one(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    value: RegisterId,
    source: RegisterId,
) -> bool {
    matches!(
        instruction(eu, definitions, value),
        Some(SIRInstruction::Binary(_, lhs, BinaryOp::Add, rhs))
            if (*lhs == source && immediate(eu, definitions, *rhs) == Some(1))
                || (*rhs == source && immediate(eu, definitions, *lhs) == Some(1))
    )
}

pub(super) fn is_zero_extended_index(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    value: RegisterId,
    index: RegisterId,
) -> bool {
    if value == index {
        return true;
    }
    let Some(SIRInstruction::Concat(_, parts)) = instruction(eu, definitions, value) else {
        return false;
    };
    parts.last() == Some(&index)
        && parts[..parts.len().saturating_sub(1)]
            .iter()
            .all(|&part| immediate(eu, definitions, part) == Some(0))
}

pub(super) fn recognize_loop(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
) -> Option<CircularPriorityPlan> {
    if loop_blocks.len() != 5 {
        return None;
    }
    let header_index = cfg.block_index(header)?;
    let outside_predecessors = cfg.predecessors[header_index]
        .iter()
        .map(|&block| cfg.block_ids[block])
        .filter(|block| !loop_blocks.contains(block))
        .collect::<Vec<_>>();
    let inside_predecessors = cfg.predecessors[header_index]
        .iter()
        .map(|&block| cfg.block_ids[block])
        .filter(|block| loop_blocks.contains(block))
        .collect::<Vec<_>>();
    let [preheader] = outside_predecessors.as_slice() else {
        return None;
    };
    let [latch] = inside_predecessors.as_slice() else {
        return None;
    };
    let preheader = *preheader;
    let latch = *latch;
    let preheader_block = &eu.blocks[&preheader];
    let header_block = &eu.blocks[&header];
    let latch_block = &eu.blocks[&latch];
    let SIRTerminator::Jump(preheader_target, entry_arguments) = &preheader_block.terminator else {
        return None;
    };
    if *preheader_target != header || entry_arguments.len() != header_block.params.len() {
        return None;
    }

    let (latch_condition, latch_true, latch_false) = branch_targets(&latch_block.terminator)?;
    let (exit, loop_when_true) = if latch_true == header && !loop_blocks.contains(&latch_false) {
        (latch_false, true)
    } else if latch_false == header && !loop_blocks.contains(&latch_true) {
        (latch_true, false)
    } else {
        return None;
    };
    let backedge_arguments = edge_arguments(&latch_block.terminator, header)?;
    let exit_arguments = edge_arguments(&latch_block.terminator, exit)?;
    if backedge_arguments.len() != header_block.params.len()
        || exit_arguments.len() != eu.blocks[&exit].params.len()
        || latch_block.params.len() != 2
    {
        return None;
    }

    let (count_position, lanes) = match_count_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        latch_condition,
        loop_when_true,
    )?;
    if !(4..=32).contains(&lanes) || !lanes.is_power_of_two() {
        return None;
    }
    let index_position = match_index_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        count_position,
    )?;
    let lane_bits = lanes.trailing_zeros() as usize;
    if eu.register_map[&header_block.params[count_position]].width() <= lane_bits
        || eu.register_map[&header_block.params[index_position]].width() < lane_bits
    {
        return None;
    }

    let (header_condition, header_true, header_false) = branch_targets(&header_block.terminator)?;
    for (candidate, skip, invert_predicate) in [
        (header_true, header_false, false),
        (header_false, header_true, true),
    ] {
        let Some(plan) = recognize_orientation(
            eu,
            cfg,
            header,
            candidate,
            skip,
            latch,
            exit,
            loop_blocks,
            definitions,
            constant_cache,
            preheader,
            header_condition,
            invert_predicate,
            lanes,
            index_position,
            count_position,
            entry_arguments,
            backedge_arguments,
            exit_arguments,
            bit_array_elements,
            array_shapes,
        ) else {
            continue;
        };
        return Some(plan);
    }
    None
}

pub(super) fn match_count_recurrence(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    header: &BasicBlock<RegionedAbsoluteAddr>,
    entry_arguments: &[RegisterId],
    backedge_arguments: &[RegisterId],
    condition: RegisterId,
    loop_when_true: bool,
) -> Option<(usize, usize)> {
    let SIRInstruction::Binary(_, lhs, operation, rhs) = instruction(eu, definitions, condition)?
    else {
        return None;
    };
    let operation_matches = matches!(operation, BinaryOp::Ne) && loop_when_true
        || matches!(operation, BinaryOp::Eq) && !loop_when_true;
    if !operation_matches {
        return None;
    }
    let next_count = if immediate(eu, definitions, *lhs) == Some(0) {
        *rhs
    } else if immediate(eu, definitions, *rhs) == Some(0) {
        *lhs
    } else {
        return None;
    };
    let position = backedge_arguments
        .iter()
        .position(|&argument| argument == next_count)?;
    let counter = *header.params.get(position)?;
    let SIRInstruction::Binary(_, decrement_lhs, BinaryOp::Sub, decrement_rhs) =
        instruction(eu, definitions, next_count)?
    else {
        return None;
    };
    if *decrement_lhs != counter || immediate(eu, definitions, *decrement_rhs) != Some(1) {
        return None;
    }
    let lanes = usize::try_from(immediate(eu, definitions, entry_arguments[position])?).ok()?;
    (lanes != 0).then_some((position, lanes))
}

pub(super) fn match_index_recurrence(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    header: &BasicBlock<RegionedAbsoluteAddr>,
    entry_arguments: &[RegisterId],
    backedge_arguments: &[RegisterId],
    count_position: usize,
) -> Option<usize> {
    let mut found = None;
    for position in 0..header.params.len() {
        if position == count_position
            || immediate(eu, definitions, entry_arguments[position]) != Some(0)
        {
            continue;
        }
        let next_index = backedge_arguments[position];
        let Some(SIRInstruction::Binary(_, lhs, BinaryOp::Add, rhs)) =
            instruction(eu, definitions, next_index)
        else {
            continue;
        };
        let index = header.params[position];
        let matches = *lhs == index && immediate(eu, definitions, *rhs) == Some(1)
            || *rhs == index && immediate(eu, definitions, *lhs) == Some(1);
        if !matches || found.replace(position).is_some() {
            return None;
        }
    }
    found
}

#[allow(clippy::too_many_arguments)]
pub(super) fn recognize_orientation(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    candidate: BlockId,
    skip: BlockId,
    latch: BlockId,
    exit: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    preheader: BlockId,
    predicate: RegisterId,
    invert_predicate: bool,
    lanes: usize,
    index_position: usize,
    count_position: usize,
    entry_arguments: &[RegisterId],
    backedge_arguments: &[RegisterId],
    exit_arguments: &[RegisterId],
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
) -> Option<CircularPriorityPlan> {
    if !loop_blocks.contains(&candidate)
        || !loop_blocks.contains(&skip)
        || candidate == skip
        || candidate == latch
        || skip == latch
    {
        return None;
    }
    let header_block = &eu.blocks[&header];
    let candidate_block = &eu.blocks[&candidate];
    let skip_block = &eu.blocks[&skip];
    let latch_block = &eu.blocks[&latch];
    if !skip_block.instructions.is_empty() {
        return None;
    }
    let SIRTerminator::Jump(skip_target, skip_arguments) = &skip_block.terminator else {
        return None;
    };
    if *skip_target != latch || skip_arguments.len() != latch_block.params.len() {
        return None;
    }

    let (update_condition, candidate_true, candidate_false) =
        branch_targets(&candidate_block.terminator)?;
    let orientations = [
        (candidate_true, candidate_false, true),
        (candidate_false, candidate_true, false),
    ];
    for (update, candidate_skip, update_when_true) in orientations {
        if candidate_skip != skip
            || !loop_blocks.contains(&update)
            || update == header
            || update == candidate
            || update == skip
            || update == latch
        {
            continue;
        }
        let update_block = &eu.blocks[&update];
        if !update_block.instructions.is_empty() {
            continue;
        }
        let SIRTerminator::Jump(update_target, update_arguments) = &update_block.terminator else {
            continue;
        };
        if *update_target != latch || update_arguments.len() != latch_block.params.len() {
            continue;
        }
        let expected_blocks = [header, candidate, skip, update, latch]
            .into_iter()
            .collect::<HashSet<_>>();
        if &expected_blocks != loop_blocks {
            continue;
        }

        let state_positions = (0..header_block.params.len())
            .filter(|position| *position != count_position && *position != index_position)
            .collect::<Vec<_>>();
        let [first_state, second_state] = state_positions.as_slice() else {
            continue;
        };
        let mut found_merge = None;
        let mut best_merge = None;
        for merge_position in 0..latch_block.params.len() {
            let Some(header_position) = [*first_state, *second_state]
                .into_iter()
                .find(|&position| header_block.params[position] == skip_arguments[merge_position])
            else {
                found_merge = None;
                best_merge = None;
                break;
            };
            if immediate(eu, definitions, update_arguments[merge_position]) == Some(1)
                && eu.register_map[&header_block.params[header_position]].width() == 1
            {
                if found_merge
                    .replace((merge_position, header_position))
                    .is_some()
                {
                    found_merge = None;
                    break;
                }
            } else if best_merge
                .replace((merge_position, header_position))
                .is_some()
            {
                best_merge = None;
                break;
            }
        }
        let (Some((found_merge, found_position)), Some((best_merge, best_position))) =
            (found_merge, best_merge)
        else {
            continue;
        };
        if immediate(eu, definitions, entry_arguments[found_position]) != Some(0)
            || immediate(eu, definitions, entry_arguments[best_position]) != Some(0)
            || backedge_arguments[found_position] != latch_block.params[found_merge]
            || backedge_arguments[best_position] != latch_block.params[best_merge]
        {
            continue;
        }

        let age = update_arguments[best_merge];
        let found = header_block.params[found_position];
        let best = header_block.params[best_position];
        let Some(head) = match_update_condition(
            eu,
            definitions,
            update_condition,
            update_when_true,
            found,
            best,
            age,
            header_block.params[index_position],
            lanes,
        ) else {
            continue;
        };

        let found_exit = exit_arguments
            .iter()
            .position(|&argument| argument == latch_block.params[found_merge]);
        let best_exit = exit_arguments
            .iter()
            .position(|&argument| argument == latch_block.params[best_merge]);
        let (Some(found_exit), Some(best_exit)) = (found_exit, best_exit) else {
            continue;
        };
        if found_exit == best_exit
            || exit_arguments.len() != 2
            || eu.register_map[&eu.blocks[&exit].params[found_exit]].width() != 1
            || eu.register_map[&eu.blocks[&exit].params[best_exit]].width()
                != lanes.trailing_zeros() as usize
            || eu.register_map[&head].width() != lanes.trailing_zeros() as usize
        {
            continue;
        }

        if !loop_is_pure(eu, loop_blocks) {
            continue;
        }
        let Some(predicate) = build_packed_expression(
            eu,
            cfg,
            definitions,
            constant_cache,
            loop_blocks,
            preheader,
            header_block.params[index_position],
            predicate,
            invert_predicate,
            lanes,
            bit_array_elements,
            array_shapes,
            &HashSet::default(),
        ) else {
            continue;
        };
        let old_cost =
            lanes.saturating_mul(predicate.dynamic_loads.saturating_add(predicate.value_ops));
        let new_cost = predicate
            .dynamic_loads
            .saturating_add(predicate.value_ops)
            .saturating_add(8);
        if predicate.dynamic_loads == 0 || old_cost <= new_cost {
            continue;
        }

        let mut loop_blocks = loop_blocks.iter().copied().collect::<Vec<_>>();
        loop_blocks.sort_unstable();
        return Some(CircularPriorityPlan {
            preheader,
            loop_blocks,
            hoisted_instructions: Vec::new(),
            exit,
            exit_found_position: found_exit,
            exit_best_position: best_exit,
            head,
            lanes,
            found_type: eu.register_map[&eu.blocks[&exit].params[found_exit]].clone(),
            best_type: eu.register_map[&eu.blocks[&exit].params[best_exit]].clone(),
            predicate,
        });
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(super) fn match_update_condition(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    condition: RegisterId,
    update_when_true: bool,
    found: RegisterId,
    best: RegisterId,
    age: RegisterId,
    index: RegisterId,
    lanes: usize,
) -> Option<RegisterId> {
    let mut condition = strip_boolean_identity(eu, definitions, condition);
    if !update_when_true {
        let SIRInstruction::Unary(_, operation @ (UnaryOp::LogicNot | UnaryOp::BitNot), inner) =
            instruction(eu, definitions, condition)?
        else {
            return None;
        };
        let _ = operation;
        condition = strip_boolean_identity(eu, definitions, *inner);
    }
    let SIRInstruction::Binary(_, lhs, operation @ (BinaryOp::LogicOr | BinaryOp::Or), rhs) =
        instruction(eu, definitions, condition)?
    else {
        return None;
    };
    let _ = operation;
    let matches = |not_found, less| {
        matches_not(eu, definitions, not_found, found)
            && matches!(
                instruction(eu, definitions, less),
                Some(SIRInstruction::Binary(_, less_lhs, BinaryOp::LtU, less_rhs))
                    if *less_lhs == age && *less_rhs == best
            )
    };
    if !matches(*lhs, *rhs) && !matches(*rhs, *lhs) {
        return None;
    }
    match_circular_age(eu, definitions, age, index, lanes)
}

pub(super) fn matches_not(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    register: RegisterId,
    source: RegisterId,
) -> bool {
    matches!(
        instruction(eu, definitions, strip_boolean_identity(eu, definitions, register)),
        Some(SIRInstruction::Unary(_, UnaryOp::LogicNot | UnaryOp::BitNot, inner))
            if *inner == source
    )
}

pub(super) fn match_circular_age(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    age: RegisterId,
    index: RegisterId,
    lanes: usize,
) -> Option<RegisterId> {
    let mask = u64::try_from(lanes - 1).ok()?;
    let SIRInstruction::Binary(_, difference, BinaryOp::And, age_mask) =
        instruction(eu, definitions, age)?
    else {
        return None;
    };
    if immediate(eu, definitions, *age_mask) != Some(mask) {
        return None;
    }
    let SIRInstruction::Binary(_, lane, BinaryOp::Sub, head) =
        instruction(eu, definitions, *difference)?
    else {
        return None;
    };
    matches_index_mod(eu, definitions, *lane, index, mask).then_some(*head)
}

pub(super) fn matches_index_mod(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    mut register: RegisterId,
    index: RegisterId,
    mask: u64,
) -> bool {
    loop {
        if register == index {
            return true;
        }
        match instruction(eu, definitions, register) {
            Some(SIRInstruction::Unary(_, UnaryOp::Ident, inner)) => register = *inner,
            Some(SIRInstruction::Binary(_, source, BinaryOp::Shr, amount))
                if immediate(eu, definitions, *amount) == Some(0) =>
            {
                register = *source;
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::And, rhs))
                if immediate(eu, definitions, *rhs) == Some(mask) =>
            {
                register = *lhs;
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::And, rhs))
                if immediate(eu, definitions, *lhs) == Some(mask) =>
            {
                register = *rhs;
            }
            _ => return false,
        }
    }
}

pub(super) fn strip_boolean_identity(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    mut register: RegisterId,
) -> RegisterId {
    loop {
        let Some(instruction) = instruction(eu, definitions, register) else {
            return register;
        };
        match instruction {
            SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or, inner)
                if eu.register_map.get(inner).map(RegisterType::width) == Some(1) =>
            {
                register = *inner;
            }
            _ => return register,
        }
    }
}

pub(super) fn loop_is_pure(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    loop_blocks: &HashSet<BlockId>,
) -> bool {
    loop_blocks.iter().all(|block| {
        eu.blocks[block]
            .instructions
            .iter()
            .all(|instruction| !instruction.has_side_effects())
    })
}
