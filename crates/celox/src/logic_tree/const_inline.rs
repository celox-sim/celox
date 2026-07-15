//! Constant variable inlining for combinational logic paths.
//!
//! Detects variables whose *every* LogicPath target is a constant expression
//! (no runtime inputs), then rewrites all `SLTNode::Input` references to those
//! variables with `SLTNode::Constant` nodes containing the precomputed value.
//! This eliminates the Store→Load memory roundtrip for compile-time constants
//! such as genvar-expanded parity-check matrices.

use std::fmt::{Debug, Display};
use std::hash::Hash;

use num_bigint::BigUint;
use num_traits::Zero;

use crate::ir::BitAccess;
use crate::logic_tree::comb::{LogicPath, NodeId, SLTNode, SLTNodeArena, SLTNodeFactsError};
use crate::{HashMap, HashSet};

/// Check if an SLT expression tree is purely constant (no Input references).
/// Conservative: only handles Constant, Slice(Constant-tree), Binary, Unary,
/// and Concat of constant-trees.
fn is_const_expr<A: Clone + Eq + Hash>(node: NodeId, arena: &SLTNodeArena<A>) -> bool {
    match arena.get(node) {
        SLTNode::Constant(..) => true,
        SLTNode::Slice { expr, .. } => is_const_expr(*expr, arena),
        SLTNode::Binary(l, _, r) => is_const_expr(*l, arena) && is_const_expr(*r, arena),
        SLTNode::Unary(_, inner) => is_const_expr(*inner, arena),
        SLTNode::Concat(parts) => parts.iter().all(|(id, _)| is_const_expr(*id, arena)),
        // The update expressions contain loop-scoped inputs whose values are
        // supplied by the fold, so child constness alone is not sufficient.
        SLTNode::ForFoldGroup { .. } => false,
        _ => false,
    }
}

/// Evaluate a constant expression tree to a (payload, mask, width) triple.
fn eval_const_expr<A: Clone + Eq + Hash + Debug>(
    node: NodeId,
    arena: &SLTNodeArena<A>,
) -> (BigUint, BigUint, usize) {
    use crate::ir::BinaryOp;
    use crate::logic_tree::comb::get_width;

    let width = get_width(node, arena);
    let width_mask = if width > 0 {
        (BigUint::from(1u32) << width) - 1u32
    } else {
        BigUint::from(0u32)
    };

    match arena.get(node) {
        SLTNode::Constant(val, msk, _, _) => (val.clone(), msk.clone(), width),
        SLTNode::Slice { expr, access } => {
            let (val, msk, _) = eval_const_expr(*expr, arena);
            let slice_w = access.msb - access.lsb + 1;
            let slice_mask = (BigUint::from(1u32) << slice_w) - 1u32;
            (
                (&val >> access.lsb) & &slice_mask,
                (&msk >> access.lsb) & &slice_mask,
                slice_w,
            )
        }
        SLTNode::Binary(l, op, r) => {
            let (lv, lm, _) = eval_const_expr(*l, arena);
            let (rv, rm, _) = eval_const_expr(*r, arena);
            // Only handle 2-state (mask==0) for safety
            if lm != BigUint::from(0u32) || rm != BigUint::from(0u32) {
                return (BigUint::from(0u32), width_mask.clone(), width); // unknown
            }
            let result = match op {
                BinaryOp::And => &lv & &rv,
                BinaryOp::Or => &lv | &rv,
                BinaryOp::Xor => &lv ^ &rv,
                BinaryOp::Add => (&lv + &rv) & &width_mask,
                BinaryOp::Sub => {
                    // Two's complement subtraction
                    let total = (&width_mask + 1u32) + &lv - &rv;
                    total & &width_mask
                }
                BinaryOp::Shl => {
                    if let Some(shift) = rv.to_u64_digits().first().copied() {
                        (&lv << shift as usize) & &width_mask
                    } else {
                        BigUint::from(0u32)
                    }
                }
                BinaryOp::Shr => {
                    if let Some(shift) = rv.to_u64_digits().first().copied() {
                        &lv >> shift as usize
                    } else {
                        BigUint::from(0u32)
                    }
                }
                _ => return (BigUint::from(0u32), width_mask.clone(), width),
            };
            (result & &width_mask, BigUint::from(0u32), width)
        }
        SLTNode::Unary(op, inner) => {
            use crate::ir::UnaryOp;
            let (v, m, inner_width) = eval_const_expr(*inner, arena);
            if matches!(op, UnaryOp::ToTwoState) {
                return (
                    v & (&width_mask ^ (&m & &width_mask)),
                    BigUint::from(0u32),
                    width,
                );
            }
            if matches!(op, UnaryOp::LogicNot | UnaryOp::Or) {
                let inner_width_mask = if inner_width > 0 {
                    (BigUint::from(1u32) << inner_width) - 1u32
                } else {
                    BigUint::from(0u32)
                };
                let unknown = &m & &inner_width_mask;
                let known = &inner_width_mask ^ &unknown;
                let definite_ones = (&v & &inner_width_mask) & known;
                let has_unknown = !unknown.is_zero();
                let (value, mask) = match op {
                    UnaryOp::LogicNot => {
                        if !definite_ones.is_zero() {
                            (0u8, 0u8)
                        } else if has_unknown {
                            (1u8, 1u8)
                        } else {
                            (1u8, 0u8)
                        }
                    }
                    UnaryOp::Or => {
                        if !definite_ones.is_zero() {
                            (1u8, 0u8)
                        } else if has_unknown {
                            (1u8, 1u8)
                        } else {
                            (0u8, 0u8)
                        }
                    }
                    _ => unreachable!(),
                };
                return (BigUint::from(value), BigUint::from(mask), width);
            }
            if m != BigUint::from(0u32) {
                return (BigUint::from(0u32), width_mask.clone(), width);
            }
            let result = match op {
                UnaryOp::BitNot => (&width_mask) ^ &v,
                UnaryOp::PopCount => BigUint::from(
                    v.iter_u64_digits()
                        .map(|digit| digit.count_ones() as usize)
                        .sum::<usize>(),
                ),
                UnaryOp::CountLeadingZeros => {
                    BigUint::from(inner_width.saturating_sub(v.bits() as usize))
                }
                UnaryOp::CountTrailingZeros => {
                    let zeros = v
                        .iter_u64_digits()
                        .enumerate()
                        .find_map(|(index, digit)| {
                            (digit != 0).then_some(
                                index * u64::BITS as usize + digit.trailing_zeros() as usize,
                            )
                        })
                        .unwrap_or(inner_width)
                        .min(inner_width);
                    BigUint::from(zeros)
                }
                _ => return (BigUint::from(0u32), width_mask.clone(), width),
            };
            (result & &width_mask, BigUint::from(0u32), width)
        }
        SLTNode::ForFoldGroup { .. } => (BigUint::from(0u32), width_mask, width),
        _ => (BigUint::from(0u32), width_mask, width),
    }
}

