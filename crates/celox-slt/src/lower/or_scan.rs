//! Fixed-width first-true scan recognition and lowering.

use super::*;

fn slt_tree_reads_any_variable<A: Hash + Eq + Clone>(
    root: NodeId,
    variables: &[&A],
    arena: &SLTNodeArena<A>,
) -> bool {
    let mut visited = crate::HashSet::default();
    let mut work = vec![root];
    while let Some(node) = work.pop() {
        if !visited.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Input {
                variable, index, ..
            } => {
                if variables.contains(&variable) {
                    return true;
                }
                work.extend(index.iter().map(|entry| entry.node));
            }
            SLTNode::Constant(..) => {}
            SLTNode::Binary(lhs, _, rhs) => work.extend([*lhs, *rhs]),
            SLTNode::Unary(_, inner)
            | SLTNode::Capture { expr: inner, .. }
            | SLTNode::Slice { expr: inner, .. } => {
                work.push(*inner);
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => work.extend([*cond, *then_expr, *else_expr]),
            SLTNode::Concat(parts) => work.extend(parts.iter().map(|(part, _)| *part)),
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return true,
        }
    }
    false
}

fn slt_is_exact_state_input<A: Hash + Eq + Clone>(
    node: NodeId,
    state: &SLTForFoldGroupState<A>,
    arena: &SLTNodeArena<A>,
) -> bool {
    match arena.get(node) {
        SLTNode::Input {
            variable,
            index,
            access,
            ..
        } => variable == &state.target.id && index.is_empty() && access == &state.target.access,
        SLTNode::Unary(UnaryOp::Ident, inner) => slt_is_exact_state_input(*inner, state, arena),
        _ => false,
    }
}

pub(super) fn slt_scan_lane_bits(trip_count: usize) -> usize {
    let maximum = trip_count.saturating_sub(1);
    (usize::BITS as usize - maximum.leading_zeros() as usize).max(1)
}

fn slt_scan_domain_preserves_identity<A: Hash + Eq + Clone>(
    spec: &FoldGroupLowerSpec<'_, A>,
) -> bool {
    if spec.loop_width == 0
        || spec.start != &BigInt::from(0u8)
        || spec.step != &BigInt::from(1u8)
        || spec.trip_count == 0
    {
        return false;
    }

    // The matcher treats the induction value as the unsigned lane number.
    // A signed counter is equivalent on this finite domain only while every
    // value 0..trip_count-1 remains in its non-negative representable range.
    let maximum = spec.trip_count - 1;
    let required_bits = usize::BITS as usize - maximum.leading_zeros() as usize;
    let available_value_bits = spec.loop_width - usize::from(spec.loop_signed);
    required_bits <= available_value_bits
}

fn slt_scan_low_mask_preserves_domain<A: Hash + Eq + Clone>(
    node: NodeId,
    trip_count: usize,
    arena: &SLTNodeArena<A>,
) -> bool {
    let required_bits = slt_scan_lane_bits(trip_count);
    let required_mask = if required_bits >= 64 {
        u64::MAX
    } else {
        (1u64 << required_bits) - 1
    };
    slt_const_u64(node, arena).is_some_and(|mask| mask & required_mask == required_mask)
}

