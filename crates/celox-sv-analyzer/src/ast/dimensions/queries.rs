//! Fixed-size array queries (IEEE 1800-2023 20.7).

use super::*;
use std::borrow::Cow;

pub(in crate::ast) fn array_query_call(
    call: &sv_parser::SystemTfCall,
    tree: &SyntaxTree,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
    dimensions: Option<&PackedDimensions>,
) -> Option<Expr> {
    let (name, _) = system_tf_call_parts(call, tree)?;
    let (bounds, unpacked, dimension) = match call {
        sv_parser::SystemTfCall::ArgDataType(call) => {
            let (data_type, dimension) = &call.nodes.1.nodes.1;
            let ty = match data_type {
                sv_parser::DataType::Type(ty) => {
                    let name = identifier_text(RefNode::TypeIdentifier(&ty.nodes.1), tree)?;
                    aliases.get(&name).cloned()
                }
                _ => type_from_ref_node_with_env(RefNode::DataType(data_type), tree, env, aliases),
            }?;
            let (bounds, unpacked) = type_bounds(&ty, env)?;
            let context = PackedDimensions::new(HashMap::default(), env, aliases);
            let dimension = match dimension {
                Some((_, expr)) => Some(
                    expr_from_expression_with_types(expr, tree, dimensions.unwrap_or(&context))
                        .ok()?,
                ),
                None => None,
            };
            (bounds, unpacked, dimension)
        }
        sv_parser::SystemTfCall::ArgExpression(call) => {
            let args = call.nodes.1.nodes.1.0.contents();
            let argument = args.first()?.as_ref()?;
            let context = if let Some(dimensions) = dimensions.filter(|d| d.scope_types_complete) {
                Cow::Borrowed(dimensions)
            } else {
                let mut discovered =
                    containing_packed_dimensions(RefNode::Expression(argument), tree, env, aliases)
                        .unwrap_or_else(|| PackedDimensions::new(HashMap::default(), env, aliases));
                if let Some(dimensions) = dimensions {
                    discovered.extend(
                        dimensions
                            .iter()
                            .map(|(name, ty)| (name.clone(), ty.clone())),
                    );
                }
                Cow::Owned(discovered)
            };
            let (bounds, unpacked) = expression_bounds(argument, tree, env, aliases, &context)?;
            let dimension = match args.get(1) {
                Some(expr) => {
                    Some(expr_from_expression_with_types(expr.as_ref()?, tree, &context).ok()?)
                }
                None => None,
            };
            (bounds, unpacked, dimension)
        }
        _ => return None,
    };
    let integer = |value| Expr::Literal(format_typed_parameter_literal(value, 32, true));
    match name {
        "$dimensions" => return Some(integer(bounds.len() as i128)),
        "$unpacked_dimensions" => return Some(integer(unpacked as i128)),
        _ => {}
    }
    let value = |&(left, right): &(i128, i128)| {
        integer(match name {
            "$left" => left,
            "$right" => right,
            "$low" => left.min(right),
            "$high" => left.max(right),
            "$increment" => {
                if left >= right {
                    1
                } else {
                    -1
                }
            }
            _ => unreachable!(),
        })
    };
    let dimension = dimension.unwrap_or_else(|| integer(1));
    if let Some(index) =
        expr_to_const(dimension.clone()).and_then(|expr| eval_ast_const_expr(&expr, env))
    {
        return Some(
            usize::try_from(index)
                .ok()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| bounds.get(index))
                .map(value)
                .unwrap_or_else(|| Expr::Literal("32'bx".to_string())),
        );
    }
    // Arithmetic-shift a signed constant table whose top word is X, then
    // take its low integer. Out-of-range or unknown shifts return X through
    // the sign bit. The dimension appears once, preserving function effects.
    // Widen the index so a large dimension cannot wrap into a valid slot.
    let index_width = dimensions
        .and_then(|context| expr_static_width(&dimension, context))
        .unwrap_or(32)
        .max(32)
        .checked_add(6)?;
    let offset = Expr::Binary {
        left: Box::new(Expr::Binary {
            left: Box::new(Expr::Resize {
                expr: Box::new(dimension),
                width: index_width,
                signed: false,
            }),
            op: BinaryOp::Sub,
            right: Box::new(Expr::Literal(format!("{index_width}'d1"))),
        }),
        op: BinaryOp::Mul,
        right: Box::new(Expr::Literal(format!("{index_width}'d32"))),
    };
    let table_width = bounds
        .len()
        .checked_add(1)?
        .checked_mul(32)?
        .checked_next_multiple_of(64)?;
    let padding = table_width.checked_sub(bounds.len().checked_mul(32)?)?;
    let table = Expr::Resize {
        expr: Box::new(Expr::Concat(
            std::iter::once(Expr::Literal(format!("{padding}'bx")))
                .chain(bounds.iter().rev().map(value))
                .collect(),
        )),
        width: table_width,
        signed: true,
    };
    Some(Expr::Resize {
        expr: Box::new(Expr::Binary {
            left: Box::new(table),
            op: BinaryOp::Sar,
            right: Box::new(offset),
        }),
        width: 32,
        signed: true,
    })
}

