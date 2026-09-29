//! Function-call expansion and expression identifier substitution.

use super::*;

pub(super) fn expand_process_calls(
    process: CombProcess,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
) -> CombProcess {
    CombProcess::new(
        process.kind,
        process.condition,
        process
            .assignments
            .into_iter()
            .map(|assignment| {
                expand_assignment_calls(assignment, functions, expression_signedness, true)
            })
            .collect(),
    )
}

pub(super) fn expand_ff_process_calls(
    process: FfProcess,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
) -> FfProcess {
    FfProcess::new(
        process.events,
        process
            .assignments
            .into_iter()
            .map(|assignment| {
                let condition = assignment.condition.map(|condition| {
                    substitute_expr_constants_with_parameter_literals(
                        expand_expr_calls(condition, functions, expression_signedness, 0, true),
                        const_env,
                        parameter_literals,
                    )
                });
                let assignment = substitute_assignment_constants_with_parameter_literals(
                    expand_assignment_calls(
                        assignment.assignment,
                        functions,
                        expression_signedness,
                        true,
                    ),
                    const_env,
                    parameter_literals,
                );
                ConditionalAssignment::new(condition, assignment)
            })
            .collect(),
    )
}

pub(super) fn expand_assignment_calls(
    assignment: Assignment,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    apply_return_type: bool,
) -> Assignment {
    Assignment::new(
        expand_lvalue_calls(assignment.lhs, functions, expression_signedness),
        expand_expr_calls(
            assignment.rhs,
            functions,
            expression_signedness,
            0,
            apply_return_type,
        ),
    )
}

fn expand_lvalue_calls(
    lvalue: LValue,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
) -> LValue {
    let expand_bound = |bound: ConstExpr| {
        let original = bound.clone();
        expr_to_lvalue_const(expand_expr_calls(
            const_expr_to_expr(bound),
            functions,
            expression_signedness,
            0,
            true,
        ))
        .unwrap_or(original)
    };
    match lvalue {
        LValue::Ident(name) => LValue::Ident(name),
        LValue::Select {
            name,
            msb,
            lsb,
            signed,
            array_slice_width,
            array_slice_reversed,
        } => LValue::Select {
            name,
            msb: expand_bound(msb),
            lsb: expand_bound(lsb),
            signed,
            array_slice_width: array_slice_width.map(expand_bound),
            array_slice_reversed,
        },
    }
}

pub(super) fn expr_signedness(
    expr: &Expr,
    identifiers: &HashMap<String, bool>,
    functions: &HashMap<String, Function>,
) -> Option<bool> {
    expr_signedness_with_return_types(expr, identifiers, functions, &HashMap::default())
}

pub(super) fn expr_signedness_with_return_types(
    expr: &Expr,
    identifiers: &HashMap<String, bool>,
    functions: &HashMap<String, Function>,
    function_return_types: &HashMap<String, FunctionReturnMetadata>,
) -> Option<bool> {
    match expr {
        Expr::Ident(name) => identifiers.get(name).copied(),
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

pub(super) fn expand_expr_calls(
    expr: Expr,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
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
            if function.params.len() != args.len() {
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
            let body = substitute_expr_idents(function.body.clone(), &env);
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
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args
                .into_iter()
                .map(|arg| substitute_expr_idents(arg, env))
                .collect(),
        },
    }
}
