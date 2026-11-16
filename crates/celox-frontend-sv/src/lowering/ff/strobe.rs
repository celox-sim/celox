use super::*;

impl Ff<'_, '_> {
    /// Count the changes of the whole static variable `id` for a monitor,
    /// once, and pass its value to the host request that registers it.
    fn watch_monitor_variable(
        &mut self,
        task: sv::ir::SystemTask,
        file: bool,
        id: SourceVarId,
        watched: &mut Vec<SourceVarId>,
        registration: &mut Vec<celox_design::HostRequestValue<SourceVarId>>,
    ) -> Result<(), sv::AnalyzerError> {
        let variable = self.m.var(id);
        if (file && variable.value_kind == celox_design::ValueKind::StrRef)
            || !variable.array_dims.is_empty()
            || !matches!(
                variable.value_kind,
                celox_design::ValueKind::Bits | celox_design::ValueKind::StrRef
            )
        {
            return Err(unsupported(format!(
                "`{task}` requires whole static scalar variables (file monitors currently require numeric variables)"
            )));
        }
        if !watched.contains(&id) {
            watched.push(id);
            registration.push(celox_design::HostRequestValue {
                value_kind: variable.value_kind,
                id,
                width: variable.width,
                signed: variable.signed,
                is_string: false,
                float_precision: variable.float,
            });
        }
        Ok(())
    }

    pub(super) fn strobe(
        &mut self,
        task: sv::ir::SystemTask,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
        site: Option<&sv::ir::SourceSpan>,
    ) -> Result<bool, sv::AnalyzerError> {
        use sv::ir::{Callee, Expr, SystemTask, SystemTaskArg};
        if matches!(task, SystemTask::MonitorOn | SystemTask::MonitorOff) {
            self.file_io_request(
                if task == SystemTask::MonitorOn {
                    19
                } else {
                    20
                },
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )?;
            return Ok(true);
        }
        let monitor = matches!(task, SystemTask::Monitor(_) | SystemTask::Fmonitor(_));
        if self.kernel.is_none() || !self.call_stack.active_calls.is_empty() {
            return Err(unsupported(format!(
                "`{task}` requires a scheduled process outside subroutines"
            )));
        }
        for arg in args {
            let SystemTaskArg::Expr(expr) = arg else {
                continue;
            };
            let mut names = HashSet::default();
            expr_idents(expr, &mut names);
            if names
                .iter()
                .any(|name| self.scope.overlay.contains_key(name))
            {
                return Err(unsupported(format!(
                    "`{task}` with an unrolled loop variable"
                )));
            }
            if names
                .iter()
                .filter_map(|name| self.m.id(name))
                .any(|id| self.postponed_locals.contains(&id))
            {
                return Err(unsupported(format!("`{task}` with a procedural local")));
            }
            let mut calls = Vec::new();
            collect_calls(expr, &mut calls);
            if calls.iter().any(|(callee, _)| match callee {
                Callee::System(function) => function.is_effectful(),
                Callee::String(
                    sv::ir::StringOp::Format { .. }
                    | sv::ir::StringOp::Putc
                    | sv::ir::StringOp::Itoa(_),
                ) => true,
                Callee::String(_) | Callee::Lookup { .. } | Callee::Stream(_) => false,
                _ => true,
            }) {
                return Err(unsupported(format!(
                    "`{task}` with a side-effecting or user call"
                )));
            }
        }
        let (immediate, file) = match task {
            SystemTask::Strobe(radix) => (SystemTask::Display(radix), false),
            SystemTask::Fstrobe(radix) => (SystemTask::Fdisplay(radix), true),
            SystemTask::Monitor(radix) => (SystemTask::Display(radix), false),
            SystemTask::Fmonitor(radix) => (SystemTask::Fdisplay(radix), true),
            _ => unreachable!(),
        };
        let mut args = args.to_vec();
        // The file selection is registered now; all displayed values are read
        // by the separate dispatch entry in Postponed.
        let mut registration = Vec::new();
        if file {
            let descriptor = self.file_descriptor_arg(&args[0])?;
            let name = self.m.var(descriptor.id).path[0].clone();
            args[0] = SystemTaskArg::Expr(Expr::Ident(name));
            registration.push(descriptor);
        }
        if monitor {
            // Whole static variables use the existing occurrence counters.
            // Thus a transient write still triggers output when its final
            // value returns to the preceding slot's value.
            let mut watched = Vec::new();
            for arg in args.iter_mut().skip(usize::from(file)) {
                let SystemTaskArg::Expr(expr) = arg else {
                    continue;
                };
                // A time query, also as a `%t` argument scaled to the time
                // unit or the operand of a pure call, changes in every slot
                // without triggering the monitor (IEEE 1800-2023 21.2.3).
                // The largest parts of such an argument that read no time
                // query, such as the `a[0]` of `$time + a[0]`, still
                // trigger it by the changes of their values.
                if reads_time_query(expr) {
                    let mut parts = Vec::new();
                    untimed_parts(expr, &mut parts);
                    for part in parts {
                        let part = match part {
                            Expr::Literal(_) => continue,
                            Expr::Ident(_) => part,
                            part => {
                                let mut samples = Vec::new();
                                let event = self.event_expression(&part, &mut samples)?;
                                let [(name, value)] = samples.as_slice() else {
                                    return Err(unsupported(format!(
                                        "`{task}` expression reading a process-local variable"
                                    )));
                                };
                                self.assign(&sv::ir::LValue::Ident(name.clone()), value)?;
                                Expr::Ident(self.m.var(event).path.join("."))
                            }
                        };
                        let Expr::Ident(name) = &part else {
                            unreachable!("a watched part is a variable");
                        };
                        let id = self
                            .m
                            .id(name)
                            .ok_or_else(|| unsupported(format!("`{task}` argument")))?;
                        self.watch_monitor_variable(
                            task,
                            file,
                            id,
                            &mut watched,
                            &mut registration,
                        )?;
                    }
                    continue;
                }
                // Any other expression is watched through a continuous
                // assignment of it to a hidden variable, which the monitor
                // then displays: it reports when the value of the
                // expression changes, not each write of an operand
                // (IEEE 1800-2023 21.2.3).
                if !matches!(
                    expr,
                    Expr::Ident(_)
                        | Expr::Literal(_)
                        | Expr::Call {
                            callee: Callee::System(
                                sv::ir::SystemFunction::Time
                                    | sv::ir::SystemFunction::Stime
                                    | sv::ir::SystemFunction::Realtime,
                            ),
                            ..
                        }
                ) && !self.m.expr_is_string(expr)
                {
                    let mut samples = Vec::new();
                    let event = self.event_expression(expr, &mut samples)?;
                    // Operands copied for the wait of a suspending process
                    // would not follow later writes.
                    let [(name, value)] = samples.as_slice() else {
                        return Err(unsupported(format!(
                            "`{task}` expression reading a process-local variable"
                        )));
                    };
                    self.assign(&sv::ir::LValue::Ident(name.clone()), value)?;
                    *expr = Expr::Ident(self.m.var(event).path.join("."));
                }
                match expr {
                    Expr::Ident(name) => {
                        let id = self
                            .m
                            .id(name)
                            .ok_or_else(|| unsupported(format!("`{task}` argument")))?;
                        self.watch_monitor_variable(
                            task,
                            file,
                            id,
                            &mut watched,
                            &mut registration,
                        )?;
                    }
                    Expr::Literal(_) => {}
                    Expr::Call {
                        callee:
                            Callee::System(
                                sv::ir::SystemFunction::Time
                                | sv::ir::SystemFunction::Stime
                                | sv::ir::SystemFunction::Realtime,
                            ),
                        ..
                    } => {}
                    _ => {
                        return Err(unsupported(format!(
                            "`{task}` currently requires variables, constants or direct time queries"
                        )));
                    }
                }
            }
            if !watched.is_empty() {
                self.sites.wait.push(WaitSite {
                    process: self.sites.process,
                    terms: watched
                        .into_iter()
                        .map(|id| (id, CounterKind::Changes))
                        .collect(),
                    gates: Vec::new(),
                });
            }
        }
        let (continuation, resume) = self
            .with_kernel(ProcessKernelBuilder::begin_event_probe)
            .map_err(kernel_error)?;
        self.system_task(immediate, &args, site, None)?;
        self.with_kernel(|kernel| kernel.end_event_probe(continuation));
        self.file_io_request(
            if monitor {
                if file { 18 } else { 17 }
            } else {
                16
            },
            registration,
            Vec::new(),
            vec![resume.to_string()],
        )?;
        Ok(true)
    }
}

