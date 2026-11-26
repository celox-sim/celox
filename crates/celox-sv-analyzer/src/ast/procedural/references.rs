//! Uses of reference formals and calls passing by reference that IEEE
//! 1800-2023 forbids, and calls that omit an actual without a default,
//! checked once the bodies are bound.
use super::*;
use crate::procedural::ParamDirection;
use lifetimes::{expr_reads, stmt_exprs, visit_calls};

pub(in crate::ast) fn validate<'a>(
    subroutines: impl Iterator<Item = &'a Subroutine>,
    comb: &[CombProcess],
    ff: &[FfProcess],
    processes: &[InitialProcess],
    directions: &lifetimes::Directions,
    names: &HashMap<String, Vec<String>>,
    nets: &HashSet<&str>,
) -> Result<(), AnalyzerError> {
    let formal = |name: &str, position: usize| {
        names
            .get(name)
            .and_then(|names| names.get(position))
            .cloned()
            .unwrap_or_else(|| (position + 1).to_string())
    };
    let invalid =
        |subroutine: &str, argument: &str, detail: &str| AnalyzerError::InvalidRefArgument {
            subroutine: subroutine.to_owned(),
            argument: argument.to_owned(),
            detail: detail.to_owned(),
        };
    let direction = |name: &str, position: usize| {
        directions
            .get(name)
            .and_then(|directions| directions.get(position))
            .copied()
    };
    let subroutines = subroutines.collect::<Vec<_>>();
    let params = subroutines
        .iter()
        .map(|subroutine| (subroutine.name.as_str(), subroutine.params.as_slice()))
        .collect::<HashMap<_, _>>();
    let check_call = |name: &str, args: &[Option<Expr>]| -> Result<(), AnalyzerError> {
        let unbindable = |detail: String| AnalyzerError::InvalidCallArguments {
            subroutine: name.strip_suffix("$virtual").unwrap_or(name).to_owned(),
            detail,
        };
        if let Some(params) = params.get(name) {
            if args.len() > params.len() {
                return Err(unbindable(format!(
                    "{} actuals for {} formals (IEEE 1800-2023 13.5.4)",
                    args.len(),
                    params.len()
                )));
            }
            // An omitted actual takes the default of its formal, which
            // shall have one (IEEE 1800-2023 13.5.3).
            if let Some((_, missing)) = params.iter().enumerate().find(|(position, param)| {
                param.default.is_none() && args.get(*position).is_none_or(Option::is_none)
            }) {
                return Err(unbindable(format!(
                    "formal `{}` has no default and is not given an actual (IEEE 1800-2023 13.5.3)",
                    missing.source_name
                )));
            }
        }
        // A net is never passed by reference (13.5.2).
        for (position, arg) in args.iter().enumerate() {
            if let Some(actual) = arg.as_ref().and_then(root_variable)
                && direction(name, position).is_some_and(ParamDirection::is_reference)
                && nets.contains(actual)
            {
                return Err(invalid(
                    name,
                    &formal(name, position),
                    "a net cannot be passed by reference (IEEE 1800-2023 13.5.2)",
                ));
            }
        }
        Ok(())
    };
    let check_exprs = |exprs: &mut dyn Iterator<Item = &Expr>| -> Result<(), AnalyzerError> {
        let mut result = Ok(());
        for expr in exprs {
            visit_calls(expr, &mut |callee, args| {
                if let Callee::Subroutine(name) = callee
                    && result.is_ok()
                {
                    result = check_call(name, args);
                }
            });
        }
        result
    };
    let check_body = |body: &[Stmt]| -> Result<(), AnalyzerError> {
        let mut result = Ok(());
        for stmt in body {
            stmt.walk(&mut |stmt| {
                if result.is_err() {
                    return;
                }
                if let Stmt::Call { name, args, .. } = stmt {
                    result = check_call(name, args);
                }
                if result.is_ok() {
                    result = check_exprs(&mut stmt_exprs(stmt).into_iter());
                }
            });
        }
        result
    };
    // A function with a ref argument is called only from procedural code,
    // and never in an event expression (13.4.1); const ref is allowed.
    let passes_reference = |expr: &Expr| {
        let mut found = None;
        visit_calls(expr, &mut |callee, args| {
            if let Callee::Subroutine(name) = callee
                && let Some(position) = (0..args.len())
                    .find(|position| direction(name, *position) == Some(ParamDirection::Ref))
            {
                found.get_or_insert_with(|| procedural_only(name, &formal(name, position)));
            }
        });
        found
    };
    for process in comb {
        for assignment in process.assignments() {
            if let Some(error) = passes_reference(assignment.rhs()) {
                return Err(error);
            }
            check_exprs(&mut std::iter::once(assignment.rhs()))?;
        }
        // A continuous assignment to a concatenation is a statement.
        if process.kind() == CombProcessKind::ContinuousAssign {
            for stmt in process.body() {
                let mut found = None;
                stmt.walk(&mut |stmt| {
                    for expr in stmt_exprs(stmt) {
                        if found.is_none() {
                            found = passes_reference(expr);
                        }
                    }
                });
                if let Some(error) = found {
                    return Err(error);
                }
            }
        }
        check_body(process.body())?;
    }
    for process in ff {
        check_body(process.body())?;
    }
    for process in processes {
        check_body(process.body())?;
        let mut found = None;
        for stmt in process.body() {
            stmt.walk(&mut |stmt| {
                if let Stmt::WaitEvent(items) = stmt {
                    for item in items {
                        for expr in std::iter::once(&item.expr).chain(&item.iff) {
                            if found.is_none() {
                                found = passes_reference(expr);
                            }
                        }
                    }
                }
            });
        }
        if let Some(error) = found {
            return Err(error);
        }
    }
    for subroutine in subroutines {
        check_body(&subroutine.body)?;
        for param in subroutine
            .params
            .iter()
            .filter(|param| param.direction.is_reference())
        {
            // A formal of unpacked structure type is used through its members.
            let formals = if param.leaves.is_empty() {
                std::slice::from_ref(&param.name)
            } else {
                param.leaves.as_slice()
            };
            // A dispatcher passes its formals to the overrides, whose
            // formals have the same directions (IEEE 1800-2023 8.20), also
            // those of classes of packages the scope does not import, which
            // a specialization made a package it uses see.
            let forwarded = subroutine
                .name
                .ends_with("$virtual")
                .then_some(subroutine.params.as_slice());
            let mut error = None;
            for formal in formals {
                for stmt in &subroutine.body {
                    check_formal(
                        stmt,
                        formal,
                        param.direction,
                        directions,
                        forwarded,
                        &mut error,
                    );
                }
            }
            if let Some(detail) = error {
                return Err(invalid(&subroutine.name, &param.source_name, detail));
            }
        }
    }
    Ok(())
}

