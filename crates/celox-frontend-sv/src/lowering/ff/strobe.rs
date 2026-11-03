use super::*;

impl Ff<'_, '_> {
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
        if self.kernel.is_none() || !self.active_calls.is_empty() {
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
            if names.iter().any(|name| self.overlay.contains_key(name)) {
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
            for arg in args.iter().skip(usize::from(file)) {
                let SystemTaskArg::Expr(expr) = arg else {
                    continue;
                };
                match expr {
                    Expr::Ident(name) => {
                        let id = self
                            .m
                            .id(name)
                            .ok_or_else(|| unsupported(format!("`{task}` argument")))?;
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
                self.wait_sites.push(WaitSite {
                    process: self.process,
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