fn slt_is_scan_loop_value<A: Hash + Eq + Clone>(
    node: NodeId,
    spec: &FoldGroupLowerSpec<'_, A>,
    arena: &SLTNodeArena<A>,
) -> bool {
    match arena.get(node) {
        SLTNode::Input {
            variable,
            index,
            access,
            ..
        } => {
            variable == spec.loop_var
                && index.is_empty()
                && access.lsb == 0
                && access.msb + 1 == spec.loop_width
        }
        SLTNode::Unary(UnaryOp::Ident, inner) => slt_is_scan_loop_value(*inner, spec, arena),
        SLTNode::Slice { expr, access }
            if access.lsb == 0 && access.msb + 1 >= slt_scan_lane_bits(spec.trip_count) =>
        {
            slt_is_scan_loop_value(*expr, spec, arena)
        }
        SLTNode::Concat(parts) if !parts.is_empty() => {
            let (low, low_width) = parts.last().copied().expect("non-empty concat");
            low_width >= slt_scan_lane_bits(spec.trip_count)
                && slt_is_scan_loop_value(low, spec, arena)
                && parts[..parts.len() - 1]
                    .iter()
                    .all(|(part, _)| slt_const_u64(*part, arena) == Some(0))
        }
        SLTNode::Binary(lhs, BinaryOp::Add, rhs) => {
            slt_const_u64(*lhs, arena) == Some(0) && slt_is_scan_loop_value(*rhs, spec, arena)
                || slt_const_u64(*rhs, arena) == Some(0)
                    && slt_is_scan_loop_value(*lhs, spec, arena)
        }
        SLTNode::Binary(lhs, BinaryOp::Mul, rhs) => {
            slt_const_u64(*lhs, arena) == Some(1) && slt_is_scan_loop_value(*rhs, spec, arena)
                || slt_const_u64(*rhs, arena) == Some(1)
                    && slt_is_scan_loop_value(*lhs, spec, arena)
        }
        // Analyzer casts of a non-negative unrolled IV commonly survive as
        // `iv & low_mask`.  It is still the identity over this exact finite
        // trip domain iff every bit needed to represent `0..trip_count` is
        // retained.  Reject masks that drop even one such bit.
        SLTNode::Binary(lhs, BinaryOp::And, rhs) => {
            slt_scan_low_mask_preserves_domain(*lhs, spec.trip_count, arena)
                && slt_is_scan_loop_value(*rhs, spec, arena)
                || slt_scan_low_mask_preserves_domain(*rhs, spec.trip_count, arena)
                    && slt_is_scan_loop_value(*lhs, spec, arena)
        }
        _ => false,
    }
}

fn match_slt_scan_indexed_input<A: Hash + Eq + Clone>(
    variable: &A,
    index: &[crate::SLTIndex],
    input_access: BitAccess,
    spec: &FoldGroupLowerSpec<'_, A>,
    state_variables: &[&A],
    arena: &SLTNodeArena<A>,
) -> Option<SLTVectorExpr<A>> {
    let [entry] = index else {
        return None;
    };
    if entry.stride != 1
        || variable == spec.loop_var
        || state_variables.contains(&variable)
        || !slt_is_scan_loop_value(entry.node, spec, arena)
    {
        return None;
    }
    let packed_access = if input_access == BitAccess::new(0, 0) {
        // A direct narrow indexed input denotes `variable[iv]`; the complete
        // identity traversal therefore reconstructs bits `0..trip_count-1`.
        BitAccess::new(0, spec.trip_count - 1)
    } else if input_access.lsb == 0 && input_access.msb + 1 == spec.trip_count {
        input_access
    } else {
        return None;
    };
    let unpacked_element_width = match entry.kind {
        crate::SLTIndexKind::Unpacked { element_width } => Some(element_width),
        crate::SLTIndexKind::Packed => None,
    };
    Some(SLTVectorExpr::StaticInput {
        variable: variable.clone(),
        access: packed_access,
        unpacked_element_width,
    })
}

fn match_slt_scan_indexed_bit<A: Hash + Eq + Clone>(
    node: NodeId,
    spec: &FoldGroupLowerSpec<'_, A>,
    state_variables: &[&A],
    arena: &SLTNodeArena<A>,
) -> Option<SLTVectorExpr<A>> {
    match arena.get(node) {
        SLTNode::Unary(UnaryOp::Ident, inner) => {
            match_slt_scan_indexed_bit(*inner, spec, state_variables, arena)
        }
        SLTNode::Input {
            variable,
            index,
            access,
            ..
        } if *access == BitAccess::new(0, 0) => {
            match_slt_scan_indexed_input(variable, index, *access, spec, state_variables, arena)
        }
        SLTNode::Slice { expr, access } if *access == BitAccess::new(0, 0) => {
            let SLTNode::Input {
                variable,
                index,
                access: input_access,
                ..
            } = arena.get(*expr)
            else {
                return None;
            };
            match_slt_scan_indexed_input(
                variable,
                index,
                *input_access,
                spec,
                state_variables,
                arena,
            )
        }
        _ => None,
    }
}