fn procedural_only(name: &str, argument: &str) -> AnalyzerError {
    AnalyzerError::InvalidRefArgument {
        subroutine: name.to_owned(),
        argument: argument.to_owned(),
        detail: "a function with a ref argument can only be called in a procedural statement \
                 outside an event expression (IEEE 1800-2023 13.4.1)"
            .into(),
    }
}

/// The variable that an actual names or selects from.
fn root_variable(actual: &Expr) -> Option<&str> {
    match actual {
        Expr::Ident(name) => Some(name),
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } => root_variable(expr),
        _ => None,
    }
}

/// Whether an actual passed to a writable formal writes a variable matching
/// `matches`: the variable itself, a select of it, or a part of a concatenation.
fn written_target_in<'a>(actual: &'a Expr, matches: &dyn Fn(&str) -> bool) -> Option<&'a str> {
    match actual {
        Expr::Ident(ident) if matches(ident) => Some(ident.as_str()),
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } => written_target_in(expr, matches),
        Expr::Concat(parts) => parts
            .iter()
            .find_map(|part| written_target_in(part, matches)),
        Expr::Call {
            callee: Callee::Checked { .. },
            args,
            ..
        } => args
            .last()
            .and_then(Option::as_ref)
            .and_then(|value| written_target_in(value, matches)),
        Expr::Call {
            callee: Callee::Object(op),
            args,
            ..
        } if matches!(
            op.as_ref(),
            crate::system_functions::ObjectOp::Container {
                op: crate::system_functions::ContainerOp::Get,
                ..
            } | crate::system_functions::ObjectOp::Load(crate::system_functions::ObjectField {
                nullable: true,
                ..
            }) | crate::system_functions::ObjectOp::Unbox(_)
        ) =>
        {
            args.first()
                .and_then(Option::as_ref)
                .and_then(|container| written_target_in(container, matches))
        }
        _ => None,
    }
}

/// First target matching `matches` written via an argument of `callee(args)`.
fn written_by_call<'a>(
    callee: &Callee,
    args: &'a [Option<Expr>],
    directions: &lifetimes::Directions,
    forwarded: Option<&[SubroutineParam]>,
    matches: &dyn Fn(&str) -> bool,
) -> Option<&'a str> {
    args.iter().enumerate().find_map(|(position, arg)| {
        let target = arg
            .as_ref()
            .and_then(|arg| written_target_in(arg, matches))?;
        call_writes_argument(callee, position, directions, forwarded).then_some(target)
    })
}

fn written_target_in_const<'a>(
    actual: &'a ConstExpr,
    matches: &dyn Fn(&str) -> bool,
) -> Option<&'a str> {
    match actual {
        ConstExpr::Ident(ident) if matches(ident) => Some(ident.as_str()),
        ConstExpr::Select { expr, .. }
        | ConstExpr::SelectRange { expr, .. }
        | ConstExpr::Resize { expr, .. } => written_target_in_const(expr, matches),
        ConstExpr::Function {
            callee: Callee::Checked { .. },
            args,
            ..
        } => args
            .last()
            .and_then(|value| written_target_in_const(value, matches)),
        ConstExpr::Function {
            callee: Callee::Object(op),
            args,
            ..
        } if matches!(
            op.as_ref(),
            crate::system_functions::ObjectOp::Container {
                op: crate::system_functions::ContainerOp::Get,
                ..
            } | crate::system_functions::ObjectOp::Load(crate::system_functions::ObjectField {
                nullable: true,
                ..
            }) | crate::system_functions::ObjectOp::Unbox(_)
        ) =>
        {
            args.first()
                .and_then(|container| written_target_in_const(container, matches))
        }
        _ => None,
    }
}

