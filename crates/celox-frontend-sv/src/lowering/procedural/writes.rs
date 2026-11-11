//! What a statement list writes: by assignments, through the written
//! arguments of calls, and in the bodies of its callees.

use super::*;

impl ProcModule<'_> {
    /// The variables `body` may write, directly, through the output and
    /// inout arguments of the calls of statements and expressions, or in the
    /// bodies of its callees. Every name of a written argument counts, and
    /// names a callee declares itself may resolve to a variable of the module
    /// too, so the set may be larger than the writes.
    pub fn written_variables(&self, body: &[sv::ir::Stmt]) -> HashSet<SourceVarId> {
        let mut names = HashSet::default();
        let mut calls = Vec::new();
        written_names(body, &mut names);
        for stmt in body {
            stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
        }
        let mut visited = HashSet::default();
        let mut seen_defaults = SeenDefaults::default();
        while let Some((callee, args)) = calls.pop() {
            let name = match callee {
                sv::ir::Callee::System(function) => {
                    for (position, arg) in args.iter().enumerate() {
                        if function.writes_argument(position)
                            && let Some(arg) = arg
                        {
                            expr_idents(arg, &mut names);
                        }
                    }
                    continue;
                }
                sv::ir::Callee::Subroutine(name) => name,
                _ => continue,
            };
            if let Some(subroutine) = self.subroutine(&name) {
                for (param, arg) in subroutine.params.iter().zip(&args) {
                    if param.direction.is_written()
                        && let Some(arg) = arg
                    {
                        expr_idents(arg, &mut names);
                    }
                }
                default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
                if visited.insert(name) {
                    written_names(&subroutine.body, &mut names);
                    for stmt in &subroutine.body {
                        stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
                    }
                }
            }
        }
        names.iter().filter_map(|name| self.id(name)).collect()
    }
}

/// The names written by the assignments (and through call arguments) of a
/// Whether a run of `stmts` may store a variable twice, given the variables
/// `written` on some path before them. Locals are not variables of the
/// design, and a loop counts as storing twice.
pub(super) fn stores_twice(stmts: &[sv::ir::Stmt], written: &mut HashSet<String>) -> bool {
    let mut locals = HashSet::default();
    stores_twice_in(stmts, written, &mut locals)
}

pub(super) fn stores_twice_in(
    stmts: &[sv::ir::Stmt],
    written: &mut HashSet<String>,
    locals: &mut HashSet<String>,
) -> bool {
    for stmt in stmts {
        match stmt {
            sv::ir::Stmt::Local { name, .. } => {
                locals.insert(name.clone());
            }
            sv::ir::Stmt::Assign { .. } | sv::ir::Stmt::AssignConcat { .. } => {
                let mut names = HashSet::default();
                written_names(std::slice::from_ref(stmt), &mut names);
                for name in names {
                    if !locals.contains(&name) && !written.insert(name) {
                        return true;
                    }
                }
            }
            sv::ir::Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                let mut other = written.clone();
                if stores_twice_in(then_body, written, locals)
                    || stores_twice_in(else_body, &mut other, locals)
                {
                    return true;
                }
                written.extend(other);
            }
            sv::ir::Stmt::Case { items, default, .. } => {
                let before = written.clone();
                for body in items.iter().map(|item| &item.body).chain(default.iter()) {
                    let mut branch = before.clone();
                    if stores_twice_in(body, &mut branch, locals) {
                        return true;
                    }
                    written.extend(branch);
                }
            }
            _ => return true,
        }
    }
    false
}

/// statement list.
pub(in crate::lowering) fn written_names(stmts: &[sv::ir::Stmt], names: &mut HashSet<String>) {
    for stmt in stmts {
        stmt.walk(&mut |stmt| match stmt {
            sv::ir::Stmt::Assign { lhs, .. } => {
                names.insert(lhs.name().to_string());
            }
            sv::ir::Stmt::AssignConcat { parts, .. } => {
                names.extend(parts.iter().map(|part| part.name().to_string()));
            }
            sv::ir::Stmt::Local { name, .. } => {
                names.insert(name.clone());
            }
            sv::ir::Stmt::Call { args, .. } => {
                for arg in args.iter().flatten() {
                    expr_idents(arg, names);
                }
            }
            _ => {}
        });
    }
}

/// The names a statement list of `module` writes, also through the
/// subroutines it calls and their written arguments.
pub(in crate::lowering) fn written_names_with_calls(
    module: &sv::ir::Module,
    stmts: &[sv::ir::Stmt],
    names: &mut HashSet<String>,
) {
    super::super::kernel_written_names(
        &super::super::DriverScan::of(module),
        stmts,
        names,
        &mut HashSet::default(),
        0,
        &mut None,
    );
}

