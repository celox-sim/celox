//! Assignment patterns (IEEE 1800-2023 10.9): `'{a, b}`, `'{i: a, default:
//! b}`, `'{n{a}}`, and typed `T'{...}`, for unpacked arrays, packed structures
//! and packed vectors. A pattern lowers to the concatenation of its elements
//! in the flat bit layout of the target, element 0 of an unpacked dimension
//! (its left bound) in the least significant bits.

use super::*;

mod binding;
pub(super) use binding::expression as native_expression;
pub(super) use binding::{bind_native, constant_native, typed_shape};

fn eval(state: &AnalysisState, expr: &ConstExpr, dims: &PackedDimensions) -> Option<i128> {
    eval_ast_const_expr(state, expr, &dims.const_env)
}

pub(super) fn shape_width(
    state: &AnalysisState,
    shape: &VariableDimensions,
    dims: &PackedDimensions,
) -> Option<usize> {
    let mut width = 1usize;
    for dimension in &shape.unpacked {
        width = width.checked_mul(usize::try_from(eval(state, &dimension.width, dims)?).ok()?)?;
    }
    for dimension in &shape.packed {
        width = width.checked_mul(usize::try_from(eval(state, &dimension.width, dims)?).ok()?)?;
    }
    Some(width)
}

fn constant_pattern_value(
    state: &AnalysisState,
    value: Expr,
    shape: &VariableDimensions,
    dims: &PackedDimensions,
) -> Converted<bound_patterns::Value> {
    let values = dims
        .parameter_values
        .iter()
        .filter(|(name, _)| !dims.const_env.contains_key(*name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let value = substitute_expr_idents(value, &dims.array_parameter_values);
    let value =
        substitute_expr_constants_with_parameter_literals(state, value, &dims.const_env, &values);
    let Expr::Literal(value) =
        fold_const_integral_expr_preserving_mask(state, value, &dims.const_env)
    else {
        return Err(unsupported("nonconstant typed parameter pattern"));
    };
    let value = typecheck::parse_integral_literal(&value)
        .ok_or_else(|| unsupported("typed parameter pattern value"))?;
    pattern_value(state, &value, shape, 0, dims)
}

fn pattern_value(
    state: &AnalysisState,
    value: &typecheck::IntegralLiteral,
    shape: &VariableDimensions,
    offset: usize,
    dims: &PackedDimensions,
) -> Converted<bound_patterns::Value> {
    let mut parts = Vec::new();
    if let Some((dimension, rest)) = shape.unpacked.split_first() {
        let (_, _, count) = dimension_bounds(state, &dimension.left, &dimension.right, dims)?;
        let element = VariableDimensions {
            declared: type_names::Declared::default(),
            container: None,
            real: false,
            string: false,
            class: None,
            unpacked: rest.to_vec(),
            ..shape.clone()
        };
        let width = shape_width(state, &element, dims)
            .ok_or_else(|| unsupported("typed pattern element width"))?;
        for position in 0..count {
            parts.push(pattern_value(
                state,
                value,
                &element,
                offset + position * width,
                dims,
            )?);
        }
    } else {
        let width = shape_width(state, shape, dims)
            .ok_or_else(|| unsupported("typed pattern value width"))?;
        let mask = (num_bigint::BigUint::from(1u8) << width) - num_bigint::BigUint::from(1u8);
        let mut literal = typecheck::IntegralLiteral {
            value: (&value.value >> offset) & &mask,
            mask: (&value.mask >> offset) & &mask,
            width,
            signed: shape.signed,
        };
        if shape.is_2state {
            literal.value ^= &literal.value & &literal.mask;
            literal.mask = num_bigint::BigUint::default();
        }
        return Ok(bound_patterns::Value::Expression {
            expr: Expr::Literal(typecheck::format_integral_literal_binary(&literal)),
            signed: Some(shape.signed),
            value_type: bound_patterns::ValueType::Known(shape.pattern_type(state, dims)),
        });
    }
    Ok(bound_patterns::Value::list(parts))
}

fn dimension_bounds(
    state: &AnalysisState,
    left: &ConstExpr,
    right: &ConstExpr,
    dims: &PackedDimensions,
) -> Converted<(i128, i128, usize)> {
    let bounds = || unsupported("assignment pattern target bounds");
    let left = eval(state, left, dims).ok_or_else(bounds)?;
    let right = eval(state, right, dims).ok_or_else(bounds)?;
    let count = usize::try_from(left.abs_diff(right))
        .ok()
        .and_then(|count| count.checked_add(1))
        .ok_or_else(bounds)?;
    Ok((left, right, count))
}

/// Apply the assignment context of a selected pattern element. A default
/// descends through subarrays and structures; state conversion happens at
/// the leaf so mixed bit/logic aggregates preserve their member types.
pub(super) fn assignment_value(
    state: &AnalysisState,
    value: &Expr,
    signed: bool,
    value_type: Option<&Type>,
    shape: &VariableDimensions,
    dims: &PackedDimensions,
    default: bool,
) -> Converted<Expr> {
    let default = default
        && !value_type.is_some_and(|ty| shape.pattern_type(state, dims).as_ref() == Some(ty));
    if default {
        let array = if let Some((dim, rest)) = shape.unpacked.split_first() {
            Some((
                &dim.width,
                VariableDimensions {
                    declared: type_names::Declared::default(),
                    container: None,
                    unpacked: rest.to_vec(),
                    ..shape.clone()
                },
            ))
        } else if let Some((dim, rest)) = shape.packed.split_first()
            && !rest.is_empty()
        {
            Some((
                &dim.width,
                VariableDimensions {
                    declared: type_names::Declared::default(),
                    container: None,
                    packed: rest.to_vec(),
                    signed: shape.signed_element_depth == Some(1),
                    signed_element_depth: shape
                        .signed_element_depth
                        .and_then(|n| n.checked_sub(1))
                        .filter(|n| *n > 0),
                    ..shape.clone()
                },
            ))
        } else {
            None
        };
        if let Some((width, element)) = array {
            let count = eval_ast_const_expr(state, width, &dims.const_env)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| unsupported("pattern target array size"))?;
            return Ok(Expr::Concat(vec![
                assignment_value(
                    state, value, signed, value_type, &element, dims, true
                )?;
                count
            ]));
        }
        if !shape.members.is_empty() {
            if shape
                .members
                .iter()
                .any(packed_structs::PackedMember::is_tagged)
                || (shape.members.len() > 1
                    && shape.members.iter().all(|member| member.offset() == 0))
            {
                return Err(unsupported("assignment pattern for a packed union"));
            }
            return Ok(Expr::Concat(
                shape
                    .members
                    .iter()
                    .map(|member| {
                        assignment_value(
                            state,
                            value,
                            signed,
                            value_type,
                            &procedural::dimensions_from_type(member.r#type()),
                            dims,
                            true,
                        )
                    })
                    .collect::<Converted<_>>()?,
            ));
        }
    }
    let actual = value_type
        .map(procedural::dimensions_from_type)
        .and_then(|shape| array_compatibility::unpacked_array_type(state, &shape, dims));
    let target = array_compatibility::unpacked_array_type(state, shape, dims);
    match (actual, target) {
        (Some(actual), Some(target)) if !actual.is_assignment_compatible_with(&target) => {
            return Err(AnalyzerError::IncompatibleUnpackedArray {
                context: "assignment pattern element".into(),
                actual: Box::new(actual),
                target: Box::new(target),
            });
        }
        (Some(_), None) => {
            return Err(unsupported(
                "unpacked array assignment pattern item for scalar",
            ));
        }
        (None, Some(_)) => {
            return Err(unsupported(
                "scalar assignment pattern item for unpacked array",
            ));
        }
        _ => {}
    }
    let width = shape_width(state, shape, dims)
        .ok_or_else(|| unsupported("assignment pattern element width"))?;
    let resized = Expr::Resize {
        expr: Box::new(value.clone()),
        width,
        signed,
    };
    Ok(if shape.is_2state {
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(resized),
        }
    } else {
        resized
    })
}
