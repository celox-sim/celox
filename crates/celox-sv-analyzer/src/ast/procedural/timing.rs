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
                            let edge = match item.edge {
                                TimingEdge::Any => EventEdge::Any,
                                TimingEdge::Pos => EventEdge::Pos,
                                TimingEdge::Neg => EventEdge::Neg,
                                TimingEdge::Both => EventEdge::Both,
                            };
                            result.push(EventItemBase {
                                edge,
                                expr: self.native_value(item.expression)?,
                                iff: item.iff.map(|iff| self.native_value(iff)).transpose()?,
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

    /// An event control whose events are members of interface instances
    /// that virtual interfaces denote, `@(posedge vif.clk)` (IEEE 1800-2023
    /// 25.9). Each member item selects the member of the instance its
    /// handle denotes, so the handle is an operand of its event expression
    /// (9.4.2): retargeting the handle is an event when the selection changes
    /// as the item's edge requires. The items keep their own edges, counts
    /// and `iff` gates. The handle becoming `null` also wakes the process,
    /// which checks the handles after every wake and so reports the access
    /// through `null` rather than an edge of the selection it ends. A handle that an automatic
    /// variable, an argument or a class property holds is unsupported.
    pub(super) fn virtual_event_control(
        &mut self,
        id: TimingControlId,
    ) -> Result<Option<Vec<Stmt>>, AnalyzerError> {
        let native = self.tree.declarations();
        let control = native.timing_control(id);
        let TimingControlKind::Event(EventControl::Items(items)) = &control.kind else {
            return Ok(None);
        };
        if !items.iter().any(|item| {
            virtual_interfaces::native_member(self.tree, item.expression, native, &self.dims)
                .is_some()
        }) {
            return Ok(None);
        }
        self.allow_timing("event control")?;
        let mut handles: Vec<Expr> = Vec::new();
        let mut events = Vec::with_capacity(items.len());
        for item in items {
            let edge = match item.edge {
                TimingEdge::Any => EventEdge::Any,
                TimingEdge::Pos => EventEdge::Pos,
                TimingEdge::Neg => EventEdge::Neg,
                TimingEdge::Both => EventEdge::Both,
            };
            let iff = item.iff.map(|iff| self.native_value(iff)).transpose()?;
            let expr = match virtual_interfaces::native_member(
                self.tree,
                item.expression,
                native,
                &self.dims,
            ) {
                Some((handle, ty, member)) => {
                    let handle = self.native_value(handle)?;
                    if !self.watched_handle(&handle) {
                        return Err(unsupported_watch());
                    }
                    if !handles.contains(&handle) {
                        handles.push(handle.clone());
                    }
                    // A write of the selected member is an event at the
                    // write; a change of the selection, at the change.
                    for (signal, denotes) in
                        virtual_interfaces::watched_writes(&self.tree.state, &handle, &ty, &member)?
                    {
                        events.push(EventItemBase {
                            edge,
                            expr: signal,
                            iff: Some(match &iff {
                                Some(iff) => Expr::Binary {
                                    left: Box::new(denotes),
                                    op: BinaryOp::LogicAnd,
                                    right: Box::new(iff.clone()),
                                },
                                None => denotes,
                            }),
                        });
                    }
                    virtual_interfaces::watched(&self.tree.state, handle, &ty, &member)?
                }
                None => self.native_value(item.expression)?,
            };
            events.push(EventItemBase { edge, expr, iff });
        }
        events.extend(handles.iter().map(|handle| EventItemBase {
            edge: EventEdge::Pos,
            expr: virtual_interfaces::is_null(handle),
            iff: None,
        }));
        let checks = handles
            .iter()
            .map(|handle| Stmt::Eval(virtual_interfaces::null_check(handle)))
            .collect::<Vec<_>>();
        let mut stmts = checks.clone();
        stmts.push(Stmt::WaitEvent(events));
        stmts.extend(checks);
        Ok(Some(vec![virtual_interfaces::event_block(stmts)]))
    }

    /// `wait (condition)` (IEEE 1800-2023 9.4.3), whose condition may read
    /// members of interface instances that virtual interfaces denote; see
    /// [`Self::virtual_event_control`] for how it is sensitive to them.
    pub(super) fn wait_statements(&mut self, condition: Expr) -> Result<Vec<Stmt>, AnalyzerError> {
        let mut handles = Vec::new();
        virtual_interfaces::read_handles(&condition, &mut handles);
        if handles.is_empty() {
            return Ok(vec![Stmt::Wait(condition)]);
        }
        if !handles.iter().all(|handle| self.watched_handle(handle)) {
            return Err(unsupported_watch());
        }
        // The process evaluates the condition, so a read through `null`
        // fails only where the condition's guards let it be evaluated.
        Ok(vec![Stmt::Wait(condition)])
    }

    /// Whether a wait can watch virtual interface `handle` with the members
    /// it reads through it: whether a variable of the design, not a local or
    /// an argument of the subroutine, holds it.
    fn watched_handle(&self, handle: &Expr) -> bool {
        matches!(handle, Expr::Ident(name)
            if !self.state.locals.iter().any(|local| &local.name == name))
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
    // The reads of a nested event control through a virtual interface are
    // its timing, not reads of the statements.
    Stmt::WaitImplicit(
        body.iter()
            .chain(removed)
            .map(virtual_interfaces::without_event_blocks)
            .collect(),
    )
}

/// A wait through a virtual interface that is not a static variable.
fn unsupported_watch() -> AnalyzerError {
    unsupported(
        "wait on a member through a virtual interface held by an automatic variable, \
         an argument or a class property",
    )
}
