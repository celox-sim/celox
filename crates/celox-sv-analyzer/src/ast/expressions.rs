//! Runtime expression and primary syntax conversion.

use super::*;

pub(super) fn expr_from_expression(
    expr: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
) -> Option<Expr> {
    expr_from_expression_with_types(expr, syntax_tree, &PackedDimensions::default())
}

pub(super) fn expr_from_expression_with_types(
    expr: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    expr_from_expression_with_types_raw(expr, syntax_tree, packed_dimensions)
        .map(guard_zero_divisions)
}

fn expr_from_expression_with_types_raw(
    expr: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    match expr {
        sv_parser::Expression::Primary(primary) => {
            expr_from_primary_with_types(primary, syntax_tree, packed_dimensions)
        }
        sv_parser::Expression::Unary(unary) => {
            let expr =
                expr_from_primary_with_types(&unary.nodes.2, syntax_tree, packed_dimensions)?;
            unary_expr_from_symbol(&unary.nodes.0.nodes.0.nodes.0, expr, syntax_tree)
        }
        sv_parser::Expression::Binary(binary) => {
            let right_is_grouped = expression_is_grouped(&binary.nodes.3);
            let left = expr_from_expression_with_types_raw(
                &binary.nodes.0,
                syntax_tree,
                packed_dimensions,
            )?;
            let op = binary_op_from_symbol(&binary.nodes.1.nodes.0.nodes.0, syntax_tree)?;
            let right = expr_from_expression_with_types_raw(
                &binary.nodes.3,
                syntax_tree,
                packed_dimensions,
            )?;
            let expr = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
            Some(if right_is_grouped {
                expr
            } else {
                left_associate_expr_binary(expr)
            })
        }
        sv_parser::Expression::ConditionalExpression(expr) => {
            expr_from_conditional_expression(expr, syntax_tree, packed_dimensions)
        }
        sv_parser::Expression::InsideExpression(inside) => {
            expr_from_inside_expression(inside, syntax_tree, packed_dimensions)
        }
        _ => None,
    }
}

/// `x inside {a, [lo:hi], ...}`: kept as an `Inside` expression, so that its
/// operands and its matching rules stay visible to later stages.
fn expr_from_inside_expression(
    inside: &sv_parser::InsideExpression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    let operand =
        expr_from_expression_with_types_raw(&inside.nodes.0, syntax_tree, packed_dimensions)?;
    let items = inside
        .nodes
        .2
        .nodes
        .1
        .nodes
        .0
        .contents()
        .into_iter()
        .map(|item| match &item.nodes.0 {
            sv_parser::ValueRange::Expression(value) => {
                expr_from_expression_with_types_raw(value, syntax_tree, packed_dimensions)
                    .map(InsideItem::Value)
            }
            sv_parser::ValueRange::Binary(range) => {
                let (low, _, high) = &range.nodes.0.nodes.1;
                Some(InsideItem::Range {
                    low: expr_from_expression_with_types_raw(low, syntax_tree, packed_dimensions)?,
                    high: expr_from_expression_with_types_raw(
                        high,
                        syntax_tree,
                        packed_dimensions,
                    )?,
                })
            }
        })
        .collect::<Option<Vec<_>>>()?;
    (!items.is_empty()).then(|| Expr::Inside {
        expr: Box::new(operand),
        items,
    })
}

pub(super) fn guard_zero_divisions(expr: Expr) -> Expr {
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => expr,
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(guard_zero_divisions(*expr)),
            msb,
            lsb,
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(parts.into_iter().map(guard_zero_divisions).collect()),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts.into_iter().map(guard_zero_divisions).collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(guard_zero_divisions(*expr)),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(guard_zero_divisions(*expr)),
        },
        Expr::Binary { left, op, right } => {
            let left = Box::new(guard_zero_divisions(*left));
            let right = Box::new(guard_zero_divisions(*right));
            let operation = Expr::Binary {
                left,
                op,
                right: right.clone(),
            };
            let divisor_is_nonzero = expr_to_const((*right).clone())
                .and_then(|right| eval_ast_const_expr(&right, &HashMap::default()))
                .is_some_and(|right| right != 0);
            if matches!(op, BinaryOp::Div | BinaryOp::Mod) && !divisor_is_nonzero {
                Expr::Mux {
                    condition: Box::new(Expr::Binary {
                        left: right,
                        op: BinaryOp::EqCase,
                        right: Box::new(Expr::Literal("0".to_string())),
                    }),
                    then_expr: Box::new(Expr::Literal(crate::DIV_ZERO_UNKNOWN_LITERAL.to_string())),
                    else_expr: Box::new(operation),
                }
            } else {
                operation
            }
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(guard_zero_divisions(*condition)),
            then_expr: Box::new(guard_zero_divisions(*then_expr)),
            else_expr: Box::new(guard_zero_divisions(*else_expr)),
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: Box::new(guard_zero_divisions(*expr)),
            items: items
                .into_iter()
                .map(|item| item.map(&mut |operand| guard_zero_divisions(operand)))
                .collect(),
        },
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args.into_iter().map(guard_zero_divisions).collect(),
        },
    }
}

