//! Sampled value functions (IEEE 1800-2023 16.9.3): `$sampled`, the `$past`
//! histories of a process and the value change functions that read them, and
//! the flush point of a process with deferred assertions (16.4.2).

use super::*;

/// The sampled values and the deferred assertions of a process.
#[derive(Default)]
pub(super) struct Assertions {
    /// The flush-point site of a process with deferred assertions, whose
    /// queue their actions wait in.
    pub(super) flush_site: Option<u32>,
    /// The clock that the sampled value functions of the process infer
    /// (IEEE 1800-2023 16.14.6).
    pub(super) inferred_clock: Option<Clock>,
    /// The `$past` histories of the process.
    pub(super) history: Vec<History>,
}

impl Ff<'_, '_> {
    /// The operand of `$sampled` (IEEE 1800-2023 16.9.3) with each static
    /// variable read through its sampled copy. Automatic variables and the
    /// lowering's own temporaries keep their current value (16.5.1). A
    /// combinational process stays sensitive to the variables themselves,
    /// whose changes run it, not to their copies: it is lowered as a gated
    /// unit that keeps the value it last computed.
    pub(super) fn sampled(
        &mut self,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let [Some(operand)] = args else {
            return Err(unsupported("operand of `$sampled`"));
        };
        // Sampled-value calls resolve their own current/history operands.
        // Sampling their arguments again would bypass that history mapping.
        let operand = self.propagate(operand);
        let mut names = HashSet::default();
        expr_idents(&operand, &mut names);
        let mut renames = HashMap::default();
        for name in names {
            let Some(id) = self.m.id(&name) else {
                continue;
            };
            if self.m.is_hidden(id) {
                continue;
            }
            if self.m.var(id).value_kind.is_handle() {
                return Err(unsupported("$sampled of a string or class handle"));
            }
            renames.insert(name, self.m.sampled_copy(id));
        }
        Ok(rename_idents_with_sampling(&operand, &renames, true))
    }

