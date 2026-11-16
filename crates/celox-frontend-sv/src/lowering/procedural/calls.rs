//! The calls of expressions and statements: whether one calls a
//! subroutine, and the calls it makes, with those in the defaults of
//! omitted arguments.

use super::*;

impl ProcModule<'_> {
    /// Whether a call of `callee` has effects or reads the state of the
    /// simulation: a user subroutine or a DPI import, or a system function
    /// such as `$urandom` or `$time`.
    pub fn is_callee(&self, callee: &sv::ir::Callee) -> bool {
        match callee {
            sv::ir::Callee::System(function) => function.reads_simulation_state(),
            sv::ir::Callee::Lookup { .. } | sv::ir::Callee::Stream(_) => false,
            sv::ir::Callee::String(_)
            | sv::ir::Callee::Object(_)
            | sv::ir::Callee::Checked { .. } => true,
            sv::ir::Callee::Subroutine(name) => {
                self.subroutines.contains_key(name) || self.dpi_imports.contains_key(name)
            }
        }
    }

    /// Whether an expression contains a call whose evaluation has effects.
    pub fn lvalue_calls(&self, lvalue: &sv::ir::LValue) -> bool {
        lvalue_calls(lvalue, &mut |callee| self.is_callee(callee))
    }

    pub fn const_calls(&self, expr: &sv::ir::ConstExpr) -> bool {
        const_calls(expr, &mut |callee| self.is_callee(callee))
    }

    /// The largest subexpressions of `expr` that call a subroutine and also
    /// occur in `other`, left to right.
    pub fn shared_calls<'e>(
        &self,
        expr: &'e sv::ir::ConstExpr,
        other: &sv::ir::ConstExpr,
    ) -> Vec<&'e sv::ir::ConstExpr> {
        let mut shared = Vec::new();
        shared_calls(
            expr,
            other,
            &mut |callee| self.is_callee(callee),
            &mut shared,
        );
        shared
    }

    pub fn calls(&self, expr: &sv::ir::Expr) -> bool {
        expr_calls(expr, &mut |callee| self.is_callee(callee))
    }
}

/// Whether a constant-expression operand, such as a run-time select index,
/// calls a subroutine.
pub(in crate::lowering) fn const_calls(
    expr: &sv::ir::ConstExpr,
    is_callee: &mut dyn FnMut(&sv::ir::Callee) -> bool,
) -> bool {
    use sv::ir::ConstExpr;
    match expr {
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => false,
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            const_calls(expr, is_callee)
                || const_calls(msb, is_callee)
                || const_calls(lsb, is_callee)
        }
        ConstExpr::Select { expr, bit } => {
            const_calls(expr, is_callee) || const_calls(bit, is_callee)
        }
        ConstExpr::Function { callee, args, .. } => {
            is_callee(callee) || args.iter().any(|arg| const_calls(arg, is_callee))
        }
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
            const_calls(expr, is_callee)
        }
        ConstExpr::Binary { left, right, .. } => {
            const_calls(left, is_callee) || const_calls(right, is_callee)
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            const_calls(condition, is_callee)
                || const_calls(then_expr, is_callee)
                || const_calls(else_expr, is_callee)
        }
    }
}

/// Whether `expr` has `part` as a subexpression.
pub(super) fn const_contains(expr: &sv::ir::ConstExpr, part: &sv::ir::ConstExpr) -> bool {
    use sv::ir::ConstExpr;
    expr == part
        || match expr {
            ConstExpr::Literal(_) | ConstExpr::Ident(_) => false,
            ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                const_contains(expr, part) || const_contains(msb, part) || const_contains(lsb, part)
            }
            ConstExpr::Select { expr, bit } => {
                const_contains(expr, part) || const_contains(bit, part)
            }
            ConstExpr::Function { args, .. } => args.iter().any(|arg| const_contains(arg, part)),
            ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
                const_contains(expr, part)
            }
            ConstExpr::Binary { left, right, .. } => {
                const_contains(left, part) || const_contains(right, part)
            }
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                const_contains(condition, part)
                    || const_contains(then_expr, part)
                    || const_contains(else_expr, part)
            }
        }
}

/// Collect the largest subexpressions of `expr` that call a subroutine and
/// also occur in `other`, left to right.
pub(super) fn shared_calls<'e>(
    expr: &'e sv::ir::ConstExpr,
    other: &sv::ir::ConstExpr,
    is_callee: &mut dyn FnMut(&sv::ir::Callee) -> bool,
    shared: &mut Vec<&'e sv::ir::ConstExpr>,
) {
    use sv::ir::ConstExpr;
    if !const_calls(expr, is_callee) {
        return;
    }
    if const_contains(other, expr) {
        shared.push(expr);
        return;
    }
    match expr {
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => {}
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            shared_calls(expr, other, is_callee, shared);
            shared_calls(msb, other, is_callee, shared);
            shared_calls(lsb, other, is_callee, shared);
        }
        ConstExpr::Select { expr, bit } => {
            shared_calls(expr, other, is_callee, shared);
            shared_calls(bit, other, is_callee, shared);
        }
        ConstExpr::Function { args, .. } => args
            .iter()
            .for_each(|arg| shared_calls(arg, other, is_callee, shared)),
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
            shared_calls(expr, other, is_callee, shared)
        }
        ConstExpr::Binary { left, right, .. } => {
            shared_calls(left, other, is_callee, shared);
            shared_calls(right, other, is_callee, shared);
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            shared_calls(condition, other, is_callee, shared);
            shared_calls(then_expr, other, is_callee, shared);
            shared_calls(else_expr, other, is_callee, shared);
        }
    }
}

