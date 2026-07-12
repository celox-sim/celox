use crate::ir::{
    BinaryOp, BitAccess, RegisterId, RegisterType, SIRBuilder, SIRInstruction, SIROffset,
    SIRTerminator, SIRValue, UnaryOp, VarAtomBase,
};
use crate::logic_tree::{
    NodeId, SLTForFoldGroupState, SLTLoopBound, SLTNode, SLTNodeArena, comb::SLTStepOp,
};
use num_bigint::{BigInt, BigUint};
use num_traits::Zero;
use std::cell::RefCell;
use std::hash::Hash;

fn slt_value_mask(width: usize) -> BigUint {
    (BigUint::from(1u8) << width) - BigUint::from(1u8)
}

/// Try to evaluate an SLT node as a compile-time constant.
/// Returns `Some((value, mask))` if the entire subtree is constant, `None` otherwise.
fn try_const_eval<A: Hash + Eq + Clone>(
    node_id: NodeId,
    arena: &SLTNodeArena<A>,
) -> Option<(BigUint, BigUint)> {
    match arena.get(node_id) {
        SLTNode::Constant(val, mask, width, _signed) => {
            let width_mask = slt_value_mask(*width);
            Some((val & &width_mask, mask & width_mask))
        }
        SLTNode::Binary(lhs, op, rhs) => {
            let (lv, lm) = try_const_eval(*lhs, arena)?;
            let (rv, rm) = try_const_eval(*rhs, arena)?;
            // Only fold 2-state (no X/Z) constants for safety.
            if lm != BigUint::from(0u32) || rm != BigUint::from(0u32) {
                return None;
            }
            let width = crate::logic_tree::comb::get_width(node_id, arena);
            let width_mask = slt_value_mask(width);
            let result = match op {
                BinaryOp::And => &lv & &rv,
                BinaryOp::Or => &lv | &rv,
                BinaryOp::Xor => &lv ^ &rv,
                BinaryOp::Add => &lv + &rv,
                BinaryOp::Sub => {
                    let modulus = BigUint::from(1u8) << width;
                    (&lv + modulus - &rv) & &width_mask
                }
                _ => return None,
            };
            Some((result & width_mask, BigUint::from(0u32)))
        }
        SLTNode::Unary(_, _) => None,
        SLTNode::Concat(parts) => {
            let mut combined_val = BigUint::from(0u32);
            let mut total_width = 0usize;
            for (part_node, part_width) in parts.iter().rev() {
                let (v, m) = try_const_eval(*part_node, arena)?;
                if m != BigUint::from(0u32) {
                    return None;
                }
                let width_mask = if *part_width >= 64 {
                    (BigUint::from(1u64) << part_width) - 1u64
                } else {
                    BigUint::from((1u64 << part_width) - 1)
                };
                combined_val |= (&v & &width_mask) << total_width;
                total_width += part_width;
            }
            Some((combined_val, BigUint::from(0u32)))
        }
        SLTNode::Slice { expr, access } => {
            let (v, m) = try_const_eval(*expr, arena)?;
            if m != BigUint::from(0u32) {
                return None;
            }
            let width = access.msb - access.lsb + 1;
            let shifted = &v >> access.lsb;
            let width_mask = if width >= 64 {
                (BigUint::from(1u64) << width) - 1u64
            } else {
                BigUint::from((1u64 << width) - 1)
            };
            Some((shifted & width_mask, BigUint::from(0u32)))
        }
        _ => None, // Input, Mux — not constant
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum SLTBitOrigin<A: Hash + Eq + Clone> {
    Node(NodeId),
    Input {
        variable: A,
        signed: bool,
        index: Vec<crate::logic_tree::comb::SLTIndex>,
    },
}

#[derive(Clone)]
struct SLTBitTerm<A: Hash + Eq + Clone> {
    predicate: NodeId,
    origin: Option<(SLTBitOrigin<A>, usize)>,
}

enum SLTCountPredicate {
    Node(NodeId),
    And(NodeId, NodeId),
}

enum SLTVectorExpr<A: Hash + Eq + Clone> {
    Origin(SLTBitOrigin<A>),
    /// A packed input reconstructed from a proven identity-indexed bit read.
    /// Unlike `SLTBitOrigin::Input`, this deliberately drops the dynamic lane
    /// index and loads the complete packed word at its static base offset.
    StaticInput {
        variable: A,
        access: BitAccess,
    },
    Broadcast(NodeId),
    LowOnes {
        bound: NodeId,
    },
    Not(Box<SLTVectorExpr<A>>),
    Binary {
        lhs: Box<SLTVectorExpr<A>>,
        op: BinaryOp,
        rhs: Box<SLTVectorExpr<A>>,
    },
}

enum SLTCountInput<A: Hash + Eq + Clone> {
    Origin(SLTBitOrigin<A>),
    Vector(SLTVectorExpr<A>),
    Predicates(Vec<SLTCountPredicate>),
}

struct SLTCountPlan<A: Hash + Eq + Clone> {
    op: UnaryOp,
    input_width: usize,
    input: SLTCountInput<A>,
    post: SLTCountPost,
}

enum SLTCountPost {
    Direct,
    /// Add a newly recovered population-count delta to an exact accumulator
    /// value that already dominates the current lowering point.
    AddTo(NodeId),
    /// Turn `clz(predicates)` into the selected last-write index.  With an
    /// all-ones default, `N - 1 - clz(0)` naturally wraps to that sentinel.
    SubtractFrom(u64),
    /// Map the count operation's zero-input result (`input_width`) back to the
    /// sentinel used by the procedural priority encoder.
    ReplaceZeroInputCount(u64),
    /// Preserve a conditional accumulator seed around the recovered count.
    Select {
        cond: NodeId,
        false_value: NodeId,
    },
}

fn slt_const_u64<A: Hash + Eq + Clone>(node: NodeId, arena: &SLTNodeArena<A>) -> Option<u64> {
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

fn slt_width<A: Hash + Eq + Clone>(node: NodeId, arena: &SLTNodeArena<A>) -> usize {
    crate::logic_tree::comb::get_width(node, arena)
}

/// Procedural control represents truth as `ToTwoState(Or(cond))`. Count-idiom
/// lowering is enabled only in two-state mode, where that exact pair is an
/// identity for a one-bit `cond`. Do not look through a real wide reduction.
fn unwrap_slt_one_bit_procedural_truth<A: Hash + Eq + Clone>(
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

fn resolve_slt_extended_bit<A: Hash + Eq + Clone>(
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

fn normalized_slt_lane_op(op: BinaryOp) -> Option<BinaryOp> {
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

fn match_slt_boolean_not<A: Hash + Eq + Clone>(
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

fn match_slt_priority_count<A: Hash + Eq + Clone>(
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
pub(crate) fn matches_slt_count_idiom<A: Hash + Eq + Clone>(
    root: NodeId,
    arena: &SLTNodeArena<A>,
) -> bool {
    match_slt_count_idiom(root, arena).is_some()
}

#[derive(Default)]
struct LoweringCostCache {
    tree_costs: Vec<Option<u128>>,
    contains_div_rem: Vec<Option<bool>>,
    fanout: Vec<usize>,
    initially_materialized: Vec<bool>,
    owned_costs: Vec<Option<u128>>,
    owned_slice_lower_costs: Vec<Option<u128>>,
    contains_shared_nontrivial: Vec<Option<bool>>,
    is_speculatable_pure: Vec<Option<bool>>,
    #[cfg(test)]
    analysis_node_visits: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct StaticBranchProbability {
    true_weight: u128,
    total_weight: u128,
}

impl StaticBranchProbability {
    const EVEN: Self = Self {
        true_weight: 1,
        total_weight: 2,
    };

    fn inverted(self) -> Self {
        Self {
            true_weight: self.total_weight - self.true_weight,
            total_weight: self.total_weight,
        }
    }

    fn conjunction(self, rhs: Self) -> Self {
        let Some(true_weight) = self.true_weight.checked_mul(rhs.true_weight) else {
            return Self::EVEN;
        };
        let Some(total_weight) = self.total_weight.checked_mul(rhs.total_weight) else {
            return Self::EVEN;
        };
        Self {
            true_weight,
            total_weight,
        }
    }
}

struct MuxCfgPlan {
    /// Nodes used by both arms which were not already materialized.  They must
    /// be evaluated once in the dominator before the control-flow split.
    shared_nodes: Vec<NodeId>,
}

#[derive(Clone, Default)]
struct ZeroControllerFacts {
    /// The expression is the known two-state value zero independently of any
    /// runtime predicate.  Such a child imposes no constraint when an outer
    /// operation requires all of its children to be zero.
    unconditional_zero: bool,
    /// One-bit descendants whose false value is sufficient to prove this
    /// expression is all zero.
    guards: crate::HashSet<NodeId>,
}

#[derive(Clone, Copy)]
struct GuardedConcatPlan {
    guard: NodeId,
    net_benefit_scaled: u128,
}

#[derive(Default)]
struct MuxLowerStats {
    normal_seen: usize,
    slice_seen: usize,
    constant_folded: usize,
    cfg_cost: usize,
    cfg_div_rem: usize,
    cfg_slice_cost: usize,
    cfg_slice_div_rem: usize,
    shared_nodes_hoisted: usize,
    kept_four_state: usize,
    kept_impure: usize,
    kept_dynamic_env: usize,
    kept_unprofitable: usize,
    kept_deep_shared: usize,
    biased_conditions: usize,
    owned_cost_sum: u128,
    owned_cost_max: u128,
    unprofitable_cost_buckets: [usize; 7],
}

impl MuxLowerStats {
    fn record_cost(&mut self, then_cost: u128, else_cost: u128) {
        let total = then_cost.saturating_add(else_cost);
        self.owned_cost_sum = self.owned_cost_sum.saturating_add(total);
        self.owned_cost_max = self.owned_cost_max.max(total);
    }

    fn record_unprofitable(&mut self, then_cost: u128, else_cost: u128) {
        self.kept_unprofitable += 1;
        let total = then_cost.saturating_add(else_cost);
        let bucket = match total {
            0..=7 => 0,
            8..=15 => 1,
            16..=31 => 2,
            32..=63 => 3,
            64..=127 => 4,
            128..=255 => 5,
            _ => 6,
        };
        self.unprofitable_cost_buckets[bucket] += 1;
    }
}

pub struct SLTToSIRLowerer {
    four_state: bool,
    cost_cache: RefCell<LoweringCostCache>,
    cache_insert_log: RefCell<Vec<NodeId>>,
    mux_stats: Option<RefCell<MuxLowerStats>>,
}

struct LowerEnv<'parent, A: Hash + Eq + Clone> {
    inputs: crate::HashMap<VarAtomBase<A>, RegisterId>,
    /// Lower-priority bindings from an enclosing lowering scope.  Keeping the
    /// layers separate is important for partial state targets: flattening the
    /// maps would make overlapping inner/outer ranges depend on HashMap
    /// iteration order.
    parent: Option<&'parent LowerEnv<'parent, A>>,
}

#[derive(Clone, Copy)]
struct FoldGroupLowerSpec<'arena, A: Hash + Eq + Clone> {
    loop_var: &'arena A,
    loop_width: usize,
    loop_signed: bool,
    start: &'arena BigInt,
    step: &'arena BigInt,
    trip_count: usize,
    entry_guard: NodeId,
    states: &'arena [SLTForFoldGroupState<A>],
}

impl<'arena, A: Hash + Eq + Clone> FoldGroupLowerSpec<'arena, A> {
    fn from_root(root: NodeId, arena: &'arena SLTNodeArena<A>) -> Option<Self> {
        let SLTNode::ForFoldGroup {
            loop_var,
            loop_width,
            loop_signed,
            start,
            step,
            trip_count,
            entry_guard,
            states,
        } = arena.get_checked(root)?
        else {
            return None;
        };
        Some(Self {
            loop_var,
            loop_width: *loop_width,
            loop_signed: *loop_signed,
            start,
            step,
            trip_count: *trip_count,
            entry_guard: *entry_guard,
            states,
        })
    }
}

/// A proven fixed-width first-true scan.  This is deliberately a transient
/// lowering plan rather than another SLT node: `ForFoldGroup` remains the
/// semantic representation and every near miss uses its generic counted-loop
/// lowering.
struct SLTOrScanPlan<A: Hash + Eq + Clone> {
    vector_state: usize,
    found_state: usize,
    width: usize,
    active: SLTVectorExpr<A>,
    source: SLTVectorExpr<A>,
    select_before: NodeId,
    select_first: NodeId,
}

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
                if variables.iter().any(|candidate| *candidate == variable) {
                    return true;
                }
                work.extend(index.iter().map(|entry| entry.node));
            }
            SLTNode::Constant(..) => {}
            SLTNode::Binary(lhs, _, rhs) => work.extend([*lhs, *rhs]),
            SLTNode::Unary(_, inner) | SLTNode::Slice { expr: inner, .. } => {
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

fn slt_scan_lane_bits(trip_count: usize) -> usize {
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
    index: &[crate::logic_tree::comb::SLTIndex],
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
        || state_variables
            .iter()
            .any(|candidate| *candidate == variable)
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
    Some(SLTVectorExpr::StaticInput {
        variable: variable.clone(),
        access: packed_access,
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

fn match_slt_or_scan_plan<A: Hash + Eq + Clone>(
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

#[cfg(test)]
pub(crate) fn matches_slt_or_scan_group<A: Hash + Eq + Clone>(
    root: NodeId,
    arena: &SLTNodeArena<A>,
) -> bool {
    FoldGroupLowerSpec::from_root(root, arena)
        .and_then(|spec| match_slt_or_scan_plan(&spec, arena))
        .is_some()
}

impl SLTToSIRLowerer {
    pub fn new(four_state: bool) -> Self {
        Self {
            four_state,
            cost_cache: RefCell::new(LoweringCostCache::default()),
            cache_insert_log: RefCell::new(Vec::new()),
            mux_stats: std::env::var_os("CELOX_MUX_LOWER_STATS")
                .is_some()
                .then(|| RefCell::new(MuxLowerStats::default())),
        }
    }

    #[inline(always)]
    fn with_mux_stats(&self, update: impl FnOnce(&mut MuxLowerStats)) {
        if let Some(stats) = &self.mux_stats {
            update(&mut stats.borrow_mut());
        }
    }

    /// Recursively expand SLT nodes into SIR instructions
    pub fn lower<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        self.reset_cost_cache(node, arena, cache, true);
        self.lower_inner(builder, node, arena, cache, None, true)
    }

    /// Lower several independent recovered folds as one counted loop.
    ///
    /// This entry point is intentionally transactional: a rejected family
    /// leaves both the builder and the materialization cache unchanged.  The
    /// scheduler remains responsible for proving that the roots are mutually
    /// unordered in its dependency graph; this method rechecks every local
    /// property needed by the joint loop itself.
    pub(crate) fn lower_fold_groups_jointly<
        A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display,
    >(
        &self,
        builder: &mut SIRBuilder<A>,
        roots: &[NodeId],
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> bool {
        if roots.is_empty()
            || roots.iter().any(|root| cache.contains_key(root))
            || roots.iter().copied().collect::<crate::HashSet<_>>().len() != roots.len()
        {
            return false;
        }
        let Some(specs) = roots
            .iter()
            .copied()
            .map(|root| FoldGroupLowerSpec::from_root(root, arena))
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        // A word-level scan is strictly cheaper than putting this group back
        // into a shared counted loop.  Leave it for ordinary single-root
        // lowering, which can apply the algebraic plan without weakening the
        // joint-lowering transaction.
        if !self.four_state
            && specs
                .iter()
                .any(|spec| match_slt_or_scan_plan(spec, arena).is_some())
        {
            return false;
        }
        if !Self::joint_fold_group_specs_are_legal(&specs, arena) {
            return false;
        }

        self.reset_cost_cache_roots(roots, arena, cache, true);
        let results = self.lower_fold_group_specs(builder, arena, cache, &specs, None, true);
        debug_assert_eq!(results.len(), roots.len());
        for (&root, result) in roots.iter().zip(results) {
            let previous = cache.insert(root, result);
            debug_assert!(previous.is_none());
            self.cache_insert_log.borrow_mut().push(root);
        }
        true
    }

    fn joint_fold_group_specs_are_legal<A: Hash + Eq + Clone>(
        specs: &[FoldGroupLowerSpec<'_, A>],
        arena: &SLTNodeArena<A>,
    ) -> bool {
        let Some(first) = specs.first() else {
            return false;
        };
        if specs.iter().any(|spec| {
            spec.loop_width != first.loop_width
                || spec.loop_signed != first.loop_signed
                || spec.start != first.start
                || spec.step != first.step
                || spec.trip_count != first.trip_count
                || spec.entry_guard != first.entry_guard
                || spec.loop_width == 0
                || spec.trip_count == 0
                || spec.states.is_empty()
        }) {
            return false;
        }

        let mut state_owners: crate::HashMap<A, Vec<(usize, BitAccess)>> =
            crate::HashMap::default();
        for (owner, spec) in specs.iter().enumerate() {
            for state in spec.states {
                if specs
                    .iter()
                    .any(|candidate| *candidate.loop_var == state.target.id)
                {
                    return false;
                }
                let owners = state_owners.entry(state.target.id.clone()).or_default();
                if owners
                    .iter()
                    .any(|(_, access)| access.overlaps(&state.target.access))
                {
                    return false;
                }
                owners.push((owner, state.target.access));
            }
        }

        let mut preheader_visited = crate::HashSet::default();
        if !Self::joint_fold_tree_is_legal(
            first.entry_guard,
            None,
            specs,
            &state_owners,
            arena,
            &mut preheader_visited,
        ) {
            return false;
        }
        let mut update_visited = (0..specs.len())
            .map(|_| crate::HashSet::default())
            .collect::<Vec<_>>();
        for (owner, spec) in specs.iter().enumerate() {
            for state in spec.states {
                if !Self::joint_fold_tree_is_legal(
                    state.initial,
                    None,
                    specs,
                    &state_owners,
                    arena,
                    &mut preheader_visited,
                ) || !Self::joint_fold_tree_is_legal(
                    state.update,
                    Some(owner),
                    specs,
                    &state_owners,
                    arena,
                    &mut update_visited[owner],
                ) {
                    return false;
                }
            }
        }
        true
    }

    fn joint_fold_tree_is_legal<A: Hash + Eq + Clone>(
        root: NodeId,
        update_owner: Option<usize>,
        specs: &[FoldGroupLowerSpec<'_, A>],
        state_owners: &crate::HashMap<A, Vec<(usize, BitAccess)>>,
        arena: &SLTNodeArena<A>,
        visited: &mut crate::HashSet<NodeId>,
    ) -> bool {
        let mut work = vec![root];
        while let Some(node) = work.pop() {
            if !visited.insert(node) {
                continue;
            }
            match arena.get(node) {
                SLTNode::Input {
                    variable,
                    index,
                    access,
                    ..
                } => {
                    if let (Some(owner), Some(owners)) = (update_owner, state_owners.get(variable))
                    {
                        let overlaps = owners
                            .iter()
                            .filter(|(_, target)| !index.is_empty() || target.overlaps(access));
                        for (state_owner, _) in overlaps {
                            if owner != *state_owner {
                                return false;
                            }
                        }
                    }
                    if let Some(owner) = update_owner {
                        for spec in specs {
                            if *spec.loop_var == *variable
                                && (*spec.loop_var != *specs[owner].loop_var || !index.is_empty())
                            {
                                return false;
                            }
                        }
                    }
                    work.extend(index.iter().map(|entry| entry.node));
                }
                SLTNode::Constant(..) => {}
                SLTNode::Binary(lhs, _, rhs) => {
                    work.push(*lhs);
                    work.push(*rhs);
                }
                SLTNode::Unary(_, inner) => work.push(*inner),
                SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                } => {
                    work.push(*cond);
                    work.push(*then_expr);
                    work.push(*else_expr);
                }
                SLTNode::Concat(parts) => {
                    work.extend(parts.iter().map(|(part, _)| *part));
                }
                SLTNode::Slice { expr, .. } => work.push(*expr),
                SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return false,
            }
        }
        true
    }

    pub fn lower_with_inputs<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        inputs: crate::HashMap<VarAtomBase<A>, RegisterId>,
    ) -> RegisterId {
        self.reset_cost_cache(node, arena, cache, false);
        let env = LowerEnv {
            inputs,
            parent: None,
        };
        self.lower_inner(builder, node, arena, cache, Some(&env), false)
    }

    fn lower_slt_vector_expr<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
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
                variable,
                signed: _,
                index,
            }) => self.lower_input(
                builder,
                &variable,
                &index,
                &BitAccess::new(0, width - 1),
                arena,
                cache,
                None,
            ),
            SLTVectorExpr::StaticInput { variable, access } => {
                self.lower_input(builder, &variable, &[], &access, arena, cache, None)
            }
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

    fn try_lower_count_idiom<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
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
                variable,
                signed: _,
                index,
            }) => self.lower_input(
                builder,
                &variable,
                &index,
                &BitAccess::new(0, plan.input_width - 1),
                arena,
                cache,
                None,
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

    pub fn lower_region_slice<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        access: BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        self.reset_cost_cache(node, arena, cache, true);
        let node_width = self.get_width(node, arena);
        if access.lsb == 0 && access.msb + 1 == node_width {
            return self.lower(builder, node, arena, cache);
        }
        self.lower_region_slice_inner(builder, node, &access, arena, cache)
    }

    fn lower_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        if allow_cache {
            if let Some(reg) = cache.get(&node) {
                return *reg;
            }
        }

        if env.is_none()
            && let Some(reg) = self.try_lower_count_idiom(builder, node, arena, cache, allow_cache)
        {
            if allow_cache {
                let previous = cache.insert(node, reg);
                debug_assert!(previous.is_none());
                self.cache_insert_log.borrow_mut().push(node);
            }
            return reg;
        }

        let reg = match arena.get(node) {
            SLTNode::Input {
                variable: id,
                index,
                access,
                ..
            } => {
                if let Some(env) = env
                    && let Some(reg) =
                        self.lookup_override(builder, arena, cache, env, id, index, access)
                {
                    reg
                } else {
                    self.lower_input(builder, id, index, access, arena, cache, env)
                }
            }
            SLTNode::Constant(val, mask, width, _signed) => {
                let reg = if mask.is_zero() {
                    builder.alloc_bit(*width, false)
                } else {
                    builder.alloc_logic(*width)
                };
                builder.emit(SIRInstruction::Imm(
                    reg,
                    SIRValue::new_four_state(val.clone(), mask.clone()),
                ));
                reg
            }
            SLTNode::Binary(lhs, op, rhs) => {
                let mut l = self.lower_inner(builder, *lhs, arena, cache, env, allow_cache);
                let mut r = self.lower_inner(builder, *rhs, arena, cache, env, allow_cache);
                let width = self.get_width(node, arena);
                if matches!(
                    op,
                    BinaryOp::Eq
                        | BinaryOp::Ne
                        | BinaryOp::LtU
                        | BinaryOp::LtS
                        | BinaryOp::LeU
                        | BinaryOp::LeS
                        | BinaryOp::GtU
                        | BinaryOp::GtS
                        | BinaryOp::GeU
                        | BinaryOp::GeS
                        | BinaryOp::EqWildcard
                        | BinaryOp::NeWildcard
                ) {
                    let operand_width = builder
                        .register(&l)
                        .width()
                        .max(builder.register(&r).width());
                    let signed = matches!(
                        op,
                        BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GtS | BinaryOp::GeS
                    ) || matches!(
                        op,
                        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard
                    ) && self.get_bound_signed(*lhs, arena)
                        && self.get_bound_signed(*rhs, arena);
                    l = self.cast_reg_width_ext(builder, l, operand_width, signed);
                    r = self.cast_reg_width_ext(builder, r, operand_width, signed);
                } else if matches!(
                    op,
                    BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
                ) {
                    let signed = matches!(op, BinaryOp::DivS | BinaryOp::RemS);
                    l = self.cast_reg_width_ext(builder, l, width, signed);
                    r = self.cast_reg_width_ext(builder, r, width, signed);
                }
                let dest = builder.alloc_logic(width);
                builder.emit(SIRInstruction::Binary(dest, l, *op, r));
                dest
            }
            SLTNode::Unary(op, inner) => {
                let i = self.lower_inner(builder, *inner, arena, cache, env, allow_cache);
                let width = self.get_width(node, arena);
                let dest = if matches!(op, UnaryOp::ToTwoState) {
                    builder.alloc_bit(width, self.get_bound_signed(node, arena))
                } else {
                    builder.alloc_logic(width)
                };
                builder.emit(SIRInstruction::Unary(dest, *op, i));
                dest
            }
            SLTNode::Slice { expr, access } => {
                self.lower_slice_inner(builder, *expr, access, arena, cache, env, allow_cache)
            }
            SLTNode::Concat(parts) => {
                self.lower_concat_inner(builder, node, parts, arena, cache, env, allow_cache)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => self.lower_mux_inner(
                builder,
                *cond,
                *then_expr,
                *else_expr,
                arena,
                cache,
                env,
                allow_cache,
            ),
            SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start,
                end,
                inclusive,
                step,
                step_op,
                reverse,
                result,
                initials,
                updates,
                effects,
                continue_cond,
            } => self.lower_for_fold(
                builder,
                arena,
                cache,
                loop_var,
                *loop_width,
                *loop_signed,
                start,
                end,
                *inclusive,
                *step,
                *step_op,
                *reverse,
                result,
                initials,
                updates,
                effects,
                *continue_cond,
            ),
            SLTNode::ForFoldGroup { .. } => {
                let spec = FoldGroupLowerSpec::from_root(node, arena)
                    .expect("matched ForFoldGroup must remain present in its arena");
                self.lower_fold_group_specs(
                    builder,
                    arena,
                    cache,
                    std::slice::from_ref(&spec),
                    env,
                    allow_cache,
                )[0]
            }
        };

        if allow_cache {
            let previous = cache.insert(node, reg);
            debug_assert!(previous.is_none());
            self.cache_insert_log.borrow_mut().push(node);
        }
        reg
    }

    fn lower_input<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        id: &A,
        index: &[crate::logic_tree::comb::SLTIndex],
        access: &BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
    ) -> RegisterId {
        let width = access.msb - access.lsb + 1;
        let dest = builder.alloc_logic(width);

        if !index.is_empty() {
            let off_reg = builder.alloc_bit(64, false);
            builder.emit(SIRInstruction::Imm(
                off_reg,
                SIRValue::new(access.lsb as u64),
            ));

            let mut total_dynamic = None;
            for idx_entry in index {
                let mut idx_val =
                    self.lower_inner(builder, idx_entry.node, arena, cache, env, env.is_none());

                if idx_entry.stride > 1 {
                    let stride_reg = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Imm(
                        stride_reg,
                        SIRValue::new(idx_entry.stride as u64),
                    ));
                    let stepped_idx = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Binary(
                        stepped_idx,
                        idx_val,
                        BinaryOp::Mul,
                        stride_reg,
                    ));
                    idx_val = stepped_idx;
                }

                if let Some(acc) = total_dynamic {
                    let new_acc = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Binary(new_acc, acc, BinaryOp::Add, idx_val));
                    total_dynamic = Some(new_acc);
                } else {
                    total_dynamic = Some(idx_val);
                }
            }

            if let Some(dynamic_off) = total_dynamic {
                let final_off = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Binary(
                    final_off,
                    off_reg,
                    BinaryOp::Add,
                    dynamic_off,
                ));
                builder.emit(SIRInstruction::Load(
                    dest,
                    id.clone(),
                    SIROffset::Dynamic(final_off),
                    width,
                ));
            } else {
                builder.emit(SIRInstruction::Load(
                    dest,
                    id.clone(),
                    SIROffset::Dynamic(off_reg),
                    width,
                ));
            }
        } else {
            builder.emit(SIRInstruction::Load(
                dest,
                id.clone(),
                SIROffset::Static(access.lsb),
                width,
            ));
        }

        dest
    }

    fn build_dynamic_offset<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        index: &[crate::logic_tree::comb::SLTIndex],
        access: &BitAccess,
    ) -> RegisterId {
        let off_reg = builder.alloc_bit(64, false);
        builder.emit(SIRInstruction::Imm(
            off_reg,
            SIRValue::new(access.lsb as u64),
        ));

        let mut total_dynamic = None;
        for idx_entry in index {
            let mut idx_val =
                self.lower_inner(builder, idx_entry.node, arena, cache, env, env.is_none());

            if idx_entry.stride > 1 {
                let stride_reg = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Imm(
                    stride_reg,
                    SIRValue::new(idx_entry.stride as u64),
                ));
                let stepped_idx = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Binary(
                    stepped_idx,
                    idx_val,
                    BinaryOp::Mul,
                    stride_reg,
                ));
                idx_val = stepped_idx;
            }

            if let Some(acc) = total_dynamic {
                let new_acc = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Binary(new_acc, acc, BinaryOp::Add, idx_val));
                total_dynamic = Some(new_acc);
            } else {
                total_dynamic = Some(idx_val);
            }
        }

        if let Some(dynamic_off) = total_dynamic {
            let final_off = builder.alloc_bit(64, false);
            builder.emit(SIRInstruction::Binary(
                final_off,
                off_reg,
                BinaryOp::Add,
                dynamic_off,
            ));
            final_off
        } else {
            off_reg
        }
    }

    fn rebuild_override_range<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: &LowerEnv<'_, A>,
        id: &A,
        index: &[crate::logic_tree::comb::SLTIndex],
        access: &BitAccess,
    ) -> Option<RegisterId> {
        let exact = VarAtomBase::new(id.clone(), access.lsb, access.msb);
        let mut higher_priority_overlap = false;
        let mut layer = Some(env);
        while let Some(current) = layer {
            if !higher_priority_overlap && let Some(reg) = current.inputs.get(&exact) {
                return Some(*reg);
            }
            for (target, reg) in &current.inputs {
                if target.id != *id {
                    continue;
                }
                if !higher_priority_overlap
                    && target.access.lsb <= access.lsb
                    && access.msb <= target.access.msb
                {
                    let rel = BitAccess::new(
                        access.lsb - target.access.lsb,
                        access.msb - target.access.lsb,
                    );
                    return Some(self.slice_reg(builder, *reg, &rel));
                }
            }
            higher_priority_overlap |= current.inputs.keys().any(|target| {
                target.id == *id
                    && target.access.lsb <= access.msb
                    && access.lsb <= target.access.msb
            });
            layer = current.parent;
        }

        let mut cut_points = vec![access.lsb, access.msb + 1];
        let mut layer = Some(env);
        while let Some(current) = layer {
            for target in current.inputs.keys() {
                if target.id != *id {
                    continue;
                }
                if target.access.msb < access.lsb || access.msb < target.access.lsb {
                    continue;
                }
                cut_points.push(target.access.lsb.max(access.lsb));
                cut_points.push((target.access.msb + 1).min(access.msb + 1));
            }
            layer = current.parent;
        }
        cut_points.sort_unstable();
        cut_points.dedup();
        if cut_points.len() <= 2 {
            return None;
        }

        let mut part_regs = Vec::new();
        for window in cut_points.windows(2).rev() {
            let part_access = BitAccess::new(window[0], window[1] - 1);
            let mut part_reg = None;
            let mut layer = Some(env);
            'layers: while let Some(current) = layer {
                for (target, reg) in &current.inputs {
                    if target.id != *id {
                        continue;
                    }
                    if target.access.lsb <= part_access.lsb && part_access.msb <= target.access.msb
                    {
                        let rel = BitAccess::new(
                            part_access.lsb - target.access.lsb,
                            part_access.msb - target.access.lsb,
                        );
                        part_reg = Some(self.slice_reg(builder, *reg, &rel));
                        break 'layers;
                    }
                }
                layer = current.parent;
            }
            let reg = part_reg.unwrap_or_else(|| {
                self.lower_input(builder, id, index, &part_access, arena, cache, None)
            });
            part_regs.push(reg);
        }

        if part_regs.len() == 1 {
            part_regs.into_iter().next()
        } else {
            let result = builder.alloc_logic(access.msb - access.lsb + 1);
            builder.emit(SIRInstruction::Concat(result, part_regs));
            Some(result)
        }
    }

    fn lookup_override<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: &LowerEnv<'_, A>,
        id: &A,
        index: &[crate::logic_tree::comb::SLTIndex],
        access: &BitAccess,
    ) -> Option<RegisterId> {
        if !index.is_empty() {
            let mut layer = Some(env);
            let mut has_override = false;
            while let Some(current) = layer {
                has_override |= current.inputs.keys().any(|target| target.id == *id);
                layer = current.parent;
            }
            if !has_override {
                return Some(self.lower_input(builder, id, index, access, arena, cache, Some(env)));
            }

            let dynamic_off =
                self.build_dynamic_offset(builder, arena, cache, Some(env), index, access);
            let mut result = self.lower_input(builder, id, index, access, arena, cache, Some(env));
            let result_width = access.msb - access.lsb + 1;
            let mut layers = Vec::new();
            let mut layer = Some(env);
            while let Some(current) = layer {
                layers.push(current);
                layer = current.parent;
            }
            // Apply outer bindings first and inner bindings last so an inner
            // loop-carried range wins whenever scopes overlap.
            for current in layers.into_iter().rev() {
                for (target, reg) in &current.inputs {
                    if target.id != *id {
                        continue;
                    }
                    let range_lo = target.access.lsb;
                    let Some(range_hi) = target
                        .access
                        .msb
                        .checked_sub(result_width.saturating_sub(1))
                    else {
                        continue;
                    };
                    if range_lo > range_hi {
                        continue;
                    }

                    let lo_reg = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Imm(lo_reg, SIRValue::new(range_lo as u64)));
                    let hi_reg = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Imm(hi_reg, SIRValue::new(range_hi as u64)));

                    let ge_lo = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(
                        ge_lo,
                        dynamic_off,
                        BinaryOp::GeU,
                        lo_reg,
                    ));
                    let le_hi = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(
                        le_hi,
                        dynamic_off,
                        BinaryOp::LeU,
                        hi_reg,
                    ));
                    let in_range = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(
                        in_range,
                        ge_lo,
                        BinaryOp::And,
                        le_hi,
                    ));

                    let rel_off = if range_lo == 0 {
                        dynamic_off
                    } else {
                        let rel = builder.alloc_bit(64, false);
                        builder.emit(SIRInstruction::Binary(
                            rel,
                            dynamic_off,
                            BinaryOp::Sub,
                            lo_reg,
                        ));
                        rel
                    };

                    let shifted = builder.alloc_logic(target.access.msb - target.access.lsb + 1);
                    builder.emit(SIRInstruction::Binary(
                        shifted,
                        *reg,
                        BinaryOp::Shr,
                        rel_off,
                    ));
                    let candidate = self.cast_reg_width(builder, shifted, result_width);
                    let merged = builder.alloc_logic(result_width);
                    builder.emit(SIRInstruction::Mux(merged, in_range, candidate, result));
                    result = merged;
                }
            }
            return Some(result);
        }
        self.rebuild_override_range(builder, arena, cache, env, id, index, access)
    }

    /// Get width (references information from veryl-analyzer)
    fn get_width<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> usize {
        crate::logic_tree::comb::get_width(node, arena)
    }

    fn get_bound_signed<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        match arena.get(node) {
            SLTNode::Input { signed, .. } => *signed,
            SLTNode::Constant(_, _, _, signed) => *signed,
            SLTNode::Binary(lhs, op, rhs) => match op {
                BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::LtU
                | BinaryOp::LtS
                | BinaryOp::LeU
                | BinaryOp::LeS
                | BinaryOp::GtU
                | BinaryOp::GtS
                | BinaryOp::GeU
                | BinaryOp::GeS
                | BinaryOp::LogicAnd
                | BinaryOp::LogicOr
                | BinaryOp::EqWildcard
                | BinaryOp::NeWildcard
                | BinaryOp::DivU
                | BinaryOp::RemU => false,
                BinaryOp::DivS | BinaryOp::RemS => true,
                BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => self.get_bound_signed(*lhs, arena),
                BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor => {
                    self.get_bound_signed(*lhs, arena) && self.get_bound_signed(*rhs, arena)
                }
            },
            SLTNode::Unary(
                UnaryOp::LogicNot
                | UnaryOp::And
                | UnaryOp::Or
                | UnaryOp::Xor
                | UnaryOp::PopCount
                | UnaryOp::CountLeadingZeros
                | UnaryOp::CountTrailingZeros,
                _,
            ) => false,
            SLTNode::Unary(
                UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Minus | UnaryOp::BitNot,
                inner,
            ) => self.get_bound_signed(*inner, arena),
            SLTNode::Mux {
                then_expr,
                else_expr,
                ..
            } => {
                self.get_bound_signed(*then_expr, arena) && self.get_bound_signed(*else_expr, arena)
            }
            SLTNode::ForFold { loop_signed, .. } => *loop_signed,
            // The grouped result has concat layout and is therefore unsigned,
            // independently of the loop counter's signedness.
            SLTNode::ForFoldGroup { .. } => false,
            // Verilog/Veryl bit- and part-select expressions are unsigned even when
            // the source signal is signed.
            SLTNode::Slice { .. } => false,
            SLTNode::Concat(_) => false,
        }
    }

    fn lower_slice_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        expr: NodeId,
        access: &crate::ir::BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        if let SLTNode::Input {
            variable,
            index,
            access: input_access,
            ..
        } = arena.get(expr)
            && !index.is_empty()
            && access.msb <= input_access.msb - input_access.lsb
        {
            let composed =
                BitAccess::new(input_access.lsb + access.lsb, input_access.lsb + access.msb);
            if let Some(env) = env {
                return self
                    .lookup_override(builder, arena, cache, env, variable, index, &composed)
                    .expect("dynamic input lookup always produces a memory fallback");
            }
            return self.lower_input(builder, variable, index, &composed, arena, cache, None);
        }

        let inner_reg = self.lower_inner(builder, expr, arena, cache, env, allow_cache);
        self.slice_reg(builder, inner_reg, access)
    }

    fn lower_region_slice_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        expr: NodeId,
        access: &crate::ir::BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        if let Some(&full_value) = cache.get(&expr) {
            if access.lsb == 0 && access.msb + 1 == self.get_width(expr, arena) {
                return full_value;
            }
            return self.slice_reg(builder, full_value, access);
        }

        match arena.get(expr) {
            SLTNode::Input {
                variable,
                index,
                access: input_access,
                ..
            } if access.msb <= input_access.msb - input_access.lsb => {
                let composed =
                    BitAccess::new(input_access.lsb + access.lsb, input_access.lsb + access.msb);
                self.lower_input(builder, variable, index, &composed, arena, cache, None)
            }
            SLTNode::Slice {
                expr: inner,
                access: inner_access,
            } if access.msb <= inner_access.msb - inner_access.lsb => {
                let composed =
                    BitAccess::new(inner_access.lsb + access.lsb, inner_access.lsb + access.msb);
                self.lower_region_slice_inner(builder, *inner, &composed, arena, cache)
            }
            SLTNode::Binary(lhs, op @ (BinaryOp::And | BinaryOp::Or | BinaryOp::Xor), rhs)
                if access.msb < self.get_width(*lhs, arena)
                    && access.msb < self.get_width(*rhs, arena) =>
            {
                let lhs_val = self.lower_region_slice_inner(builder, *lhs, access, arena, cache);
                let rhs_val = self.lower_region_slice_inner(builder, *rhs, access, arena, cache);
                let result = builder.alloc_logic(access.msb - access.lsb + 1);
                builder.emit(SIRInstruction::Binary(result, lhs_val, *op, rhs_val));
                result
            }
            SLTNode::Binary(lhs, op @ (BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul), rhs)
                if access.lsb == 0
                    && access.msb < self.get_width(*lhs, arena)
                    && access.msb < self.get_width(*rhs, arena) =>
            {
                let lhs_val = self.lower_region_slice_inner(builder, *lhs, access, arena, cache);
                let rhs_val = self.lower_region_slice_inner(builder, *rhs, access, arena, cache);
                let result = builder.alloc_logic(access.msb + 1);
                builder.emit(SIRInstruction::Binary(result, lhs_val, *op, rhs_val));
                result
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } if access.msb < self.get_width(*then_expr, arena)
                && access.msb < self.get_width(*else_expr, arena) =>
            {
                self.lower_region_slice_mux_inner(
                    builder, *cond, *then_expr, *else_expr, access, arena, cache,
                )
            }
            _ => self.lower_slice_inner(builder, expr, access, arena, cache, None, true),
        }
    }

    fn zero_required_from_both(
        lhs: &ZeroControllerFacts,
        rhs: &ZeroControllerFacts,
    ) -> ZeroControllerFacts {
        match (lhs.unconditional_zero, rhs.unconditional_zero) {
            (true, true) => ZeroControllerFacts {
                unconditional_zero: true,
                guards: crate::HashSet::default(),
            },
            (true, false) => rhs.clone(),
            (false, true) => lhs.clone(),
            (false, false) => {
                let (smaller, larger) = if lhs.guards.len() <= rhs.guards.len() {
                    (&lhs.guards, &rhs.guards)
                } else {
                    (&rhs.guards, &lhs.guards)
                };
                ZeroControllerFacts {
                    unconditional_zero: false,
                    guards: smaller
                        .iter()
                        .copied()
                        .filter(|guard| larger.contains(guard))
                        .collect(),
                }
            }
        }
    }

    fn zero_from_either(
        lhs: &ZeroControllerFacts,
        rhs: &ZeroControllerFacts,
    ) -> ZeroControllerFacts {
        if lhs.unconditional_zero || rhs.unconditional_zero {
            return ZeroControllerFacts {
                unconditional_zero: true,
                guards: crate::HashSet::default(),
            };
        }
        let mut guards = lhs.guards.clone();
        guards.extend(rhs.guards.iter().copied());
        ZeroControllerFacts {
            unconditional_zero: false,
            guards,
        }
    }

    /// Compute exact two-state zero controllers for the small algebra used by
    /// guarded lane vectors. A controller `g` is present only when assuming
    /// the one-bit value `g == 0` proves every bit of `node` is zero.
    fn zero_controller_facts<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        root: NodeId,
        arena: &SLTNodeArena<A>,
        memo: &mut crate::HashMap<NodeId, ZeroControllerFacts>,
    ) -> ZeroControllerFacts {
        if let Some(facts) = memo.get(&root) {
            return facts.clone();
        }

        // Explicit postorder avoids consuming the native stack on procedural
        // expression chains. Each node in this concat cone is analyzed once.
        let mut stack = vec![(root, false)];
        while let Some((node, expanded)) = stack.pop() {
            if memo.contains_key(&node) {
                continue;
            }
            if !expanded {
                stack.push((node, true));
                for child in Self::node_children(node, arena).into_iter().rev() {
                    if !memo.contains_key(&child) {
                        stack.push((child, false));
                    }
                }
                continue;
            }

            let child = |node: NodeId| {
                memo.get(&node)
                    .cloned()
                    .expect("zero-controller postorder must analyze children first")
            };
            let mut facts = match arena.get(node) {
                SLTNode::Constant(value, mask, _, _) => ZeroControllerFacts {
                    unconditional_zero: value.is_zero() && mask.is_zero(),
                    guards: crate::HashSet::default(),
                },
                SLTNode::Input { .. } => ZeroControllerFacts::default(),
                SLTNode::Binary(lhs, op, rhs) => {
                    let lhs = child(*lhs);
                    let rhs = child(*rhs);
                    match op {
                        BinaryOp::And | BinaryOp::LogicAnd | BinaryOp::Mul => {
                            Self::zero_from_either(&lhs, &rhs)
                        }
                        BinaryOp::Or
                        | BinaryOp::Xor
                        | BinaryOp::Add
                        | BinaryOp::Sub
                        | BinaryOp::LogicOr => Self::zero_required_from_both(&lhs, &rhs),
                        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => lhs,
                        BinaryOp::Eq
                        | BinaryOp::Ne
                        | BinaryOp::EqWildcard
                        | BinaryOp::NeWildcard
                        | BinaryOp::LtU
                        | BinaryOp::LtS
                        | BinaryOp::LeU
                        | BinaryOp::LeS
                        | BinaryOp::GtU
                        | BinaryOp::GtS
                        | BinaryOp::GeU
                        | BinaryOp::GeS
                        | BinaryOp::DivU
                        | BinaryOp::DivS
                        | BinaryOp::RemU
                        | BinaryOp::RemS => ZeroControllerFacts::default(),
                    }
                }
                SLTNode::Unary(op, inner) => match op {
                    UnaryOp::Ident
                    | UnaryOp::ToTwoState
                    | UnaryOp::Minus
                    | UnaryOp::And
                    | UnaryOp::Or
                    | UnaryOp::Xor
                    | UnaryOp::PopCount => child(*inner),
                    UnaryOp::LogicNot
                    | UnaryOp::BitNot
                    | UnaryOp::CountLeadingZeros
                    | UnaryOp::CountTrailingZeros => ZeroControllerFacts::default(),
                },
                SLTNode::Slice { expr, .. } => child(*expr),
                SLTNode::Concat(parts) => {
                    let mut combined = ZeroControllerFacts {
                        unconditional_zero: true,
                        guards: crate::HashSet::default(),
                    };
                    for (part, _) in parts {
                        combined = Self::zero_required_from_both(&combined, &child(*part));
                    }
                    combined
                }
                SLTNode::Mux {
                    then_expr,
                    else_expr,
                    ..
                } => Self::zero_required_from_both(&child(*then_expr), &child(*else_expr)),
                SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {
                    ZeroControllerFacts::default()
                }
            };

            let shared = self.cost_cache.borrow().fanout[node.0] > 1;
            if !facts.unconditional_zero
                && shared
                && self.get_width(node, arena) == 1
                && !matches!(arena.get(node), SLTNode::Constant(..))
            {
                // This shared compound value covers every zero case of its
                // descendant controllers and possibly more. Keeping only the
                // maximal value prevents a leaf from winning on a tiny local
                // cost difference and keeps deep conjunction sets linear.
                facts.guards.clear();
                facts.guards.insert(node);
            }
            memo.insert(node, facts);
        }

        memo.get(&root)
            .cloned()
            .expect("zero-controller root must be produced by its postorder")
    }

    fn guarded_concat_root_is_supported<A: Hash + Eq + Clone>(
        root: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        let mut visited = crate::HashSet::default();
        let mut work = vec![root];
        while let Some(node) = work.pop() {
            if !visited.insert(node) {
                continue;
            }
            match arena.get(node) {
                SLTNode::Binary(
                    _,
                    BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS,
                    _,
                )
                | SLTNode::ForFold { .. }
                | SLTNode::ForFoldGroup { .. } => return false,
                _ => work.extend(Self::node_children(node, arena)),
            }
        }
        true
    }

    fn guarded_concat_region_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        root: NodeId,
        guard: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
    ) -> (u128, u128) {
        let mut guard_closure = crate::HashSet::default();
        let mut guard_work = vec![guard];
        while let Some(node) = guard_work.pop() {
            if guard_closure.insert(node) {
                guard_work.extend(Self::node_children(node, arena));
            }
        }

        let mut visited = crate::HashSet::default();
        let mut live_through = crate::HashSet::default();
        let mut work = vec![root];
        let mut cost = 0u128;
        while let Some(node) = work.pop() {
            if !visited.insert(node) {
                continue;
            }
            // The guard and its dependencies are evaluated in the dominator.
            // If one is also reached outside the guard expression, the true
            // arm consumes that already-materialized value as a live-through.
            if guard_closure.contains(&node) || materialized.contains_key(&node) {
                live_through.insert(node);
                continue;
            }

            cost = cost.saturating_add(self.intrinsic_node_cost(node, arena));
            // A nested Mux may already lower to control flow. Counting only
            // the Mux itself and none of its condition/arms is a conservative
            // lower bound on work skipped by the new outer branch.
            if !matches!(arena.get(node), SLTNode::Mux { .. }) {
                work.extend(Self::node_children(node, arena));
            }
        }
        let live_through_cost = live_through
            .into_iter()
            .map(|node| Self::chunks(self.get_width(node, arena)))
            .fold(0u128, u128::saturating_add);
        (cost, live_through_cost)
    }

    fn guarded_concat_net_benefit<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        root: NodeId,
        guard: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
    ) -> Option<u128> {
        const CONTROL_COST: u128 = 3;
        const MISPREDICT_COST: u128 = 16;
        const PHI_COPY_COST_PER_CHUNK: u128 = 2;
        const LIVE_THROUGH_COST_PER_CHUNK: u128 = 1;

        let probability = Self::guarded_true_probability(guard, arena);
        let false_weight = probability.total_weight - probability.true_weight;
        let (skippable_cost, live_through_chunks) =
            self.guarded_concat_region_cost(root, guard, arena, materialized);
        let result_chunks = Self::chunks(self.get_width(root, arena));
        let saved_scaled = false_weight.saturating_mul(skippable_cost);
        let introduced_scaled = probability
            .total_weight
            .saturating_mul(
                CONTROL_COST
                    .saturating_add(result_chunks.saturating_mul(PHI_COPY_COST_PER_CHUNK))
                    .saturating_add(
                        live_through_chunks.saturating_mul(LIVE_THROUGH_COST_PER_CHUNK),
                    ),
            )
            .saturating_add(false_weight.saturating_mul(result_chunks))
            .saturating_add(
                probability
                    .true_weight
                    .min(false_weight)
                    .saturating_mul(MISPREDICT_COST),
            );
        (saved_scaled > introduced_scaled).then(|| saved_scaled - introduced_scaled)
    }

    fn guarded_concat_plan<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        root: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
    ) -> Option<GuardedConcatPlan> {
        if self.four_state || !Self::guarded_concat_root_is_supported(root, arena) {
            return None;
        }

        let mut memo = crate::HashMap::default();
        let facts = self.zero_controller_facts(root, arena, &mut memo);
        if facts.unconditional_zero {
            return None;
        }
        let mut guards = facts.guards.into_iter().collect::<Vec<_>>();
        guards.sort_unstable();

        let mut best = None;
        for guard in guards {
            if guard == root || self.get_width(guard, arena) != 1 {
                continue;
            }
            let Some(net_benefit_scaled) =
                self.guarded_concat_net_benefit(root, guard, arena, materialized)
            else {
                continue;
            };
            let candidate = GuardedConcatPlan {
                guard,
                net_benefit_scaled,
            };
            let replace = best.as_ref().is_none_or(|current: &GuardedConcatPlan| {
                candidate.net_benefit_scaled > current.net_benefit_scaled
                    || candidate.net_benefit_scaled == current.net_benefit_scaled
                        && candidate.guard < current.guard
            });
            if replace {
                best = Some(candidate);
            }
        }
        best
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_guarded_concat_cfg<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        plan: GuardedConcatPlan,
        parts: &[(NodeId, usize)],
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        let guard = self.lower_inner(builder, plan.guard, arena, cache, None, true);
        let width = parts.iter().map(|(_, width)| *width).sum();
        let result = builder.alloc_logic(width);
        let true_block = builder.new_block();
        let false_block = builder.new_block();
        let merge_block = builder.new_block_with(vec![result]);
        builder.seal_block(SIRTerminator::Branch {
            cond: guard,
            true_block: (true_block, Vec::new()),
            false_block: (false_block, Vec::new()),
        });

        let true_transaction = self.cache_transaction();
        builder.switch_to_block(true_block);
        let true_value = self.lower_concat_eager_inner(builder, parts, arena, cache, None, true);
        builder.seal_block(SIRTerminator::Jump(merge_block, vec![true_value]));
        self.rollback_cache(cache, true_transaction);

        builder.switch_to_block(false_block);
        let zero = builder.alloc_logic(width);
        builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
        builder.seal_block(SIRTerminator::Jump(merge_block, vec![zero]));

        builder.switch_to_block(merge_block);
        result
    }

    fn lower_concat_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        parts: &[(NodeId, usize)],
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        // Fast path: if all parts are constants, fold into a single wide Imm.
        if env.is_none()
            && let Some(reg) = self.try_fold_const_concat(builder, parts, arena)
        {
            return reg;
        }

        if env.is_none()
            && allow_cache
            && let Some(plan) = self.guarded_concat_plan(node, arena, cache)
        {
            return self.lower_guarded_concat_cfg(builder, plan, parts, arena, cache);
        }

        self.lower_concat_eager_inner(builder, parts, arena, cache, env, allow_cache)
    }

    fn lower_concat_eager_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        parts: &[(NodeId, usize)],
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        // Use SIR Concat instruction directly. This preserves Z bits in 4-state
        // mode (unlike the Shl+Or pattern which converts Z to X through Binary Or
        // normalization). Concat args are [MSB, ..., LSB] — same order as `parts`.
        let total_width: usize = parts.iter().map(|(_, w)| w).sum();
        let part_regs: Vec<RegisterId> = parts
            .iter()
            .map(|(node, width)| {
                let reg = self.lower_inner(builder, *node, arena, cache, env, allow_cache);
                self.cast_reg_width(builder, reg, *width)
            })
            .collect();
        let result = builder.alloc_logic(total_width);
        builder.emit(SIRInstruction::Concat(result, part_regs));
        result
    }

    /// Try to fold a Concat of all-constant parts into a single wide Imm.
    /// Recursively evaluates each part to check if it's a compile-time constant.
    fn try_fold_const_concat<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        parts: &[(NodeId, usize)],
        arena: &SLTNodeArena<A>,
    ) -> Option<RegisterId> {
        let mut const_parts: Vec<(BigUint, BigUint, usize)> = Vec::with_capacity(parts.len());
        for (node_id, width) in parts {
            let (val, mask) = try_const_eval(*node_id, arena)?;
            const_parts.push((val, mask, *width));
        }

        // Build the combined value and mask (parts are MSB-first, reverse for LSB-first).
        let mut combined_val = BigUint::from(0u32);
        let mut combined_mask = BigUint::from(0u32);
        let mut total_width = 0usize;
        for (val, mask, width) in const_parts.iter().rev() {
            let width_mask = if *width >= 64 {
                (BigUint::from(1u64) << width) - 1u64
            } else {
                BigUint::from((1u64 << width) - 1)
            };
            combined_val |= (val & &width_mask) << total_width;
            combined_mask |= (mask & &width_mask) << total_width;
            total_width += *width;
        }

        let reg = if combined_mask.is_zero() {
            builder.alloc_bit(total_width, false)
        } else {
            builder.alloc_logic(total_width)
        };
        builder.emit(SIRInstruction::Imm(
            reg,
            SIRValue::new_four_state(combined_val, combined_mask),
        ));
        Some(reg)
    }

    fn reset_cost_cache<A: Hash + Eq + Clone>(
        &self,
        root: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
        honor_materialized: bool,
    ) {
        self.reset_cost_cache_roots(
            std::slice::from_ref(&root),
            arena,
            materialized,
            honor_materialized,
        );
    }

    fn reset_cost_cache_roots<A: Hash + Eq + Clone>(
        &self,
        roots: &[NodeId],
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
        honor_materialized: bool,
    ) {
        let node_count = arena.len();
        let mut fanout = vec![0usize; node_count];
        let mut initially_materialized = vec![false; node_count];
        let mut visited = crate::HashSet::default();
        let mut work = roots.to_vec();
        while let Some(node) = work.pop() {
            if !visited.insert(node) {
                continue;
            }
            if honor_materialized && materialized.contains_key(&node) {
                initially_materialized[node.0] = true;
                continue;
            }
            for child in Self::node_children(node, arena) {
                fanout[child.0] = fanout[child.0].saturating_add(1);
                work.push(child);
            }
        }
        *self.cost_cache.borrow_mut() = LoweringCostCache {
            tree_costs: vec![None; node_count],
            contains_div_rem: vec![None; node_count],
            fanout,
            initially_materialized,
            owned_costs: vec![None; node_count],
            owned_slice_lower_costs: vec![None; node_count],
            contains_shared_nontrivial: vec![None; node_count],
            is_speculatable_pure: vec![None; node_count],
            #[cfg(test)]
            analysis_node_visits: visited.len(),
        };
        self.cache_insert_log.borrow_mut().clear();
    }

    fn cache_transaction(&self) -> usize {
        self.cache_insert_log.borrow().len()
    }

    #[cfg(test)]
    fn note_analysis_visits(&self, visits: usize) {
        let mut cache = self.cost_cache.borrow_mut();
        cache.analysis_node_visits = cache.analysis_node_visits.saturating_add(visits);
    }

    #[cfg(not(test))]
    #[inline(always)]
    fn note_analysis_visits(&self, _visits: usize) {}

    #[cfg(test)]
    fn analysis_node_visits(&self) -> usize {
        self.cost_cache.borrow().analysis_node_visits
    }

    fn rollback_cache(&self, cache: &mut crate::HashMap<NodeId, RegisterId>, transaction: usize) {
        let mut log = self.cache_insert_log.borrow_mut();
        for node in log.drain(transaction..) {
            cache.remove(&node);
        }
    }

    fn prepare_cost_cache<A: Hash + Eq + Clone>(&self, arena: &SLTNodeArena<A>) {
        let mut cache = self.cost_cache.borrow_mut();
        if cache.tree_costs.len() < arena.len() {
            cache.tree_costs.resize(arena.len(), None);
            cache.contains_div_rem.resize(arena.len(), None);
            cache.fanout.resize(arena.len(), 0);
            cache.initially_materialized.resize(arena.len(), false);
            cache.owned_costs.resize(arena.len(), None);
            cache.owned_slice_lower_costs.resize(arena.len(), None);
            cache.contains_shared_nontrivial.resize(arena.len(), None);
            cache.is_speculatable_pure.resize(arena.len(), None);
        }
    }

    fn node_children<A: Hash + Eq + Clone>(node: NodeId, arena: &SLTNodeArena<A>) -> Vec<NodeId> {
        match arena.get(node) {
            SLTNode::Input { index, .. } => index.iter().map(|entry| entry.node).collect(),
            SLTNode::Constant(..) => Vec::new(),
            SLTNode::Binary(lhs, _, rhs) => vec![*lhs, *rhs],
            SLTNode::Unary(_, inner) => vec![*inner],
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => vec![*cond, *then_expr, *else_expr],
            SLTNode::Concat(parts) => parts.iter().map(|(part, _)| *part).collect(),
            SLTNode::Slice { expr, .. } => vec![*expr],
            SLTNode::ForFold {
                start,
                end,
                initials,
                updates,
                effects,
                continue_cond,
                ..
            } => {
                let mut children = Vec::new();
                if let SLTLoopBound::Expr(node) = start {
                    children.push(*node);
                }
                if let SLTLoopBound::Expr(node) = end {
                    children.push(*node);
                }
                children.extend(initials.iter().map(|update| update.expr));
                children.extend(updates.iter().map(|update| update.expr));
                for effect in effects {
                    children.extend(effect.guard);
                    children.extend(effect.args.iter().copied());
                }
                children.push(*continue_cond);
                children
            }
            SLTNode::ForFoldGroup {
                entry_guard,
                states,
                ..
            } => std::iter::once(*entry_guard)
                .chain(
                    states
                        .iter()
                        .flat_map(|state| [state.initial, state.update]),
                )
                .collect(),
        }
    }

    fn chunks(width: usize) -> u128 {
        width.div_ceil(64).max(1) as u128
    }

    fn binary_operation_cost(op: BinaryOp, width: usize) -> u128 {
        let chunks = Self::chunks(width);
        match op {
            BinaryOp::And
            | BinaryOp::Or
            | BinaryOp::Xor
            | BinaryOp::LogicAnd
            | BinaryOp::LogicOr => chunks,
            BinaryOp::Add | BinaryOp::Sub => 3 * chunks,
            BinaryOp::Mul => 5 * chunks.saturating_mul(chunks),
            BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
                12 * chunks.saturating_mul(chunks)
            }
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => 4 * chunks,
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::EqWildcard
            | BinaryOp::NeWildcard
            | BinaryOp::LtU
            | BinaryOp::LtS
            | BinaryOp::LeU
            | BinaryOp::LeS
            | BinaryOp::GtU
            | BinaryOp::GtS
            | BinaryOp::GeU
            | BinaryOp::GeS => 3 * chunks,
        }
    }

    /// Runtime work introduced by this node itself.  Child work is accounted
    /// separately so hash-consed descendants can be counted exactly once.
    fn intrinsic_node_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> u128 {
        match arena.get(node) {
            SLTNode::Input { access, index, .. } => {
                let chunks = Self::chunks(access.msb - access.lsb + 1);
                3 * chunks + u128::from(!index.is_empty()) * 3
            }
            SLTNode::Constant(_, _, width, _) => Self::chunks(*width),
            SLTNode::Binary(lhs, op, rhs) => {
                let width = self.get_width(*lhs, arena).max(self.get_width(*rhs, arena));
                Self::binary_operation_cost(*op, width)
            }
            SLTNode::Unary(op, inner) => {
                let chunks = Self::chunks(self.get_width(*inner, arena));
                match op {
                    UnaryOp::PopCount => 2 * chunks + 1,
                    UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => 3 * chunks + 1,
                    _ => 2 * chunks,
                }
            }
            SLTNode::Mux {
                then_expr,
                else_expr,
                ..
            } => Self::chunks(
                self.get_width(*then_expr, arena)
                    .max(self.get_width(*else_expr, arena)),
            ),
            SLTNode::Concat(parts) => {
                let width = parts.iter().map(|(_, width)| *width).sum();
                Self::chunks(width) + parts.len() as u128
            }
            SLTNode::Slice { access, .. } => 2 * Self::chunks(access.msb - access.lsb + 1),
            // A fold contains at least a loop test, a backedge, loop-carried
            // values, and an exit edge.  Its child DAG is still counted below;
            // this fixed cost represents the control operation itself rather
            // than an input-size or iteration cap.
            SLTNode::ForFold { updates, .. } => 8 + 2 * updates.len() as u128,
            SLTNode::ForFoldGroup { states, .. } => 6 + 2 * states.len() as u128,
        }
    }

    /// Cheap, memoized upper bound used only to avoid building reachability
    /// sets for muxes that cannot possibly pay for a branch.  It may count a
    /// shared descendant more than once; the final decision below never does.
    fn estimated_tree_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> u128 {
        self.prepare_cost_cache(arena);
        if let Some(cost) = self.cost_cache.borrow().tree_costs[node.0] {
            return cost;
        }
        self.note_analysis_visits(1);
        let mut cost = self.intrinsic_node_cost(node, arena);
        for child in Self::node_children(node, arena) {
            cost = cost.saturating_add(self.estimated_tree_cost(child, arena));
        }
        self.cost_cache.borrow_mut().tree_costs[node.0] = Some(cost);
        cost
    }

    fn is_nontrivial_node<A: Hash + Eq + Clone>(node: NodeId, arena: &SLTNodeArena<A>) -> bool {
        !matches!(
            arena.get(node),
            SLTNode::Input { .. } | SLTNode::Constant(..)
        )
    }

    /// Cost which is provably owned by this node in the current top-level DAG.
    /// A node with more than one incoming DAG edge is excluded together with
    /// its descendants: charging it to either mux arm could mistake shared CSE
    /// work for conditionally skippable work.  The memo makes all nested mux
    /// queries constant-time after one traversal of the top-level DAG.
    fn owned_tree_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> u128 {
        self.prepare_cost_cache(arena);
        if let Some(cost) = self.cost_cache.borrow().owned_costs[node.0] {
            return cost;
        }
        self.note_analysis_visits(1);
        let excluded = {
            let cache = self.cost_cache.borrow();
            cache.initially_materialized[node.0] || cache.fanout[node.0] > 1
        };
        let mut cost = if excluded {
            0
        } else {
            self.intrinsic_node_cost(node, arena)
        };
        if !excluded {
            for child in Self::node_children(node, arena) {
                cost = cost.saturating_add(self.owned_tree_cost(child, arena));
            }
        }
        self.cost_cache.borrow_mut().owned_costs[node.0] = Some(cost);
        cost
    }

    /// Width-independent lower bound for region-slice lowering.  A Slice node
    /// may compose into its child without emitting an instruction, while every
    /// other non-materialized node emits at least its one-chunk operation.
    fn owned_slice_lower_cost<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> u128 {
        self.prepare_cost_cache(arena);
        if let Some(cost) = self.cost_cache.borrow().owned_slice_lower_costs[node.0] {
            return cost;
        }
        self.note_analysis_visits(1);
        let excluded = {
            let cache = self.cost_cache.borrow();
            cache.initially_materialized[node.0] || cache.fanout[node.0] > 1
        };
        let mut cost = if excluded {
            0
        } else {
            match arena.get(node) {
                SLTNode::Slice { .. } => 0,
                SLTNode::Binary(_, op, _) => Self::binary_operation_cost(*op, 1),
                SLTNode::Unary(..) => 1,
                SLTNode::ForFold { updates, .. } => 8 + 2 * updates.len() as u128,
                SLTNode::ForFoldGroup { states, .. } => 6 + 2 * states.len() as u128,
                SLTNode::Input { .. }
                | SLTNode::Constant(..)
                | SLTNode::Mux { .. }
                | SLTNode::Concat(..) => 1,
            }
        };
        if !excluded {
            for child in Self::node_children(node, arena) {
                cost = cost.saturating_add(self.owned_slice_lower_cost(child, arena));
            }
        }
        self.cost_cache.borrow_mut().owned_slice_lower_costs[node.0] = Some(cost);
        cost
    }

    fn contains_shared_nontrivial<A: Hash + Eq + Clone>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        self.prepare_cost_cache(arena);
        if let Some(result) = self.cost_cache.borrow().contains_shared_nontrivial[node.0] {
            return result;
        }
        self.note_analysis_visits(1);
        let (materialized, fanout) = {
            let cache = self.cost_cache.borrow();
            (cache.initially_materialized[node.0], cache.fanout[node.0])
        };
        let result = !materialized
            && ((fanout > 1 && Self::is_nontrivial_node(node, arena))
                || Self::node_children(node, arena)
                    .into_iter()
                    .any(|child| self.contains_shared_nontrivial(child, arena)));
        self.cost_cache.borrow_mut().contains_shared_nontrivial[node.0] = Some(result);
        result
    }

    fn direct_shared_candidates<A: Hash + Eq + Clone>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
    ) -> crate::HashSet<NodeId> {
        let candidates = std::iter::once(node)
            .chain(Self::node_children(node, arena))
            .collect::<Vec<_>>();
        self.note_analysis_visits(candidates.len());
        candidates
            .into_iter()
            .filter(|candidate| {
                !materialized.contains_key(candidate)
                    && self.cost_cache.borrow().fanout[candidate.0] > 1
                    && Self::is_nontrivial_node(*candidate, arena)
            })
            .collect()
    }

    fn arm_has_only_direct_shared<A: Hash + Eq + Clone>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
        allowed_shared: &crate::HashSet<NodeId>,
    ) -> bool {
        if materialized.contains_key(&node) || allowed_shared.contains(&node) {
            return true;
        }
        let node_is_shared =
            self.cost_cache.borrow().fanout[node.0] > 1 && Self::is_nontrivial_node(node, arena);
        if node_is_shared {
            return false;
        }
        let children = Self::node_children(node, arena);
        self.note_analysis_visits(children.len().max(1));
        children.into_iter().all(|child| {
            materialized.contains_key(&child)
                || allowed_shared.contains(&child)
                || !self.contains_shared_nontrivial(child, arena)
        })
    }

    /// Find shared expressions without walking either entire arm.  Only a
    /// common root or direct operand is hoisted.  If a deeper shared expression
    /// exists, the mux remains a Select; this conservative rule preserves CSE
    /// and keeps analysis linear for long nested priority-mux chains.
    fn shared_mux_nodes<A: Hash + Eq + Clone>(
        &self,
        then_expr: NodeId,
        else_expr: NodeId,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
    ) -> Option<Vec<NodeId>> {
        let then_candidates = self.direct_shared_candidates(then_expr, arena, materialized);
        let else_candidates = self.direct_shared_candidates(else_expr, arena, materialized);
        let shared = then_candidates
            .intersection(&else_candidates)
            .copied()
            .collect::<crate::HashSet<_>>();
        if !self.arm_has_only_direct_shared(then_expr, arena, materialized, &shared)
            || !self.arm_has_only_direct_shared(else_expr, arena, materialized, &shared)
        {
            return None;
        }
        let mut shared = shared.into_iter().collect::<Vec<_>>();
        shared.sort_unstable_by_key(|node| std::cmp::Reverse(node.0));
        Some(shared)
    }

    fn is_speculatable_pure<A: Hash + Eq + Clone>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        self.prepare_cost_cache(arena);
        if let Some(result) = self.cost_cache.borrow().is_speculatable_pure[node.0] {
            return result;
        }
        self.note_analysis_visits(1);
        // Fold nodes carry a scoped loop environment and lower to CFG.  They
        // must not be hoisted or cloned as an ordinary mux-arm expression;
        // ForFold can additionally emit effects and Error exits.  All other
        // SLT nodes lower to read-only/value instructions.
        let result = !matches!(
            arena.get(node),
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. }
        ) && Self::node_children(node, arena)
            .into_iter()
            .all(|child| self.is_speculatable_pure(child, arena));
        self.cost_cache.borrow_mut().is_speculatable_pure[node.0] = Some(result);
        result
    }

    fn fold_node_is_invariant<A: Hash + Eq + Clone>(
        node: NodeId,
        rebound_variables: &crate::HashSet<&A>,
        arena: &SLTNodeArena<A>,
        memo: &mut crate::HashMap<NodeId, bool>,
    ) -> bool {
        if let Some(&invariant) = memo.get(&node) {
            return invariant;
        }
        let invariant = match arena.get(node) {
            SLTNode::Input {
                variable, index, ..
            } => {
                !rebound_variables.contains(variable)
                    && index.iter().all(|entry| {
                        Self::fold_node_is_invariant(entry.node, rebound_variables, arena, memo)
                    })
            }
            SLTNode::Constant(..) => true,
            SLTNode::Binary(lhs, _, rhs) => {
                Self::fold_node_is_invariant(*lhs, rebound_variables, arena, memo)
                    && Self::fold_node_is_invariant(*rhs, rebound_variables, arena, memo)
            }
            SLTNode::Unary(_, inner) | SLTNode::Slice { expr: inner, .. } => {
                Self::fold_node_is_invariant(*inner, rebound_variables, arena, memo)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                Self::fold_node_is_invariant(*cond, rebound_variables, arena, memo)
                    && Self::fold_node_is_invariant(*then_expr, rebound_variables, arena, memo)
                    && Self::fold_node_is_invariant(*else_expr, rebound_variables, arena, memo)
            }
            SLTNode::Concat(parts) => parts.iter().all(|(part, _)| {
                Self::fold_node_is_invariant(*part, rebound_variables, arena, memo)
            }),
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => false,
        };
        memo.insert(node, invariant);
        invariant
    }

    fn fold_capture_is_total<A: Hash + Eq + Clone>(
        node: NodeId,
        arena: &SLTNodeArena<A>,
        memo: &mut crate::HashMap<NodeId, bool>,
    ) -> bool {
        if let Some(&total) = memo.get(&node) {
            return total;
        }
        let total = match arena.get(node) {
            SLTNode::Binary(
                _,
                BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS,
                _,
            )
            | SLTNode::ForFold { .. }
            | SLTNode::ForFoldGroup { .. } => false,
            _ => Self::node_children(node, arena)
                .into_iter()
                .all(|child| Self::fold_capture_is_total(child, arena, memo)),
        };
        memo.insert(node, total);
        total
    }

    fn fold_invariant_capture_frontier<A: Hash + Eq + Clone>(
        specs: &[FoldGroupLowerSpec<'_, A>],
        arena: &SLTNodeArena<A>,
    ) -> Vec<NodeId> {
        let mut rebound_variables = crate::HashSet::default();
        for spec in specs {
            rebound_variables.insert(spec.loop_var);
            rebound_variables.extend(spec.states.iter().map(|state| &state.target.id));
        }
        let mut invariant_memo = crate::HashMap::default();
        let mut total_memo = crate::HashMap::default();
        let mut captures = crate::HashSet::default();
        let mut pending = specs
            .iter()
            .flat_map(|spec| spec.states.iter().map(|state| state.update))
            .collect::<Vec<_>>();
        let mut visited = crate::HashSet::default();
        while let Some(node) = pending.pop() {
            if !visited.insert(node) {
                continue;
            }
            let invariant =
                Self::fold_node_is_invariant(node, &rebound_variables, arena, &mut invariant_memo);
            if invariant
                && Self::fold_capture_is_total(node, arena, &mut total_memo)
                && !matches!(arena.get(node), SLTNode::Constant(..))
            {
                captures.insert(node);
                continue;
            }
            pending.extend(Self::node_children(node, arena));
        }
        let mut captures = captures.into_iter().collect::<Vec<_>>();
        captures.sort_unstable();
        captures
    }

    fn contains_div_rem<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        self.prepare_cost_cache(arena);
        if let Some(result) = self.cost_cache.borrow().contains_div_rem[node.0] {
            return result;
        }
        self.note_analysis_visits(1);
        let excluded = {
            let cache = self.cost_cache.borrow();
            cache.initially_materialized[node.0] || cache.fanout[node.0] > 1
        };
        let result = !excluded
            && (matches!(
                arena.get(node),
                SLTNode::Binary(
                    _,
                    BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS,
                    _,
                )
            ) || Self::node_children(node, arena)
                .into_iter()
                .any(|child| self.contains_div_rem(child, arena)));
        self.cost_cache.borrow_mut().contains_div_rem[node.0] = Some(result);
        result
    }

    fn static_true_probability<A: Hash + Eq + Clone>(
        cond: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> StaticBranchProbability {
        match arena.get(cond) {
            SLTNode::Unary(UnaryOp::LogicNot, inner) => {
                Self::static_true_probability(*inner, arena).inverted()
            }
            SLTNode::Unary(UnaryOp::Ident, inner) => Self::static_true_probability(*inner, arena),
            SLTNode::Binary(
                lhs,
                op @ (BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard),
                rhs,
            ) if try_const_eval(*lhs, arena).is_some() || try_const_eval(*rhs, arena).is_some() => {
                // Ball and Larus, "Branch Prediction for Free" (PLDI 1993),
                // predict equality-to-constant tests false.  Their complete
                // static heuristic reports a 20% average miss rate; use that
                // measured uncertainty as the 20/80 local prior.  This affects
                // expected executed cost, never whether analysis is allowed to
                // stop or how large a CFG may become.
                let equality = StaticBranchProbability {
                    true_weight: 1,
                    total_weight: 5,
                };
                if matches!(*op, BinaryOp::Eq | BinaryOp::EqWildcard) {
                    equality
                } else {
                    equality.inverted()
                }
            }
            _ => StaticBranchProbability::EVEN,
        }
    }

    fn guarded_true_probability<A: Hash + Eq + Clone>(
        guard: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> StaticBranchProbability {
        let mut probability = StaticBranchProbability {
            true_weight: 1,
            total_weight: 1,
        };
        let mut visited = crate::HashSet::default();
        let mut work = vec![guard];
        while let Some(node) = work.pop() {
            if !visited.insert(node) {
                continue;
            }
            match arena.get(node) {
                SLTNode::Binary(lhs, BinaryOp::LogicAnd, rhs) => {
                    work.extend([*lhs, *rhs]);
                }
                SLTNode::Unary(UnaryOp::Ident, inner) => work.push(*inner),
                _ => {
                    probability =
                        probability.conjunction(Self::static_true_probability(node, arena));
                }
            }
        }
        probability
    }

    fn mux_cfg_is_profitable(
        then_cost: u128,
        else_cost: u128,
        result_width: usize,
        probability: StaticBranchProbability,
    ) -> bool {
        Self::mux_cfg_is_profitable_with_extra_cost(
            then_cost,
            else_cost,
            result_width,
            probability,
            0,
        )
    }

    fn mux_cfg_is_profitable_with_extra_cost(
        then_cost: u128,
        else_cost: u128,
        result_width: usize,
        probability: StaticBranchProbability,
        extra_always_executed_cost: u128,
    ) -> bool {
        // Native and Cranelift both pay for a conditional transfer, the taken
        // arm's merge transfer, and a result phi copy.  With no dynamic profile,
        // predict the more likely edge and charge a 16-cycle x86 branch miss on
        // the less likely edge.  All terms are scaled by total_weight, so this
        // remains exact integer expected-cost arithmetic.
        const CONTROL_COST: u128 = 3;
        const MISPREDICT_COST: u128 = 16;
        const PHI_COPY_COST_PER_CHUNK: u128 = 2;

        let false_weight = probability.total_weight - probability.true_weight;
        let select_cost = Self::chunks(result_width);
        let skipped_cost = false_weight
            .saturating_mul(then_cost)
            .saturating_add(probability.true_weight.saturating_mul(else_cost))
            .saturating_add(probability.total_weight.saturating_mul(select_cost));
        let predictable_misses = probability.true_weight.min(false_weight);
        let introduced_cost = probability
            .total_weight
            .saturating_mul(
                CONTROL_COST
                    .saturating_add(
                        PHI_COPY_COST_PER_CHUNK.saturating_mul(Self::chunks(result_width)),
                    )
                    .saturating_add(extra_always_executed_cost),
            )
            .saturating_add(predictable_misses.saturating_mul(MISPREDICT_COST));
        skipped_cost > introduced_cost
    }

    fn mux_cfg_plan<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        result_width: usize,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
        allow_cache: bool,
    ) -> Option<MuxCfgPlan> {
        let empty_materialized = crate::HashMap::default();
        let materialized = if allow_cache {
            materialized
        } else {
            &empty_materialized
        };
        // Control flow selects one arm, while a four-state Mux bitwise-merges
        // both arms for X/Z conditions. No expression shape may bypass this
        // semantic policy.
        if self.four_state {
            self.with_mux_stats(|stats| stats.kept_four_state += 1);
            return None;
        }
        if !self.is_speculatable_pure(then_expr, arena)
            || !self.is_speculatable_pure(else_expr, arena)
        {
            self.with_mux_stats(|stats| stats.kept_impure += 1);
            return None;
        }
        let forced =
            self.contains_div_rem(then_expr, arena) || self.contains_div_rem(else_expr, arena);
        if !forced && !allow_cache {
            self.with_mux_stats(|stats| stats.kept_dynamic_env += 1);
            return None;
        }

        let probability = Self::static_true_probability(cond, arena);
        let then_cost = self.owned_tree_cost(then_expr, arena);
        let else_cost = self.owned_tree_cost(else_expr, arena);
        self.with_mux_stats(|stats| {
            stats.record_cost(then_cost, else_cost);
            stats.biased_conditions += usize::from(probability != StaticBranchProbability::EVEN);
        });
        if !forced && !Self::mux_cfg_is_profitable(then_cost, else_cost, result_width, probability)
        {
            self.with_mux_stats(|stats| stats.record_unprofitable(then_cost, else_cost));
            return None;
        }

        let shared_nodes = match self.shared_mux_nodes(then_expr, else_expr, arena, materialized) {
            Some(shared) => shared,
            None if forced => Vec::new(),
            None => {
                self.with_mux_stats(|stats| stats.kept_deep_shared += 1);
                return None;
            }
        };
        self.with_mux_stats(|stats| {
            if forced {
                stats.cfg_div_rem += 1;
            } else {
                stats.cfg_cost += 1;
            }
        });
        Some(MuxCfgPlan { shared_nodes })
    }

    fn mux_slice_cfg_plan<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        access: &BitAccess,
        arena: &SLTNodeArena<A>,
        materialized: &crate::HashMap<NodeId, RegisterId>,
    ) -> Option<MuxCfgPlan> {
        if self.four_state {
            self.with_mux_stats(|stats| stats.kept_four_state += 1);
            return None;
        }
        if !self.is_speculatable_pure(then_expr, arena)
            || !self.is_speculatable_pure(else_expr, arena)
        {
            self.with_mux_stats(|stats| stats.kept_impure += 1);
            return None;
        }
        let forced =
            self.contains_div_rem(then_expr, arena) || self.contains_div_rem(else_expr, arena);
        let shared_nodes = match self.shared_mux_nodes(then_expr, else_expr, arena, materialized) {
            Some(shared) => shared,
            None if forced => Vec::new(),
            None => {
                self.with_mux_stats(|stats| stats.kept_deep_shared += 1);
                return None;
            }
        };
        if !forced {
            let then_cost = self.owned_slice_lower_cost(then_expr, arena);
            let else_cost = self.owned_slice_lower_cost(else_expr, arena);
            let probability = Self::static_true_probability(cond, arena);
            self.with_mux_stats(|stats| {
                stats.record_cost(then_cost, else_cost);
                stats.biased_conditions +=
                    usize::from(probability != StaticBranchProbability::EVEN);
            });
            // Slice lowering can be cheaper than computing the corresponding
            // full shared node.  Charge the entire full hoist as additional
            // always-executed work; this deliberately underestimates the
            // transformation's benefit and prevents optimistic branchification.
            let shared_hoist_cost = shared_nodes
                .iter()
                .map(|node| self.estimated_tree_cost(*node, arena))
                .fold(0u128, u128::saturating_add);
            if !Self::mux_cfg_is_profitable_with_extra_cost(
                then_cost,
                else_cost,
                access.msb - access.lsb + 1,
                probability,
                shared_hoist_cost,
            ) {
                self.with_mux_stats(|stats| stats.record_unprofitable(then_cost, else_cost));
                return None;
            }
        }
        self.with_mux_stats(|stats| {
            if forced {
                stats.cfg_slice_div_rem += 1;
            } else {
                stats.cfg_slice_cost += 1;
            }
        });
        Some(MuxCfgPlan { shared_nodes })
    }

    fn constant_condition<A: Hash + Eq + Clone>(
        cond: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> Option<bool> {
        let (value, mask) = try_const_eval(cond, arena)?;
        (mask == BigUint::from(0u8)).then(|| value != BigUint::from(0u8))
    }

    fn hoist_shared_mux_nodes<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        plan: &MuxCfgPlan,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) {
        if !allow_cache {
            return;
        }
        self.with_mux_stats(|stats| stats.shared_nodes_hoisted += plan.shared_nodes.len());
        for &node in &plan.shared_nodes {
            self.lower_inner(builder, node, arena, cache, env, true);
        }
    }

    /// Cost-directed reverse if-conversion for symbolic expression DAGs.
    ///
    /// Cheap pure muxes remain `SIRInstruction::Mux`.  When the expected work
    /// skipped by preserving control exceeds branch, prediction, and phi-copy
    /// costs, the arms are lowered into separate CFG blocks.  Division and
    /// remainder remain a correctness case: an unselected zero divisor must
    /// never reach a native divide instruction.
    fn lower_mux_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        self.with_mux_stats(|stats| stats.normal_seen += 1);
        let then_width = self.get_width(then_expr, arena);
        let else_width = self.get_width(else_expr, arena);
        let res_width = then_width.max(else_width);

        if let Some(take_then) = Self::constant_condition(cond, arena) {
            self.with_mux_stats(|stats| stats.constant_folded += 1);
            let selected = if take_then { then_expr } else { else_expr };
            let value = self.lower_inner(builder, selected, arena, cache, env, allow_cache);
            return self.cast_reg_width(builder, value, res_width);
        }

        let cond_reg = self.lower_inner(builder, cond, arena, cache, env, allow_cache);
        if let Some(plan) = self.mux_cfg_plan(
            cond,
            then_expr,
            else_expr,
            res_width,
            arena,
            cache,
            allow_cache,
        ) {
            self.hoist_shared_mux_nodes(builder, &plan, arena, cache, env, allow_cache);
            return self.lower_mux_cfg(
                builder,
                cond_reg,
                then_expr,
                else_expr,
                res_width,
                arena,
                cache,
                env,
                allow_cache,
            );
        }

        let then_val = self.lower_inner(builder, then_expr, arena, cache, env, allow_cache);
        let else_val = self.lower_inner(builder, else_expr, arena, cache, env, allow_cache);

        // Use Mux instruction: preserves Z in 4-state, branchless select in 2-state.
        // Backends handle value and mask selection independently.
        let result = builder.alloc_logic(res_width);
        builder.emit(SIRInstruction::Mux(result, cond_reg, then_val, else_val));

        result
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_mux_cfg<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        cond_reg: RegisterId,
        then_expr: NodeId,
        else_expr: NodeId,
        result_width: usize,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        let result = builder.alloc_logic(result_width);
        let then_block = builder.new_block();
        let else_block = builder.new_block();
        let merge_block = builder.new_block_with(vec![result]);

        builder.seal_block(SIRTerminator::Branch {
            cond: cond_reg,
            true_block: (then_block, vec![]),
            false_block: (else_block, vec![]),
        });

        let then_transaction = self.cache_transaction();
        builder.switch_to_block(then_block);
        let then_val = self.lower_inner(builder, then_expr, arena, cache, env, allow_cache);
        let then_val = self.cast_reg_width(builder, then_val, result_width);
        builder.seal_block(SIRTerminator::Jump(merge_block, vec![then_val]));
        if allow_cache {
            self.rollback_cache(cache, then_transaction);
        }

        let else_transaction = self.cache_transaction();
        builder.switch_to_block(else_block);
        let else_val = self.lower_inner(builder, else_expr, arena, cache, env, allow_cache);
        let else_val = self.cast_reg_width(builder, else_val, result_width);
        builder.seal_block(SIRTerminator::Jump(merge_block, vec![else_val]));
        if allow_cache {
            self.rollback_cache(cache, else_transaction);
        }

        builder.switch_to_block(merge_block);
        result
    }

    fn lower_region_slice_mux_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        access: &BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        self.with_mux_stats(|stats| stats.slice_seen += 1);
        let result_width = access.msb - access.lsb + 1;
        if let Some(take_then) = Self::constant_condition(cond, arena) {
            self.with_mux_stats(|stats| stats.constant_folded += 1);
            return self.lower_region_slice_inner(
                builder,
                if take_then { then_expr } else { else_expr },
                access,
                arena,
                cache,
            );
        }

        let cond_reg = self.lower_inner(builder, cond, arena, cache, None, true);
        if let Some(plan) =
            self.mux_slice_cfg_plan(cond, then_expr, else_expr, access, arena, cache)
        {
            self.hoist_shared_mux_nodes(builder, &plan, arena, cache, None, true);
            let result = builder.alloc_logic(result_width);
            let then_block = builder.new_block();
            let else_block = builder.new_block();
            let merge_block = builder.new_block_with(vec![result]);

            builder.seal_block(SIRTerminator::Branch {
                cond: cond_reg,
                true_block: (then_block, vec![]),
                false_block: (else_block, vec![]),
            });

            let then_transaction = self.cache_transaction();
            builder.switch_to_block(then_block);
            let then_value =
                self.lower_region_slice_inner(builder, then_expr, access, arena, cache);
            builder.seal_block(SIRTerminator::Jump(merge_block, vec![then_value]));
            self.rollback_cache(cache, then_transaction);

            let else_transaction = self.cache_transaction();
            builder.switch_to_block(else_block);
            let else_value =
                self.lower_region_slice_inner(builder, else_expr, access, arena, cache);
            builder.seal_block(SIRTerminator::Jump(merge_block, vec![else_value]));
            self.rollback_cache(cache, else_transaction);

            builder.switch_to_block(merge_block);
            return result;
        }

        let then_value = self.lower_region_slice_inner(builder, then_expr, access, arena, cache);
        let else_value = self.lower_region_slice_inner(builder, else_expr, access, arena, cache);
        let result = builder.alloc_logic(result_width);
        builder.emit(SIRInstruction::Mux(
            result, cond_reg, then_value, else_value,
        ));
        result
    }

    fn slice_reg<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        access: &BitAccess,
    ) -> RegisterId {
        let width = access.msb - access.lsb + 1;
        let shift_amt = builder.alloc_bit(64, false);
        builder.emit(SIRInstruction::Imm(
            shift_amt,
            SIRValue::new(access.lsb as u64),
        ));

        let shifted = builder.alloc_logic(width);
        builder.emit(SIRInstruction::Binary(
            shifted,
            reg,
            BinaryOp::Shr,
            shift_amt,
        ));

        let mask_val = (BigUint::from(1u64) << width) - BigUint::from(1u64);
        let mask_reg = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Imm(mask_reg, SIRValue::new(mask_val)));

        let dest = builder.alloc_logic(width);
        builder.emit(SIRInstruction::Binary(
            dest,
            shifted,
            BinaryOp::And,
            mask_reg,
        ));
        dest
    }

    fn cast_reg_width<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        width: usize,
    ) -> RegisterId {
        self.cast_reg_width_ext(builder, reg, width, false)
    }

    fn cast_reg_width_ext<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        width: usize,
        signed: bool,
    ) -> RegisterId {
        let source_type = builder.register(&reg).clone();
        let current_width = source_type.width();
        let alloc_like_source = |builder: &mut SIRBuilder<A>, width, signed| match &source_type {
            RegisterType::Logic { .. } => builder.alloc_logic(width),
            RegisterType::Bit { .. } => builder.alloc_bit(width, signed),
        };
        if current_width == width {
            return reg;
        }
        if current_width < width {
            let pad_width = width - current_width;
            let pad = if signed {
                let sign = self.slice_reg(
                    builder,
                    reg,
                    &BitAccess::new(current_width - 1, current_width - 1),
                );
                if pad_width == 1 {
                    sign
                } else {
                    let ext = alloc_like_source(builder, pad_width, true);
                    builder.emit(SIRInstruction::Concat(
                        ext,
                        std::iter::repeat_n(sign, pad_width).collect(),
                    ));
                    ext
                }
            } else {
                let zero = builder.alloc_bit(pad_width, false);
                builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
                zero
            };
            let dest = alloc_like_source(builder, width, signed);
            builder.emit(SIRInstruction::Concat(dest, vec![pad, reg]));
            return dest;
        }

        let mask_val = (BigUint::from(1u64) << width) - BigUint::from(1u64);
        let mask_reg = builder.alloc_bit(current_width, false);
        builder.emit(SIRInstruction::Imm(mask_reg, SIRValue::new(mask_val)));
        let masked = alloc_like_source(builder, current_width, signed);
        builder.emit(SIRInstruction::Binary(masked, reg, BinaryOp::And, mask_reg));
        let sliced = self.slice_reg(builder, masked, &BitAccess::new(0, width - 1));
        let dest = alloc_like_source(builder, width, signed);
        builder.emit(SIRInstruction::Unary(
            dest,
            crate::ir::UnaryOp::Ident,
            sliced,
        ));
        dest
    }

    fn lower_bound<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        bound: &SLTLoopBound,
        _canonical_width: usize,
        width: usize,
        signed: bool,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        match bound {
            SLTLoopBound::Const(v) => {
                let reg = builder.alloc_bit(width, signed);
                builder.emit(SIRInstruction::Imm(reg, SIRValue::new(*v as u64)));
                reg
            }
            SLTLoopBound::Expr(node) => {
                let reg = self.lower_inner(builder, *node, arena, cache, None, true);
                let source_signed = self.get_bound_signed(*node, arena);
                let extend_signed = source_signed && signed;
                let sized = self.cast_reg_width_ext(builder, reg, width, extend_signed);
                if extend_signed == signed {
                    sized
                } else {
                    let dest = builder.alloc_bit(width, signed);
                    builder.emit(SIRInstruction::Unary(dest, UnaryOp::Ident, sized));
                    dest
                }
            }
        }
    }

    fn bound_width(bound: &SLTLoopBound) -> usize {
        match bound {
            SLTLoopBound::Const(v) => {
                let bits = usize::BITS as usize - v.leading_zeros() as usize;
                bits.max(1)
            }
            SLTLoopBound::Expr(_) => 0,
        }
    }

    fn step_math_width(base_width: usize, step_op: SLTStepOp, step: usize) -> usize {
        match step_op {
            SLTStepOp::Add => {
                let step_bits = (usize::BITS as usize - step.leading_zeros() as usize).max(1);
                base_width.saturating_add(step_bits)
            }
            SLTStepOp::Mul => {
                let step_bits = (usize::BITS as usize - step.leading_zeros() as usize).max(1);
                base_width.saturating_add(step_bits)
            }
            SLTStepOp::Shl => base_width.saturating_add(step.max(1)),
        }
    }

    fn bigint_payload(value: &BigInt, width: usize) -> BigUint {
        let modulus = BigInt::from(1u8) << width;
        let mut wrapped = value % &modulus;
        if wrapped < BigInt::from(0u8) {
            wrapped += modulus;
        }
        wrapped
            .to_biguint()
            .expect("a modulo-reduced loop value must be non-negative")
    }

    fn pack_fold_group_states<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        states: &[RegisterId],
    ) -> RegisterId {
        debug_assert!(!states.is_empty());
        let width = states
            .iter()
            .map(|state| builder.register(state).width())
            .sum();
        let packed = builder.alloc_logic(width);
        builder.emit(SIRInstruction::Concat(packed, states.to_vec()));
        packed
    }

    fn lower_or_scan_plan<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
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

    /// Lower one or more independent, fixed-trip-count multi-state folds.
    ///
    /// The loop body sees one immutable set of block parameters, so every
    /// update is computed from the previous iteration and the backedge applies
    /// all updates simultaneously.  The counter is a remaining-iteration
    /// count: it cannot stall and needs neither a safety cap nor an Error exit.
    fn lower_fold_group_specs<'env, A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        specs: &[FoldGroupLowerSpec<'_, A>],
        outer_env: Option<&'env LowerEnv<'env, A>>,
        allow_cache: bool,
    ) -> Vec<RegisterId> {
        let first = specs
            .first()
            .expect("joint fold lowering requires at least one group");
        debug_assert!(first.loop_width > 0);
        debug_assert!(first.trip_count > 0);
        debug_assert!(specs.iter().all(|spec| !spec.states.is_empty()));
        debug_assert!(specs.iter().all(|spec| {
            spec.loop_width == first.loop_width
                && spec.loop_signed == first.loop_signed
                && spec.start == first.start
                && spec.step == first.step
                && spec.trip_count == first.trip_count
                && spec.entry_guard == first.entry_guard
        }));

        if !self.four_state
            && outer_env.is_none()
            && specs.len() == 1
            && let Some(plan) = match_slt_or_scan_plan(first, arena)
        {
            return vec![self.lower_or_scan_plan(builder, arena, cache, first, plan, allow_cache)];
        }

        let guard = self.lower_inner(
            builder,
            first.entry_guard,
            arena,
            cache,
            outer_env,
            allow_cache,
        );
        let mut group_ranges = Vec::with_capacity(specs.len());
        let mut state_count = 0usize;
        for spec in specs {
            let start = state_count;
            state_count = state_count
                .checked_add(spec.states.len())
                .expect("verified joint fold state count must fit usize");
            group_ranges.push(start..state_count);
        }
        let initial_states: Vec<_> = specs
            .iter()
            .flat_map(|spec| spec.states)
            .map(|state| {
                let initial =
                    self.lower_inner(builder, state.initial, arena, cache, outer_env, allow_cache);
                self.cast_reg_width(
                    builder,
                    initial,
                    state.target.access.msb - state.target.access.lsb + 1,
                )
            })
            .collect();
        let initial_packed = if self.four_state {
            group_ranges
                .iter()
                .map(|range| {
                    Some(self.pack_fold_group_states(builder, &initial_states[range.clone()]))
                })
                .collect::<Vec<_>>()
        } else {
            vec![None; specs.len()]
        };
        let remaining_width =
            (usize::BITS as usize - first.trip_count.leading_zeros() as usize).max(1);
        let initial_remaining = builder.alloc_bit(remaining_width, false);
        let zero = builder.alloc_bit(remaining_width, false);
        let one = builder.alloc_bit(remaining_width, false);
        let initial_loop_value = builder.alloc_bit(first.loop_width, first.loop_signed);
        let step_value = builder.alloc_bit(first.loop_width, first.loop_signed);
        let body_remaining = builder.alloc_bit(remaining_width, false);
        let body_loop_value = builder.alloc_bit(first.loop_width, first.loop_signed);
        let body_states: Vec<_> = specs
            .iter()
            .flat_map(|spec| spec.states)
            .map(|state| builder.alloc_logic(state.target.access.msb - state.target.access.lsb + 1))
            .collect();
        let exit_states: Vec<_> = specs
            .iter()
            .flat_map(|spec| spec.states)
            .map(|state| builder.alloc_logic(state.target.access.msb - state.target.access.lsb + 1))
            .collect();
        let body = builder.new_block_with(
            std::iter::once(body_remaining)
                .chain(std::iter::once(body_loop_value))
                .chain(body_states.iter().copied())
                .collect(),
        );
        let exit = builder.new_block_with(exit_states.clone());

        let capture_nodes = Self::fold_invariant_capture_frontier(specs, arena);
        let needs_capture_block = !capture_nodes.is_empty()
            && (!allow_cache || capture_nodes.iter().any(|node| !cache.contains_key(node)));
        let enter = needs_capture_block.then(|| builder.new_block());
        let emit_loop_setup = |builder: &mut SIRBuilder<A>| {
            builder.emit(SIRInstruction::Imm(
                initial_remaining,
                SIRValue::new(BigUint::from(first.trip_count)),
            ));
            builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
            builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
            builder.emit(SIRInstruction::Imm(
                initial_loop_value,
                SIRValue::new(Self::bigint_payload(first.start, first.loop_width)),
            ));
            builder.emit(SIRInstruction::Imm(
                step_value,
                SIRValue::new(Self::bigint_payload(first.step, first.loop_width)),
            ));
        };
        let initial_body_args = || {
            std::iter::once(initial_remaining)
                .chain(std::iter::once(initial_loop_value))
                .chain(initial_states.iter().copied())
                .collect::<Vec<_>>()
        };
        let mut captured_values = crate::HashMap::default();
        if let Some(enter) = enter {
            builder.seal_block(SIRTerminator::Branch {
                cond: guard,
                true_block: (enter, Vec::new()),
                false_block: (exit, initial_states.clone()),
            });
            builder.switch_to_block(enter);
            let capture_transaction = self.cache_transaction();
            if allow_cache {
                for node in capture_nodes {
                    let value = cache.get(&node).copied().unwrap_or_else(|| {
                        self.lower_inner(builder, node, arena, cache, outer_env, true)
                    });
                    captured_values.insert(node, value);
                }
                self.rollback_cache(cache, capture_transaction);
            } else {
                let mut capture_cache = crate::HashMap::default();
                for node in capture_nodes {
                    let value =
                        self.lower_inner(builder, node, arena, &mut capture_cache, outer_env, true);
                    captured_values.insert(node, value);
                }
                self.rollback_cache(&mut capture_cache, capture_transaction);
            }
            emit_loop_setup(builder);
            builder.seal_block(SIRTerminator::Jump(body, initial_body_args()));
        } else {
            for node in capture_nodes {
                let value = *cache
                    .get(&node)
                    .expect("a capture without an enter block must already dominate the loop");
                captured_values.insert(node, value);
            }
            emit_loop_setup(builder);
            builder.seal_block(SIRTerminator::Branch {
                cond: guard,
                true_block: (body, initial_body_args()),
                false_block: (exit, initial_states.clone()),
            });
        }

        builder.switch_to_block(body);
        let mut env_inputs = crate::HashMap::default();
        for (state, value) in specs
            .iter()
            .flat_map(|spec| spec.states)
            .zip(body_states.iter().copied())
        {
            env_inputs.insert(state.target.clone(), value);
        }
        for spec in specs {
            env_inputs.insert(
                VarAtomBase::new(spec.loop_var.clone(), 0, first.loop_width - 1),
                body_loop_value,
            );
        }
        let env = LowerEnv {
            inputs: env_inputs,
            parent: outer_env,
        };
        let mut local_cache = captured_values;
        let local_cache_transaction = self.cache_transaction();
        let next_states: Vec<_> = specs
            .iter()
            .flat_map(|spec| spec.states)
            .map(|state| {
                // The loop body uses its own environment-scoped cache.  A
                // child that was materialized while lowering a guard or an
                // initial value is not reusable here: its value may depend on
                // the loop variable or an old carried state.  Rebuild the cost
                // model from the cache that lower_inner will actually use so
                // mux profitability and mandatory lazy Div/Rem lowering do not
                // mistake an unavailable outer value for a body-local one.
                self.reset_cost_cache(state.update, arena, &local_cache, true);
                let next = self.lower_inner(
                    builder,
                    state.update,
                    arena,
                    &mut local_cache,
                    Some(&env),
                    true,
                );
                self.cast_reg_width(
                    builder,
                    next,
                    state.target.access.msb - state.target.access.lsb + 1,
                )
            })
            .collect();
        // These entries are valid only under this loop body's state/counter
        // environment.  Keep the local CSE results, but remove their tracking
        // records before returning to the caller's global cache transaction.
        self.cache_insert_log
            .borrow_mut()
            .truncate(local_cache_transaction);

        let next_remaining = builder.alloc_bit(remaining_width, false);
        builder.emit(SIRInstruction::Binary(
            next_remaining,
            body_remaining,
            BinaryOp::Sub,
            one,
        ));
        let has_more = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Binary(
            has_more,
            next_remaining,
            BinaryOp::Ne,
            zero,
        ));
        // The final value of this addition is unobserved when `has_more` is
        // false. Computing it eagerly lets the conditional edge carry the next
        // loop parameters directly, avoiding an extra hot advance block and
        // unconditional jump on every taken iteration.
        let next_loop_value = builder.alloc_bit(first.loop_width, first.loop_signed);
        builder.emit(SIRInstruction::Binary(
            next_loop_value,
            body_loop_value,
            BinaryOp::Add,
            step_value,
        ));
        builder.seal_block(SIRTerminator::Branch {
            cond: has_more,
            true_block: (
                body,
                std::iter::once(next_remaining)
                    .chain(std::iter::once(next_loop_value))
                    .chain(next_states.iter().copied())
                    .collect(),
            ),
            false_block: (exit, next_states.clone()),
        });

        builder.switch_to_block(exit);
        let mut results = Vec::with_capacity(specs.len());
        for (range, initial) in group_ranges.iter().zip(initial_packed) {
            let candidate = self.pack_fold_group_states(builder, &exit_states[range.clone()]);
            if let Some(initial) = initial {
                let result = builder.alloc_logic(builder.register(&candidate).width());
                builder.emit(SIRInstruction::Mux(result, guard, candidate, initial));
                results.push(result);
            } else {
                results.push(candidate);
            }
        }
        results
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_for_fold<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        loop_var: &A,
        loop_width: usize,
        loop_signed: bool,
        start: &SLTLoopBound,
        end: &SLTLoopBound,
        inclusive: bool,
        step: usize,
        step_op: SLTStepOp,
        reverse: bool,
        result: &VarAtomBase<A>,
        initials: &[crate::logic_tree::comb::SLTForUpdate<A>],
        updates: &[crate::logic_tree::comb::SLTForUpdate<A>],
        effects: &[crate::logic_tree::comb::SLTForEffect],
        continue_cond: NodeId,
    ) -> RegisterId {
        let mut counter_width = loop_width.max(1);
        counter_width = counter_width.max(Self::bound_width(start));
        counter_width = counter_width.max(Self::bound_width(end));
        if let SLTLoopBound::Expr(node) = start {
            counter_width = counter_width.max(self.get_width(*node, arena));
        }
        if let SLTLoopBound::Expr(node) = end {
            counter_width = counter_width.max(self.get_width(*node, arena));
        }

        let widen_inclusive = inclusive && !loop_signed;
        let compare_width = if widen_inclusive {
            counter_width + 1
        } else {
            counter_width
        };

        let start_reg = self.lower_bound(
            builder,
            start,
            loop_width,
            compare_width,
            loop_signed,
            arena,
            cache,
        );
        let end_reg = self.lower_bound(
            builder,
            end,
            loop_width,
            compare_width,
            loop_signed,
            arena,
            cache,
        );
        let one_reg = builder.alloc_bit(compare_width, loop_signed);
        builder.emit(SIRInstruction::Imm(one_reg, SIRValue::new(1u64)));
        let end_limit = if widen_inclusive {
            let reg = builder.alloc_bit(compare_width, loop_signed);
            builder.emit(SIRInstruction::Binary(reg, end_reg, BinaryOp::Add, one_reg));
            reg
        } else {
            end_reg
        };

        let init_counter = if reverse { end_reg } else { start_reg };

        let initial_states: Vec<RegisterId> = initials
            .iter()
            .zip(updates.iter())
            .map(|(init, update)| {
                let reg = self.lower_inner(builder, init.expr, arena, cache, None, true);
                let width = update.target.access.msb - update.target.access.lsb + 1;
                self.cast_reg_width(builder, reg, width)
            })
            .collect();

        let header_counter = builder.alloc_bit(compare_width, loop_signed);
        let header_states: Vec<_> = updates
            .iter()
            .map(|update| {
                let width = update.target.access.msb - update.target.access.lsb + 1;
                builder.alloc_logic(width)
            })
            .collect();
        let body_counter = builder.alloc_bit(compare_width, loop_signed);
        let body_states: Vec<_> = updates
            .iter()
            .map(|update| {
                let width = update.target.access.msb - update.target.access.lsb + 1;
                builder.alloc_logic(width)
            })
            .collect();
        let exit_states: Vec<_> = updates
            .iter()
            .map(|update| {
                let width = update.target.access.msb - update.target.access.lsb + 1;
                builder.alloc_logic(width)
            })
            .collect();

        let header_params = std::iter::once(header_counter)
            .chain(header_states.iter().copied())
            .collect();
        let body_params = std::iter::once(body_counter)
            .chain(body_states.iter().copied())
            .collect();
        let header_block = builder.new_block_with(header_params);
        let body_block = builder.new_block_with(body_params);
        let exit_block = builder.new_block_with(exit_states.clone());

        builder.seal_block(SIRTerminator::Jump(
            header_block,
            std::iter::once(init_counter)
                .chain(initial_states.iter().copied())
                .collect(),
        ));

        builder.switch_to_block(header_block);
        if reverse {
            if step == 0 {
                let cmp_op = if loop_signed {
                    if inclusive {
                        BinaryOp::GeS
                    } else {
                        BinaryOp::GtS
                    }
                } else if inclusive {
                    BinaryOp::GeU
                } else {
                    BinaryOp::GtU
                };
                let in_range = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    in_range,
                    header_counter,
                    cmp_op,
                    start_reg,
                ));
                let singleton = if inclusive {
                    let eq = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(
                        eq,
                        header_counter,
                        BinaryOp::Eq,
                        start_reg,
                    ));
                    Some(eq)
                } else {
                    None
                };
                let singleton_block = builder.new_block();
                let true_loop_block = builder.new_block();
                let in_range_block = builder.new_block();
                builder.seal_block(SIRTerminator::Branch {
                    cond: in_range,
                    true_block: (in_range_block, vec![]),
                    false_block: (exit_block, header_states.clone()),
                });
                builder.switch_to_block(in_range_block);
                if let Some(singleton) = singleton {
                    builder.seal_block(SIRTerminator::Branch {
                        cond: singleton,
                        true_block: (
                            singleton_block,
                            std::iter::once(header_counter)
                                .chain(header_states.iter().copied())
                                .collect(),
                        ),
                        false_block: (true_loop_block, vec![]),
                    });
                } else {
                    builder.seal_block(SIRTerminator::Jump(true_loop_block, vec![]));
                }
                builder.switch_to_block(true_loop_block);
                builder.seal_block(SIRTerminator::Jump(
                    body_block,
                    std::iter::once(header_counter)
                        .chain(header_states.iter().copied())
                        .collect(),
                ));
                builder.switch_to_block(singleton_block);
                builder.seal_block(SIRTerminator::Jump(
                    body_block,
                    std::iter::once(header_counter)
                        .chain(header_states.iter().copied())
                        .collect(),
                ));
            } else {
                let reverse_width = Self::step_math_width(compare_width, SLTStepOp::Add, step);
                let header_counter_ext =
                    self.cast_reg_width_ext(builder, header_counter, reverse_width, loop_signed);
                let start_ext =
                    self.cast_reg_width_ext(builder, start_reg, reverse_width, loop_signed);
                let reverse_step = builder.alloc_bit(reverse_width, loop_signed);
                builder.emit(SIRInstruction::Imm(
                    reverse_step,
                    SIRValue::new(step as u64),
                ));
                let threshold = builder.alloc_bit(reverse_width, loop_signed);
                builder.emit(SIRInstruction::Binary(
                    threshold,
                    start_ext,
                    BinaryOp::Add,
                    reverse_step,
                ));
                let cond = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    cond,
                    header_counter_ext,
                    if loop_signed {
                        BinaryOp::GeS
                    } else {
                        BinaryOp::GeU
                    },
                    if inclusive { start_ext } else { threshold },
                ));
                let body_counter_reg = if inclusive {
                    header_counter
                } else {
                    let next_counter_ext = builder.alloc_bit(reverse_width, loop_signed);
                    builder.emit(SIRInstruction::Binary(
                        next_counter_ext,
                        header_counter_ext,
                        BinaryOp::Sub,
                        reverse_step,
                    ));
                    self.cast_reg_width_ext(builder, next_counter_ext, compare_width, loop_signed)
                };
                builder.seal_block(SIRTerminator::Branch {
                    cond,
                    true_block: (
                        body_block,
                        std::iter::once(body_counter_reg)
                            .chain(header_states.iter().copied())
                            .collect(),
                    ),
                    false_block: (exit_block, header_states.clone()),
                });
            }
        } else {
            let cond = builder.alloc_bit(1, false);
            builder.emit(SIRInstruction::Binary(
                cond,
                header_counter,
                if loop_signed {
                    if inclusive {
                        BinaryOp::LeS
                    } else {
                        BinaryOp::LtS
                    }
                } else {
                    BinaryOp::LtU
                },
                end_limit,
            ));
            builder.seal_block(SIRTerminator::Branch {
                cond,
                true_block: (
                    body_block,
                    std::iter::once(header_counter)
                        .chain(header_states.iter().copied())
                        .collect(),
                ),
                false_block: (exit_block, header_states.clone()),
            });
        }

        builder.switch_to_block(body_block);
        let loop_value = body_counter;
        let loop_value_trunc =
            self.cast_reg_width_ext(builder, loop_value, loop_width, loop_signed);

        let mut env_inputs = crate::HashMap::default();
        for (update, state_reg) in updates.iter().zip(body_states.iter().copied()) {
            env_inputs.insert(update.target.clone(), state_reg);
        }
        env_inputs.insert(
            VarAtomBase::new(loop_var.clone(), 0, loop_width - 1),
            loop_value_trunc,
        );
        let env = LowerEnv {
            inputs: env_inputs,
            parent: None,
        };
        let mut local_cache = crate::HashMap::default();
        self.lower_for_effects(builder, arena, &mut local_cache, &env, effects);
        let next_states: Vec<_> = updates
            .iter()
            .map(|update| {
                let reg = self.lower_inner(
                    builder,
                    update.expr,
                    arena,
                    &mut local_cache,
                    Some(&env),
                    false,
                );
                let width = update.target.access.msb - update.target.access.lsb + 1;
                self.cast_reg_width(builder, reg, width)
            })
            .collect();

        let continue_reg = self.lower_inner(
            builder,
            continue_cond,
            arena,
            &mut local_cache,
            Some(&env),
            false,
        );

        let progress_block = builder.new_block();
        builder.seal_block(SIRTerminator::Branch {
            cond: continue_reg,
            true_block: (progress_block, vec![]),
            false_block: (exit_block, next_states.clone()),
        });
        builder.switch_to_block(progress_block);

        if reverse {
            if step == 0 {
                if inclusive {
                    let error_block = builder.new_block();
                    let terminal = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(
                        terminal,
                        body_counter,
                        BinaryOp::Eq,
                        start_reg,
                    ));
                    builder.seal_block(SIRTerminator::Branch {
                        cond: terminal,
                        true_block: (exit_block, next_states.clone()),
                        false_block: (error_block, vec![]),
                    });
                    builder.switch_to_block(error_block);
                }
                builder.seal_block(SIRTerminator::Error(1));
            } else {
                let reverse_width = Self::step_math_width(compare_width, SLTStepOp::Add, step);
                let current_math =
                    self.cast_reg_width_ext(builder, body_counter, reverse_width, loop_signed);
                let start_math =
                    self.cast_reg_width_ext(builder, start_reg, reverse_width, loop_signed);
                let reverse_step = builder.alloc_bit(reverse_width, loop_signed);
                builder.emit(SIRInstruction::Imm(
                    reverse_step,
                    SIRValue::new(step as u64),
                ));
                let threshold = builder.alloc_bit(reverse_width, loop_signed);
                builder.emit(SIRInstruction::Binary(
                    threshold,
                    start_math,
                    BinaryOp::Add,
                    reverse_step,
                ));
                let can_continue = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    can_continue,
                    current_math,
                    if loop_signed {
                        BinaryOp::GeS
                    } else {
                        BinaryOp::GeU
                    },
                    threshold,
                ));
                let next_counter_ext = builder.alloc_bit(reverse_width, loop_signed);
                builder.emit(SIRInstruction::Binary(
                    next_counter_ext,
                    current_math,
                    BinaryOp::Sub,
                    reverse_step,
                ));
                let next_counter =
                    self.cast_reg_width_ext(builder, next_counter_ext, compare_width, loop_signed);
                builder.seal_block(SIRTerminator::Branch {
                    cond: can_continue,
                    true_block: (
                        header_block,
                        std::iter::once(if inclusive {
                            next_counter
                        } else {
                            body_counter
                        })
                        .chain(next_states.iter().copied())
                        .collect(),
                    ),
                    false_block: (exit_block, next_states.clone()),
                });
            }
        } else {
            if inclusive {
                let terminal = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    terminal,
                    body_counter,
                    BinaryOp::Eq,
                    end_reg,
                ));
                let advance_block = builder.new_block();
                builder.seal_block(SIRTerminator::Branch {
                    cond: terminal,
                    true_block: (exit_block, next_states.clone()),
                    false_block: (advance_block, vec![]),
                });
                builder.switch_to_block(advance_block);
            }

            let math_width = Self::step_math_width(compare_width, step_op, step);
            let current_math =
                self.cast_reg_width_ext(builder, body_counter, math_width, loop_signed);
            let step_math = builder.alloc_bit(math_width, loop_signed);
            builder.emit(SIRInstruction::Imm(step_math, SIRValue::new(step as u64)));
            let next_math = builder.alloc_bit(math_width, loop_signed);
            let op = match step_op {
                SLTStepOp::Add => BinaryOp::Add,
                SLTStepOp::Mul => BinaryOp::Mul,
                SLTStepOp::Shl => BinaryOp::Shl,
            };
            builder.emit(SIRInstruction::Binary(
                next_math,
                current_math,
                op,
                step_math,
            ));

            let progress = builder.alloc_bit(1, false);
            builder.emit(SIRInstruction::Binary(
                progress,
                next_math,
                BinaryOp::Ne,
                current_math,
            ));
            let check_block = builder.new_block();
            let stall_block = builder.new_block();
            builder.seal_block(SIRTerminator::Branch {
                cond: progress,
                true_block: (check_block, vec![]),
                false_block: (stall_block, vec![]),
            });

            builder.switch_to_block(check_block);
            let end_math = self.cast_reg_width_ext(builder, end_limit, math_width, loop_signed);
            let in_range = builder.alloc_bit(1, false);
            builder.emit(SIRInstruction::Binary(
                in_range,
                next_math,
                if loop_signed {
                    if inclusive {
                        BinaryOp::LeS
                    } else {
                        BinaryOp::LtS
                    }
                } else {
                    BinaryOp::LtU
                },
                end_math,
            ));
            let next_counter =
                self.cast_reg_width_ext(builder, next_math, compare_width, loop_signed);
            builder.seal_block(SIRTerminator::Branch {
                cond: in_range,
                true_block: (
                    header_block,
                    std::iter::once(next_counter)
                        .chain(next_states.iter().copied())
                        .collect(),
                ),
                false_block: (exit_block, next_states.clone()),
            });

            builder.switch_to_block(stall_block);
            builder.seal_block(SIRTerminator::Error(1));
        }

        builder.switch_to_block(exit_block);
        let result_idx = updates
            .iter()
            .position(|update| update.target == *result)
            .expect("ForFold result target must be present in updates");
        exit_states[result_idx]
    }

    fn lower_for_effects<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: &LowerEnv<'_, A>,
        effects: &[crate::logic_tree::comb::SLTForEffect],
    ) {
        for effect in effects {
            let emit = |builder: &mut SIRBuilder<A>,
                        this: &Self,
                        cache: &mut crate::HashMap<NodeId, RegisterId>| {
                let args = effect
                    .args
                    .iter()
                    .map(|arg| this.lower_inner(builder, *arg, arena, cache, Some(env), false))
                    .collect();
                builder.emit(SIRInstruction::CombCaptureEvent {
                    site_id: effect.site_id,
                    args,
                    fatal_error_code: effect.fatal_error_code,
                    consume_enabled: false,
                });
            };
            if let Some(guard) = effect.guard {
                let cond = self.lower_inner(builder, guard, arena, cache, Some(env), false);
                let branch_cond = if effect.emit_on_true {
                    cond
                } else {
                    let inverted = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Unary(inverted, UnaryOp::LogicNot, cond));
                    inverted
                };
                let event_block = builder.new_block();
                let done_block = builder.new_block();
                builder.seal_block(SIRTerminator::Branch {
                    cond: branch_cond,
                    true_block: (event_block, vec![]),
                    false_block: (done_block, vec![]),
                });
                builder.switch_to_block(event_block);
                emit(builder, self, cache);
                builder.seal_block(SIRTerminator::Jump(done_block, vec![]));
                builder.switch_to_block(done_block);
            } else {
                emit(builder, self, cache);
            }
        }
    }
}

