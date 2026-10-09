//! Run-time exponentiation (IEEE 1800-2023 11.4.3).

use std::hash::Hash;

use celox_design::{BinaryOp, BitAccess, UnaryOp};
use celox_slt::{NodeId, SLTNode, SLTNodeArena, SLTNodeFactsError, get_width};
use num_bigint::BigUint;

/// `base ** exponent` with a run-time exponent, as a `width`-bit value.
pub fn lower_runtime_pow<A: Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    base: NodeId,
    exponent: NodeId,
    width: usize,
    exponent_signed: bool,
    result_signed: bool,
) -> Result<NodeId, SLTNodeFactsError> {
    let exponent_width = get_width(exponent, arena);
    debug_assert!(width > 0);
    debug_assert!(exponent_width > 0);

    let constant = |arena: &mut SLTNodeArena<A>, value: BigUint, mask: BigUint| {
        arena.alloc(SLTNode::Constant(value, mask, width, result_signed))
    };
    let zero = constant(arena, BigUint::from(0u8), BigUint::from(0u8))?;
    let one = constant(arena, BigUint::from(1u8), BigUint::from(0u8))?;
    let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
    let minus_one = constant(arena, width_mask.clone(), BigUint::from(0u8))?;
    let unknown = constant(arena, BigUint::from(0u8), width_mask)?;

    // Exponentiation by squaring keeps the generated graph linear in the
    // exponent width instead of in its runtime value.
    let mut result = one;
    let mut factor = base;
    let exponent_lsb = arena.alloc(SLTNode::Slice {
        expr: exponent,
        access: BitAccess::new(0, 0),
    })?;
    let magnitude_width = exponent_width - usize::from(exponent_signed);
    for bit_index in 0..magnitude_width {
        let bit = arena.alloc(SLTNode::Slice {
            expr: exponent,
            access: BitAccess::new(bit_index, bit_index),
        })?;
        let product = arena.alloc(SLTNode::Binary(result, BinaryOp::Mul, factor))?;
        result = arena.alloc(SLTNode::Mux {
            cond: bit,
            then_expr: product,
            else_expr: result,
        })?;
        if bit_index + 1 != magnitude_width {
            factor = arena.alloc(SLTNode::Binary(factor, BinaryOp::Mul, factor))?;
        }
    }

    if exponent_signed {
        let sign = arena.alloc(SLTNode::Slice {
            expr: exponent,
            access: BitAccess::new(exponent_width - 1, exponent_width - 1),
        })?;
        let base_is_zero = arena.alloc(SLTNode::Binary(base, BinaryOp::Eq, zero))?;
        let base_is_one = arena.alloc(SLTNode::Binary(base, BinaryOp::Eq, one))?;
        let base_is_minus_one = arena.alloc(SLTNode::Binary(base, BinaryOp::Eq, minus_one))?;
        let minus_one_result = arena.alloc(SLTNode::Mux {
            cond: exponent_lsb,
            then_expr: minus_one,
            else_expr: one,
        })?;
        let negative_nonzero = arena.alloc(SLTNode::Mux {
            cond: base_is_minus_one,
            then_expr: minus_one_result,
            else_expr: zero,
        })?;
        let negative_nonzero = arena.alloc(SLTNode::Mux {
            cond: base_is_one,
            then_expr: one,
            else_expr: negative_nonzero,
        })?;
        let negative_result = arena.alloc(SLTNode::Mux {
            cond: base_is_zero,
            then_expr: unknown,
            else_expr: negative_nonzero,
        })?;
        result = arena.alloc(SLTNode::Mux {
            cond: sign,
            then_expr: negative_result,
            else_expr: result,
        })?;
    }

    // IEEE power semantics produce an entirely unknown result if either
    // operand contains X/Z. Comparing each operand with its two-state image is
    // known one exactly when it has no unknown bits; muxing against all-X
    // expands any unknown predicate to the complete result width.
    let base_two_state = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, base))?;
    let exponent_two_state = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, exponent))?;
    let base_known = arena.alloc(SLTNode::Binary(base, BinaryOp::Eq, base_two_state))?;
    let exponent_known =
        arena.alloc(SLTNode::Binary(exponent, BinaryOp::Eq, exponent_two_state))?;
    let operands_known = arena.alloc(SLTNode::Binary(
        base_known,
        BinaryOp::LogicAnd,
        exponent_known,
    ))?;
    arena.alloc(SLTNode::Mux {
        cond: operands_known,
        then_expr: result,
        else_expr: unknown,
    })
}