    /// Declare the `$past` histories that the sampled value functions of
    /// `body`, and of the subroutines it calls, read, as nonblocking targets
    /// of the process. A subroutine's calls infer the clock of the process
    /// that calls it.
    pub(super) fn declare_history(
        &mut self,
        body: &[sv::ir::Stmt],
    ) -> Result<(), sv::AnalyzerError> {
        use sv::ir::SystemFunction;
        let mut calls = Vec::new();
        for stmt in body {
            stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
        }
        // The calls after these are in subroutines the body calls.
        let body_calls = calls.len();
        let mut visited = HashSet::default();
        let mut index = 0;
        let mut seen_defaults = super::super::procedural::SeenDefaults::default();
        while index < calls.len() {
            if let (sv::ir::Callee::Subroutine(name), args) = calls[index].clone()
                && let Some(subroutine) = self.m.subroutine(&name).cloned()
            {
                // The defaults of omitted arguments run at the call.
                default_calls(&subroutine, &args, &mut calls, &mut seen_defaults);
                if visited.insert(name) {
                    for stmt in &subroutine.body {
                        stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
                    }
                }
            }
            index += 1;
        }
        for (position, (callee, args)) in calls.into_iter().enumerate() {
            let sv::ir::Callee::System(
                function @ (SystemFunction::Past
                | SystemFunction::Rose
                | SystemFunction::Fell
                | SystemFunction::Stable
                | SystemFunction::Changed),
            ) = callee
            else {
                continue;
            };
            if self.assertions.inferred_clock.is_none() {
                return Err(unsupported(format!(
                    "`{function}` outside an edge-triggered process whose clock it infers"
                )));
            }
            let (depth, gate) = past_depth_and_gate(function, &args)?;
            let Some(Some(operand)) = args.first() else {
                return Err(unsupported(format!("operand of `{function}`")));
            };
            // The process evaluates the gate before its body runs, outside
            // the subroutine: there the gate can read only module variables,
            // not the subroutine's arguments or locals.
            if position >= body_calls
                && let Some(gate) = &gate
            {
                let mut names = HashSet::default();
                expr_idents(gate, &mut names);
                if names
                    .iter()
                    .any(|name| self.m.id(name).is_none_or(|id| self.m.is_hidden(id)))
                {
                    return Err(unsupported(format!(
                        "`{function}` in a subroutine with a gate that reads the subroutine's arguments or locals"
                    )));
                }
            }
            // The history of a nested call would need its own chain.
            let mut nested = Vec::new();
            collect_calls(operand, &mut nested);
            if nested.iter().any(|(callee, _)| {
                matches!(
                    callee,
                    sv::ir::Callee::System(
                        SystemFunction::Past
                            | SystemFunction::Rose
                            | SystemFunction::Fell
                            | SystemFunction::Stable
                            | SystemFunction::Changed
                    )
                )
            }) {
                return Err(unsupported(format!(
                    "`{function}` of an operand that calls a sampled value function other than `$sampled`"
                )));
            }
            // The histories hold variables: the result of a call, which can
            // read the time or state the operand does not name, has none.
            if nested.iter().any(|(callee, _)| {
                !matches!(callee, sv::ir::Callee::System(SystemFunction::Sampled))
            }) {
                return Err(unsupported(format!(
                    "`{function}` of an operand that calls a function"
                )));
            }
            let mut names = HashSet::default();
            expr_idents(operand, &mut names);
            let mut names: Vec<_> = names.into_iter().collect();
            names.sort();
            for name in names {
                let Some(id) = self.m.id(&name) else {
                    continue;
                };
                if self.m.is_hidden(id) {
                    continue;
                }
                if self.m.var(id).value_kind.is_handle() {
                    return Err(unsupported(format!(
                        "`{function}` of a string or class handle"
                    )));
                }
                // A history moves one tick back with one nonblocking copy
                // of the variable, which an unpacked array has no form of.
                if !self.m.var(id).array_dims.is_empty() {
                    return Err(unsupported(format!("`{function}` of an unpacked array")));
                }
                let index = match self
                    .assertions
                    .history
                    .iter()
                    .position(|history| history.name == name && history.gate == gate)
                {
                    Some(index) => index,
                    None => {
                        self.assertions.history.push(History {
                            name: name.clone(),
                            gate: gate.clone(),
                            gate_value: None,
                            chain: Vec::new(),
                        });
                        self.assertions.history.len() - 1
                    }
                };
                while self.assertions.history[index].chain.len() < depth {
                    let register = self.m.history_copy(id);
                    // A hidden variable is no target of its own: the
                    // history is state the process commits at each tick.
                    self.record_target(&sv::ir::LValue::Ident(register.clone()), false);
                    let copy = self.m.id(&register).expect("a new history register");
                    let width = self.m.var(copy).width;
                    self.writes
                        .targets
                        .push((copy, BitAccess::new(0, width - 1)));
                    self.assertions.history[index].chain.push(register);
                }
            }
        }
        Ok(())
    }

    /// The operand of `$past` (IEEE 1800-2023 16.9.3) evaluated over the
    /// sampled values of its variables `depth` ticks ago.
    pub(super) fn past(
        &self,
        function: sv::ir::SystemFunction,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let (depth, gate) = past_depth_and_gate(function, args)?;
        let Some(Some(operand)) = args.first() else {
            return Err(unsupported(format!("operand of `{function}`")));
        };
        let operand = self.propagate(operand);
        let mut names = HashSet::default();
        expr_idents(&operand, &mut names);
        let mut renames = HashMap::default();
        for name in names {
            if self.m.id(&name).is_none_or(|id| self.m.is_hidden(id)) {
                continue;
            }
            // `declare_history` declared the histories of the process body;
            // a call elsewhere, as in a subroutine, has no clock.
            let history = self
                .assertions
                .history
                .iter()
                .find(|history| {
                    history.name == name && history.gate == gate && history.chain.len() >= depth
                })
                .ok_or_else(|| {
                    unsupported(format!(
                        "`{function}` outside an edge-triggered process whose clock it infers"
                    ))
                })?;
            renames.insert(name, history.chain[depth - 1].clone());
        }
        Ok(rename_idents(&operand, &renames))
    }