/// A fully-resolved constant value for one variable.
struct ConstVar {
    /// Combined payload (little-endian bit ordering: bit 0 = LSB).
    payload: BigUint,
    /// Combined 4-state mask.
    mask: BigUint,
}

/// Inline constant variables: rewrite Input references → Constant nodes.
///
/// Returns `true` if any rewriting was performed.
pub fn inline_constant_variables<A: Clone + Eq + Hash + Debug + Display>(
    paths: &mut [LogicPath<A>],
    arena: &mut SLTNodeArena<A>,
) -> Result<bool, SLTNodeFactsError> {
    // 1. Identify constant variables.
    //    A variable is "fully constant" if every LogicPath targeting it has a
    //    Constant expression and no dynamic index.
    let mut const_candidates: HashMap<A, Vec<(BitAccess, NodeId)>> = HashMap::default();
    let mut non_const: HashSet<A> = HashSet::default();

    for path in paths.iter() {
        let Some(target) = path.target.var() else {
            continue;
        };
        let var = &target.id;
        if non_const.contains(var) {
            continue;
        }
        if is_const_expr(path.expr, arena) {
            const_candidates
                .entry(var.clone())
                .or_default()
                .push((target.access, path.expr));
        } else {
            non_const.insert(var.clone());
            const_candidates.remove(var);
        }
    }

    // Exclude variables that are read via dynamic index anywhere in the arena,
    // because inlining them would change the value seen by the dynamic load.
    for node in arena.iter() {
        if let SLTNode::Input {
            variable, index, ..
        } = node
            && !index.is_empty()
        {
            non_const.insert(variable.clone());
            const_candidates.remove(variable);
        }
    }

    if const_candidates.is_empty() {
        return Ok(false);
    }

    // 2. Build the combined constant value for each constant variable.
    let mut const_vars: HashMap<A, ConstVar> = HashMap::default();
    for (var, entries) in &const_candidates {
        // Determine total width from the entries.
        let total_width: usize = entries
            .iter()
            .map(|(access, _)| access.msb + 1)
            .max()
            .unwrap_or(0);
        if total_width == 0 {
            continue;
        }

        let mut payload = BigUint::from(0u32);
        let mut mask = BigUint::from(0u32);

        for &(access, expr) in entries {
            let (val, msk, _) = eval_const_expr(expr, arena);
            let entry_width = access.msb - access.lsb + 1;
            let entry_mask_bits: BigUint = (BigUint::from(1u32) << entry_width) - 1u32;
            payload |= (&val & &entry_mask_bits) << access.lsb;
            mask |= (&msk & &entry_mask_bits) << access.lsb;
        }

        // If any entry has unknown bits (mask != 0), skip this variable.
        if mask != BigUint::from(0u32) {
            continue;
        }
        const_vars.insert(var.clone(), ConstVar { payload, mask });
    }

    if const_vars.is_empty() {
        return Ok(false);
    }

    // 3. Rewrite expression trees (see rewrite_expr below): for each remaining LogicPath, recursively
    //    replace Input(const_var) nodes with Constant nodes in its expression.
    //    We allocate new nodes instead of mutating existing ones (arena is a DAG
    //    with shared nodes, so in-place mutation would corrupt unrelated paths).
    let mut rewrite_cache: HashMap<NodeId, NodeId> = HashMap::default();
    for path in paths.iter_mut() {
        if path.sources.iter().any(|s| const_vars.contains_key(&s.id)) {
            path.expr = rewrite_expr(path.expr, arena, &const_vars, &mut rewrite_cache)?;
            path.sources.retain(|src| !const_vars.contains_key(&src.id));
            path.previous_sources
                .retain(|src| !const_vars.contains_key(&src.id));
            path.address_sources
                .retain(|src| !const_vars.contains_key(&src.id));
        }
    }

    // Note: we do NOT remove LogicPaths that target constant variables.
    // Their Stores must persist so that other EUs (FF evaluation) reading from
    // working memory see the correct values.

    Ok(true)
}