/// Whether `expr` calls `$time`, `$stime` or `$realtime`.
fn reads_time_query(expr: &sv::ir::Expr) -> bool {
    let mut calls = Vec::new();
    collect_calls(expr, &mut calls);
    calls.iter().any(|(callee, _)| {
        matches!(
            callee,
            sv::ir::Callee::System(
                sv::ir::SystemFunction::Time
                    | sv::ir::SystemFunction::Stime
                    | sv::ir::SystemFunction::Realtime
            )
        )
    })
}

/// The largest subexpressions of `expr` that read no time query, in order.
fn untimed_parts(expr: &sv::ir::Expr, parts: &mut Vec<sv::ir::Expr>) {
    use sv::ir::Expr;
    if !reads_time_query(expr) {
        parts.push(expr.clone());
        return;
    }
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => {}
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            untimed_parts(expr, parts)
        }
        Expr::Concat(items) | Expr::RepeatConcat { parts: items, .. } => {
            for item in items {
                untimed_parts(item, parts);
            }
        }
        Expr::Binary { left, right, .. } => {
            untimed_parts(left, parts);
            untimed_parts(right, parts);
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            untimed_parts(condition, parts);
            untimed_parts(then_expr, parts);
            untimed_parts(else_expr, parts);
        }
        Expr::Call { args, .. } => {
            for arg in args.iter().flatten() {
                untimed_parts(arg, parts);
            }
        }
        Expr::Inside { expr, .. } => untimed_parts(expr, parts),
    }
}
