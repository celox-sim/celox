//! Count-idiom recognition, predicate compaction, and count-operation lowering.

use super::*;

pub(super) fn slt_const_u64<A: Hash + Eq + Clone>(
    node: NodeId,
    arena: &SLTNodeArena<A>,
) -> Option<u64> {
    let (value, mask) = try_const_eval(node, arena)?;
    if mask != BigUint::from(0u8) {
        return None;
    }
    match value.to_u64_digits().as_slice() {
        [] => Some(0),
        [value] => Some(*value),
        _ => None,
    }
}

fn slt_literal_u64<A: Hash + Eq + Clone>(node: NodeId, arena: &SLTNodeArena<A>) -> Option<u64> {
    let SLTNode::Constant(value, mask, _, _) = arena.get(node) else {
        return None;
    };
    if mask != &BigUint::from(0u8) {
        return None;
    }
    match value.to_u64_digits().as_slice() {
        [] => Some(0),
        [value] => Some(*value),
        _ => None,
    }
}

pub(super) fn slt_width<A: Hash + Eq + Clone>(node: NodeId, arena: &SLTNodeArena<A>) -> usize {
    crate::get_width(node, arena)
}

/// Procedural control represents truth as `ToTwoState(Or(cond))`. Count-idiom
/// lowering is enabled only in two-state mode, where that exact pair is an
/// identity for a one-bit `cond`. Do not look through a real wide reduction.
pub(super) fn unwrap_slt_one_bit_procedural_truth<A: Hash + Eq + Clone>(
    node: NodeId,
    arena: &SLTNodeArena<A>,
) -> NodeId {
    if let SLTNode::Unary(UnaryOp::ToTwoState, truth) = arena.get(node)
        && let SLTNode::Unary(UnaryOp::Or, inner) = arena.get(*truth)
        && slt_width(*inner, arena) == 1
    {
        *inner
    } else {
        node
    }
}

fn slt_literal_zero_of_width<A: Hash + Eq + Clone>(
    node: NodeId,
    width: usize,
    arena: &SLTNodeArena<A>,
) -> bool {
    slt_width(node, arena) == width && slt_literal_u64(node, arena) == Some(0)
}

fn slt_width_can_represent(width: usize, maximum: usize) -> bool {
    width >= usize::BITS as usize || maximum < (1usize << width)
}

fn resolve_slt_bit_origin<A: Hash + Eq + Clone>(
    node: NodeId,
    arena: &SLTNodeArena<A>,
) -> Option<(SLTBitOrigin<A>, usize)> {
    let node = unwrap_slt_one_bit_procedural_truth(node, arena);
    match arena.get(node) {
        SLTNode::Input {
            variable,
            signed,
            index,
            access,
        } if access.msb == access.lsb => Some((
            SLTBitOrigin::Input {
                node,
                variable: variable.clone(),
                signed: *signed,
                index: index.clone(),
            },
            access.lsb,
        )),
        SLTNode::Slice { expr, access } if access.msb == access.lsb => {
            Some((SLTBitOrigin::Node(*expr), access.lsb))
        }
        SLTNode::Unary(UnaryOp::Ident, inner) => resolve_slt_bit_origin(*inner, arena),
        SLTNode::Binary(lhs, BinaryOp::Eq, rhs) => {
            if slt_const_u64(*lhs, arena) == Some(1) {
                resolve_slt_bit_origin(*rhs, arena)
            } else if slt_const_u64(*rhs, arena) == Some(1) {
                resolve_slt_bit_origin(*lhs, arena)
            } else {
                None
            }
        }
        SLTNode::Binary(lhs, BinaryOp::And, rhs) => {
            let shifted = if slt_const_u64(*lhs, arena) == Some(1) {
                *rhs
            } else if slt_const_u64(*rhs, arena) == Some(1) {
                *lhs
            } else {
                return None;
            };
            match arena.get(shifted) {
                SLTNode::Binary(source, BinaryOp::Shr, amount) => Some((
                    SLTBitOrigin::Node(*source),
                    slt_const_u64(*amount, arena)? as usize,
                )),
                _ if slt_width(shifted, arena) == 1 => Some((SLTBitOrigin::Node(shifted), 0)),
                _ => None,
            }
        }
        _ => None,
    }
}

pub(super) fn resolve_slt_extended_bit<A: Hash + Eq + Clone>(
    node: NodeId,
    arena: &SLTNodeArena<A>,
) -> Option<SLTBitTerm<A>> {
    let node = unwrap_slt_one_bit_procedural_truth(node, arena);
    if slt_width(node, arena) == 1 {
        return Some(SLTBitTerm {
            predicate: node,
            origin: resolve_slt_bit_origin(node, arena),
        });
    }
    match arena.get(node) {
        SLTNode::Unary(UnaryOp::Ident, inner) => resolve_slt_extended_bit(*inner, arena),
        SLTNode::Concat(parts) => {
            // A numeric conditional increment must be exactly a zero-extended
            // bit in the LSB position.  Merely finding one nonzero one-bit
            // part is insufficient: `{bit, 0...}` contributes 2^K, not 1.
            let (least_significant, leading) = parts.split_last()?;
            if leading
                .iter()
                .any(|(part, _)| slt_literal_u64(*part, arena) != Some(0))
            {
                return None;
            }
            resolve_slt_extended_bit(least_significant.0, arena)
        }
        _ => None,
    }
}

