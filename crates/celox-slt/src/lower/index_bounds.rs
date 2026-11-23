//! Prove that a two-state dynamic array index fits its declared element count.
use super::*;

/// The frontend checks both planes and bounds before loading an element.
/// Only the range test can fail in two-state mode.
pub(super) fn always_in_bounds<A: Hash + Eq + Clone>(
    arena: &SLTNodeArena<A>,
    mut cond: NodeId,
) -> bool {
    while let SLTNode::Unary(UnaryOp::Or | UnaryOp::Ident | UnaryOp::ToTwoState, inner) =
        *arena.get(cond)
    {
        cond = inner;
    }
    let SLTNode::Binary(a, BinaryOp::LogicAnd | BinaryOp::And, b) = *arena.get(cond) else {
        return false;
    };
    for (known, test) in [(a, b), (b, a)] {
        let SLTNode::Binary(index, BinaryOp::LtU, limit) = *arena.get(test) else {
            continue;
        };
        if !matches!(arena.get(limit), SLTNode::Constant(..)) {
            continue;
        }
        let Some(limit) = slt_const_u64(limit, arena) else {
            continue;
        };
        let SLTNode::Binary(x, BinaryOp::EqCase, y) = *arena.get(known) else {
            continue;
        };
        for (source, converted) in [(x, y), (y, x)] {
            if matches!(arena.get(converted), SLTNode::Unary(UnaryOp::ToTwoState, n) if *n == source)
            {
                // The knownness check is true for any two-state source, even
                // when the address has since been simplified independently.
                // Range analysis is optional and shared DAG nodes can recur
                // through several bounds. Limit the work per guard.
                let mut budget = 128;
                return bounds(arena, index, &mut budget).is_some_and(|(_, max)| max < limit);
            }
        }
    }
    false
}

/// Closed unsigned intervals for the arithmetic used to flatten an index.
/// Stop if arithmetic could wrap, the direction is unknown, or the index
/// is too complex to prove cheaply. In those cases the emitted guard stands.
fn bounds<A: Hash + Eq + Clone>(
    arena: &SLTNodeArena<A>,
    node: NodeId,
    budget: &mut usize,
) -> Option<(u64, u64)> {
    *budget = budget.checked_sub(1)?;
    let width = arena.node_width(node)?;
    if !(1..=64).contains(&width) {
        return None;
    }
    let mask = u64::MAX >> (64 - width);
    let range = match arena.get(node) {
        SLTNode::Constant(value, unknown, _, _) if unknown.is_zero() => {
            let value = value.to_u64()? & mask;
            (value, value)
        }
        SLTNode::Input { index, .. } if index.is_empty() => (0, mask),
        SLTNode::Unary(op @ (UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or), inner) => {
            let (lo, hi) = bounds(arena, *inner, budget)?;
            if *op == UnaryOp::Or {
                (u64::from(lo != 0), u64::from(hi != 0))
            } else {
                (lo, hi)
            }
        }
        SLTNode::Concat(parts) => {
            let (mut lo, mut hi) = (0u128, 0u128);
            for &(part, bits) in parts {
                let (a, b) = bounds(arena, part, budget)?;
                if bits > 64 || u128::from(b) >= 1u128 << bits {
                    return None;
                }
                lo = (lo << bits) | u128::from(a);
                hi = (hi << bits) | u128::from(b);
            }
            (lo.try_into().ok()?, hi.try_into().ok()?)
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            let (lo, hi) = bounds(arena, *cond, budget)?;
            let arm = if lo != 0 {
                then_expr
            } else if hi == 0 {
                else_expr
            } else {
                return None;
            };
            bounds(arena, *arm, budget)?
        }
        SLTNode::Binary(left, op, right) => {
            let (a, b) = bounds(arena, *left, budget)?;
            let (c, d) = bounds(arena, *right, budget)?;
            match op {
                BinaryOp::Add | BinaryOp::Mul | BinaryOp::Sub
                    if arena.node_width(*left) == Some(width)
                        && arena.node_width(*right) == Some(width) =>
                {
                    match op {
                        BinaryOp::Add => (a.checked_add(c)?, b.checked_add(d)?),
                        BinaryOp::Mul => (a.checked_mul(c)?, b.checked_mul(d)?),
                        _ => (a.checked_sub(d)?, b.checked_sub(c)?),
                    }
                }
                BinaryOp::LogicAnd => (u64::from(a > 0 && c > 0), u64::from(b > 0 && d > 0)),
                BinaryOp::GeU => (u64::from(a >= d), u64::from(b >= c)),
                BinaryOp::LeU => (u64::from(b <= c), u64::from(a <= d)),
                BinaryOp::GeS | BinaryOp::LeS => {
                    // Nonnegative at their original signed widths, so signed
                    // extension preserves these unsigned mathematical bounds.
                    if b >> (arena.node_width(*left)? - 1) != 0
                        || d >> (arena.node_width(*right)? - 1) != 0
                    {
                        return None;
                    }
                    if *op == BinaryOp::GeS {
                        (u64::from(a >= d), u64::from(b >= c))
                    } else {
                        (u64::from(b <= c), u64::from(a <= d))
                    }
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    (range.1 <= mask).then_some(range)
}
