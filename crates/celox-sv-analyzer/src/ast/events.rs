//! Named events (IEEE 1800-2023 15.5): handles of synchronization objects
//! that the scheduler owns.
use super::*;
use celox_design::ProcessOp;
use celox_sv_syntax::{Declarations, ExprId, ExprKind};
use system_functions::{Callee, ObjectOp, SystemFunction};

/// The built-in class of event handles.
pub(super) const CLASS: &str = "$builtin::event";

/// A new synchronization object: the value of an event declared without an
/// initializer (15.5).
pub(super) fn new_event() -> Expr {
    classes::object_call(ObjectOp::NewEvent, Vec::new())
}

/// Whether type `ty` is `event`.
pub(super) fn is_event(ty: &Type) -> bool {
    ty.kind() == TypeKind::Event && ty.unpacked_ranges.is_empty() && ty.container.is_none()
}

/// A call of scheduler operation `op` on events `events`.
pub(super) fn call(op: ProcessOp, events: Vec<Expr>) -> Expr {
    Expr::Call {
        callee: Callee::System(SystemFunction::Process(op)),
        args: events.into_iter().map(Some).collect(),
        arg_order: Vec::new(),
    }
}

/// Whether native expression `id` is a value of type `event`: a variable,
/// property or element of type `event`, or a conditional whose operands are,
/// or are `null` (15.5.5.2).
/// Subroutines returning events are rejected, so a call is never one.
pub(super) fn is_handle(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> bool {
    if let ExprKind::Conditional {
        then_expression,
        else_expression,
        ..
    } = &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    {
        let null = |id| {
            matches!(
                &native.expression(expression_binding::ungroup(id, native)).kind,
                ExprKind::Literal(value) if value == "null"
            )
        };
        let event = |id| is_handle(tree, id, native, dimensions);
        return (event(*then_expression) || null(*then_expression))
            && (event(*else_expression) || null(*else_expression))
            && !(null(*then_expression) && null(*else_expression));
    }
    expression_binding::object_native_class(tree, id, native, dimensions).as_deref() == Some(CLASS)
}

/// The event of `e.triggered` or `e.triggered()` (15.5.3).
pub(super) fn triggered(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<ExprId>> {
    let id = expression_binding::ungroup(id, native);
    let callee = match &native.expression(id).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => {
            if arguments
                .iter()
                .any(|argument| argument.name.is_some() || argument.value.is_some())
            {
                return Ok(None);
            }
            *expression
        }
        _ => id,
    };
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(callee).kind
    else {
        return Ok(None);
    };
    Ok((separator == "."
        && instances::normalize_identifier(&name.spelling) == "triggered"
        && is_handle(tree, *expression, native, dimensions))
    .then_some(*expression))
}

/// Whether statement `stmt` blocks on a synchronization object the scheduler
/// owns: an event control or wait on named events, or `process.await`.
pub(super) fn blocks(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::Eval(Expr::Call {
            callee: Callee::System(SystemFunction::Process(
                ProcessOp::WaitEvent
                    | ProcessOp::WaitTriggered
                    | ProcessOp::Await
                    | ProcessOp::SemaphoreGet
                    | ProcessOp::MailboxPut
                    | ProcessOp::MailboxGet
                    | ProcessOp::MailboxPeek
            )),
            ..
        })
    )
}

/// Whether expression `expr` reads the `triggered` state of an event. A
/// trigger does not reevaluate a wait condition that only contains it.
pub(super) fn reads_triggered(expr: &Expr) -> bool {
    let mut found = false;
    const_functions::visit_expr_calls(expr, &mut |callee, _| {
        found |= matches!(
            callee,
            Callee::System(SystemFunction::Process(ProcessOp::Triggered))
        );
    });
    found
}