fn lift_slt_scan_lane_expr<A: Hash + Eq + Clone>(
    node: NodeId,
    spec: &FoldGroupLowerSpec<'_, A>,
    state_variables: &[&A],
    arena: &SLTNodeArena<A>,
) -> Option<SLTVectorExpr<A>> {
    if let Some(input) = match_slt_scan_indexed_bit(node, spec, state_variables, arena) {
        return Some(input);
    }
    // Procedural control normalizes a condition as ToTwoState(|cond).  The
    // word-scan plan is emitted only in two-state mode, so that pair is an
    // identity when the original condition is already one bit.  Look through
    // exactly that shape; a reduction of a wider condition is not lane-wise.
    if let SLTNode::Unary(UnaryOp::ToTwoState, truth) = arena.get(node)
        && let SLTNode::Unary(UnaryOp::Or, inner) = arena.get(*truth)
        && slt_width(*inner, arena) == 1
    {
        return lift_slt_scan_lane_expr(*inner, spec, state_variables, arena);
    }
    let mut forbidden = Vec::with_capacity(state_variables.len() + 1);
    forbidden.push(spec.loop_var);
    forbidden.extend_from_slice(state_variables);
    if slt_width(node, arena) == 1 && !slt_tree_reads_any_variable(node, &forbidden, arena) {
        return Some(SLTVectorExpr::Broadcast(node));
    }
    match arena.get(node) {
        SLTNode::Binary(index, BinaryOp::LtU, bound)
            if slt_is_scan_loop_value(*index, spec, arena)
                && !slt_tree_reads_any_variable(*bound, &forbidden, arena) =>
        {
            Some(SLTVectorExpr::LowOnes { bound: *bound })
        }
        SLTNode::Binary(lhs, op, rhs) => {
            let op = normalized_slt_lane_op(*op)?;
            Some(SLTVectorExpr::Binary {
                lhs: Box::new(lift_slt_scan_lane_expr(*lhs, spec, state_variables, arena)?),
                op,
                rhs: Box::new(lift_slt_scan_lane_expr(*rhs, spec, state_variables, arena)?),
            })
        }
        SLTNode::Unary(UnaryOp::LogicNot | UnaryOp::BitNot, inner) => {
            Some(SLTVectorExpr::Not(Box::new(lift_slt_scan_lane_expr(
                *inner,
                spec,
                state_variables,
                arena,
            )?)))
        }
        _ => None,
    }
}

fn slt_binary_operands<A: Hash + Eq + Clone>(
    node: NodeId,
    op: BinaryOp,
    arena: &SLTNodeArena<A>,
) -> Option<(NodeId, NodeId)> {
    let SLTNode::Binary(lhs, actual, rhs) = arena.get(node) else {
        return None;
    };
    (*actual == op).then_some((*lhs, *rhs))
}

fn slt_matches_commutative_pair<A: Hash + Eq + Clone>(
    node: NodeId,
    ops: &[BinaryOp],
    lhs: NodeId,
    rhs: NodeId,
    arena: &SLTNodeArena<A>,
) -> bool {
    matches!(
        arena.get(node),
        SLTNode::Binary(actual_lhs, op, actual_rhs)
            if ops.contains(op)
                && ((*actual_lhs == lhs && *actual_rhs == rhs)
                    || (*actual_lhs == rhs && *actual_rhs == lhs))
    )
}