fn common_complete_slt_origin<A: Hash + Eq + Clone>(
    terms: &[SLTBitTerm<A>],
    arena: &SLTNodeArena<A>,
) -> Option<SLTBitOrigin<A>> {
    let (origin, _) = terms.first()?.origin.clone()?;
    let width = terms.len();
    if let SLTBitOrigin::Node(node) = origin
        && slt_width(node, arena) != width
    {
        return None;
    }
    let mut seen = vec![false; width];
    for term in terms {
        let (term_origin, bit) = term.origin.as_ref()?;
        if *term_origin != origin || *bit >= width || seen[*bit] {
            return None;
        }
        seen[*bit] = true;
    }
    Some(origin)
}

pub(super) fn normalized_slt_lane_op(op: BinaryOp) -> Option<BinaryOp> {
    match op {
        BinaryOp::And | BinaryOp::LogicAnd => Some(BinaryOp::And),
        BinaryOp::Or | BinaryOp::LogicOr => Some(BinaryOp::Or),
        BinaryOp::Xor => Some(BinaryOp::Xor),
        _ => None,
    }
}

fn compact_slt_predicate_nodes<A: Hash + Eq + Clone>(
    nodes: &[NodeId],
    arena: &SLTNodeArena<A>,
) -> Option<SLTVectorExpr<A>> {
    let width = nodes.len();
    if width == 0 || nodes.iter().any(|node| slt_width(*node, arena) != 1) {
        return None;
    }

    if nodes.iter().all(|node| *node == nodes[0]) {
        return Some(SLTVectorExpr::Broadcast(nodes[0]));
    }

    let mut common_origin = None;
    let mut origin_matches = true;
    for (concat_index, node) in nodes.iter().copied().enumerate() {
        let Some((origin, bit)) = resolve_slt_bit_origin(node, arena) else {
            origin_matches = false;
            break;
        };
        if bit != width - 1 - concat_index {
            origin_matches = false;
            break;
        }
        if let Some(previous) = &common_origin {
            if previous != &origin {
                origin_matches = false;
                break;
            }
        } else {
            common_origin = Some(origin);
        }
    }
    if origin_matches {
        let origin = common_origin?;
        if !matches!(&origin, SLTBitOrigin::Node(node) if slt_width(*node, arena) != width) {
            return Some(SLTVectorExpr::Origin(origin));
        }
    }

    // `{(W-1 < bound), ..., (0 < bound)}` is the saturated low-ones mask
    // `(1_W << bound) - 1`.  Native shift legalization defines shifts by W or
    // more as zero, so the expression also produces all ones for bound >= W.
    let mut bound = None;
    let mut is_low_ones = true;
    for (concat_index, node) in nodes.iter().copied().enumerate() {
        let SLTNode::Binary(index, BinaryOp::LtU, lane_bound) = arena.get(node) else {
            is_low_ones = false;
            break;
        };
        if slt_const_u64(*index, arena) != Some((width - 1 - concat_index) as u64)
            || slt_width(*index, arena) != slt_width(*lane_bound, arena)
        {
            is_low_ones = false;
            break;
        }
        if bound.is_some_and(|previous| previous != *lane_bound) {
            is_low_ones = false;
            break;
        }
        bound = Some(*lane_bound);
    }
    if is_low_ones {
        return Some(SLTVectorExpr::LowOnes { bound: bound? });
    }

    let mut op = None;
    let mut lhs_nodes = Vec::with_capacity(width);
    let mut rhs_nodes = Vec::with_capacity(width);
    for node in nodes {
        let SLTNode::Binary(lhs, lane_op, rhs) = arena.get(*node) else {
            return None;
        };
        let lane_op = normalized_slt_lane_op(*lane_op)?;
        if op.is_some_and(|previous| previous != lane_op) {
            return None;
        }
        op = Some(lane_op);
        lhs_nodes.push(*lhs);
        rhs_nodes.push(*rhs);
    }
    Some(SLTVectorExpr::Binary {
        lhs: Box::new(compact_slt_predicate_nodes(&lhs_nodes, arena)?),
        op: op?,
        rhs: Box::new(compact_slt_predicate_nodes(&rhs_nodes, arena)?),
    })
}

