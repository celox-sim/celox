//! Floating-point semantics shared by RTL and testbench lowering.
use celox_design::{FloatBinaryOp, FloatPrecision, FloatUnaryOp, UnaryOp};
use veryl_analyzer::ir::{Expression, Factor, Op, Type, TypeKind, ValueVariant};

pub(crate) fn precision(ty: &Type) -> Option<FloatPrecision> {
    match ty.kind {
        TypeKind::F32 => Some(FloatPrecision::F32),
        TypeKind::F64 => Some(FloatPrecision::F64),
        _ => None,
    }
}

/// Veryl 0.22 reports unary and ternary float results as bit types.
/// Recover their numeric type before choosing operations or conversions.
pub(crate) fn type_of(expr: &Expression) -> &Type {
    // A reported f64 is already the widest numeric type. Other results may
    // hide a float behind the analyzer's unary/conditional bit-type result.
    if matches!(
        precision(&expr.comptime().r#type),
        Some(FloatPrecision::F64)
    ) {
        return &expr.comptime().r#type;
    }
    if !matches!(
        expr,
        Expression::Unary(Op::Add | Op::Sub, _, _) | Expression::Ternary(..)
    ) && !matches!(
        expr,
        Expression::Binary(
            _,
            Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Rem | Op::Pow,
            _,
            _
        )
    ) {
        return &expr.comptime().r#type;
    }
    let mut pending = vec![expr];
    let mut float = None;
    while let Some(node) = pending.pop() {
        let reported = &node.comptime().r#type;
        match precision(reported) {
            Some(FloatPrecision::F64) => return reported,
            Some(FloatPrecision::F32) => float = Some(reported),
            None => (),
        }
        match node {
            Expression::Unary(Op::Add | Op::Sub, inner, _) => pending.push(inner),
            Expression::Binary(
                left,
                Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Rem | Op::Pow,
                right,
                _,
            ) => {
                pending.push(right);
                pending.push(left);
            }
            Expression::Ternary(_, left, right, _) => {
                pending.push(right);
                pending.push(left);
            }
            // Comparisons and explicit casts are numeric type boundaries.
            _ => (),
        }
    }
    float.unwrap_or(&expr.comptime().r#type)
}

/// Cached analyzer values involving floats can contain integer evaluation of
/// a unary operand. Keep the original expression's numeric operations instead.
pub(crate) fn needs_numeric_lowering(expr: &Expression) -> bool {
    if precision(type_of(expr)).is_some() {
        return !matches!(expr, Expression::Term(_));
    }
    match expr {
        Expression::Unary(_, inner, _) => precision(type_of(inner)).is_some(),
        Expression::Binary(left, _, right, _) => {
            precision(type_of(left)).is_some() || precision(type_of(right)).is_some()
        }
        _ => false,
    }
}

pub(crate) fn binary(op: Op) -> Option<FloatBinaryOp> {
    Some(match op {
        Op::Add => FloatBinaryOp::Add,
        Op::Sub => FloatBinaryOp::Sub,
        Op::Mul => FloatBinaryOp::Mul,
        Op::Div => FloatBinaryOp::Div,
        Op::Rem => FloatBinaryOp::Rem,
        Op::Pow => FloatBinaryOp::Pow,
        Op::Eq => FloatBinaryOp::Eq,
        Op::Ne => FloatBinaryOp::Ne,
        Op::Less => FloatBinaryOp::Lt,
        Op::LessEq => FloatBinaryOp::Le,
        Op::Greater => FloatBinaryOp::Gt,
        Op::GreaterEq => FloatBinaryOp::Ge,
        _ => return None,
    })
}

pub(crate) fn conversion(source: &Type, target: &Type) -> Option<UnaryOp> {
    let source_precision = precision(source);
    let target_precision = precision(target);
    let (precision, op) = match (source_precision, target_precision) {
        (Some(from), Some(to)) if from != to => (from, FloatUnaryOp::Convert { to }),
        (Some(from), None) => (
            from,
            if target.signed {
                FloatUnaryOp::ToSigned {
                    width: target.total_width()?,
                    round: true,
                }
            } else {
                FloatUnaryOp::ToUnsigned {
                    width: target.total_width()?,
                    round: true,
                }
            },
        ),
        (None, Some(to)) => (
            to,
            if source.signed {
                FloatUnaryOp::FromSigned {
                    width: source.total_width()?,
                }
            } else {
                FloatUnaryOp::FromUnsigned {
                    width: source.total_width()?,
                }
            },
        ),
        _ => return None,
    };
    Some(UnaryOp::Float { op, precision })
}

/// Insert a numeric conversion without asking the analyzer to reinterpret the
/// source's IEEE payload as an integer or to fold a synthetic expression.
pub(crate) fn converted<'a>(
    expr: &'a Expression,
    target: &Type,
) -> std::borrow::Cow<'a, Expression> {
    if conversion(type_of(expr), target).is_none() {
        return std::borrow::Cow::Borrowed(expr);
    }
    let mut ct = expr.comptime().clone();
    ct.r#type = target.clone();
    ct.is_const = false;
    let mut target_ct = ct.clone();
    target_ct.value = ValueVariant::Type(target.clone());
    std::borrow::Cow::Owned(Expression::Binary(
        Box::new(expr.clone()),
        Op::As,
        Box::new(Expression::Term(Box::new(Factor::Value(target_ct)))),
        Box::new(ct),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use veryl_analyzer::ir::{Comptime, Shape};

    #[test]
    fn recovers_float_types_in_deep_arithmetic_without_recursion() {
        let mut float_type = Type::new(TypeKind::F32);
        float_type.set_concrete_width(Shape::new(vec![Some(32)]));
        let mut integer = Type::new(TypeKind::Bit);
        integer.set_concrete_width(Shape::new(vec![Some(32)]));
        let ct = Comptime {
            r#type: integer,
            ..Default::default()
        };
        let float_ct = Comptime {
            r#type: float_type,
            ..Default::default()
        };
        let mut expression = Expression::Unary(
            Op::Sub,
            Box::new(Expression::Term(Box::new(Factor::Value(float_ct)))),
            Box::new(ct.clone()),
        );
        for _ in 0..10_000 {
            expression = Expression::Binary(
                Box::new(expression),
                Op::Add,
                Box::new(Expression::Term(Box::new(Factor::Value(ct.clone())))),
                Box::new(ct.clone()),
            );
        }
        assert_eq!(precision(type_of(&expression)), Some(FloatPrecision::F32));
        // The test also destroys the recursive AST iteratively.
        let mut pending = vec![expression];
        while let Some(expression) = pending.pop() {
            match expression {
                Expression::Unary(_, inner, _) => pending.push(*inner),
                Expression::Binary(left, _, right, _) => {
                    pending.push(*left);
                    pending.push(*right);
                }
                _ => (),
            }
        }
    }
}