/// Whether the select positions of an assignment target call a subroutine.
pub(in crate::lowering) fn lvalue_calls(
    lvalue: &sv::ir::LValue,
    is_callee: &mut dyn FnMut(&sv::ir::Callee) -> bool,
) -> bool {
    match lvalue {
        sv::ir::LValue::Ident(_) => false,
        sv::ir::LValue::Select { msb, lsb, .. } => {
            const_calls(msb, is_callee) || const_calls(lsb, is_callee)
        }
    }
}

/// Whether an expression calls a user subroutine.
pub(in crate::lowering) fn expr_calls(
    expr: &sv::ir::Expr,
    is_callee: &mut dyn FnMut(&sv::ir::Callee) -> bool,
) -> bool {
    match expr {
        sv::ir::Expr::Ident(_) | sv::ir::Expr::Literal(_) => false,
        sv::ir::Expr::Select { expr, msb, lsb, .. } => {
            expr_calls(expr, is_callee)
                || const_calls(msb, is_callee)
                || const_calls(lsb, is_callee)
        }
        sv::ir::Expr::Resize { expr, .. } | sv::ir::Expr::Unary { expr, .. } => {
            expr_calls(expr, is_callee)
        }
        sv::ir::Expr::Concat(parts) | sv::ir::Expr::RepeatConcat { parts, .. } => {
            parts.iter().any(|part| expr_calls(part, is_callee))
        }
        sv::ir::Expr::Binary { left, right, .. } => {
            expr_calls(left, is_callee) || expr_calls(right, is_callee)
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_calls(condition, is_callee)
                || expr_calls(then_expr, is_callee)
                || expr_calls(else_expr, is_callee)
        }
        sv::ir::Expr::Call { callee, args, .. } => {
            is_callee(callee) || args.iter().flatten().any(|arg| expr_calls(arg, is_callee))
        }
        sv::ir::Expr::Inside { expr, items } => {
            expr_calls(expr, is_callee)
                || items
                    .iter()
                    .flat_map(sv::ir::InsideItem::exprs)
                    .any(|operand| expr_calls(operand, is_callee))
        }
    }
}

/// The calls an expression makes, outermost first, with their arguments.
pub(in crate::lowering) fn collect_calls(
    expr: &sv::ir::Expr,
    calls: &mut Vec<(sv::ir::Callee, Vec<Option<sv::ir::Expr>>)>,
) {
    match expr {
        sv::ir::Expr::Call { callee, args, .. } => {
            calls.push((callee.clone(), args.clone()));
            for arg in args.iter().flatten() {
                collect_calls(arg, calls);
            }
        }
        sv::ir::Expr::Ident(_) | sv::ir::Expr::Literal(_) => {}
        sv::ir::Expr::Select { expr, msb, lsb, .. } => {
            collect_calls(expr, calls);
            collect_const_calls(msb, calls);
            collect_const_calls(lsb, calls);
        }
        sv::ir::Expr::Resize { expr, .. } | sv::ir::Expr::Unary { expr, .. } => {
            collect_calls(expr, calls)
        }
        sv::ir::Expr::Concat(parts) => parts.iter().for_each(|part| collect_calls(part, calls)),
        sv::ir::Expr::RepeatConcat { count, parts } => {
            collect_const_calls(count, calls);
            parts.iter().for_each(|part| collect_calls(part, calls))
        }
        sv::ir::Expr::Binary { left, right, .. } => {
            collect_calls(left, calls);
            collect_calls(right, calls);
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_calls(condition, calls);
            collect_calls(then_expr, calls);
            collect_calls(else_expr, calls);
        }
        sv::ir::Expr::Inside { expr, items } => {
            collect_calls(expr, calls);
            for item in items {
                item.exprs()
                    .into_iter()
                    .for_each(|operand| collect_calls(operand, calls));
            }
        }
    }
}