fn match_slt_scan_found_update<A: Hash + Eq + Clone>(
    state: &SLTForFoldGroupState<A>,
    arena: &SLTNodeArena<A>,
) -> Option<(NodeId, NodeId, NodeId)> {
    if state.target.access != BitAccess::new(0, 0) || slt_const_u64(state.initial, arena) != Some(0)
    {
        return None;
    }
    let SLTNode::Mux {
        cond,
        then_expr,
        else_expr,
    } = arena.get(state.update)
    else {
        return None;
    };
    if !slt_is_exact_state_input(*else_expr, state, arena) {
        return None;
    }
    let SLTNode::Binary(lhs, BinaryOp::Or | BinaryOp::LogicOr, rhs) = arena.get(*then_expr) else {
        return None;
    };
    let source = if slt_is_exact_state_input(*lhs, state, arena) {
        *rhs
    } else if slt_is_exact_state_input(*rhs, state, arena) {
        *lhs
    } else {
        return None;
    };
    (slt_width(source, arena) == 1).then_some((*cond, source, *else_expr))
}

fn match_slt_scan_offset<A: Hash + Eq + Clone>(
    node: NodeId,
    spec: &FoldGroupLowerSpec<'_, A>,
    arena: &SLTNodeArena<A>,
) -> bool {
    slt_is_scan_loop_value(node, spec, arena)
}

fn match_slt_scan_zext_bit<A: Hash + Eq + Clone>(
    node: NodeId,
    width: usize,
    arena: &SLTNodeArena<A>,
) -> Option<NodeId> {
    if width == 1 && slt_width(node, arena) == 1 {
        return Some(node);
    }
    let SLTNode::Concat(parts) = arena.get(node) else {
        return None;
    };
    let (bit, bit_width) = parts.last().copied()?;
    (bit_width == 1
        && slt_width(bit, arena) == 1
        && parts
            .iter()
            .map(|(_, part_width)| *part_width)
            .sum::<usize>()
            == width
        && parts[..parts.len() - 1]
            .iter()
            .all(|(part, _)| slt_const_u64(*part, arena) == Some(0)))
    .then_some(bit)
}

fn match_slt_scan_insert<A: Hash + Eq + Clone>(
    node: NodeId,
    old: NodeId,
    width: usize,
    spec: &FoldGroupLowerSpec<'_, A>,
    arena: &SLTNodeArena<A>,
) -> Option<NodeId> {
    let (lhs, rhs) = slt_binary_operands(node, BinaryOp::Or, arena)?;
    for (old_masked, new_masked) in [(lhs, rhs), (rhs, lhs)] {
        let (old_lhs, old_rhs) = slt_binary_operands(old_masked, BinaryOp::And, arena)?;
        let inverted_mask = if old_lhs == old {
            old_rhs
        } else if old_rhs == old {
            old_lhs
        } else {
            continue;
        };
        let SLTNode::Unary(UnaryOp::BitNot, mask) = arena.get(inverted_mask) else {
            continue;
        };
        let SLTNode::Binary(one, BinaryOp::Shl, offset) = arena.get(*mask) else {
            continue;
        };
        if slt_width(*mask, arena) != width
            || slt_const_u64(*one, arena) != Some(1)
            || slt_width(*one, arena) != width
            || !match_slt_scan_offset(*offset, spec, arena)
        {
            continue;
        }
        let (new_lhs, new_rhs) = slt_binary_operands(new_masked, BinaryOp::And, arena)?;
        let shifted = if new_lhs == *mask {
            new_rhs
        } else if new_rhs == *mask {
            new_lhs
        } else {
            continue;
        };
        let SLTNode::Binary(value, BinaryOp::Shl, value_offset) = arena.get(shifted) else {
            continue;
        };
        if value_offset != offset {
            continue;
        }
        if let Some(bit) = match_slt_scan_zext_bit(*value, width, arena) {
            return Some(bit);
        }
    }
    None
}

fn match_slt_scan_mode_test<A: Hash + Eq + Clone>(
    node: NodeId,
    expected: u64,
    forbidden: &[&A],
    arena: &SLTNodeArena<A>,
) -> Option<NodeId> {
    let SLTNode::Binary(lhs, BinaryOp::Eq | BinaryOp::EqWildcard, rhs) = arena.get(node) else {
        return None;
    };
    let mode = if slt_const_u64(*lhs, arena) == Some(expected) {
        *rhs
    } else if slt_const_u64(*rhs, arena) == Some(expected) {
        *lhs
    } else {
        return None;
    };
    (slt_width(mode, arena) == 2 && !slt_tree_reads_any_variable(mode, forbidden, arena))
        .then_some(mode)
}

