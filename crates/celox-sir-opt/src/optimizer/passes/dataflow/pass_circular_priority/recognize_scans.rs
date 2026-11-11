//! Recognizing bitmap scans, payload scans and sparse bitmap loops.

use super::*;

/// Recognize a fixed-trip scan which counts a packed predicate and remembers
/// its first matching lane.  The source CFG deliberately remains the proof:
/// no source-order or scheduler-order assumption is used.
#[allow(clippy::too_many_arguments)]
pub(super) fn recognize_linear_bitmap_scan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    use_blocks: &HashMap<RegisterId, HashSet<BlockId>>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
) -> Option<LinearBitmapScanPlan> {
    if loop_blocks.len() != 4 || !loop_is_pure(eu, loop_blocks) {
        return None;
    }
    if loop_blocks.iter().any(|block| {
        eu.blocks[block]
            .params
            .iter()
            .copied()
            .chain(eu.blocks[block].instructions.iter().filter_map(def_reg))
            .any(|value| {
                use_blocks
                    .get(&value)
                    .is_some_and(|users| users.iter().any(|user| !loop_blocks.contains(user)))
            })
    }) {
        return None;
    }
    let header_index = cfg.block_index(header)?;
    let outside_predecessors = cfg.predecessors[header_index]
        .iter()
        .map(|&block| cfg.block_ids[block])
        .filter(|block| !loop_blocks.contains(block))
        .collect::<Vec<_>>();
    let [preheader] = outside_predecessors.as_slice() else {
        return None;
    };
    let preheader = *preheader;
    let header_block = &eu.blocks[&header];
    let SIRTerminator::Jump(target, entry_arguments) = &eu.blocks[&preheader].terminator else {
        return None;
    };
    if *target != header || entry_arguments.len() != header_block.params.len() {
        return None;
    }

    let (header_condition, first_arm, carry_arm) = branch_targets(&header_block.terminator)?;
    let first_jump = edge_arguments(
        &eu.blocks[&first_arm].terminator,
        common_jump_target(eu, first_arm)?,
    )?;
    let body = common_jump_target(eu, first_arm)?;
    if common_jump_target(eu, carry_arm)? != body || !loop_blocks.contains(&body) {
        return None;
    }
    let carry_jump = edge_arguments(&eu.blocks[&carry_arm].terminator, body)?;
    let body_block = &eu.blocks[&body];
    if body_block.params.len() != 2 || first_jump.len() != 2 || carry_jump.len() != 2 {
        return None;
    }

    let (latch_condition, latch_true, latch_false) = branch_targets(&body_block.terminator)?;
    let (exit, loop_when_true) = if latch_true == header && !loop_blocks.contains(&latch_false) {
        (latch_false, true)
    } else if latch_false == header && !loop_blocks.contains(&latch_true) {
        (latch_true, false)
    } else {
        return None;
    };
    let backedge_arguments = edge_arguments(&body_block.terminator, header)?;
    let exit_arguments = edge_arguments(&body_block.terminator, exit)?;
    if backedge_arguments.len() != header_block.params.len()
        || exit_arguments.len() != eu.blocks[&exit].params.len()
    {
        return None;
    }
    let (trip_position, lanes) = match_count_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        latch_condition,
        loop_when_true,
    )?;
    if lanes < 4 {
        return None;
    }
    let index_position = match_index_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        trip_position,
    )?;
    let index = header_block.params[index_position];

    // The header chooses `(zero_extend(index), true)` until `found`, then
    // carries `(best, found)`.  Normalize an inverted branch orientation.
    let mut found_position = None;
    let mut best_position = None;
    for (position, (&parameter, &entry_argument)) in
        header_block.params.iter().zip(entry_arguments).enumerate()
    {
        if position == trip_position || position == index_position {
            continue;
        }
        if immediate(eu, definitions, entry_argument) == Some(0)
            && matches_not(eu, definitions, header_condition, parameter)
        {
            found_position = Some(position);
        }
    }
    let found_position = found_position?;
    let found = header_block.params[found_position];
    for position in 0..header_block.params.len() {
        if position == trip_position || position == index_position || position == found_position {
            continue;
        }
        let best = header_block.params[position];
        let first_matches = is_zero_extended_index(eu, definitions, first_jump[0], index)
            && immediate(eu, definitions, first_jump[1]) == Some(1)
            && carry_jump == [best, found];
        if first_matches {
            best_position = Some(position);
            break;
        }
    }
    let best_position = best_position?;
    let best = header_block.params[best_position];

    let mut count_position = None;
    let mut predicate = None;
    let mut count_update = None;
    let mut best_update = None;
    let mut found_update = None;
    for position in 0..header_block.params.len() {
        if position == trip_position || position == index_position {
            continue;
        }
        let current = header_block.params[position];
        let update = backedge_arguments[position];
        let Some(SIRInstruction::Mux(_, condition, on_true, on_false)) =
            instruction(eu, definitions, update)
        else {
            continue;
        };
        if *on_false != current {
            continue;
        }
        if position == best_position && *on_true == body_block.params[0] {
            predicate.get_or_insert(*condition);
            if predicate != Some(*condition) {
                return None;
            }
            best_update = Some(update);
        } else if position == found_position && *on_true == body_block.params[1] {
            predicate.get_or_insert(*condition);
            if predicate != Some(*condition) {
                return None;
            }
            found_update = Some(update);
        } else if immediate(eu, definitions, entry_arguments[position]) == Some(0)
            && is_add_one(eu, definitions, *on_true, current)
        {
            predicate.get_or_insert(*condition);
            if predicate != Some(*condition) || count_position.replace(position).is_some() {
                return None;
            }
            count_update = Some(update);
        }
    }
    let count_position = count_position?;
    let predicate_register = predicate?;
    let count_update = count_update?;
    let best_update = best_update?;
    let found_update = found_update?;
    let exit_count_position = exit_arguments
        .iter()
        .position(|&value| value == count_update)?;
    let exit_best_position = exit_arguments
        .iter()
        .position(|&value| value == best_update)?;
    let exit_found_position = exit_arguments
        .iter()
        .position(|&value| value == found_update)?;
    if [exit_count_position, exit_best_position, exit_found_position]
        .into_iter()
        .collect::<HashSet<_>>()
        .len()
        != 3
    {
        return None;
    }

    let predicate = build_packed_expression(
        eu,
        cfg,
        definitions,
        constant_cache,
        loop_blocks,
        preheader,
        index,
        predicate_register,
        false,
        lanes,
        bit_array_elements,
        array_shapes,
        &HashSet::default(),
    )?;
    if predicate.dynamic_loads == 0 {
        return None;
    }

    let count_type = eu.register_map[&header_block.params[count_position]].clone();
    let best_type = eu.register_map[&best].clone();
    let found_type = eu.register_map[&found].clone();
    let bit_count_width = UnaryOp::PopCount.result_width(lanes);
    if count_type.width() != bit_count_width
        || best_type.width() < bit_count_width
        || found_type.width() != 1
    {
        return None;
    }

    let mut loop_blocks = loop_blocks.iter().copied().collect::<Vec<_>>();
    loop_blocks.sort_unstable();
    Some(LinearBitmapScanPlan {
        preheader,
        loop_blocks,
        exit,
        exit_count_position,
        exit_best_position,
        exit_found_position,
        exit_arguments: exit_arguments.to_vec(),
        lanes,
        count_type,
        best_type,
        found_type,
        no_match_best: entry_arguments[best_position],
        no_match_found: entry_arguments[found_position],
        predicate,
    })
}

