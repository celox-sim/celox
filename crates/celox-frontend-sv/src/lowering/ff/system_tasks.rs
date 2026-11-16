//! System task statements, and the system functions that read the
//! simulation's time or its command line.

use super::*;

impl Ff<'_, '_> {
    pub(super) fn vcd_task(
        &mut self,
        operation: sv::ir::VcdTask,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
    ) -> Result<(), sv::AnalyzerError> {
        use celox_design::{HostRequestKind, HostRequestSite, HostRequestValue};
        use sv::ir::VcdTask;
        if self.kernel.is_none() {
            return Err(unsupported("VCD tasks require a timed process"));
        }
        let mut values = Vec::new();
        let mut strings = Vec::new();
        for (index, arg) in args.iter().enumerate() {
            if operation == VcdTask::Vars
                && index == 0
                && matches!(arg, sv::ir::SystemTaskArg::Empty)
            {
                continue;
            }
            if operation == VcdTask::Vars && index > 0 {
                let sv::ir::SystemTaskArg::Str(name) = arg else {
                    return Err(unsupported("$dumpvars module or variable selection"));
                };
                strings.push(name.clone());
                continue;
            }
            let (reg, signed, is_string) = match arg {
                sv::ir::SystemTaskArg::Expr(expr) => {
                    (self.eval(expr, None)?, self.expr_signed(expr), false)
                }
                sv::ir::SystemTaskArg::Str(text) if operation == VcdTask::File => {
                    let bytes = unescape(text)?;
                    let width = (bytes.len() * 8).max(8);
                    let value = bytes.iter().fold(BigUint::zero(), |value, byte| {
                        (value << 8u32) | BigUint::from(*byte)
                    });
                    let reg = self.b.alloc_bit(width, false);
                    self.b.emit(SIRInstruction::Imm(reg, SIRValue::new(value)));
                    (reg, false, true)
                }
                _ => {
                    return Err(unsupported(
                        "VCD argument must be an integral value or filename string",
                    ));
                }
            };
            let width = self.b.register(&reg).width();
            let is_4state = matches!(self.b.register(&reg), RegisterType::Logic { .. });
            let (id, _) = self.m.temp("vcd_argument", width, signed, is_4state);
            self.store(id, SIROffset::Static(0), width, reg);
            values.push(HostRequestValue {
                value_kind: celox_design::ValueKind::Bits,
                float_precision: None,
                id,
                width,
                signed,
                is_string,
            });
        }
        let local_site = self
            .sites
            .host_request
            .iter()
            .filter(|site| site.kind == HostRequestKind::Vcd)
            .count() as u32;
        self.sites.host_request.push(HostRequestSite {
            process: celox_design::SiteProcess::own(self.sites.process),
            kind: HostRequestKind::Vcd,
            operation: operation as u32,
            args: values,
            results: Vec::new(),
            strings,
        });
        self.with_kernel(|kernel| kernel.host_request(HostRequestKind::Vcd, local_site))
            .map_err(|error| unsupported(error.to_string()))?;
        Ok(())
    }

