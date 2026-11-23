//! Reduce priority mux chains over mutually exclusive two-state selector values.
mod decision;
mod state_table;
use crate::{
    HashMap, HashSet, NodeId, SLTNode, SLTNodeArena, SLTNodeFactsError, canon, lower::bound_signed,
};
use celox_design::{BinaryOp, BitAccess, UnaryOp};
use num_traits::{ToPrimitive, Zero};
use std::hash::Hash;

#[derive(Copy, Clone)]
struct Row {
    selector: NodeId,
    label: u64,
    predicate: NodeId,
    value: NodeId,
    tail: NodeId,
}

fn row<A: Clone + Eq + Hash>(arena: &SLTNodeArena<A>, node: NodeId) -> Option<Row> {
    let SLTNode::Mux {
        cond,
        then_expr,
        else_expr,
    } = *arena.get(node)
    else {
        return None;
    };
    if arena.node_width(then_expr) != arena.node_width(else_expr) {
        return None;
    }
    let (selector, _, label) = literal_comparison(arena, cond)?;
    Some(Row {
        selector,
        label,
        predicate: cond,
        value: then_expr,
        tail: else_expr,
    })
}

fn boolean_child<A: Clone + Eq + Hash>(arena: &SLTNodeArena<A>, node: NodeId) -> Option<NodeId> {
    match *arena.get(node) {
        SLTNode::Unary(UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or, child)
            if arena.node_width(child) == Some(1) =>
        {
            Some(child)
        }
        _ => None,
    }
}

/// An equality test on a scalar input and a known constant of its width.
/// A wildcard comparison is exact only when the right operand has no wildcard bits.
pub(super) fn exact_label<A: Clone + Eq + Hash>(
    arena: &SLTNodeArena<A>,
    predicate: NodeId,
) -> Option<(NodeId, BinaryOp, u64)> {
    let test = literal_comparison(arena, predicate)?;
    matches!(arena.get(test.0), SLTNode::Input { index, .. } if index.is_empty()).then_some(test)
}

fn literal_comparison<A: Clone + Eq + Hash>(
    arena: &SLTNodeArena<A>,
    mut predicate: NodeId,
) -> Option<(NodeId, BinaryOp, u64)> {
    while let Some(inner) = boolean_child(arena, predicate) {
        predicate = inner;
    }
    let SLTNode::Binary(left, op @ (BinaryOp::Eq | BinaryOp::EqCase | BinaryOp::EqWildcard), right) =
        *arena.get(predicate)
    else {
        return None;
    };
    let (selector, literal) =
        if op != BinaryOp::EqWildcard && matches!(arena.get(left), SLTNode::Constant(..)) {
            (right, left)
        } else {
            (left, right)
        };
    let SLTNode::Constant(bits, mask, width, _) = arena.get(literal) else {
        return None;
    };
    if !mask.is_zero() || !(1..=64).contains(width) || arena.node_width(selector) != Some(*width) {
        return None;
    }
    let label = bits.to_u64()?;
    if *width < 64 && label >= (1u64 << width) {
        return None;
    }
    Some((selector, op, label))
}

/// Match unique labels using one masked comparison or one immediate bitmap.
fn compact_membership<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    selector: NodeId,
    labels: impl ExactSizeIterator<Item = u64> + Clone,
) -> Result<Option<NodeId>, SLTNodeFactsError> {
    let first = labels.clone().next().expect("nonempty decode group");
    let width = crate::get_width(selector, arena);
    let varying = labels.clone().fold(0, |diff, value| diff | (first ^ value));
    // Distinct labels exhaust all assignments to the varying bits.
    if 1usize.checked_shl(varying.count_ones()) == Some(labels.len()) {
        let mask = (u64::MAX >> (64 - width)) & !varying;
        if mask == 0 {
            return arena
                .alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 1, false))
                .map(Some);
        }
        let literal = arena.alloc(SLTNode::Constant(
            (first & mask).into(),
            0u8.into(),
            width,
            false,
        ))?;
        let mask = arena.alloc(SLTNode::Constant(mask.into(), 0u8.into(), width, false))?;
        let masked = arena.alloc(SLTNode::Binary(selector, BinaryOp::And, mask))?;
        return arena
            .alloc(SLTNode::Binary(masked, BinaryOp::Eq, literal))
            .map(Some);
    }
    if width <= 6 && labels.len() > 2 {
        let table_width = 1usize << width;
        let bits = labels.fold(0u64, |bits, value| bits | (1u64 << value));
        let table = arena.alloc(SLTNode::Constant(
            bits.into(),
            0u8.into(),
            table_width,
            false,
        ))?;
        let shifted = arena.alloc(SLTNode::Binary(table, BinaryOp::Shr, selector))?;
        return arena
            .alloc(SLTNode::Slice {
                expr: shifted,
                access: BitAccess::new(0, 0),
            })
            .map(Some);
    }
    Ok(None)
}