type Bounds = (Vec<(i128, i128)>, usize);

fn type_bounds(ty: &Type, env: &HashMap<String, i128>) -> Option<Bounds> {
    let bounds = ty
        .unpacked_ranges()
        .iter()
        .map(|range| (range.left(), range.right()))
        .chain(
            ty.packed_ranges()
                .iter()
                .map(|range| (range.left(), range.right())),
        )
        .map(|(left, right)| {
            Some((
                eval_ast_const_expr(left, env)?,
                eval_ast_const_expr(right, env)?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some((bounds, ty.unpacked_ranges().len()))
}

fn expression_bounds(
    argument: &sv_parser::Expression,
    tree: &SyntaxTree,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
    context: &PackedDimensions,
) -> Option<Bounds> {
    if let sv_parser::Expression::Primary(primary) = argument {
        if let sv_parser::Primary::MintypmaxExpression(group) = &**primary
            && let sv_parser::MintypmaxExpression::Expression(expr) = &group.nodes.0.nodes.1
        {
            return expression_bounds(expr, tree, env, aliases, context);
        }
        if let sv_parser::Primary::Hierarchical(id) = &**primary {
            let name = identifier_text(RefNode::HierarchicalIdentifier(&id.nodes.1), tree)?;
            if let Some(shape) = context.get(&name) {
                let mut bounds = shape
                    .unpacked
                    .iter()
                    .map(|d| (&d.left, &d.right))
                    .chain(shape.packed.iter().map(|d| (&d.left, &d.right)))
                    .map(|(l, r)| {
                        Some((eval_ast_const_expr(l, env)?, eval_ast_const_expr(r, env)?))
                    })
                    .collect::<Option<Vec<_>>>()?;
                let removed = id.nodes.2.nodes.1.nodes.0.len();
                if removed > bounds.len() {
                    return None;
                }
                bounds.drain(..removed);
                let unpacked = shape.unpacked.len().saturating_sub(removed);
                if id.nodes.2.nodes.2.is_some() {
                    let width = selected_expression_first_dimension_width(argument, tree, context)?;
                    return Some((vec![(width as i128 - 1, 0)], 0));
                }
                return Some((bounds, unpacked));
            }
            if let Some(ty) = aliases.get(&name) {
                return type_bounds(ty, env);
            }
            if let Some(count) = env.get(&parameter_dimensions_marker(&name)) {
                if id.nodes.2.nodes.2.is_some() {
                    let expression =
                        expr_from_expression_with_types(argument, tree, context).ok()?;
                    let width = expr_static_width(&expression, context)?;
                    return Some((vec![(width as i128 - 1, 0)], 0));
                }
                let count = usize::try_from(*count).ok()?;
                let removed = id.nodes.2.nodes.1.nodes.0.len();
                if removed > count {
                    return None;
                }
                let bounds = (removed..count)
                    .map(|index| {
                        Some((
                            *env.get(&parameter_dimension_marker(&name, index, "left"))?,
                            *env.get(&parameter_dimension_marker(&name, index, "right"))?,
                        ))
                    })
                    .collect::<Option<Vec<_>>>()?;
                return Some((bounds, 0));
            }
            if let Some(ty) = parameter_type_from_const_env(env, &name) {
                return Some((vec![(ty.width as i128 - 1, 0)], 0));
            }
        }
    }
    let expr = expr_from_expression_with_types(argument, tree, context).ok()?;
    let width = expr_static_width(&expr, context)?;
    Some((
        if width > 1 {
            vec![(width as i128 - 1, 0)]
        } else {
            Vec::new()
        },
        0,
    ))
}
