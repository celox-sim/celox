//! The subroutines of the packages a scope uses that the scope can call.
//!
//! A scope carries a copy of each package subroutine it calls, directly or
//! through other package subroutines. A virtual method is called through its
//! dispatcher, whose arms call every override, so the overrides are reached
//! as calls too; so are the randomization hooks a `randomize` calls.

use super::*;

/// The names of the subroutines `expr` calls.
fn expr_calls<'a>(expr: &'a Expr, calls: &mut Vec<&'a str>) {
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => {}
        Expr::Select { expr, msb, lsb, .. } => {
            expr_calls(expr, calls);
            const_calls(msb, calls);
            const_calls(lsb, calls);
        }
        Expr::Concat(parts) => parts.iter().for_each(|part| expr_calls(part, calls)),
        Expr::RepeatConcat { count, parts } => {
            const_calls(count, calls);
            parts.iter().for_each(|part| expr_calls(part, calls));
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => expr_calls(expr, calls),
        Expr::Binary { left, right, .. } => {
            expr_calls(left, calls);
            expr_calls(right, calls);
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_calls(condition, calls);
            expr_calls(then_expr, calls);
            expr_calls(else_expr, calls);
        }
        Expr::Call { callee, args, .. } => {
            calls.extend(callee.subroutine());
            args.iter().flatten().for_each(|arg| expr_calls(arg, calls));
        }
        Expr::Inside { expr, items } => {
            expr_calls(expr, calls);
            for item in items {
                match item {
                    InsideItem::Value(value) => expr_calls(value, calls),
                    InsideItem::Range { low, high } => {
                        expr_calls(low, calls);
                        expr_calls(high, calls);
                    }
                }
            }
        }
    }
}

/// The names of the subroutines a constant expression, such as a run-time
/// select position, calls.
fn const_calls<'a>(expr: &'a ConstExpr, calls: &mut Vec<&'a str>) {
    match expr {
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => {}
        ConstExpr::Select { expr, bit } => {
            const_calls(expr, calls);
            const_calls(bit, calls);
        }
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            const_calls(expr, calls);
            const_calls(msb, calls);
            const_calls(lsb, calls);
        }
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => const_calls(expr, calls),
        ConstExpr::Function { callee, args, .. } => {
            calls.extend(callee.subroutine());
            args.iter().for_each(|arg| const_calls(arg, calls));
        }
        ConstExpr::Binary { left, right, .. } => {
            const_calls(left, calls);
            const_calls(right, calls);
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            const_calls(condition, calls);
            const_calls(then_expr, calls);
            const_calls(else_expr, calls);
        }
    }
}

/// The names of the subroutines the positions, handles and guards of an
/// assignment target call.
fn lvalue_calls<'a>(lvalue: &'a LValue, calls: &mut Vec<&'a str>) {
    match lvalue {
        LValue::Ident(_) => {}
        LValue::Select {
            msb,
            lsb,
            array_slice_width,
            ..
        } => {
            const_calls(msb, calls);
            const_calls(lsb, calls);
            array_slice_width
                .iter()
                .for_each(|width| const_calls(width, calls));
        }
        LValue::Object {
            place,
            select,
            guards,
            ..
        } => {
            match &**place {
                ObjectPlace::Property { handle, .. } => expr_calls(handle, calls),
                ObjectPlace::Element {
                    container, index, ..
                } => {
                    expr_calls(container, calls);
                    expr_calls(index, calls);
                }
            }
            lvalue_calls(select, calls);
            guards.iter().for_each(|guard| expr_calls(guard, calls));
        }
    }
}

/// The names of the subroutines `stmts` call, those that only describe the
/// reads of an `@*` included.
fn stmt_calls<'a>(stmts: &'a [Stmt], calls: &mut Vec<&'a str>) {
    let mut exprs = Vec::new();
    let mut lvalues = Vec::new();
    for stmt in stmts {
        stmt.visit(
            &mut |expr| exprs.push(expr),
            &mut |lvalue| lvalues.push(lvalue),
            &mut |name| calls.push(name),
        );
    }
    exprs.into_iter().for_each(|expr| expr_calls(expr, calls));
    lvalues
        .into_iter()
        .for_each(|lvalue| lvalue_calls(lvalue, calls));
}