fn compact_slt_predicates<A: Hash + Eq + Clone>(
    predicates: &[SLTCountPredicate],
    arena: &SLTNodeArena<A>,
) -> Option<SLTVectorExpr<A>> {
    if predicates
        .iter()
        .all(|predicate| matches!(predicate, SLTCountPredicate::Node(_)))
    {
        let nodes = predicates
            .iter()
            .map(|predicate| match predicate {
                SLTCountPredicate::Node(node) => *node,
                SLTCountPredicate::And(..) => unreachable!(),
            })
            .collect::<Vec<_>>();
        return compact_slt_predicate_nodes(&nodes, arena);
    }
    if predicates
        .iter()
        .all(|predicate| matches!(predicate, SLTCountPredicate::And(..)))
    {
        let mut lhs = Vec::with_capacity(predicates.len());
        let mut rhs = Vec::with_capacity(predicates.len());
        for predicate in predicates {
            let SLTCountPredicate::And(lane_lhs, lane_rhs) = predicate else {
                unreachable!();
            };
            lhs.push(*lane_lhs);
            rhs.push(*lane_rhs);
        }
        return Some(SLTVectorExpr::Binary {
            lhs: Box::new(compact_slt_predicate_nodes(&lhs, arena)?),
            op: BinaryOp::And,
            rhs: Box::new(compact_slt_predicate_nodes(&rhs, arena)?),
        });
    }
    None
}

fn match_slt_increment<A: Hash + Eq + Clone>(
    value: NodeId,
    accumulator: NodeId,
    arena: &SLTNodeArena<A>,
) -> bool {
    let SLTNode::Binary(lhs, BinaryOp::Add, rhs) = arena.get(value) else {
        return false;
    };
    *lhs == accumulator && slt_literal_u64(*rhs, arena) == Some(1)
        || *rhs == accumulator && slt_literal_u64(*lhs, arena) == Some(1)
}

fn collect_slt_conditional_increments<A: Hash + Eq + Clone>(
    mut cursor: NodeId,
    accumulator_width: usize,
    arena: &SLTNodeArena<A>,
    materialized: Option<&crate::HashMap<NodeId, RegisterId>>,
) -> Option<(Vec<SLTBitTerm<A>>, Option<NodeId>)> {
    let mut terms = Vec::new();
    loop {
        // Only reuse the immediate predecessor.  A longer partial suffix can
        // destroy a profitable whole-vector count shape; one exact +1 delta
        // is always the recurrence edge we are replacing.
        if terms.len() == 1
            && materialized.is_some_and(|cache| cache.contains_key(&cursor))
            && slt_width(cursor, arena) == accumulator_width
        {
            return Some((terms, Some(cursor)));
        }
        let SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } = arena.get(cursor)
        else {
            return None;
        };
        if slt_width(cursor, arena) != accumulator_width
            || !match_slt_increment(*then_expr, *else_expr, arena)
            || slt_width(*cond, arena) != 1
        {
            return None;
        }
        let cond = unwrap_slt_one_bit_procedural_truth(*cond, arena);
        terms.push(SLTBitTerm {
            predicate: cond,
            origin: resolve_slt_bit_origin(cond, arena),
        });
        cursor = *else_expr;
        if slt_literal_zero_of_width(cursor, accumulator_width, arena) {
            break;
        }
    }
    Some((terms, None))
}

fn collect_slt_additive_bits<A: Hash + Eq + Clone>(
    mut cursor: NodeId,
    accumulator_width: usize,
    arena: &SLTNodeArena<A>,
    materialized: Option<&crate::HashMap<NodeId, RegisterId>>,
) -> Option<(Vec<SLTBitTerm<A>>, Option<NodeId>)> {
    let mut terms = Vec::new();
    loop {
        // See the conditional form above: a materialized immediate
        // predecessor is an exact delta edge, not an arbitrary split point.
        if terms.len() == 1
            && materialized.is_some_and(|cache| cache.contains_key(&cursor))
            && slt_width(cursor, arena) == accumulator_width
        {
            return Some((terms, Some(cursor)));
        }
        if slt_literal_zero_of_width(cursor, accumulator_width, arena) {
            break;
        }
        let SLTNode::Binary(lhs, BinaryOp::Add, rhs) = arena.get(cursor) else {
            return None;
        };
        if slt_width(cursor, arena) != accumulator_width {
            return None;
        }
        let lhs_term = resolve_slt_extended_bit(*lhs, arena);
        let rhs_term = resolve_slt_extended_bit(*rhs, arena);
        match (lhs_term, rhs_term) {
            (Some(term), None) => {
                terms.push(term);
                cursor = *rhs;
            }
            (None, Some(term)) => {
                terms.push(term);
                cursor = *lhs;
            }
            _ => return None,
        }
    }
    Some((terms, None))
}

