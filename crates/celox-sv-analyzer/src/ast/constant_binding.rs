//! Constant values bind native expression IDs without original primary pairing.
use super::*;
use celox_sv_syntax::{Declarations, Dimension, DimensionKind, ExprId, ExprKind};
fn disagreement() -> AnalyzerError {
    AnalyzerError::Parse("invalid native constant ownership".into())
}
pub(super) fn expression(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Option<ConstExpr>> {
    bind_in(
        root,
        native,
        tree,
        &PackedDimensions::new(HashMap::default(), env, aliases),
    )
}

pub(super) fn value_expression(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<Option<ConstExpr>> {
    let mut context = dimensions.clone();
    context.const_env = HashMap::default().into();
    context.type_aliases = SharedMap::default();
    bind_in_context(root, native, tree, &context, dimensions)
}
pub(super) fn bind(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<Option<ConstExpr>> {
    bind_in(root, tree.declarations(), tree, dimensions)
}
pub(super) fn indexed_expression(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    {
        let mut context = dimensions.clone();
        context.constant_indexed_base = true;
        expression_binding::bind_value(root, native, tree, &context, None)
    }
}
fn bind_in(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<Option<ConstExpr>> {
    bind_in_context(root, native, tree, dimensions, dimensions)
}
fn bind_in_context(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    query_dimensions: &PackedDimensions,
) -> Converted<Option<ConstExpr>> {
    enum Job {
        Enter(ExprId),
        Unary(String),
        Binary(String),
        Mux,
        System(SystemFunction, usize),
        Cast(casts::RuntimeCastTarget),
        Select(String, usize),
    }
    let mut jobs = vec![Job::Enter(root)];
    let mut values = Vec::new();
    while let Some(job) = jobs.pop() {
        match job {
            Job::Enter(id) if timescales::is_query(id, native) => {
                values.push(ConstExpr::Literal(
                    parameters::format_typed_parameter_literal(
                        i128::from(timescales::query(id, tree)?),
                        32,
                        true,
                    ),
                ));
            }
            Job::Enter(id) => match &native.expression(id).kind {
                ExprKind::Group(inner) => jobs.push(Job::Enter(*inner)),
                ExprKind::Literal(text)
                    if typecheck::integral_literal_type_metadata(text).is_some()
                        || (text.contains(['.', 'e', 'E'])
                            && text.replace('_', "").parse::<f64>().is_ok()) =>
                {
                    values.push(ConstExpr::Literal(text.clone()))
                }
                ExprKind::Name(name) if name.spelling == "$" => {
                    values.push(ConstExpr::Literal("$".into()))
                }
                ExprKind::Name(name) if !name.spelling.starts_with('$') => {
                    let name = instances::normalize_identifier(&name.spelling);
                    let literal = dimensions.parameter_values.get(&name).and_then(|value| {
                        let Expr::Literal(text) = value else {
                            return None;
                        };
                        (text.contains(['.', 'e', 'E']) && text.parse::<f64>().is_ok())
                            .then(|| ConstExpr::Literal(text.clone()))
                    });
                    values.push(literal.unwrap_or(ConstExpr::Ident(name)));
                }
                ExprKind::Unary {
                    operator,
                    expression,
                    postfix: false,
                } => {
                    if constants::unary_op_from_text(operator).is_none() {
                        return Ok(None);
                    }
                    jobs.push(Job::Unary(operator.clone()));
                    jobs.push(Job::Enter(*expression));
                }
                ExprKind::Binary {
                    left,
                    operator,
                    right,
                } => {
                    if !matches!(operator.as_str(), "->" | "<->")
                        && constants::binary_op_from_text(operator).is_none()
                    {
                        return Ok(None);
                    }
                    // A string operand orders the operands as strings
                    // (11.4.4), not as the integral values of their bytes.
                    if matches!(operator.as_str(), "<" | "<=" | ">" | ">=")
                        && (typename_operand(*left, native).is_some()
                            || typename_operand(*right, native).is_some())
                    {
                        let text = |id| -> Converted<Option<Vec<u8>>> {
                            let id = expression_binding::ungroup(id, native);
                            if let Some(argument) = typename_operand(id, native) {
                                return Ok(Some(
                                    type_names::query_string(argument, tree, query_dimensions)?
                                        .into_bytes(),
                                ));
                            }
                            Ok(match &native.expression(id).kind {
                                ExprKind::Literal(text) => {
                                    expression_binding::string_literal_bytes(text)
                                }
                                _ => None,
                            })
                        };
                        let (Some(left), Some(right)) = (text(*left)?, text(*right)?) else {
                            return Ok(None);
                        };
                        let ordering = left.cmp(&right);
                        let holds = match operator.as_str() {
                            "<" => ordering.is_lt(),
                            "<=" => ordering.is_le(),
                            ">" => ordering.is_gt(),
                            _ => ordering.is_ge(),
                        };
                        values.push(ConstExpr::Literal(
                            if holds { "1'b1" } else { "1'b0" }.into(),
                        ));
                        continue;
                    }
                    jobs.push(Job::Binary(operator.clone()));
                    jobs.push(Job::Enter(*right));
                    jobs.push(Job::Enter(*left));
                }
                ExprKind::Conditional {
                    condition,
                    then_expression,
                    else_expression,
                } => {
                    jobs.push(Job::Mux);
                    jobs.push(Job::Enter(*else_expression));
                    jobs.push(Job::Enter(*then_expression));
                    jobs.push(Job::Enter(*condition));
                }
                ExprKind::Cast { target, expression } => {
                    let target = match expression_binding::cast_target_native(
                        *target,
                        *expression,
                        native,
                        tree,
                        query_dimensions,
                    ) {
                        Ok(target) => target,
                        Err(AnalyzerError::Unsupported(_)) => return Ok(None),
                        Err(error) => return Err(error),
                    };
                    jobs.push(Job::Cast(target));
                    jobs.push(Job::Enter(*expression));
                }
                ExprKind::Concatenation(_) | ExprKind::Replication { .. } => {
                    // Actual values retain the parent's specialization context.
                    // The symbolic context deliberately omits its numeric env,
                    // but a concat must be folded to a typed literal here.
                    let expr =
                        expression_binding::bind_value(id, native, tree, query_dimensions, None)?;
                    let Some(value) =
                        selects::indexed_expression_value(&tree.state, expr, query_dimensions)
                    else {
                        return Ok(None);
                    };
                    values.push(value);
                }

                ExprKind::Call {
                    expression,
                    arguments,
                } if matches!(&native.expression(*expression).kind,ExprKind::Name(name) if name.spelling=="$isunbounded") =>
                {
                    let presence = arguments
                        .iter()
                        .map(|a| {
                            if a.value.is_some() {
                                system_functions::Arg::Given
                            } else {
                                system_functions::Arg::Omitted
                            }
                        })
                        .collect::<Vec<_>>();
                    system_functions::check_function_call(
                        "$isunbounded",
                        (!arguments.iter().any(|a| a.name.is_some()))
                            .then_some(presence.as_slice()),
                    )?;
                    let name = arguments
                        .first()
                        .and_then(|a| a.value)
                        .and_then(|id| expression_binding::native_name(&tree.state, id, native))
                        .ok_or_else(|| AnalyzerError::InvalidSystemTfCall {
                            name: "$isunbounded".into(),
                            detail: "argument must name a value parameter".into(),
                        })?;
                    let Some(value) = query_dimensions
                        .const_env
                        .get(&parameters::unbounded_parameter_marker(&name))
                    else {
                        return Ok(None);
                    };
                    values.push(ConstExpr::Literal(format!(
                        "1'b{}",
                        usize::from(*value == 1)
                    )));
                }
                ExprKind::Call {
                    expression,
                    arguments,
                } if matches!(&native.expression(*expression).kind,ExprKind::Name(name) if matches!(name.spelling.as_str(),"$clog2"|"$signed"|"$unsigned")) =>
                {
                    let ExprKind::Name(name) = &native.expression(*expression).kind else {
                        unreachable!()
                    };
                    let presence = arguments
                        .iter()
                        .map(|a| {
                            if a.value.is_some() {
                                system_functions::Arg::Given
                            } else {
                                system_functions::Arg::Omitted
                            }
                        })
                        .collect::<Vec<_>>();
                    let function = system_functions::check_function_call(
                        &name.spelling,
                        (!arguments.iter().any(|a| a.name.is_some()))
                            .then_some(presence.as_slice()),
                    )?;
                    jobs.push(Job::System(function, arguments.len()));
                    jobs.extend(
                        arguments
                            .iter()
                            .rev()
                            .filter_map(|a| a.value)
                            .map(Job::Enter),
                    );
                }
                ExprKind::Select { .. } => {
                    let mut base = id;
                    let mut indices = Vec::new();
                    while let ExprKind::Select {
                        expression,
                        dimension,
                    } = &native.expression(base).kind
                    {
                        let DimensionKind::Index(index) = dimension.kind else {
                            break;
                        };
                        indices.push(index);
                        base = *expression;
                    }
                    if let ExprKind::Name(name) = &native.expression(base).kind {
                        // Override expressions retain symbolic values, but a packed
                        // element still has its declared width and coordinates.
                        // The symbolic context deliberately has no numeric env.
                        let name_key = instances::normalize_identifier(&name.spelling);
                        if dimensions.const_env.is_empty()
                            && query_dimensions.get(&name_key).is_some_and(|shape| {
                                shape.packed.len() > 1 || !shape.unpacked.is_empty()
                            })
                        {
                            let value = indexed_expression(id, native, tree, query_dimensions)?;
                            let value = if query_dimensions
                                .array_parameter_values
                                .contains_key(&name_key)
                            {
                                selects::indexed_expression_value(
                                    &tree.state,
                                    value,
                                    query_dimensions,
                                )
                            } else {
                                expr_to_index_const(&tree.state, value, query_dimensions)
                            };
                            values.push(
                                value.ok_or_else(|| unsupported("parameter selection shape"))?,
                            );
                            continue;
                        }
                        indices.reverse();
                        jobs.push(Job::Select(
                            instances::normalize_identifier(&name.spelling),
                            indices.len(),
                        ));
                        jobs.extend(indices.into_iter().rev().map(Job::Enter));
                    } else {
                        let value = match indexed_expression(id, native, tree, query_dimensions) {
                            Ok(value) => value,
                            Err(AnalyzerError::Unsupported(_)) => return Ok(None),
                            Err(error) => return Err(error),
                        };
                        let Some(value) =
                            selects::indexed_expression_value(&tree.state, value, query_dimensions)
                        else {
                            return Ok(None);
                        };
                        values.push(value);
                    }
                }
                ExprKind::Call { .. } | ExprKind::Member { .. } => {
                    let is_query = matches!(&native.expression(id).kind,ExprKind::Call{expression,..} if matches!(&native.expression(*expression).kind,ExprKind::Name(name) if SystemFunction::from_name(&name.spelling).is_some_and(|f|f.is_data_query()||matches!(f,SystemFunction::Isunbounded|SystemFunction::Typename))));
                    let mut context = if is_query {
                        query_dimensions.clone()
                    } else {
                        dimensions.clone()
                    };
                    context.subroutine_param_names =
                        const_functions::argument_names(&tree.state.const_functions);
                    let value =
                        match expression_binding::bind_value(id, native, tree, &context, None) {
                            Ok(value) => value,
                            Err(AnalyzerError::Unsupported(_)) => return Ok(None),
                            Err(error) => return Err(error),
                        };
                    // A `$typename` string is the constant of its bytes, as a
                    // string literal is (5.9).
                    let value = match value {
                        Expr::Call {
                            callee: Callee::String(StringOp::FromBits),
                            mut args,
                            ..
                        } if is_query && matches!(args.as_slice(), [Some(Expr::Literal(_))]) => {
                            args.pop().flatten().unwrap()
                        }
                        value => value,
                    };
                    let countbits = matches!(
                        &value,
                        Expr::Call {
                            callee: Callee::System(SystemFunction::Countbits),
                            ..
                        }
                    );
                    let Some(value) = (if countbits {
                        constants::countbits_constant_call(
                            &tree.state,
                            value,
                            &dimensions.const_env,
                        )
                    } else {
                        constants::user_call_value(&tree.state, value, &dimensions.const_env)
                    }) else {
                        return Ok(None);
                    };
                    values.push(value);
                }
                ExprKind::ArrayMethodWith { .. } => {
                    return Err(unsupported("array method with expression"));
                }
                _ => return Ok(None),
            },
            Job::Cast(target) => {
                let value = values.pop().ok_or_else(disagreement)?;
                let Some(value) = casts::cast_constant_target(
                    &tree.state,
                    value,
                    target,
                    &query_dimensions.const_env,
                ) else {
                    return Ok(None);
                };
                values.push(value);
            }
            Job::Unary(op) => {
                let expr = values.pop().ok_or_else(disagreement)?;
                values.push(ConstExpr::Unary {
                    op: constants::unary_op_from_text(&op).ok_or_else(disagreement)?,
                    expr: Box::new(expr),
                });
            }
            Job::Binary(op) => {
                let right = values.pop().ok_or_else(disagreement)?;
                let left = values.pop().ok_or_else(disagreement)?;
                let left = if op == "->" || op == "<->" {
                    ConstExpr::Unary {
                        op: UnaryOp::LogicNot,
                        expr: Box::new(left),
                    }
                } else {
                    left
                };
                let right = if op == "<->" {
                    ConstExpr::Unary {
                        op: UnaryOp::LogicNot,
                        expr: Box::new(right),
                    }
                } else {
                    right
                };
                values.push(ConstExpr::Binary {
                    left: Box::new(left),
                    op: if op == "<->" {
                        BinaryOp::Eq
                    } else if op == "->" {
                        BinaryOp::LogicOr
                    } else {
                        constants::binary_op_from_text(&op).ok_or_else(disagreement)?
                    },
                    right: Box::new(right),
                });
            }
            Job::Mux => {
                let else_expr = values.pop().ok_or_else(disagreement)?;
                let then_expr = values.pop().ok_or_else(disagreement)?;
                let condition = values.pop().ok_or_else(disagreement)?;
                values.push(ConstExpr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                });
            }
            Job::System(function, count) => {
                let start = values.len().checked_sub(count).ok_or_else(disagreement)?;
                let args = values.split_off(start);
                values.push(ConstExpr::call(&tree.state, Callee::System(function), args));
            }
            Job::Select(name, count) => {
                let start = values.len().checked_sub(count).ok_or_else(disagreement)?;
                let mut indices = values.split_off(start);
                if let Some(indices) = indices
                    .iter()
                    .map(|index| eval_ast_const_expr(&tree.state, index, &dimensions.const_env))
                    .collect::<Option<Vec<_>>>()
                    && let Some(literal) =
                        parameter_element_literal(&name, &indices, &dimensions.const_env)
                {
                    values.push(ConstExpr::Literal(literal));
                } else if indices.len() == 1 {
                    values.push(ConstExpr::Select {
                        expr: Box::new(ConstExpr::Ident(name)),
                        bit: Box::new(indices.pop().unwrap()),
                    });
                } else {
                    return Ok(None);
                }
            }
        }
    }
    if values.len() != 1 {
        return Err(disagreement());
    }
    Ok(values.pop())
}
pub(super) fn packed_ranges(
    dimensions: &[Dimension],
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Vec<PackedRange>> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    type_binding::binding::packed_ranges(dimensions, tree, &context)
}

pub(super) fn unpacked_ranges(
    dimensions: &[Dimension],
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Vec<UnpackedRange>> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    dimensions
        .iter()
        .map(|dimension| bind_unpacked_dimension(dimension, tree, &context))
        .collect()
}

pub(super) fn unpacked_dimension(
    dimension: &Dimension,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<UnpackedRange> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    bind_unpacked_dimension(dimension, tree, &context)
}

/// Native array dimensions retain their authored bound handles.
pub(super) fn bind_unpacked_ranges(
    dimensions: &[Dimension],
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Vec<UnpackedRange>> {
    dimensions
        .iter()
        .map(|dimension| bind_unpacked_dimension(dimension, tree, context))
        .collect()
}
pub(super) fn bind_unpacked_dimension(
    dimension: &Dimension,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<UnpackedRange> {
    let range = match dimension.kind {
        DimensionKind::Range { left, right } => bind(left, tree, context)?
            .zip(bind(right, tree, context)?)
            .map(|(left, right)| UnpackedRange::new(left, right)),
        DimensionKind::Index(size) => bind(size, tree, context)?.map(UnpackedRange::sized),
        DimensionKind::Unsized => return Err(unsupported("unsized unpacked array dimension")),
        DimensionKind::Queue(_) => return Err(unsupported("queue array dimension")),
        DimensionKind::Wildcard => return Err(unsupported("associative array dimension")),
        DimensionKind::Indexed { .. } => {
            return Err(unsupported("indexed array declaration dimension"));
        }
    };
    range.ok_or_else(|| unsupported("unresolved unpacked array dimension"))
}

/// The operand of native expression `id` if it is a `$typename` call.
fn typename_operand(id: ExprId, native: &Declarations) -> Option<ExprId> {
    let ExprKind::Call {
        expression,
        arguments,
    } = &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    else {
        return None;
    };
    let ExprKind::Name(name) = &native.expression(*expression).kind else {
        return None;
    };
    match arguments.as_slice() {
        [argument]
            if SystemFunction::from_name(&name.spelling) == Some(SystemFunction::Typename) =>
        {
            argument.value
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_constant_literals_and_operators_supply_values() {
        for (text, expected) in [("8", 8), ("7+8", 15), ("$bits(logic)", 1)] {
            let tree = crate::source_tree::parse_source(
                &format!("module M #(parameter P={text});endmodule"),
                Path::new("native-constant-values.sv"),
            )
            .unwrap();
            let tree = AnalysisContext::unspecialized(&tree);
            let celox_sv_syntax::ParameterKind::Value {
                default: Some(id), ..
            } = tree.declarations().units[0].parameters.as_ref().unwrap()[0].kind
            else {
                panic!()
            };
            let value = bind(id, &tree, &PackedDimensions::default())
                .unwrap()
                .unwrap();
            assert_eq!(
                eval_ast_const_expr(&tree.state, &value, &HashMap::default()),
                Some(expected)
            );
        }
    }
    #[test]
    fn deeply_grouped_native_binding_uses_no_recursive_walk() {
        let path = Path::new("deep-native-constant.sv");
        let tree =
            crate::source_tree::parse_source("module M #(parameter P=7);endmodule", path).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let native = Declarations::parse(
            &format!(
                "module M #(parameter P={}7{});endmodule",
                "(".repeat(10000),
                ")".repeat(10000)
            ),
            path,
        )
        .unwrap();
        let celox_sv_syntax::ParameterKind::Value {
            default: Some(id), ..
        } = native.units[0].parameters.as_ref().unwrap()[0].kind
        else {
            panic!()
        };
        assert_eq!(
            bind_in(id, &native, &tree, &PackedDimensions::default()).unwrap(),
            Some(ConstExpr::Literal("7".into()))
        );
    }
}
