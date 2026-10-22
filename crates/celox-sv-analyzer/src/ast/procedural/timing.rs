//! Native timing payloads feed the existing process suspension semantics.
use super::*;
use celox_sv_syntax::{
    EventControl, ExprId, ExprKind, TimingControlId, TimingControlKind, TimingEdge,
};

impl BodyBuilder<'_, '_, '_> {
    pub(super) fn native_timing_control(&self, id: TimingControlId) -> Result<Stmt, AnalyzerError> {
        let control = self.tree.declarations().timing_control(id);
        match &control.kind {
            TimingControlKind::Delay {
                value,
                parenthesized,
            } => {
                self.allow_timing("delay control")?;
                let literal = expression_binding::ungroup(*value, self.tree.declarations());
                if let ExprKind::Literal(text) = &self.tree.declarations().expression(literal).kind
                    && let Some(ticks) = timescales::delay_literal(literal, text, self.tree)?
                {
                    return Ok(Stmt::Delay {
                        amount: Expr::Literal(format!("64'd{ticks}")),
                        ticks_per_unit: 1,
                        ticks_per_precision: 1,
                    });
                }
                Ok(Stmt::Delay {
                    amount: self.native_delay(*value, *parenthesized)?,
                    ticks_per_unit: timescales::time_divisor(*value, self.tree)?,
                    ticks_per_precision: timescales::precision_divisor(*value, self.tree)?,
                })
            }
            TimingControlKind::Event(event) => {
                self.allow_timing("event control")?;
                match event {
                    EventControl::Items(items) => {
                        let mut result = Vec::with_capacity(items.len());
                        for item in items {
                            if item.iff.is_some() {
                                return Err(unsupported("iff-qualified event"));
                            }
                            let edge = match item.edge {
                                TimingEdge::Any => EventEdge::Any,
                                TimingEdge::Pos => EventEdge::Pos,
                                TimingEdge::Neg => EventEdge::Neg,
                                TimingEdge::Both => EventEdge::Both,
                            };
                            result.push(EventItemBase {
                                edge,
                                expr: self.native_value(item.expression)?,
                            });
                        }
                        Ok(Stmt::WaitEvent(result))
                    }
                    EventControl::Named(_) => Err(unsupported("named event")),
                    // The statement it controls gives its event expression.
                    EventControl::Wildcard => Err(crate::ast::timing::disagreement()),
                }
            }
        }
    }

    /// Whether `id` is `@*` (or `@(*)`), whose event expression the
    /// statement it controls determines (see [`implicit_event`]).
    pub(super) fn implicit_event_control(
        &self,
        id: TimingControlId,
    ) -> Result<bool, AnalyzerError> {
        let control = self.tree.declarations().timing_control(id);
        if !matches!(
            control.kind,
            TimingControlKind::Event(EventControl::Wildcard)
        ) {
            return Ok(false);
        }
        self.allow_timing("event control")?;
        Ok(true)
    }

    fn native_delay(&self, mut value: ExprId, parenthesized: bool) -> Result<Expr, AnalyzerError> {
        let native = self.tree.declarations();
        if parenthesized {
            value = expression_binding::ungroup(value, native);
            if let ExprKind::MinTypMax { typical, .. } = native.expression(value).kind {
                value = typical;
            }
            return self.native_value(value);
        }
        match &native.expression(value).kind {
            ExprKind::Literal(text) if text.chars().all(|c| c.is_ascii_digit() || c == '_') => {
                Ok(Expr::Literal(text.replace('_', "")))
            }
            // A real_number delay value such as `#1.5` (A.2.2.3).
            ExprKind::Literal(text)
                if text.contains(['.', 'e', 'E'])
                    && text.replace('_', "").parse::<f64>().is_ok() =>
            {
                Ok(Expr::Literal(text.replace('_', "")))
            }
            ExprKind::Name(name) => {
                let mut result = Expr::Ident(instances::normalize_identifier(&name.spelling));
                self.rename_expr(&mut result);
                Ok(result)
            }
            ExprKind::Member { .. } => {
                let mut parts = Vec::new();
                loop {
                    match &native.expression(value).kind {
                        ExprKind::Member {
                            expression,
                            separator,
                            name,
                        } if separator == "." => {
                            parts.push(instances::normalize_identifier(&name.spelling));
                            value = *expression;
                        }
                        ExprKind::Member { separator, .. } if separator == "::" => {
                            return Err(unsupported("package-scoped delay value"));
                        }
                        ExprKind::Name(name) => {
                            parts.push(instances::normalize_identifier(&name.spelling));
                            break;
                        }
                        _ => return Err(unsupported("delay value")),
                    }
                }
                parts.reverse();
                let mut result = Expr::Ident(parts.join("."));
                self.rename_expr(&mut result);
                Ok(result)
            }
            _ => Err(unsupported("delay value")),
        }
    }
}

