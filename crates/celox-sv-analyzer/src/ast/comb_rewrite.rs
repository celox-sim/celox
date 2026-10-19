//! Static widths of expressions and whole-vector lvalues.

use super::*;

pub(super) fn expr_static_width(
    state: &AnalysisState,
    expr: &Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<usize> {
    match expr {
        Expr::Ident(name) => {
            lvalue_expr_type(state, &LValue::Ident(name.clone()), packed_dimensions)
                .map(|r#type| r#type.width)
                .or_else(|| variable_size_function_width(&packed_dimensions.const_env, name, false))
                .or_else(|| {
                    parameter_type_from_const_env(&packed_dimensions.const_env, name)
                        .map(|r#type| r#type.width)
                })
        }
        Expr::Literal(literal) => {
            typecheck::integral_literal_type_metadata(literal).map(|(width, _)| width)
        }
        Expr::Select { msb, lsb, .. } => typecheck::select_range_width(
            &msb.clone().into(),
            &lsb.clone().into(),
            &packed_dimensions.const_env,
        ),
        Expr::Concat(parts) => parts.iter().try_fold(0usize, |width, part| {
            width.checked_add(expr_static_width(state, part, packed_dimensions)?)
        }),
        Expr::RepeatConcat { count, parts } => {
            let count = eval_ast_const_expr(state, count, &packed_dimensions.const_env)?;
            let count = usize::try_from(count).ok()?;
            let width = parts.iter().try_fold(0usize, |width, part| {
                width.checked_add(expr_static_width(state, part, packed_dimensions)?)
            })?;
            width.checked_mul(count)
        }
        Expr::Resize { width, .. } => Some(*width),
        Expr::Unary { op, expr } => {
            if matches!(
                op,
                UnaryOp::LogicNot | UnaryOp::RedAnd | UnaryOp::RedOr | UnaryOp::RedXor
            ) {
                Some(1)
            } else {
                expr_static_width(state, expr, packed_dimensions)
            }
        }
        Expr::Binary { left, op, right } => {
            // A logical result has a fixed width, but a type query must still
            // resolve the types of both original operands.
            if packed_dimensions.type_query && matches!(op, BinaryOp::LogicAnd | BinaryOp::LogicOr)
            {
                expr_static_width(state, left, packed_dimensions)?;
                expr_static_width(state, right, packed_dimensions)?;
            }
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
                Some(1)
            } else if matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
                expr_static_width(state, left, packed_dimensions)
            } else {
                Some(
                    expr_static_width(state, left, packed_dimensions)?.max(expr_static_width(
                        state,
                        right,
                        packed_dimensions,
                    )?),
                )
            }
        }
        Expr::Inside { .. } => Some(1),
        Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => Some(
            expr_static_width(state, then_expr, packed_dimensions)?.max(expr_static_width(
                state,
                else_expr,
                packed_dimensions,
            )?),
        ),
        Expr::Call {
            callee: Callee::System(function),
            ..
        } => function.result_type().map(|(width, _)| width),
        Expr::Call {
            callee: Callee::Subroutine(name),
            ..
        } => packed_dimensions
            .function_return_types
            .get(name)
            .and_then(|metadata| metadata.width),
    }
}

pub(super) fn whole_packed_lvalue(
    name: &str,
    packed_dimensions: &PackedDimensions,
) -> Option<LValue> {
    let dimensions = packed_dimensions.get(name)?;
    let (msb, lsb) = if dimensions.unpacked.is_empty()
        && dimensions.packed.len() == 1
        && !dimensions.packed[0].normalize_single
    {
        (
            dimensions.packed[0].left.clone(),
            dimensions.packed[0].right.clone(),
        )
    } else {
        let width = product_expr(
            &dimensions
                .packed
                .iter()
                .map(|dimension| dimension.width.clone())
                .chain(
                    dimensions
                        .unpacked
                        .iter()
                        .map(|dimension| dimension.width.clone()),
                )
                .collect::<Vec<_>>(),
        );
        (
            ConstExpr::Binary {
                left: Box::new(width),
                op: BinaryOp::Sub,
                right: Box::new(ConstExpr::Literal("1".to_string())),
            },
            ConstExpr::Literal("0".to_string()),
        )
    };
    Some(LValue::Select {
        name: name.to_string(),
        msb,
        lsb,
        signed: dimensions.signed,
        array_slice_width: None,
        array_slice_reversed: false,
        is_2state: false,
    })
}
