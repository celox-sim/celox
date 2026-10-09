//! Runtime expression and primary syntax conversion.

use super::*;

pub(super) fn expr_from_expression(
    expr: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
) -> Converted<Expr> {
    expr_from_expression_with_types(expr, syntax_tree, &PackedDimensions::default())
}

pub(super) fn expr_from_expression_with_types(
    expr: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    expr_from_expression_with_types_raw(expr, syntax_tree, packed_dimensions)
        .map(guard_zero_divisions)
}

fn expr_from_expression_with_types_raw(
    expr: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    match expr {
        sv_parser::Expression::Primary(primary) => {
            expr_from_primary_with_types(primary, syntax_tree, packed_dimensions)
        }
        sv_parser::Expression::Unary(unary) => {
            let expr =
                expr_from_primary_with_types(&unary.nodes.2, syntax_tree, packed_dimensions)?;
            let symbol = &unary.nodes.0.nodes.0.nodes.0;
            unary_expr_from_symbol(symbol, expr, syntax_tree).ok_or_else(|| {
                unsupported(format!(
                    "unary operator `{}`",
                    syntax_tree.get_str(symbol).unwrap_or_default()
                ))
            })
        }
        sv_parser::Expression::Binary(binary) => {
            let right_is_grouped = expression_is_grouped(&binary.nodes.3);
            let left = expr_from_expression_with_types_raw(
                &binary.nodes.0,
                syntax_tree,
                packed_dimensions,
            )?;
            let symbol = &binary.nodes.1.nodes.0.nodes.0;
            // `a ~^ b` is the complement of `a ^ b` (IEEE 1800-2023 11.4.8).
            if matches!(syntax_tree.get_str(symbol), Some("~^" | "^~")) {
                let right = expr_from_expression_with_types_raw(
                    &binary.nodes.3,
                    syntax_tree,
                    packed_dimensions,
                )?;
                return Ok(Expr::Unary {
                    op: UnaryOp::BitNot,
                    expr: Box::new(Expr::Binary {
                        left: Box::new(left),
                        op: BinaryOp::BitXor,
                        right: Box::new(right),
                    }),
                });
            }
            let op = binary_op_from_symbol(symbol, syntax_tree).ok_or_else(|| {
                unsupported(format!(
                    "binary operator `{}`",
                    syntax_tree.get_str(symbol).unwrap_or_default()
                ))
            })?;
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
            Ok(if right_is_grouped {
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
        sv_parser::Expression::IncOrDecExpression(_) => {
            Err(unsupported("increment or decrement expression"))
        }
        sv_parser::Expression::OperatorAssignment(_) => {
            Err(unsupported("assignment used as an expression"))
        }
        sv_parser::Expression::TaggedUnionExpression(_) => {
            Err(unsupported("tagged union expression"))
        }
    }
}

/// Lower the right-hand side of an assignment to `lhs`. An assignment pattern
/// (`'{a, b}`, `'{x: a, default: b}`) takes its shape from the target.
pub(super) fn expr_from_expression_for_lvalue(
    expr: &sv_parser::Expression,
    lhs: &LValue,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    if let sv_parser::Expression::Primary(primary) = expr
        && let sv_parser::Primary::AssignmentPatternExpression(pattern) = &**primary
    {
        if pattern.nodes.0.is_some() {
            return patterns::typed_pattern(pattern, syntax_tree, packed_dimensions);
        }
        if let LValue::Ident(name) = lhs
            && let Some(shape) = packed_dimensions.get(name)
        {
            return patterns::expr_from_pattern(
                &pattern.nodes.1,
                shape,
                syntax_tree,
                packed_dimensions,
            )
            .or_else(|pattern_error| {
                // A packed structure or a vector `default` fill.
                expr_from_assignment_pattern(&pattern.nodes.1, lhs, syntax_tree, packed_dimensions)
                    .map_err(|_| pattern_error)
            });
        }
        return expr_from_assignment_pattern(&pattern.nodes.1, lhs, syntax_tree, packed_dimensions);
    }
    expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)
}

/// An assignment pattern for a packed structure (its members in declaration
/// order, the first being the most significant) or a `default` fill.
fn expr_from_assignment_pattern(
    pattern: &sv_parser::AssignmentPattern,
    lhs: &LValue,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let target = || unsupported("assignment pattern for this target");
    let LValue::Ident(name) = lhs else {
        return Err(target());
    };
    let members = &packed_dimensions.get(name).ok_or_else(target)?.members;
    let lower = |expr: &sv_parser::Expression| {
        expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)
    };
    // The expression assigned to each member, in declaration order.
    let values: Vec<&sv_parser::Expression> = match pattern {
        sv_parser::AssignmentPattern::List(list) => {
            let values = list.nodes.0.nodes.1.contents();
            if members.is_empty() || values.len() != members.len() {
                return Err(unsupported(
                    "assignment pattern whose items do not match the structure members",
                ));
            }
            values
        }
        sv_parser::AssignmentPattern::Structure(structure) => {
            let items = structure.nodes.0.nodes.1.contents();
            let mut default = None;
            let mut named: Vec<(String, &sv_parser::Expression)> = Vec::new();
            for (key, _, value) in items {
                match key {
                    sv_parser::StructurePatternKey::MemberIdentifier(member) => named.push((
                        identifier_text(RefNode::MemberIdentifier(member), syntax_tree)
                            .ok_or_else(|| unsupported("assignment pattern member name"))?,
                        value,
                    )),
                    sv_parser::StructurePatternKey::AssignmentPatternKey(key) => {
                        let sv_parser::AssignmentPatternKey::Default(_) = &**key else {
                            return Err(unsupported("assignment pattern type key"));
                        };
                        default = Some(value);
                    }
                }
            }
            if members.is_empty() {
                // `'{default: v}` fills a vector.
                return match (named.is_empty(), default) {
                    (true, Some(value)) => lower(value),
                    _ => Err(target()),
                };
            }
            if let Some((name, _)) = named
                .iter()
                .find(|(name, _)| !members.iter().any(|member| member.name() == name))
            {
                return Err(unsupported(format!(
                    "assignment pattern key `{name}` that is not a structure member"
                )));
            }
            members
                .iter()
                .map(|member| {
                    named
                        .iter()
                        .find(|(name, _)| name == member.name())
                        .map(|(_, value)| *value)
                        .or(default)
                        .ok_or_else(|| {
                            unsupported(format!(
                                "assignment pattern without a value for member `{}`",
                                member.name()
                            ))
                        })
                })
                .collect::<Converted<Vec<_>>>()?
        }
        _ => return Err(unsupported("replicated assignment pattern")),
    };
    let parts = members
        .iter()
        .zip(values)
        .map(|(member, value)| {
            let width = expr_type_from_type(member.r#type(), &packed_dimensions.const_env)
                .ok_or_else(|| {
                    unsupported(format!("type of structure member `{}`", member.name()))
                })?
                .width;
            let value = lower(value)?;
            let signed = expr_signedness(
                &value,
                &packed_dimensions.expression_signedness,
                &packed_dimensions.functions,
            )
            .unwrap_or(false);
            Ok(Expr::Resize {
                expr: Box::new(value),
                width,
                signed,
            })
        })
        .collect::<Converted<Vec<_>>>()?;
    Ok(match <[Expr; 1]>::try_from(parts) {
        Ok([part]) => part,
        Err(parts) => Expr::Concat(parts),
    })
}

/// `x inside {a, [lo:hi], ...}`: kept as an `Inside` expression, so that its
/// operands and its matching rules stay visible to later stages.
fn expr_from_inside_expression(
    inside: &sv_parser::InsideExpression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
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
                Ok(InsideItem::Range {
                    low: expr_from_expression_with_types_raw(low, syntax_tree, packed_dimensions)?,
                    high: expr_from_expression_with_types_raw(
                        high,
                        syntax_tree,
                        packed_dimensions,
                    )?,
                })
            }
        })
        .collect::<Converted<Vec<_>>>()?;
    if items.is_empty() {
        return Err(unsupported("inside expression without items"));
    }
    Ok(Expr::Inside {
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
) -> Converted<Expr> {
    expr_from_primary_with_types(primary, syntax_tree, &PackedDimensions::default())
}

fn expr_from_primary_with_types(
    primary: &sv_parser::Primary,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    match primary {
        sv_parser::Primary::PrimaryLiteral(_) => {
            primary_literal_text(RefNode::Primary(primary), syntax_tree)
                .map(Expr::Literal)
                .ok_or_else(|| unsupported("literal"))
        }
        sv_parser::Primary::Hierarchical(hierarchical) => {
            let node = RefNode::HierarchicalIdentifier(&hierarchical.nodes.1);
            if packed_structs::has_member_access(
                node.clone(),
                RefNode::Select(&hierarchical.nodes.2),
            ) {
                let value = packed_structs::variable_member(
                    node,
                    hierarchical
                        .nodes
                        .0
                        .is_some()
                        .then(|| {
                            reference_name(RefNode::PrimaryHierarchical(hierarchical), syntax_tree)
                        })
                        .flatten(),
                    &hierarchical.nodes.2,
                    syntax_tree,
                    packed_dimensions,
                )?;
                return Ok(expr_from_lvalue(&value, packed_dimensions));
            }
            let name = reference_name(RefNode::PrimaryHierarchical(hierarchical), syntax_tree)
                .ok_or_else(|| unsupported("hierarchical identifier"))?;
            let base = Expr::Ident(name);
            let select = &hierarchical.nodes.2;
            if select.nodes.1.nodes.0.is_empty() && select.nodes.2.is_none() {
                Ok(base)
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
                .collect::<Converted<Vec<_>>>()?;
            if parts.is_empty() {
                return Err(unsupported("empty concatenation"));
            }
            selected_concatenation(
                Expr::Concat(parts),
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
                .collect::<Converted<Vec<_>>>()?;
            if parts.is_empty() {
                return Err(unsupported("empty replication"));
            }
            selected_concatenation(
                Expr::RepeatConcat { count, parts },
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
                return Ok(const_expr_to_expr(value));
            }
            let expr = expr_from_expression_with_types(
                &cast.nodes.2.nodes.1,
                syntax_tree,
                packed_dimensions,
            )?;
            runtime_cast_expr(cast, expr, syntax_tree, packed_dimensions)
        }
        sv_parser::Primary::AssignmentPatternExpression(pattern) if pattern.nodes.0.is_some() => {
            patterns::typed_pattern(pattern, syntax_tree, packed_dimensions)
        }
        sv_parser::Primary::AssignmentPatternExpression(_) => {
            Err(unsupported("assignment pattern without a target type"))
        }
        sv_parser::Primary::MintypmaxExpression(expr) => match &expr.nodes.0.nodes.1 {
            sv_parser::MintypmaxExpression::Expression(expr) => {
                expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)
            }
            sv_parser::MintypmaxExpression::Ternary(_) => {
                Err(unsupported("min:typ:max expression"))
            }
        },
        sv_parser::Primary::EmptyUnpackedArrayConcatenation(_) => {
            Err(unsupported("empty unpacked array concatenation"))
        }
        sv_parser::Primary::StreamingConcatenation(_) => {
            Err(unsupported("streaming concatenation"))
        }
        sv_parser::Primary::SequenceMethodCall(_) => Err(unsupported("sequence method call")),
        sv_parser::Primary::This(_) => Err(unsupported("`this`")),
        sv_parser::Primary::Dollar(_) => Err(unsupported("`$` as a value")),
        sv_parser::Primary::Null(_) => Err(unsupported("`null`")),
        sv_parser::Primary::LetExpression(_) => Err(unsupported("let expression")),
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
) -> Converted<Expr> {
    Ok(Expr::Mux {
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
) -> Converted<Expr> {
    expr_from_subroutine_call(&call.nodes.0, syntax_tree, packed_dimensions)
}

/// A subroutine call as an expression. A call statement uses this too, for a
/// system function whose value it discards.
pub(super) fn expr_from_subroutine_call(
    call: &sv_parser::SubroutineCall,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    match call {
        sv_parser::SubroutineCall::SystemTfCall(call) => {
            expr_from_system_function_call(call, syntax_tree, packed_dimensions)
        }
        sv_parser::SubroutineCall::TfCall(call) => {
            expr_from_tf_call(call, syntax_tree, packed_dimensions)
        }
        sv_parser::SubroutineCall::MethodCall(_) => Err(unsupported("method call")),
        sv_parser::SubroutineCall::Randomize(_) => Err(unsupported("randomize call")),
    }
}

/// A system function call as an expression operand.
fn expr_from_system_function_call(
    call: &sv_parser::SystemTfCall,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let (name, args) = system_tf_call_parts(call, syntax_tree)
        .ok_or_else(|| unsupported("system function name"))?;
    system_functions::check_call(
        name,
        args.as_deref(),
        system_functions::CallSite::Expression,
    )?;
    let operand_error = || unsupported(format!("operand of `{name}`"));
    match name {
        // `$bits(x)` and `$size(x)` depend only on the declared type of `x`.
        "$bits" | "$size" => dimensions::size_system_function_call_type(
            call,
            syntax_tree,
            &packed_dimensions.const_env,
            &packed_dimensions.type_aliases,
            Some(packed_dimensions),
        )
        .map(|ty| Expr::Literal(ty.width.to_string()))
        .ok_or_else(operand_error),
        "$signed" | "$unsigned" => {
            // Reinterpret the operand's signedness without changing its width.
            let arg = expr_from_expression_with_types(
                single_expression_argument(call).ok_or_else(operand_error)?,
                syntax_tree,
                packed_dimensions,
            )?;
            let width = expr_static_width(&arg, packed_dimensions).ok_or_else(operand_error)?;
            Ok(Expr::Resize {
                expr: Box::new(arg),
                width,
                signed: name == "$signed",
            })
        }
        "$clog2" | "$countones" | "$onehot" | "$onehot0" | "$isunknown" => {
            let arg = expr_from_expression_with_types(
                single_expression_argument(call).ok_or_else(operand_error)?,
                syntax_tree,
                packed_dimensions,
            )?;
            // `$clog2` of a constant is a constant.
            if name == "$clog2"
                && let Some(argument) = expr_to_const(arg.clone())
                && let Some(value) = eval_ast_const_expr(
                    &ConstExpr::Function {
                        name: name.to_string(),
                        args: vec![argument],
                        site: None,
                    },
                    &packed_dimensions.const_env,
                )
            {
                return Ok(Expr::Literal(value.to_string()));
            }
            Ok(Expr::Call {
                name: name.to_string(),
                args: vec![arg],
            })
        }
        _ => Err(AnalyzerError::Unsupported(format!(
            "system function `{name}` in an expression"
        ))),
    }
}

/// The one expression argument of a system function call such as `$clog2(x)`.
fn single_expression_argument(call: &sv_parser::SystemTfCall) -> Option<&sv_parser::Expression> {
    let sv_parser::SystemTfCall::ArgExpression(call) = call else {
        return None;
    };
    let (args, clocking) = &call.nodes.1.nodes.1;
    match (args.contents().as_slice(), clocking) {
        ([Some(arg)], None) => Some(arg),
        _ => None,
    }
}

/// A user function call as an expression operand.
pub(super) fn expr_from_tf_call(
    call: &sv_parser::TfCall,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let name = reference_name(
        RefNode::PsOrHierarchicalTfIdentifier(&call.nodes.0),
        syntax_tree,
    )
    .ok_or_else(|| unsupported("subroutine name"))?;
    let args = match call.nodes.2.as_ref().map(|paren| &paren.nodes.1) {
        None => Vec::new(),
        Some(sv_parser::ListOfArguments::Ordered(args)) => {
            let contents = args.nodes.0.contents();
            if contents.len() == 1 && contents[0].is_none() {
                Vec::new()
            } else {
                let mut lowered = Vec::new();
                for (position, expr) in contents.into_iter().enumerate() {
                    let Some(expr) = expr.as_ref() else {
                        return Err(unsupported(format!(
                            "omitted argument {} of `{name}`",
                            position + 1
                        )));
                    };
                    // An assignment pattern takes its shape from the formal.
                    let shape = packed_dimensions
                        .subroutine_param_shapes
                        .get(&name)
                        .and_then(|shapes| shapes.get(position));
                    if let Some(shape) = shape {
                        // Passing an argument is an assignment-like context
                        // (IEEE 1800-2023 10.8).
                        check_unpacked_array_assignment(
                            expr,
                            shape,
                            || format!("argument {} of `{name}`", position + 1),
                            syntax_tree,
                            packed_dimensions,
                        )?;
                    }
                    let lowered_arg = match (patterns::pattern_expression(expr), shape) {
                        (Some(pattern), Some(shape)) if pattern.nodes.0.is_none() => {
                            patterns::expr_from_pattern(
                                &pattern.nodes.1,
                                shape,
                                syntax_tree,
                                packed_dimensions,
                            )?
                        }
                        _ => expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)?,
                    };
                    lowered.push(lowered_arg);
                }
                lowered
            }
        }
        Some(sv_parser::ListOfArguments::Named(_)) => {
            return Err(unsupported(format!("named arguments of `{name}`")));
        }
    };
    Ok(Expr::Call { name, args })
}

fn selected_concatenation(
    base: Expr,
    selection: Option<&sv_parser::RangeExpression>,
    syntax_tree: &SyntaxTree,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let (msb, lsb) = match selection {
        None => return Ok(base),
        Some(sv_parser::RangeExpression::PartSelectRange(range)) => {
            part_select_bounds(range, syntax_tree, None, 0, dimensions)?
        }
        Some(sv_parser::RangeExpression::Expression(bit)) => {
            let bit = selects::bit_select_index(bit, syntax_tree, dimensions)?;
            (bit.clone(), bit)
        }
    };
    Ok(Expr::Select {
        expr: Box::new(base),
        msb,
        lsb,
        signed: false,
    })
}

/// The name of a system task or function call, with its leading `$`, and its
/// arguments as written (`None` when they are bound by name).
pub(super) fn system_tf_call_parts<'a>(
    call: &sv_parser::SystemTfCall,
    syntax_tree: &'a SyntaxTree,
) -> Option<(&'a str, Option<Vec<system_functions::Arg>>)> {
    use system_functions::Arg;
    let positional = |contents: Vec<&Option<sv_parser::Expression>>| -> Vec<Arg> {
        // `$f()` parses as one omitted argument.
        if contents.len() == 1 && contents[0].is_none() {
            Vec::new()
        } else {
            contents
                .iter()
                .map(|arg| {
                    if arg.is_some() {
                        Arg::Given
                    } else {
                        Arg::Omitted
                    }
                })
                .collect()
        }
    };
    let (identifier, args) = match call {
        sv_parser::SystemTfCall::ArgOptionl(call) => (
            &call.nodes.0,
            match call.nodes.1.as_ref() {
                None => Some(Vec::new()),
                Some(paren) => match &paren.nodes.1 {
                    sv_parser::ListOfArguments::Ordered(args) if args.nodes.1.is_empty() => {
                        Some(positional(args.nodes.0.contents()))
                    }
                    _ => None,
                },
            },
        ),
        sv_parser::SystemTfCall::ArgDataType(call) => (
            &call.nodes.0,
            Some(vec![
                Arg::Given;
                1 + usize::from(call.nodes.1.nodes.1.1.is_some())
            ]),
        ),
        sv_parser::SystemTfCall::ArgExpression(call) => {
            let (args, clocking) = &call.nodes.1.nodes.1;
            let mut args = positional(args.contents());
            if let Some((_, event)) = clocking {
                args.push(if event.is_some() {
                    Arg::Given
                } else {
                    Arg::Omitted
                });
            }
            (&call.nodes.0, Some(args))
        }
    };
    Some((syntax_tree.get_str(&identifier.nodes.0)?, args))
}