    /// `$rose`, `$fell`, `$stable` or `$changed` (IEEE 1800-2023 16.9.3): the
    /// sampled value of the operand against its value at the previous tick,
    /// with case equality for integral values and numeric equality for reals.
    /// A change from or to X counts. `$rose` and
    /// `$fell` look at the least significant bit.
    pub(super) fn value_change(
        &mut self,
        function: sv::ir::SystemFunction,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr, SystemFunction};
        let current = self.sampled(args)?;
        let past = self.past(function, args)?;
        // Resolve nested $sampled calls before inspecting the operand's type.
        let current = self.hoist(&current)?;
        let past = self.hoist(&past)?;
        let real = floating::precision(&current, self.m.variables, self.m.name_to_id).is_some();
        let binary = |left: Expr, op, right: Expr| Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
        let bit = |value: Expr| binary(value, BinaryOp::BitAnd, Expr::Literal("1'b1".into()));
        let literal = |text: &str| Expr::Literal(text.into());
        Ok(match function {
            SystemFunction::Rose | SystemFunction::Fell => {
                let level = if function == SystemFunction::Rose {
                    "1'b1"
                } else {
                    "1'b0"
                };
                binary(
                    binary(bit(past), BinaryOp::NeCase, literal(level)),
                    BinaryOp::LogicAnd,
                    binary(bit(current), BinaryOp::EqCase, literal(level)),
                )
            }
            SystemFunction::Stable => binary(
                current,
                if real { BinaryOp::Eq } else { BinaryOp::EqCase },
                past,
            ),
            _ => binary(
                current,
                if real { BinaryOp::Ne } else { BinaryOp::NeCase },
                past,
            ),
        })
    }

    /// The `$past` updates at the end of an evaluation, a tick of the
    /// process's clock: each history moves one tick back and takes the
    /// sampled value, where its gate holds at the tick.
    pub(super) fn update_history(&mut self) -> Result<bool, sv::AnalyzerError> {
        let stmts = self.history_updates();
        self.exec_block(&stmts)
    }

    pub(super) fn history_updates(&self) -> Vec<sv::ir::Stmt> {
        use sv::ir::{Expr, LValue, Stmt};
        let sampled = |expr: Expr| Expr::Call {
            callee: sv::ir::Callee::System(sv::ir::SystemFunction::Sampled),
            args: vec![Some(expr)],
            arg_order: Vec::new(),
        };
        let mut stmts = Vec::new();
        if self.assertions.history.is_empty() {
            return stmts;
        }
        for history in &self.assertions.history {
            let mut updates: Vec<Stmt> = history
                .chain
                .windows(2)
                .map(|pair| Stmt::Assign {
                    lhs: LValue::Ident(pair[1].clone()),
                    rhs: Expr::Ident(pair[0].clone()),
                    nonblocking: true,
                })
                .collect();
            updates.push(Stmt::Assign {
                lhs: LValue::Ident(history.chain[0].clone()),
                rhs: sampled(Expr::Ident(history.name.clone())),
                nonblocking: true,
            });
            match &history.gate_value {
                None => stmts.extend(updates),
                Some(gate) => stmts.push(Stmt::If {
                    condition: Expr::Ident(gate.clone()),
                    then_body: updates,
                    else_body: Vec::new(),
                }),
            }
        }
        // A run for another event, such as an asynchronous reset, is no
        // tick: the clock has kept the level it had before the slot.
        let clock = self
            .assertions
            .inferred_clock
            .as_ref()
            .expect("histories have a clock");
        if !clock.alone {
            use sv::ir::BinaryOp::{EqCase, LogicAnd, LogicOr, NeCase};
            let to = Expr::Literal(if clock.rising { "1'b1" } else { "1'b0" }.into());
            let from = Expr::Literal(if clock.rising { "1'b0" } else { "1'b1" }.into());
            // Edge events inspect only the least significant bit (9.4.2).
            let signal = Expr::Resize {
                expr: Box::new(Expr::Ident(clock.signal.clone())),
                width: 1,
                signed: false,
            };
            let old = sampled(signal.clone());
            let compare = |left: Expr, op, right: Expr| Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
            // Table 9-2: leaving the opposite known level for X/Z is an
            // edge too, as is arriving at the target level from X/Z.
            let tick = compare(
                compare(
                    compare(old.clone(), EqCase, from.clone()),
                    LogicAnd,
                    compare(signal.clone(), NeCase, from),
                ),
                LogicOr,
                compare(
                    compare(old, NeCase, to.clone()),
                    LogicAnd,
                    compare(signal, EqCase, to),
                ),
            );
            stmts = vec![Stmt::If {
                condition: tick,
                then_body: stmts,
                else_body: Vec::new(),
            }];
        }
        stmts
    }

    /// Evaluate the gate of each `$past` history before the body runs. The
    /// gate is the `iff` of the clocking event, which is evaluated at the
    /// tick (IEEE 1800-2023 9.4.2.3), not after the body's assignments.
    pub(super) fn capture_history_gates(&mut self) -> Result<(), sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr};
        for index in 0..self.assertions.history.len() {
            let Some(gate) = self.assertions.history[index].gate.clone() else {
                continue;
            };
            // Its truth, which keeps an unknown gate unknown, as `if` reads it.
            let truth = Expr::Binary {
                left: Box::new(gate),
                op: BinaryOp::Ne,
                right: Box::new(Expr::Literal("1'b0".into())),
            };
            let value = self.eval(&truth, Some((1, false)))?;
            let (temp, name) = self.m.temp("gate", 1, false, true);
            self.store(temp, SIROffset::Static(0), 1, value);
            self.assertions.history[index].gate_value = Some(name);
        }
        Ok(())
    }

    /// Whether `stmts`, or a subroutine they call, including in timing
    /// controls and the defaults of omitted arguments, hold a deferred
    /// assertion.
    pub(super) fn has_deferred_assertions(
        &self,
        stmts: &[sv::ir::Stmt],
        visited: &mut HashSet<String>,
    ) -> bool {
        let mut found = false;
        let mut calls = Vec::new();
        for stmt in stmts {
            stmt.walk(&mut |stmt| {
                if let sv::ir::Stmt::SystemTask {
                    assertion: Some(assertion),
                    ..
                } = stmt
                    && assertion.deferral != celox_design::AssertionDeferral::None
                {
                    found = true;
                }
                stmt_calls(stmt, &mut calls);
            });
        }
        if found {
            return true;
        }
        let mut seen_defaults = super::super::procedural::SeenDefaults::default();
        while let Some((callee, args)) = calls.pop() {
            let sv::ir::Callee::Subroutine(name) = callee else {
                continue;
            };
            let Some(subroutine) = self.m.subroutine(&name).cloned() else {
                continue;
            };
            default_calls(&subroutine, &args, &mut calls, &mut seen_defaults);
            if visited.insert(name) && self.has_deferred_assertions(&subroutine.body, visited) {
                return true;
            }
        }
        false
    }

    /// For a process with deferred assertions, its flush-point site (IEEE
    /// 1800-2023 16.4.2), whose queue their actions wait in.
    pub(super) fn deferred_flush_site(&mut self, body: &[sv::ir::Stmt]) -> Option<u32> {
        if !self.has_deferred_assertions(body, &mut HashSet::default()) {
            return None;
        }
        let site = self.m.event_site(RuntimeEventSite {
            kind: RuntimeEventKind::Write,
            template: None,
            sizing: DisplaySizing::Ieee,
            scope: None,
            location: None,
            assertion: None,
            deferred: Some(DeferredSite::FlushPoint),
            timestamp_arg: false,
            time_format_arg: false,
            arg_widths: Vec::new(),
            arg_signed: Vec::new(),
            arg_is_string: Vec::new(),
            arg_float_precision: Vec::new(),
            arg_is_handle: Vec::new(),
        });
        self.assertions.flush_site = Some(site);
        Some(site)
    }
}

/// The depth and the gate of a `$past` call, or of the comparison a
/// value change function makes with the previous tick.
pub(super) fn past_depth_and_gate(
    function: sv::ir::SystemFunction,
    args: &[Option<sv::ir::Expr>],
) -> Result<(usize, Option<sv::ir::Expr>), sv::AnalyzerError> {
    if function != sv::ir::SystemFunction::Past {
        return Ok((1, None));
    }
    let depth = match args.get(1) {
        None | Some(None) => 1,
        Some(Some(depth)) => {
            let mut depth = depth;
            while let sv::ir::Expr::Resize { expr, .. } = depth {
                depth = expr;
            }
            literal_usize(depth)
                .filter(|&depth| depth >= 1)
                .ok_or_else(|| {
                    unsupported("number of ticks of `$past` other than a positive constant")
                })?
        }
    };
    Ok((depth, args.get(2).cloned().flatten()))
}