/// Recursively rewrite an expression tree, replacing Input nodes that reference
/// constant variables with fresh Constant nodes. Returns the (potentially new) NodeId.
fn rewrite_expr<A: Clone + Eq + Hash + Debug + Display>(
    node: NodeId,
    arena: &mut SLTNodeArena<A>,
    const_vars: &HashMap<A, ConstVar>,
    cache: &mut HashMap<NodeId, NodeId>,
) -> Result<NodeId, SLTNodeFactsError> {
    if let Some(&cached) = cache.get(&node) {
        return Ok(cached);
    }

    let result = match arena.get(node).clone() {
        SLTNode::Input {
            variable,
            index,
            access,
            ..
        } if index.is_empty() => {
            if let Some(cv) = const_vars.get(&variable) {
                let width = access.msb - access.lsb + 1;
                let bit_mask = (BigUint::from(1u32) << width) - 1u32;
                let val = (&cv.payload >> access.lsb) & &bit_mask;
                let msk = (&cv.mask >> access.lsb) & &bit_mask;
                arena.alloc(SLTNode::Constant(val, msk, width, false))?
            } else {
                node
            }
        }
        SLTNode::Slice { expr, access } => {
            let new_expr = rewrite_expr(expr, arena, const_vars, cache)?;
            if new_expr == expr {
                node
            } else {
                arena.alloc(SLTNode::Slice {
                    expr: new_expr,
                    access,
                })?
            }
        }
        SLTNode::Binary(l, op, r) => {
            let new_l = rewrite_expr(l, arena, const_vars, cache)?;
            let new_r = rewrite_expr(r, arena, const_vars, cache)?;
            if new_l == l && new_r == r {
                node
            } else {
                arena.alloc(SLTNode::Binary(new_l, op, new_r))?
            }
        }
        SLTNode::Unary(op, inner) => {
            let new_inner = rewrite_expr(inner, arena, const_vars, cache)?;
            if new_inner == inner {
                node
            } else {
                arena.alloc(SLTNode::Unary(op, new_inner))?
            }
        }
        SLTNode::Concat(parts) => {
            let new_parts: Vec<_> = parts
                .iter()
                .map(|&(id, w)| Ok((rewrite_expr(id, arena, const_vars, cache)?, w)))
                .collect::<Result<_, SLTNodeFactsError>>()?;
            if new_parts.iter().zip(parts.iter()).all(|(a, b)| a.0 == b.0) {
                node
            } else {
                arena.alloc(SLTNode::Concat(new_parts))?
            }
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            let new_cond = rewrite_expr(cond, arena, const_vars, cache)?;
            let new_then = rewrite_expr(then_expr, arena, const_vars, cache)?;
            let new_else = rewrite_expr(else_expr, arena, const_vars, cache)?;
            if new_cond == cond && new_then == then_expr && new_else == else_expr {
                node
            } else {
                arena.alloc(SLTNode::Mux {
                    cond: new_cond,
                    then_expr: new_then,
                    else_expr: new_else,
                })?
            }
        }
        SLTNode::ForFoldGroup {
            loop_var,
            loop_width,
            loop_signed,
            start,
            step,
            trip_count,
            entry_guard,
            states,
        } => {
            // Never replace the loop variable or loop-carried state bindings
            // with a module-level constant.  If none of those IDs is a
            // constant candidate, ordinary child rewriting is context-free
            // and can share the caller's memoization table safely.
            let binding_is_constant = const_vars.contains_key(&loop_var)
                || states
                    .iter()
                    .any(|state| const_vars.contains_key(&state.target.id));
            if binding_is_constant {
                node
            } else {
                let new_entry_guard = rewrite_expr(entry_guard, arena, const_vars, cache)?;
                let new_states = states
                    .iter()
                    .map(|state| {
                        Ok(crate::logic_tree::comb::SLTForFoldGroupState {
                            target: state.target.clone(),
                            initial: rewrite_expr(state.initial, arena, const_vars, cache)?,
                            update: rewrite_expr(state.update, arena, const_vars, cache)?,
                        })
                    })
                    .collect::<Result<Vec<_>, SLTNodeFactsError>>()?;
                let unchanged = new_entry_guard == entry_guard
                    && new_states
                        .iter()
                        .zip(&states)
                        .all(|(new, old)| new.initial == old.initial && new.update == old.update);
                if unchanged {
                    node
                } else {
                    arena.alloc(SLTNode::ForFoldGroup {
                        loop_var,
                        loop_width,
                        loop_signed,
                        start,
                        step,
                        trip_count,
                        entry_guard: new_entry_guard,
                        states: new_states,
                    })?
                }
            }
        }
        _ => node,
    };

    cache.insert(node, result);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use crate::ir::UnaryOp;
    use crate::logic_tree::comb::{SLTNode, SLTNodeArena};
    use num_bigint::BigUint;

    use super::eval_const_expr;

    #[test]
    fn evaluates_two_state_bit_count_constants() {
        let mut arena = SLTNodeArena::<u32>::new();
        let value = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0b0011_0100u8),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();

        for (op, expected) in [
            (UnaryOp::PopCount, 3u8),
            (UnaryOp::CountLeadingZeros, 2u8),
            (UnaryOp::CountTrailingZeros, 2u8),
        ] {
            let node = arena.alloc(SLTNode::Unary(op, value)).unwrap();
            assert_eq!(
                eval_const_expr(node, &arena),
                (BigUint::from(expected), BigUint::from(0u8), 4),
            );
        }
    }

    #[test]
    fn zero_has_full_operand_width_leading_and_trailing_zero_counts() {
        let mut arena = SLTNodeArena::<u32>::new();
        let zero = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();

        for op in [UnaryOp::CountLeadingZeros, UnaryOp::CountTrailingZeros] {
            let node = arena.alloc(SLTNode::Unary(op, zero)).unwrap();
            assert_eq!(
                eval_const_expr(node, &arena),
                (BigUint::from(8u8), BigUint::from(0u8), 4),
            );
        }
    }

    #[test]
    fn logical_not_and_reduction_or_constants_use_dominant_known_one() {
        let mut arena = SLTNodeArena::<u32>::new();
        let known_one_and_x = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0b1000_0100u8),
                BigUint::from(0b0000_0100u8),
                8,
                false,
            ))
            .unwrap();
        let only_x = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0b0000_0100u8),
                BigUint::from(0b0000_0100u8),
                8,
                false,
            ))
            .unwrap();
        let only_z = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0b0000_0100u8),
                8,
                false,
            ))
            .unwrap();

        let known_not = arena
            .alloc(SLTNode::Unary(UnaryOp::LogicNot, known_one_and_x))
            .unwrap();
        let known_or = arena
            .alloc(SLTNode::Unary(UnaryOp::Or, known_one_and_x))
            .unwrap();
        assert_eq!(
            eval_const_expr(known_not, &arena),
            (BigUint::from(0u8), BigUint::from(0u8), 1),
        );
        assert_eq!(
            eval_const_expr(known_or, &arena),
            (BigUint::from(1u8), BigUint::from(0u8), 1),
        );

        for inner in [only_x, only_z] {
            for op in [UnaryOp::LogicNot, UnaryOp::Or] {
                let node = arena.alloc(SLTNode::Unary(op, inner)).unwrap();
                assert_eq!(
                    eval_const_expr(node, &arena),
                    (BigUint::from(1u8), BigUint::from(1u8), 1),
                    "{op:?}",
                );
            }
        }
    }
}
