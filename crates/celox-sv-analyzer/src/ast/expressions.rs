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
            let value = patterns::expr_from_pattern(
                &pattern.nodes.1,
                shape,
                syntax_tree,
                packed_dimensions,
            );
            // The legacy structure fallback cannot concatenate union aliases.
            if shape.members.len() > 1 && shape.members.iter().all(|member| member.offset() == 0) {
                return value;
            }
            return value.or_else(|pattern_error| {
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
        Expr::Call {
            callee,
            args,
            arg_order,
        } => Expr::Call {
            arg_order: arg_order.clone(),
            callee,
            args: args
                .into_iter()
                .map(|arg| arg.map(guard_zero_divisions))
                .collect(),
        },
    }
}

pub(super) fn expr_from_primary(
    primary: &sv_parser::Primary,
    syntax_tree: &SyntaxTree,
) -> Converted<Expr> {
    expr_from_primary_with_types(primary, syntax_tree, &PackedDimensions::default())
}

pub(super) fn expr_from_primary_with_types(
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
            if scope::imported().interface_types.contains_key(&name)
                && packed_dimensions.get(&name).is_none()
            {
                return Err(unsupported(format!(
                    "use of interface `{name}` other than as a port connection or through a member"
                )));
            }
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
        sv_parser::SubroutineCall::MethodCall(call) => {
            let (name, arguments) = native_interfaces::method_call(call, syntax_tree)?;
            expr_from_named_call(name, arguments, syntax_tree, packed_dimensions)
        }
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
    let function = system_functions::check_function_call(name, args.as_deref())?;
    parameters::reject_unbounded_data_query(
        call,
        syntax_tree,
        &packed_dimensions.const_env,
        packed_dimensions.scope_types_complete,
    )?;
    let operand_error = || unsupported(format!("operand of `{name}`"));
    match function {
        SystemFunction::Urandom | SystemFunction::Random | SystemFunction::UrandomRange => {
            let arguments = match call {
                _ if args.as_deref() == Some(&[]) => Vec::new(),
                sv_parser::SystemTfCall::ArgOptionl(call) if call.nodes.1.is_none() => Vec::new(),
                sv_parser::SystemTfCall::ArgExpression(call)
                    if call.nodes.1.nodes.1.1.is_none() =>
                {
                    call.nodes.1.nodes.1.0.contents()
                }
                _ => return Err(operand_error()),
            };
            let mut converted = Vec::new();
            for (position, argument) in arguments.into_iter().enumerate() {
                let argument = argument.as_ref().ok_or_else(operand_error)?;
                let argument = integral_argument(name, argument, syntax_tree, packed_dimensions)?;
                // 20.14.1: the seed of `$random` is an integral variable,
                // which the call updates.
                if function.writes_argument(position)
                    && !query_actual_is_writable(&argument, packed_dimensions)
                {
                    return Err(AnalyzerError::InvalidSystemTfCall {
                        name: name.into(),
                        detail: "seed must be an integral variable".into(),
                    });
                }
                converted.push(Some(argument));
            }
            Ok(Expr::Call {
                callee: Callee::System(function),
                args: converted,
                arg_order: Vec::new(),
            })
        }
        SystemFunction::Timeunit | SystemFunction::Timeprecision => {
            timescales::query(call, syntax_tree).map(|value| {
                Expr::Literal(format_typed_parameter_literal(i128::from(value), 32, true))
            })
        }
        SystemFunction::Isunbounded => {
            parameters::isunbounded_call(call, syntax_tree, &packed_dimensions.const_env)?
                .map(const_expr_to_expr)
                .ok_or_else(|| AnalyzerError::InvalidSystemTfCall {
                    name: name.into(),
                    detail: "argument must name a value parameter".into(),
                })
        }
        SystemFunction::Dimensions => dimensions::dimensions_system_function_call_value(
            call,
            syntax_tree,
            &packed_dimensions.const_env,
            &packed_dimensions.type_aliases,
            Some(packed_dimensions),
        )
        .map(|count| Expr::Literal(count.to_string()))
        .ok_or_else(operand_error),
        SystemFunction::UnpackedDimensions => dimensions::unpacked_dimensions_call_value(
            call,
            syntax_tree,
            &packed_dimensions.const_env,
            &packed_dimensions.type_aliases,
            Some(packed_dimensions),
        )
        .map(|count| Expr::Literal(count.to_string()))
        .ok_or_else(operand_error),
        SystemFunction::ArrayQuery(_) => dimensions::array_query_call(
            call,
            syntax_tree,
            &packed_dimensions.const_env,
            &packed_dimensions.type_aliases,
            Some(packed_dimensions),
        )
        .ok_or_else(operand_error),
        // `$bits(x)` and `$size(x)` depend only on the declared type of `x`.
        SystemFunction::Bits | SystemFunction::Size => dimensions::size_system_function_call_type(
            call,
            syntax_tree,
            &packed_dimensions.const_env,
            &packed_dimensions.type_aliases,
            Some(packed_dimensions),
        )
        .map(|ty| Expr::Literal(ty.width.to_string()))
        .ok_or_else(operand_error),
        SystemFunction::Signed | SystemFunction::Unsigned => {
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
                signed: function == SystemFunction::Signed,
            })
        }
        SystemFunction::Countbits => {
            let sv_parser::SystemTfCall::ArgExpression(call) = call else {
                return Err(operand_error());
            };
            let mut args = Vec::new();
            for argument in call.nodes.1.nodes.1.0.contents() {
                args.push(expr_from_expression_with_types(
                    argument.as_ref().ok_or_else(operand_error)?,
                    syntax_tree,
                    packed_dimensions,
                )?);
            }
            Ok(Expr::Call {
                arg_order: Vec::new(),
                callee: Callee::System(function),
                args: args.into_iter().map(Some).collect(),
            })
        }
        SystemFunction::Clog2
        | SystemFunction::Countones
        | SystemFunction::Onehot
        | SystemFunction::Onehot0
        | SystemFunction::Isunknown => {
            let arg = expr_from_expression_with_types(
                single_expression_argument(call).ok_or_else(operand_error)?,
                syntax_tree,
                packed_dimensions,
            )?;
            // `$clog2` of a constant is a constant.
            if function == SystemFunction::Clog2
                && let Some(argument) = expr_to_const(arg.clone())
                && let Some(value) = eval_ast_const_expr(
                    &ConstExpr::Function {
                        callee: Callee::System(function),
                        args: vec![argument],
                        site: None,
                    },
                    &packed_dimensions.const_env,
                )
            {
                return Ok(Expr::Literal(value.to_string()));
            }
            Ok(Expr::Call {
                arg_order: Vec::new(),
                callee: Callee::System(function),
                args: vec![Some(arg)],
            })
        }
    }
}

