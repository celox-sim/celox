//! The built-in semaphore class (IEEE 1800-2023 15.3), whose objects the
//! scheduler owns.
use super::*;
use celox_design::ProcessOp;
use celox_sv_syntax::{Argument, Declarations, ExprId, ExprKind};

/// The built-in class of semaphore handles.
pub(super) const CLASS: &str = "$builtin::semaphore";

/// The key count of a call, `default` when it is omitted.
fn key_count(
    tree: &AnalysisContext<'_>,
    arguments: &[Argument],
    native: &Declarations,
    dimensions: &PackedDimensions,
    method: &str,
    default: u32,
) -> Converted<Expr> {
    // `s.get()` lists one empty argument.
    let arguments = match arguments {
        [argument] if argument.name.is_none() && argument.value.is_none() => &[],
        arguments => arguments,
    };
    match arguments {
        [] => Ok(Expr::Literal(format!("32'sd{default}"))),
        [argument]
            if argument
                .name
                .as_ref()
                .is_none_or(|name| name.spelling == "keyCount") =>
        {
            match argument.value {
                Some(value) => expression_binding::value(value, native, tree, dimensions),
                None => Ok(Expr::Literal(format!("32'sd{default}"))),
            }
        }
        _ => Err(unsupported(format!("arguments of semaphore {method}"))),
    }
}

/// `new(keyCount)` of a semaphore (15.3.1).
pub(super) fn new(
    tree: &AnalysisContext<'_>,
    arguments: &[Argument],
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let keys = key_count(tree, arguments, native, dimensions, "new", 0)?;
    Ok(events::call(ProcessOp::SemaphoreNew, vec![keys]))
}

/// A call of `put`, `get` or `try_get` on a semaphore (15.3.2–15.3.4), as a
/// statement or, for `try_get`, as a value.
pub(super) fn value(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    statement: bool,
) -> Converted<Option<Expr>> {
    let id = expression_binding::ungroup(id, native);
    let (callee, arguments) = match &native.expression(id).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => (*expression, arguments.as_slice()),
        _ => (id, &[][..]),
    };
    let ExprKind::Member {
        expression: receiver,
        separator,
        name,
    } = &native.expression(callee).kind
    else {
        return Ok(None);
    };
    if separator != "."
        || expression_binding::object_native_class(tree, *receiver, native, dimensions).as_deref()
            != Some(CLASS)
    {
        return Ok(None);
    }
    let method = instances::normalize_identifier(&name.spelling);
    let op = match method.as_str() {
        "put" => ProcessOp::SemaphorePut,
        "get" => ProcessOp::SemaphoreGet,
        "try_get" => ProcessOp::SemaphoreTryGet,
        _ => return Err(unsupported(format!("semaphore method `{method}`"))),
    };
    if !statement && op != ProcessOp::SemaphoreTryGet {
        return Err(unsupported(format!(
            "semaphore {method}, which returns nothing, used as a value"
        )));
    }
    let semaphore = expression_binding::value(*receiver, native, tree, dimensions)?;
    let keys = key_count(tree, arguments, native, dimensions, &method, 1)?;
    Ok(Some(events::call(op, vec![semaphore, keys])))
}
