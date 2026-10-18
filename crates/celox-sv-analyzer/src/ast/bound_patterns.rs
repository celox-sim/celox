//! Assignment-pattern bindings retain structure, not generated source. Names,
//! keys and repetition counts bind in the caller; element sizing happens in
//! the receiving declaration's assignment context (IEEE 1800-2023 10.9).
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BoundPattern {
    items: Items,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Items {
    Value(Expr),
    List(Vec<Value>),
    Repeat(usize, Vec<Value>),
    Keyed(Vec<(Key, Value)>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Index(i128),
    Member(String),
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Value {
    Expression(Expr),
    Pattern(BoundPattern),
}

pub(super) fn from_param(
    expression: &sv_parser::ParamExpression,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<BoundPattern> {
    let sv_parser::ParamExpression::MintypmaxExpression(expression) = expression else {
        return Err(unsupported("parameter assignment pattern"));
    };
    let sv_parser::MintypmaxExpression::Expression(expression) = expression.as_ref() else {
        return Err(unsupported("parameter pattern mintypmax expression"));
    };
    match value(expression, tree, dims)? {
        Value::Pattern(pattern) => Ok(pattern),
        Value::Expression(value) => Ok(BoundPattern {
            items: Items::Value(value),
        }),
    }
}

fn frozen(expr: Expr, dims: &PackedDimensions) -> Converted<Expr> {
    let freeze = |expr| frozen(expr, dims).map(Box::new);
    Ok(match expr {
        Expr::Literal(_) => expr,
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: freeze(*expr)?,
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: freeze(*left)?,
            op,
            right: freeze(*right)?,
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: freeze(*condition)?,
            then_expr: freeze(*then_expr)?,
            else_expr: freeze(*else_expr)?,
        },
        Expr::Call {
            callee: Callee::Subroutine(name),
            args,
            arg_order,
        } if arg_order.is_empty() => {
            let args = args
                .into_iter()
                .map(|arg| {
                    let expr = frozen(
                        arg.ok_or_else(|| {
                            unsupported("default constant function argument in pattern")
                        })?,
                        dims,
                    )?;
                    expr_to_const(expr)
                        .map(Into::into)
                        .ok_or_else(|| unsupported("constant pattern function argument"))
                })
                .collect::<Converted<Vec<crate::ir::ConstExpr>>>()?;
            let literal = const_functions::eval_call_literal(&name, &args, &dims.const_env)
                .ok_or_else(|| unsupported("constant parameter pattern function"))?;
            Expr::Literal(typecheck::format_integral_literal_binary(&literal))
        }
        value => {
            let values = dims
                .parameter_values
                .iter()
                .map(|(n, v)| (n.clone(), v.clone()))
                .collect();
            let value =
                substitute_expr_constants_with_parameter_literals(value, &dims.const_env, &values);
            let folded = fold_const_integral_expr_preserving_mask(value, &dims.const_env);
            if !matches!(folded, Expr::Literal(_)) {
                return Err(unsupported("nonconstant parameter pattern element"));
            }
            folded
        }
    })
}

fn value(
    expr: &sv_parser::Expression,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<Value> {
    if let Some(pattern) = patterns::pattern_expression(expr) {
        value_pattern(pattern, tree, dims)
    } else {
        Ok(Value::Expression(frozen(
            expr_from_expression_with_types(expr, tree, dims)?,
            dims,
        )?))
    }
}

fn value_pattern(
    pattern: &sv_parser::AssignmentPatternExpression,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<Value> {
    if pattern.nodes.0.is_some() {
        return patterns::constant_typed_pattern_value(pattern, tree, dims);
    }
    let integer = |expr: &sv_parser::ConstantExpression| {
        const_expr_from_ref_node_with_env(
            RefNode::ConstantExpression(expr),
            tree,
            &dims.const_env,
            &dims.type_aliases,
        )?
        .and_then(|expr| eval_ast_const_expr(&expr, &dims.const_env))
        .ok_or_else(|| unsupported("nonconstant assignment pattern key or count"))
    };
    let values = |items: Vec<&sv_parser::Expression>| {
        items
            .into_iter()
            .map(|expr| value(expr, tree, dims))
            .collect::<Converted<Vec<_>>>()
    };
    let default_key = |key: &sv_parser::AssignmentPatternKey| match key {
        sv_parser::AssignmentPatternKey::Default(_) => Ok(Key::Default),
        _ => Err(unsupported("assignment pattern type key")),
    };
    let items = match &pattern.nodes.1 {
        sv_parser::AssignmentPattern::List(list) => {
            Items::List(values(list.nodes.0.nodes.1.contents())?)
        }
        sv_parser::AssignmentPattern::Repeat(repeat) => {
            let (count, items) = &repeat.nodes.0.nodes.1;
            let count = usize::try_from(integer(count)?)
                .map_err(|_| unsupported("assignment pattern repetition count"))?;
            let values = values(items.nodes.1.contents())?;
            Items::Repeat(count, values)
        }
        sv_parser::AssignmentPattern::Array(array) => Items::Keyed(
            array
                .nodes
                .0
                .nodes
                .1
                .contents()
                .into_iter()
                .map(|(key, _, expr)| {
                    let key = match key {
                        sv_parser::ArrayPatternKey::ConstantExpression(index) => {
                            Key::Index(integer(index)?)
                        }
                        sv_parser::ArrayPatternKey::AssignmentPatternKey(key) => default_key(key)?,
                    };
                    Ok((key, value(expr, tree, dims)?))
                })
                .collect::<Converted<_>>()?,
        ),
        sv_parser::AssignmentPattern::Structure(structure) => Items::Keyed(
            structure
                .nodes
                .0
                .nodes
                .1
                .contents()
                .into_iter()
                .map(|(key, _, expr)| {
                    let key = match key {
                        sv_parser::StructurePatternKey::MemberIdentifier(member) => Key::Member(
                            identifier_text(RefNode::MemberIdentifier(member), tree)
                                .ok_or_else(|| unsupported("assignment pattern member name"))?,
                        ),
                        sv_parser::StructurePatternKey::AssignmentPatternKey(key) => {
                            default_key(key)?
                        }
                    };
                    Ok((key, value(expr, tree, dims)?))
                })
                .collect::<Converted<_>>()?,
        ),
    };
    Ok(Value::Pattern(BoundPattern { items }))
}

impl Value {
    pub(super) fn list(values: Vec<Self>) -> Self {
        Self::Pattern(BoundPattern {
            items: Items::List(values),
        })
    }
    fn lower(
        &self,
        shape: &VariableDimensions,
        dims: &PackedDimensions,
        default: bool,
    ) -> Converted<Expr> {
        if let Self::Pattern(pattern) = self {
            return pattern.lower(shape, dims);
        }
        let Self::Expression(value) = self else {
            unreachable!()
        };
        if default && let Some((dimension, rest)) = shape.unpacked.split_first() {
            let count = eval_ast_const_expr(&dimension.width, &dims.const_env)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| unsupported("pattern target array size"))?;
            let shape = VariableDimensions {
                unpacked: rest.to_vec(),
                ..shape.clone()
            };
            return Ok(Expr::Concat(vec![self.lower(&shape, dims, true)?; count]));
        }
        if !shape.unpacked.is_empty() {
            return Err(unsupported(
                "scalar assignment pattern item for unpacked array",
            ));
        }
        let width = patterns::shape_width(shape, dims)
            .ok_or_else(|| unsupported("assignment pattern element width"))?;
        let signed =
            expr_signedness(value, &dims.expression_signedness, &dims.functions).unwrap_or(false);
        Ok(Expr::Resize {
            expr: Box::new(value.clone()),
            width,
            signed,
        })
    }
}

impl BoundPattern {
    pub(super) fn lower(
        &self,
        shape: &VariableDimensions,
        dims: &PackedDimensions,
    ) -> Converted<Expr> {
        if let Items::Value(value) = &self.items {
            return Ok(value.clone());
        }
        let eval = |expr| {
            eval_ast_const_expr(expr, &dims.const_env)
                .ok_or_else(|| unsupported("assignment pattern target bounds"))
        };
        let unpacked = !shape.unpacked.is_empty();
        let mut shapes = Vec::new();
        let keys;
        if let Some((dim, rest)) = shape.unpacked.split_first() {
            let left = eval(&dim.left)?;
            let right = eval(&dim.right)?;
            let count = usize::try_from(left.abs_diff(right) + 1)
                .map_err(|_| unsupported("assignment pattern target size"))?;
            shapes = vec![
                VariableDimensions {
                    unpacked: rest.to_vec(),
                    ..shape.clone()
                };
                count
            ];
            keys = (0..count)
                .map(|i| {
                    Key::Index(if left <= right {
                        left + i as i128
                    } else {
                        left - i as i128
                    })
                })
                .collect::<Vec<_>>();
        } else if !shape.members.is_empty() {
            if shape.members.len() > 1 && shape.members.iter().all(|m| m.offset() == 0) {
                return Err(unsupported("assignment pattern for a packed union"));
            }
            keys = shape
                .members
                .iter()
                .map(|m| Key::Member(m.name().into()))
                .collect();
            shapes.extend(
                shape
                    .members
                    .iter()
                    .map(|m| procedural::dimensions_from_type(m.r#type())),
            );
        } else if let Some((dim, rest)) = shape.packed.split_first()
            && !rest.is_empty()
        {
            let left = eval(&dim.left)?;
            let right = eval(&dim.right)?;
            let count = usize::try_from(left.abs_diff(right) + 1)
                .map_err(|_| unsupported("assignment pattern target size"))?;
            shapes = vec![
                VariableDimensions {
                    packed: rest.to_vec(),
                    signed: shape.signed_element_depth == Some(1),
                    signed_element_depth: shape
                        .signed_element_depth
                        .and_then(|n| n.checked_sub(1))
                        .filter(|n| *n > 0),
                    ..shape.clone()
                };
                count
            ];
            keys = (0..count)
                .map(|i| {
                    Key::Index(if left <= right {
                        left + i as i128
                    } else {
                        left - i as i128
                    })
                })
                .collect();
        } else {
            let width = patterns::shape_width(shape, dims)
                .ok_or_else(|| unsupported("pattern target width"))?;
            shapes = vec![
                VariableDimensions {
                    packed: Vec::new(),
                    signed: false,
                    signed_element_depth: None,
                    ..shape.clone()
                };
                width
            ];
            keys = (0..width).rev().map(|i| Key::Index(i as i128)).collect();
        }
        let selected = match &self.items {
            Items::Value(value) => return Ok(value.clone()),
            Items::Repeat(count, values) => {
                if count.checked_mul(values.len()) != Some(shapes.len()) {
                    return Err(unsupported(
                        "assignment pattern repetition size does not match target",
                    ));
                }
                (0..*count)
                    .flat_map(|_| values.iter().map(|v| (v, false)))
                    .collect()
            }
            Items::List(values) => {
                if values.len() != shapes.len() {
                    return Err(unsupported(format!(
                        "assignment pattern with {} items for {} elements",
                        values.len(),
                        shapes.len()
                    )));
                }
                values.iter().map(|v| (v, false)).collect::<Vec<_>>()
            }
            Items::Keyed(items) => {
                for (key, _) in items {
                    if *key != Key::Default && !keys.contains(key) {
                        return Err(unsupported("assignment pattern key outside target"));
                    }
                }
                keys.iter()
                    .map(|key| {
                        items
                            .iter()
                            .rev()
                            .find(|(k, _)| k == key)
                            .map(|(_, v)| (v, false))
                            .or_else(|| {
                                items
                                    .iter()
                                    .rev()
                                    .find(|(k, _)| *k == Key::Default)
                                    .map(|(_, v)| (v, true))
                            })
                            .ok_or_else(|| unsupported("assignment pattern missing element"))
                    })
                    .collect::<Converted<Vec<_>>>()?
            }
        };
        let mut parts = selected
            .into_iter()
            .zip(shapes)
            .map(|((v, default), shape)| v.lower(&shape, dims, default))
            .collect::<Converted<Vec<_>>>()?;
        if unpacked {
            parts.reverse();
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Expr::Concat(parts)
        })
    }
}