fn call_writes_argument(
    callee: &Callee,
    position: usize,
    directions: &lifetimes::Directions,
    forwarded: Option<&[SubroutineParam]>,
) -> bool {
    match callee {
        Callee::Subroutine(name) => match directions.get(name) {
            Some(directions) => directions.get(position).copied(),
            None => forwarded
                .and_then(|params| params.get(position))
                .map(|param| param.direction),
        }
        .is_none_or(|direction| direction.is_written()),
        Callee::System(function) => function.writes_argument(position),
        Callee::Object(op) => op.writes_argument(position),
        Callee::String(_) | Callee::Checked { .. } | Callee::Lookup { .. } | Callee::Stream(_) => {
            false
        }
    }
}

fn written_by_const_call<'a>(
    callee: &Callee,
    args: &'a [ConstExpr],
    directions: &lifetimes::Directions,
    forwarded: Option<&[SubroutineParam]>,
    matches: &dyn Fn(&str) -> bool,
) -> Option<&'a str> {
    args.iter().enumerate().find_map(|(position, arg)| {
        let target = written_target_in_const(arg, matches)?;
        call_writes_argument(callee, position, directions, forwarded).then_some(target)
    })
}

fn visit_const_calls_in<'a>(expr: &'a ConstExpr, f: &mut impl FnMut(&'a Callee, &'a [ConstExpr])) {
    match expr {
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => {}
        ConstExpr::Function { callee, args, .. } => {
            f(callee, args);
            for arg in args {
                visit_const_calls_in(arg, f);
            }
        }
        ConstExpr::Select { expr, bit } => {
            visit_const_calls_in(expr, f);
            visit_const_calls_in(bit, f);
        }
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            visit_const_calls_in(expr, f);
            visit_const_calls_in(msb, f);
            visit_const_calls_in(lsb, f);
        }
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
            visit_const_calls_in(expr, f);
        }
        ConstExpr::Binary { left, right, .. } => {
            visit_const_calls_in(left, f);
            visit_const_calls_in(right, f);
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            visit_const_calls_in(condition, f);
            visit_const_calls_in(then_expr, f);
            visit_const_calls_in(else_expr, f);
        }
    }
}

fn const_expr_written_by_call<'a>(
    expr: &'a ConstExpr,
    directions: &lifetimes::Directions,
    forwarded: Option<&[SubroutineParam]>,
    matches: &dyn Fn(&str) -> bool,
) -> Option<&'a str> {
    let mut hit = None;
    visit_const_calls_in(expr, &mut |callee, args| {
        if hit.is_none() {
            hit = written_by_const_call(callee, args, directions, forwarded, matches);
        }
    });
    hit
}

fn expr_written_by_call<'a>(
    expr: &'a Expr,
    directions: &lifetimes::Directions,
    forwarded: Option<&[SubroutineParam]>,
    matches: &dyn Fn(&str) -> bool,
) -> Option<&'a str> {
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => None,
        Expr::Call { callee, args, .. } => {
            written_by_call(callee, args, directions, forwarded, matches).or_else(|| {
                args.iter()
                    .flatten()
                    .find_map(|arg| expr_written_by_call(arg, directions, forwarded, matches))
            })
        }
        Expr::Select { expr, msb, lsb, .. } => {
            expr_written_by_call(expr, directions, forwarded, matches)
                .or_else(|| const_expr_written_by_call(msb, directions, forwarded, matches))
                .or_else(|| const_expr_written_by_call(lsb, directions, forwarded, matches))
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            expr_written_by_call(expr, directions, forwarded, matches)
        }
        Expr::Concat(parts) => parts
            .iter()
            .find_map(|part| expr_written_by_call(part, directions, forwarded, matches)),
        Expr::RepeatConcat { count, parts } => {
            const_expr_written_by_call(count, directions, forwarded, matches).or_else(|| {
                parts
                    .iter()
                    .find_map(|part| expr_written_by_call(part, directions, forwarded, matches))
            })
        }
        Expr::Binary { left, right, .. } => {
            expr_written_by_call(left, directions, forwarded, matches)
                .or_else(|| expr_written_by_call(right, directions, forwarded, matches))
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => expr_written_by_call(condition, directions, forwarded, matches)
            .or_else(|| expr_written_by_call(then_expr, directions, forwarded, matches))
            .or_else(|| expr_written_by_call(else_expr, directions, forwarded, matches)),
        Expr::Inside { expr, items } => expr_written_by_call(expr, directions, forwarded, matches)
            .or_else(|| {
                items.iter().find_map(|item| match item {
                    InsideItem::Value(value) => {
                        expr_written_by_call(value, directions, forwarded, matches)
                    }
                    InsideItem::Range { low, high } => {
                        expr_written_by_call(low, directions, forwarded, matches)
                            .or_else(|| expr_written_by_call(high, directions, forwarded, matches))
                    }
                })
            }),
    }
}