fn match_slt_popcount<A: Hash + Eq + Clone>(
    root: NodeId,
    arena: &SLTNodeArena<A>,
    materialized: Option<&crate::HashMap<NodeId, RegisterId>>,
) -> Option<SLTCountPlan<A>> {
    let result_width = slt_width(root, arena);
    let (terms, base) = match arena.get(root) {
        SLTNode::Mux { .. } => {
            collect_slt_conditional_increments(root, result_width, arena, materialized)?
        }
        SLTNode::Binary(_, BinaryOp::Add, _) => {
            collect_slt_additive_bits(root, result_width, arena, materialized)?
        }
        _ => return None,
    };
    let minimum_terms = if base.is_some() { 1 } else { 4 };
    if terms.len() < minimum_terms || !slt_width_can_represent(result_width, terms.len()) {
        return None;
    }
    let input_width = terms.len();
    let input = if let Some(origin) = common_complete_slt_origin(&terms, arena) {
        SLTCountInput::Origin(origin)
    } else {
        let predicates = terms
            .into_iter()
            .map(|term| term.predicate)
            .collect::<Vec<_>>();
        compact_slt_predicate_nodes(&predicates, arena)
            .map(SLTCountInput::Vector)
            .unwrap_or_else(|| {
                SLTCountInput::Predicates(
                    predicates
                        .into_iter()
                        .map(SLTCountPredicate::Node)
                        .collect(),
                )
            })
    };
    Some(SLTCountPlan {
        op: UnaryOp::PopCount,
        input_width,
        input,
        post: base.map_or(SLTCountPost::Direct, SLTCountPost::AddTo),
    })
}

pub(super) fn match_slt_boolean_not<A: Hash + Eq + Clone>(
    node: NodeId,
    arena: &SLTNodeArena<A>,
) -> Option<NodeId> {
    let node = unwrap_slt_one_bit_procedural_truth(node, arena);
    match arena.get(node) {
        SLTNode::Unary(UnaryOp::LogicNot, inner) => Some(*inner),
        SLTNode::Binary(lhs, BinaryOp::Eq, rhs) if slt_const_u64(*lhs, arena) == Some(0) => {
            Some(*rhs)
        }
        SLTNode::Binary(lhs, BinaryOp::Eq, rhs) if slt_const_u64(*rhs, arena) == Some(0) => {
            Some(*lhs)
        }
        _ => None,
    }
}

fn match_slt_sets_found<A: Hash + Eq + Clone>(
    node: NodeId,
    previous: NodeId,
    arena: &SLTNodeArena<A>,
) -> bool {
    if slt_const_u64(node, arena) == Some(1) {
        return true;
    }
    let SLTNode::Mux {
        cond,
        then_expr,
        else_expr,
    } = arena.get(node)
    else {
        return false;
    };
    *else_expr == previous
        && slt_const_u64(*then_expr, arena) == Some(1)
        && match_slt_boolean_not(*cond, arena) == Some(previous)
}

fn match_slt_found_update<A: Hash + Eq + Clone>(
    next: NodeId,
    previous: NodeId,
    predicate: NodeId,
    arena: &SLTNodeArena<A>,
) -> bool {
    let predicate = unwrap_slt_one_bit_procedural_truth(predicate, arena);
    match arena.get(next) {
        SLTNode::Binary(lhs, BinaryOp::Or | BinaryOp::LogicOr, rhs) => {
            (*lhs == previous && *rhs == predicate) || (*rhs == previous && *lhs == predicate)
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            unwrap_slt_one_bit_procedural_truth(*cond, arena) == predicate
                && *else_expr == previous
                && match_slt_sets_found(*then_expr, previous, arena)
        }
        _ => false,
    }
}

fn match_slt_found_reduction<A: Hash + Eq + Clone>(
    root: NodeId,
    arena: &SLTNodeArena<A>,
) -> Option<SLTCountPlan<A>> {
    if slt_width(root, arena) != 1 {
        return None;
    }
    let mut cursor = root;
    let mut predicates = Vec::new();
    loop {
        let SLTNode::Mux {
            cond,
            then_expr,
            else_expr: previous,
        } = arena.get(cursor)
        else {
            return None;
        };
        if slt_width(*cond, arena) != 1 || slt_width(*previous, arena) != 1 {
            return None;
        }
        let cond = unwrap_slt_one_bit_procedural_truth(*cond, arena);
        let predicate = if match_slt_sets_found(*then_expr, *previous, arena) {
            SLTCountPredicate::Node(cond)
        } else {
            let SLTNode::Binary(lhs, BinaryOp::Or | BinaryOp::LogicOr, rhs) = arena.get(*then_expr)
            else {
                return None;
            };
            let lane = if *lhs == *previous {
                *rhs
            } else if *rhs == *previous {
                *lhs
            } else {
                return None;
            };
            if slt_width(lane, arena) != 1 {
                return None;
            }
            SLTCountPredicate::And(cond, lane)
        };
        predicates.push(predicate);
        cursor = *previous;
        if slt_const_u64(cursor, arena) == Some(0) && slt_width(cursor, arena) == 1 {
            break;
        }
    }
    if predicates.len() < 4 {
        return None;
    }
    let input_width = predicates.len();
    let input = compact_slt_predicates(&predicates, arena)
        .map(SLTCountInput::Vector)
        .unwrap_or(SLTCountInput::Predicates(predicates));
    Some(SLTCountPlan {
        op: UnaryOp::Or,
        input_width,
        input,
        post: SLTCountPost::Direct,
    })
}