/// Recognize an ascending fixed-trip search which retains the payload from
/// every matching lane.  Since each match overwrites the previous payload,
/// the final state is exactly the payload of the highest set predicate bit.
#[allow(clippy::too_many_arguments)]
pub(super) fn recognize_last_payload_scan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    use_blocks: &HashMap<RegisterId, HashSet<BlockId>>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
) -> Option<LastPayloadScanPlan> {
    if loop_blocks.len() != 4 || !loop_is_pure(eu, loop_blocks) {
        return None;
    }
    let header_index = cfg.block_index(header)?;
    let outside_predecessors = cfg.predecessors[header_index]
        .iter()
        .map(|&block| cfg.block_ids[block])
        .filter(|block| !loop_blocks.contains(block))
        .collect::<Vec<_>>();
    let [preheader] = outside_predecessors.as_slice() else {
        return None;
    };
    let preheader = *preheader;
    let escaping_roots = loop_blocks
        .iter()
        .flat_map(|block| {
            eu.blocks[block]
                .params
                .iter()
                .copied()
                .chain(eu.blocks[block].instructions.iter().filter_map(def_reg))
        })
        .filter(|value| {
            use_blocks
                .get(value)
                .is_some_and(|users| users.iter().any(|user| !loop_blocks.contains(user)))
        })
        .collect::<HashSet<_>>();
    let hoisted_instructions = hoist_escaping_definitions(
        eu,
        cfg,
        preheader,
        loop_blocks,
        definitions,
        &escaping_roots,
    )?;
    let header_block = &eu.blocks[&header];
    let SIRTerminator::Jump(target, entry_arguments) = &eu.blocks[&preheader].terminator else {
        return None;
    };
    if *target != header || entry_arguments.len() != header_block.params.len() {
        return None;
    }
    let (predicate_register, selected_arm, carry_arm) = branch_targets(&header_block.terminator)?;
    let latch = common_jump_target(eu, selected_arm)?;
    if common_jump_target(eu, carry_arm)? != latch || !loop_blocks.contains(&latch) {
        return None;
    }
    let selected_arguments = edge_arguments(&eu.blocks[&selected_arm].terminator, latch)?;
    let carry_arguments = edge_arguments(&eu.blocks[&carry_arm].terminator, latch)?;
    let latch_block = &eu.blocks[&latch];
    if selected_arguments.len() != latch_block.params.len()
        || carry_arguments.len() != latch_block.params.len()
        || latch_block.params.is_empty()
    {
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
    {
        return None;
    }
    let (trip_position, lanes) = match_count_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        latch_condition,
        loop_when_true,
    )?;
    if lanes < 4 {
        return None;
    }
    let index_position = match_index_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        trip_position,
    )?;
    let index = header_block.params[index_position];

    let found_position = (0..header_block.params.len()).find(|&position| {
        if position == trip_position || position == index_position {
            return false;
        }
        let current = header_block.params[position];
        matches!(
            instruction(eu, definitions, backedge_arguments[position]),
            Some(SIRInstruction::Mux(_, condition, on_true, on_false))
                if *condition == predicate_register
                    && immediate(eu, definitions, *on_true) == Some(1)
                    && *on_false == current
                    && immediate(eu, definitions, entry_arguments[position]) == Some(0)
        )
    })?;
    let found_update = backedge_arguments[found_position];
    let exit_found_position = exit_arguments
        .iter()
        .position(|&argument| argument == found_update)?;

    let mut mapped_positions = HashSet::default();
    mapped_positions.extend([trip_position, index_position, found_position]);
    let mut used_exit_positions = HashSet::default();
    used_exit_positions.insert(exit_found_position);
    let mut payloads = Vec::with_capacity(latch_block.params.len());
    for (merge_position, &merge_parameter) in latch_block.params.iter().enumerate() {
        let header_position = (0..header_block.params.len()).find(|&position| {
            !mapped_positions.contains(&position)
                && backedge_arguments[position] == merge_parameter
                && carry_arguments[merge_position] == header_block.params[position]
        })?;
        let selected = selected_arguments[merge_position];
        let SIRInstruction::Load(
            _,
            address,
            SIROffset::Element {
                index: load_index,
                element_width,
                bit_offset,
                dynamic_bit_offset: None,
            },
            width,
        ) = instruction(eu, definitions, selected)?
        else {
            return None;
        };
        let shape = array_shapes.get(&address.absolute_addr())?;
        if *load_index != index
            || shape.element_width != *element_width
            || shape.element_count < lanes
            || *width == 0
            || bit_offset.checked_add(*width)? > *element_width
        {
            return None;
        }
        let exit_position = exit_arguments
            .iter()
            .position(|&argument| argument == merge_parameter)?;
        if !used_exit_positions.insert(exit_position) {
            return None;
        }
        let value_type = eu.register_map.get(&selected)?.clone();
        if eu.register_map.get(&header_block.params[header_position]) != Some(&value_type)
            || eu.register_map.get(&eu.blocks[&exit].params[exit_position]) != Some(&value_type)
        {
            return None;
        }
        mapped_positions.insert(header_position);
        payloads.push(PayloadField {
            exit_position,
            address: *address,
            element_width: *element_width,
            bit_offset: *bit_offset,
            width: *width,
            value_type,
            no_match: entry_arguments[header_position],
        });
    }
    if mapped_positions.len() != header_block.params.len() {
        return None;
    }

    let hoisted_values = hoisted_instructions
        .iter()
        .filter_map(def_reg)
        .collect::<HashSet<_>>();
    let predicate = build_packed_expression(
        eu,
        cfg,
        definitions,
        constant_cache,
        loop_blocks,
        preheader,
        index,
        predicate_register,
        false,
        lanes,
        bit_array_elements,
        array_shapes,
        &hoisted_values,
    )?;
    if predicate.dynamic_loads == 0
        || eu.register_map[&header_block.params[found_position]].width() != 1
    {
        return None;
    }
    let mut loop_blocks = loop_blocks.iter().copied().collect::<Vec<_>>();
    loop_blocks.sort_unstable();
    Some(LastPayloadScanPlan {
        preheader,
        loop_blocks,
        hoisted_instructions,
        exit,
        exit_arguments: exit_arguments.to_vec(),
        exit_found_position,
        found_type: eu.register_map[&header_block.params[found_position]].clone(),
        no_match_found: entry_arguments[found_position],
        lanes,
        predicate,
        payloads,
    })
}