    /// IEEE 1800-2023 20.3.1: `$time` is the simulation time, which counts
    /// the global time precision, in the time unit of the calling scope,
    /// rounded to an integer; halves round up. The analyzer passes the
    /// precision's units per time unit as the operand. `$stime` (20.3.2) is
    /// its low 32 bits.
    pub(super) fn time(
        &mut self,
        function: sv::ir::SystemFunction,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr};
        let divisor = args
            .first()
            .and_then(Option::as_ref)
            .ok_or_else(|| unsupported(format!("operand of `{function}`")))?
            .clone();
        let binary = |left, op, right| Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
        let time = Expr::Ident(self.m.time_slot());
        if function == sv::ir::SystemFunction::Realtime {
            // 20.3.3 returns the fractional scope time. Convert the entire
            // unsigned 64-bit tick counter, rather than the 32-bit $itor formal.
            let real = |value| {
                let call = |function, value| Expr::Call {
                    callee: sv::ir::Callee::System(function),
                    args: vec![Some(value)],
                    arg_order: Vec::new(),
                };
                call(
                    sv::ir::SystemFunction::Bitstoreal,
                    call(sv::ir::SystemFunction::Realtobits, value),
                )
            };
            return Ok(binary(real(time), BinaryOp::Div, real(divisor)));
        }
        let time = if divisor == Expr::Literal("64'd1".into()) {
            time
        } else {
            let half = binary(
                divisor.clone(),
                BinaryOp::Shr,
                Expr::Literal("64'd1".into()),
            );
            let rounds_up = binary(
                binary(time.clone(), BinaryOp::Mod, divisor.clone()),
                BinaryOp::Ge,
                half,
            );
            binary(
                binary(time, BinaryOp::Div, divisor),
                BinaryOp::Add,
                Expr::Resize {
                    expr: Box::new(rounds_up),
                    width: 64,
                    signed: false,
                },
            )
        };
        Ok(match function {
            sv::ir::SystemFunction::Stime => Expr::Resize {
                expr: Box::new(time),
                width: 32,
                signed: false,
            },
            _ => time,
        })
    }

    /// The value of `expr` now, in a temporary of its type.
    pub(super) fn keep_argument(
        &mut self,
        expr: &sv::ir::Expr,
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let signed = self.expr_signed(expr);
        let float = floating::precision(expr, self.m.variables, self.m.name_to_id);
        let handle = self.m.expr_is_string(expr);
        let value = self.eval(expr, None)?;
        let register = self.b.register(&value).clone();
        let width = register.width();
        let four_state = matches!(register, celox_sir::RegisterType::Logic { .. });
        let (temp, name) = self.m.temp("task_argument", width, signed, four_state);
        let variable = self.m.variables.get_mut(&temp).unwrap();
        variable.float = float;
        if handle {
            variable.value_kind = celox_design::ValueKind::StrRef;
        }
        self.store(temp, SIROffset::Static(0), width, value);
        Ok(sv::ir::Expr::Ident(name))
    }

    pub(super) fn plusargs(
        &mut self,
        function: sv::ir::SystemFunction,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use celox_design::{HostRequestKind, HostRequestSite, HostRequestValue};
        use sv::ir::Expr;
        if !self.can_request() {
            return Err(unsupported(format!(
                "{} outside a timed process",
                function.name()
            )));
        }
        let query = self.hoist(
            args.first()
                .and_then(Option::as_ref)
                .ok_or_else(|| unsupported("plusargs query"))?,
        )?;
        let width = self
            .m
            .comparison_width(&query, &query)
            .ok_or_else(|| unsupported("packed plusargs query"))?;
        let (query_id, _) = self.host_hidden("plusargs_query", &query, width, false, true)?;
        let value_query = function == sv::ir::SystemFunction::ValuePlusargs;
        let mut results = Vec::new();
        let (ret_id, ret_name) = self.m.temp("plusargs_result", 32, true, false);
        results.push(HostRequestValue {
            value_kind: celox_design::ValueKind::Bits,
            float_precision: None,
            id: ret_id,
            width: 32,
            signed: true,
            is_string: false,
        });
        let target = if value_query {
            let expr = args
                .get(1)
                .and_then(Option::as_ref)
                .ok_or_else(|| unsupported("plusargs destination"))?;
            // A string destination receives the argument's text (21.6); it
            // is a heap handle, which the request replaces on a match.
            let string_target = self.m.expr_is_string(expr);
            let (lvalues, captured, guards) = self.capture_system_actual(expr)?;
            let (id, result, width, value_kind) = if string_target {
                let value = self.eval(&captured, None)?;
                let width = celox_design::HANDLE_WIDTH;
                let (id, name) = self.m.temp("plusargs_value", width, false, false);
                self.m
                    .variables
                    .get_mut(&id)
                    .expect("plusargs text result")
                    .value_kind = celox_design::ValueKind::StrRef;
                self.store(id, SIROffset::Static(0), width, value);
                (
                    id,
                    Expr::Ident(name),
                    width,
                    celox_design::ValueKind::StrRef,
                )
            } else {
                let width = self
                    .m
                    .comparison_width(expr, expr)
                    .ok_or_else(|| unsupported("integral plusargs destination"))?;
                let (id, result) =
                    self.host_hidden("plusargs_value", &captured, width, false, true)?;
                (id, result, width, celox_design::ValueKind::Bits)
            };
            results.push(HostRequestValue {
                value_kind,
                float_precision: None,
                id,
                width,
                signed: false,
                is_string: false,
            });
            Some((lvalues, result, guards))
        } else {
            None
        };
        let site = self
            .sites
            .host_request
            .iter()
            .filter(|site| site.kind == HostRequestKind::Plusargs)
            .count() as u32;
        self.sites.host_request.push(HostRequestSite {
            process: celox_design::SiteProcess::own(0),
            kind: HostRequestKind::Plusargs,
            operation: u32::from(value_query),
            args: vec![HostRequestValue {
                value_kind: celox_design::ValueKind::Bits,
                float_precision: None,
                id: query_id,
                width,
                signed: false,
                is_string: true,
            }],
            results,
            strings: Vec::new(),
        });
        self.request_host(HostRequestKind::Plusargs, site)?;
        if let Some((lvalues, result, guards)) = target {
            self.check_tagged_guards(&guards)?;
            self.write_system_actual(&lvalues, &result)?;
        }
        Ok(Expr::Ident(ret_name))
    }

    /// A system task statement: a runtime event, and for `$fatal` the end of
    /// simulation with an error.
    pub(super) fn system_task(
        &mut self,
        task: sv::ir::SystemTask,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
        site: Option<&sv::ir::SourceSpan>,
        assertion_site: Option<&celox_design::AssertionSite>,
    ) -> Result<bool, sv::AnalyzerError> {
        if matches!(
            task,
            sv::ir::SystemTask::Strobe(_)
                | sv::ir::SystemTask::Fstrobe(_)
                | sv::ir::SystemTask::Monitor(_)
                | sv::ir::SystemTask::Fmonitor(_)
                | sv::ir::SystemTask::MonitorOn
                | sv::ir::SystemTask::MonitorOff
        ) {
            return self.strobe(task, args, site);
        }
        if let sv::ir::SystemTask::Vcd(operation) = task {
            self.vcd_task(operation, args)?;
            return Ok(true);
        }
        if matches!(
            task,
            sv::ir::SystemTask::Fclose
                | sv::ir::SystemTask::Fflush
                | sv::ir::SystemTask::Fdisplay(_)
                | sv::ir::SystemTask::Fwrite(_)
                | sv::ir::SystemTask::Writemem(_)
        ) {
            return self.file_task(task, args);
        }
        if let sv::ir::SystemTask::Readmem(format) = task {
            self.readmem(format, args, site)?;
            return Ok(true);
        }
        // IEEE 1800-2023 24.7: `$exit` in a thread of a program's `initial`
        // procedure ends the program's initial procedures; elsewhere it is
        // ignored, with a warning.
        if task == sv::ir::SystemTask::Exit {
            if self.m.program && self.kernel.is_some() {
                self.with_kernel(ProcessKernelBuilder::exit);
                return Ok(false);
            }
            self.m
                .warn(crate::FrontendWarningKind::ExitOutsideProgram, site);
            return Ok(true);
        }
        if task == sv::ir::SystemTask::Timeformat {
            self.time_format(args)?;
            return Ok(true);
        }
        let kind =
            system_task_kind(task).ok_or_else(|| unsupported(format!("system task `{task}`")))?;
        // A process ends the simulation itself; the rest of its body does not
        // run. A stop suspends the process with the simulation, which then
        // continues after the call.
        match kind {
            SystemTaskKind::Control(RuntimeEventKind::Finish) if self.kernel.is_some() => {
                self.with_kernel(ProcessKernelBuilder::finish);
                return Ok(false);
            }
            SystemTaskKind::Control(RuntimeEventKind::Stop) if self.kernel.is_some() => {
                self.with_kernel(ProcessKernelBuilder::stop)
                    .map_err(|error| unsupported(error.to_string()))?;
                return Ok(true);
            }
            // A unit suspends the process with its frames. After `$finish`
            // the process never resumes, so the code after its resume point
            // is dead.
            SystemTaskKind::Control(
                event @ (RuntimeEventKind::Finish | RuntimeEventKind::Stop),
            ) if self.can_request() => {
                let status = if event == RuntimeEventKind::Finish {
                    celox_design::ProcessStatus::Finish
                } else {
                    celox_design::ProcessStatus::Stop
                };
                let request = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
                self.b
                    .emit(SIRInstruction::Imm(request, SIRValue::new(0u64)));
                self.suspend_unit_with(status, request)?;
                return Ok(true);
            }
            _ => {}
        }
        let assertion = if matches!(kind, SystemTaskKind::Assertion(_)) {
            let Some(sv::ir::SystemTaskArg::Expr(condition)) = args.first() else {
                return Err(unsupported(format!("condition of `{task}`")));
            };
            Some(self.eval_truth(condition)?)
        } else {
            None
        };
        let (template, mut values) = system_task_template(self.m, &kind, args)?;
        prepare_time_operands(self.m, template.as_deref(), &mut values)?;
        // Arguments are evaluated left to right. A later argument's call can
        // change what an earlier one reads, or suspend the process (a host
        // request) so that a value read before it would not survive the
        // resume. So every argument before the last one with a call is
        // evaluated whole into a temporary at its place; that one only has
        // its calls run before its own reads, as any expression does.
        let last_call = values.iter().rposition(
            |value| matches!(value, sv::ir::SystemTaskArg::Expr(expr) if self.m.calls(expr)),
        );
        if let Some(last_call) = last_call {
            for (index, value) in values[..=last_call].iter_mut().enumerate() {
                let sv::ir::SystemTaskArg::Expr(expr) = value else {
                    continue;
                };
                let hoisted = if self.m.calls(expr) {
                    self.hoist(expr)?
                } else {
                    expr.clone()
                };
                *expr = if index == last_call || matches!(hoisted, sv::ir::Expr::Literal(_)) {
                    hoisted
                } else {
                    self.keep_argument(&hoisted)?
                };
            }
        }
        let mut regs = Vec::with_capacity(values.len());
        let mut arg_widths = Vec::with_capacity(values.len());
        let mut arg_signed = Vec::with_capacity(values.len());
        let mut arg_is_string = Vec::with_capacity(values.len());
        let mut arg_float_precision = Vec::with_capacity(values.len());
        let mut arg_is_handle = Vec::with_capacity(values.len());
        for value in &values {
            match value {
                sv::ir::SystemTaskArg::Expr(expr) => {
                    let expr = expr.clone();
                    let signed = self.expr_signed(&expr);
                    arg_is_handle.push(self.m.expr_is_string(&expr));
                    arg_float_precision.push(floating::precision(
                        &expr,
                        self.m.variables,
                        self.m.name_to_id,
                    ));
                    let reg = self.eval(&expr, None)?;
                    arg_widths.push(self.b.register(&reg).width());
                    arg_signed.push(signed);
                    arg_is_string.push(false);
                    regs.push(reg);
                }
                sv::ir::SystemTaskArg::Str(text) => {
                    let bytes = unescape(text)?;
                    let width = (bytes.len() * 8).max(8);
                    let value = bytes.iter().fold(BigUint::zero(), |value, byte| {
                        (value << 8u32) | BigUint::from(*byte)
                    });
                    let reg = self.b.alloc_bit(width, false);
                    self.b.emit(SIRInstruction::Imm(reg, SIRValue::new(value)));
                    arg_widths.push(width);
                    arg_signed.push(false);
                    arg_is_string.push(true);
                    arg_float_precision.push(None);
                    arg_is_handle.push(false);
                    regs.push(reg);
                }
                sv::ir::SystemTaskArg::Empty => {
                    return Err(unsupported(format!("empty argument of `{task}`")));
                }
            }
        }
        // A `%t` reports in the format the design's `$timeformat` calls set
        // when the site runs (IEEE 1800-2023 20.4.3).
        let time_format_arg =
            match time_format_operand(self.m, template.as_deref().is_some_and(converts_time)) {
                Some(format) => {
                    let reg = self.eval(&format, None)?;
                    arg_widths.push(self.b.register(&reg).width());
                    arg_signed.push(false);
                    arg_is_string.push(false);
                    arg_float_precision.push(None);
                    arg_is_handle.push(false);
                    regs.push(reg);
                    true
                }
                None => false,
            };
        // A report carries the time it is issued at (IEEE 1800-2023 20.10).
        let timestamp_arg = matches!(kind, SystemTaskKind::Message(_) | SystemTaskKind::Fatal);
        if timestamp_arg {
            let time = sv::ir::Expr::Ident(self.m.time_slot());
            let reg = self.eval(&time, None)?;
            arg_widths.push(self.b.register(&reg).width());
            arg_signed.push(false);
            arg_is_string.push(false);
            arg_float_precision.push(None);
            arg_is_handle.push(false);
            regs.push(reg);
        }
        // Argument effects happen before branching, and the condition register
        // retains the value it had before those effects.
        let resume = match assertion {
            Some((_, Some(true))) => return Ok(true),
            Some((truth, None)) => {
                let resume = self.b.new_block();
                let fail = self.b.new_block();
                self.b.seal_block(SIRTerminator::Branch {
                    cond: truth,
                    true_block: (resume, Vec::new()),
                    false_block: (fail, Vec::new()),
                });
                self.b.switch_to_block(fail);
                Some(resume)
            }
            _ => None,
        };
        let event_kind = kind.event_kind();
        // The action of a deferred assertion waits in its process's queue
        // until the assertion matures (IEEE 1800-2023 16.4.1); in a unit,
        // which serves every process, in that of the process it runs in.
        let deferred = assertion_site
            .map(|assertion| assertion.deferral)
            .filter(|&region| region != celox_design::AssertionDeferral::None)
            .and_then(|region| match self.assertions.flush_site {
                _ if self.frame.is_some() => Some(DeferredSite::ProcessAction { region }),
                Some(queue) => Some(DeferredSite::Action { queue, region }),
                None => None,
            });
        let site_id = self.m.event_site(RuntimeEventSite {
            kind: event_kind,
            template: template.clone(),
            sizing: DisplaySizing::Ieee,
            scope: None,
            location: report_location(site),
            assertion: assertion_site.cloned(),
            deferred,
            timestamp_arg,
            time_format_arg,
            arg_widths,
            arg_signed,
            arg_is_string,
            arg_float_precision,
            arg_is_handle,
        });
        self.b.emit(SIRInstruction::RuntimeEvent {
            site_id,
            args: regs,
        });
        // A deferred `$fatal` ends the simulation when it matures.
        if kind.is_fatal() && deferred.is_none() {
            let code = self
                .m
                .runtime_error(template.unwrap_or_else(|| sv::ir::SystemTask::Fatal.to_string()));
            self.b.seal_block(SIRTerminator::Error(code));
            if resume.is_none() {
                return Ok(false);
            }
        } else if let Some(resume) = resume {
            self.jump(resume);
        }
        if let Some(resume) = resume {
            self.b.switch_to_block(resume);
        }
        Ok(true)
    }
}
