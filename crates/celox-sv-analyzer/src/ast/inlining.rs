//! Function-call expansion and expression identifier substitution.

use super::*;

pub(super) fn expr_signedness(
    expr: &Expr,
    identifiers: &impl Signedness,
    functions: &HashMap<String, Function>,
) -> Option<bool> {
    expr_signedness_with_return_types(expr, identifiers, functions, &HashMap::default())
}

pub(super) fn expr_signedness_with_return_types(
    expr: &Expr,
    identifiers: &impl Signedness,
    functions: &HashMap<String, Function>,
    function_return_types: &HashMap<String, FunctionReturnMetadata>,
) -> Option<bool> {
    match expr {
        Expr::Ident(name) => identifiers.signedness(name),
        Expr::Literal(literal) => {
            typecheck::parse_integral_literal(literal).map(|literal| literal.signed)
        }
        Expr::Select { signed, .. } => Some(*signed),
        Expr::Concat(_) | Expr::RepeatConcat { .. } => Some(false),
        Expr::Resize { signed, .. } => Some(*signed),
        Expr::Unary { op, expr } => {
            if matches!(
                op,
                UnaryOp::LogicNot | UnaryOp::RedAnd | UnaryOp::RedOr | UnaryOp::RedXor
            ) {
                Some(false)
            } else {
                expr_signedness_with_return_types(
                    expr,
                    identifiers,
                    functions,
                    function_return_types,
                )
            }
        }
        Expr::Binary { left, op, right } => {
            if matches!(
                op,
                BinaryOp::LogicAnd
                    | BinaryOp::LogicOr
                    | BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::EqCase
                    | BinaryOp::NeCase
                    | BinaryOp::EqWildcard
                    | BinaryOp::NeWildcard
                    | BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
            ) {
                Some(false)
            } else if matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
                expr_signedness_with_return_types(
                    left,
                    identifiers,
                    functions,
                    function_return_types,
                )
            } else {
                Some(
                    expr_signedness_with_return_types(
                        left,
                        identifiers,
                        functions,
                        function_return_types,
                    )? && expr_signedness_with_return_types(
                        right,
                        identifiers,
                        functions,
                        function_return_types,
                    )?,
                )
            }
        }
        Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => Some(
            expr_signedness_with_return_types(
                then_expr,
                identifiers,
                functions,
                function_return_types,
            )? && expr_signedness_with_return_types(
                else_expr,
                identifiers,
                functions,
                function_return_types,
            )?,
        ),
        Expr::Inside { .. } => Some(false),
        Expr::Call { name, args } => typecheck::bit_vector_function_return_type(name, args.len())
            .map(|(_, signed)| signed)
            .or_else(|| functions.get(name).map(|function| function.return_signed))
            .or_else(|| {
                function_return_types
                    .get(name)
                    .map(|metadata| metadata.signed)
            }),
    }
}

/// Whether `expr` calls a subroutine, which may have side effects.
fn calls_subroutine(expr: &Expr) -> bool {
    fn constant(expr: &ConstExpr) -> bool {
        match expr {
            ConstExpr::Function { name, args, .. } => {
                !name.starts_with('$') || args.iter().any(constant)
            }
            ConstExpr::Select { expr, bit } => constant(expr) || constant(bit),
            ConstExpr::Unary { expr, .. } => constant(expr),
            ConstExpr::Binary { left, right, .. } => constant(left) || constant(right),
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => constant(condition) || constant(then_expr) || constant(else_expr),
            ConstExpr::Literal(_) | ConstExpr::Ident(_) => false,
        }
    }
    match expr {
        Expr::Call { name, args } => !name.starts_with('$') || args.iter().any(calls_subroutine),
        Expr::Ident(_) | Expr::Literal(_) => false,
        Expr::Select { expr, msb, lsb, .. } => {
            calls_subroutine(expr) || constant(msb) || constant(lsb)
        }
        Expr::Concat(parts) => parts.iter().any(calls_subroutine),
        Expr::RepeatConcat { count, parts } => {
            constant(count) || parts.iter().any(calls_subroutine)
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => calls_subroutine(expr),
        Expr::Binary { left, right, .. } => calls_subroutine(left) || calls_subroutine(right),
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            calls_subroutine(condition)
                || calls_subroutine(then_expr)
                || calls_subroutine(else_expr)
        }
        Expr::Inside { expr, items } => {
            calls_subroutine(expr)
                || items
                    .iter()
                    .any(|item| item.exprs().into_iter().any(calls_subroutine))
        }
    }
}