/// The calls of a constant-expression operand, such as a run-time select
/// position.
pub(super) fn collect_const_calls(
    expr: &sv::ir::ConstExpr,
    calls: &mut Vec<(sv::ir::Callee, Vec<Option<sv::ir::Expr>>)>,
) {
    use sv::ir::ConstExpr;
    match expr {
        ConstExpr::Function { callee, args, .. } => {
            calls.push((
                callee.clone(),
                args.iter()
                    .map(|arg| expr_from_const_expr_with_calls(arg, true))
                    .collect(),
            ));
            args.iter().for_each(|arg| collect_const_calls(arg, calls));
        }
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => {}
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            collect_const_calls(expr, calls);
            collect_const_calls(msb, calls);
            collect_const_calls(lsb, calls);
        }
        ConstExpr::Select { expr, bit } => {
            collect_const_calls(expr, calls);
            collect_const_calls(bit, calls);
        }
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
            collect_const_calls(expr, calls)
        }
        ConstExpr::Binary { left, right, .. } => {
            collect_const_calls(left, calls);
            collect_const_calls(right, calls);
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_const_calls(condition, calls);
            collect_const_calls(then_expr, calls);
            collect_const_calls(else_expr, calls);
        }
    }
}

/// The calls a statement makes in its own expressions (not in the statements
/// it contains), including a call statement itself.
/// The calls in the select positions of an assignment target.
pub(super) fn lvalue_position_calls(
    lvalue: &sv::ir::LValue,
    calls: &mut Vec<(sv::ir::Callee, Vec<Option<sv::ir::Expr>>)>,
) {
    if let sv::ir::LValue::Select { msb, lsb, .. } = lvalue {
        collect_const_calls(msb, calls);
        collect_const_calls(lsb, calls);
    }
}

pub(in crate::lowering) fn stmt_calls(
    stmt: &sv::ir::Stmt,
    calls: &mut Vec<(sv::ir::Callee, Vec<Option<sv::ir::Expr>>)>,
) {
    let mut exprs: Vec<&sv::ir::Expr> = Vec::new();
    match stmt {
        sv::ir::Stmt::Call { name, args, .. } => {
            calls.push((sv::ir::Callee::Subroutine(name.clone()), args.clone()));
            exprs.extend(args.iter().flatten());
        }
        sv::ir::Stmt::Assign { lhs, rhs, .. } => {
            lvalue_position_calls(lhs, calls);
            exprs.push(rhs);
        }
        sv::ir::Stmt::AssignConcat { parts, rhs, .. } => {
            parts
                .iter()
                .for_each(|part| lvalue_position_calls(part, calls));
            exprs.push(rhs);
        }
        sv::ir::Stmt::Eval(rhs) => exprs.push(rhs),
        sv::ir::Stmt::If { condition, .. } => exprs.push(condition),
        sv::ir::Stmt::Case {
            selector, items, ..
        } => {
            exprs.push(selector);
            for item in items {
                for label in &item.labels {
                    match label {
                        sv::ir::CaseLabel::Value(value) => exprs.push(value),
                        sv::ir::CaseLabel::Range { low, high } => {
                            exprs.push(low);
                            exprs.push(high);
                        }
                    }
                }
            }
        }
        sv::ir::Stmt::Loop {
            kind, condition, ..
        } => {
            if let sv::ir::LoopKind::Repeat(count) = kind {
                exprs.push(count);
            }
            exprs.extend(condition);
        }
        sv::ir::Stmt::Return(Some(value)) => exprs.push(value),
        sv::ir::Stmt::Local {
            init: Some(init), ..
        } => exprs.push(init),
        sv::ir::Stmt::SystemTask { args, .. } => {
            for arg in args {
                if let sv::ir::SystemTaskArg::Expr(expr) = arg {
                    exprs.push(expr);
                }
            }
        }
        // Timing controls evaluate their expressions too.
        sv::ir::Stmt::Wait(condition) => exprs.push(condition),
        sv::ir::Stmt::WaitEvent(items) => {
            for item in items {
                exprs.push(&item.expr);
                exprs.extend(&item.iff);
            }
        }
        sv::ir::Stmt::Delay { amount, .. } => exprs.push(amount),
        _ => {}
    }
    for expr in exprs {
        collect_calls(expr, calls);
    }
}

/// The defaults a walk over calls has taken the calls of: each subroutine
/// with the omitted arguments of a call of it.
pub(in crate::lowering) type SeenDefaults = HashSet<(String, Vec<bool>)>;

/// The calls in the default values a call uses for its omitted arguments,
/// unless `seen` has them already: a walk takes the defaults of each
/// subroutine once for each pattern of omitted arguments, since defaults
/// that call each other would otherwise be walked forever.
pub(in crate::lowering) fn default_calls(
    subroutine: &sv::ir::Subroutine,
    args: &[Option<sv::ir::Expr>],
    calls: &mut Vec<(sv::ir::Callee, Vec<Option<sv::ir::Expr>>)>,
    seen: &mut SeenDefaults,
) {
    let omitted: Vec<bool> = (0..subroutine.params.len())
        .map(|position| matches!(args.get(position), None | Some(None)))
        .collect();
    if !seen.insert((subroutine.name.clone(), omitted)) {
        return;
    }
    for (position, param) in subroutine.params.iter().enumerate() {
        if matches!(args.get(position), None | Some(None))
            && let Some(default) = &param.default
        {
            collect_calls(default, calls);
        }
    }
}