fn membership<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    selector: NodeId,
    labels: &[(u64, NodeId)],
) -> Result<NodeId, SLTNodeFactsError> {
    let (_, predicate) = labels[0];
    if labels.len() == 1 {
        return Ok(predicate);
    }
    if let Some(test) = compact_membership(arena, selector, labels.iter().map(|&(label, _)| label))?
    {
        return Ok(test);
    }
    any(arena, labels.iter().map(|&(_, node)| node).collect())
}

fn any<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    mut conditions: Vec<NodeId>,
) -> Result<NodeId, SLTNodeFactsError> {
    while conditions.len() > 1 {
        let mut next = Vec::with_capacity(conditions.len().div_ceil(2));
        for pair in conditions.chunks(2) {
            next.push(if let [a, b] = *pair {
                arena.alloc(SLTNode::Binary(a, BinaryOp::Or, b))?
            } else {
                pair[0]
            });
        }
        conditions = next;
    }
    Ok(conditions[0])
}

// Even with two-state storage, an expression can introduce unknown bits.
// A bit-select tree is equivalent to exact comparisons only for a known address.
fn known_selector<A: Clone + Eq + Hash>(arena: &SLTNodeArena<A>, selector: NodeId) -> bool {
    let mut pending = vec![selector];
    let mut seen = HashSet::default();
    while let Some(node) = pending.pop() {
        if !seen.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Input { index, .. } if index.is_empty() => {}
            SLTNode::Constant(_, mask, ..) if mask.is_zero() => {}
            SLTNode::Unary(UnaryOp::ToTwoState, _) | SLTNode::Binary(_, BinaryOp::EqCase, _) => {}
            SLTNode::Slice { expr, access } if access.msb < crate::get_width(*expr, arena) => {
                pending.push(*expr)
            }
            n @ (SLTNode::Concat(_)
            | SLTNode::Mux { .. }
            | SLTNode::Binary(_, BinaryOp::Eq | BinaryOp::And | BinaryOp::Or, _)
            | SLTNode::Unary(
                UnaryOp::Ident | UnaryOp::BitNot | UnaryOp::LogicNot | UnaryOp::Or,
                _,
            )) => pending.extend(canon::operands(n)),
            _ => return false,
        }
    }
    true
}

/// Read a dense bank of scalar inputs by the bits of its selector. Missing
/// addresses use the original default, including its uninitialized bits.
fn input_bank<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    selector: NodeId,
    groups: &[(NodeId, Vec<(u64, NodeId)>)],
    default: NodeId,
) -> Result<Option<NodeId>, SLTNodeFactsError> {
    let width = crate::get_width(selector, arena);
    if !(3..=8).contains(&width) || groups.len() < 8 {
        return Ok(None);
    }
    let entries = 1usize << width;
    // Limit table growth for sparse decodes and keep value grouping for
    // cases dominated by repeated constants.
    let scalar_inputs = groups.iter().filter(|(value, _)| {
        matches!(arena.get(*value), SLTNode::Input { index, .. } if index.is_empty())
    }).count();
    if scalar_inputs < 8
        || scalar_inputs < groups.len().div_ceil(2)
        || groups.iter().map(|(_, labels)| labels.len()).sum::<usize>() < entries / 2
    {
        return Ok(None);
    }
    if !known_selector(arena, selector) {
        return Ok(None);
    }
    let mut values = vec![default; entries];
    for (value, labels) in groups {
        for &(label, _) in labels {
            values[label as usize] = *value;
        }
    }
    for bit in 0..width {
        let cond = arena.alloc(SLTNode::Slice {
            expr: selector,
            access: BitAccess::new(bit, bit),
        })?;
        for i in 0..values.len() / 2 {
            let (low, high) = (values[2 * i], values[2 * i + 1]);
            values[i] = if low == high {
                low
            } else {
                arena.alloc(SLTNode::Mux {
                    cond,
                    then_expr: high,
                    else_expr: low,
                })?
            };
        }
        values.truncate(values.len() / 2);
    }
    Ok(Some(values[0]))
}

