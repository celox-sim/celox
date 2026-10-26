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
                let delay = value;
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
                        // A package-scoped identifier (A.6.5 delay_value,
                        // ps_identifier) is bound like any other value.
                        ExprKind::Member { separator, .. } if separator == "::" => {
                            return self.native_value(delay);
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
/// 9.4.2.2). The frontend reads the statements with the analysis that gives
/// a combinational `always @*` its sensitivity, so both count the same
/// reads, the defaults of omitted subroutine arguments included.
pub(super) fn implicit_event(body: &[Stmt], removed: &[Stmt]) -> Stmt {
    Stmt::WaitImplicit(body.iter().chain(removed).cloned().collect())
}