fn object_place_written_by_call<'a>(
    place: &'a ObjectPlace,
    directions: &lifetimes::Directions,
    forwarded: Option<&[SubroutineParam]>,
    matches: &dyn Fn(&str) -> bool,
) -> Option<&'a str> {
    match place {
        ObjectPlace::Property { handle, .. } => {
            expr_written_by_call(handle, directions, forwarded, matches)
        }
        ObjectPlace::Element {
            container, index, ..
        } => expr_written_by_call(container, directions, forwarded, matches)
            .or_else(|| expr_written_by_call(index, directions, forwarded, matches)),
        ObjectPlace::Member { outer, .. } => {
            object_place_written_by_call(outer, directions, forwarded, matches)
        }
    }
}

fn lvalue_written_by_call<'a>(
    lvalue: &'a LValue,
    directions: &lifetimes::Directions,
    forwarded: Option<&[SubroutineParam]>,
    matches: &dyn Fn(&str) -> bool,
) -> Option<&'a str> {
    match lvalue {
        LValue::Ident(_) => None,
        LValue::Select {
            msb,
            lsb,
            array_slice_width,
            window,
            ..
        } => const_expr_written_by_call(msb, directions, forwarded, matches)
            .or_else(|| const_expr_written_by_call(lsb, directions, forwarded, matches))
            .or_else(|| {
                array_slice_width.as_ref().and_then(|width| {
                    const_expr_written_by_call(width, directions, forwarded, matches)
                })
            })
            .or_else(|| {
                window
                    .as_ref()
                    .and_then(|w| w.at.as_deref())
                    .and_then(|at| const_expr_written_by_call(at, directions, forwarded, matches))
            }),
        LValue::Object {
            place,
            select,
            guards,
            ..
        } => object_place_written_by_call(place, directions, forwarded, matches)
            .or_else(|| lvalue_written_by_call(select, directions, forwarded, matches))
            .or_else(|| {
                guards
                    .iter()
                    .find_map(|guard| expr_written_by_call(guard, directions, forwarded, matches))
            }),
    }
}

fn stmt_lvalues(stmt: &Stmt) -> &[LValue] {
    match stmt {
        Stmt::Assign { lhs, .. }
        | Stmt::ProceduralAssign { lhs, .. }
        | Stmt::Force { lhs, .. }
        | Stmt::ForceStore { lhs, .. }
        | Stmt::Deassign { lhs }
        | Stmt::Release { lhs, .. } => std::slice::from_ref(lhs),
        Stmt::AssignConcat { parts, .. } => parts.as_slice(),
        _ => &[],
    }
}

/// Check that no statement or continuous assignment in module `module` writes
/// any of its `const ref` ports (or their unpacked structure members).
pub(in crate::ast) fn validate_const_ref_ports<'a>(
    module: &str,
    ports: &[Port],
    subroutines: impl Iterator<Item = &'a Subroutine>,
    comb: &[CombProcess],
    ff: &[FfProcess],
    processes: &[InitialProcess],
    directions: &lifetimes::Directions,
) -> Result<(), AnalyzerError> {
    let mut const_ports = HashSet::default();
    for port in ports {
        if port.direction() == PortDirection::Ref && port.is_const() {
            const_ports.insert(port.name().to_string());
            for (member, _) in port.r#type().structure_leaves() {
                const_ports.insert(format!("{}.{member}", port.name()));
            }
        }
    }
    if const_ports.is_empty() {
        return Ok(());
    }
    let check_stmts = |stmts: &[Stmt], shadowed: &HashSet<&str>| -> Result<(), AnalyzerError> {
        let is_target = |name: &str| !shadowed.contains(name) && const_ports.contains(name);
        let mut hit: Option<String> = None;
        for stmt in stmts {
            stmt.walk(&mut |stmt| {
                if hit.is_some() {
                    return;
                }
                match stmt {
                    Stmt::Assign { lhs, .. } => {
                        if is_target(lhs.name()) {
                            hit = Some(lhs.name().to_owned());
                        }
                    }
                    Stmt::AssignConcat { parts, .. } => {
                        if let Some(part) = parts.iter().find(|part| is_target(part.name())) {
                            hit = Some(part.name().to_owned());
                        }
                    }
                    Stmt::Call { name, args, .. } => {
                        if let Some(target) = written_by_call(
                            &Callee::Subroutine(name.clone()),
                            args,
                            directions,
                            None,
                            &is_target,
                        ) {
                            hit = Some(target.to_owned());
                        }
                    }
                    Stmt::SystemTask { task, args, .. } => {
                        for (position, arg) in args.iter().enumerate() {
                            if task.writes_argument(position)
                                && let crate::procedural::SystemTaskArg::Expr(expr) = arg
                                && let Some(target) = written_target_in(expr, &is_target)
                            {
                                hit = Some(target.to_owned());
                                break;
                            }
                        }
                    }
                    _ => {}
                }
                if hit.is_none() {
                    hit = stmt_exprs(stmt)
                        .into_iter()
                        .find_map(|expr| expr_written_by_call(expr, directions, None, &is_target))
                        .or_else(|| {
                            stmt_lvalues(stmt).iter().find_map(|lhs| {
                                lvalue_written_by_call(lhs, directions, None, &is_target)
                            })
                        })
                        .map(str::to_owned);
                }
            });
            if let Some(port) = hit {
                return Err(AnalyzerError::ConstRefPortAssignment {
                    module: module.to_owned(),
                    port,
                });
            }
        }
        Ok(())
    };
    let empty = HashSet::default();
    let is_const_port = |name: &str| const_ports.contains(name);
    for process in comb {
        for assignment in process.assignments() {
            if const_ports.contains(assignment.lhs()) {
                return Err(AnalyzerError::ConstRefPortAssignment {
                    module: module.to_owned(),
                    port: assignment.lhs().to_owned(),
                });
            }
            if let Some(port) = expr_written_by_call(
                assignment.rhs(),
                directions,
                None,
                &is_const_port,
            )
            .or_else(|| {
                lvalue_written_by_call(assignment.lhs_value(), directions, None, &is_const_port)
            }) {
                return Err(AnalyzerError::ConstRefPortAssignment {
                    module: module.to_owned(),
                    port: port.to_owned(),
                });
            }
        }
        check_stmts(process.body(), &empty)?;
    }
    for process in ff {
        check_stmts(process.body(), &empty)?;
    }
    for process in processes {
        check_stmts(process.body(), &empty)?;
    }
    for subroutine in subroutines {
        let shadowed: HashSet<&str> = subroutine
            .params
            .iter()
            .map(|param| param.name.as_str())
            .collect();
        check_stmts(&subroutine.body, &shadowed)?;
    }
    Ok(())
}