/// The wait of an `@*` that controls the statement lowered to `body`, with
/// the branches constant folding removed from it in `removed`: an event of
/// any change of each variable the statement reads (IEEE 1800-2023
/// 9.4.2.2). That is every identifier in an expression, a subroutine
/// argument or a left-hand-side index, but not in a timing control (a
/// delay, wait or event control) and not an assignment target itself. Names that turn out not to
/// denote a variable, such as parameters, never change.
pub(super) fn implicit_event(body: &[Stmt], removed: &[Stmt]) -> Stmt {
    fn constant(expr: &ConstExpr, names: &mut Vec<String>) {
        match expr {
            ConstExpr::Ident(name) => push(name, names),
            ConstExpr::Literal(_) => {}
            ConstExpr::Select { expr, bit } => {
                constant(expr, names);
                constant(bit, names);
            }
            ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                constant(expr, names);
                constant(msb, names);
                constant(lsb, names);
            }
            ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => constant(expr, names),
            ConstExpr::Function { args, .. } => args.iter().for_each(|arg| constant(arg, names)),
            ConstExpr::Binary { left, right, .. } => {
                constant(left, names);
                constant(right, names);
            }
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                constant(condition, names);
                constant(then_expr, names);
                constant(else_expr, names);
            }
        }
    }
    fn expression(expr: &Expr, names: &mut Vec<String>) {
        match expr {
            Expr::Ident(name) => push(name, names),
            Expr::Literal(_) => {}
            Expr::Select { expr, msb, lsb, .. } => {
                expression(expr, names);
                constant(msb, names);
                constant(lsb, names);
            }
            Expr::Concat(parts) => parts.iter().for_each(|part| expression(part, names)),
            Expr::RepeatConcat { count, parts } => {
                constant(count, names);
                parts.iter().for_each(|part| expression(part, names));
            }
            Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => expression(expr, names),
            Expr::Binary { left, right, .. } => {
                expression(left, names);
                expression(right, names);
            }
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                expression(condition, names);
                expression(then_expr, names);
                expression(else_expr, names);
            }
            // A function's own reads are not part of the event expression.
            Expr::Call { args, .. } => args.iter().flatten().for_each(|arg| expression(arg, names)),
            Expr::Inside { expr, items } => {
                expression(expr, names);
                for item in items {
                    item.exprs()
                        .into_iter()
                        .for_each(|operand| expression(operand, names));
                }
            }
        }
    }
    fn target(lvalue: &LValue, names: &mut Vec<String>) {
        if let LValue::Select {
            msb,
            lsb,
            array_slice_width,
            ..
        } = lvalue
        {
            constant(msb, names);
            constant(lsb, names);
            if let Some(width) = array_slice_width {
                constant(width, names);
            }
        }
    }
    fn push(name: &str, names: &mut Vec<String>) {
        if !names.iter().any(|known| known == name) {
            names.push(name.to_string());
        }
    }
    let mut names = Vec::new();
    for stmt in body.iter().chain(removed) {
        stmt.walk(&mut |stmt| match stmt {
            Stmt::Assign { lhs, rhs, .. } => {
                target(lhs, &mut names);
                expression(rhs, &mut names);
            }
            Stmt::AssignConcat { parts, rhs, .. } => {
                parts.iter().for_each(|part| target(part, &mut names));
                expression(rhs, &mut names);
            }
            Stmt::If { condition, .. } => expression(condition, &mut names),
            Stmt::Case {
                selector, items, ..
            } => {
                expression(selector, &mut names);
                for label in items.iter().flat_map(|item| &item.labels) {
                    match label {
                        CaseLabel::Value(value) => expression(value, &mut names),
                        CaseLabel::Range { low, high } => {
                            expression(low, &mut names);
                            expression(high, &mut names);
                        }
                    }
                }
            }
            Stmt::Loop {
                kind, condition, ..
            } => {
                if let LoopKind::Repeat(count) = kind {
                    expression(count, &mut names);
                }
                if let Some(condition) = condition {
                    expression(condition, &mut names);
                }
            }
            Stmt::Return(value) => value.iter().for_each(|value| expression(value, &mut names)),
            Stmt::Call { args, .. } => args
                .iter()
                .flatten()
                .for_each(|arg| expression(arg, &mut names)),
            Stmt::Eval(expr) => expression(expr, &mut names),
            Stmt::SystemTask { args, .. } => {
                for arg in args {
                    if let SystemTaskArg::Expr(expr) = arg {
                        expression(expr, &mut names);
                    }
                }
            }
            Stmt::Local { init, .. } => init.iter().for_each(|init| expression(init, &mut names)),
            Stmt::Break
            | Stmt::Continue
            | Stmt::Delay { .. }
            | Stmt::WaitEvent(_)
            | Stmt::Wait(_) => {}
        });
    }
    Stmt::WaitEvent(
        names
            .into_iter()
            .map(|name| EventItemBase {
                edge: EventEdge::Any,
                expr: Expr::Ident(name),
            })
            .collect(),
    )
}
