//! Scheduler-owned process handles (IEEE 1800-2023 9.7).
use super::*;
use celox_design::ProcessOp;
use celox_sv_syntax::{Declarations, ExprId, ExprKind};

pub(super) const CLASS: &str = "$builtin::process";

/// `enum state { FINISHED, RUNNING, WAITING, SUSPENDED, KILLED }` (9.7).
const STATES: [&str; 5] = ["FINISHED", "RUNNING", "WAITING", "SUSPENDED", "KILLED"];

pub(super) fn state_type() -> Type {
    let mut ty = procedural::int_type();
    ty.is_enum = true;
    ty.enum_domain = Some(Arc::new(enums::EnumDomain {
        members: STATES
            .into_iter()
            .zip(0..)
            .map(|(name, value)| enums::EnumMember {
                name: name.into(),
                value,
            })
            .collect(),
        width: 32,
        signed: true,
        four_state: false,
    }));
    ty
}

/// The built-in scope, also available unambiguously through std (26.7).
pub(super) fn scope(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> bool {
    match &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    {
        ExprKind::Name(name) => {
            let name = instances::normalize_identifier(&name.spelling);
            if let Some(ty) = dimensions.type_aliases.get(&name) {
                return ty.class() == Some(CLASS);
            }
            if dimensions.get(&name).is_some() {
                return false;
            }
            !scope::imported(&tree.state).package_scopes.contains(&name)
                && classes::resolve(tree, &name).as_deref() == Some(CLASS)
        }
        ExprKind::Member {
            expression,
            separator,
            name,
        } => {
            separator == "::"
                && name.spelling == "process"
                && matches!(&native.expression(*expression).kind, ExprKind::Name(name) if name.spelling == "std")
        }
        _ => false,
    }
}

/// The type of a name of a built-in class the scheduler owns: `std::process`,
/// `std::semaphore`, `std::process::state` or `mailbox #(T)`. Boxed and kept
/// out of line, so that the frame of the recursive type binder stays small.
#[inline(never)]
pub(super) fn builtin_type(
    path: &[celox_sv_syntax::TypeSegment],
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Option<Box<Type>>> {
    if let Some(ty) = qualified_type(path) {
        return Ok(Some(Box::new(ty)));
    }
    if path
        .last()
        .is_some_and(|part| part.name.spelling == "mailbox")
    {
        return super::mailboxes::named_type(path, tree, context);
    }
    Ok(None)
}

pub(super) fn qualified_type(path: &[celox_sv_syntax::TypeSegment]) -> Option<Type> {
    if path.iter().any(|part| part.arguments.is_some()) {
        return None;
    }
    match path {
        [std, process] if std.name.spelling == "std" && process.name.spelling == "process" => {
            Some(classes::handle(CLASS))
        }
        [std, semaphore]
            if std.name.spelling == "std" && semaphore.name.spelling == "semaphore" =>
        {
            Some(classes::handle(super::semaphores::CLASS))
        }
        [std, process, state]
            if std.name.spelling == "std"
                && process.name.spelling == "process"
                && state.name.spelling == "state" =>
        {
            Some(state_type())
        }
        _ => None,
    }
}

/// The enumerated type `state` of native expression `id` when it is a
/// constant of that type or the result of `status()` of a process handle.
pub(super) fn state(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Option<Arc<enums::EnumDomain>> {
    let id = expression_binding::ungroup(id, native);
    let (callee, called) = match &native.expression(id).kind {
        ExprKind::Call { expression, .. } => (*expression, true),
        _ => (id, false),
    };
    let ExprKind::Member {
        expression: receiver,
        separator,
        name,
    } = &native.expression(callee).kind
    else {
        return None;
    };
    let member = instances::normalize_identifier(&name.spelling);
    let state = match separator.as_str() {
        "::" => {
            !called
                && STATES.contains(&member.as_str())
                && scope(tree, *receiver, native, dimensions)
        }
        "." => {
            member == "status"
                && expression_binding::object_native_class(tree, *receiver, native, dimensions)
                    .as_deref()
                    == Some(CLASS)
        }
        _ => false,
    };
    state.then(|| state_type().enum_domain).flatten()
}

pub(super) fn value(
    tree: &AnalysisContext<'_>,
    root: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    statement: bool,
) -> Converted<Option<Expr>> {
    let (callee, args) = match &native.expression(root).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => (*expression, arguments.as_slice()),
        _ => (root, &[][..]),
    };
    let ExprKind::Member {
        expression: receiver,
        separator,
        name,
    } = &native.expression(callee).kind
    else {
        return Ok(None);
    };
    let static_receiver = separator == "::" && scope(tree, *receiver, native, dimensions);
    let handle_receiver = separator == "."
        && expression_binding::object_native_class(tree, *receiver, native, dimensions).as_deref()
            == Some(CLASS);
    if !static_receiver && !handle_receiver {
        return Ok(None);
    }
    let method = instances::normalize_identifier(&name.spelling);
    if static_receiver && let Some(code) = STATES.iter().position(|&n| n == method) {
        if !args.is_empty() {
            return Err(unsupported("call of process state constant"));
        }
        return Ok(Some(Expr::Literal(format!("32'sd{code}"))));
    }
    let op = match method.as_str() {
        "self" => ProcessOp::SelfHandle,
        "status" => ProcessOp::Status,
        "kill" => ProcessOp::Kill,
        "await" => ProcessOp::Await,
        "suspend" => ProcessOp::Suspend,
        "resume" => ProcessOp::Resume,
        "srandom" => ProcessOp::Srandom,
        "get_randstate" => ProcessOp::GetRandstate,
        "set_randstate" => ProcessOp::SetRandstate,
        _ if static_receiver => return Ok(None),
        _ => return Err(unsupported(format!("process member `{method}`"))),
    };
    if !statement
        && !matches!(
            op,
            ProcessOp::SelfHandle | ProcessOp::Status | ProcessOp::GetRandstate
        )
    {
        return Err(unsupported(format!(
            "void process method `{method}` used as a value"
        )));
    }
    if static_receiver && op != ProcessOp::SelfHandle {
        return Err(unsupported(format!(
            "non-static process method `{method}` without a receiver"
        )));
    }
    let expected = usize::from(matches!(op, ProcessOp::Srandom | ProcessOp::SetRandstate));
    if args.len() != expected || args.iter().any(|arg| arg.value.is_none()) {
        return Err(unsupported(format!(
            "process method `{method}` expects {expected} arguments"
        )));
    }
    let mut bound = Vec::new();
    if op != ProcessOp::SelfHandle {
        bound.push(Some(expression_binding::value(
            *receiver, native, tree, dimensions,
        )?));
    }
    for arg in args {
        if let Some(name) = &arg.name {
            let formal = if op == ProcessOp::Srandom {
                "seed"
            } else {
                "state"
            };
            if name.spelling != formal {
                return Err(unsupported(format!(
                    "process method `{method}` argument `{}`",
                    name.spelling
                )));
            }
        }
        bound.push(Some(expression_binding::value(
            arg.value.unwrap(),
            native,
            tree,
            dimensions,
        )?));
    }
    Ok(Some(Expr::Call {
        callee: system_functions::Callee::System(system_functions::SystemFunction::Process(op)),
        args: bound,
        arg_order: Vec::new(),
    }))
}