pub(super) fn expand_expr_calls(
    expr: Expr,
    functions: &HashMap<String, Function>,
    expression_signedness: &impl Signedness,
    depth: usize,
    apply_return_type: bool,
) -> Expr {
    if depth > 32 {
        return expr;
    }
    match expr {
        Expr::Ident(name) => Expr::Ident(name),
        Expr::Literal(value) => Expr::Literal(value),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(expand_expr_calls(
                *expr,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
            msb,
            lsb,
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| {
                    expand_expr_calls(
                        part,
                        functions,
                        expression_signedness,
                        depth,
                        apply_return_type,
                    )
                })
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts
                .into_iter()
                .map(|part| {
                    expand_expr_calls(
                        part,
                        functions,
                        expression_signedness,
                        depth,
                        apply_return_type,
                    )
                })
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(expand_expr_calls(
                *expr,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(expand_expr_calls(
                *expr,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(expand_expr_calls(
                *left,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
            op,
            right: Box::new(expand_expr_calls(
                *right,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(expand_expr_calls(
                *condition,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
            then_expr: Box::new(expand_expr_calls(
                *then_expr,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
            else_expr: Box::new(expand_expr_calls(
                *else_expr,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: Box::new(expand_expr_calls(
                *expr,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
            items: items
                .into_iter()
                .map(|item| {
                    item.map(&mut |operand| {
                        expand_expr_calls(
                            operand,
                            functions,
                            expression_signedness,
                            depth,
                            apply_return_type,
                        )
                    })
                })
                .collect(),
        },
        Expr::Call { name, args } => {
            let args = args
                .into_iter()
                .map(|arg| {
                    expand_expr_calls(
                        arg,
                        functions,
                        expression_signedness,
                        depth,
                        apply_return_type,
                    )
                })
                .collect::<Vec<_>>();
            let Some(function) = functions.get(&name) else {
                return Expr::Call { name, args };
            };
            let Some(function_body) = &function.body else {
                return Expr::Call { name, args };
            };
            // Inlining copies or drops an argument with its parameter; an
            // argument that calls a subroutine runs exactly once instead.
            if function.params.len() != args.len() || args.iter().any(calls_subroutine) {
                return Expr::Call { name, args };
            }
            let env = function
                .params
                .iter()
                .zip(args)
                .map(|(param, arg)| {
                    let mut arg = if apply_return_type && let Some(width) = param.width {
                        let assigned = Expr::Resize {
                            signed: expr_signedness(&arg, expression_signedness, functions)
                                .unwrap_or(false),
                            expr: Box::new(arg),
                            width,
                        };
                        Expr::Resize {
                            expr: Box::new(assigned),
                            width,
                            signed: param.signed,
                        }
                    } else {
                        arg
                    };
                    if param.is_2state {
                        arg = Expr::Unary {
                            op: UnaryOp::ToTwoState,
                            expr: Box::new(arg),
                        };
                    }
                    (param.name.clone(), arg)
                })
                .collect::<HashMap<_, _>>();
            let body = substitute_expr_idents(function_body.clone(), &env);
            let expanded = expand_expr_calls(
                body,
                functions,
                expression_signedness,
                depth + 1,
                apply_return_type,
            );
            let mut expanded = if apply_return_type && let Some(width) = function.return_width {
                let expression_signed =
                    expr_signedness(&expanded, expression_signedness, functions).unwrap_or(false);
                let assigned = if expression_signed == function.return_signed {
                    expanded
                } else {
                    Expr::Resize {
                        signed: expression_signed,
                        expr: Box::new(expanded),
                        width,
                    }
                };
                Expr::Resize {
                    expr: Box::new(assigned),
                    width,
                    signed: function.return_signed,
                }
            } else {
                expanded
            };
            if function.return_is_2state {
                expanded = Expr::Unary {
                    op: UnaryOp::ToTwoState,
                    expr: Box::new(expanded),
                };
            }
            expanded
        }
    }
}

pub(super) fn substitute_expr_idents(expr: Expr, env: &HashMap<String, Expr>) -> Expr {
    match expr {
        Expr::Ident(name) => env.get(&name).cloned().unwrap_or(Expr::Ident(name)),
        Expr::Literal(value) => Expr::Literal(value),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(substitute_expr_idents(*expr, env)),
            msb,
            lsb,
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| substitute_expr_idents(part, env))
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts
                .into_iter()
                .map(|part| substitute_expr_idents(part, env))
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(substitute_expr_idents(*expr, env)),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(substitute_expr_idents(*expr, env)),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(substitute_expr_idents(*left, env)),
            op,
            right: Box::new(substitute_expr_idents(*right, env)),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(substitute_expr_idents(*condition, env)),
            then_expr: Box::new(substitute_expr_idents(*then_expr, env)),
            else_expr: Box::new(substitute_expr_idents(*else_expr, env)),
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: Box::new(substitute_expr_idents(*expr, env)),
            items: items
                .into_iter()
                .map(|item| item.map(&mut |operand| substitute_expr_idents(operand, env)))
                .collect(),
        },
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args
                .into_iter()
                .map(|arg| substitute_expr_idents(arg, env))
                .collect(),
        },
    }
}
