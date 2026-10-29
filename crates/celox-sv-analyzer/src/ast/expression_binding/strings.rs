//! `string` operands (IEEE 1800-2023 6.16).
//!
//! A string variable holds a heap handle, so the integral operators would
//! act on handles. Expressions with a string operand are rewritten into
//! [`Callee::String`] calls instead, which the frontend lowers to heap
//! operations. Equality stays an integral comparison of handles: equal
//! strings have equal handles.

use super::*;

/// Whether `expr` has type `string`.
pub(in crate::ast) fn string_value(expr: &Expr, dimensions: &PackedDimensions) -> bool {
    match expr {
        Expr::Call {
            callee:
                Callee::System(crate::system_functions::SystemFunction::Process(
                    celox_design::ProcessOp::GetRandstate,
                )),
            ..
        } => true,
        Expr::Ident(name) => dimensions.get(name).is_some_and(|ty| ty.string),
        Expr::Call {
            callee: Callee::String(op),
            ..
        } => op.returns_string(),
        Expr::Call {
            callee: Callee::Subroutine(name),
            ..
        } => {
            dimensions
                .functions
                .get(name)
                .is_some_and(|function| function.return_string)
                || dimensions
                    .function_return_types
                    .get(name)
                    .is_some_and(|metadata| metadata.string)
        }
        Expr::Call {
            callee: Callee::Object(op),
            ..
        } => {
            matches!(**op, crate::system_functions::ObjectOp::Load(field)
            if field.handle == Some(crate::system_functions::HandleProperty::String))
                || matches!(**op, ObjectOp::Container { op: ContainerOp::Get | ContainerOp::PopFront | ContainerOp::PopBack, layout } if layout.handle == Some(crate::system_functions::HandleProperty::String))
                || matches!(
                    **op,
                    ObjectOp::Stream(
                        crate::system_functions::StreamOp::ToString
                            | crate::system_functions::StreamOp::TakeString { .. }
                    )
                )
        }
        Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => string_value(then_expr, dimensions) || string_value(else_expr, dimensions),
        _ => false,
    }
}

/// A call of string operation `op`.
pub(in crate::ast) fn call(op: StringOp, args: Vec<Expr>) -> Expr {
    Expr::Call {
        callee: Callee::String(op),
        args: args.into_iter().map(Some).collect(),
        arg_order: Vec::new(),
    }
}

/// `expr` as a string: a string unchanged, or the bytes of an integral
/// value, as a string literal or a cast to `string` gives them.
pub(in crate::ast) fn to_string(expr: Expr, dimensions: &PackedDimensions) -> Expr {
    if string_value(&expr, dimensions) {
        expr
    } else {
        call(StringOp::FromBits, vec![expr])
    }
}

/// A binary operator with a string operand (11.4.4, 11.4.5, Table 6-9).
pub(in crate::ast) fn binary(
    op: &str,
    left: Expr,
    right: Expr,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let left = to_string(left, dimensions);
    let right = to_string(right, dimensions);
    let compare = |op| Expr::Binary {
        left: Box::new(call(StringOp::Compare, vec![left.clone(), right.clone()])),
        op,
        right: Box::new(Expr::Literal("0".into())),
    };
    Ok(match op {
        "==" | "===" => Expr::Binary {
            left: Box::new(left),
            op: BinaryOp::Eq,
            right: Box::new(right),
        },
        "!=" | "!==" => Expr::Binary {
            left: Box::new(left),
            op: BinaryOp::Ne,
            right: Box::new(right),
        },
        "<" => compare(BinaryOp::Lt),
        "<=" => compare(BinaryOp::Le),
        ">" => compare(BinaryOp::Gt),
        ">=" => compare(BinaryOp::Ge),
        _ => return Err(unsupported(format!("operator `{op}` on a string operand"))),
    })
}

/// The arguments of string operation `op` after binding: the operands it
/// reads as strings are converted to strings.
pub(in crate::ast) fn operands(
    op: StringOp,
    operands: Vec<Expr>,
    dimensions: &PackedDimensions,
) -> Vec<Expr> {
    let strings = match op {
        // A format keeps its type: the frontend reads a literal one as text.
        StringOp::FromBits | StringOp::Itoa(_) | StringOp::Format { .. } | StringOp::Pattern(_) => {
            0
        }
        StringOp::Concat => usize::MAX,
        StringOp::Compare | StringOp::ICompare => 2,
        _ => 1,
    };
    operands
        .into_iter()
        .enumerate()
        .map(|(index, operand)| {
            // A concatenation converts each integral operand to its bytes.
            if index < strings {
                to_string(operand, dimensions)
            } else {
                operand
            }
        })
        .collect()
}