fn nested_first_write_predicates<A: Hash + Eq + Clone>(
    items: &[(usize, SLTCountPredicate, Option<(SLTBitOrigin<A>, usize)>)],
    arena: &SLTNodeArena<A>,
) -> Option<Vec<NodeId>> {
    let ordered = items.iter().rev().map(|(_, predicate, _)| {
        let SLTCountPredicate::And(outer, inner) = predicate else {
            return None;
        };
        Some((*outer, *inner, match_slt_boolean_not(*inner, arena)?))
    });
    let ordered = ordered.collect::<Option<Vec<_>>>()?;
    let &(_, _, first_state) = ordered.first()?;
    if slt_const_u64(first_state, arena) != Some(0) {
        return None;
    }
    for pair in ordered.windows(2) {
        let (predicate, _, previous) = pair[0];
        let (_, _, next) = pair[1];
        if !match_slt_found_update(next, previous, predicate, arena) {
            return None;
        }
    }
    Some(
        ordered
            .into_iter()
            .rev()
            .map(|(outer, _, _)| outer)
            .collect(),
    )
}

fn split_slt_priority_condition<A: Hash + Eq + Clone>(
    cond: NodeId,
    accumulator: NodeId,
    arena: &SLTNodeArena<A>,
) -> (bool, NodeId, Option<NodeId>) {
    let cond = unwrap_slt_one_bit_procedural_truth(cond, arena);
    let SLTNode::Binary(lhs, BinaryOp::And | BinaryOp::LogicAnd, rhs) = arena.get(cond) else {
        return (false, cond, None);
    };
    if let Some(default) = match_slt_accumulator_default(*lhs, accumulator, arena) {
        (true, *rhs, Some(default))
    } else if let Some(default) = match_slt_accumulator_default(*rhs, accumulator, arena) {
        (true, *lhs, Some(default))
    } else {
        (false, cond, None)
    }
}

fn match_slt_accumulator_default<A: Hash + Eq + Clone>(
    candidate: NodeId,
    accumulator: NodeId,
    arena: &SLTNodeArena<A>,
) -> Option<NodeId> {
    let SLTNode::Binary(lhs, BinaryOp::Eq, rhs) = arena.get(candidate) else {
        return None;
    };
    if *lhs == accumulator && slt_const_u64(*rhs, arena).is_some() {
        Some(*rhs)
    } else if *rhs == accumulator && slt_const_u64(*lhs, arena).is_some() {
        Some(*lhs)
    } else {
        None
    }
}