/// An integral argument of a random number function, such as the seed of
/// `$urandom`.
fn integral_argument(
    name: &str,
    argument: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let operand_error = || unsupported(format!("operand of `{name}`"));
    if dimensions::unpacked_expression_dimensions(
        argument,
        syntax_tree,
        &packed_dimensions.const_env,
        &packed_dimensions.type_aliases,
        Some(packed_dimensions),
    )
    .is_some_and(|rank| rank != 0)
    {
        return Err(operand_error());
    }
    let argument = expr_from_expression_with_types(argument, syntax_tree, packed_dimensions)
        .map_err(|error| AnalyzerError::InvalidSystemTfCall {
            name: name.into(),
            detail: format!("argument must be an integral expression: {error}"),
        })?;
    expr_static_width(&argument, packed_dimensions).ok_or_else(operand_error)?;
    Ok(argument)
}

/// The one expression argument of a system function call such as `$clog2(x)`.
pub(super) fn single_expression_argument(
    call: &sv_parser::SystemTfCall,
) -> Option<&sv_parser::Expression> {
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
    // `$unit::x` without arguments parses as a call, but names a parameter
    // or variable of the compilation unit unless `x` is a subroutine.
    if call.nodes.2.is_none()
        && RefNode::PsOrHierarchicalTfIdentifier(&call.nodes.0)
            .into_iter()
            .any(|node| {
                matches!(
                    node,
                    RefNode::PackageScope(sv_parser::PackageScope::Unit(_))
                )
            })
        && (packed_dimensions.const_env.contains_key(&name)
            || packed_dimensions.parameter_values.contains_key(&name)
            || packed_dimensions.get(&name).is_some())
    {
        return Ok(Expr::Ident(name));
    }
    expr_from_named_call(
        name,
        call.nodes.2.as_ref().map(|paren| &paren.nodes.1),
        syntax_tree,
        packed_dimensions,
    )
}