impl ProcModule<'_> {
    /// The names `body` writes, by assignments and through the written
    /// arguments of the system tasks, functions and subroutines it calls.
    /// The actual of a `ref` formal is written when the callee writes the
    /// formal, also by passing it on to another `ref` formal it writes
    /// (formals have module-unique names).
    pub(in crate::lowering) fn written_names(&self, body: &[sv::ir::Stmt]) -> HashSet<String> {
        let mut names = HashSet::default();
        let mut refs = Some(Vec::new());
        super::super::kernel_written_names(
            self,
            body,
            &mut names,
            &mut HashSet::default(),
            0,
            &mut refs,
        );
        let refs = refs.unwrap_or_default();
        loop {
            let mut grew = false;
            for (formal, actuals) in &refs {
                if names.contains(formal) {
                    for actual in actuals {
                        grew |= names.insert(actual.clone());
                    }
                }
            }
            if !grew {
                return names;
            }
        }
    }
}

/// The lvalue an output argument writes back to.
/// The variables a call of `function` writes through its arguments, such as
/// the seed of `$random(seed)`.
pub(in crate::lowering) fn written_system_arguments(
    function: sv::ir::SystemFunction,
    args: &[Option<sv::ir::Expr>],
) -> Vec<sv::ir::LValue> {
    args.iter()
        .enumerate()
        .filter(|(position, _)| function.writes_argument(*position))
        .filter_map(|(_, arg)| arg.as_ref().and_then(lvalue_from_expr))
        .flatten()
        .collect()
}

/// The variables a call of DPI-C import `import` writes through its output
/// arguments (IEEE 1800-2023 35.5.1.2).
pub(in crate::lowering) fn written_dpi_arguments(
    import: &sv::ir::DpiImport,
    args: &[Option<sv::ir::Expr>],
) -> Vec<sv::ir::LValue> {
    import
        .arguments()
        .iter()
        .zip(args)
        .filter(|(argument, _)| argument.is_output())
        .filter_map(|(_, arg)| arg.as_ref().and_then(lvalue_from_expr))
        .flatten()
        .collect()
}

/// State writes performed by heap container calls.
pub(in crate::lowering) fn written_object_arguments(
    op: &sv::ir::ObjectOp,
    args: &[Option<sv::ir::Expr>],
) -> Vec<sv::ir::LValue> {
    args.iter()
        .enumerate()
        .filter(|(position, _)| op.writes_argument(*position))
        .filter_map(|(_, arg)| arg.as_ref().and_then(lvalue_from_expr))
        .flatten()
        .collect()
}

/// Keep checked member references writable while carrying their live tag checks
/// separately. Callers that perform stores must execute these checks.
pub(in crate::lowering) fn tagged_actual_guards(expr: &sv::ir::Expr) -> Vec<sv::ir::Expr> {
    use sv::ir::{Callee, Expr};
    let mut guards = Vec::new();
    let mut pending = vec![expr];
    while let Some(expr) = pending.pop() {
        match expr {
            Expr::Call {
                callee: Callee::Checked { .. },
                args,
                ..
            } => {
                if let [Some(guard), Some(value)] = args.as_slice() {
                    guards.push(guard.clone());
                    pending.push(value);
                }
            }
            Expr::Resize { expr, .. } | Expr::Select { expr, .. } => pending.push(expr),
            Expr::Concat(parts) => pending.extend(parts.iter().rev()),
            _ => {}
        }
    }
    guards
}

pub(in crate::lowering) fn lvalue_from_expr(expr: &sv::ir::Expr) -> Option<Vec<sv::ir::LValue>> {
    match expr {
        sv::ir::Expr::Ident(name) => Some(vec![sv::ir::LValue::Ident(name.clone())]),
        sv::ir::Expr::Select { expr, msb, lsb, .. } => match &**expr {
            sv::ir::Expr::Ident(name) => Some(vec![sv::ir::LValue::Select {
                name: name.clone(),
                msb: msb.clone(),
                lsb: lsb.clone(),
                array_slice_width: None,
                array_slice_reversed: false,
                window: None,
            }]),
            _ => None,
        },
        sv::ir::Expr::Concat(parts) => {
            let mut lvalues = Vec::new();
            for part in parts {
                lvalues.extend(lvalue_from_expr(part)?);
            }
            Some(lvalues)
        }
        sv::ir::Expr::Resize { expr, .. } => lvalue_from_expr(expr),
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::Checked { .. },
            args,
            ..
        } => match args.as_slice() {
            [Some(_), Some(value)] => lvalue_from_expr(value),
            _ => None,
        },
        _ => None,
    }
}
