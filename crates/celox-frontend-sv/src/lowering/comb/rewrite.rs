//! Rewrites of expressions before they are lowered: identifiers replaced by
//! the literals of their known values.

use super::*;

/// Replace identifiers by literals, in expressions and select bounds.
pub(super) fn substitute_literals(
    expr: &sv::ir::Expr,
    literals: &HashMap<String, (String, i128)>,
) -> sv::ir::Expr {
    substitute(expr, literals, true)
}

/// `expr` with the known values `literals` substituted into the positions
/// of its selects only, its operands kept as the variables they name.
pub(super) fn substitute_select_bounds(
    expr: &sv::ir::Expr,
    literals: &HashMap<String, (String, i128)>,
) -> sv::ir::Expr {
    substitute(expr, literals, false)
}

/// `expr` with the known values `literals` substituted, into its operands
/// too where `operands`.
fn substitute(
    expr: &sv::ir::Expr,
    literals: &HashMap<String, (String, i128)>,
    operands: bool,
) -> sv::ir::Expr {
    fn constant(
        expr: &sv::ir::ConstExpr,
        literals: &HashMap<String, (String, i128)>,
    ) -> sv::ir::ConstExpr {
        use sv::ir::ConstExpr;
        match expr {
            ConstExpr::Ident(name) => match literals.get(name) {
                Some((_, value)) => ConstExpr::Literal(value.to_string()),
                None => expr.clone(),
            },
            ConstExpr::Literal(_) => expr.clone(),
            ConstExpr::SelectRange {
                expr,
                msb,
                lsb,
                signed,
            } => ConstExpr::SelectRange {
                // Keep the declaration available: bounds may be in declared
                // packed coordinates, or select an unpacked element at run time.
                expr: Box::new(if matches!(&**expr, ConstExpr::Ident(_)) {
                    (**expr).clone()
                } else {
                    constant(expr, literals)
                }),
                msb: Box::new(constant(msb, literals)),
                lsb: Box::new(constant(lsb, literals)),
                signed: *signed,
            },
            // Likewise for a bit select, such as a known vector's bit
            // inside a run-time index: its base stays the variable, whose
            // bits past it read X (IEEE 1800-2023 11.5.1).
            ConstExpr::Select { expr, bit } => ConstExpr::Select {
                expr: Box::new(if matches!(&**expr, ConstExpr::Ident(_)) {
                    (**expr).clone()
                } else {
                    constant(expr, literals)
                }),
                bit: Box::new(constant(bit, literals)),
            },
            ConstExpr::Function { callee, args, site } => ConstExpr::Function {
                callee: callee.clone(),
                site: *site,
                args: args.iter().map(|arg| constant(arg, literals)).collect(),
            },
            ConstExpr::Resize {
                expr,
                width,
                signed,
            } => ConstExpr::Resize {
                expr: Box::new(constant(expr, literals)),
                width: *width,
                signed: *signed,
            },
            ConstExpr::Unary { op, expr } => ConstExpr::Unary {
                op: *op,
                expr: Box::new(constant(expr, literals)),
            },
            ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
                left: Box::new(constant(left, literals)),
                op: *op,
                right: Box::new(constant(right, literals)),
            },
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => ConstExpr::Mux {
                condition: Box::new(constant(condition, literals)),
                then_expr: Box::new(constant(then_expr, literals)),
                else_expr: Box::new(constant(else_expr, literals)),
            },
        }
    }
    use sv::ir::Expr;
    let go = |expr: &Expr| substitute(expr, literals, operands);
    match expr {
        Expr::Ident(name) => match literals.get(name).filter(|_| operands) {
            Some((literal, _)) => Expr::Literal(literal.clone()),
            None => expr.clone(),
        },
        Expr::Literal(_) => expr.clone(),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => {
            fn has_ident(expr: &sv::ir::ConstExpr) -> bool {
                use sv::ir::ConstExpr;
                match expr {
                    ConstExpr::Ident(_) => true,
                    ConstExpr::Literal(_) => false,
                    ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                        has_ident(expr) || has_ident(msb) || has_ident(lsb)
                    }
                    ConstExpr::Select { expr, bit } => has_ident(expr) || has_ident(bit),
                    ConstExpr::Function { args, .. } => args.iter().any(has_ident),
                    ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
                        has_ident(expr)
                    }
                    ConstExpr::Binary { left, right, .. } => has_ident(left) || has_ident(right),
                    ConstExpr::Mux {
                        condition,
                        then_expr,
                        else_expr,
                    } => has_ident(condition) || has_ident(then_expr) || has_ident(else_expr),
                }
            }
            let msb = constant(msb, literals);
            let lsb = constant(lsb, literals);
            // A select at a run-time position reads its base variable, whose
            // value comes from the store; a literal base cannot be selected
            // at a run-time position.
            let base = if matches!(**expr, Expr::Ident(_)) && (has_ident(&msb) || has_ident(&lsb)) {
                (**expr).clone()
            } else {
                go(expr)
            };
            Expr::Select {
                expr: Box::new(base),
                msb,
                lsb,
                signed: *signed,
            }
        }
        Expr::Concat(parts) => Expr::Concat(parts.iter().map(go).collect()),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count: constant(count, literals),
            parts: parts.iter().map(go).collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(go(expr)),
            width: *width,
            signed: *signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op: *op,
            expr: Box::new(go(expr)),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(go(left)),
            op: *op,
            right: Box::new(go(right)),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(go(condition)),
            then_expr: Box::new(go(then_expr)),
            else_expr: Box::new(go(else_expr)),
        },
        Expr::Call {
            callee,
            args,
            arg_order,
        } => Expr::Call {
            arg_order: arg_order.clone(),
            callee: callee.clone(),
            args: args.iter().map(|arg| arg.as_ref().map(go)).collect(),
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: Box::new(go(expr)),
            items: items
                .iter()
                .map(|item| item.map(&mut |operand| go(operand)))
                .collect(),
        },
    }
}