impl Drop for SLTToSIRLowerer {
    fn drop(&mut self) {
        let Some(stats) = &self.mux_stats else {
            return;
        };
        let stats = stats.borrow();
        eprintln!(
            "[mux-lower-stats] normal_seen={} slice_seen={} constant_folded={} cfg_cost={} cfg_div_rem={} cfg_slice_cost={} cfg_slice_div_rem={} shared_nodes_hoisted={} kept_four_state={} kept_impure={} kept_dynamic_env={} kept_unprofitable={} kept_deep_shared={} biased_conditions={} owned_cost_sum={} owned_cost_max={} unprofitable_buckets_0_7_15_31_63_127_255_inf={:?}",
            stats.normal_seen,
            stats.slice_seen,
            stats.constant_folded,
            stats.cfg_cost,
            stats.cfg_div_rem,
            stats.cfg_slice_cost,
            stats.cfg_slice_div_rem,
            stats.shared_nodes_hoisted,
            stats.kept_four_state,
            stats.kept_impure,
            stats.kept_dynamic_env,
            stats.kept_unprofitable,
            stats.kept_deep_shared,
            stats.biased_conditions,
            stats.owned_cost_sum,
            stats.owned_cost_max,
            stats.unprofitable_cost_buckets,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BitAccess, BlockId, ExecutionUnit};
    use crate::logic_tree::comb::SLTNodeArena;

    fn input(arena: &mut SLTNodeArena<u32>, variable: u32, width: usize) -> NodeId {
        arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index: vec![],
                access: BitAccess::new(0, width - 1),
            })
            .unwrap()
    }

    fn input_bit(arena: &mut SLTNodeArena<u32>, variable: u32, bit: usize) -> NodeId {
        arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index: vec![],
                access: BitAccess::new(bit, bit),
            })
            .unwrap()
    }

    fn constant(arena: &mut SLTNodeArena<u32>, value: u64, width: usize) -> NodeId {
        arena
            .alloc(SLTNode::Constant(value.into(), 0u8.into(), width, false))
            .unwrap()
    }

    fn operation_chain(
        arena: &mut SLTNodeArena<u32>,
        mut value: NodeId,
        op: BinaryOp,
        operations: usize,
        constant_base: u64,
        width: usize,
    ) -> NodeId {
        for index in 0..operations {
            let rhs = constant(arena, constant_base + index as u64, width);
            value = arena.alloc(SLTNode::Binary(value, op, rhs)).unwrap();
        }
        value
    }

    fn guarded_lane_concat(
        arena: &mut SLTNodeArena<u32>,
        lanes: usize,
        ungated_lane: Option<usize>,
    ) -> (NodeId, NodeId, NodeId) {
        let valid = input(arena, 10_000, 1);
        let is_store = input(arena, 10_001, 1);
        let guard = arena
            .alloc(SLTNode::Binary(valid, BinaryOp::LogicAnd, is_store))
            .unwrap();
        let threshold = constant(arena, 0x8000_0000_0000_0000, 64);
        let mut parts = Vec::with_capacity(lanes);
        let mut first_predicate = None;
        for lane in 0..lanes {
            let source = input(arena, 11_000 + lane as u32, 64);
            let expensive = operation_chain(arena, source, BinaryOp::Add, 6, 3, 64);
            let predicate = arena
                .alloc(SLTNode::Binary(expensive, BinaryOp::GeU, threshold))
                .unwrap();
            first_predicate.get_or_insert(predicate);
            let value = if ungated_lane == Some(lane) {
                predicate
            } else {
                arena
                    .alloc(SLTNode::Binary(guard, BinaryOp::LogicAnd, predicate))
                    .unwrap()
            };
            parts.push((value, 1));
        }
        let root = arena.alloc(SLTNode::Concat(parts)).unwrap();
        (root, guard, first_predicate.unwrap())
    }

    fn finish_lowering(mut builder: SIRBuilder<u32>) -> ExecutionUnit<u32> {
        builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = builder.drain();
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result()
            .unwrap_or_else(|error| panic!("{error}\n{eu}"));
        eu
    }

    fn instruction_count(
        eu: &ExecutionUnit<u32>,
        predicate: impl Fn(&SIRInstruction<u32>) -> bool,
    ) -> usize {
        eu.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| predicate(instruction))
            .count()
    }

    fn branch_count(eu: &ExecutionUnit<u32>) -> usize {
        eu.blocks
            .values()
            .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
            .count()
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TestSIRValue {
        payload: BigUint,
        mask: BigUint,
    }

    fn width_mask(width: usize) -> BigUint {
        (BigUint::from(1u8) << width) - BigUint::from(1u8)
    }

    /// Execute the small, value-only SIR subset emitted by ForFoldGroup.
    /// Keeping this interpreter local to the lowering tests lets the tests pin
    /// exact iteration and four-state merge semantics without adding a second
    /// production execution path.
    fn execute_fold_group_sir(eu: &ExecutionUnit<u32>) -> crate::HashMap<RegisterId, TestSIRValue> {
        execute_fold_group_sir_with_memory(eu, &crate::HashMap::default())
    }

    fn execute_fold_group_sir_with_memory(
        eu: &ExecutionUnit<u32>,
        memory: &crate::HashMap<u32, TestSIRValue>,
    ) -> crate::HashMap<RegisterId, TestSIRValue> {
        let mut values = crate::HashMap::default();
        let mut current = eu.entry_block_id;

        for _ in 0..100 {
            let block = &eu.blocks[&current];
            for instruction in &block.instructions {
                match instruction {
                    SIRInstruction::Imm(dst, value) => {
                        values.insert(
                            *dst,
                            TestSIRValue {
                                payload: value.payload.clone(),
                                mask: value.mask.clone(),
                            },
                        );
                    }
                    SIRInstruction::Binary(dst, lhs, op, rhs) => {
                        let lhs_reg = *lhs;
                        let rhs_reg = *rhs;
                        let lhs = &values[&lhs_reg];
                        let rhs = &values[&rhs_reg];
                        let width = eu.register_map[dst].width();
                        let modulus = BigUint::from(1u8) << width;
                        let (payload, mask) = match op {
                            BinaryOp::LogicAnd | BinaryOp::LogicOr => {
                                let truth = |reg: RegisterId, value: &TestSIRValue| {
                                    let known =
                                        width_mask(eu.register_map[&reg].width()) ^ &value.mask;
                                    if (&value.payload & known) != BigUint::from(0u8) {
                                        Some(true)
                                    } else if value.mask.is_zero() {
                                        Some(false)
                                    } else {
                                        None
                                    }
                                };
                                let lhs_truth = truth(lhs_reg, lhs);
                                let rhs_truth = truth(rhs_reg, rhs);
                                let known = match op {
                                    BinaryOp::LogicAnd => {
                                        if lhs_truth == Some(false) || rhs_truth == Some(false) {
                                            Some(false)
                                        } else if lhs_truth == Some(true) && rhs_truth == Some(true)
                                        {
                                            Some(true)
                                        } else {
                                            None
                                        }
                                    }
                                    BinaryOp::LogicOr => {
                                        if lhs_truth == Some(true) || rhs_truth == Some(true) {
                                            Some(true)
                                        } else if lhs_truth == Some(false)
                                            && rhs_truth == Some(false)
                                        {
                                            Some(false)
                                        } else {
                                            None
                                        }
                                    }
                                    _ => unreachable!(),
                                };
                                match known {
                                    Some(value) => (BigUint::from(value), BigUint::from(0u8)),
                                    None => (BigUint::from(0u8), BigUint::from(1u8)),
                                }
                            }
                            _ => {
                                assert_eq!(lhs.mask, BigUint::from(0u8));
                                assert_eq!(rhs.mask, BigUint::from(0u8));
                                let payload = match op {
                                    BinaryOp::Add => (&lhs.payload + &rhs.payload) % &modulus,
                                    BinaryOp::Mul => (&lhs.payload * &rhs.payload) % &modulus,
                                    BinaryOp::Sub => {
                                        (&lhs.payload + &modulus - &rhs.payload) % &modulus
                                    }
                                    BinaryOp::And => &lhs.payload & &rhs.payload,
                                    BinaryOp::Or => &lhs.payload | &rhs.payload,
                                    BinaryOp::Shl => {
                                        let shift = rhs
                                            .payload
                                            .to_u64_digits()
                                            .first()
                                            .copied()
                                            .unwrap_or(0);
                                        if shift > usize::MAX as u64 {
                                            BigUint::from(0u8)
                                        } else {
                                            (&lhs.payload << shift as usize) % &modulus
                                        }
                                    }
                                    BinaryOp::Shr => {
                                        let shift = rhs
                                            .payload
                                            .to_u64_digits()
                                            .first()
                                            .copied()
                                            .unwrap_or(0);
                                        if shift > usize::MAX as u64 {
                                            BigUint::from(0u8)
                                        } else {
                                            &lhs.payload >> shift as usize
                                        }
                                    }
                                    BinaryOp::Eq | BinaryOp::EqWildcard => {
                                        BigUint::from(lhs.payload == rhs.payload)
                                    }
                                    BinaryOp::Ne => BigUint::from(lhs.payload != rhs.payload),
                                    BinaryOp::GeU => BigUint::from(lhs.payload >= rhs.payload),
                                    other => {
                                        panic!("unexpected grouped-fold binary op {other:?}")
                                    }
                                };
                                (payload, BigUint::from(0u8))
                            }
                        };
                        values.insert(*dst, TestSIRValue { payload, mask });
                    }
                    SIRInstruction::Unary(dst, op, src) => {
                        let width = eu.register_map[dst].width();
                        let value = &values[src];
                        let (payload, mask) = match op {
                            UnaryOp::Ident => (value.payload.clone(), value.mask.clone()),
                            UnaryOp::ToTwoState => {
                                let known = width_mask(width) ^ &value.mask;
                                (&value.payload & known, BigUint::from(0u8))
                            }
                            UnaryOp::BitNot => {
                                (&width_mask(width) ^ &value.payload, value.mask.clone())
                            }
                            UnaryOp::LogicNot => (
                                BigUint::from(value.payload == BigUint::from(0u8)),
                                value.mask.clone(),
                            ),
                            UnaryOp::Or => (
                                BigUint::from(value.payload != BigUint::from(0u8)),
                                value.mask.clone(),
                            ),
                            UnaryOp::PopCount => (
                                BigUint::from(
                                    value
                                        .payload
                                        .to_u64_digits()
                                        .iter()
                                        .map(|word| word.count_ones() as u64)
                                        .sum::<u64>(),
                                ),
                                value.mask.clone(),
                            ),
                            other => panic!("unexpected grouped-fold unary op {other:?}"),
                        };
                        values.insert(*dst, TestSIRValue { payload, mask });
                    }
                    SIRInstruction::Load(dst, address, offset, width) => {
                        let offset = match offset {
                            SIROffset::Static(offset) => *offset,
                            SIROffset::Dynamic(offset) => values[offset]
                                .payload
                                .to_u64_digits()
                                .first()
                                .copied()
                                .unwrap_or(0)
                                as usize,
                        };
                        let source = memory
                            .get(address)
                            .unwrap_or_else(|| panic!("missing test memory value at {address}"));
                        let mask = width_mask(*width);
                        values.insert(
                            *dst,
                            TestSIRValue {
                                payload: (&source.payload >> offset) & &mask,
                                mask: (&source.mask >> offset) & mask,
                            },
                        );
                    }
                    SIRInstruction::Concat(dst, args) => {
                        let mut payload = BigUint::from(0u8);
                        let mut mask = BigUint::from(0u8);
                        for arg in args {
                            let width = eu.register_map[arg].width();
                            payload = (payload << width) | &values[arg].payload;
                            mask = (mask << width) | &values[arg].mask;
                        }
                        values.insert(*dst, TestSIRValue { payload, mask });
                    }
                    SIRInstruction::Slice(dst, src, bit_offset, width) => {
                        let mask = width_mask(*width);
                        values.insert(
                            *dst,
                            TestSIRValue {
                                payload: (&values[src].payload >> *bit_offset) & &mask,
                                mask: (&values[src].mask >> *bit_offset) & mask,
                            },
                        );
                    }
                    SIRInstruction::Mux(dst, cond, then_value, else_value) => {
                        let cond = &values[cond];
                        let selected = if cond.payload == BigUint::from(0u8) {
                            &values[else_value]
                        } else {
                            &values[then_value]
                        };
                        let width = eu.register_map[dst].width();
                        values.insert(
                            *dst,
                            TestSIRValue {
                                payload: &selected.payload & width_mask(width),
                                mask: if cond.mask == BigUint::from(0u8) {
                                    &selected.mask & width_mask(width)
                                } else {
                                    width_mask(width)
                                },
                            },
                        );
                    }
                    other => panic!("unexpected grouped-fold instruction {other:?}"),
                }
            }

            let (next, args) = match &block.terminator {
                SIRTerminator::Jump(target, args) => (*target, args),
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    if values[cond].payload == BigUint::from(0u8) {
                        (false_block.0, &false_block.1)
                    } else {
                        (true_block.0, &true_block.1)
                    }
                }
                SIRTerminator::Return => return values,
                SIRTerminator::Error(code) => panic!("unexpected Error({code})"),
            };
            let arguments = args
                .iter()
                .map(|argument| values[argument].clone())
                .collect::<Vec<_>>();
            for (&parameter, argument) in eu.blocks[&next].params.iter().zip(arguments) {
                values.insert(parameter, argument);
            }
            current = next;
        }
        panic!("grouped fold did not terminate at its exact trip count")
    }

    const SCAN_VECTOR_STATE: u32 = 100;
    const SCAN_FOUND_STATE: u32 = 101;
    const SCAN_SOURCE: u32 = 102;
    const SCAN_MASK: u32 = 103;
    const SCAN_BOUND: u32 = 104;
    const SCAN_UNMASKED: u32 = 105;
    const SCAN_MODE: u32 = 106;
    const SCAN_GUARD: u32 = 107;
    const SCAN_LOOP: u32 = 108;

    #[derive(Clone, Copy)]
    enum ScanMutation {
        None,
        OverflowFalseGuard,
        DifferentActive,
        NonIdentityOffset,
        NonIdentityInputStride,
        NarrowLoopMask,
        WrongBeforeValue,
    }

    fn scan_dynamic_bit(
        arena: &mut SLTNodeArena<u32>,
        variable: u32,
        loop_value: NodeId,
        width: usize,
        stride: usize,
    ) -> NodeId {
        let _ = width;
        arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index: vec![crate::logic_tree::comb::SLTIndex {
                    node: loop_value,
                    stride,
                }],
                access: BitAccess::new(0, 0),
            })
            .unwrap()
    }

    fn synthetic_or_scan_group(
        width: usize,
        mutation: ScanMutation,
    ) -> (SLTNodeArena<u32>, NodeId) {
        let mut arena = SLTNodeArena::new();
        let loop_value = input(&mut arena, SCAN_LOOP, 64);
        let old_vector = input(&mut arena, SCAN_VECTOR_STATE, width);
        let old_found = input(&mut arena, SCAN_FOUND_STATE, 1);
        let source = scan_dynamic_bit(
            &mut arena,
            SCAN_SOURCE,
            loop_value,
            width,
            if matches!(mutation, ScanMutation::NonIdentityInputStride) {
                2
            } else {
                1
            },
        );
        let mask = scan_dynamic_bit(&mut arena, SCAN_MASK, loop_value, width, 1);
        let bound = input(&mut arena, SCAN_BOUND, 8);
        let unmasked = input(&mut arena, SCAN_UNMASKED, 1);
        let mode = input(&mut arena, SCAN_MODE, 2);
        let guard = if matches!(mutation, ScanMutation::OverflowFalseGuard) {
            let one = constant(&mut arena, 1, 1);
            arena
                .alloc(SLTNode::Binary(one, BinaryOp::Add, one))
                .unwrap()
        } else {
            input(&mut arena, SCAN_GUARD, 1)
        };

        let lane_bits = slt_scan_lane_bits(width);
        let valid_lane_mask = (1u64 << lane_bits) - 1;
        let lane_mask = constant(
            &mut arena,
            if matches!(mutation, ScanMutation::NarrowLoopMask) {
                valid_lane_mask >> 1
            } else {
                0xff
            },
            64,
        );
        let truncated_loop = arena
            .alloc(SLTNode::Binary(loop_value, BinaryOp::And, lane_mask))
            .unwrap();
        let in_range = arena
            .alloc(SLTNode::Binary(truncated_loop, BinaryOp::LtU, bound))
            .unwrap();
        let enabled = arena
            .alloc(SLTNode::Binary(unmasked, BinaryOp::LogicOr, mask))
            .unwrap();
        let active = arena
            .alloc(SLTNode::Binary(in_range, BinaryOp::LogicAnd, enabled))
            .unwrap();
        let found_next = arena
            .alloc(SLTNode::Binary(old_found, BinaryOp::LogicOr, source))
            .unwrap();
        let found_update = arena
            .alloc(SLTNode::Mux {
                cond: active,
                then_expr: found_next,
                else_expr: old_found,
            })
            .unwrap();

        let not_found = arena
            .alloc(SLTNode::Unary(UnaryOp::LogicNot, old_found))
            .unwrap();
        let not_source = arena
            .alloc(SLTNode::Unary(UnaryOp::LogicNot, source))
            .unwrap();
        let before = arena
            .alloc(SLTNode::Binary(
                not_found,
                BinaryOp::LogicAnd,
                if matches!(mutation, ScanMutation::WrongBeforeValue) {
                    source
                } else {
                    not_source
                },
            ))
            .unwrap();
        let first = arena
            .alloc(SLTNode::Binary(not_found, BinaryOp::LogicAnd, source))
            .unwrap();
        let one_mode = constant(&mut arena, 1, 2);
        let two_mode = constant(&mut arena, 2, 2);
        let is_before = arena
            .alloc(SLTNode::Binary(mode, BinaryOp::EqWildcard, one_mode))
            .unwrap();
        let is_first = arena
            .alloc(SLTNode::Binary(mode, BinaryOp::EqWildcard, two_mode))
            .unwrap();
        let first_or_through = arena
            .alloc(SLTNode::Mux {
                cond: is_first,
                then_expr: first,
                else_expr: not_found,
            })
            .unwrap();
        let selected = arena
            .alloc(SLTNode::Mux {
                cond: is_before,
                then_expr: before,
                else_expr: first_or_through,
            })
            .unwrap();

        let zero64 = constant(&mut arena, 0, 64);
        let one64 = constant(&mut arena, 1, 64);
        let scaled = arena
            .alloc(SLTNode::Binary(loop_value, BinaryOp::Mul, one64))
            .unwrap();
        let identity_offset = arena
            .alloc(SLTNode::Binary(zero64, BinaryOp::Add, scaled))
            .unwrap();
        let offset = if matches!(mutation, ScanMutation::NonIdentityOffset) {
            arena
                .alloc(SLTNode::Binary(identity_offset, BinaryOp::Add, one64))
                .unwrap()
        } else {
            identity_offset
        };
        let one = constant(&mut arena, 1, width);
        let bit_mask = arena
            .alloc(SLTNode::Binary(one, BinaryOp::Shl, offset))
            .unwrap();
        let inverted_mask = arena
            .alloc(SLTNode::Unary(UnaryOp::BitNot, bit_mask))
            .unwrap();
        let preserved = arena
            .alloc(SLTNode::Binary(old_vector, BinaryOp::And, inverted_mask))
            .unwrap();
        let extended = if width == 1 {
            selected
        } else {
            let zero = constant(&mut arena, 0, width - 1);
            arena
                .alloc(SLTNode::Concat(vec![(zero, width - 1), (selected, 1)]))
                .unwrap()
        };
        let shifted = arena
            .alloc(SLTNode::Binary(extended, BinaryOp::Shl, offset))
            .unwrap();
        let inserted_bit = arena
            .alloc(SLTNode::Binary(shifted, BinaryOp::And, bit_mask))
            .unwrap();
        let inserted = arena
            .alloc(SLTNode::Binary(preserved, BinaryOp::Or, inserted_bit))
            .unwrap();
        let vector_update = arena
            .alloc(SLTNode::Mux {
                cond: if matches!(mutation, ScanMutation::DifferentActive) {
                    source
                } else {
                    active
                },
                then_expr: inserted,
                else_expr: old_vector,
            })
            .unwrap();
        let initial_vector = input(&mut arena, SCAN_VECTOR_STATE, width);
        let initial_found = constant(&mut arena, 0, 1);
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: SCAN_LOOP,
                loop_width: 64,
                loop_signed: false,
                start: BigInt::from(0u8),
                step: BigInt::from(1u8),
                trip_count: width,
                entry_guard: guard,
                states: vec![
                    SLTForFoldGroupState {
                        target: VarAtomBase::new(SCAN_VECTOR_STATE, 0, width - 1),
                        initial: initial_vector,
                        update: vector_update,
                    },
                    SLTForFoldGroupState {
                        target: VarAtomBase::new(SCAN_FOUND_STATE, 0, 0),
                        initial: initial_found,
                        update: found_update,
                    },
                ],
            })
            .unwrap();
        (arena, group)
    }

    fn lower_synthetic_scan(
        width: usize,
        mutation: ScanMutation,
        four_state: bool,
    ) -> (ExecutionUnit<u32>, RegisterId) {
        let (arena, group) = synthetic_or_scan_group(width, mutation);
        let mut builder = SIRBuilder::new();
        let result = SLTToSIRLowerer::new(four_state).lower(
            &mut builder,
            group,
            &arena,
            &mut crate::HashMap::default(),
        );
        (finish_lowering(builder), result)
    }

    fn scan_reference(
        width: usize,
        source: u64,
        mask: u64,
        old: u64,
        bound: u64,
        unmasked: bool,
        mode: u64,
        guard: bool,
    ) -> (u64, bool) {
        if !guard {
            return (old, false);
        }
        let mut result = old;
        let mut found = false;
        for lane in 0..width {
            let active = (lane as u64) < bound && (unmasked || (mask >> lane) & 1 != 0);
            if active {
                let bit = (source >> lane) & 1 != 0;
                let selected = match mode {
                    1 => !found && !bit,
                    2 => !found && bit,
                    _ => !found,
                };
                let lane_mask = 1u64 << lane;
                result = if selected {
                    result | lane_mask
                } else {
                    result & !lane_mask
                };
                found |= bit;
            }
        }
        (result, found)
    }

    #[test]
    fn exact_two_state_or_scan_lowers_without_a_runtime_loop() {
        let (eu, _) = lower_synthetic_scan(8, ScanMutation::None, false);
        assert_eq!(branch_count(&eu), 0);
        assert_eq!(
            instruction_count(&eu, |instruction| matches!(
                instruction,
                SIRInstruction::Unary(_, UnaryOp::Or, _)
            )),
            1
        );
    }

    #[test]
    fn scan_entry_guard_constant_evaluation_uses_bitvector_width() {
        let width = 4;
        let old = 0b1010u64;
        let (eu, result) = lower_synthetic_scan(width, ScanMutation::OverflowFalseGuard, false);
        let memory = crate::HashMap::from_iter([
            (
                SCAN_VECTOR_STATE,
                TestSIRValue {
                    payload: old.into(),
                    mask: 0u8.into(),
                },
            ),
            (
                SCAN_SOURCE,
                TestSIRValue {
                    payload: 0b1111u8.into(),
                    mask: 0u8.into(),
                },
            ),
            (
                SCAN_MASK,
                TestSIRValue {
                    payload: 0b1111u8.into(),
                    mask: 0u8.into(),
                },
            ),
            (
                SCAN_BOUND,
                TestSIRValue {
                    payload: width.into(),
                    mask: 0u8.into(),
                },
            ),
            (
                SCAN_UNMASKED,
                TestSIRValue {
                    payload: 1u8.into(),
                    mask: 0u8.into(),
                },
            ),
            (
                SCAN_MODE,
                TestSIRValue {
                    payload: 2u8.into(),
                    mask: 0u8.into(),
                },
            ),
        ]);

        assert_eq!(branch_count(&eu), 0);
        assert_eq!(
            execute_fold_group_sir_with_memory(&eu, &memory)[&result].payload,
            BigUint::from(old << 1)
        );
    }

    #[test]
    fn word_scan_matches_the_sequential_first_true_semantics_exhaustively() {
        for width in 1..=4 {
            let (eu, result) = lower_synthetic_scan(width, ScanMutation::None, false);
            let values = 1u64 << width;
            for source in 0..values {
                for mask in 0..values {
                    for old in 0..values {
                        for bound in 0..=width as u64 {
                            for unmasked in [false, true] {
                                for mode in 1..=3 {
                                    for guard in [false, true] {
                                        let memory = crate::HashMap::from_iter([
                                            (
                                                SCAN_VECTOR_STATE,
                                                TestSIRValue {
                                                    payload: old.into(),
                                                    mask: 0u8.into(),
                                                },
                                            ),
                                            (
                                                SCAN_SOURCE,
                                                TestSIRValue {
                                                    payload: source.into(),
                                                    mask: 0u8.into(),
                                                },
                                            ),
                                            (
                                                SCAN_MASK,
                                                TestSIRValue {
                                                    payload: mask.into(),
                                                    mask: 0u8.into(),
                                                },
                                            ),
                                            (
                                                SCAN_BOUND,
                                                TestSIRValue {
                                                    payload: bound.into(),
                                                    mask: 0u8.into(),
                                                },
                                            ),
                                            (
                                                SCAN_UNMASKED,
                                                TestSIRValue {
                                                    payload: u8::from(unmasked).into(),
                                                    mask: 0u8.into(),
                                                },
                                            ),
                                            (
                                                SCAN_MODE,
                                                TestSIRValue {
                                                    payload: mode.into(),
                                                    mask: 0u8.into(),
                                                },
                                            ),
                                            (
                                                SCAN_GUARD,
                                                TestSIRValue {
                                                    payload: u8::from(guard).into(),
                                                    mask: 0u8.into(),
                                                },
                                            ),
                                        ]);
                                        let actual =
                                            &execute_fold_group_sir_with_memory(&eu, &memory)
                                                [&result]
                                                .payload;
                                        let (expected_vector, expected_found) = scan_reference(
                                            width, source, mask, old, bound, unmasked, mode, guard,
                                        );
                                        let expected =
                                            (expected_vector << 1) | u64::from(expected_found);
                                        assert_eq!(
                                            actual,
                                            &BigUint::from(expected),
                                            "width={width} source={source:#x} mask={mask:#x} old={old:#x} bound={bound} unmasked={unmasked} mode={mode} guard={guard}",
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn or_scan_matcher_rejects_every_near_miss_and_four_state_mode() {
        for mutation in [
            ScanMutation::DifferentActive,
            ScanMutation::NonIdentityOffset,
            ScanMutation::NonIdentityInputStride,
            ScanMutation::NarrowLoopMask,
            ScanMutation::WrongBeforeValue,
        ] {
            let (eu, _) = lower_synthetic_scan(4, mutation, false);
            assert!(branch_count(&eu) > 0);
        }
        let (eu, _) = lower_synthetic_scan(4, ScanMutation::None, true);
        assert!(branch_count(&eu) > 0);
    }

    #[test]
    fn common_zero_controller_guards_rob_sized_lane_concat() {
        let mut arena = SLTNodeArena::new();
        let (root, guard, first_predicate) = guarded_lane_concat(&mut arena, 32, None);
        let lowerer = SLTToSIRLowerer::new(false);
        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        lowerer.reset_cost_cache(root, &arena, &cache, true);
        assert_eq!(
            lowerer
                .guarded_concat_plan(root, &arena, &cache)
                .expect("ROB-sized guarded scan must be profitable")
                .guard,
            guard,
            "the maximal compound guard must dominate its individual leaves"
        );
        let result = lowerer.lower(&mut builder, root, &arena, &mut cache);

        assert_eq!(cache.get(&root), Some(&result));
        assert!(
            cache.contains_key(&guard),
            "the compound valid/store guard must dominate the outlined region"
        );
        assert!(
            !cache.contains_key(&first_predicate),
            "true-only values must be rolled back at the merge"
        );

        let eu = finish_lowering(builder);
        assert_eq!(branch_count(&eu), 1, "one guard, not one branch per lane");
        assert_eq!(eu.blocks.len(), 4);
        let entry = &eu.blocks[&BlockId(0)];
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &entry.terminator
        else {
            panic!("guarded concat entry must branch")
        };
        assert_eq!(
            entry
                .instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
                .count(),
            2,
            "only valid and is_store may be loaded before the branch"
        );
        assert_eq!(
            eu.blocks[&true_block.0]
                .instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
                .count(),
            32,
            "all lane inputs belong to the selected arm"
        );
        let false_instructions = &eu.blocks[&false_block.0].instructions;
        assert_eq!(false_instructions.len(), 1);
        assert!(matches!(
            &false_instructions[0],
            SIRInstruction::Imm(_, value) if value.payload.is_zero() && value.mask.is_zero()
        ));
        let merge = eu
            .blocks
            .values()
            .find(|block| block.params.contains(&result))
            .expect("guarded concat result must be a merge parameter");
        assert_eq!(eu.register_map[&merge.params[0]].width(), 32);
    }

    #[test]
    fn guarded_concat_cfg_matches_eager_two_state_truth_table() {
        const LANES: usize = 4;
        let mut arena = SLTNodeArena::new();
        let (root, _, _) = guarded_lane_concat(&mut arena, LANES, None);
        let mut builder = SIRBuilder::new();
        let result = SLTToSIRLowerer::new(false).lower(
            &mut builder,
            root,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);
        assert_eq!(branch_count(&eu), 1);

        for valid in [false, true] {
            for is_store in [false, true] {
                for predicates in 0u8..(1 << LANES) {
                    let mut memory = crate::HashMap::from_iter([
                        (
                            10_000,
                            TestSIRValue {
                                payload: u8::from(valid).into(),
                                mask: 0u8.into(),
                            },
                        ),
                        (
                            10_001,
                            TestSIRValue {
                                payload: u8::from(is_store).into(),
                                mask: 0u8.into(),
                            },
                        ),
                    ]);
                    for lane in 0..LANES {
                        let predicate = predicates & (1 << lane) != 0;
                        memory.insert(
                            11_000 + lane as u32,
                            TestSIRValue {
                                payload: if predicate {
                                    BigUint::from(0x8000_0000_0000_0000u64)
                                } else {
                                    BigUint::from(0u8)
                                },
                                mask: BigUint::from(0u8),
                            },
                        );
                    }
                    let actual = &execute_fold_group_sir_with_memory(&eu, &memory)[&result];
                    let mut expected = 0u8;
                    for lane in 0..LANES {
                        expected <<= 1;
                        expected |= u8::from(valid && is_store && predicates & (1 << lane) != 0);
                    }
                    assert_eq!(actual.payload, BigUint::from(expected));
                    assert!(actual.mask.is_zero());
                }
            }
        }
    }

    #[test]
    fn common_zero_controller_rejects_one_ungated_lane() {
        let mut arena = SLTNodeArena::new();
        let (root, _, _) = guarded_lane_concat(&mut arena, 32, Some(17));
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            root,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 0);
        assert_eq!(eu.blocks.len(), 1);
    }

    #[test]
    fn guarded_concat_recomputes_true_only_value_after_merge() {
        let mut arena = SLTNodeArena::new();
        let (root, guard, first_predicate) = guarded_lane_concat(&mut arena, 32, None);
        let lowerer = SLTToSIRLowerer::new(false);
        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        lowerer.lower(&mut builder, root, &arena, &mut cache);
        assert!(cache.contains_key(&guard));
        assert!(!cache.contains_key(&first_predicate));

        let merge_block = builder.current_block();
        let predicate = lowerer.lower(&mut builder, first_predicate, &arena, &mut cache);
        assert_eq!(cache.get(&first_predicate), Some(&predicate));
        let eu = finish_lowering(builder);
        assert!(eu.verify_result().is_ok());
        assert!(eu.blocks[&merge_block].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(dst, _, BinaryOp::GeU, _) if *dst == predicate
            )
        }));
    }

    #[test]
    fn four_state_common_zero_controller_stays_eager() {
        let mut arena = SLTNodeArena::new();
        let (root, _, first_predicate) = guarded_lane_concat(&mut arena, 32, None);
        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        SLTToSIRLowerer::new(true).lower(&mut builder, root, &arena, &mut cache);
        assert!(cache.contains_key(&first_predicate));
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 0);
        assert_eq!(eu.blocks.len(), 1);
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Load(..))),
            34
        );
    }

    #[test]
    fn four_state_guarded_concat_retains_logical_and_mask_semantics() {
        const LANES: usize = 4;
        let mut arena = SLTNodeArena::new();
        let (root, _, _) = guarded_lane_concat(&mut arena, LANES, None);
        let mut builder = SIRBuilder::new();
        let result = SLTToSIRLowerer::new(true).lower(
            &mut builder,
            root,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);
        assert_eq!(branch_count(&eu), 0);

        // Veryl encodes X=(payload 0, mask 1), Z=(payload 1, mask 1).
        for (guard_payload, guard_mask) in [(0u8, 0u8), (1, 0), (0, 1), (1, 1)] {
            for predicates in 0u8..(1 << LANES) {
                let mut memory = crate::HashMap::from_iter([
                    (
                        10_000,
                        TestSIRValue {
                            payload: guard_payload.into(),
                            mask: guard_mask.into(),
                        },
                    ),
                    (
                        10_001,
                        TestSIRValue {
                            payload: 1u8.into(),
                            mask: 0u8.into(),
                        },
                    ),
                ]);
                for lane in 0..LANES {
                    let predicate = predicates & (1 << lane) != 0;
                    memory.insert(
                        11_000 + lane as u32,
                        TestSIRValue {
                            payload: if predicate {
                                BigUint::from(0x8000_0000_0000_0000u64)
                            } else {
                                BigUint::from(0u8)
                            },
                            mask: BigUint::from(0u8),
                        },
                    );
                }

                let actual = &execute_fold_group_sir_with_memory(&eu, &memory)[&result];
                let mut expected_payload = 0u8;
                let mut expected_mask = 0u8;
                for lane in 0..LANES {
                    expected_payload <<= 1;
                    expected_mask <<= 1;
                    let predicate = predicates & (1 << lane) != 0;
                    if guard_mask == 0 {
                        expected_payload |= u8::from(guard_payload != 0 && predicate);
                    } else if predicate {
                        expected_mask |= 1;
                    }
                }
                assert_eq!(actual.payload, BigUint::from(expected_payload));
                assert_eq!(actual.mask, BigUint::from(expected_mask));
            }
        }
    }

    #[test]
    fn cheap_common_zero_controller_stays_eager() {
        let mut arena = SLTNodeArena::new();
        let valid = input(&mut arena, 0, 1);
        let is_store = input(&mut arena, 1, 1);
        let guard = arena
            .alloc(SLTNode::Binary(valid, BinaryOp::LogicAnd, is_store))
            .unwrap();
        let payload = input(&mut arena, 2, 1);
        let lane = arena
            .alloc(SLTNode::Binary(guard, BinaryOp::LogicAnd, payload))
            .unwrap();
        let root = arena.alloc(SLTNode::Concat(vec![(lane, 1)])).unwrap();
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            root,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 0);
    }

    #[test]
    fn zero_controller_analysis_uses_iterative_postorder_on_deep_dag() {
        let mut arena = SLTNodeArena::new();
        let valid = input(&mut arena, 0, 1);
        let is_store = input(&mut arena, 1, 1);
        let guard = arena
            .alloc(SLTNode::Binary(valid, BinaryOp::LogicAnd, is_store))
            .unwrap();
        let mut parts = Vec::new();
        for lane in 0..2 {
            let mut payload = input(&mut arena, 100 + lane, 1);
            for _ in 0..20_000 {
                payload = arena
                    .alloc(SLTNode::Unary(UnaryOp::Ident, payload))
                    .unwrap();
            }
            let gated = arena
                .alloc(SLTNode::Binary(guard, BinaryOp::LogicAnd, payload))
                .unwrap();
            parts.push((gated, 1));
        }
        let root = arena.alloc(SLTNode::Concat(parts)).unwrap();
        let lowerer = SLTToSIRLowerer::new(false);
        let cache = crate::HashMap::default();
        lowerer.reset_cost_cache(root, &arena, &cache, true);

        let plan = lowerer
            .guarded_concat_plan(root, &arena, &cache)
            .expect("deep guarded concat must be analyzed without recursion");
        assert_eq!(plan.guard, guard);
    }

    #[test]
    fn cheap_mux_stays_branchless() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let then_expr = input(&mut arena, 1, 8);
        let else_expr = input(&mut arena, 2, 8);
        let mux = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            })
            .unwrap();
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            mux,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 0);
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            1
        );
    }

    #[test]
    fn sequential_priority_index_becomes_clz_and_subtract() {
        let mut arena = SLTNodeArena::new();
        let mut acc = constant(&mut arena, u64::MAX, 64);
        for index in 0..8 {
            let cond = input(&mut arena, index, 1);
            let value = constant(&mut arena, index as u64, 64);
            acc = arena
                .alloc(SLTNode::Mux {
                    cond,
                    then_expr: value,
                    else_expr: acc,
                })
                .unwrap();
        }

        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            acc,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            0
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Unary(_, UnaryOp::CountLeadingZeros, _)
            )),
            1
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Binary(_, _, BinaryOp::Sub, _)
            )),
            1
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Concat(_, args) if args.len() == 8
            )),
            1
        );
    }

    #[test]
    fn nested_conditional_priority_writes_use_combined_predicates() {
        let mut arena = SLTNodeArena::new();
        let mut acc = constant(&mut arena, u64::MAX, 64);
        for index in 0..8 {
            let outer = input(&mut arena, index * 2, 1);
            let inner = input(&mut arena, index * 2 + 1, 1);
            let value = constant(&mut arena, index as u64, 64);
            let write = arena
                .alloc(SLTNode::Mux {
                    cond: inner,
                    then_expr: value,
                    else_expr: acc,
                })
                .unwrap();
            acc = arena
                .alloc(SLTNode::Mux {
                    cond: outer,
                    then_expr: write,
                    else_expr: acc,
                })
                .unwrap();
        }

        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            acc,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            0
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Binary(_, _, BinaryOp::LogicAnd, _)
            )),
            8
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Unary(_, UnaryOp::CountLeadingZeros, _)
            )),
            1
        );
    }

    #[test]
    fn first_write_found_recurrence_uses_outer_predicates_and_ctz() {
        let mut arena = SLTNodeArena::new();
        let mut acc = constant(&mut arena, u64::MAX, 64);
        let mut found = constant(&mut arena, 0, 1);
        let one = constant(&mut arena, 1, 1);
        for index in 0..8 {
            let outer = input(&mut arena, index, 1);
            let not_found = arena
                .alloc(SLTNode::Unary(UnaryOp::LogicNot, found))
                .unwrap();
            let value = constant(&mut arena, index as u64, 64);
            let write = arena
                .alloc(SLTNode::Mux {
                    cond: not_found,
                    then_expr: value,
                    else_expr: acc,
                })
                .unwrap();
            acc = arena
                .alloc(SLTNode::Mux {
                    cond: outer,
                    then_expr: write,
                    else_expr: acc,
                })
                .unwrap();

            let set_found = arena
                .alloc(SLTNode::Mux {
                    cond: not_found,
                    then_expr: one,
                    else_expr: found,
                })
                .unwrap();
            found = arena
                .alloc(SLTNode::Mux {
                    cond: outer,
                    then_expr: set_found,
                    else_expr: found,
                })
                .unwrap();
        }

        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            acc,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            1,
            "only the zero-input sentinel select should remain"
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Unary(_, UnaryOp::CountTrailingZeros, _)
            )),
            1
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Concat(_, args) if args.len() == 8
            )),
            1
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Unary(_, UnaryOp::LogicNot, _)
            )),
            0,
            "the found prefix recurrence must not be lowered"
        );
    }

    #[test]
    fn conditionally_seeded_priority_count_preserves_fallback() {
        let width = 8;
        let result_width = UnaryOp::CountLeadingZeros.result_width(width);
        let mut arena = SLTNodeArena::new();
        let source = input(&mut arena, 0, width);
        let gate = input(&mut arena, 1, 1);
        let fallback = input(&mut arena, 2, result_width);
        let sentinel = constant(&mut arena, width as u64, result_width);
        let mut acc = arena
            .alloc(SLTNode::Mux {
                cond: gate,
                then_expr: sentinel,
                else_expr: fallback,
            })
            .unwrap();

        for value in 0..width {
            let bit = arena
                .alloc(SLTNode::Slice {
                    expr: source,
                    access: BitAccess::new(width - 1 - value, width - 1 - value),
                })
                .unwrap();
            let unmatched = arena
                .alloc(SLTNode::Binary(acc, BinaryOp::Eq, sentinel))
                .unwrap();
            let write = arena
                .alloc(SLTNode::Binary(bit, BinaryOp::LogicAnd, unmatched))
                .unwrap();
            let value = constant(&mut arena, value as u64, result_width);
            let candidate = arena
                .alloc(SLTNode::Mux {
                    cond: gate,
                    then_expr: value,
                    else_expr: acc,
                })
                .unwrap();
            acc = arena
                .alloc(SLTNode::Mux {
                    cond: write,
                    then_expr: candidate,
                    else_expr: acc,
                })
                .unwrap();
        }

        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            acc,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Unary(_, UnaryOp::CountLeadingZeros, _)
            )),
            1
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            1,
            "only gate ? clz(source) : fallback should remain"
        );
    }

    #[derive(Clone, Copy, Debug)]
    enum ConditionalPriorityCorruption {
        StageGate(usize),
        SeedGate,
        SeedDefault,
        StageFallback(usize),
        BitOrder(usize),
        ValueOrder(usize),
    }

    fn corrupted_conditional_priority(
        corruption: ConditionalPriorityCorruption,
    ) -> (SLTNodeArena<u32>, NodeId) {
        let width = 8;
        let result_width = UnaryOp::CountLeadingZeros.result_width(width);
        let mut arena = SLTNodeArena::new();
        let source = input(&mut arena, 0, width);
        let gate = input(&mut arena, 1, 1);
        let other_gate = input(&mut arena, 2, 1);
        let fallback = input(&mut arena, 3, result_width);
        let other_fallback = input(&mut arena, 4, result_width);
        let sentinel = constant(&mut arena, width as u64, result_width);
        let other_default = constant(&mut arena, width as u64 - 1, result_width);
        let seed_gate = if matches!(corruption, ConditionalPriorityCorruption::SeedGate) {
            other_gate
        } else {
            gate
        };
        let seed_default = if matches!(corruption, ConditionalPriorityCorruption::SeedDefault) {
            other_default
        } else {
            sentinel
        };
        let mut acc = arena
            .alloc(SLTNode::Mux {
                cond: seed_gate,
                then_expr: seed_default,
                else_expr: fallback,
            })
            .unwrap();

        for stage in 0..width {
            let source_bit = if matches!(
                corruption,
                ConditionalPriorityCorruption::BitOrder(corrupt_stage)
                    if corrupt_stage == stage
            ) {
                width - 1 - ((stage + 1) % width)
            } else {
                width - 1 - stage
            };
            let bit = arena
                .alloc(SLTNode::Slice {
                    expr: source,
                    access: BitAccess::new(source_bit, source_bit),
                })
                .unwrap();
            let unmatched = arena
                .alloc(SLTNode::Binary(acc, BinaryOp::Eq, sentinel))
                .unwrap();
            let write = arena
                .alloc(SLTNode::Binary(bit, BinaryOp::LogicAnd, unmatched))
                .unwrap();
            let selected_value = if matches!(
                corruption,
                ConditionalPriorityCorruption::ValueOrder(corrupt_stage)
                    if corrupt_stage == stage
            ) {
                (stage + 1) % width
            } else {
                stage
            };
            let value = constant(&mut arena, selected_value as u64, result_width);
            let stage_gate = if matches!(
                corruption,
                ConditionalPriorityCorruption::StageGate(corrupt_stage)
                    if corrupt_stage == stage
            ) {
                other_gate
            } else {
                gate
            };
            let stage_fallback = if matches!(
                corruption,
                ConditionalPriorityCorruption::StageFallback(corrupt_stage)
                    if corrupt_stage == stage
            ) {
                other_fallback
            } else {
                acc
            };
            let candidate = arena
                .alloc(SLTNode::Mux {
                    cond: stage_gate,
                    then_expr: value,
                    else_expr: stage_fallback,
                })
                .unwrap();
            acc = arena
                .alloc(SLTNode::Mux {
                    cond: write,
                    then_expr: candidate,
                    else_expr: acc,
                })
                .unwrap();
        }
        (arena, acc)
    }

    fn assert_conditional_priority_rejected(corruption: ConditionalPriorityCorruption) {
        let (arena, root) = corrupted_conditional_priority(corruption);
        assert!(
            match_slt_priority_count(root, &arena).is_none(),
            "conditionally seeded priority chain with {corruption:?} must not match"
        );
    }

    #[test]
    fn conditional_priority_rejects_mismatched_per_stage_gate() {
        assert_conditional_priority_rejected(ConditionalPriorityCorruption::StageGate(3));
    }

    #[test]
    fn conditional_priority_rejects_mismatched_seed_gate_and_default() {
        assert_conditional_priority_rejected(ConditionalPriorityCorruption::SeedGate);
        assert_conditional_priority_rejected(ConditionalPriorityCorruption::SeedDefault);
    }

    #[test]
    fn conditional_priority_rejects_mismatched_stage_fallback() {
        assert_conditional_priority_rejected(ConditionalPriorityCorruption::StageFallback(3));
    }

    #[test]
    fn conditional_priority_rejects_reordered_bit_or_value() {
        assert_conditional_priority_rejected(ConditionalPriorityCorruption::BitOrder(3));
        assert_conditional_priority_rejected(ConditionalPriorityCorruption::ValueOrder(3));
    }

    #[test]
    fn additive_popcount_accepts_only_lsb_zero_extension() {
        let mut arena = SLTNodeArena::new();
        let bit = input(&mut arena, 0, 1);
        let zero3 = constant(&mut arena, 0, 3);
        let lsb_extended = arena
            .alloc(SLTNode::Concat(vec![(zero3, 3), (bit, 1)]))
            .unwrap();
        let msb_shifted = arena
            .alloc(SLTNode::Concat(vec![(bit, 1), (zero3, 3)]))
            .unwrap();
        let wide = input(&mut arena, 1, 4);
        let multi_bit_slice = arena
            .alloc(SLTNode::Slice {
                expr: wide,
                access: BitAccess::new(0, 1),
            })
            .unwrap();

        assert!(resolve_slt_extended_bit(lsb_extended, &arena).is_some());
        assert!(resolve_slt_extended_bit(msb_shifted, &arena).is_none());
        assert!(resolve_slt_extended_bit(multi_bit_slice, &arena).is_none());
    }

    #[test]
    fn procedural_truth_unwrap_keeps_wide_reduction() {
        let mut arena = SLTNodeArena::new();
        let wide = input(&mut arena, 0, 4);
        let truth = arena.alloc(SLTNode::Unary(UnaryOp::Or, wide)).unwrap();
        let normalized = arena
            .alloc(SLTNode::Unary(UnaryOp::ToTwoState, truth))
            .unwrap();

        assert_eq!(
            unwrap_slt_one_bit_procedural_truth(normalized, &arena),
            normalized,
            "a wide reduction is a real booleanization, not an identity"
        );
    }

    #[test]
    fn cached_popcount_accumulator_only_lowers_new_increment() {
        let width = 8;
        let result_width = 4;
        let mut arena = SLTNodeArena::new();
        let source = input(&mut arena, 0, width);
        let one = constant(&mut arena, 1, result_width);
        let mut base = constant(&mut arena, 0, result_width);
        for bit in 0..width {
            let predicate = arena
                .alloc(SLTNode::Slice {
                    expr: source,
                    access: BitAccess::new(bit, bit),
                })
                .unwrap();
            let incremented = arena
                .alloc(SLTNode::Binary(base, BinaryOp::Add, one))
                .unwrap();
            base = arena
                .alloc(SLTNode::Mux {
                    cond: predicate,
                    then_expr: incremented,
                    else_expr: base,
                })
                .unwrap();
        }

        let delta = input(&mut arena, 1, 1);
        let incremented = arena
            .alloc(SLTNode::Binary(base, BinaryOp::Add, one))
            .unwrap();
        let root = arena
            .alloc(SLTNode::Mux {
                cond: delta,
                then_expr: incremented,
                else_expr: base,
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        let lowerer = SLTToSIRLowerer::new(false);
        let base_reg = lowerer.lower(&mut builder, base, &arena, &mut cache);
        let root_reg = lowerer.lower(&mut builder, root, &arena, &mut cache);
        let eu = finish_lowering(builder);

        assert_eq!(
            instruction_count(&eu, |instruction| matches!(
                instruction,
                SIRInstruction::Unary(_, UnaryOp::PopCount, _)
            )),
            1,
            "the already materialized population count must not be rebuilt"
        );
        assert_eq!(
            instruction_count(&eu, |instruction| matches!(
                instruction,
                SIRInstruction::Concat(_, arguments) if arguments.len() == width + 1
            )),
            0
        );
        assert!(
            eu.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(
                    instruction,
                    SIRInstruction::Binary(dst, lhs, BinaryOp::Add, _)
                        if *dst == root_reg && *lhs == base_reg
                ))
        );
    }

    #[test]
    fn cached_additive_popcount_accumulator_only_lowers_new_bit() {
        let width = 8;
        let result_width = 4;
        let mut arena = SLTNodeArena::new();
        let source = input(&mut arena, 0, width);
        let zero = constant(&mut arena, 0, result_width);
        let padding = constant(&mut arena, 0, result_width - 1);
        let mut base = zero;
        for bit in 0..width {
            let predicate = arena
                .alloc(SLTNode::Slice {
                    expr: source,
                    access: BitAccess::new(bit, bit),
                })
                .unwrap();
            let extended = arena
                .alloc(SLTNode::Concat(vec![
                    (padding, result_width - 1),
                    (predicate, 1),
                ]))
                .unwrap();
            base = arena
                .alloc(SLTNode::Binary(base, BinaryOp::Add, extended))
                .unwrap();
        }

        let delta = input(&mut arena, 1, 1);
        let extended_delta = arena
            .alloc(SLTNode::Concat(vec![
                (padding, result_width - 1),
                (delta, 1),
            ]))
            .unwrap();
        let root = arena
            .alloc(SLTNode::Binary(base, BinaryOp::Add, extended_delta))
            .unwrap();

        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        let lowerer = SLTToSIRLowerer::new(false);
        let base_reg = lowerer.lower(&mut builder, base, &arena, &mut cache);
        let root_reg = lowerer.lower(&mut builder, root, &arena, &mut cache);
        let eu = finish_lowering(builder);

        assert_eq!(
            instruction_count(&eu, |instruction| matches!(
                instruction,
                SIRInstruction::Unary(_, UnaryOp::PopCount, _)
            )),
            1
        );
        assert!(
            eu.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(
                    instruction,
                    SIRInstruction::Binary(dst, lhs, BinaryOp::Add, _)
                        if *dst == root_reg && *lhs == base_reg
                ))
        );
    }

    #[test]
    fn cached_popcount_delta_preserves_wrapping_semantics() {
        let width = 7;
        let result_width = 3;
        let mut arena = SLTNodeArena::new();
        let source = input(&mut arena, 0, width);
        let one = constant(&mut arena, 1, result_width);
        let mut base = constant(&mut arena, 0, result_width);
        for bit in 0..width {
            let predicate = arena
                .alloc(SLTNode::Slice {
                    expr: source,
                    access: BitAccess::new(bit, bit),
                })
                .unwrap();
            let incremented = arena
                .alloc(SLTNode::Binary(base, BinaryOp::Add, one))
                .unwrap();
            base = arena
                .alloc(SLTNode::Mux {
                    cond: predicate,
                    then_expr: incremented,
                    else_expr: base,
                })
                .unwrap();
        }
        let delta = input(&mut arena, 1, 1);
        let incremented = arena
            .alloc(SLTNode::Binary(base, BinaryOp::Add, one))
            .unwrap();
        let root = arena
            .alloc(SLTNode::Mux {
                cond: delta,
                then_expr: incremented,
                else_expr: base,
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        let lowerer = SLTToSIRLowerer::new(false);
        lowerer.lower(&mut builder, base, &arena, &mut cache);
        let result = lowerer.lower(&mut builder, root, &arena, &mut cache);
        let eu = finish_lowering(builder);

        assert_eq!(
            instruction_count(&eu, |instruction| matches!(
                instruction,
                SIRInstruction::Unary(_, UnaryOp::PopCount, _)
            )),
            1
        );

        for source_value in 0u64..(1 << width) {
            for delta_value in 0u64..=1 {
                let mut memory = crate::HashMap::default();
                memory.insert(
                    0u32,
                    TestSIRValue {
                        payload: source_value.into(),
                        mask: 0u8.into(),
                    },
                );
                memory.insert(
                    1u32,
                    TestSIRValue {
                        payload: delta_value.into(),
                        mask: 0u8.into(),
                    },
                );
                let actual = &execute_fold_group_sir_with_memory(&eu, &memory)[&result].payload;
                let expected =
                    (source_value.count_ones() as u64 + delta_value) & ((1 << result_width) - 1);
                assert_eq!(actual, &BigUint::from(expected));
            }
        }
    }

    #[test]
    fn cached_accumulator_is_not_reused_for_non_unit_update() {
        let width = 8;
        let result_width = 4;
        let mut arena = SLTNodeArena::new();
        let source = input(&mut arena, 0, width);
        let one = constant(&mut arena, 1, result_width);
        let two = constant(&mut arena, 2, result_width);
        let mut base = constant(&mut arena, 0, result_width);
        for bit in 0..width {
            let predicate = arena
                .alloc(SLTNode::Slice {
                    expr: source,
                    access: BitAccess::new(bit, bit),
                })
                .unwrap();
            let incremented = arena
                .alloc(SLTNode::Binary(base, BinaryOp::Add, one))
                .unwrap();
            base = arena
                .alloc(SLTNode::Mux {
                    cond: predicate,
                    then_expr: incremented,
                    else_expr: base,
                })
                .unwrap();
        }
        let delta = input(&mut arena, 1, 1);
        let incremented = arena
            .alloc(SLTNode::Binary(base, BinaryOp::Add, two))
            .unwrap();
        let root = arena
            .alloc(SLTNode::Mux {
                cond: delta,
                then_expr: incremented,
                else_expr: base,
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        let lowerer = SLTToSIRLowerer::new(false);
        lowerer.lower(&mut builder, base, &arena, &mut cache);
        lowerer.lower(&mut builder, root, &arena, &mut cache);
        let eu = finish_lowering(builder);

        assert_eq!(
            instruction_count(&eu, |instruction| matches!(
                instruction,
                SIRInstruction::Mux(..)
            )),
            1,
            "only an exact conditional +1 update may become a count delta"
        );
    }

    #[test]
    fn active_bit_predicate_family_becomes_one_wide_expression() {
        let width = 8;
        let mut arena = SLTNodeArena::new();
        let bound = input(&mut arena, 0, 4);
        let vm = input(&mut arena, 1, 1);
        let zero = constant(&mut arena, 0, 4);
        let one = constant(&mut arena, 1, 4);
        let mut acc = zero;

        for index in 0..width {
            let index_value = constant(&mut arena, index as u64, 4);
            let in_range = arena
                .alloc(SLTNode::Binary(index_value, BinaryOp::LtU, bound))
                .unwrap();
            let mask = input_bit(&mut arena, 2, index);
            let enabled = arena
                .alloc(SLTNode::Binary(vm, BinaryOp::LogicOr, mask))
                .unwrap();
            let eligible = arena
                .alloc(SLTNode::Binary(in_range, BinaryOp::LogicAnd, enabled))
                .unwrap();
            let source = input_bit(&mut arena, 3, index);
            let active = arena
                .alloc(SLTNode::Binary(eligible, BinaryOp::LogicAnd, source))
                .unwrap();
            let incremented = arena
                .alloc(SLTNode::Binary(acc, BinaryOp::Add, one))
                .unwrap();
            acc = arena
                .alloc(SLTNode::Mux {
                    cond: active,
                    then_expr: incremented,
                    else_expr: acc,
                })
                .unwrap();
        }

        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            acc,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Unary(_, UnaryOp::PopCount, _)
            )),
            1
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Binary(_, _, BinaryOp::LtU, _)
            )),
            0,
            "the ordered comparison ladder must become a low-ones mask"
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            1,
            "the saturated low-ones mask needs one word-level select"
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Concat(_, args) if args.len() == width
            )),
            0,
            "the scalar active predicates must not be reassembled one bit at a time"
        );
    }

    #[test]
    fn low_ones_saturates_when_only_a_wide_bound_high_limb_is_set() {
        let mut arena = SLTNodeArena::new();
        let bound = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8) << 64,
                BigUint::from(0u8),
                128,
                false,
            ))
            .unwrap();
        let mut builder = SIRBuilder::new();
        let result = SLTToSIRLowerer::new(false).lower_slt_vector_expr(
            &mut builder,
            SLTVectorExpr::LowOnes { bound },
            8,
            &arena,
            &mut crate::HashMap::default(),
            true,
        );
        let eu = finish_lowering(builder);

        assert_eq!(
            execute_fold_group_sir(&eu)[&result].payload,
            BigUint::from(0xffu8)
        );
        assert_eq!(
            instruction_count(&eu, |instruction| matches!(
                instruction,
                SIRInstruction::Binary(_, _, BinaryOp::GeU, _)
            )),
            1
        );
    }

    #[test]
    fn masked_found_recurrence_becomes_wide_or_reduction() {
        let width = 8;
        let mut arena = SLTNodeArena::new();
        let bound = input(&mut arena, 0, 4);
        let vm = input(&mut arena, 1, 1);
        let mut found = constant(&mut arena, 0, 1);

        for index in 0..width {
            let index_value = constant(&mut arena, index as u64, 4);
            let in_range = arena
                .alloc(SLTNode::Binary(index_value, BinaryOp::LtU, bound))
                .unwrap();
            let mask = input_bit(&mut arena, 2, index);
            let enabled = arena
                .alloc(SLTNode::Binary(vm, BinaryOp::LogicOr, mask))
                .unwrap();
            let eligible = arena
                .alloc(SLTNode::Binary(in_range, BinaryOp::LogicAnd, enabled))
                .unwrap();
            let source = input_bit(&mut arena, 3, index);
            let set = arena
                .alloc(SLTNode::Binary(found, BinaryOp::LogicOr, source))
                .unwrap();
            found = arena
                .alloc(SLTNode::Mux {
                    cond: eligible,
                    then_expr: set,
                    else_expr: found,
                })
                .unwrap();
        }

        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            found,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Unary(_, UnaryOp::Or, _)
            )),
            1
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            1,
            "the saturated low-ones mask needs one word-level select"
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Binary(_, _, BinaryOp::LtU, _)
            )),
            0
        );
    }

    #[test]
    fn expected_cost_uses_static_equality_probability() {
        let even = StaticBranchProbability::EVEN;
        let equality = StaticBranchProbability {
            true_weight: 1,
            total_weight: 5,
        };

        // With a 50/50 prior, ten units in the true arm cannot repay the
        // expected branch miss.  When equality is predicted false, 80% of that
        // arm is skipped and the same transformation is profitable.
        assert!(!SLTToSIRLowerer::mux_cfg_is_profitable(10, 0, 64, even));
        assert!(SLTToSIRLowerer::mux_cfg_is_profitable(10, 0, 64, equality));
        assert!(!SLTToSIRLowerer::mux_cfg_is_profitable(
            10,
            0,
            64,
            equality.inverted(),
        ));
    }

    #[test]
    fn wildcard_equality_uses_the_decoder_bias() {
        let mut arena = SLTNodeArena::new();
        let selector = input(&mut arena, 0, 8);
        let opcode = constant(&mut arena, 0x13, 8);
        let eq = arena
            .alloc(SLTNode::Binary(selector, BinaryOp::EqWildcard, opcode))
            .unwrap();
        let ne = arena
            .alloc(SLTNode::Binary(selector, BinaryOp::NeWildcard, opcode))
            .unwrap();

        let eq_probability = SLTToSIRLowerer::static_true_probability(eq, &arena);
        let ne_probability = SLTToSIRLowerer::static_true_probability(ne, &arena);
        assert_eq!(
            (eq_probability.true_weight, eq_probability.total_weight),
            (1, 5)
        );
        assert_eq!(
            (ne_probability.true_weight, ne_probability.total_weight),
            (4, 5)
        );
    }

    #[test]
    fn expensive_mux_preserves_control_flow_and_verifies() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let then_input = input(&mut arena, 1, 64);
        let else_input = input(&mut arena, 2, 64);
        let then_expr = operation_chain(&mut arena, then_input, BinaryOp::Add, 8, 10, 64);
        let else_expr = operation_chain(&mut arena, else_input, BinaryOp::Xor, 12, 100, 64);
        let mux = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            })
            .unwrap();
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            mux,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 1);
        assert_eq!(eu.blocks.len(), 4);
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            0
        );
    }

    #[test]
    fn shared_arm_dag_is_hoisted_once() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let source = input(&mut arena, 1, 64);
        let shared = operation_chain(&mut arena, source, BinaryOp::Mul, 3, 3, 64);
        let then_source = input(&mut arena, 2, 64);
        let else_source = input(&mut arena, 3, 64);
        let then_unique = operation_chain(&mut arena, then_source, BinaryOp::Add, 5, 20, 64);
        let else_unique = operation_chain(&mut arena, else_source, BinaryOp::Sub, 5, 40, 64);
        let then_expr = arena
            .alloc(SLTNode::Binary(shared, BinaryOp::Add, then_unique))
            .unwrap();
        let else_expr = arena
            .alloc(SLTNode::Binary(shared, BinaryOp::Sub, else_unique))
            .unwrap();
        let mux = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            })
            .unwrap();
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            mux,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 1);
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Binary(_, _, BinaryOp::Mul, _)
            )),
            3,
        );
        let entry = &eu.blocks[&BlockId(0)];
        assert_eq!(
            entry
                .instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::Mul, _)))
                .count(),
            3,
        );
    }

    #[test]
    fn nested_cost_directed_muxes_form_valid_ssa() {
        let mut arena = SLTNodeArena::new();
        let outer_cond = input(&mut arena, 0, 1);
        let inner_cond = input(&mut arena, 1, 1);
        let a = input(&mut arena, 2, 64);
        let b = input(&mut arena, 3, 64);
        let c = input(&mut arena, 4, 64);
        let inner_then = operation_chain(&mut arena, a, BinaryOp::Add, 8, 10, 64);
        let inner_else = operation_chain(&mut arena, b, BinaryOp::Sub, 8, 30, 64);
        let inner = arena
            .alloc(SLTNode::Mux {
                cond: inner_cond,
                then_expr: inner_then,
                else_expr: inner_else,
            })
            .unwrap();
        let outer_else = operation_chain(&mut arena, c, BinaryOp::Xor, 16, 70, 64);
        let outer = arena
            .alloc(SLTNode::Mux {
                cond: outer_cond,
                then_expr: inner,
                else_expr: outer_else,
            })
            .unwrap();
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            outer,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 2);
    }

    #[test]
    fn deep_division_forces_cfg_and_casts_merge_width() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let narrow = input(&mut arena, 1, 8);
        let numerator = input(&mut arena, 2, 16);
        let denominator = input(&mut arena, 3, 16);
        let quotient = arena
            .alloc(SLTNode::Binary(numerator, BinaryOp::DivU, denominator))
            .unwrap();
        let one = constant(&mut arena, 1, 16);
        let deep_division = arena
            .alloc(SLTNode::Binary(quotient, BinaryOp::Add, one))
            .unwrap();
        let mux = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr: narrow,
                else_expr: deep_division,
            })
            .unwrap();
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            mux,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 1);
        let merge = eu
            .blocks
            .values()
            .find(|block| !block.params.is_empty())
            .unwrap();
        assert_eq!(eu.register_map[&merge.params[0]].width(), 16);
    }

    #[test]
    fn four_state_expensive_mux_keeps_xz_select_semantics() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let then_input = input(&mut arena, 1, 64);
        let else_input = input(&mut arena, 2, 64);
        let then_expr = operation_chain(&mut arena, then_input, BinaryOp::Add, 10, 10, 64);
        let else_expr = operation_chain(&mut arena, else_input, BinaryOp::Sub, 10, 30, 64);
        let mux = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            })
            .unwrap();
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(true).lower(&mut builder, mux, &arena, &mut crate::HashMap::default());
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 0);
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            1
        );
    }

    #[test]
    fn for_fold_is_not_a_pure_mux_arm() {
        let mut arena = SLTNodeArena::new();
        let initial = input(&mut arena, 0, 8);
        let update = input(&mut arena, 1, 8);
        let continue_cond = constant(&mut arena, 1, 1);
        let target = VarAtomBase::new(2, 0, 7);
        let fold = arena
            .alloc(SLTNode::ForFold {
                loop_var: 3,
                loop_width: 8,
                loop_signed: false,
                start: SLTLoopBound::Const(0),
                end: SLTLoopBound::Const(2),
                inclusive: false,
                step: 1,
                step_op: SLTStepOp::Add,
                reverse: false,
                result: target.clone(),
                initials: vec![crate::logic_tree::comb::SLTForUpdate {
                    target: target.clone(),
                    expr: initial,
                }],
                updates: vec![crate::logic_tree::comb::SLTForUpdate {
                    target,
                    expr: update,
                }],
                effects: vec![crate::logic_tree::comb::SLTForEffect {
                    site_id: 1,
                    guard: None,
                    emit_on_true: true,
                    args: vec![update],
                    fatal_error_code: None,
                }],
                continue_cond,
            })
            .unwrap();

        assert!(!SLTToSIRLowerer::new(false).is_speculatable_pure(fold, &arena));
    }

    #[test]
    fn joint_fold_groups_share_one_counted_backedge() {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 1, 1);
        let initial = constant(&mut arena, 0, 8);
        let one = constant(&mut arena, 1, 8);
        let previous_a = input(&mut arena, 10, 8);
        let previous_b = input(&mut arena, 11, 8);
        let update_a = arena
            .alloc(SLTNode::Binary(previous_a, BinaryOp::Add, one))
            .unwrap();
        let update_b = arena
            .alloc(SLTNode::Binary(previous_b, BinaryOp::Add, one))
            .unwrap();
        let group_a = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 20,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 3,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(10, 0, 7),
                    initial,
                    update: update_a,
                }],
            })
            .unwrap();
        let group_b = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 21,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 3,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(11, 0, 7),
                    initial,
                    update: update_b,
                }],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        assert!(SLTToSIRLowerer::new(false).lower_fold_groups_jointly(
            &mut builder,
            &[group_a, group_b],
            &arena,
            &mut cache,
        ));
        let result_a = cache[&group_a];
        let result_b = cache[&group_b];
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 2, "one entry branch and one backedge");
        assert_eq!(
            eu.blocks
                .values()
                .filter(|block| matches!(
                    &block.terminator,
                    SIRTerminator::Branch { true_block, .. } if true_block.0 == block.id
                ))
                .count(),
            1,
        );
        let values = execute_fold_group_sir(&eu);
        assert_eq!(values[&result_a].payload, BigUint::from(3u8));
        assert_eq!(values[&result_b].payload, BigUint::from(3u8));
    }

    #[test]
    fn joint_fold_groups_keep_all_updates_simultaneous() {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 1, 1);
        let initial_a = constant(&mut arena, 0x12, 8);
        let initial_b = constant(&mut arena, 0x34, 8);
        let initial_c = constant(&mut arena, 7, 8);
        let previous_a = input(&mut arena, 10, 8);
        let previous_b = input(&mut arena, 11, 8);
        let previous_c = input(&mut arena, 12, 8);
        let one = constant(&mut arena, 1, 8);
        let update_c = arena
            .alloc(SLTNode::Binary(previous_c, BinaryOp::Add, one))
            .unwrap();
        let swap_group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 20,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 3,
                entry_guard: guard,
                states: vec![
                    SLTForFoldGroupState {
                        target: VarAtomBase::new(10, 0, 7),
                        initial: initial_a,
                        update: previous_b,
                    },
                    SLTForFoldGroupState {
                        target: VarAtomBase::new(11, 0, 7),
                        initial: initial_b,
                        update: previous_a,
                    },
                ],
            })
            .unwrap();
        let increment_group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 21,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 3,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(12, 0, 7),
                    initial: initial_c,
                    update: update_c,
                }],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        assert!(SLTToSIRLowerer::new(false).lower_fold_groups_jointly(
            &mut builder,
            &[swap_group, increment_group],
            &arena,
            &mut cache,
        ));
        let swap_result = cache[&swap_group];
        let increment_result = cache[&increment_group];
        let values = execute_fold_group_sir(&finish_lowering(builder));
        assert_eq!(values[&swap_result].payload, BigUint::from(0x3412u16));
        assert_eq!(values[&increment_result].payload, BigUint::from(10u8));
    }

    #[test]
    fn joint_fold_groups_reject_mismatched_domains_atomically() {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 1, 1);
        let initial = constant(&mut arena, 0, 8);
        let update_a = input(&mut arena, 10, 8);
        let update_b = input(&mut arena, 11, 8);
        let make_group = |arena: &mut SLTNodeArena<u32>, loop_var, target, update, trip_count| {
            arena
                .alloc(SLTNode::ForFoldGroup {
                    loop_var,
                    loop_width: 8,
                    loop_signed: false,
                    start: BigInt::from(0),
                    step: BigInt::from(1),
                    trip_count,
                    entry_guard: guard,
                    states: vec![SLTForFoldGroupState {
                        target: VarAtomBase::new(target, 0, 7),
                        initial,
                        update,
                    }],
                })
                .unwrap()
        };
        let group_a = make_group(&mut arena, 20, 10, update_a, 3);
        let group_b = make_group(&mut arena, 21, 11, update_b, 4);

        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        assert!(!SLTToSIRLowerer::new(false).lower_fold_groups_jointly(
            &mut builder,
            &[group_a, group_b],
            &arena,
            &mut cache,
        ));
        assert!(cache.is_empty());
        assert_eq!(builder.block_count(), 1);
        let value = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Imm(value, SIRValue::new(1u8)));
        let eu = finish_lowering(builder);
        assert_eq!(branch_count(&eu), 0);
        assert_eq!(eu.blocks[&BlockId(0)].instructions.len(), 1);
    }

    #[test]
    fn joint_fold_groups_accept_pre_loop_target_initials() {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 1, 1);
        let previous_a = input(&mut arena, 10, 8);
        let previous_b = input(&mut arena, 11, 8);
        let make_group = |arena: &mut SLTNodeArena<u32>, loop_var, target, previous| {
            arena
                .alloc(SLTNode::ForFoldGroup {
                    loop_var,
                    loop_width: 8,
                    loop_signed: false,
                    start: BigInt::from(0),
                    step: BigInt::from(1),
                    trip_count: 2,
                    entry_guard: guard,
                    states: vec![SLTForFoldGroupState {
                        target: VarAtomBase::new(target, 0, 7),
                        initial: previous,
                        update: previous,
                    }],
                })
                .unwrap()
        };
        let group_a = make_group(&mut arena, 20, 10, previous_a);
        let group_b = make_group(&mut arena, 21, 11, previous_b);

        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        assert!(SLTToSIRLowerer::new(false).lower_fold_groups_jointly(
            &mut builder,
            &[group_a, group_b],
            &arena,
            &mut cache,
        ));
        assert!(cache.contains_key(&group_a));
        assert!(cache.contains_key(&group_b));
        let eu = finish_lowering(builder);
        assert_eq!(branch_count(&eu), 2);
    }

    #[test]
    fn fold_body_cost_analysis_uses_the_environment_scoped_cache() {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 1, 1);
        let initial = constant(&mut arena, 8, 8);
        let previous = input(&mut arena, 10, 8);
        let cond = input_bit(&mut arena, 10, 0);
        let divisor = constant(&mut arena, 2, 8);
        let quotient = arena
            .alloc(SLTNode::Binary(previous, BinaryOp::DivU, divisor))
            .unwrap();
        let update = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr: quotient,
                else_expr: previous,
            })
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 20,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 2,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(10, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let unavailable_outer_value = builder.alloc_logic(8);
        builder.emit(SIRInstruction::Imm(
            unavailable_outer_value,
            SIRValue::new(0u8),
        ));
        let mut cache = crate::HashMap::default();
        cache.insert(quotient, unavailable_outer_value);
        SLTToSIRLowerer::new(false).lower(&mut builder, group, &arena, &mut cache);
        let eu = finish_lowering(builder);

        assert_eq!(
            branch_count(&eu),
            3,
            "the body-local division arm must retain its mandatory lazy branch"
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(
                inst,
                SIRInstruction::Binary(_, _, BinaryOp::DivU, _)
            )),
            1,
        );
        eu.verify_result().unwrap();
    }

    #[test]
    fn joint_fold_groups_reject_cross_group_carried_reads() {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 1, 1);
        let initial = constant(&mut arena, 0, 8);
        let previous_a = input(&mut arena, 10, 8);
        let group_a = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 20,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 2,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(10, 0, 7),
                    initial,
                    update: previous_a,
                }],
            })
            .unwrap();
        let group_b = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 21,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 2,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(11, 0, 7),
                    initial,
                    update: previous_a,
                }],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        assert!(!SLTToSIRLowerer::new(false).lower_fold_groups_jointly(
            &mut builder,
            &[group_a, group_b],
            &arena,
            &mut cache,
        ));
        assert!(cache.is_empty());
        assert_eq!(builder.block_count(), 1);
    }

    #[test]
    fn four_state_joint_fold_groups_preserve_unknown_guard_per_result() {
        let mut arena = SLTNodeArena::new();
        let guard = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(1u8),
                1,
                false,
            ))
            .unwrap();
        let initial_a = constant(&mut arena, 0x12, 8);
        let initial_b = constant(&mut arena, 0x34, 8);
        let update_a = input(&mut arena, 10, 8);
        let update_b = input(&mut arena, 11, 8);
        let make_group = |arena: &mut SLTNodeArena<u32>, loop_var, target, initial, update| {
            arena
                .alloc(SLTNode::ForFoldGroup {
                    loop_var,
                    loop_width: 8,
                    loop_signed: false,
                    start: BigInt::from(0),
                    step: BigInt::from(1),
                    trip_count: 2,
                    entry_guard: guard,
                    states: vec![SLTForFoldGroupState {
                        target: VarAtomBase::new(target, 0, 7),
                        initial,
                        update,
                    }],
                })
                .unwrap()
        };
        let group_a = make_group(&mut arena, 20, 10, initial_a, update_a);
        let group_b = make_group(&mut arena, 21, 11, initial_b, update_b);

        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        assert!(SLTToSIRLowerer::new(true).lower_fold_groups_jointly(
            &mut builder,
            &[group_a, group_b],
            &arena,
            &mut cache,
        ));
        let result_a = cache[&group_a];
        let result_b = cache[&group_b];
        let eu = finish_lowering(builder);
        assert_eq!(branch_count(&eu), 2);
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            2,
        );
        let values = execute_fold_group_sir(&eu);
        assert_eq!(values[&result_a].mask, BigUint::from(0xffu8));
        assert_eq!(values[&result_b].mask, BigUint::from(0xffu8));
    }

    #[test]
    fn for_fold_group_lowers_swap_updates_as_one_counted_cfg() {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 1, 1);
        let initial_a = constant(&mut arena, 0x12, 8);
        let initial_b = constant(&mut arena, 0x34, 8);
        let previous_a = input(&mut arena, 10, 8);
        let previous_b = input(&mut arena, 11, 8);
        let target_a = VarAtomBase::new(10, 0, 7);
        let target_b = VarAtomBase::new(11, 0, 7);
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 20,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(2),
                step: BigInt::from(3),
                trip_count: 3,
                entry_guard: guard,
                states: vec![
                    SLTForFoldGroupState {
                        target: target_a,
                        initial: initial_a,
                        update: previous_b,
                    },
                    SLTForFoldGroupState {
                        target: target_b,
                        initial: initial_b,
                        update: previous_a,
                    },
                ],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let result = SLTToSIRLowerer::new(false).lower(
            &mut builder,
            group,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(eu.register_map[&result].width(), 16);
        assert_eq!(
            execute_fold_group_sir(&eu)[&result].payload,
            BigUint::from(0x3412u16),
            "three simultaneous swaps leave the first state in the MSBs"
        );
        assert_eq!(branch_count(&eu), 2, "entry guard plus counted backedge");
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Concat(..))),
            1,
            "the final states must be packed once at the common exit"
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            0
        );
        assert!(
            eu.blocks
                .values()
                .all(|block| !matches!(block.terminator, SIRTerminator::Error(_)))
        );

        let body = eu
            .blocks
            .values()
            .find(|block| {
                block.params.len() == 4 && matches!(block.terminator, SIRTerminator::Branch { .. })
            })
            .expect("counted body block");
        assert_eq!(eu.register_map[&body.params[0]].width(), 2);
        let SIRTerminator::Branch { true_block, .. } = &body.terminator else {
            unreachable!()
        };
        assert_eq!(true_block.0, body.id);
        let backedge_args = &true_block.1;
        assert_eq!(backedge_args[2], body.params[3]);
        assert_eq!(backedge_args[3], body.params[2]);

        let exit = eu
            .blocks
            .values()
            .find(|block| {
                block.params.len() == 2
                    && block
                        .instructions
                        .iter()
                        .any(|inst| matches!(inst, SIRInstruction::Concat(..)))
            })
            .expect("packed common exit");
        let packed_args = exit
            .instructions
            .iter()
            .find_map(|inst| match inst {
                SIRInstruction::Concat(_, args) => Some(args),
                _ => None,
            })
            .unwrap();
        assert_eq!(packed_args, &exit.params, "state zero occupies the MSBs");
    }

    #[test]
    fn for_fold_group_executes_exactly_one_and_three_iterations() {
        for (trip_count, expected) in [(1usize, 1u8), (3, 3)] {
            let mut arena = SLTNodeArena::new();
            let guard = constant(&mut arena, 1, 1);
            let initial = constant(&mut arena, 0, 8);
            let previous = input(&mut arena, 10, 8);
            let one = constant(&mut arena, 1, 8);
            let update = arena
                .alloc(SLTNode::Binary(previous, BinaryOp::Add, one))
                .unwrap();
            let group = arena
                .alloc(SLTNode::ForFoldGroup {
                    loop_var: 20,
                    loop_width: 8,
                    loop_signed: false,
                    start: BigInt::from(0),
                    step: BigInt::from(1),
                    trip_count,
                    entry_guard: guard,
                    states: vec![SLTForFoldGroupState {
                        target: VarAtomBase::new(10, 0, 7),
                        initial,
                        update,
                    }],
                })
                .unwrap();

            let mut builder = SIRBuilder::new();
            let result = SLTToSIRLowerer::new(false).lower(
                &mut builder,
                group,
                &arena,
                &mut crate::HashMap::default(),
            );
            let eu = finish_lowering(builder);

            assert_eq!(
                execute_fold_group_sir(&eu)[&result].payload,
                BigUint::from(expected),
                "trip_count={trip_count}"
            );
            assert!(
                eu.blocks
                    .values()
                    .all(|block| !matches!(block.terminator, SIRTerminator::Error(_)))
            );
        }
    }

    #[test]
    fn for_fold_group_inherits_outer_inputs_with_inner_partial_state_priority() {
        let mut arena = SLTNodeArena::new();
        let guard = input(&mut arena, 20, 1);
        let initial = constant(&mut arena, 0x12, 8);
        let outer_wide = input(&mut arena, 10, 16);
        let update = arena
            .alloc(SLTNode::Slice {
                expr: outer_wide,
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 30,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 2,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(10, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let outer_value = builder.alloc_logic(16);
        builder.emit(SIRInstruction::Imm(outer_value, SIRValue::new(0xabcdu16)));
        let outer_guard = builder.alloc_logic(1);
        builder.emit(SIRInstruction::Imm(outer_guard, SIRValue::new(1u8)));
        let mut inputs = crate::HashMap::default();
        inputs.insert(VarAtomBase::new(10, 0, 15), outer_value);
        inputs.insert(VarAtomBase::new(20, 0, 0), outer_guard);

        let result = SLTToSIRLowerer::new(false).lower_with_inputs(
            &mut builder,
            group,
            &arena,
            &mut crate::HashMap::default(),
            inputs,
        );
        let eu = finish_lowering(builder);

        assert_eq!(
            execute_fold_group_sir(&eu)[&result].payload,
            BigUint::from(0x12u8),
            "the inner carried low byte must override the overlapping outer full value"
        );
        assert!(eu.blocks.values().all(|block| {
            block
                .instructions
                .iter()
                .all(|instruction| !matches!(instruction, SIRInstruction::Load(..)))
        }));
    }

    #[test]
    fn for_fold_group_dynamic_array_read_stays_a_narrow_load_under_env() {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 1, 1);
        let initial = constant(&mut arena, 0, 8);
        let loop_index = input(&mut arena, 20, 8);
        let raw_array = arena
            .alloc(SLTNode::Input {
                variable: 30,
                signed: false,
                index: vec![crate::logic_tree::comb::SLTIndex {
                    node: loop_index,
                    stride: 8,
                }],
                access: BitAccess::new(0, 255),
            })
            .unwrap();
        let update = arena
            .alloc(SLTNode::Slice {
                expr: raw_array,
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 20,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 3,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(10, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            group,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);
        let load_widths = eu
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                SIRInstruction::Load(_, variable, SIROffset::Dynamic(_), width)
                    if *variable == 30 =>
                {
                    Some(*width)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(load_widths, vec![8]);
    }

    #[test]
    fn for_fold_group_captures_invariant_work_on_the_true_entry_edge() {
        let mut arena = SLTNodeArena::new();
        let guard = input(&mut arena, 40, 1);
        let initial = constant(&mut arena, 0, 8);
        let previous = input(&mut arena, 10, 8);
        let external = input(&mut arena, 30, 8);
        let two = constant(&mut arena, 2, 8);
        let invariant = arena
            .alloc(SLTNode::Binary(external, BinaryOp::Add, two))
            .unwrap();
        let update = arena
            .alloc(SLTNode::Binary(previous, BinaryOp::Add, invariant))
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 20,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 3,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(10, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            group,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);
        let SIRTerminator::Branch { true_block, .. } = &eu.blocks[&BlockId(0)].terminator else {
            panic!("entry must branch around the recovered loop")
        };
        assert!(
            true_block.1.is_empty(),
            "the true edge must enter the capture block"
        );
        let enter = &eu.blocks[&true_block.0];
        let SIRTerminator::Jump(body, _) = &enter.terminator else {
            panic!("capture block must jump to the counted body")
        };
        let body = *body;
        assert!(enter.instructions.iter().any(|instruction| matches!(
            instruction,
            SIRInstruction::Load(_, variable, SIROffset::Static(0), 8) if *variable == 30
        )));
        assert!(
            eu.blocks[&body]
                .instructions
                .iter()
                .all(|instruction| !matches!(
                    instruction,
                    SIRInstruction::Load(_, variable, _, _) if *variable == 30
                ))
        );
        assert!(matches!(
            &eu.blocks[&body].terminator,
            SIRTerminator::Branch { true_block: (target, _), .. } if *target == body
        ));
    }

    #[test]
    fn for_fold_group_does_not_capture_invariant_division() {
        let mut arena = SLTNodeArena::new();
        let guard = input(&mut arena, 40, 1);
        let initial = constant(&mut arena, 0, 8);
        let previous = input(&mut arena, 10, 8);
        let numerator = input(&mut arena, 30, 8);
        let denominator = input(&mut arena, 31, 8);
        let quotient = arena
            .alloc(SLTNode::Binary(numerator, BinaryOp::DivU, denominator))
            .unwrap();
        let update = arena
            .alloc(SLTNode::Binary(previous, BinaryOp::Add, quotient))
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 20,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 3,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(10, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower(
            &mut builder,
            group,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);
        assert_eq!(
            instruction_count(&eu, |instruction| matches!(
                instruction,
                SIRInstruction::Binary(_, _, BinaryOp::DivU, _)
            )),
            1,
        );
        let division_block = eu
            .blocks
            .values()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::DivU, _))
                })
            })
            .unwrap();
        assert!(matches!(
            &division_block.terminator,
            SIRTerminator::Branch { true_block: (target, _), .. }
                if *target == division_block.id
        ));
    }

    #[test]
    fn false_for_fold_group_entry_guard_skips_all_updates() {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 0, 1);
        let initial = constant(&mut arena, 0x5a, 8);
        let previous = input(&mut arena, 10, 8);
        let one = constant(&mut arena, 1, 8);
        let update = arena
            .alloc(SLTNode::Binary(previous, BinaryOp::Add, one))
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 20,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 3,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(10, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let result = SLTToSIRLowerer::new(false).lower(
            &mut builder,
            group,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 2);
        assert_eq!(
            execute_fold_group_sir(&eu)[&result].payload,
            BigUint::from(0x5au8)
        );
        let SIRTerminator::Branch { false_block, .. } = &eu.blocks[&BlockId(0)].terminator else {
            panic!("entry must branch around the loop")
        };
        assert_eq!(false_block.1.len(), 1);
        let skipped_to = &eu.blocks[&false_block.0];
        assert_eq!(skipped_to.params.len(), 1);
        assert!(
            skipped_to
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Concat(..)))
        );
    }

    #[test]
    fn four_state_for_fold_group_branches_then_applies_one_packed_mux() {
        let mut arena = SLTNodeArena::new();
        let guard = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(1u8),
                1,
                false,
            ))
            .unwrap();
        let initial_a = constant(&mut arena, 1, 8);
        let initial_b = constant(&mut arena, 2, 8);
        let previous_a = input(&mut arena, 10, 8);
        let previous_b = input(&mut arena, 11, 8);
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 20,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 2,
                entry_guard: guard,
                states: vec![
                    SLTForFoldGroupState {
                        target: VarAtomBase::new(10, 0, 7),
                        initial: initial_a,
                        update: previous_b,
                    },
                    SLTForFoldGroupState {
                        target: VarAtomBase::new(11, 0, 7),
                        initial: initial_b,
                        update: previous_a,
                    },
                ],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let result = SLTToSIRLowerer::new(true).lower(
            &mut builder,
            group,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(
            execute_fold_group_sir(&eu)[&result].mask,
            BigUint::from(0xffffu16),
            "an unknown entry guard must make the packed result all-X"
        );

        assert_eq!(
            branch_count(&eu),
            2,
            "the guard value plane controls loop entry"
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Concat(..))),
            2,
            "initial and branch-selected candidates are each packed once"
        );
        assert_eq!(
            instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
            1,
            "one packed Mux must restore X/Z guard semantics"
        );

        let entry_guard = match &eu.blocks[&BlockId(0)].terminator {
            SIRTerminator::Branch { cond, .. } => *cond,
            other => panic!("expected entry guard branch, got {other:?}"),
        };
        let mux_guard = eu
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find_map(|inst| match inst {
                SIRInstruction::Mux(_, cond, _, _) => Some(*cond),
                _ => None,
            })
            .unwrap();
        assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Unary(dst, UnaryOp::ToTwoState, src)
                    if *dst == entry_guard && *src == mux_guard
            )
        }));
    }

    #[test]
    fn signed_for_fold_group_uses_a_direct_conditional_backedge() {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 1, 1);
        let initial = constant(&mut arena, 0, 8);
        let previous = input(&mut arena, 10, 8);
        let loop_value = arena
            .alloc(SLTNode::Input {
                variable: 20,
                signed: true,
                index: vec![],
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let update = arena
            .alloc(SLTNode::Binary(previous, BinaryOp::Add, loop_value))
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 20,
                loop_width: 8,
                loop_signed: true,
                start: BigInt::from(-1),
                step: BigInt::from(-2),
                trip_count: 3,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(10, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let result = SLTToSIRLowerer::new(false).lower(
            &mut builder,
            group,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(
            execute_fold_group_sir(&eu)[&result].payload,
            BigUint::from(0xf7u16),
            "three iterations must observe -1, -3, and -5 exactly"
        );

        let body = eu
            .blocks
            .values()
            .find(|block| {
                block.params.len() == 3 && matches!(block.terminator, SIRTerminator::Branch { .. })
            })
            .expect("counted body block");
        assert!(body.instructions.iter().any(|inst| matches!(
            inst,
            SIRInstruction::Binary(_, lhs, BinaryOp::Add, rhs)
                if *lhs == body.params[2] && *rhs == body.params[1]
        )));

        let step_reg = body
            .instructions
            .iter()
            .find_map(|inst| match inst {
                SIRInstruction::Binary(_, lhs, BinaryOp::Add, rhs) if *lhs == body.params[1] => {
                    Some(*rhs)
                }
                _ => None,
            })
            .expect("loop-value step");
        let step_payload = eu
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find_map(|inst| match inst {
                SIRInstruction::Imm(dst, value) if *dst == step_reg => Some(&value.payload),
                _ => None,
            })
            .unwrap();
        assert_eq!(step_payload, &BigUint::from(0xfeu16));
        let SIRTerminator::Branch { true_block, .. } = &body.terminator else {
            unreachable!()
        };
        assert_eq!(true_block.0, body.id);
        assert!(eu.blocks.values().all(|block| {
            !matches!(&block.terminator, SIRTerminator::Jump(target, _) if *target == body.id)
        }));
        assert!(
            eu.blocks
                .values()
                .all(|block| !matches!(block.terminator, SIRTerminator::Error(_)))
        );
    }

    #[test]
    fn region_slice_uses_slice_aware_cfg_cost_and_verifies() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let then_input = input(&mut arena, 1, 256);
        let else_input = input(&mut arena, 2, 256);
        let then_expr = operation_chain(&mut arena, then_input, BinaryOp::And, 12, 10, 256);
        let else_expr = operation_chain(&mut arena, else_input, BinaryOp::Xor, 12, 100, 256);
        let mux = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            })
            .unwrap();
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(false).lower_region_slice(
            &mut builder,
            mux,
            BitAccess::new(0, 63),
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(branch_count(&eu), 1);
    }

    #[test]
    fn nested_mux_analysis_is_linear_in_dag_size() {
        let mut arena = SLTNodeArena::new();
        let mut value = input(&mut arena, 0, 64);
        for depth in 0..256u32 {
            let cond = input(&mut arena, 1 + depth * 2, 1);
            let arm_input = input(&mut arena, 2 + depth * 2, 64);
            let arm = operation_chain(
                &mut arena,
                arm_input,
                BinaryOp::Add,
                4,
                1_000 + u64::from(depth) * 8,
                64,
            );
            value = arena
                .alloc(SLTNode::Mux {
                    cond,
                    then_expr: arm,
                    else_expr: value,
                })
                .unwrap();
        }

        let lowerer = SLTToSIRLowerer::new(false);
        let mut builder = SIRBuilder::new();
        lowerer.lower(&mut builder, value, &arena, &mut crate::HashMap::default());
        let visits = lowerer.analysis_node_visits();
        let node_count = arena.len();
        finish_lowering(builder);

        assert!(
            visits <= node_count * 20,
            "analysis revisited {visits} nodes for a {node_count}-node nested mux DAG",
        );
    }

    #[test]
    fn unrelated_global_cache_does_not_enter_mux_analysis() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let then_input = input(&mut arena, 1, 64);
        let else_input = input(&mut arena, 2, 64);
        let then_expr = operation_chain(&mut arena, then_input, BinaryOp::Add, 8, 10, 64);
        let else_expr = operation_chain(&mut arena, else_input, BinaryOp::Sub, 8, 100, 64);
        let mux = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            })
            .unwrap();

        let empty_lowerer = SLTToSIRLowerer::new(false);
        let mut empty_builder = SIRBuilder::new();
        empty_lowerer.lower(
            &mut empty_builder,
            mux,
            &arena,
            &mut crate::HashMap::default(),
        );
        let empty_visits = empty_lowerer.analysis_node_visits();
        finish_lowering(empty_builder);

        let mut large_cache = crate::HashMap::default();
        for index in 0..20_000usize {
            large_cache.insert(NodeId(arena.len() + index), RegisterId(index));
        }
        let cached_lowerer = SLTToSIRLowerer::new(false);
        let mut cached_builder = SIRBuilder::new();
        cached_lowerer.lower(&mut cached_builder, mux, &arena, &mut large_cache);
        let cached_visits = cached_lowerer.analysis_node_visits();
        finish_lowering(cached_builder);

        assert_eq!(cached_visits, empty_visits);
    }

    #[test]
    fn signed_inputs_report_signedness() {
        let mut arena = SLTNodeArena::<u32>::new();
        let node = arena
            .alloc(SLTNode::Input {
                variable: 0,
                signed: true,
                index: vec![],
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let lowerer = SLTToSIRLowerer::new(false);
        assert!(lowerer.get_bound_signed(node, &arena));
    }

    #[test]
    fn unsigned_inputs_report_unsignedness() {
        let mut arena = SLTNodeArena::<u32>::new();
        let node = arena
            .alloc(SLTNode::Input {
                variable: 0,
                signed: false,
                index: vec![],
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let lowerer = SLTToSIRLowerer::new(false);
        assert!(!lowerer.get_bound_signed(node, &arena));
    }

    #[test]
    fn bit_count_results_are_unsigned_even_for_signed_inputs() {
        let mut arena = SLTNodeArena::<u32>::new();
        let input = arena
            .alloc(SLTNode::Input {
                variable: 0,
                signed: true,
                index: vec![],
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let lowerer = SLTToSIRLowerer::new(false);

        for op in [
            UnaryOp::PopCount,
            UnaryOp::CountLeadingZeros,
            UnaryOp::CountTrailingZeros,
        ] {
            let node = arena.alloc(SLTNode::Unary(op, input)).unwrap();
            assert!(!lowerer.get_bound_signed(node, &arena));
        }
    }

    #[test]
    fn unary_value_operators_preserve_operand_expression_signedness() {
        let mut arena = SLTNodeArena::<u32>::new();
        let signed = arena
            .alloc(SLTNode::Input {
                variable: 0,
                signed: true,
                index: vec![],
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let unsigned = arena
            .alloc(SLTNode::Input {
                variable: 1,
                signed: false,
                index: vec![],
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let lowerer = SLTToSIRLowerer::new(false);

        for op in [
            UnaryOp::Ident,
            UnaryOp::ToTwoState,
            UnaryOp::Minus,
            UnaryOp::BitNot,
        ] {
            let signed_result = arena.alloc(SLTNode::Unary(op, signed)).unwrap();
            let unsigned_result = arena.alloc(SLTNode::Unary(op, unsigned)).unwrap();
            assert!(lowerer.get_bound_signed(signed_result, &arena), "{op}");
            assert!(!lowerer.get_bound_signed(unsigned_result, &arena), "{op}");
        }
    }

    #[test]
    fn width_materialization_preserves_four_state_register_kind() {
        let lowerer = SLTToSIRLowerer::new(true);
        let mut builder = SIRBuilder::<usize>::new();
        let source = builder.alloc_logic(5);
        builder.emit(SIRInstruction::Imm(
            source,
            SIRValue::new_four_state(0x11u8, 0x10u8),
        ));

        let widened = lowerer.cast_reg_width_ext(&mut builder, source, 8, true);
        let narrowed = lowerer.cast_reg_width_ext(&mut builder, widened, 4, true);

        assert!(matches!(
            builder.register(&widened),
            RegisterType::Logic { width: 8 }
        ));
        assert!(matches!(
            builder.register(&narrowed),
            RegisterType::Logic { width: 4 }
        ));
    }

    #[test]
    fn mixed_sign_subtraction_bound_is_unsigned() {
        let mut arena = SLTNodeArena::<u32>::new();
        let lhs = arena
            .alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 8, false))
            .unwrap();
        let rhs = arena
            .alloc(SLTNode::Input {
                variable: 0,
                signed: true,
                index: vec![],
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let node = arena
            .alloc(SLTNode::Binary(lhs, BinaryOp::Sub, rhs))
            .unwrap();
        let lowerer = SLTToSIRLowerer::new(false);
        assert!(!lowerer.get_bound_signed(node, &arena));
    }

    #[test]
    fn mixed_sign_mux_bound_is_unsigned() {
        let mut arena = SLTNodeArena::<u32>::new();
        let cond = arena
            .alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 1, false))
            .unwrap();
        let then_expr = arena
            .alloc(SLTNode::Input {
                variable: 0,
                signed: true,
                index: vec![],
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let else_expr = arena
            .alloc(SLTNode::Input {
                variable: 1,
                signed: false,
                index: vec![],
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let node = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            })
            .unwrap();
        let lowerer = SLTToSIRLowerer::new(false);
        assert!(!lowerer.get_bound_signed(node, &arena));
    }

    #[test]
    fn comparison_bound_is_not_signed() {
        let mut arena = SLTNodeArena::<u32>::new();
        let lhs = arena
            .alloc(SLTNode::Input {
                variable: 0,
                signed: false,
                index: vec![],
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let rhs = arena
            .alloc(SLTNode::Input {
                variable: 1,
                signed: true,
                index: vec![],
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let node = arena
            .alloc(SLTNode::Binary(lhs, BinaryOp::LtS, rhs))
            .unwrap();
        let lowerer = SLTToSIRLowerer::new(false);
        assert!(!lowerer.get_bound_signed(node, &arena));
    }

    #[test]
    fn unsigned_target_bound_zero_extends_signed_slice_without_losing_state_kind() {
        let mut arena = SLTNodeArena::<u32>::new();
        let inner = arena
            .alloc(SLTNode::Input {
                variable: 0,
                signed: true,
                index: vec![],
                access: BitAccess::new(0, 15),
            })
            .unwrap();
        let casted = arena
            .alloc(SLTNode::Slice {
                expr: inner,
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let mut builder = SIRBuilder::<u32>::new();
        let mut cache = crate::HashMap::default();
        let lowerer = SLTToSIRLowerer::new(false);
        let reg = lowerer.lower_bound(
            &mut builder,
            &SLTLoopBound::Expr(casted),
            8,
            9,
            false,
            &arena,
            &mut cache,
        );
        assert!(matches!(
            builder.register(&reg),
            crate::ir::RegisterType::Logic { width: 9 }
        ));
        assert!(!lowerer.get_bound_signed(casted, &arena));
    }
}
