//! Shared semantic runtime lookup table for native array-bound queries.
use super::*;

pub(super) fn bound_query(
    state: &AnalysisState,
    query: ArrayQuery,
    bounds: &[(i128, i128)],
    dimension: Option<Expr>,
    context: &PackedDimensions,
) -> Option<Expr> {
    let integer = |value| Expr::Literal(format_typed_parameter_literal(value, 32, true));
    let env = &context.const_env;
    let dimensions = Some(context);
    let value = |&(left, right): &(i128, i128)| {
        integer(match query {
            ArrayQuery::Left => left,
            ArrayQuery::Right => right,
            ArrayQuery::Low => left.min(right),
            ArrayQuery::High => left.max(right),
            ArrayQuery::Increment => {
                if left >= right {
                    1
                } else {
                    -1
                }
            }
        })
    };
    let dimension = dimension.unwrap_or_else(|| integer(1));
    if let Some(index) = expr_to_const(state, dimension.clone())
        .and_then(|expr| eval_ast_const_expr(state, &expr, env))
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
        .and_then(|context| expr_static_width(state, &dimension, context))
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