/// Record in `error` why statement `stmt` cannot use reference formal
/// `formal` of direction `direction`.
fn check_formal(
    stmt: &Stmt,
    formal: &str,
    direction: ParamDirection,
    directions: &lifetimes::Directions,
    forwarded: Option<&[SubroutineParam]>,
    error: &mut Option<&'static str>,
) {
    let constant = direction == ParamDirection::ConstRef;
    let is_formal = |name: &str| name == formal;
    stmt.walk(&mut |stmt| {
        if error.is_some() {
            return;
        }
        let (targets, nonblocking): (Vec<&LValue>, bool) = match stmt {
            Stmt::Assign {
                lhs, nonblocking, ..
            } => (vec![lhs], *nonblocking),
            Stmt::AssignConcat {
                parts, nonblocking, ..
            } => (parts.iter().collect(), *nonblocking),
            _ => (Vec::new(), false),
        };
        if targets.iter().any(|target| target.name() == formal) {
            if nonblocking {
                *error = Some(
                    "a ref argument cannot be written by a nonblocking assignment \
                     (IEEE 1800-2023 6.21, 13.5.2)",
                );
            } else if constant {
                *error = Some("a const ref argument cannot be written (IEEE 1800-2023 13.5.2)");
            }
        }
        if constant && error.is_none() {
            let mut written = match stmt {
                Stmt::Call { name, args, .. } => written_by_call(
                    &Callee::Subroutine(name.clone()),
                    args,
                    directions,
                    forwarded,
                    &is_formal,
                )
                .is_some(),
                Stmt::SystemTask { task, args, .. } => args.iter().enumerate().any(|(pos, arg)| {
                    task.writes_argument(pos)
                        && matches!(arg, crate::procedural::SystemTaskArg::Expr(arg)
                            if written_target_in(arg, &is_formal).is_some())
                }),
                _ => false,
            };
            written |= stmt_exprs(stmt).into_iter().any(|expr| {
                expr_written_by_call(expr, directions, forwarded, &is_formal).is_some()
            }) || stmt_lvalues(stmt).iter().any(|lhs| {
                lvalue_written_by_call(lhs, directions, forwarded, &is_formal).is_some()
            });
            if written {
                *error = Some("a const ref argument cannot be written (IEEE 1800-2023 13.5.2)");
            }
        }
        // Processes a fork-join_any or fork-join_none spawns may outlive
        // the call (9.3.2).
        if let Stmt::Fork { join, branches, .. } = stmt
            && *join != celox_design::ForkJoin::All
        {
            let mut used = false;
            for branch in branches {
                for stmt in branch {
                    stmt.walk(&mut |stmt| {
                        // Variables of the fork may be initialized from it.
                        if matches!(stmt, Stmt::Local { .. }) {
                            return;
                        }
                        used |= stmt_exprs(stmt).iter().any(|expr| expr_reads(expr, formal))
                            || match stmt {
                                Stmt::Assign { lhs, .. } => lhs.name() == formal,
                                Stmt::AssignConcat { parts, .. } => {
                                    parts.iter().any(|part| part.name() == formal)
                                }
                                _ => false,
                            };
                    });
                }
            }
            if used {
                *error = Some(
                    "a ref argument cannot be used in a fork-join_any or fork-join_none block \
                     (IEEE 1800-2023 9.3.2)",
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {

    use crate::{AnalyzerError, analyze_source};
    use std::path::Path;

    fn invalid(body: &str) -> String {
        let source = format!("module Top; {body} endmodule");
        match analyze_source(&source, Path::new("ref_arguments.sv")) {
            Err(error @ AnalyzerError::InvalidRefArgument { .. }) => error.to_string(),
            other => panic!("{body}: expected an invalid ref argument, got {other:?}"),
        }
    }

    #[test]
    fn reference_formals_need_automatic_subroutines() {
        let error = invalid("int g; function void f(ref int x); x = 1; endfunction initial f(g);");
        assert!(error.contains("static lifetime"), "{error}");
    }

    #[test]
    fn only_variables_are_passed_by_reference() {
        for (declarations, actual) in [
            ("int g;", "g + 1"),
            ("int g;", "3"),
            ("logic [31:0] g;", "g[7:0]"),
            ("parameter int P = 1;", "P"),
        ] {
            let error = invalid(&format!(
                "{declarations} function automatic void f(ref int x); x = 1; endfunction \
                 initial f({actual});"
            ));
            assert!(error.contains("13.5.2"), "{actual}: {error}");
        }
        let error = invalid(
            "wire [31:0] w; function automatic void f(ref logic [31:0] x); $display(x); \
             endfunction initial f(w);",
        );
        assert!(error.contains("net"), "{error}");
        let error = invalid(
            "wire [31:0] w[2]; function automatic void f(ref logic [31:0] x); x = 1; \
             endfunction initial f(w[0]);",
        );
        assert!(error.contains("net"), "{error}");
    }

    #[test]
    fn actuals_have_equivalent_types() {
        for declaration in [
            "logic [31:0] g;",
            "shortint g;",
            "int unsigned g;",
            "typedef enum {A, B} e_t; e_t g;",
            "int g[$];",
        ] {
            let error = invalid(&format!(
                "{declaration} function automatic void f(ref int x); x = 1; endfunction \
                 initial f(g);"
            ));
            assert!(error.contains("6.22.2"), "{declaration}: {error}");
        }
        let error = invalid(
            "class A; endclass class B extends A; endclass \
             function automatic void f(ref A x); x = null; endfunction \
             initial begin automatic B b; f(b); end",
        );
        assert!(error.contains("6.22.2"), "{error}");
        for declaration in ["bit signed [31:0] g;", "int g;"] {
            analyze_source(
                &format!(
                    "module Top; {declaration} function automatic void f(ref int x); x = 1; \
                     endfunction initial f(g); endmodule"
                ),
                Path::new("ref_equivalent.sv"),
            )
            .unwrap_or_else(|error| panic!("{declaration}: {error}"));
        }
    }

    #[test]
    fn queues_of_different_bounds_are_not_bound_by_reference() {
        let result = analyze_source(
            "module Top; int q[$:1]; function automatic void f(ref int x[$]); \
             x.push_back(3); endfunction initial f(q); endmodule",
            Path::new("ref_queue_bounds.sv"),
        );
        assert!(
            matches!(&result, Err(AnalyzerError::Unsupported(construct)) if construct.contains("different bounds")),
            "{result:?}"
        );
    }

    #[test]
    fn const_ref_formals_are_read_only() {
        for body in ["x = 1;", "x++;", "h(x);", "{x, y} = 64'd1;"] {
            let error = invalid(&format!(
                "int g, y; function automatic void h(ref int z); z = 2; endfunction \
                 function automatic void f(const ref int x); {body} endfunction initial f(g);"
            ));
            assert!(error.contains("const ref"), "{body}: {error}");
        }
        let error = invalid(
            "int a[2]; function automatic void set(ref int x); x = 1; endfunction \
             function automatic void f(const ref int v[2]); set(v[0]); endfunction \
             initial f(a);",
        );
        assert!(error.contains("const ref"), "{error}");
        let error = invalid(
            "int a[2]; task automatic load(const ref int v[2]); $readmemh(\"data.hex\", v); \
             endtask initial load(a);",
        );
        assert!(error.contains("const ref"), "{error}");
        let error = invalid(
            "int q[$]; function automatic void f(const ref int x[$]); x.push_back(1); \
             endfunction initial f(q);",
        );
        assert!(error.contains("const ref"), "{error}");
        let error = invalid(
            "int q[$][$]; function automatic void f(const ref int x[$][$]); x[0].push_back(1); \
             endfunction initial f(q);",
        );
        assert!(error.contains("const ref"), "{error}");
        // `const` is part of the direction the next formal inherits (13.3).
        let error = invalid(
            "int g, h; function automatic void f(const ref int a, b); b = 1; endfunction \
             initial f(g, h);",
        );
        assert!(error.contains("`b`"), "{error}");
        let error = invalid(
            "int g; function automatic int mut(ref int y); y = 1; return 0; endfunction \
             function automatic void f(const ref int x); logic [3:0] a; a[mut(x)] = 1'b1; \
             endfunction initial f(g);",
        );
        assert!(error.contains("const ref"), "{error}");
    }

    // `$root` of the two top modules uses `p`, whose dispatcher of `f` also
    // calls the override of `q::B`, which the specialization `p::Reg#(B)`
    // made `p` see: the override's `rw` is const ref too (8.20).
    #[test]
    fn dispatchers_pass_const_ref_formals_to_overrides_of_other_packages() {
        analyze_source(
            "package p; typedef struct { int kind; } op_t; \
             virtual class A; pure virtual function int f(const ref op_t rw); endclass \
             class Reg #(type T = int); \
             static function A create(); T t = new; return t; endfunction endclass \
             endpackage \
             package q; class B extends p::A; typedef p::Reg#(B) type_id; \
             virtual function int f(const ref p::op_t rw); return rw.kind + 1; endfunction \
             endclass endpackage \
             module Top; initial begin p::A a; p::op_t o; o.kind = 4; \
             a = q::B::type_id::create(); $display(a.f(o)); end endmodule \
             module Other; endmodule",
            Path::new("const_ref_dispatch.sv"),
        )
        .unwrap();
    }

    #[test]
    fn reference_formals_follow_automatic_variable_rules() {
        let error = invalid("int g; task automatic f(ref int x); x <= 1; endtask initial f(g);");
        assert!(error.contains("nonblocking"), "{error}");
        for join in ["join_none", "join_any"] {
            let error = invalid(&format!(
                "int g; task automatic f(ref int x); fork x = 1; {join} endtask initial f(g);"
            ));
            assert!(error.contains("9.3.2"), "{join}: {error}");
        }
        analyze_source(
            "module Top; int g; task automatic f(ref int x); \
             fork automatic int k = x; $display(k); join_none \
             fork x = 2; join endtask initial f(g); endmodule",
            Path::new("ref_fork_initializer.sv"),
        )
        .unwrap();
    }

    #[test]
    fn functions_with_ref_arguments_are_called_procedurally() {
        let error = invalid(
            "int g, r; function automatic int f(ref int x); return x; endfunction \
             assign r = f(g);",
        );
        assert!(error.contains("13.4.1"), "{error}");
        let error = invalid(
            "int g; logic [15:0] a, b; function automatic int f(ref int x); return x; \
             endfunction assign {a, b} = f(g);",
        );
        assert!(error.contains("13.4.1"), "{error}");
        analyze_source(
            "module Top; int g, r; function automatic int f(const ref int x); return x; \
             endfunction assign r = f(g); endmodule",
            Path::new("const_ref_continuous.sv"),
        )
        .unwrap();
    }

    #[test]
    fn const_ref_ports_are_read_only_and_ref_ports_allow_module_constructs() {
        for bad in [
            "module Sub(const ref int x); initial x = 1; endmodule",
            "module Sub(const ref logic clk, const ref int x); always_ff @(posedge clk) x <= 1; endmodule",
            "module Sub(const ref int x); assign x = 2; endmodule",
            "module Sub(const ref int x); always_comb x = 3; endmodule",
            "module Sub(const ref int q[$]); initial q.push_back(1); endmodule",
            "module Sub(const ref int x); task automatic w(ref int z); z = 1; endtask initial w(x); endmodule",
            "module Sub(const ref struct { int a; bit b; } s); initial s.a = 5; endmodule",
            "module Sub(const ref int q[$][$]); initial q[0].push_back(1); endmodule",
            "module Sub(const ref int q[$][$]); initial q[0][0] = 2; endmodule",
            "module Sub(const ref int q[$][$]); initial q[0].delete(); endmodule",
            "module Sub(const ref logic [127:0] q[$]); initial q[0] = 128'd1; endmodule",
            "package p; typedef struct { int x; } s_t; endpackage module Sub(const ref p::s_t q[$]); initial q[0].x = 1; endmodule",
            "module Sub(const ref int x); logic [3:0] a; function automatic int f(ref int y); y = 1; return 0; endfunction initial a[f(x)] = 1'b1; endmodule",
            "module Sub(const ref int x); logic [3:0] a; logic b; function automatic int f(ref int y); y = 1; return 0; endfunction initial {a[f(x)], b} = 2'b10; endmodule",
            "module Sub(const ref int x); logic [3:0] a; logic y; function automatic int f(ref int z); z = 1; return 0; endfunction initial y = a[f(x)]; endmodule",
        ] {
            let result = analyze_source(bad, Path::new("const_ref_port_write.sv"));
            assert!(
                matches!(result, Err(AnalyzerError::ConstRefPortAssignment { .. })),
                "{bad}: got {result:?}"
            );
        }

        for allowed in [
            "package p; class Box; int items[$]; endclass endpackage
             module Sub(const ref p::Box b);
                initial if (b != null) b.items.push_back(1);
             endmodule",
            "package p; class Box; int items[$]; endclass endpackage
             module Sub(const ref p::Box q[$]);
                initial if (q[0] != null) q[0].items.push_back(1);
             endmodule",
        ] {
            analyze_source(allowed, Path::new("const_ref_class_prop_write.sv"))
                .unwrap_or_else(|e| panic!("{allowed}: {e}"));
        }

        analyze_source(
            "module Sub(const ref logic clk, ref int x, ref int q[$], ref struct { int a; bit b; } s);
                task t(input int a); endtask
                always_ff @(posedge clk) x <= x + 1;
                initial begin
                    t(q.size());
                    fork
                        x = 10;
                    join_none
                    s.a = 7;
                end
            endmodule",
            Path::new("ref_port_valid.sv"),
        )
        .unwrap();

        for unsupported in [
            "module Sub(ref logic [7:0] x); initial force x = 8'hff; endmodule",
            "module Sub(ref logic [7:0] x); initial assign x = 8'h11; endmodule",
        ] {
            let result = analyze_source(unsupported, Path::new("ref_port_unsupported.sv"));
            assert!(
                matches!(result, Err(AnalyzerError::Unsupported(_))),
                "{unsupported}: got {result:?}"
            );
        }
    }

    #[test]
    fn ref_port_type_equivalence_handles_packed_arrays_queues_and_objects() {
        let source = r#"
            package p;
                typedef struct { logic [7:0] x; int y; } s_t;
                typedef enum { A, B } e1_t;
                typedef enum { C, D } e2_t;
                class Base; int v; endclass
                class Derived extends Base; int w; endclass
            endpackage
            module M(
                ref int r_int,
                ref bit signed [31:0] r_bit_signed,
                ref int unsigned r_uint,
                ref int r_arr4[4],
                ref int r_arr_bounds[0:3],
                ref int r_q[$],
                ref int r_q1[$:1],
                ref int r_qq[$][$],
                ref int r_qq1[$][$:1],
                ref int r_qq2[$][$:2],
                ref int r_qq2_b[$][$:2],
                ref int r_dq1[][$:1],
                ref int r_dq2[][$:2],
                ref p::s_t r_s,
                ref p::e1_t r_e1,
                ref p::e2_t r_e2,
                ref p::Base r_base,
                ref p::Derived r_derived,
                ref string r_str,
                ref event r_ev
            );
                p::s_t s_var;
                int dyn_arr[];
            endmodule
        "#;
        let path = Path::new("ref_equiv.sv");
        let packages = crate::analyze_packages(&[(source, path)]).unwrap();
        let mut design = packages.design();
        let ir = crate::analyze_source_module_with_packages(
            source,
            path,
            "M",
            &crate::HashMap::default(),
            &packages,
            &mut design,
        )
        .unwrap();
        let m = ir.modules().iter().find(|m| m.name() == "M").unwrap();
        let port = |name: &str| {
            m.ports()
                .iter()
                .find(|p| p.name() == name)
                .unwrap()
                .r#type()
        };
        assert_eq!(
            design.check_ref_port_type_equivalent(port("r_bit_signed"), port("r_int")),
            Ok(())
        );
        assert_eq!(
            design.check_ref_port_type_equivalent(port("r_arr4"), port("r_arr_bounds")),
            Ok(())
        );
        assert_eq!(
            design.check_ref_port_type_equivalent(port("r_str"), port("r_str")),
            Ok(())
        );
        assert_eq!(
            design.check_ref_port_type_equivalent(port("r_ev"), port("r_ev")),
            Ok(())
        );
        assert_eq!(
            design.check_ref_port_type_equivalent(port("r_uint"), port("r_int")),
            Err(crate::RefPortTypeError::NotEquivalent)
        );
        assert_eq!(
            design.check_ref_port_type_equivalent(port("r_e1"), port("r_e2")),
            Err(crate::RefPortTypeError::NotEquivalent)
        );
        assert_eq!(
            design.check_ref_port_type_equivalent(port("r_derived"), port("r_base")),
            Err(crate::RefPortTypeError::NotEquivalent)
        );
        assert_eq!(
            design.check_ref_port_type_equivalent(port("r_q1"), port("r_q")),
            Err(crate::RefPortTypeError::QueueBoundsDiffer)
        );
        assert_eq!(
            design.check_ref_port_type_equivalent(port("r_qq1"), port("r_qq2")),
            Err(crate::RefPortTypeError::QueueBoundsDiffer)
        );
        assert_eq!(
            design.check_ref_port_type_equivalent(port("r_qq"), port("r_qq1")),
            Err(crate::RefPortTypeError::QueueBoundsDiffer)
        );
        assert_eq!(
            design.check_ref_port_type_equivalent(port("r_dq1"), port("r_dq2")),
            Err(crate::RefPortTypeError::QueueBoundsDiffer)
        );
        assert_eq!(
            design.check_ref_port_type_equivalent(port("r_qq2"), port("r_qq2_b")),
            Ok(())
        );
    }
}