pub(super) fn conjunction_nodes(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    root: RegisterId,
) -> HashSet<RegisterId> {
    let mut result = HashSet::default();
    let mut stack = vec![root];
    while let Some(value) = stack.pop() {
        if !result.insert(value) {
            continue;
        }
        match instruction(eu, definitions, value) {
            Some(SIRInstruction::Unary(
                _,
                UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or,
                source,
            )) if eu.register_map.get(source).map(RegisterType::width) == Some(1) => {
                stack.push(*source);
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd | BinaryOp::And, rhs)) => {
                stack.extend([*lhs, *rhs]);
            }
            _ => {}
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
pub(super) fn recognize_sparse_bitmap_loop(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    use_blocks: &HashMap<RegisterId, HashSet<BlockId>>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
) -> Option<SparseBitmapLoopPlan> {
    if loop_blocks.len() != 1 || !loop_is_pure(eu, loop_blocks) {
        return None;
    }
    let header_block = &eu.blocks[&header];
    // The rewrite lets the preheader branch straight to the exit, so the loop
    // no longer dominates it: values defined in the loop may leave it only as
    // exit arguments.
    if header_block
        .params
        .iter()
        .copied()
        .chain(header_block.instructions.iter().filter_map(def_reg))
        .any(|value| {
            use_blocks
                .get(&value)
                .is_some_and(|users| users.iter().any(|user| !loop_blocks.contains(user)))
        })
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
    if *target != header || entry_arguments.len() != header_block.params.len() {
        return None;
    }
    let (condition, true_target, false_target) = branch_targets(&header_block.terminator)?;
    let (exit, loop_when_true) = if true_target == header && false_target != header {
        (false_target, true)
    } else if false_target == header && true_target != header {
        (true_target, false)
    } else {
        return None;
    };
    let backedge_arguments = edge_arguments(&header_block.terminator, header)?;
    let exit_arguments = edge_arguments(&header_block.terminator, exit)?;
    if backedge_arguments.len() != header_block.params.len()
        || exit_arguments.len() != eu.blocks[&exit].params.len()
    {
        return None;
    }
    let (count_position, lanes) = match_count_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        condition,
        loop_when_true,
    )?;
    if !(16..=64).contains(&lanes) || !lanes.is_power_of_two() {
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
    let index = header_block.params[index_position];

    let mut update_conditions = Vec::new();
    let mut bypass_arguments = exit_arguments.to_vec();
    for position in 0..header_block.params.len() {
        if position == count_position || position == index_position {
            continue;
        }
        let current = header_block.params[position];
        let update = backedge_arguments[position];
        let SIRInstruction::Mux(_, update_condition, on_true, on_false) =
            instruction(eu, definitions, update)?
        else {
            return None;
        };
        if *on_false != current
            || immediate(eu, definitions, *on_true) != Some(1)
            || eu.register_map.get(&current).map(RegisterType::width) != Some(1)
        {
            return None;
        }
        update_conditions.push(*update_condition);
        let exit_position = exit_arguments
            .iter()
            .position(|&argument| argument == update)?;
        bypass_arguments[exit_position] = entry_arguments[position];
    }
    if update_conditions.is_empty()
        || bypass_arguments
            .iter()
            .zip(exit_arguments)
            .any(|(&bypass, &normal)| bypass == normal)
    {
        return None;
    }

    let mut common = conjunction_nodes(eu, definitions, update_conditions[0]);
    for &update_condition in &update_conditions[1..] {
        let nodes = conjunction_nodes(eu, definitions, update_condition);
        common.retain(|node| nodes.contains(node));
    }
    let mut best = None;
    for candidate in common {
        if !loop_blocks.contains(&definitions.get(&candidate)?.block()) {
            continue;
        }
        let Some(predicate) = build_packed_expression(
            eu,
            cfg,
            definitions,
            constant_cache,
            loop_blocks,
            preheader,
            index,
            candidate,
            false,
            lanes,
            bit_array_elements,
            array_shapes,
            &HashSet::default(),
        ) else {
            continue;
        };
        if predicate.dynamic_loads == 0 {
            continue;
        }
        let score = (predicate.dynamic_loads, predicate.value_ops);
        if best
            .as_ref()
            .is_none_or(|(_, best_predicate): &(RegisterId, PackedExpression)| {
                score > (best_predicate.dynamic_loads, best_predicate.value_ops)
            })
        {
            best = Some((candidate, predicate));
        }
    }
    let (common_predicate, predicate) = best?;
    Some(SparseBitmapLoopPlan {
        preheader,
        header,
        exit,
        lanes,
        count_position,
        index_position,
        entry_arguments: entry_arguments.to_vec(),
        backedge_arguments: backedge_arguments.to_vec(),
        exit_arguments: exit_arguments.to_vec(),
        bypass_arguments,
        common_predicate,
        predicate,
    })
}

pub(super) fn binary_definition(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    value: RegisterId,
    op: BinaryOp,
) -> Option<(RegisterId, RegisterId)> {
    let SIRInstruction::Binary(_, lhs, actual, rhs) = instruction(eu, definitions, value)? else {
        return None;
    };
    (*actual == op).then_some((*lhs, *rhs))
}

pub(super) fn commutative_operand(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    value: RegisterId,
    op: BinaryOp,
    operand: RegisterId,
) -> Option<RegisterId> {
    let (lhs, rhs) = binary_definition(eu, definitions, value, op)?;
    if lhs == operand {
        Some(rhs)
    } else if rhs == operand {
        Some(lhs)
    } else {
        None
    }
}

pub(super) fn match_inserted_bit(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    current: RegisterId,
    update: RegisterId,
) -> Option<(RegisterId, RegisterId)> {
    let (outer_lhs, outer_rhs) = binary_definition(eu, definitions, update, BinaryOp::Or)?;
    for (cleared, inserted) in [(outer_lhs, outer_rhs), (outer_rhs, outer_lhs)] {
        let (clear_lhs, clear_rhs) = binary_definition(eu, definitions, cleared, BinaryOp::And)?;
        let inverted = if clear_lhs == current {
            clear_rhs
        } else if clear_rhs == current {
            clear_lhs
        } else {
            continue;
        };
        let SIRInstruction::Unary(_, UnaryOp::BitNot, onehot) =
            instruction(eu, definitions, inverted)?
        else {
            continue;
        };
        let shifted = commutative_operand(eu, definitions, inserted, BinaryOp::And, *onehot)?;
        let (one, shift) = binary_definition(eu, definitions, *onehot, BinaryOp::Shl)?;
        if immediate(eu, definitions, one) != Some(1) {
            continue;
        }
        let (extended, inserted_shift) =
            binary_definition(eu, definitions, shifted, BinaryOp::Shl)?;
        if inserted_shift != shift {
            continue;
        }
        let bit = if eu.register_map.get(&extended).map(RegisterType::width) == Some(1) {
            extended
        } else {
            let SIRInstruction::Concat(_, parts) = instruction(eu, definitions, extended)? else {
                continue;
            };
            let (&bit, padding) = parts.split_last()?;
            if eu.register_map.get(&bit).map(RegisterType::width) != Some(1)
                || !padding
                    .iter()
                    .all(|&part| immediate(eu, definitions, part) == Some(0))
            {
                continue;
            }
            bit
        };
        return Some((shift, bit));
    }
    None
}

pub(super) fn shift_is_index(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    shift: RegisterId,
    index: RegisterId,
) -> bool {
    if shift == index || is_zero_extended_index(eu, definitions, shift, index) {
        return true;
    }
    binary_definition(eu, definitions, shift, BinaryOp::Mul).is_some_and(|(lhs, rhs)| {
        (lhs == index && immediate(eu, definitions, rhs) == Some(1))
            || (rhs == index && immediate(eu, definitions, lhs) == Some(1))
    })
}

pub(super) fn collect_bit_dependencies(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    definitions: &HashMap<RegisterId, Definition>,
    header: BlockId,
    preheader: BlockId,
    root: RegisterId,
    allowed_parameter: RegisterId,
    forbidden: &[RegisterId],
) -> Option<Vec<usize>> {
    let mut seen = HashSet::default();
    let mut stack = vec![root];
    while let Some(value) = stack.pop() {
        if forbidden.contains(&value) {
            return None;
        }
        if !seen.insert(value) {
            continue;
        }
        let definition = *definitions.get(&value)?;
        if definition.block() != header {
            if !cfg.dominates(definition.block(), preheader) {
                return None;
            }
            continue;
        }
        if value == allowed_parameter && matches!(definition, Definition::Parameter { .. }) {
            continue;
        }
        let Definition::Instruction { index, .. } = definition else {
            return None;
        };
        let instruction = &eu.blocks[&header].instructions[index];
        if instruction.has_side_effects() {
            return None;
        }
        let mut operands = Vec::new();
        instruction_uses(instruction, &mut operands);
        stack.extend(operands);
    }
    let mut indices = seen
        .into_iter()
        .filter_map(|value| match definitions.get(&value) {
            Some(Definition::Instruction { block, index }) if *block == header => Some(*index),
            _ => None,
        })
        .collect::<Vec<_>>();
    indices.sort_unstable();
    Some(indices)
}
