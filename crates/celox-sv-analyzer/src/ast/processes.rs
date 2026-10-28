//! Scheduler-owned process handles (IEEE 1800-2023 9.7).
use super::*;
use celox_design::ProcessOp;
use celox_sv_syntax::{Declarations, ExprId, ExprKind};

pub(super) const CLASS: &str = "$builtin::process";

pub(super) fn state_type() -> Type {
    let mut ty = procedural::int_type();
    ty.is_enum = true;
    ty.enum_values = Some((0..5).collect());
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

pub(super) fn qualified_type(path: &[celox_sv_syntax::TypeSegment]) -> Option<Type> {
    if path.iter().any(|part| part.arguments.is_some()) {
        return None;
    }
    match path {
        [std, process] if std.name.spelling == "std" && process.name.spelling == "process" => {
            Some(classes::handle(CLASS))
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
    if static_receiver
        && let Some(code) = ["FINISHED", "RUNNING", "WAITING", "SUSPENDED", "KILLED"]
            .iter()
            .position(|&n| n == method)
    {
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
