//! Integral unpacked array reductions (IEEE 1800-2023 7.12.3).
use super::*;
use celox_sv_syntax::{Declarations, ExprId, ExprKind};
use system_functions::ContainerOp;

pub(super) fn bind(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<(Expr, VariableDimensions)>> {
    let callee = match &native.expression(id).kind {
        ExprKind::Call { expression, .. } => *expression,
        _ => id,
    };
    let ExprKind::Member {
        expression: base,
        separator,
        name,
    } = &native.expression(callee).kind
    else {
        return Ok(None);
    };
    if separator != "." {
        return Ok(None);
    }
    let op = match name.spelling.as_str() {
        "sum" => ContainerOp::ReduceSum,
        "product" => ContainerOp::ReduceProduct,
        "and" => ContainerOp::ReduceAnd,
        "or" => ContainerOp::ReduceOr,
        "xor" => ContainerOp::ReduceXor,
        _ => return Ok(None),
    };
    let ExprKind::Name(receiver) = &native
        .expression(expression_binding::ungroup(*base, native))
        .kind
    else {
        return Ok(None);
    };
    let receiver = instances::normalize_identifier(&receiver.spelling);
    let Some(shape) = dims.get(&receiver) else {
        return Ok(None);
    };
    if shape.container.is_none() && shape.unpacked.is_empty() {
        return Ok(None);
    }
    if let ExprKind::Call { arguments, .. } = &native.expression(id).kind
        && !arguments.is_empty()
    {
        return Err(unsupported(
            "array reduction arguments require a with expression",
        ));
    }
    if let Some(container) = shape.container.as_deref() {
        if container.layout.handle.is_some() {
            return Err(unsupported("array reduction requires integral elements"));
        }
        return Ok(Some((
            containers::call(op, container.layout, vec![Expr::Ident(receiver)]),
            procedural::dimensions_from_type(&container.element),
        )));
    }
    if shape.unpacked.len() != 1 || shape.real || shape.string || shape.class.is_some() {
        return Err(unsupported(
            "array reduction requires one-dimensional integral elements",
        ));
    }
    let width = shape
        .packed
        .iter()
        .try_fold(1usize, |width, dim| {
            width.checked_mul(
                usize::try_from(eval_ast_const_expr(
                    &tree.state,
                    &dim.width,
                    &dims.const_env,
                )?)
                .ok()?,
            )
        })
        .ok_or_else(|| unsupported("array reduction element width"))?;
    let count = usize::try_from(
        eval_ast_const_expr(&tree.state, &shape.unpacked[0].width, &dims.const_env)
            .ok_or_else(|| unsupported("array reduction length"))?,
    )
    .map_err(|_| unsupported("array reduction length"))?;
    let elements = (0..count).map(|index| Expr::Select {
        expr: Box::new(Expr::Ident(receiver.clone())),
        msb: ConstExpr::Literal(((index + 1) * width - 1).to_string()),
        lsb: ConstExpr::Literal((index * width).to_string()),
        signed: shape.signed,
    });
    let identity = match op {
        ContainerOp::ReduceProduct => format!("{width}'d1"),
        ContainerOp::ReduceAnd => format!("{width}'b{}", "1".repeat(width)),
        _ => format!("{width}'d0"),
    };
    let mut level = std::iter::once(Expr::Literal(identity))
        .chain(elements)
        .collect::<Vec<_>>();
    // A balanced tree keeps large fixed arrays from making expression depth
    // proportional to the number of elements.
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut values = level.into_iter();
        while let Some(left) = values.next() {
            next.push(if let Some(right) = values.next() {
                Expr::Binary {
                    left: Box::new(left),
                    op: op.reduction().expect("reduction method").into(),
                    right: Box::new(right),
                }
            } else {
                left
            });
        }
        level = next;
    }
    let result = level.pop().expect("reduction identity");
    // A method result is self-determined: assignment context must not widen
    // intermediate additions or products past the element type.
    let mut result_shape = shape.clone();
    result_shape.unpacked.clear();
    Ok(Some((
        Expr::Resize {
            expr: Box::new(result),
            width,
            signed: shape.signed,
        },
        result_shape,
    )))
}