/// Whether native expression `id` has type `string`, judged before it is
/// bound: a string variable, or a method of a string value that returns one.
pub(in crate::ast) fn native_string(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> bool {
    match &native.expression(ungroup(id, native)).kind {
        ExprKind::Call { expression, .. } if matches!(&native.expression(*expression).kind, ExprKind::Member { expression: base, name, separator } if separator == "." && name.spelling == "get_randstate" && objects::native_class(tree, *base, native, dimensions).as_deref() == Some(super::super::processes::CLASS)) => {
            true
        }
        ExprKind::Select { expression, .. }
            if containers::native_type(*expression, native, tree, dimensions)
                .ok()
                .flatten()
                .is_some() =>
        {
            containers::native_type(*expression, native, tree, dimensions)
                .ok()
                .flatten()
                .is_some_and(|ty| {
                    ty.layout.handle == Some(crate::system_functions::HandleProperty::String)
                })
        }
        ExprKind::Call { expression, .. } if matches!(&native.expression(*expression).kind, ExprKind::Member { expression: base, name, .. } if matches!(name.spelling.as_str(), "pop_front" | "pop_back") && containers::native_type(*base, native, tree, dimensions).ok().flatten().is_some_and(|ty| ty.layout.handle == Some(crate::system_functions::HandleProperty::String))) => {
            true
        }
        ExprKind::Name(name)
            if dimensions
                .get(&instances::normalize_identifier(&name.spelling))
                .is_some() =>
        {
            dimensions
                .get(&instances::normalize_identifier(&name.spelling))
                .is_some_and(|ty| ty.string)
        }
        ExprKind::Call { expression, .. } => {
            matches!(&native.expression(ungroup(*expression, native)).kind,
                ExprKind::Name(name) if matches!(name.spelling.as_str(), "$sformatf" | "$typename"))
                || method(tree, *expression, native, dimensions)
                    .is_some_and(|(op, _)| op.returns_string())
                || enum_name(tree, *expression, native, dimensions)
        }
        // `e.name` without parentheses (6.19.5.6, 6.19.5.7).
        ExprKind::Member { .. } if enum_name(tree, id, native, dimensions) => true,
        // A string member of a container element of unpacked structure type.
        ExprKind::Member { .. }
            if containers::element_member(ungroup(id, native), native, tree, dimensions)
                .ok()
                .flatten()
                .is_some() =>
        {
            containers::element_member(ungroup(id, native), native, tree, dimensions)
                .ok()
                .flatten()
                .is_some_and(|(ty, _)| ty.kind() == TypeKind::String)
        }
        // A string member of an unpacked structure variable.
        ExprKind::Member { .. }
            if unpacked_structs::native_member(ungroup(id, native), native, dimensions)
                .is_some() =>
        {
            unpacked_structs::native_member(ungroup(id, native), native, dimensions)
                .and_then(|name| dimensions.get(&name))
                .is_some_and(|ty| ty.string)
        }
        // A string property of a class object.
        _ => objects::property(tree, id, native, dimensions)
            .ok()
            .flatten()
            .is_some_and(|(property, _)| {
                property.field.handle == Some(crate::system_functions::HandleProperty::String)
            }),
    }
}

/// Whether native callee expression `id` names the `name` method of an
/// enumerated type (6.19.5.6).
fn enum_name(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> bool {
    enums::method(tree, id, native, dimensions)
        .ok()
        .flatten()
        .is_some_and(|(method, _, _)| method.returns_string())
}

/// The string method that native callee expression `id` names, with the
/// expression it is called on.
pub(in crate::ast) fn method(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Option<(StringOp, ExprId)> {
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(ungroup(id, native)).kind
    else {
        return None;
    };
    if separator != "." || !native_string(tree, *expression, native, dimensions) {
        return None;
    }
    let (op, _) = StringOp::method(&name.spelling)?;
    Some((op, *expression))
}