pub(super) fn match_slt_priority_count<A: Hash + Eq + Clone>(
    root: NodeId,
    arena: &SLTNodeArena<A>,
) -> Option<SLTCountPlan<A>> {
    let mut cursor = root;
    let mut items = Vec::new();
    let mut default_node = None;
    let mut default_value = None;
    let mut guarded = None;
    let mut conditional_gate = None;
    loop {
        let SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } = arena.get(cursor)
        else {
            return None;
        };
        let cond = unwrap_slt_one_bit_procedural_truth(*cond, arena);
        let mut value_node = *then_expr;
        let mut predicate = SLTCountPredicate::Node(cond);
        let mut origin_guard = Some(cond);
        let (is_guarded, guard, matched_default) =
            split_slt_priority_condition(cond, *else_expr, arena);

        // Procedural `if outer { if inner { acc = constant; } }` expands to
        // two muxes with the same else accumulator.  Treat it as one write
        // guarded by `outer && inner`; this preserves the exact mux semantics
        // while avoiding dependence on source-level loop structure.
        let nested_write = if slt_const_u64(value_node, arena).is_none()
            && let SLTNode::Mux {
                cond: inner_cond,
                then_expr: inner_then,
                else_expr: inner_else,
            } = arena.get(value_node)
            && *inner_else == *else_expr
            && slt_const_u64(*inner_then, arena).is_some()
            && slt_width(cond, arena) == 1
            && (is_guarded || slt_width(*inner_cond, arena) == 1)
        {
            let inner_cond = unwrap_slt_one_bit_procedural_truth(*inner_cond, arena);
            value_node = *inner_then;
            if is_guarded {
                if conditional_gate.is_some_and(|previous| previous != inner_cond) {
                    return None;
                }
                conditional_gate = Some(inner_cond);
                predicate = SLTCountPredicate::Node(guard);
                origin_guard = Some(guard);
            } else {
                predicate = SLTCountPredicate::And(cond, inner_cond);
                origin_guard = None;
            }
            true
        } else {
            false
        };
        if conditional_gate.is_some() && !nested_write {
            return None;
        }
        if guarded.is_some_and(|previous| previous != is_guarded) {
            return None;
        }
        guarded = Some(is_guarded);
        if let Some(matched_default) = matched_default {
            if default_node.is_some_and(|previous| previous != matched_default) {
                return None;
            }
            default_node = Some(matched_default);
            default_value = slt_const_u64(matched_default, arena);
        }
        let value = slt_const_u64(value_node, arena)? as usize;
        let origin = origin_guard.and_then(|_| resolve_slt_bit_origin(guard, arena));
        items.push((value, predicate, origin));
        cursor = *else_expr;
        if let (Some(gate), Some(default)) = (conditional_gate, default_node)
            && matches!(
                arena.get(cursor),
                SLTNode::Mux {
                    cond,
                    then_expr,
                    ..
                } if unwrap_slt_one_bit_procedural_truth(*cond, arena) == gate
                    && *then_expr == default
            )
        {
            break;
        }
        if !matches!(arena.get(cursor), SLTNode::Mux { .. }) {
            break;
        }
    }
    if guarded == Some(false) {
        default_node = Some(cursor);
        default_value = slt_const_u64(cursor, arena);
    }
    let default_value = default_value?;
    let result_width = slt_width(root, arena);

    // A last-write mux chain with values 0, 1, ..., N-1 is a priority
    // encoder over its conditions.  Collecting from the root visits the
    // highest-priority condition first, so `N - 1 - clz(conditions)` yields
    // the selected value.  This is exact for arbitrary predicates; no claim
    // about how those predicates were produced is required.  For no match,
    // clz is N and the subtraction wraps to the original all-ones sentinel.
    let all_ones_default = match result_width {
        1..=63 => default_value == (1u64 << result_width) - 1,
        64 => default_value == u64::MAX,
        _ => false,
    };
    if guarded == Some(false)
        && items.len() >= 4
        && all_ones_default
        && slt_width_can_represent(result_width, items.len().saturating_sub(1))
        && items
            .iter()
            .enumerate()
            .all(|(stage, (value, predicate, _))| {
                *value == items.len() - 1 - stage
                    && match predicate {
                        SLTCountPredicate::Node(node) => slt_width(*node, arena) == 1,
                        SLTCountPredicate::And(lhs, rhs) => {
                            slt_width(*lhs, arena) == 1 && slt_width(*rhs, arena) == 1
                        }
                    }
            })
    {
        let input_width = items.len();
        if slt_width_can_represent(result_width, input_width)
            && let Some(predicates) = nested_first_write_predicates(&items, arena)
        {
            let input = compact_slt_predicate_nodes(&predicates, arena)
                .map(SLTCountInput::Vector)
                .unwrap_or_else(|| {
                    SLTCountInput::Predicates(
                        predicates
                            .into_iter()
                            .map(SLTCountPredicate::Node)
                            .collect(),
                    )
                });
            return Some(SLTCountPlan {
                op: UnaryOp::CountTrailingZeros,
                input_width,
                input,
                post: SLTCountPost::ReplaceZeroInputCount(default_value),
            });
        }
        return Some(SLTCountPlan {
            op: UnaryOp::CountLeadingZeros,
            input_width,
            input: SLTCountInput::Predicates(
                items
                    .into_iter()
                    .map(|(_, predicate, _)| predicate)
                    .collect(),
            ),
            post: SLTCountPost::SubtractFrom((input_width - 1) as u64),
        });
    }

    let conditional_fallback = conditional_gate.and_then(|gate| {
        let default = default_node?;
        let SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } = arena.get(cursor)
        else {
            return None;
        };
        (unwrap_slt_one_bit_procedural_truth(*cond, arena) == gate && *then_expr == default)
            .then_some(*else_expr)
    });
    let base_matches = Some(cursor) == default_node || conditional_fallback.is_some();
    let width = default_value as usize;
    if items.len() < 4
        || items.len() != width
        || !base_matches
        || !slt_width_can_represent(slt_width(root, arena), width)
    {
        return None;
    }
    let origin = items.first()?.2.clone()?.0;
    if let SLTBitOrigin::Node(node) = origin
        && slt_width(node, arena) != width
    {
        return None;
    }
    if items.iter().any(|item| {
        item.2
            .as_ref()
            .is_none_or(|(item_origin, _)| *item_origin != origin)
    }) {
        return None;
    }

    let op = if guarded == Some(true)
        && items.iter().enumerate().all(|(j, (value, _, origin))| {
            *value == width - 1 - j && origin.as_ref().is_some_and(|(_, bit)| *bit == j)
        }) {
        UnaryOp::CountLeadingZeros
    } else if guarded == Some(true)
        && items.iter().enumerate().all(|(j, (value, _, origin))| {
            *value == width - 1 - j
                && origin
                    .as_ref()
                    .is_some_and(|(_, bit)| *bit == width - 1 - j)
        })
    {
        UnaryOp::CountTrailingZeros
    } else if guarded == Some(false)
        && items.iter().enumerate().all(|(j, (value, _, origin))| {
            *value == j
                && origin
                    .as_ref()
                    .is_some_and(|(_, bit)| *bit == width - 1 - j)
        })
    {
        UnaryOp::CountLeadingZeros
    } else if guarded == Some(false)
        && items.iter().enumerate().all(|(j, (value, _, origin))| {
            *value == j && origin.as_ref().is_some_and(|(_, bit)| *bit == j)
        })
    {
        UnaryOp::CountTrailingZeros
    } else {
        return None;
    };
    Some(SLTCountPlan {
        op,
        input_width: width,
        input: SLTCountInput::Origin(origin),
        post: if let (Some(cond), Some(false_value)) = (conditional_gate, conditional_fallback) {
            SLTCountPost::Select { cond, false_value }
        } else {
            SLTCountPost::Direct
        },
    })
}

