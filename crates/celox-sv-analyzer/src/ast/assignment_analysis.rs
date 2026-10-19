//! Complementary two-state conditions.

use super::*;

pub(super) fn two_state_conditions_are_complements(
    state: &AnalysisState,
    left: &Expr,
    right: &Expr,
    packed_dimensions: &PackedDimensions,
) -> bool {
    if let (Some((left, left_positive)), Some((right, right_positive))) = (
        normalized_two_state_boolean(state, left, packed_dimensions),
        normalized_two_state_boolean(state, right, packed_dimensions),
    ) && left == right
        && left_positive != right_positive
    {
        return true;
    }
    let is_complement = |candidate: &Expr, other: &Expr| {
        matches!(
            candidate,
            Expr::Unary {
                op: UnaryOp::LogicNot,
                expr,
            } if &**expr == other && expr_is_two_state(state, other, packed_dimensions)
        )
    };
    let are_inverse_equalities = |left: &Expr, right: &Expr| {
        let (
            Expr::Binary {
                left: left_lhs,
                op: left_op,
                right: left_rhs,
            },
            Expr::Binary {
                left: right_lhs,
                op: right_op,
                right: right_rhs,
            },
        ) = (left, right)
        else {
            return false;
        };
        let operands_match = (left_lhs == right_lhs && left_rhs == right_rhs)
            || (left_lhs == right_rhs && left_rhs == right_lhs);
        if !operands_match {
            return false;
        }
        match (left_op, right_op) {
            (BinaryOp::Eq, BinaryOp::Ne) | (BinaryOp::Ne, BinaryOp::Eq) => {
                expr_is_two_state(state, left_lhs, packed_dimensions)
                    && expr_is_two_state(state, left_rhs, packed_dimensions)
            }
            (BinaryOp::EqCase, BinaryOp::NeCase) | (BinaryOp::NeCase, BinaryOp::EqCase) => true,
            _ => false,
        }
    };
    is_complement(left, right) || is_complement(right, left) || are_inverse_equalities(left, right)
}

fn normalized_two_state_boolean<'a>(
    state: &AnalysisState,
    expr: &'a Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<(&'a Expr, bool)> {
    match expr {
        Expr::Resize {
            expr: operand,
            width,
            ..
        } if expr_static_width(state, operand, packed_dimensions)
            .is_some_and(|source| source <= *width)
            && expr_is_two_state(state, operand, packed_dimensions) =>
        {
            return normalized_two_state_boolean(state, operand, packed_dimensions);
        }
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: operand,
        } if expr_is_two_state(state, operand, packed_dimensions) => {
            return normalized_two_state_boolean(state, operand, packed_dimensions);
        }
        _ => {}
    }
    if let Expr::Unary { op, expr } = expr
        && (*op == UnaryOp::LogicNot
            || (*op == UnaryOp::BitNot
                && expr_static_width(state, expr, packed_dimensions) == Some(1)
                && expr_is_two_state(state, expr, packed_dimensions)))
    {
        let (expr, positive) = normalized_two_state_boolean(state, expr, packed_dimensions)?;
        return Some((expr, !positive));
    }
    if let Expr::Binary { left, op, right } = expr
        && matches!(
            op,
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqCase | BinaryOp::NeCase
        )
    {
        let nonzero_side = if expr_is_constant_zero(state, left, &packed_dimensions.const_env) {
            &**right
        } else if expr_is_constant_zero(state, right, &packed_dimensions.const_env) {
            &**left
        } else {
            return expr_is_two_state(state, expr, packed_dimensions).then_some((expr, true));
        };
        if expr_is_two_state(state, nonzero_side, packed_dimensions) {
            return Some((nonzero_side, matches!(op, BinaryOp::Ne | BinaryOp::NeCase)));
        }
    }
    expr_is_two_state(state, expr, packed_dimensions).then_some((expr, true))
}

fn expr_is_constant_zero(
    state: &AnalysisState,
    expr: &Expr,
    const_env: &HashMap<String, i128>,
) -> bool {
    expr_to_const(state, expr.clone()).and_then(|expr| eval_ast_const_expr(state, &expr, const_env))
        == Some(0)
}
