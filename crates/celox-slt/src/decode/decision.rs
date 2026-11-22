//! Factor a case item's exact labels by the high bits of its selector.
use super::*;
use std::collections::BTreeMap;

/// Return the root OR underneath wrappers preserving a one-bit predicate.
pub(super) fn disjunction<A: Clone + Eq + Hash>(
    arena: &SLTNodeArena<A>,
    mut node: NodeId,
) -> Option<NodeId> {
    while let Some(inner) = boolean_child(arena, node) {
        node = inner;
    }
    matches!(
        arena.get(node),
        SLTNode::Binary(_, BinaryOp::Or | BinaryOp::LogicOr, _)
    )
    .then_some(node)
}

fn test_values<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    input: NodeId,
    labels: &[u64],
) -> Result<NodeId, SLTNodeFactsError> {
    if labels.len() > 1
        && let Some(compact) = compact_membership(arena, input, labels.iter().copied())?
    {
        return Ok(compact);
    }
    let width = crate::get_width(input, arena);
    let mut tests = Vec::with_capacity(labels.len());
    for &label in labels {
        let literal = arena.alloc(SLTNode::Constant(label.into(), 0u8.into(), width, false))?;
        tests.push(arena.alloc(SLTNode::Binary(input, BinaryOp::Eq, literal))?);
    }
    any(arena, tests)
}

pub(super) fn reduce<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    root: NodeId,
    uses: &HashMap<NodeId, usize>,
) -> Result<Option<NodeId>, SLTNodeFactsError> {
    // Each high prefix owns a word. Bit i is set if that prefix accepts i
    // in the six low selector bits. Repeated labels set the same bit.
    let mut pages = BTreeMap::<u64, u64>::new();
    let mut selector = None;
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node != root && uses[&node] > 1 && disjunction(arena, node).is_some() {
            // Leave shared OR interiors intact. Expanding each successive
            // prefix for its users could turn a linear DAG into quadratic work.
            return Ok(None);
        }
        if let Some(inner) = boolean_child(arena, node) {
            pending.push(inner);
        } else if let SLTNode::Binary(a, BinaryOp::Or | BinaryOp::LogicOr, b) = *arena.get(node) {
            if arena.node_width(a) != Some(1) || arena.node_width(b) != Some(1) {
                return Ok(None);
            }
            pending.extend([a, b]);
        } else {
            let Some((input, _, label)) = exact_label(arena, node) else {
                return Ok(None);
            };
            if *selector.get_or_insert(input) != input {
                return Ok(None);
            }
            *pages.entry(label >> 6).or_default() |= 1 << (label & 63);
        }
    }
    let selector = selector.expect("a disjunction has leaves");
    let width = crate::get_width(selector, arena);
    let nlabels: usize = pages.values().map(|v| v.count_ones() as usize).sum();
    if nlabels < 8 {
        return Ok(None);
    }
    let mut patterns = BTreeMap::<u64, Vec<u64>>::new();
    for (&high, &word) in &pages {
        patterns.entry(word).or_default().push(high);
    }
    // Isolated labels on separate pages gain nothing from testing two pieces.
    if pages.len() + 2 * patterns.len() >= nlabels {
        return Ok(None);
    }
    let low = if width <= 6 {
        selector
    } else {
        arena.alloc(SLTNode::Slice {
            expr: selector,
            access: BitAccess::new(0, 5),
        })?
    };
    let high = if width > 6 {
        Some(arena.alloc(SLTNode::Slice {
            expr: selector,
            access: BitAccess::new(6, width - 1),
        })?)
    } else {
        None
    };
    let mut tests = Vec::with_capacity(patterns.len());
    for (word, prefixes) in patterns {
        let prefix = high
            .map(|input| test_values(arena, input, &prefixes))
            .transpose()?;
        if let Some(test) = prefix
            && word == u64::MAX
        {
            tests.push(test);
            continue;
        }
        let low_values: Vec<_> = (0..64).filter(|bit| word & (1u64 << bit) != 0).collect();
        let bits = test_values(arena, low, &low_values)?;
        let test = match prefix {
            Some(prefix) => arena.alloc(SLTNode::Binary(prefix, BinaryOp::And, bits))?,
            None => bits,
        };
        tests.push(test);
    }
    any(arena, tests).map(Some)
}