/// The names of the subroutines a subroutine calls, in its body, in the
/// statements that describe its reads and in the defaults of its formals.
fn subroutine_calls<'a>(subroutine: &'a Subroutine, calls: &mut Vec<&'a str>) {
    stmt_calls(&subroutine.body, calls);
    stmt_calls(&subroutine.sensitivity_body, calls);
    for param in &subroutine.params {
        param
            .default
            .iter()
            .for_each(|default| expr_calls(default, calls));
    }
}

impl ScopeElaboration<'_> {
    /// The names of the subroutines the scope's own processes, subroutines,
    /// instances and declarations call.
    fn own_calls(&self) -> Vec<&str> {
        let mut calls = Vec::new();
        for process in &self.comb_processes {
            process
                .condition
                .iter()
                .for_each(|condition| const_calls(condition, &mut calls));
            stmt_calls(&process.body, &mut calls);
            stmt_calls(&process.sensitivity_body, &mut calls);
            for assignment in &process.assignments {
                lvalue_calls(&assignment.lhs, &mut calls);
                expr_calls(&assignment.rhs, &mut calls);
            }
        }
        for process in &self.ff_processes {
            stmt_calls(&process.body, &mut calls);
        }
        for process in &self.initial_processes {
            process
                .condition
                .iter()
                .for_each(|condition| const_calls(condition, &mut calls));
            stmt_calls(&process.body, &mut calls);
            stmt_calls(&process.validation_body, &mut calls);
        }
        for subroutine in &self.subroutines {
            subroutine_calls(subroutine, &mut calls);
        }
        for instance in &self.instances {
            instance
                .condition()
                .iter()
                .for_each(|condition| const_calls(condition, &mut calls));
            for parameter in instance.parameter_overrides() {
                parameter
                    .value()
                    .iter()
                    .for_each(|value| const_calls(value, &mut calls));
                if let Some(pattern) = parameter.bound_pattern() {
                    pattern.expressions(&mut |expr| expr_calls(expr, &mut calls));
                }
            }
            for connection in instance.port_connections() {
                connection
                    .actual_expr()
                    .iter()
                    .for_each(|expr| expr_calls(expr, &mut calls));
                if let Some(pattern) = connection.bound_pattern() {
                    pattern.expressions(&mut |expr| expr_calls(expr, &mut calls));
                }
            }
        }
        for port in &self.ports {
            port.default
                .iter()
                .for_each(|default| const_calls(default, &mut calls));
        }
        for parameter in &self.parameters {
            parameter
                .value()
                .iter()
                .for_each(|value| const_calls(value, &mut calls));
        }
        calls
    }

    /// The names of the subroutines of `imported` that the scope can call:
    /// those its own code calls, and those they call in turn. The functions
    /// the types of its header may call are apart
    /// (`declaration_subroutines`): they call each other only.
    pub(super) fn reachable_imports<'a>(
        &self,
        imported: &HashMap<&'a str, &'a Subroutine>,
    ) -> HashSet<&'a str> {
        let mut reached = HashSet::default();
        let mut pending: Vec<&'a Subroutine> = Vec::new();
        let visit = |name: &str, reached: &mut HashSet<&'a str>, pending: &mut Vec<_>| {
            if let Some((&name, &subroutine)) = imported.get_key_value(name)
                && reached.insert(name)
            {
                pending.push(subroutine);
            }
        };
        for name in self.own_calls() {
            visit(name, &mut reached, &mut pending);
        }
        let mut calls = Vec::new();
        while let Some(subroutine) = pending.pop() {
            subroutine_calls(subroutine, &mut calls);
            for name in calls.drain(..) {
                visit(name, &mut reached, &mut pending);
            }
        }
        reached
    }
}
