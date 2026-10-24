//! Assignment-pattern bindings retain structure, not generated source. Names,
//! keys and repetition counts bind in the caller; element sizing happens in
//! the receiving declaration's assignment context (IEEE 1800-2023 10.9).
use super::*;
mod binding;
mod ports;
pub(super) use binding::{Kind as NativeKind, prepare_native};
pub(in crate::ast) use ports::{ValueType, from_port, native_value_type};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BoundPattern {
    items: Items,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Items {
    Value(Expr),
    ArrayValue {
        value: Expr,
        ty: typecheck::UnpackedArrayType,
    },
    List(Vec<Value>),
    Repeat(usize, Vec<Value>),
    Keyed(Vec<(Key, Value)>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Key {
    Index(i128),
    Member(String),
    Name { name: String, index: Option<i128> },
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Value {
    Expression {
        expr: Expr,
        signed: Option<bool>,
        value_type: ports::ValueType,
    },
    Pattern(BoundPattern),
}

pub(super) fn from_param(
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<BoundPattern> {
    match binding::value(root, tree, dims)? {
        Value::Pattern(pattern) => Ok(pattern),
        Value::Expression { expr, .. } => Ok(BoundPattern {
            items: Items::Value(expr),
        }),
    }
}

pub(super) fn from_array_parameter(
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
    shape: &VariableDimensions,
) -> Converted<BoundPattern> {
    let ty = array_compatibility::unpacked_array_type(&tree.state, shape, dims)
        .ok_or_else(|| unsupported("array parameter argument type"))?;
    let value = substitute_expr_idents(
        expression_binding::bind(root, tree, dims)?,
        &dims.array_parameter_values,
    );
    let value = frozen(&tree.state, value, dims)?;
    Ok(BoundPattern {
        items: Items::ArrayValue { value, ty },
    })
}

fn frozen(state: &AnalysisState, expr: Expr, dims: &PackedDimensions) -> Converted<Expr> {
    let freeze = |expr| frozen(state, expr, dims).map(Box::new);
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
                        state,
                        arg.ok_or_else(|| {
                            unsupported("default constant function argument in pattern")
                        })?,
                        dims,
                    )?;
                    expr_to_const(state, expr)
                        .map(Into::into)
                        .ok_or_else(|| unsupported("constant pattern function argument"))
                })
                .collect::<Converted<Vec<crate::ir::ConstExpr>>>()?;
            let literal = const_functions::eval_call_literal(
                &state.const_functions,
                &name,
                &args,
                &dims.const_env,
            )
            .ok_or_else(|| unsupported("constant parameter pattern function"))?;
            Expr::Literal(typecheck::format_integral_literal_binary(&literal))
        }
        value => {
            let value = substitute_expr_idents(value, &dims.array_parameter_values);
            let values = dims
                .parameter_values
                .iter()
                .map(|(n, v)| (n.clone(), v.clone()))
                .collect();
            let value = substitute_expr_constants_with_parameter_literals(
                state,
                value,
                &dims.const_env,
                &values,
            );
            let folded = fold_const_integral_expr_preserving_mask(state, value, &dims.const_env);
            if !matches!(folded, Expr::Literal(_)) {
                return Err(unsupported("nonconstant parameter pattern element"));
            }
            folded
        }
    })
}

impl Value {
    pub(super) fn list(values: Vec<Self>) -> Self {
        Self::Pattern(BoundPattern {
            items: Items::List(values),
        })
    }
    fn lower(
        &self,
        state: &AnalysisState,
        shape: &VariableDimensions,
        dims: &PackedDimensions,
        default: bool,
    ) -> Converted<Expr> {
        if let Self::Pattern(pattern) = self {
            return pattern.lower(state, shape, dims);
        }
        let (value, signed, value_type) = match self {
            Self::Expression {
                expr,
                signed,
                value_type,
            } => (
                expr,
                signed.ok_or_else(|| unsupported("assignment pattern operand signedness"))?,
                value_type.known(),
            ),
            Self::Pattern(_) => unreachable!(),
        };
        patterns::assignment_value(state, value, signed, value_type, shape, dims, default)
    }
}

impl BoundPattern {
    pub(super) fn lower(
        &self,
        state: &AnalysisState,
        shape: &VariableDimensions,
        dims: &PackedDimensions,
    ) -> Converted<Expr> {
        if let Items::ArrayValue { value, ty } = &self.items {
            let target = array_compatibility::unpacked_array_type(state, shape, dims)
                .ok_or_else(|| unsupported("unpacked array parameter supplied to a scalar"))?;
            if !ty.is_assignment_compatible_with(&target) {
                return Err(AnalyzerError::IncompatibleUnpackedArray {
                    context: "module parameter override".into(),
                    actual: ty.clone(),
                    target,
                });
            }
            return Ok(value.clone());
        }
        if let Items::Value(value) = &self.items {
            return Ok(value.clone());
        }
        let eval = |expr| {
            eval_ast_const_expr(state, expr, &dims.const_env)
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
                    real: false,
                    string: false,
                    class: None,
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
        } else if let Some((dim, rest)) = shape.packed.split_first()
            && !rest.is_empty()
        {
            let left = eval(&dim.left)?;
            let right = eval(&dim.right)?;
            let count = usize::try_from(left.abs_diff(right) + 1)
                .map_err(|_| unsupported("assignment pattern target size"))?;
            shapes = vec![
                VariableDimensions {
                    real: false,
                    string: false,
                    class: None,
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
        } else if !shape.members.is_empty() {
            if shape
                .members
                .iter()
                .any(packed_structs::PackedMember::is_tagged)
                || (shape.members.len() > 1 && shape.members.iter().all(|m| m.offset() == 0))
            {
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
        } else {
            let width = patterns::shape_width(state, shape, dims)
                .ok_or_else(|| unsupported("pattern target width"))?;
            shapes = vec![
                VariableDimensions {
                    real: false,
                    string: false,
                    class: None,
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
            Items::Value(value) | Items::ArrayValue { value, .. } => return Ok(value.clone()),
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
                let structure = shape.unpacked.is_empty()
                    && shape.packed.len() <= 1
                    && !shape.members.is_empty();
                let items = items
                    .iter()
                    .map(|(key, value)| {
                        let key = match key {
                            Key::Name { name, .. } if structure => Key::Member(name.clone()),
                            Key::Name { index, .. } => Key::Index(index.ok_or_else(|| {
                                unsupported("nonconstant assignment pattern index")
                            })?),
                            key => key.clone(),
                        };
                        Ok((key, value))
                    })
                    .collect::<Converted<Vec<_>>>()?;
                let mut seen = HashSet::default();
                for (key, _) in &items {
                    if !seen.insert(key) {
                        return Err(unsupported("duplicate assignment pattern key"));
                    }
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
                            .map(|(_, v)| (*v, false))
                            .or_else(|| {
                                items
                                    .iter()
                                    .rev()
                                    .find(|(k, _)| *k == Key::Default)
                                    .map(|(_, v)| (*v, true))
                            })
                            .ok_or_else(|| unsupported("assignment pattern missing element"))
                    })
                    .collect::<Converted<Vec<_>>>()?
            }
        };
        let mut parts = selected
            .into_iter()
            .zip(shapes)
            .map(|((v, default), shape)| v.lower(state, &shape, dims, default))
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
