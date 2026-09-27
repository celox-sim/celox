//! Compile-time evaluation of symbolic logic trees.

use super::*;

fn slt_value_mask(width: usize) -> BigUint {
    (BigUint::from(1u8) << width) - BigUint::from(1u8)
}

/// Try to evaluate an SLT node as a compile-time constant.
/// Returns `Some((value, mask))` if the entire subtree is constant, `None` otherwise.
pub(super) fn try_const_eval<A: Hash + Eq + Clone>(
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
            let width = crate::get_width(node_id, arena);
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
