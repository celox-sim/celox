//! Case-label helpers: two-state expression classification.

use super::*;

pub(super) fn expr_is_two_state(
    state: &AnalysisState,
    expr: &Expr,
    packed_dimensions: &PackedDimensions,
) -> bool {
    match expr {
        Expr::Ident(name) => {
            packed_dimensions
                .get(name)
                .is_some_and(|dimensions| dimensions.is_2state)
                || packed_dimensions.const_env.contains_key(name)
        }
        Expr::Literal(value) => typecheck::parse_integral_literal(value)
            .is_some_and(|literal| literal.mask == num_bigint::BigUint::default()),
        Expr::Select { expr, msb, lsb, .. } => {
            expr_is_two_state(state, expr, packed_dimensions)
                && select_bounds_are_statically_valid(state, expr, msb, lsb, packed_dimensions)
        }
        Expr::Resize { expr, .. } => expr_is_two_state(state, expr, packed_dimensions),
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => parts
            .iter()
            .all(|part| expr_is_two_state(state, part, packed_dimensions)),
        Expr::Unary { op, expr } => {
            *op == UnaryOp::ToTwoState || expr_is_two_state(state, expr, packed_dimensions)
        }
        Expr::Binary { left, op, right } => {
            matches!(op, BinaryOp::EqCase | BinaryOp::NeCase)
                || (expr_is_two_state(state, left, packed_dimensions)
                    && expr_is_two_state(state, right, packed_dimensions)
                    && (!matches!(op, BinaryOp::Div | BinaryOp::Mod)
                        || expr_to_const(state, (**right).clone())
                            .and_then(|right| {
                                eval_ast_const_expr(state, &right, &packed_dimensions.const_env)
                            })
                            .is_some_and(|right| right != 0)))
        }
        Expr::Inside { expr, items } => {
            expr_is_two_state(state, expr, packed_dimensions)
                && items.iter().all(|item| {
                    item.exprs()
                        .into_iter()
                        .all(|operand| expr_is_two_state(state, operand, packed_dimensions))
                })
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_is_two_state(state, then_expr, packed_dimensions)
                && (then_expr == else_expr
                    || (expr_is_two_state(state, condition, packed_dimensions)
                        && expr_is_two_state(state, else_expr, packed_dimensions)))
        }
        Expr::Call {
            callee: Callee::System(function),
            ..
        } => function.result_type().is_some(),
        Expr::Call {
            callee: Callee::String(op),
            ..
        } => !matches!(op, crate::system_functions::StringOp::Atoi(_)),
        // Properties are 2-state.
        Expr::Call {
            callee: Callee::Checked { .. },
            ..
        } => false,
        Expr::Call {
            callee: Callee::Object(_),
            ..
        } => true,
        Expr::Call {
            callee: Callee::Subroutine(name),
            ..
        } => packed_dimensions
            .function_return_types
            .get(name)
            .is_some_and(|metadata| metadata.is_2state),
    }
}

fn select_bounds_are_statically_valid(
    state: &AnalysisState,
    expr: &Expr,
    msb: &ConstExpr,
    lsb: &ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> bool {
    let (Some(msb), Some(lsb)) = (
        eval_ast_const_expr(state, msb, &packed_dimensions.const_env),
        eval_ast_const_expr(state, lsb, &packed_dimensions.const_env),
    ) else {
        return false;
    };
    let (valid_low, valid_high) = if let Expr::Ident(name) = expr {
        let Some(LValue::Select { msb, lsb, .. }) = whole_packed_lvalue(name, packed_dimensions)
        else {
            return false;
        };
        let (Some(msb), Some(lsb)) = (
            eval_ast_const_expr(state, &msb, &packed_dimensions.const_env),
            eval_ast_const_expr(state, &lsb, &packed_dimensions.const_env),
        ) else {
            return false;
        };
        (msb.min(lsb), msb.max(lsb))
    } else {
        let Some(width) = expr_static_width(state, expr, packed_dimensions)
            .and_then(|width| width.checked_sub(1))
            .and_then(|high| i128::try_from(high).ok())
        else {
            return false;
        };
        (0, width)
    };
    msb.min(lsb) >= valid_low && msb.max(lsb) <= valid_high
}