fn match_slt_scan_selected_bit<A: Hash + Eq + Clone>(
    node: NodeId,
    found: NodeId,
    source: NodeId,
    forbidden: &[&A],
    arena: &SLTNodeArena<A>,
) -> Option<(NodeId, NodeId)> {
    let not_found = match_slt_boolean_not(node, arena).filter(|inner| *inner == found);
    if not_found.is_some() {
        return None;
    }
    let SLTNode::Mux {
        cond: before_cond,
        then_expr: before,
        else_expr,
    } = arena.get(node)
    else {
        return None;
    };
    let SLTNode::Mux {
        cond: first_cond,
        then_expr: first,
        else_expr: through,
    } = arena.get(*else_expr)
    else {
        return None;
    };
    let not_found = match_slt_boolean_not(*through, arena)?;
    if not_found != found {
        return None;
    }
    let before_matches = match arena.get(*before) {
        SLTNode::Binary(lhs, BinaryOp::And | BinaryOp::LogicAnd, rhs) => {
            (match_slt_boolean_not(*lhs, arena) == Some(found)
                && match_slt_boolean_not(*rhs, arena) == Some(source))
                || (match_slt_boolean_not(*rhs, arena) == Some(found)
                    && match_slt_boolean_not(*lhs, arena) == Some(source))
        }
        _ => false,
    };
    if !before_matches
        || !slt_matches_commutative_pair(
            *first,
            &[BinaryOp::And, BinaryOp::LogicAnd],
            *through,
            source,
            arena,
        )
    {
        return None;
    }
    let before_mode = match_slt_scan_mode_test(*before_cond, 1, forbidden, arena)?;
    let first_mode = match_slt_scan_mode_test(*first_cond, 2, forbidden, arena)?;
    (before_mode == first_mode).then_some((*before_cond, *first_cond))
}

pub(super) fn match_slt_or_scan_plan<A: Hash + Eq + Clone>(
    spec: &FoldGroupLowerSpec<'_, A>,
    arena: &SLTNodeArena<A>,
) -> Option<SLTOrScanPlan<A>> {
    if !slt_scan_domain_preserves_identity(spec) || spec.states.len() != 2 {
        return None;
    }
    let state_variables = spec
        .states
        .iter()
        .map(|state| &state.target.id)
        .collect::<Vec<_>>();
    for (found_state, found) in spec.states.iter().enumerate() {
        let Some((active, source, old_found)) = match_slt_scan_found_update(found, arena) else {
            continue;
        };
        let vector_state = 1 - found_state;
        let vector = &spec.states[vector_state];
        let width = vector.target.access.msb - vector.target.access.lsb + 1;
        if vector.target.access.lsb != 0
            || width != spec.trip_count
            || slt_width(vector.initial, arena) != width
        {
            continue;
        }
        let SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } = arena.get(vector.update)
        else {
            continue;
        };
        if *cond != active || !slt_is_exact_state_input(*else_expr, vector, arena) {
            continue;
        }
        let Some(new_bit) = match_slt_scan_insert(*then_expr, *else_expr, width, spec, arena)
        else {
            continue;
        };
        let mut forbidden = Vec::with_capacity(state_variables.len() + 1);
        forbidden.push(spec.loop_var);
        forbidden.extend(state_variables.iter().copied());
        let Some((select_before, select_first)) =
            match_slt_scan_selected_bit(new_bit, old_found, source, &forbidden, arena)
        else {
            continue;
        };
        let active = lift_slt_scan_lane_expr(active, spec, &state_variables, arena)?;
        let source = match_slt_scan_indexed_bit(source, spec, &state_variables, arena)?;
        return Some(SLTOrScanPlan {
            vector_state,
            found_state,
            width,
            active,
            source,
            select_before,
            select_first,
        });
    }
    None
}