fn expr_from_named_call(
    name: String,
    arguments: Option<&sv_parser::ListOfArguments>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let query_formals = packed_dimensions
        .type_query
        .then(|| {
            packed_dimensions
                .function_return_types
                .get(&name)
                .and_then(|metadata| metadata.arguments.as_deref())
        })
        .flatten();
    let query_names;
    let signatures = if let Some(formals) = query_formals {
        query_names = HashMap::from_iter([(
            name.clone(),
            formals.iter().map(|formal| formal.name.clone()).collect(),
        )]);
        &query_names
    } else {
        &packed_dimensions.subroutine_param_names
    };
    let (bindings, arg_order) = bind_call_arguments(&name, arguments, syntax_tree, signatures)?;
    if let Some(formals) = query_formals {
        if bindings.len() > formals.len() {
            return Err(unsupported(format!("extra arguments of `{name}`")));
        }
        for (position, formal) in formals.iter().enumerate() {
            if bindings.get(position).is_none_or(|actual| actual.is_none()) && !formal.has_default {
                return Err(unsupported(format!(
                    "omitted argument {} of `{name}`",
                    position + 1
                )));
            }
        }
    }
    let mut args = Vec::new();
    for (position, expr) in bindings.into_iter().enumerate() {
        let Some(expr) = expr else {
            args.push(None);
            continue;
        };
        let shape = packed_dimensions
            .subroutine_param_shapes
            .get(&name)
            .and_then(|shapes| shapes.get(position));
        if let Some(shape) = shape {
            check_unpacked_array_assignment(
                expr,
                shape,
                || format!("argument {} of `{name}`", position + 1),
                syntax_tree,
                packed_dimensions,
            )?;
        }
        let arg = match (patterns::pattern_expression(expr), shape) {
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
        if query_formals.is_some_and(|formals| formals[position].direction.is_written())
            && !query_actual_is_writable(&arg, packed_dimensions)
        {
            return Err(unsupported(format!(
                "non-writable argument {} of `{name}`",
                position + 1
            )));
        }
        args.push(Some(arg));
    }
    Ok(Expr::Call {
        callee: Callee::Subroutine(name),
        args,
        arg_order,
    })
}

fn query_actual_is_writable(expression: &Expr, dimensions: &PackedDimensions) -> bool {
    match expression {
        Expr::Ident(name) => {
            dimensions.get(name).is_some()
                && !dimensions.const_env.contains_key(name)
                && !dimensions.parameter_values.contains_key(name)
                && !dimensions
                    .const_env
                    .contains_key(&parameters::unbounded_parameter_marker(name))
        }
        Expr::Select { expr, .. } => query_actual_is_writable(expr, dimensions),
        Expr::Concat(parts) => {
            !parts.is_empty()
                && parts
                    .iter()
                    .all(|part| query_actual_is_writable(part, dimensions))
        }
        _ => false,
    }
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

/// Check a system function call in expression position and identify the
/// function; `None` if its name cannot be read.
pub(crate) fn system_function_call(
    call: &sv_parser::SystemTfCall,
    syntax_tree: &SyntaxTree,
) -> Converted<Option<SystemFunction>> {
    let Some((name, args)) = system_tf_call_parts(call, syntax_tree) else {
        return Ok(None);
    };
    system_functions::check_function_call(name, args.as_deref()).map(Some)
}

/// The supported system function a call names, without checking the call.
pub(super) fn called_system_function(
    call: &sv_parser::SystemTfCall,
    syntax_tree: &SyntaxTree,
) -> Option<SystemFunction> {
    SystemFunction::from_name(system_tf_call_parts(call, syntax_tree)?.0)
}

/// The supported system task a call names, without checking the call.
pub(super) fn called_system_task(
    call: &sv_parser::SystemTfCall,
    syntax_tree: &SyntaxTree,
) -> Option<SystemTask> {
    SystemTask::from_name(system_tf_call_parts(call, syntax_tree)?.0)
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

/// Bind actuals to formal positions while retaining their source order.
/// IEEE 1800-2023 13.5.4 allows positional actuals followed by named actuals.
pub(super) fn bind_call_arguments<'a>(
    name: &str,
    args: Option<&'a sv_parser::ListOfArguments>,
    tree: &SyntaxTree,
    signatures: &HashMap<String, Vec<String>>,
) -> Converted<(Vec<Option<&'a sv_parser::Expression>>, Vec<usize>)> {
    let Some(args) = args else {
        return Ok((Vec::new(), Vec::new()));
    };
    let (mut positional, named): (Vec<_>, Vec<_>) = match args {
        sv_parser::ListOfArguments::Ordered(args) => {
            let contents = args.nodes.0.contents();
            let positional = if contents.len() == 1 && contents[0].is_none() {
                Vec::new()
            } else {
                contents.into_iter().map(|arg| arg.as_ref()).collect()
            };
            (
                positional,
                args.nodes
                    .1
                    .iter()
                    .map(|(_, _, identifier, value)| (identifier, value.nodes.1.as_ref()))
                    .collect(),
            )
        }
        sv_parser::ListOfArguments::Named(args) => {
            let mut named = vec![(&args.nodes.1, args.nodes.2.nodes.1.as_ref())];
            named.extend(
                args.nodes
                    .3
                    .iter()
                    .map(|(_, _, identifier, value)| (identifier, value.nodes.1.as_ref())),
            );
            (Vec::new(), named)
        }
    };
    if named.is_empty() {
        return Ok((positional, Vec::new()));
    }
    let params = signatures
        .get(name)
        .ok_or_else(|| unsupported(format!("named arguments of `{name}`")))?;
    if positional.len() > params.len() {
        return Err(unsupported(format!("too many arguments of `{name}`")));
    }
    let mut order: Vec<_> = (0..positional.len()).collect();
    let mut bound: HashSet<_> = order.iter().copied().collect();
    positional.resize(params.len(), None);
    for (identifier, expr) in named {
        let formal = identifier_text(RefNode::Identifier(identifier), tree)
            .ok_or_else(|| unsupported("named argument"))?;
        let position = params
            .iter()
            .position(|param| *param == formal)
            .ok_or_else(|| unsupported(format!("argument `{formal}` of `{name}`")))?;
        if !bound.insert(position) {
            return Err(unsupported(format!(
                "duplicate argument `{formal}` of `{name}`"
            )));
        }
        positional[position] = expr;
        order.push(position);
    }
    // Defaults for unbound formals are resolved by the call lowerer.
    order.extend((0..params.len()).filter(|position| !bound.contains(position)));
    Ok((positional, order))
}