pub(super) fn expr_from_primary(
    primary: &sv_parser::Primary,
    syntax_tree: &SyntaxTree,
) -> Option<Expr> {
    expr_from_primary_with_types(primary, syntax_tree, &PackedDimensions::default())
}

fn expr_from_primary_with_types(
    primary: &sv_parser::Primary,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    match primary {
        sv_parser::Primary::PrimaryLiteral(_) => {
            primary_literal_text(RefNode::Primary(primary), syntax_tree).map(Expr::Literal)
        }
        sv_parser::Primary::Hierarchical(hierarchical) => {
            let node = RefNode::HierarchicalIdentifier(&hierarchical.nodes.1);
            if packed_structs::has_member_access(
                node.clone(),
                RefNode::Select(&hierarchical.nodes.2),
            ) {
                let value = packed_structs::variable_member(
                    node,
                    &hierarchical.nodes.2,
                    syntax_tree,
                    packed_dimensions,
                )?;
                return Some(expr_from_lvalue(&value, packed_dimensions));
            }
            let name = identifier_text(
                RefNode::HierarchicalIdentifier(&hierarchical.nodes.1),
                syntax_tree,
            )?;
            let base = Expr::Ident(name);
            let select = &hierarchical.nodes.2;
            if select.nodes.1.nodes.0.is_empty() && select.nodes.2.is_none() {
                Some(base)
            } else {
                expr_select_from_select(base, select, syntax_tree, packed_dimensions)
            }
        }
        sv_parser::Primary::Concatenation(concat) => {
            let parts = concat
                .nodes
                .0
                .nodes
                .0
                .nodes
                .1
                .contents()
                .into_iter()
                .map(|expr| expr_from_expression_with_types(expr, syntax_tree, packed_dimensions))
                .collect::<Option<Vec<_>>>()?;
            let base = (!parts.is_empty()).then_some(Expr::Concat(parts))?;
            selected_concatenation(
                base,
                concat.nodes.1.as_ref().map(|range| &range.nodes.1),
                syntax_tree,
                packed_dimensions,
            )
        }
        sv_parser::Primary::MultipleConcatenation(concat) => {
            let (count, repeated) = &concat.nodes.0.nodes.0.nodes.1;
            let count = selects::bit_select_index(count, syntax_tree, packed_dimensions)?;
            let parts = repeated
                .nodes
                .0
                .nodes
                .1
                .contents()
                .into_iter()
                .map(|expr| expr_from_expression_with_types(expr, syntax_tree, packed_dimensions))
                .collect::<Option<Vec<_>>>()?;
            let base = (!parts.is_empty()).then_some(Expr::RepeatConcat { count, parts })?;
            selected_concatenation(
                base,
                concat.nodes.1.as_ref().map(|range| &range.nodes.1),
                syntax_tree,
                packed_dimensions,
            )
        }
        sv_parser::Primary::FunctionSubroutineCall(call) => {
            expr_from_function_subroutine_call(call, syntax_tree, packed_dimensions)
        }
        sv_parser::Primary::Cast(cast) => {
            if packed_dimensions.constant_indexed_base
                && let Some(value) =
                    runtime_constant_cast_const_expr(cast, syntax_tree, packed_dimensions)
            {
                return Some(const_expr_to_expr(value));
            }
            let expr = expr_from_expression_with_types(
                &cast.nodes.2.nodes.1,
                syntax_tree,
                packed_dimensions,
            )?;
            runtime_cast_expr(cast, expr, syntax_tree, packed_dimensions)
        }
        sv_parser::Primary::MintypmaxExpression(expr) => match &expr.nodes.0.nodes.1 {
            sv_parser::MintypmaxExpression::Expression(expr) => {
                expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)
            }
            sv_parser::MintypmaxExpression::Ternary(_) => None,
        },
        _ => None,
    }
}

pub(super) fn expression_is_grouped(expr: &sv_parser::Expression) -> bool {
    matches!(
        expr,
        sv_parser::Expression::Primary(primary)
            if matches!(&**primary, sv_parser::Primary::MintypmaxExpression(_))
    )
}