fn reduce_chain<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    node: NodeId,
    selector: NodeId,
    rows: &HashMap<NodeId, Row>,
    interior: &HashSet<NodeId>,
    mapped: impl Fn(&NodeId) -> NodeId,
) -> Result<Option<NodeId>, SLTNodeFactsError> {
    let mut cursor = node;
    let mut seen = HashSet::default();
    let mut groups = Vec::<(NodeId, Vec<(u64, NodeId)>)>::new();
    let mut indices = HashMap::default();
    let mut count = 0;
    loop {
        let row = rows[&cursor];
        count += 1;
        if seen.insert(row.label) {
            let value = mapped(&row.value);
            let i = *indices.entry(value).or_insert_with(|| {
                groups.push((value, Vec::new()));
                groups.len() - 1
            });
            groups[i].1.push((row.label, row.predicate));
        }
        cursor = row.tail;
        if !interior.contains(&cursor) {
            break;
        }
    }
    let mut result = mapped(&cursor);
    if let Some(bank) = input_bank(arena, selector, &groups, result)?
        && bound_signed(bank, arena) == bound_signed(node, arena)
    {
        return Ok(Some(bank));
    }
    if !matches!(arena.get(selector), SLTNode::Input { index, .. } if index.is_empty()) {
        return Ok(None);
    }
    // Keep small constant tables recognizable as a whole by later lowering.
    // Grouping their rows separately can cost more than the original decode.
    let width = crate::get_width(selector, arena);
    if width <= 6
        && crate::get_width(result, arena) <= (64 >> width)
        && matches!(arena.get(result), SLTNode::Constant(..))
        && groups
            .iter()
            .all(|(value, _)| matches!(arena.get(*value), SLTNode::Constant(..)))
    {
        return Ok(None);
    }
    if let Some(table) = state_table::tabulate(arena, selector, &groups, result, node)? {
        return Ok(Some(table));
    }
    groups.retain(|(v, _)| *v != result);
    if groups.len() == count {
        return Ok(None);
    }
    for (value, labels) in groups.into_iter().rev() {
        let cond = membership(arena, selector, &labels)?;
        result = arena.alloc(SLTNode::Mux {
            cond,
            then_expr: value,
            else_expr: result,
        })?;
    }
    // The replacement may have exposed a signed arm by removing the last mux.
    Ok((!canon::signed_register(arena, result)
        && bound_signed(node, arena) == bound_signed(result, arena))
    .then_some(result))
}

/// Group equal-valued arms and balance dense scalar-input selections. Call
/// only for two-state storage: masking a differing selector bit would hide X/Z.
pub fn reduce_two_state_decodes<'a, A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    roots: impl IntoIterator<Item = &'a mut NodeId>,
) -> Result<(), SLTNodeFactsError> {
    let roots: Vec<_> = roots.into_iter().collect();
    let mut pending: Vec<_> = roots.iter().map(|n| **n).collect();
    let mut uses = HashMap::<NodeId, usize>::default();
    let mut order = Vec::new();
    let mut rows = HashMap::default();
    let mut disjunctions = HashSet::default();
    while let Some(n) = pending.pop() {
        let uses = uses.entry(n).or_default();
        *uses += 1;
        if *uses > 1 {
            continue;
        }
        order.push(n);
        if let SLTNode::Mux { cond, .. } = *arena.get(n)
            && let Some(disjunction) = decision::disjunction(arena, cond)
        {
            disjunctions.insert(disjunction);
        }
        if let Some(r) = row(arena, n) {
            rows.insert(n, r);
        }
        pending.extend(canon::operands(arena.get(n)));
    }
    // Visit each private chain once. A shared suffix is an independent value.
    let interior: HashSet<_> = rows
        .values()
        .filter(|r| {
            uses[&r.tail] == 1 && rows.get(&r.tail).is_some_and(|t| r.selector == t.selector)
        })
        .map(|r| r.tail)
        .collect();
    if interior.is_empty() && disjunctions.is_empty() {
        return Ok(());
    }
    // Arena operands precede their users, so replacements propagate in one pass.
    order.sort_unstable();
    let mut changed = HashMap::default();
    let mut impure = HashSet::default();
    for node in order {
        let children = canon::operands(arena.get(node));
        if matches!(
            arena.get(node),
            SLTNode::ForFold { .. }
                | SLTNode::ForFoldGroup { .. }
                | SLTNode::Capture { .. }
                | SLTNode::Binary(
                    _,
                    BinaryOp::DivS | BinaryOp::DivU | BinaryOp::RemS | BinaryOp::RemU,
                    _
                )
        ) || children.iter().any(|c| impure.contains(c))
        {
            impure.insert(node);
        }
        let mapped = |id: &NodeId| changed.get(id).copied().unwrap_or(*id);
        let mut replacement = if disjunctions.contains(&node) {
            decision::reduce(arena, node, &uses)?
        } else {
            None
        };
        if !impure.contains(&node)
            && !interior.contains(&node)
            && let Some(head) = rows.get(&node)
            && interior.contains(&head.tail)
        {
            replacement = reduce_chain(arena, node, head.selector, &rows, &interior, mapped)?;
        }
        if replacement.is_none()
            && children.iter().any(|c| changed.contains_key(c))
            && let Some(expr) = canon::map_expression(arena.get(node), mapped)
        {
            replacement = Some(arena.alloc(expr)?);
        }
        if let Some(result) = replacement
            && result != node
        {
            changed.insert(node, result);
        }
    }
    for root in roots {
        if let Some(&n) = changed.get(root) {
            *root = n;
        }
    }
    Ok(())
}