pub fn matches_slt_or_scan_group<A: Hash + Eq + Clone>(
    root: NodeId,
    arena: &SLTNodeArena<A>,
) -> bool {
    FoldGroupLowerSpec::from_root(root, arena)
        .and_then(|spec| match_slt_or_scan_plan(&spec, arena))
        .is_some()
}

impl SLTToSIRLowerer {
    pub(super) fn lower_or_scan_plan<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        spec: &FoldGroupLowerSpec<'_, A>,
        plan: SLTOrScanPlan<A>,
        allow_cache: bool,
    ) -> RegisterId {
        debug_assert!(!self.four_state);
        let initial_states = spec
            .states
            .iter()
            .map(|state| {
                let initial =
                    self.lower_inner(builder, state.initial, arena, cache, None, allow_cache);
                self.cast_reg_width(
                    builder,
                    initial,
                    state.target.access.msb - state.target.access.lsb + 1,
                )
            })
            .collect::<Vec<_>>();
        let guard = self.lower_inner(builder, spec.entry_guard, arena, cache, None, allow_cache);
        let active =
            self.lower_slt_vector_expr(builder, plan.active, plan.width, arena, cache, allow_cache);
        let source =
            self.lower_slt_vector_expr(builder, plan.source, plan.width, arena, cache, allow_cache);

        let hits = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Binary(hits, active, BinaryOp::And, source));
        let zero = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        let negated_hits = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Binary(
            negated_hits,
            zero,
            BinaryOp::Sub,
            hits,
        ));
        let first = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Binary(
            first,
            hits,
            BinaryOp::And,
            negated_hits,
        ));
        let one = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        let before = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Binary(before, first, BinaryOp::Sub, one));
        // `before | first` is true exactly through the first hit.  It is all
        // ones when `hits` is zero, matching the sequential `!found` state.
        let through = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Binary(through, before, BinaryOp::Or, first));

        let not_source = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Unary(not_source, UnaryOp::BitNot, source));
        let before_bits = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Binary(
            before_bits,
            through,
            BinaryOp::And,
            not_source,
        ));
        let first_bits = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Binary(
            first_bits,
            through,
            BinaryOp::And,
            source,
        ));
        let select_first =
            self.lower_inner(builder, plan.select_first, arena, cache, None, allow_cache);
        let first_or_through = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Mux(
            first_or_through,
            select_first,
            first_bits,
            through,
        ));
        let select_before =
            self.lower_inner(builder, plan.select_before, arena, cache, None, allow_cache);
        let selected = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Mux(
            selected,
            select_before,
            before_bits,
            first_or_through,
        ));

        let not_active = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Unary(not_active, UnaryOp::BitNot, active));
        let preserved = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Binary(
            preserved,
            initial_states[plan.vector_state],
            BinaryOp::And,
            not_active,
        ));
        let replaced = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Binary(
            replaced,
            selected,
            BinaryOp::And,
            active,
        ));
        let vector_result = builder.alloc_bit(plan.width, false);
        builder.emit(SIRInstruction::Binary(
            vector_result,
            preserved,
            BinaryOp::Or,
            replaced,
        ));
        let found_result = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Unary(found_result, UnaryOp::Or, hits));

        let mut candidates = initial_states.clone();
        candidates[plan.vector_state] = vector_result;
        candidates[plan.found_state] = found_result;
        let final_states = candidates
            .into_iter()
            .zip(initial_states)
            .zip(spec.states)
            .map(
                |((candidate, initial), state)| match slt_const_u64(spec.entry_guard, arena) {
                    Some(0) => initial,
                    Some(_) => candidate,
                    None => {
                        let result = builder
                            .alloc_logic(state.target.access.msb - state.target.access.lsb + 1);
                        builder.emit(SIRInstruction::Mux(result, guard, candidate, initial));
                        result
                    }
                },
            )
            .collect::<Vec<_>>();
        self.pack_fold_group_states(builder, &final_states)
    }
}