fn expr_from_conditional_expression(
    expr: &sv_parser::ConditionalExpression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    Some(Expr::Mux {
        condition: Box::new(expr_from_cond_predicate(
            &expr.nodes.0,
            syntax_tree,
            packed_dimensions,
        )?),
        then_expr: Box::new(expr_from_expression_with_types(
            &expr.nodes.3,
            syntax_tree,
            packed_dimensions,
        )?),
        else_expr: Box::new(expr_from_expression_with_types(
            &expr.nodes.5,
            syntax_tree,
            packed_dimensions,
        )?),
    })
}

pub(super) fn expr_from_function_subroutine_call(
    call: &sv_parser::FunctionSubroutineCall,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    if packed_dimensions.constant_indexed_base
        && let Some(ty) = dimensions::size_system_function_call_type(
            call,
            syntax_tree,
            &packed_dimensions.const_env,
            &packed_dimensions.type_aliases,
            Some(packed_dimensions),
        )
    {
        return Some(Expr::Literal(ty.width.to_string()));
    }
    if let sv_parser::SubroutineCall::SystemTfCall(call) = &call.nodes.0 {
        let sv_parser::SystemTfCall::ArgExpression(call) = &**call else {
            return None;
        };
        let name = syntax_tree.get_str(&call.nodes.0.nodes.0)?;
        let args = call.nodes.1.nodes.1.0.contents();
        if matches!(name, "$signed" | "$unsigned")
            && args.len() == 1
            && call.nodes.1.nodes.1.1.is_none()
        {
            // Reinterpret the operand's signedness without changing its width.
            let arg =
                expr_from_expression_with_types(args[0].as_ref()?, syntax_tree, packed_dimensions)?;
            let width = expr_static_width(&arg, packed_dimensions)?;
            return Some(Expr::Resize {
                expr: Box::new(arg),
                width,
                signed: name == "$signed",
            });
        }
        let constant_clog2 =
            packed_dimensions.constant_indexed_base && name == "$clog2" && args.len() == 1;
        if (!constant_clog2
            && typecheck::bit_vector_function_return_type(name, args.len()).is_none())
            || call.nodes.1.nodes.1.1.is_some()
        {
            return None;
        }
        let arg =
            expr_from_expression_with_types(args[0].as_ref()?, syntax_tree, packed_dimensions)?;
        return Some(Expr::Call {
            name: name.to_string(),
            args: vec![arg],
        });
    }
    let sv_parser::SubroutineCall::TfCall(call) = &call.nodes.0 else {
        return None;
    };
    let name = identifier_text(
        RefNode::PsOrHierarchicalTfIdentifier(&call.nodes.0),
        syntax_tree,
    )?;
    let args = match call.nodes.2.as_ref().map(|paren| &paren.nodes.1) {
        None => Vec::new(),
        Some(sv_parser::ListOfArguments::Ordered(args)) => {
            let contents = args.nodes.0.contents();
            if contents.len() == 1 && contents[0].is_none() {
                Vec::new()
            } else {
                let mut lowered = Vec::new();
                for expr in contents {
                    let Some(expr) = expr.as_ref() else {
                        return Some(Expr::Call {
                            name: "$unsupported_function_call".to_string(),
                            args: Vec::new(),
                        });
                    };
                    let Some(expr) =
                        expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)
                    else {
                        return Some(Expr::Call {
                            name: "$unsupported_function_call".to_string(),
                            args: Vec::new(),
                        });
                    };
                    lowered.push(expr);
                }
                lowered
            }
        }
        Some(sv_parser::ListOfArguments::Named(_)) => {
            return Some(Expr::Call {
                name: "$unsupported_function_call".to_string(),
                args: Vec::new(),
            });
        }
    };
    Some(Expr::Call { name, args })
}

fn selected_concatenation(
    base: Expr,
    selection: Option<&sv_parser::RangeExpression>,
    syntax_tree: &SyntaxTree,
    dimensions: &PackedDimensions,
) -> Option<Expr> {
    let (msb, lsb) = match selection {
        None => return Some(base),
        Some(sv_parser::RangeExpression::PartSelectRange(range)) => {
            part_select_bounds(range, syntax_tree, None, 0, dimensions)?
        }
        Some(sv_parser::RangeExpression::Expression(bit)) => {
            let bit = selects::bit_select_index(bit, syntax_tree, dimensions)?;
            (bit.clone(), bit)
        }
    };
    Some(Expr::Select {
        expr: Box::new(base),
        msb,
        lsb,
        signed: false,
    })
}
