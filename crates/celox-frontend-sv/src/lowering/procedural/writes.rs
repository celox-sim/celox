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

impl ProcModule<'_> {
    /// The width and signedness of integral variable `name`, such as the
    /// variable of a `for` loop (see [`LoopBounds`]).
    pub fn integral_variable_type(&self, name: &str) -> Option<(usize, bool)> {
        let variable = self.var(self.id(name)?);
        (variable.float.is_none() && variable.array_dims.is_empty())
            .then_some((variable.width, variable.signed))
    }

    /// The bits of its variable that `lvalue`, a select at run-time
    /// positions, may write: the written value keeps the other bits, so it
    /// defines the whole vector, or the array element or structure member
    /// the select stays within (see `dynamic_packed_write`).
    pub fn dynamic_select_access(&self, lvalue: &sv::ir::LValue) -> Option<BitAccess> {
        let variable = self.var(self.id(lvalue.name())?);
        let element = dynamic_array_element_lvalue(
            lvalue,
            self.variables,
            self.name_to_id,
            self.constants,
            self.parameter_types,
        );
        Some(
            element
                .is_none()
                .then(|| {
                    dynamic_packed_write(
                        lvalue,
                        self.variables,
                        self.name_to_id,
                        self.constants,
                        self.parameter_types,
                    )
                })
                .flatten()
                .and_then(|write| write.window)
                .unwrap_or(BitAccess::new(0, variable.width - 1)),
        )
    }

    /// The bits of its variable that `lvalue`, a select whose positions
    /// read the variables of the `for` loops `loops` around it and no other
    /// run-time value, writes in their iterations, as the loops unrolled
    /// write them (IEEE 1800-2023 6.5, 12.7.1): in each iteration the bits
    /// it selects there, or, where those leave the array element or
    /// structure member the select stays within, that element or member,
    /// which its run-time form writes (see `keep_element_window`). `None`
    /// for another select.
    pub fn unrolled_select_writes(
        &self,
        lvalue: &sv::ir::LValue,
        loops: &LoopValues,
    ) -> Option<Vec<BitAccess>> {
        let sv::ir::LValue::Select { msb, lsb, .. } = lvalue else {
            return None;
        };
        let variable = self.var(self.id(lvalue.name())?);
        let bounds = unrolled_select_bounds(msb, lsb, loops, self.constants, self.parameter_types)?;
        let mut accesses = Vec::with_capacity(bounds.len());
        for (msb, lsb) in bounds {
            let mut folded = lvalue.clone();
            if let sv::ir::LValue::Select {
                msb: folded_msb,
                lsb: folded_lsb,
                ..
            } = &mut folded
            {
                *folded_msb = const_expr_from_i128(msb);
                *folded_lsb = const_expr_from_i128(lsb);
            }
            let folded = keep_element_window(
                lvalue,
                folded,
                self.variables,
                self.name_to_id,
                self.constants,
                self.parameter_types,
            );
            if folded == *lvalue {
                accesses.extend(self.dynamic_select_access(lvalue));
                continue;
            }
            // A select entirely outside the vector writes nothing.
            let (access, _) = constant_select_write(variable, msb, lsb)?;
            accesses.extend(access);
        }
        Some(accesses)
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
            // A select of a selected part of a variable, such as one of a
            // two-state member read through its conversion, selects bits
            // of the variable from the part's lowest bit.
            inner => {
                let parts = lvalue_from_expr(inner)?;
                let [
                    sv::ir::LValue::Select {
                        name,
                        msb: top,
                        lsb: base,
                        array_slice_width: None,
                        window: None,
                        ..
                    },
                ] = parts.as_slice()
                else {
                    return None;
                };
                let offset = |bound: &sv::ir::ConstExpr| sv::ir::ConstExpr::Binary {
                    left: Box::new(base.clone()),
                    op: sv::ir::BinaryOp::Add,
                    right: Box::new(bound.clone()),
                };
                // A run-time select of the part, such as one of a member of
                // a packed structure, writes only the part's bits (IEEE
                // 1800-2023 11.5.1): the part is its window.
                let literal = |bound: &sv::ir::ConstExpr| match bound {
                    sv::ir::ConstExpr::Literal(value) => value.parse::<u32>().ok(),
                    _ => None,
                };
                let window = if literal(msb).is_some() && literal(lsb).is_some() {
                    None
                } else {
                    Some(sv::ir::SelectWindow::bits(literal(base)?, literal(top)?))
                };
                Some(vec![sv::ir::LValue::Select {
                    name: name.clone(),
                    msb: offset(msb),
                    lsb: offset(lsb),
                    array_slice_width: None,
                    array_slice_reversed: false,
                    window,
                }])
            }
        },
        sv::ir::Expr::Concat(parts) => {
            let mut lvalues = Vec::new();
            for part in parts {
                lvalues.extend(lvalue_from_expr(part)?);
            }
            Some(lvalues)
        }
        sv::ir::Expr::Resize { expr, .. }
        | sv::ir::Expr::Unary {
            op: sv::ir::UnaryOp::ToTwoState,
            expr,
        } => lvalue_from_expr(expr),
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