fn match_slt_count_idiom<A: Hash + Eq + Clone>(
    root: NodeId,
    arena: &SLTNodeArena<A>,
) -> Option<SLTCountPlan<A>> {
    match_slt_found_reduction(root, arena)
        .or_else(|| match_slt_priority_count(root, arena))
        .or_else(|| match_slt_popcount(root, arena, None))
}

fn match_slt_count_idiom_with_materialized<A: Hash + Eq + Clone>(
    root: NodeId,
    arena: &SLTNodeArena<A>,
    materialized: &crate::HashMap<NodeId, RegisterId>,
) -> Option<SLTCountPlan<A>> {
    match_slt_found_reduction(root, arena)
        .or_else(|| match_slt_priority_count(root, arena))
        .or_else(|| match_slt_popcount(root, arena, Some(materialized)))
}

/// Whether the ordinary expanded SLT already matches a native count idiom.
/// Loop recovery uses this as a semantic priority check so it does not replace
/// an exact PopCount/CLZ/CTZ plan with a slower counted loop.
pub fn matches_slt_count_idiom<A: Hash + Eq + Clone>(
    root: NodeId,
    arena: &SLTNodeArena<A>,
) -> bool {
    match_slt_count_idiom(root, arena).is_some()
}

impl SLTToSIRLowerer {
    pub(super) fn lower_slt_vector_expr<
        A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display,
    >(
        &self,
        builder: &mut SIRBuilder<A>,
        expr: SLTVectorExpr<A>,
        width: usize,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        allow_cache: bool,
    ) -> RegisterId {
        match expr {
            SLTVectorExpr::Origin(SLTBitOrigin::Node(source)) => {
                self.lower_inner(builder, source, arena, cache, None, allow_cache)
            }
            SLTVectorExpr::Origin(SLTBitOrigin::Input {
                node,
                variable,
                signed: _,
                index,
            }) => self.lower_compacted_input(builder, node, &variable, &index, width, arena, cache),
            SLTVectorExpr::StaticInput {
                variable,
                access,
                unpacked_element_width,
            } => match unpacked_element_width {
                Some(element_width) => {
                    let width = access.msb - access.lsb + 1;
                    let destination = builder.alloc_logic(width);
                    builder.emit(SIRInstruction::Load(
                        destination,
                        variable,
                        SIROffset::PackedElements {
                            bit_offset: access.lsb,
                            element_width,
                        },
                        width,
                    ));
                    destination
                }
                None => self.lower_input(builder, &variable, &[], &access, arena, cache, None),
            },
            SLTVectorExpr::Broadcast(bit) => {
                let bit = self.lower_inner(builder, bit, arena, cache, None, allow_cache);
                if width == 1 {
                    return bit;
                }
                let padding = builder.alloc_bit(width - 1, false);
                builder.emit(SIRInstruction::Imm(padding, SIRValue::new(0u8)));
                let extended = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Concat(extended, vec![padding, bit]));
                let zero = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
                let result = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Binary(
                    result,
                    zero,
                    BinaryOp::Sub,
                    extended,
                ));
                result
            }
            SLTVectorExpr::LowOnes { bound } => {
                let bound = self.lower_inner(builder, bound, arena, cache, None, allow_cache);
                let one = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
                let shifted = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Binary(shifted, one, BinaryOp::Shl, bound));
                let low_ones = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Binary(
                    low_ones,
                    shifted,
                    BinaryOp::Sub,
                    one,
                ));

                // A shift count wider than the host word may have non-zero
                // high limbs even when its low limb is zero.  Saturate from a
                // full-width unsigned comparison instead of relying on the
                // legalized shift to distinguish that case.
                let bound_width = builder.register(&bound).width();
                let width_bits = (usize::BITS as usize - width.leading_zeros() as usize).max(1);
                if bound_width < width_bits {
                    return low_ones;
                }
                let compare_width = bound_width.max(width_bits);
                let extended_bound = self.cast_reg_width_ext(builder, bound, compare_width, false);
                let width_value = builder.alloc_bit(compare_width, false);
                builder.emit(SIRInstruction::Imm(
                    width_value,
                    SIRValue::new(BigUint::from(width)),
                ));
                let saturated = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    saturated,
                    extended_bound,
                    BinaryOp::GeU,
                    width_value,
                ));
                let all_ones = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Imm(
                    all_ones,
                    SIRValue::new((BigUint::from(1u8) << width) - BigUint::from(1u8)),
                ));
                let result = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Mux(result, saturated, all_ones, low_ones));
                result
            }
            SLTVectorExpr::Not(inner) => {
                let inner =
                    self.lower_slt_vector_expr(builder, *inner, width, arena, cache, allow_cache);
                let result = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Unary(result, UnaryOp::BitNot, inner));
                result
            }
            SLTVectorExpr::Binary { lhs, op, rhs } => {
                let lhs =
                    self.lower_slt_vector_expr(builder, *lhs, width, arena, cache, allow_cache);
                let rhs =
                    self.lower_slt_vector_expr(builder, *rhs, width, arena, cache, allow_cache);
                let result = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Binary(result, lhs, op, rhs));
                result
            }
        }
    }

    pub(super) fn try_lower_count_idiom<
        A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display,
    >(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        allow_cache: bool,
    ) -> Option<RegisterId> {
        if self.four_state {
            return None;
        }
        let plan = if allow_cache {
            match_slt_count_idiom_with_materialized(node, arena, cache)?
        } else {
            match_slt_count_idiom(node, arena)?
        };
        if let SLTCountPost::AddTo(base) = &plan.post {
            let base = cache.get(base)?;
            if builder.register(base).width() != self.get_width(node, arena) {
                return None;
            }
        }
        let source = match plan.input {
            SLTCountInput::Origin(SLTBitOrigin::Node(source)) => {
                self.lower_inner(builder, source, arena, cache, None, allow_cache)
            }
            SLTCountInput::Origin(SLTBitOrigin::Input {
                node,
                variable,
                signed: _,
                index,
            }) => self.lower_compacted_input(
                builder,
                node,
                &variable,
                &index,
                plan.input_width,
                arena,
                cache,
            ),
            SLTCountInput::Vector(expr) => self.lower_slt_vector_expr(
                builder,
                expr,
                plan.input_width,
                arena,
                cache,
                allow_cache,
            ),
            SLTCountInput::Predicates(predicates) => {
                let args = predicates
                    .into_iter()
                    .map(|predicate| match predicate {
                        SLTCountPredicate::Node(predicate) => {
                            self.lower_inner(builder, predicate, arena, cache, None, allow_cache)
                        }
                        SLTCountPredicate::And(lhs, rhs) => {
                            let lhs =
                                self.lower_inner(builder, lhs, arena, cache, None, allow_cache);
                            let rhs =
                                self.lower_inner(builder, rhs, arena, cache, None, allow_cache);
                            let predicate = builder.alloc_bit(1, false);
                            builder.emit(SIRInstruction::Binary(
                                predicate,
                                lhs,
                                BinaryOp::LogicAnd,
                                rhs,
                            ));
                            predicate
                        }
                    })
                    .collect();
                let source = builder.alloc_bit(plan.input_width, false);
                builder.emit(SIRInstruction::Concat(source, args));
                source
            }
        };
        let result_width = self.get_width(node, arena);
        match plan.post {
            SLTCountPost::Direct => {
                let result = builder.alloc_logic(result_width);
                builder.emit(SIRInstruction::Unary(result, plan.op, source));
                Some(result)
            }
            SLTCountPost::AddTo(base) => {
                let delta = if plan.op == UnaryOp::PopCount && plan.input_width == 1 {
                    source
                } else {
                    let delta = builder.alloc_logic(result_width);
                    builder.emit(SIRInstruction::Unary(delta, plan.op, source));
                    delta
                };
                let delta = self.cast_reg_width_ext(builder, delta, result_width, false);
                let base = *cache
                    .get(&base)
                    .expect("validated materialized count base must remain cached");
                let result = builder.alloc_logic(result_width);
                builder.emit(SIRInstruction::Binary(result, base, BinaryOp::Add, delta));
                Some(result)
            }
            SLTCountPost::SubtractFrom(minuend) => {
                let count = builder.alloc_logic(result_width);
                builder.emit(SIRInstruction::Unary(count, plan.op, source));
                let base = builder.alloc_logic(result_width);
                builder.emit(SIRInstruction::Imm(base, SIRValue::new(minuend)));
                let result = builder.alloc_logic(result_width);
                builder.emit(SIRInstruction::Binary(result, base, BinaryOp::Sub, count));
                Some(result)
            }
            SLTCountPost::ReplaceZeroInputCount(sentinel) => {
                let count = builder.alloc_logic(result_width);
                builder.emit(SIRInstruction::Unary(count, plan.op, source));
                let zero_count = builder.alloc_logic(result_width);
                builder.emit(SIRInstruction::Imm(
                    zero_count,
                    SIRValue::new(plan.input_width as u64),
                ));
                let is_zero_input = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    is_zero_input,
                    count,
                    BinaryOp::Eq,
                    zero_count,
                ));
                let default = builder.alloc_logic(result_width);
                builder.emit(SIRInstruction::Imm(default, SIRValue::new(sentinel)));
                let result = builder.alloc_logic(result_width);
                builder.emit(SIRInstruction::Mux(result, is_zero_input, default, count));
                Some(result)
            }
            SLTCountPost::Select { cond, false_value } => {
                let count = builder.alloc_logic(result_width);
                builder.emit(SIRInstruction::Unary(count, plan.op, source));
                let cond = self.lower_inner(builder, cond, arena, cache, None, allow_cache);
                let false_value =
                    self.lower_inner(builder, false_value, arena, cache, None, allow_cache);
                let result = builder.alloc_logic(result_width);
                builder.emit(SIRInstruction::Mux(result, cond, count, false_value));
                Some(result)
            }
        }
    }
}
