//! Constant expression parsing, substitution, and generate-iteration helpers.

use super::*;

/// The value of an `Option` in a constant-expression parser, which returns
/// `Ok(None)` for a form it leaves to the typed expression path.
macro_rules! some {
    ($option:expr) => {
        match $option {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}

/// The value of a nested constant-expression parse: its error is returned,
/// and a form it leaves to the typed path is left to it here too.
macro_rules! parsed {
    ($result:expr) => {
        match $result? {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}

/// The next value of a genvar, or `None` for an update Celox does not
/// support. `evaluate` converts the right-hand side of an assignment update.
pub(super) fn next_genvar_value(
    value: i128,
    iteration: &sv_parser::GenvarIteration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    evaluate: impl FnOnce(&sv_parser::ConstantExpression) -> Converted<Option<ConstExpr>>,
) -> Converted<Option<i128>> {
    Ok(match iteration {
        sv_parser::GenvarIteration::Prefix(iteration) => {
            match some!(syntax_tree.get_str(&iteration.nodes.0.nodes.0.nodes.0)) {
                "++" => value.checked_add(1),
                "--" => value.checked_sub(1),
                _ => None,
            }
        }
        sv_parser::GenvarIteration::Suffix(iteration) => {
            match some!(syntax_tree.get_str(&iteration.nodes.1.nodes.0.nodes.0)) {
                "++" => value.checked_add(1),
                "--" => value.checked_sub(1),
                _ => None,
            }
        }
        sv_parser::GenvarIteration::Assignment(iteration) => {
            let op = some!(syntax_tree.get_str(&iteration.nodes.1.nodes.0.nodes.0));
            let rhs = parsed!(evaluate(&iteration.nodes.2.nodes.0));
            if op == "=" {
                return Ok(eval_ast_const_expr(&rhs, const_env));
            }
            let op = match op {
                "+=" => BinaryOp::Add,
                "-=" => BinaryOp::Sub,
                "*=" => BinaryOp::Mul,
                "/=" => BinaryOp::Div,
                "%=" => BinaryOp::Mod,
                "<<=" => BinaryOp::Shl,
                ">>=" => BinaryOp::Shr,
                _ => return Ok(None),
            };
            // A compound assignment performs the typed binary operation before
            // assignment conversion. Do not erase the RHS width or signedness.
            eval_ast_const_expr(
                &ConstExpr::Binary {
                    left: Box::new(ConstExpr::Literal(format_typed_parameter_literal(
                        value, 32, true,
                    ))),
                    op,
                    right: Box::new(rhs),
                },
                const_env,
            )
        }
    })
}

pub(super) fn bind_generate_parameter(
    parameter: Parameter,
    const_env: &mut HashMap<String, i128>,
    parameter_literals: &mut HashMap<String, Expr>,
) {
    let mut parameter_types = parameter_types_from_const_env(const_env);
    bind_generate_parameter_with_types(
        parameter,
        const_env,
        parameter_literals,
        &mut parameter_types,
    );
}

pub(super) fn bind_generate_parameter_with_types(
    parameter: Parameter,
    const_env: &mut HashMap<String, i128>,
    parameter_literals: &mut HashMap<String, Expr>,
    parameter_types: &mut HashMap<String, ExprType>,
) {
    let unbounded = parameter
        .value()
        .is_some_and(|value| parameters::is_unbounded(value, const_env));
    if unbounded {
        const_env.remove(parameter.name());
        const_env.remove(&parameter_marker(parameter.name()));
        const_env.remove(&local_parameter_marker(parameter.name()));
    }
    const_env.insert(
        parameters::unbounded_parameter_marker(parameter.name()),
        i128::from(unbounded),
    );
    let resolved_type = parameter.resolved_type(parameter_types);
    let (resolved, resolved_literal) =
        parameter.resolved_value_and_literal(const_env, parameter_types, parameter_literals);
    let literal = if let Some(value) = resolved {
        const_env.insert(parameter.name().to_string(), value);
        Some(Expr::Literal(if let Some(ty) = resolved_type {
            format_typed_parameter_literal(value, ty.width, ty.signed)
        } else {
            value.to_string()
        }))
    } else {
        resolved_literal.or_else(|| {
            parameter_value_env(std::slice::from_ref(&parameter), const_env)
                .remove(parameter.name())
                .map(|value| substitute_expr_idents(value, parameter_literals))
        })
    };
    if let Some(ty) = resolved_type {
        insert_parameter_type_markers(const_env, parameter.name(), ty);
        parameter_types.insert(parameter.name().to_string(), ty);
    }
    if let Some(literal) = literal {
        parameter_literals.insert(parameter.name().to_string(), literal);
    }
}

pub(super) fn eval_ast_const_expr(
    expr: &ConstExpr,
    const_env: &HashMap<String, i128>,
) -> Option<i128> {
    // Look up only the identifiers in this expression, rather than scanning
    // every visible signal/parameter marker for each declaration bound.
    let expr = parameters::substitute_typed_parameter_literals_with_lookup(
        expr.clone(),
        const_env,
        &|name| parameter_type_from_const_env(const_env, name),
    );
    typecheck::eval_const_expr(&expr.into(), const_env)
}

pub(super) fn substitute_process_constants(
    process: CombProcess,
    const_env: &HashMap<String, i128>,
) -> CombProcess {
    substitute_process_constants_with_parameter_literals(process, const_env, &HashMap::default())
}

pub(super) fn substitute_process_constants_with_parameter_literals(
    process: CombProcess,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
) -> CombProcess {
    let condition = process
        .condition
        .map(|condition| substitute_const_expr_constants(condition, const_env));
    if process.assignments.is_empty() {
        let mut body = process.body;
        for stmt in &mut body {
            procedural::substitute_stmt_constants(stmt, const_env, parameter_literals);
        }
        return CombProcess::procedural(process.kind, condition, body);
    }
    CombProcess::new(
        process.kind,
        condition,
        process
            .assignments
            .into_iter()
            .map(|assignment| {
                substitute_assignment_constants_with_parameter_literals(
                    assignment,
                    const_env,
                    parameter_literals,
                )
            })
            .collect(),
    )
}

pub(super) fn substitute_assignment_constants_with_parameter_literals(
    assignment: Assignment,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
) -> Assignment {
    Assignment::new(
        substitute_lvalue_constants(assignment.lhs, const_env),
        substitute_expr_constants_with_parameter_literals(
            assignment.rhs,
            const_env,
            parameter_literals,
        ),
    )
}

pub(super) fn substitute_lvalue_constants(
    lvalue: LValue,
    const_env: &HashMap<String, i128>,
) -> LValue {
    match lvalue {
        LValue::Ident(name) => LValue::Ident(name),
        LValue::Select {
            name,
            msb,
            lsb,
            signed,
            array_slice_width,
            array_slice_reversed,
            is_2state,
        } => LValue::Select {
            name,
            msb: substitute_const_expr_constants(msb, const_env),
            lsb: substitute_const_expr_constants(lsb, const_env),
            signed,
            array_slice_width: array_slice_width
                .map(|width| substitute_const_expr_constants(width, const_env)),
            array_slice_reversed,
            is_2state,
        },
    }
}

pub(super) fn substitute_expr_constants_with_parameter_literals(
    expr: Expr,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
) -> Expr {
    match expr {
        Expr::Ident(name) => parameter_literals
            .get(&name)
            .cloned()
            .or_else(|| {
                let value = *const_env.get(&name)?;
                if const_env.contains_key(&enum_marker(&name)) {
                    let width = const_env
                        .get(&parameter_width_marker(&name))
                        .and_then(|width| usize::try_from(*width).ok())?;
                    let signed = const_env
                        .get(&parameter_signed_marker(&name))
                        .is_some_and(|signed| *signed != 0);
                    Some(Expr::Literal(format_typed_parameter_literal(
                        value, width, signed,
                    )))
                } else if !const_env.contains_key(&parameter_marker(&name)) {
                    Some(Expr::Literal(value.to_string()))
                } else {
                    None
                }
            })
            .unwrap_or(Expr::Ident(name)),
        Expr::Literal(value) => Expr::Literal(value),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(substitute_expr_constants_with_parameter_literals(
                *expr,
                const_env,
                parameter_literals,
            )),
            msb: substitute_const_expr_constants(msb, const_env),
            lsb: substitute_const_expr_constants(lsb, const_env),
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| {
                    substitute_expr_constants_with_parameter_literals(
                        part,
                        const_env,
                        parameter_literals,
                    )
                })
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count: substitute_const_expr_constants(count, const_env),
            parts: parts
                .into_iter()
                .map(|part| {
                    substitute_expr_constants_with_parameter_literals(
                        part,
                        const_env,
                        parameter_literals,
                    )
                })
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(substitute_expr_constants_with_parameter_literals(
                *expr,
                const_env,
                parameter_literals,
            )),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(substitute_expr_constants_with_parameter_literals(
                *expr,
                const_env,
                parameter_literals,
            )),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(substitute_expr_constants_with_parameter_literals(
                *left,
                const_env,
                parameter_literals,
            )),
            op,
            right: Box::new(substitute_expr_constants_with_parameter_literals(
                *right,
                const_env,
                parameter_literals,
            )),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(substitute_expr_constants_with_parameter_literals(
                *condition,
                const_env,
                parameter_literals,
            )),
            then_expr: Box::new(substitute_expr_constants_with_parameter_literals(
                *then_expr,
                const_env,
                parameter_literals,
            )),
            else_expr: Box::new(substitute_expr_constants_with_parameter_literals(
                *else_expr,
                const_env,
                parameter_literals,
            )),
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: Box::new(substitute_expr_constants_with_parameter_literals(
                *expr,
                const_env,
                parameter_literals,
            )),
            items: items
                .into_iter()
                .map(|item| {
                    item.map(&mut |operand| {
                        substitute_expr_constants_with_parameter_literals(
                            operand,
                            const_env,
                            parameter_literals,
                        )
                    })
                })
                .collect(),
        },
        Expr::Call { name, args } => {
            let args: Vec<_> = args
                .into_iter()
                .map(|arg| {
                    substitute_expr_constants_with_parameter_literals(
                        arg,
                        const_env,
                        parameter_literals,
                    )
                })
                .collect();
            if matches!(name.as_str(), "$signed" | "$unsigned")
                && let [arg] = args.as_slice()
                && let Some(constant) = expr_to_const(arg.clone())
                && let Some(ty) =
                    infer_const_expr_type(&constant, &parameter_types_from_const_env(const_env))
            {
                Expr::Resize {
                    expr: Box::new(arg.clone()),
                    width: ty.width,
                    signed: name == "$signed",
                }
            } else if name == "$countbits" {
                // Resolve constant calls before SLT lowering while retaining X/Z masks.
                fold_const_integral_expr_preserving_mask(Expr::Call { name, args }, const_env)
            } else {
                Expr::Call { name, args }
            }
        }
    }
}

pub(super) fn substitute_const_expr_constants(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
) -> ConstExpr {
    substitute_const_expr_constants_impl(expr, const_env, false, false)
}

pub(super) fn substitute_dimension_constants(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
) -> ConstExpr {
    substitute_const_expr_constants_impl(expr, const_env, true, false)
}

pub(super) fn substitute_const_expr_constants_preserving_enum_types(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
) -> ConstExpr {
    substitute_const_expr_constants_impl(expr, const_env, false, true)
}

fn substitute_const_expr_constants_impl(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
    include_local_parameters: bool,
    preserve_enum_types: bool,
) -> ConstExpr {
    match expr {
        ConstExpr::Ident(name) => {
            let Some(value) = const_env.get(&name).filter(|_| {
                !const_env.contains_key(&parameter_marker(&name))
                    || include_local_parameters
                        && const_env.contains_key(&local_parameter_marker(&name))
            }) else {
                return ConstExpr::Ident(name);
            };
            if preserve_enum_types && const_env.contains_key(&enum_marker(&name)) {
                let width = const_env
                    .get(&parameter_width_marker(&name))
                    .and_then(|width| usize::try_from(*width).ok());
                let signed = const_env
                    .get(&parameter_signed_marker(&name))
                    .is_some_and(|signed| *signed != 0);
                if let Some(width) = width {
                    return ConstExpr::Literal(format_typed_parameter_literal(
                        *value, width, signed,
                    ));
                }
            }
            ConstExpr::Literal(value.to_string())
        }
        ConstExpr::Literal(value) => ConstExpr::Literal(value),
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: Box::new(substitute_const_expr_constants_impl(
                *expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            bit: Box::new(substitute_const_expr_constants_impl(
                *bit,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
        },
        ConstExpr::Function { name, args, site } => ConstExpr::Function {
            name,
            site,
            args: args
                .into_iter()
                .map(|arg| {
                    substitute_const_expr_constants_impl(
                        arg,
                        const_env,
                        include_local_parameters,
                        preserve_enum_types,
                    )
                })
                .collect(),
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op,
            expr: Box::new(substitute_const_expr_constants_impl(
                *expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: Box::new(substitute_const_expr_constants_impl(
                *left,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            op,
            right: Box::new(substitute_const_expr_constants_impl(
                *right,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: Box::new(substitute_const_expr_constants_impl(
                *condition,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            then_expr: Box::new(substitute_const_expr_constants_impl(
                *then_expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            else_expr: Box::new(substitute_const_expr_constants_impl(
                *else_expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
        },
    }
}

/// A constant expression, or `None` for a form the typed expression path
/// converts instead. An error is a call that no path can convert.
pub(super) fn const_expr_from_expr(
    expr: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
) -> Converted<Option<ConstExpr>> {
    // Indexed selects need the typed path, including when nested in call arguments.
    if expr
        .into_iter()
        .any(|node| matches!(node, RefNode::IndexedRange(_)))
    {
        return Ok(None);
    }
    Ok(Some(match expr {
        sv_parser::Expression::Primary(primary) => {
            parsed!(const_expr_from_primary(primary, syntax_tree))
        }
        sv_parser::Expression::Unary(unary) => {
            let op = some!(unary_op_from_symbol(
                &unary.nodes.0.nodes.0.nodes.0,
                syntax_tree
            ));
            let expr = parsed!(const_expr_from_primary(&unary.nodes.2, syntax_tree));
            ConstExpr::Unary {
                op,
                expr: Box::new(expr),
            }
        }
        sv_parser::Expression::Binary(binary) => {
            let right_is_grouped = expression_is_grouped(&binary.nodes.3);
            let left = parsed!(const_expr_from_expr(&binary.nodes.0, syntax_tree));
            let op = some!(binary_op_from_symbol(
                &binary.nodes.1.nodes.0.nodes.0,
                syntax_tree
            ));
            let right = parsed!(const_expr_from_expr(&binary.nodes.3, syntax_tree));
            let expr = ConstExpr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
            if right_is_grouped {
                expr
            } else {
                left_associate_const_binary(expr)
            }
        }
        _ => return Ok(None),
    }))
}

fn const_expr_from_primary(
    primary: &sv_parser::Primary,
    syntax_tree: &SyntaxTree,
) -> Converted<Option<ConstExpr>> {
    match primary {
        sv_parser::Primary::Dollar(_) => Ok(Some(ConstExpr::Literal("$".into()))),
        sv_parser::Primary::PrimaryLiteral(_) => Ok(primary_literal_text(
            RefNode::Primary(primary),
            syntax_tree,
        )
        .map(ConstExpr::Literal)),
        sv_parser::Primary::Hierarchical(hierarchical) => {
            if packed_structs::has_member_access(
                RefNode::HierarchicalIdentifier(&hierarchical.nodes.1),
                RefNode::Select(&hierarchical.nodes.2),
            ) {
                return Ok(None);
            }
            let ident = some!(
                reference_name(RefNode::PrimaryHierarchical(hierarchical), syntax_tree)
                    .map(ConstExpr::Ident)
            );
            // A single bit-select is kept; other selections need the typed
            // expression path and must not be dropped here.
            let select = &hierarchical.nodes.2;
            if select.nodes.0.is_some() || select.nodes.2.is_some() {
                return Ok(None);
            }
            Ok(match select.nodes.1.nodes.0.as_slice() {
                [] => Some(ident),
                [bit] => Some(ConstExpr::Select {
                    expr: Box::new(ident),
                    bit: Box::new(parsed!(const_expr_from_expr(&bit.nodes.1, syntax_tree))),
                }),
                _ => None,
            })
        }
        sv_parser::Primary::FunctionSubroutineCall(call) => {
            const_expr_from_function_subroutine_call(call, syntax_tree, &HashMap::default())
        }
        sv_parser::Primary::MintypmaxExpression(expr) => match &expr.nodes.0.nodes.1 {
            sv_parser::MintypmaxExpression::Expression(expr) => {
                const_expr_from_expr(expr, syntax_tree)
            }
            sv_parser::MintypmaxExpression::Ternary(_) => Ok(None),
        },
        _ => Ok(expr_from_primary(primary, syntax_tree)
            .ok()
            .and_then(expr_to_const)),
    }
}

pub(super) fn left_associate_expr_binary(expr: Expr) -> Expr {
    let Expr::Binary { left, op, right } = expr else {
        return expr;
    };
    match *right {
        Expr::Binary {
            left: right_left,
            op: right_op,
            right: right_right,
        } if binary_precedence(op) >= binary_precedence(right_op) => {
            left_associate_expr_binary(Expr::Binary {
                left: Box::new(left_associate_expr_binary(Expr::Binary {
                    left,
                    op,
                    right: right_left,
                })),
                op: right_op,
                right: right_right,
            })
        }
        right => Expr::Binary {
            left,
            op,
            right: Box::new(right),
        },
    }
}

fn left_associate_const_binary(expr: ConstExpr) -> ConstExpr {
    let ConstExpr::Binary { left, op, right } = expr else {
        return expr;
    };
    match *right {
        ConstExpr::Binary {
            left: right_left,
            op: right_op,
            right: right_right,
        } if binary_precedence(op) >= binary_precedence(right_op) => {
            left_associate_const_binary(ConstExpr::Binary {
                left: Box::new(left_associate_const_binary(ConstExpr::Binary {
                    left,
                    op,
                    right: right_left,
                })),
                op: right_op,
                right: right_right,
            })
        }
        right => ConstExpr::Binary {
            left,
            op,
            right: Box::new(right),
        },
    }
}

fn binary_precedence(op: BinaryOp) -> u8 {
    match op {
        BinaryOp::Pow => 12,
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => 11,
        BinaryOp::Add | BinaryOp::Sub => 10,
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => 9,
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => 8,
        BinaryOp::Eq
        | BinaryOp::Ne
        | BinaryOp::EqCase
        | BinaryOp::NeCase
        | BinaryOp::EqWildcard
        | BinaryOp::NeWildcard => 7,
        BinaryOp::BitAnd => 6,
        BinaryOp::BitXor => 5,
        BinaryOp::BitOr => 4,
        BinaryOp::LogicAnd => 3,
        BinaryOp::LogicOr => 2,
    }
}

pub(super) fn expr_to_const(expr: Expr) -> Option<ConstExpr> {
    match expr {
        Expr::Ident(name) => Some(ConstExpr::Ident(name)),
        Expr::Literal(value) => Some(ConstExpr::Literal(value)),
        Expr::Unary { op, expr } => Some(ConstExpr::Unary {
            op,
            expr: Box::new(expr_to_const(*expr)?),
        }),
        Expr::Binary { left, op, right } => Some(ConstExpr::Binary {
            left: Box::new(expr_to_const(*left)?),
            op,
            right: Box::new(expr_to_const(*right)?),
        }),
        Expr::Select { expr, msb, lsb, .. } if msb == lsb => Some(ConstExpr::Select {
            expr: Box::new(expr_to_const(*expr)?),
            bit: Box::new(msb),
        }),
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Some(ConstExpr::Mux {
            condition: Box::new(expr_to_const(*condition)?),
            then_expr: Box::new(expr_to_const(*then_expr)?),
            else_expr: Box::new(expr_to_const(*else_expr)?),
        }),
        Expr::Call { name, args } => Some(ConstExpr::call(
            name,
            args.into_iter().map(expr_to_const).collect::<Option<_>>()?,
        )),
        Expr::Select { .. }
        | Expr::Concat(_)
        | Expr::RepeatConcat { .. }
        | Expr::Resize { .. }
        | Expr::Inside { .. } => None,
    }
}

pub(super) fn expr_to_index_const(
    expr: Expr,
    const_env: &HashMap<String, i128>,
) -> Option<ConstExpr> {
    match expr {
        Expr::Resize {
            expr,
            width,
            signed,
        } => {
            let expr = expr_to_index_const(*expr, const_env)?;
            if width == 0 {
                return Some(ConstExpr::Literal("0".to_string()));
            }
            if width == 1 && !signed {
                return Some(ConstExpr::Select {
                    expr: Box::new(expr),
                    bit: Box::new(ConstExpr::Literal("0".to_string())),
                });
            }
            let mask = (num_bigint::BigUint::from(1u8) << width) - num_bigint::BigUint::from(1u8);
            let truncated = ConstExpr::Binary {
                left: Box::new(expr),
                op: BinaryOp::BitAnd,
                right: Box::new(ConstExpr::Literal(format!("{width}'h{mask:x}"))),
            };
            if signed {
                Some(ConstExpr::Mux {
                    condition: Box::new(ConstExpr::Select {
                        expr: Box::new(truncated.clone()),
                        bit: Box::new(ConstExpr::Literal((width - 1).to_string())),
                    }),
                    // Every negative packed index is out of range. A stable
                    // positive sentinel preserves that selection behavior
                    // without requiring a signed-resize node in ConstExpr.
                    then_expr: Box::new(ConstExpr::Literal(i128::MAX.to_string())),
                    else_expr: Box::new(truncated),
                })
            } else {
                Some(truncated)
            }
        }
        Expr::Ident(name) => Some(ConstExpr::Ident(name)),
        Expr::Literal(value) => Some(ConstExpr::Literal(value)),
        Expr::Unary { op, expr } => Some(ConstExpr::Unary {
            op,
            expr: Box::new(expr_to_index_const(*expr, const_env)?),
        }),
        Expr::Binary { left, op, right } => Some(ConstExpr::Binary {
            left: Box::new(expr_to_index_const(*left, const_env)?),
            op,
            right: Box::new(expr_to_index_const(*right, const_env)?),
        }),
        Expr::Select { expr, msb, lsb, .. } if msb == lsb => Some(ConstExpr::Select {
            expr: Box::new(expr_to_index_const(*expr, const_env)?),
            bit: Box::new(msb),
        }),
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Some(ConstExpr::Mux {
            condition: Box::new(expr_to_index_const(*condition, const_env)?),
            then_expr: Box::new(expr_to_index_const(*then_expr, const_env)?),
            else_expr: Box::new(expr_to_index_const(*else_expr, const_env)?),
        }),
        Expr::Call { name, args } => Some(ConstExpr::call(
            name,
            args.into_iter()
                .map(|arg| expr_to_index_const(arg, const_env))
                .collect::<Option<_>>()?,
        )),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed: false,
        } => {
            let msb = eval_ast_const_expr(&msb, const_env)?;
            let lsb = eval_ast_const_expr(&lsb, const_env)?;
            let width = usize::try_from(msb.abs_diff(lsb).checked_add(1)?).ok()?;
            if width > constant_folding::MAX_CONSTANT_CONCAT_BITS {
                return None;
            }
            let expr = expr_to_index_const(*expr, const_env)?;
            // Symbolic indices only have single-bit selects. Assemble a constant
            // part-select from those nodes, keeping its own unsigned width rather
            // than the source vector's width (IEEE 1800-2023 11.5.1, 11.8.1).
            // Select bits individually so X/Z outside the slice cannot taint it.
            let mut bits = (0..width)
                .map(|offset| {
                    let bit = if msb >= lsb {
                        lsb + offset as i128
                    } else {
                        lsb - offset as i128
                    };
                    ConstExpr::Binary {
                        left: Box::new(ConstExpr::Select {
                            expr: Box::new(expr.clone()),
                            bit: Box::new(const_expr_from_i128(bit)),
                        }),
                        op: BinaryOp::Shl,
                        right: Box::new(ConstExpr::Literal(offset.to_string())),
                    }
                })
                .collect::<Vec<_>>();
            // A balanced tree also bounds recursion for wider slices.
            while bits.len() > 1 {
                let mut remaining = bits.into_iter();
                bits = std::iter::from_fn(|| {
                    let left = remaining.next()?;
                    Some(match remaining.next() {
                        Some(right) => ConstExpr::Binary {
                            left: Box::new(left),
                            op: BinaryOp::BitOr,
                            right: Box::new(right),
                        },
                        None => left,
                    })
                })
                .collect();
            }
            Some(ConstExpr::Binary {
                left: Box::new(ConstExpr::Literal(format!("{width}'b0"))),
                op: BinaryOp::BitOr,
                right: Box::new(bits.pop()?),
            })
        }
        Expr::Select { .. } | Expr::Concat(_) | Expr::RepeatConcat { .. } | Expr::Inside { .. } => {
            None
        }
    }
}

pub(super) fn const_expr_from_constant_param_with_env(
    expr: &sv_parser::ConstantParamExpression,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Converted<Option<ConstExpr>> {
    match expr {
        sv_parser::ConstantParamExpression::ConstantMintypmaxExpression(expr) => match &**expr {
            sv_parser::ConstantMintypmaxExpression::Unary(expr) => {
                const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(expr),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )
            }
            sv_parser::ConstantMintypmaxExpression::Ternary(_) => Ok(None),
        },
        sv_parser::ConstantParamExpression::Dollar(_) => Ok(Some(ConstExpr::Literal("$".into()))),
        _ => Ok(None),
    }
}

pub(super) fn const_expr_from_param_expression(
    expr: &sv_parser::ParamExpression,
    syntax_tree: &SyntaxTree,
) -> Converted<Option<ConstExpr>> {
    match expr {
        sv_parser::ParamExpression::MintypmaxExpression(expr) => match &**expr {
            sv_parser::MintypmaxExpression::Expression(expr) => {
                const_expr_from_expr(expr.as_ref(), syntax_tree)
            }
            sv_parser::MintypmaxExpression::Ternary(_) => Ok(None),
        },
        sv_parser::ParamExpression::Dollar(_) => Ok(Some(ConstExpr::Literal("$".into()))),
        sv_parser::ParamExpression::DataType(_) => Ok(None),
    }
}

pub(super) fn const_expr_from_ref_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
) -> Converted<Option<ConstExpr>> {
    const_expr_from_ref_node_with_env(node, syntax_tree, &HashMap::default(), &HashMap::default())
}

pub(super) fn const_expr_from_ref_node_with_env(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Converted<Option<ConstExpr>> {
    // Indexed selections require declared dimensions and typed selection
    // lowering. Never let this lightweight parser replace them by the base.
    if node
        .clone()
        .into_iter()
        .any(|child| matches!(child, RefNode::ConstantIndexedRange(_)))
    {
        return Ok(None);
    }
    let convert =
        |node| const_expr_from_ref_node_with_env(node, syntax_tree, const_env, type_aliases);
    match node {
        RefNode::ConstantExpression(expr) => match expr {
            sv_parser::ConstantExpression::ConstantPrimary(primary) => {
                convert(RefNode::ConstantPrimary(primary))
            }
            sv_parser::ConstantExpression::Unary(unary) => {
                let op = some!(unary_op_from_symbol(
                    &unary.nodes.0.nodes.0.nodes.0,
                    syntax_tree
                ));
                let expr = parsed!(convert(RefNode::ConstantPrimary(&unary.nodes.2)));
                Ok(Some(ConstExpr::Unary {
                    op,
                    expr: Box::new(expr),
                }))
            }
            sv_parser::ConstantExpression::Binary(binary) => {
                let right_is_grouped = constant_expression_is_grouped(&binary.nodes.3);
                let left = parsed!(convert(RefNode::ConstantExpression(&binary.nodes.0)));
                let op = some!(binary_op_from_symbol(
                    &binary.nodes.1.nodes.0.nodes.0,
                    syntax_tree
                ));
                let right = parsed!(convert(RefNode::ConstantExpression(&binary.nodes.3)));
                let expr = ConstExpr::Binary {
                    left: Box::new(left),
                    op,
                    right: Box::new(right),
                };
                Ok(Some(if right_is_grouped {
                    expr
                } else {
                    left_associate_const_binary(expr)
                }))
            }
            sv_parser::ConstantExpression::Ternary(expr) => {
                const_expr_from_constant_expression_ternary_with_env(
                    expr,
                    syntax_tree,
                    const_env,
                    type_aliases,
                )
            }
            sv_parser::ConstantExpression::Inside(_) => Ok(None),
        },
        RefNode::ConstantPrimary(primary) => match primary {
            sv_parser::ConstantPrimary::Dollar(_) => Ok(Some(ConstExpr::Literal("$".into()))),
            sv_parser::ConstantPrimary::PrimaryLiteral(_) => {
                Ok(primary_literal_text(node, syntax_tree).map(ConstExpr::Literal))
            }
            sv_parser::ConstantPrimary::PsParameter(parameter) => {
                if parameter.nodes.1.nodes.0.is_some() {
                    // Struct-valued parameters need typed member evaluation;
                    // never replace a member by the entire parameter value.
                    return Ok(None);
                }
                let base = some!(
                    reference_name(
                        RefNode::PsParameterIdentifier(&parameter.nodes.0),
                        syntax_tree
                    )
                    .map(ConstExpr::Ident)
                );
                Ok(Some(
                    const_select_expr(
                        base.clone(),
                        &parameter.nodes.1,
                        syntax_tree,
                        const_env,
                        type_aliases,
                    )?
                    .unwrap_or(base),
                ))
            }
            sv_parser::ConstantPrimary::ConstantFunctionCall(call) => {
                if let sv_parser::SubroutineCall::SystemTfCall(system_call) = &call.nodes.0.nodes.0
                {
                    let (name, args) = some!(system_tf_call_parts(system_call, syntax_tree));
                    system_functions::check_call(
                        name,
                        args.as_deref(),
                        system_functions::CallSite::Expression,
                    )?;
                    parameters::reject_unbounded_data_query(
                        system_call,
                        syntax_tree,
                        const_env,
                        false,
                    )?;
                    if name == "$isunbounded" {
                        // Imported aliases may not be bound in preliminary
                        // collection. Retain the query for later resolution,
                        // including its one-bit type for dependent queries.
                        return const_expr_from_function_subroutine_call(
                            &call.nodes.0,
                            syntax_tree,
                            const_env,
                        );
                    }
                    if name == "$dimensions"
                        && let Some(count) = dimensions::dimensions_system_function_call_value(
                            system_call,
                            syntax_tree,
                            const_env,
                            type_aliases,
                            None,
                        )
                    {
                        return Ok(Some(ConstExpr::Literal(count.to_string())));
                    }
                    if matches!(
                        name,
                        "$left"
                            | "$right"
                            | "$low"
                            | "$high"
                            | "$increment"
                            | "$unpacked_dimensions"
                    ) {
                        return Ok(dimensions::array_query_call(
                            system_call,
                            syntax_tree,
                            const_env,
                            type_aliases,
                            None,
                        )
                        .and_then(expr_to_const));
                    }
                }
                if let sv_parser::SubroutineCall::TfCall(tf_call) = &call.nodes.0.nodes.0
                    && tf_call.nodes.2.is_some()
                {
                    // Plain scalar operands do not read declaration metadata
                    // while lowering. Their types are substituted later during
                    // parameter evaluation; avoid copying the whole environment
                    // for every initializer in a growing declaration prefix.
                    let dimensions = if call_arguments_are_context_free(tf_call) {
                        PackedDimensions::default()
                    } else {
                        #[cfg(test)]
                        CALL_CONTEXT_COPIES.with(|count| count.set(count.get() + 1));
                        PackedDimensions::new(HashMap::default(), const_env, type_aliases)
                    };
                    return Ok(expr_from_function_subroutine_call(
                        &call.nodes.0,
                        syntax_tree,
                        &dimensions,
                    )
                    .ok()
                    .and_then(expr_to_const));
                }
                if let Some(ty) =
                    size_system_function_expr_type(primary, syntax_tree, const_env, type_aliases)
                {
                    return Ok(Some(ConstExpr::Literal(ty.width.to_string())));
                }
                let lowered = const_expr_from_function_subroutine_call(
                    &call.nodes.0,
                    syntax_tree,
                    const_env,
                )?;
                if let Some(ConstExpr::Function { name, args, .. }) = &lowered
                    && name == "$bits"
                    && let [arg] = args.as_slice()
                    && let Some(r#type) =
                        infer_const_expr_type(arg, &parameter_types_from_const_env(const_env))
                {
                    return Ok(Some(ConstExpr::Literal(r#type.width.to_string())));
                }
                Ok(lowered.or_else(|| {
                    let sv_parser::SubroutineCall::TfCall(tf_call) = &call.nodes.0.nodes.0 else {
                        return None;
                    };
                    if tf_call.nodes.2.is_some() {
                        return None;
                    }
                    reference_name(
                        RefNode::PsOrHierarchicalTfIdentifier(&tf_call.nodes.0),
                        syntax_tree,
                    )
                    .map(ConstExpr::Ident)
                }))
            }
            sv_parser::ConstantPrimary::ConstantCast(cast) => Ok(constant_cast_const_expr(
                cast,
                syntax_tree,
                const_env,
                type_aliases,
            )),
            sv_parser::ConstantPrimary::MintypmaxExpression(expr) => match &expr.nodes.0.nodes.1 {
                sv_parser::ConstantMintypmaxExpression::Unary(expr) => {
                    convert(RefNode::ConstantExpression(expr))
                }
                sv_parser::ConstantMintypmaxExpression::Ternary(_) => Ok(None),
            },
            _ => Ok(None),
        },
        _ => {
            if let Some(integral_number) = unwrap_node!(node.clone(), IntegralNumber) {
                return Ok(
                    integral_number_literal(integral_number, syntax_tree).map(ConstExpr::Literal)
                );
            }
            if let Some(identifier) = unwrap_node!(node, SimpleIdentifier, EscapedIdentifier) {
                return Ok(identifier_locate(identifier)
                    .and_then(|locate| syntax_tree.get_str(&locate).map(str::to_string))
                    .map(ConstExpr::Ident));
            }
            Ok(None)
        }
    }
}

fn constant_expression_is_grouped(expr: &sv_parser::ConstantExpression) -> bool {
    matches!(
        expr,
        sv_parser::ConstantExpression::ConstantPrimary(primary)
            if matches!(
                &**primary,
                sv_parser::ConstantPrimary::MintypmaxExpression(_)
            )
    )
}

fn const_expr_from_constant_expression_ternary_with_env(
    expr: &sv_parser::ConstantExpressionTernary,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Converted<Option<ConstExpr>> {
    let convert = |expr| {
        const_expr_from_ref_node_with_env(
            RefNode::ConstantExpression(expr),
            syntax_tree,
            const_env,
            type_aliases,
        )
    };
    Ok(Some(ConstExpr::Mux {
        condition: Box::new(parsed!(convert(&expr.nodes.0))),
        then_expr: Box::new(parsed!(convert(&expr.nodes.3))),
        else_expr: Box::new(parsed!(convert(&expr.nodes.5))),
    }))
}

/// These operands follow only expression-lowering branches that do not read
/// `PackedDimensions`. Keep selections, casts, system calls, and patterns on
/// the contextual path so their ranges, aliases, and parameter types survive.
fn call_arguments_are_context_free(call: &sv_parser::TfCall) -> bool {
    let Some(paren) = &call.nodes.2 else {
        return false;
    };
    let sv_parser::ListOfArguments::Ordered(args) = &paren.nodes.1 else {
        return false;
    };
    let args = args.nodes.0.contents();
    // The parser represents `f()` as one omitted argument.
    if args.len() == 1 && args[0].is_none() {
        return true;
    }
    args.iter()
        .all(|arg| arg.as_ref().is_some_and(context_free_expression))
}

fn context_free_expression(expr: &sv_parser::Expression) -> bool {
    match expr {
        sv_parser::Expression::Primary(primary) => context_free_primary(primary),
        sv_parser::Expression::Unary(unary) => context_free_primary(&unary.nodes.2),
        sv_parser::Expression::Binary(binary) => {
            context_free_expression(&binary.nodes.0) && context_free_expression(&binary.nodes.3)
        }
        _ => false,
    }
}

fn context_free_primary(primary: &sv_parser::Primary) -> bool {
    match primary {
        sv_parser::Primary::PrimaryLiteral(_) => true,
        sv_parser::Primary::Hierarchical(primary) => {
            let select = &primary.nodes.2;
            select.nodes.0.is_none()
                && select.nodes.1.nodes.0.is_empty()
                && select.nodes.2.is_none()
                && !packed_structs::has_member_access(
                    RefNode::HierarchicalIdentifier(&primary.nodes.1),
                    RefNode::Select(select),
                )
        }
        sv_parser::Primary::MintypmaxExpression(primary) => match &primary.nodes.0.nodes.1 {
            sv_parser::MintypmaxExpression::Expression(expr) => context_free_expression(expr),
            sv_parser::MintypmaxExpression::Ternary(_) => false,
        },
        sv_parser::Primary::FunctionSubroutineCall(call) => match &call.nodes.0 {
            sv_parser::SubroutineCall::TfCall(call) => call_arguments_are_context_free(call),
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
thread_local! {
    static CALL_CONTEXT_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;

fn const_select_expr(
    base: ConstExpr,
    select: &sv_parser::ConstantSelect,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Converted<Option<ConstExpr>> {
    let bit_selects = select.nodes.1.nodes.0.as_slice();
    if select.nodes.2.is_some() {
        return Ok(None);
    }
    // An element of a packed parameter takes its width and signedness from
    // the parameter's dimensions.
    if let ConstExpr::Ident(name) = &base {
        let mut indices = Vec::with_capacity(bit_selects.len());
        for bit_select in bit_selects {
            let index = const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(&bit_select.nodes.1),
                syntax_tree,
                const_env,
                type_aliases,
            )?;
            indices.push(index.and_then(|index| eval_ast_const_expr(&index, const_env)));
        }
        if let Some(indices) = indices.into_iter().collect::<Option<Vec<_>>>()
            && let Some(literal) = parameter_element_literal(name, &indices, const_env)
        {
            return Ok(Some(ConstExpr::Literal(literal)));
        }
    }
    if bit_selects.len() != 1 {
        return Ok(None);
    }
    let bit = parsed!(const_expr_from_ref_node_with_env(
        RefNode::ConstantExpression(&bit_selects[0].nodes.1),
        syntax_tree,
        const_env,
        type_aliases,
    ));
    Ok(Some(ConstExpr::Select {
        expr: Box::new(base),
        bit: Box::new(bit),
    }))
}

/// A system function call in a constant expression. The call is checked
/// against the system function catalog: no path converts a call it rejects.
fn const_expr_from_function_subroutine_call(
    call: &sv_parser::FunctionSubroutineCall,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Converted<Option<ConstExpr>> {
    let system_call = match &call.nodes.0 {
        sv_parser::SubroutineCall::SystemTfCall(system_call) => system_call,
        // A user function call, such as in a run-time select index; the
        // procedural lowering evaluates it before the operation. Without
        // parentheses the name parses as a call but is an identifier.
        sv_parser::SubroutineCall::TfCall(tf_call) if tf_call.nodes.2.is_some() => {
            return Ok(
                expr_from_tf_call(tf_call, syntax_tree, &PackedDimensions::default())
                    .ok()
                    .and_then(expr_to_const),
            );
        }
        // `$unit::x` names an item of the compilation unit.
        sv_parser::SubroutineCall::TfCall(tf_call)
            if RefNode::PsOrHierarchicalTfIdentifier(&tf_call.nodes.0)
                .into_iter()
                .any(|node| {
                    matches!(
                        node,
                        RefNode::PackageScope(sv_parser::PackageScope::Unit(_))
                    )
                }) =>
        {
            return Ok(reference_name(
                RefNode::PsOrHierarchicalTfIdentifier(&tf_call.nodes.0),
                syntax_tree,
            )
            .map(ConstExpr::Ident));
        }
        _ => return Ok(None),
    };
    let (name, args) = some!(system_tf_call_parts(system_call, syntax_tree));
    system_functions::check_call(
        name,
        args.as_deref(),
        system_functions::CallSite::Expression,
    )?;
    if name == "$isunbounded" {
        if let Some(value) = parameters::isunbounded_call(system_call, syntax_tree, const_env)? {
            return Ok(Some(value));
        }
        let argument = expressions::single_expression_argument(system_call).unwrap();
        return Ok(Some(ConstExpr::call(
            name.into(),
            vec![const_expr_from_expr(argument, syntax_tree)?.unwrap()],
        )));
    }
    let sv_parser::SystemTfCall::ArgExpression(expression_call) = &**system_call else {
        return Ok(None);
    };
    if matches!(
        name,
        "$countbits" | "$countones" | "$onehot" | "$onehot0" | "$isunknown"
    ) {
        // Use expression lowering so selections are never silently discarded
        // by the limited constant-primary identifier path below. Unsupported
        // constant argument forms must remain unresolved rather than counting
        // the entire identifier in place of its selection.
        let expression = expr_from_function_subroutine_call(
            call,
            syntax_tree,
            &PackedDimensions::new(HashMap::default(), const_env, &HashMap::default()),
        )
        .ok();
        return Ok(expression.and_then(|expression| {
            if name == "$countbits" {
                countbits_constant_call(expression, const_env)
            } else {
                expr_to_const(expression)
            }
        }));
    }
    let mut lowered = Vec::new();
    for argument in expression_call.nodes.1.nodes.1.0.contents() {
        // The catalog check rejects omitted arguments of functions.
        let argument = some!(argument.as_ref());
        lowered.push(parsed!(const_expr_from_expr(argument, syntax_tree)));
    }
    Ok(Some(ConstExpr::call(name.to_string(), lowered)))
}

// ConstExpr represents bit selects but not concatenations or part selects.
// Counting each stream segment separately keeps parameter dependencies symbolic
// and preserves the selected width, including when zero bits are counted.
fn countbits_constant_call(
    expression: Expr,
    const_env: &HashMap<String, i128>,
) -> Option<ConstExpr> {
    let Expr::Call { name, mut args } = expression else {
        return None;
    };
    let operand = args.remove(0);
    let controls = args
        .into_iter()
        .map(|control| match control {
            Expr::Select { expr, lsb, .. } => Some(ConstExpr::Select {
                expr: Box::new(expr_to_const(*expr)?),
                bit: Box::new(lsb),
            }),
            control => expr_to_const(control),
        })
        .collect::<Option<Vec<_>>>()?;
    countbits_constant_operand(operand, &name, &controls, const_env)
}

fn countbits_constant_operand(
    operand: Expr,
    name: &str,
    controls: &[ConstExpr],
    const_env: &HashMap<String, i128>,
) -> Option<ConstExpr> {
    let call = |operand| {
        let mut args = Vec::with_capacity(controls.len() + 1);
        args.push(operand);
        args.extend_from_slice(controls);
        ConstExpr::call(name.to_string(), args)
    };
    let sum = |mut terms: Vec<ConstExpr>| {
        // Keep large selected vectors from creating a deeply nested sum.
        while terms.len() > 1 {
            let mut next = Vec::with_capacity(terms.len().div_ceil(2));
            let mut terms_iter = terms.into_iter();
            while let Some(left) = terms_iter.next() {
                next.push(match terms_iter.next() {
                    Some(right) => ConstExpr::Binary {
                        left: Box::new(left),
                        op: BinaryOp::Add,
                        right: Box::new(right),
                    },
                    None => left,
                });
            }
            terms = next;
        }
        terms.pop()
    };
    match operand {
        Expr::Select { expr, msb, lsb, .. } if msb != lsb => {
            let msb = usize::try_from(eval_ast_const_expr(&msb, const_env)?).ok()?;
            let lsb = usize::try_from(eval_ast_const_expr(&lsb, const_env)?).ok()?;
            let width = msb.checked_sub(lsb)?.checked_add(1)?;
            if width > constant_folding::MAX_CONSTANT_CONCAT_BITS {
                return None;
            }
            let operand = expr_to_const(*expr)?;
            sum((lsb..=msb)
                .map(|bit| {
                    call(ConstExpr::Select {
                        expr: Box::new(operand.clone()),
                        bit: Box::new(ConstExpr::Literal(bit.to_string())),
                    })
                })
                .collect())
        }
        Expr::Concat(parts) => sum(parts
            .into_iter()
            .map(|part| countbits_constant_operand(part, name, controls, const_env))
            .collect::<Option<Vec<_>>>()?),
        Expr::RepeatConcat { count, parts } => {
            let count = eval_ast_const_expr(&count, const_env)?;
            if count < 0 {
                return None;
            }
            let counted =
                countbits_constant_operand(Expr::Concat(parts), name, controls, const_env)?;
            Some(ConstExpr::Binary {
                left: Box::new(counted),
                op: BinaryOp::Mul,
                right: Box::new(ConstExpr::Literal(format!("32'sd{count}"))),
            })
        }
        operand => Some(call(expr_to_const(operand)?)),
    }
}

fn integral_number_literal(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Option<String> {
    let RefNode::IntegralNumber(number) = node else {
        return None;
    };
    match number {
        sv_parser::IntegralNumber::DecimalNumber(decimal) => match &**decimal {
            sv_parser::DecimalNumber::UnsignedNumber(number) => {
                locate_text(&number.nodes.0, syntax_tree)
            }
            sv_parser::DecimalNumber::BaseUnsigned(number) => based_literal(
                number.nodes.0.as_ref().map(|size| &size.nodes.0.nodes.0),
                &number.nodes.1.nodes.0,
                &number.nodes.2.nodes.0,
                syntax_tree,
            ),
            sv_parser::DecimalNumber::BaseXNumber(number) => based_literal(
                number.nodes.0.as_ref().map(|size| &size.nodes.0.nodes.0),
                &number.nodes.1.nodes.0,
                &number.nodes.2.nodes.0,
                syntax_tree,
            ),
            sv_parser::DecimalNumber::BaseZNumber(number) => based_literal(
                number.nodes.0.as_ref().map(|size| &size.nodes.0.nodes.0),
                &number.nodes.1.nodes.0,
                &number.nodes.2.nodes.0,
                syntax_tree,
            ),
        },
        sv_parser::IntegralNumber::BinaryNumber(number) => based_literal(
            number.nodes.0.as_ref().map(|size| &size.nodes.0.nodes.0),
            &number.nodes.1.nodes.0,
            &number.nodes.2.nodes.0,
            syntax_tree,
        ),
        sv_parser::IntegralNumber::OctalNumber(number) => based_literal(
            number.nodes.0.as_ref().map(|size| &size.nodes.0.nodes.0),
            &number.nodes.1.nodes.0,
            &number.nodes.2.nodes.0,
            syntax_tree,
        ),
        sv_parser::IntegralNumber::HexNumber(number) => based_literal(
            number.nodes.0.as_ref().map(|size| &size.nodes.0.nodes.0),
            &number.nodes.1.nodes.0,
            &number.nodes.2.nodes.0,
            syntax_tree,
        ),
    }
}

fn based_literal(
    size: Option<&Locate>,
    base: &Locate,
    digits: &Locate,
    syntax_tree: &SyntaxTree,
) -> Option<String> {
    let size = size
        .and_then(|size| locate_text(size, syntax_tree))
        .unwrap_or_default();
    let base = locate_text(base, syntax_tree)?;
    let digits = locate_text(digits, syntax_tree)?;
    Some(format!("{size}{base}{digits}"))
}

fn locate_text(locate: &Locate, syntax_tree: &SyntaxTree) -> Option<String> {
    syntax_tree.get_str(locate).map(str::to_string)
}

pub(super) fn primary_literal_text(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Option<String> {
    if let Some(integral_number) = unwrap_node!(node.clone(), IntegralNumber) {
        return integral_number_literal(integral_number, syntax_tree);
    }
    let unbased = unwrap_node!(node, UnbasedUnsizedLiteral)?;
    let RefNode::UnbasedUnsizedLiteral(unbased) = unbased else {
        return None;
    };
    syntax_tree.get_str(&unbased.nodes.0).map(str::to_string)
}

fn unary_op_from_symbol(symbol: &Locate, syntax_tree: &SyntaxTree) -> Option<UnaryOp> {
    match syntax_tree.get_str(symbol)? {
        "+" => Some(UnaryOp::Plus),
        "-" => Some(UnaryOp::Minus),
        "~" => Some(UnaryOp::BitNot),
        "!" => Some(UnaryOp::LogicNot),
        "&" => Some(UnaryOp::RedAnd),
        "|" => Some(UnaryOp::RedOr),
        "^" => Some(UnaryOp::RedXor),
        _ => None,
    }
}

pub(super) fn unary_expr_from_symbol(
    symbol: &Locate,
    expr: Expr,
    syntax_tree: &SyntaxTree,
) -> Option<Expr> {
    let reduction = match syntax_tree.get_str(symbol)? {
        "~&" => Some(UnaryOp::RedAnd),
        "~|" => Some(UnaryOp::RedOr),
        "~^" | "^~" => Some(UnaryOp::RedXor),
        _ => None,
    };
    if let Some(op) = reduction {
        return Some(Expr::Unary {
            op: UnaryOp::BitNot,
            expr: Box::new(Expr::Unary {
                op,
                expr: Box::new(expr),
            }),
        });
    }
    Some(Expr::Unary {
        op: unary_op_from_symbol(symbol, syntax_tree)?,
        expr: Box::new(expr),
    })
}

pub(super) fn binary_op_from_symbol(symbol: &Locate, syntax_tree: &SyntaxTree) -> Option<BinaryOp> {
    match syntax_tree.get_str(symbol)? {
        "+" => Some(BinaryOp::Add),
        "-" => Some(BinaryOp::Sub),
        "*" => Some(BinaryOp::Mul),
        "/" => Some(BinaryOp::Div),
        "%" => Some(BinaryOp::Mod),
        "**" => Some(BinaryOp::Pow),
        "<<" => Some(BinaryOp::Shl),
        "<<<" => Some(BinaryOp::Shl),
        ">>" => Some(BinaryOp::Shr),
        ">>>" => Some(BinaryOp::Sar),
        "&" => Some(BinaryOp::BitAnd),
        "|" => Some(BinaryOp::BitOr),
        "^" => Some(BinaryOp::BitXor),
        "&&" => Some(BinaryOp::LogicAnd),
        "||" => Some(BinaryOp::LogicOr),
        "==" => Some(BinaryOp::Eq),
        "!=" => Some(BinaryOp::Ne),
        "===" => Some(BinaryOp::EqCase),
        "!==" => Some(BinaryOp::NeCase),
        "==?" => Some(BinaryOp::EqWildcard),
        "!=?" => Some(BinaryOp::NeWildcard),
        "<" => Some(BinaryOp::Lt),
        "<=" => Some(BinaryOp::Le),
        ">" => Some(BinaryOp::Gt),
        ">=" => Some(BinaryOp::Ge),
        _ => None,
    }
}
