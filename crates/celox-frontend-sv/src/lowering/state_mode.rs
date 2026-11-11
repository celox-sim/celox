//! Rewriting of expressions for two-state or four-state lowering.

use super::*;

/// Borrow expressions that need no state-mode conversion. Select bounds and
/// repeat counts are not rewritten by this pass, so do not copy their trees
/// merely to inspect the expression that contains them.
pub(super) fn expr_for_state_mode(
    expr: &sv::ir::Expr,
    four_state: bool,
) -> std::borrow::Cow<'_, sv::ir::Expr> {
    if needs_state_mode_rewrite(expr, four_state) {
        std::borrow::Cow::Owned(rewrite_expr_for_state_mode(expr, four_state))
    } else {
        std::borrow::Cow::Borrowed(expr)
    }
}

fn needs_state_mode_rewrite(expr: &sv::ir::Expr, four_state: bool) -> bool {
    use sv::ir::Expr;
    let needs = |expr| needs_state_mode_rewrite(expr, four_state);
    match expr {
        Expr::Literal(_) => !four_state && expr_is_unknown_literal(expr),
        Expr::Ident(_) => false,
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            needs(expr)
        }
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => parts.iter().any(needs),
        Expr::Binary { left, op, right } => {
            let keep_unknown =
                four_state || matches!(op, sv::ir::BinaryOp::EqCase | sv::ir::BinaryOp::NeCase);
            needs_state_mode_rewrite(left, keep_unknown)
                || needs_state_mode_rewrite(right, keep_unknown)
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            matches!(&**then_expr, Expr::Literal(literal) if literal == sv::DIV_ZERO_UNKNOWN_LITERAL)
                || needs(condition)
                || needs(then_expr)
                || needs(else_expr)
        }
        Expr::Inside { expr, items } => {
            needs(expr) || items.iter().any(|item| item.exprs().into_iter().any(needs))
        }
        Expr::Call { args, .. } => args.iter().flatten().any(needs),
    }
}

fn rewrite_expr_for_state_mode(expr: &sv::ir::Expr, four_state: bool) -> sv::ir::Expr {
    match expr {
        sv::ir::Expr::Mux {
            then_expr,
            else_expr,
            ..
        } if matches!(
            &**then_expr,
            sv::ir::Expr::Literal(literal)
                if literal == sv::DIV_ZERO_UNKNOWN_LITERAL
        ) =>
        {
            if four_state {
                let sv::ir::Expr::Mux {
                    condition,
                    else_expr,
                    ..
                } = expr
                else {
                    unreachable!()
                };
                let else_expr = rewrite_expr_for_state_mode(else_expr, four_state);
                // The unknown result has the width and signedness of the
                // division: an X operand makes the whole sum X (IEEE 1800-2023
                // 11.4.3), and `'x` alone would make the result unsigned.
                let unknown = match &else_expr {
                    sv::ir::Expr::Binary { left, right, .. } => sv::ir::Expr::Binary {
                        left: Box::new(sv::ir::Expr::Binary {
                            left: left.clone(),
                            op: sv::ir::BinaryOp::Add,
                            right: right.clone(),
                        }),
                        op: sv::ir::BinaryOp::Add,
                        right: Box::new(sv::ir::Expr::Literal("1'sbx".to_string())),
                    },
                    _ => sv::ir::Expr::Literal("'x".to_string()),
                };
                sv::ir::Expr::Mux {
                    condition: Box::new(rewrite_expr_for_state_mode(condition, four_state)),
                    then_expr: Box::new(unknown),
                    else_expr: Box::new(else_expr),
                }
            } else {
                rewrite_expr_for_state_mode(else_expr, four_state)
            }
        }
        sv::ir::Expr::Literal(literal) if !four_state && expr_is_unknown_literal(expr) => {
            if unbased_fill_literal(literal).is_some() {
                sv::ir::Expr::Literal("'0".to_string())
            } else {
                sv::ir::Expr::Unary {
                    op: sv::ir::UnaryOp::ToTwoState,
                    expr: Box::new(expr.clone()),
                }
            }
        }
        sv::ir::Expr::Ident(_) | sv::ir::Expr::Literal(_) => expr.clone(),
        sv::ir::Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => sv::ir::Expr::Select {
            expr: Box::new(rewrite_expr_for_state_mode(expr, four_state)),
            msb: msb.clone(),
            lsb: lsb.clone(),
            signed: *signed,
        },
        sv::ir::Expr::Concat(parts) => sv::ir::Expr::Concat(
            parts
                .iter()
                .map(|part| rewrite_expr_for_state_mode(part, four_state))
                .collect(),
        ),
        sv::ir::Expr::RepeatConcat { count, parts } => sv::ir::Expr::RepeatConcat {
            count: count.clone(),
            parts: parts
                .iter()
                .map(|part| rewrite_expr_for_state_mode(part, four_state))
                .collect(),
        },
        sv::ir::Expr::Resize {
            expr,
            width,
            signed,
        } => sv::ir::Expr::Resize {
            expr: Box::new(rewrite_expr_for_state_mode(expr, four_state)),
            width: *width,
            signed: *signed,
        },
        sv::ir::Expr::Unary { op, expr } => sv::ir::Expr::Unary {
            op: *op,
            expr: Box::new(rewrite_expr_for_state_mode(expr, four_state)),
        },
        sv::ir::Expr::Binary { left, op, right }
            if !four_state && matches!(op, sv::ir::BinaryOp::EqCase | sv::ir::BinaryOp::NeCase) =>
        {
            // Storage remains two-state; exact comparison operands retain
            // literal X/Z until value and mask are compared (11.4.5).
            sv::ir::Expr::Binary {
                left: Box::new(rewrite_expr_for_state_mode(left, true)),
                op: *op,
                right: Box::new(rewrite_expr_for_state_mode(right, true)),
            }
        }
        sv::ir::Expr::Binary { left, op, right } => sv::ir::Expr::Binary {
            left: Box::new(rewrite_expr_for_state_mode(left, four_state)),
            op: *op,
            right: Box::new(rewrite_expr_for_state_mode(right, four_state)),
        },
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => sv::ir::Expr::Mux {
            condition: Box::new(rewrite_expr_for_state_mode(condition, four_state)),
            then_expr: Box::new(rewrite_expr_for_state_mode(then_expr, four_state)),
            else_expr: Box::new(rewrite_expr_for_state_mode(else_expr, four_state)),
        },
        sv::ir::Expr::Inside { expr, items } => sv::ir::Expr::Inside {
            expr: Box::new(rewrite_expr_for_state_mode(expr, four_state)),
            items: items
                .iter()
                .map(|item| {
                    item.map(&mut |operand| rewrite_expr_for_state_mode(operand, four_state))
                })
                .collect(),
        },
        sv::ir::Expr::Call {
            callee,
            args,
            arg_order,
        } => sv::ir::Expr::Call {
            arg_order: arg_order.clone(),
            callee: callee.clone(),
            args: args
                .iter()
                .map(|arg| {
                    arg.as_ref()
                        .map(|arg| rewrite_expr_for_state_mode(arg, four_state))
                })
                .collect(),
        },
    }
}
