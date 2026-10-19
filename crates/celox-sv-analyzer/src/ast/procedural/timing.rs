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
                Ok(Stmt::Delay {
                    amount: self.native_delay(*value, *parenthesized)?,
                    ticks_per_unit: timescales::time_divisor(*value, self.tree)?,
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
                    EventControl::Wildcard => Err(unsupported("`@*` event control in a process")),
                }
            }
        }
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
