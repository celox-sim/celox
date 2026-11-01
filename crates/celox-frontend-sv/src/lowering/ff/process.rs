//! Process methods use the shared scheduler on every execution backend.
use super::*;
use celox_design::{HostRequestKind, HostRequestSite, HostRequestValue, ProcessOp, ValueKind};
impl Ff<'_, '_> {
    pub(super) fn process_call(
        &mut self,
        op: ProcessOp,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        if self.kernel.is_none() {
            return Err(unsupported("process method outside a process kernel"));
        }
        // The pre-scan classifies earlier waits. Keep the frame invariant here
        // too, including calls materialized while binding argument defaults.
        self.process_control = true;
        self.forked = true;
        self.urandom_state();
        let function = self
            .active_calls
            .iter()
            .skip(self.timing_call_boundary)
            .any(|name| self.m.subroutine(name).is_some_and(|s| !s.is_task));
        if op == ProcessOp::Await && function {
            return Err(unsupported("process.await task called from a function"));
        }
        let mut values = Vec::new();
        for (index, arg) in args.iter().enumerate() {
            let arg = self.hoist(
                arg.as_ref()
                    .ok_or_else(|| unsupported("missing process argument"))?,
            )?;
            // Key counts are `int` formals (IEEE 1800-2023 15.3).
            let int = match op {
                ProcessOp::Srandom
                | ProcessOp::SemaphorePut
                | ProcessOp::SemaphoreGet
                | ProcessOp::SemaphoreTryGet => index == 1,
                ProcessOp::SemaphoreNew => true,
                _ => false,
            };
            let arg = if int { self.int_formal(arg) } else { arg };
            let string = op == ProcessOp::SetRandstate && index == 1;
            // A string formal converts packed values before the handle is
            // materialized, just like an ordinary subroutine string argument.
            let arg = if string && !self.m.expr_is_string(&arg) {
                self.string_call(sv::ir::StringOp::FromBits, &[Some(arg)])?
            } else {
                arg
            };
            let (id, _) = self.host_hidden("process_argument", &arg, 32, false, !string)?;
            if string {
                self.m.variables.get_mut(&id).unwrap().value_kind = ValueKind::StrRef;
            }
            values.push(HostRequestValue {
                id,
                width: 32,
                signed: false,
                is_string: false,
                value_kind: if string {
                    ValueKind::StrRef
                } else {
                    ValueKind::Bits
                },
                float_precision: None,
            });
        }
        let signed = matches!(op, ProcessOp::Status | ProcessOp::SemaphoreTryGet);
        let (id, name) = self.m.temp("process_result", 32, signed, false);
        let string = op == ProcessOp::GetRandstate;
        if string {
            self.m.variables.get_mut(&id).unwrap().value_kind = ValueKind::StrRef;
        }
        let index = self
            .host_request_sites
            .iter()
            .filter(|s| s.kind == HostRequestKind::Process)
            .count() as u32;
        self.host_request_sites.push(HostRequestSite {
            process: 0,
            kind: HostRequestKind::Process,
            operation: op as u32
                | if self.process_predicate { 0x100 } else { 0 }
                | if function { 0x200 } else { 0 },
            args: values,
            results: vec![HostRequestValue {
                id,
                width: 32,
                signed,
                is_string: false,
                value_kind: if string {
                    ValueKind::StrRef
                } else {
                    ValueKind::Bits
                },
                float_precision: None,
            }],
            strings: Vec::new(),
        });
        self.with_kernel(|k| k.host_request(HostRequestKind::Process, index))
            .map_err(kernel_error)?;
        // `triggered` is a one-bit value in its expression (IEEE 1800-2023
        // 15.5.3), whatever the width of the result the scheduler writes.
        if op == ProcessOp::Triggered {
            return Ok(sv::ir::Expr::Resize {
                expr: Box::new(sv::ir::Expr::Ident(name)),
                width: 1,
                signed: false,
            });
        }
        Ok(sv::ir::Expr::Ident(name))
    }

    /// A mailbox method (IEEE 1800-2023 15.4). The analyzer passes the heap
    /// layout of the messages last, which gives the shape of their slots. A
    /// blocking method that cannot complete parks the process and is called
    /// again when the process is woken.
    pub(super) fn mailbox_call(
        &mut self,
        op: ProcessOp,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::Expr;
        if self.kernel.is_none() {
            return Err(unsupported("mailbox method outside a process kernel"));
        }
        self.process_control = true;
        self.forked = true;
        let layout = |arg: &Option<Expr>| match arg {
            Some(Expr::Literal(literal)) => literal
                .strip_prefix("64'd")
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| unsupported("mailbox message layout")),
            _ => Err(unsupported("mailbox message layout")),
        };
        let value = |id, width, value_kind| HostRequestValue {
            id,
            width,
            signed: false,
            is_string: false,
            value_kind,
            float_precision: None,
        };
        let mut values = Vec::new();
        let mut results = Vec::new();
        let (status_id, status) = self.m.temp("mailbox_status", 32, true, false);
        let mut destination = None;
        if op == ProcessOp::MailboxNew {
            let bound = args[0]
                .as_ref()
                .ok_or_else(|| unsupported("missing mailbox bound"))?;
            // The bound is an `int` formal (IEEE 1800-2023 15.4.1).
            let bound = self.hoist(bound)?;
            let bound = self.int_formal(bound);
            let (bound, _) = self.host_hidden("mailbox_bound", &bound, 32, true, false)?;
            values.push(value(bound, 32, ValueKind::Bits));
            let parameter = self.constant64(layout(&args[1])?);
            let (id, _) = self.m.temp("mailbox_layout", 64, false, false);
            self.store(id, SIROffset::Static(0), 64, parameter);
            values.push(value(id, 64, ValueKind::Bits));
            self.m.variables.get_mut(&status_id).unwrap().value_kind = ValueKind::ObjRef;
            results.push(value(status_id, 32, ValueKind::ObjRef));
        } else {
            let mailbox = args[0]
                .as_ref()
                .ok_or_else(|| unsupported("missing mailbox"))?;
            let (mailbox, _) = self.host_hidden("mailbox", mailbox, 32, false, false)?;
            self.m.variables.get_mut(&mailbox).unwrap().value_kind = ValueKind::ObjRef;
            values.push(value(mailbox, 32, ValueKind::ObjRef));
            results.push(HostRequestValue {
                signed: true,
                ..value(status_id, 32, ValueKind::Bits)
            });
            if op != ProcessOp::MailboxNum {
                let layout = layout(&args[2])?;
                let width = (layout & 127) as usize;
                let four_state = layout & 128 == 0 && self.m.four_state;
                let kind = match (layout >> 8) & 3 {
                    1 => ValueKind::StrRef,
                    2 => ValueKind::ObjRef,
                    _ => ValueKind::Bits,
                };
                let message = args[1]
                    .as_ref()
                    .ok_or_else(|| unsupported("missing mailbox message"))?;
                if matches!(op, ProcessOp::MailboxPut | ProcessOp::MailboxTryPut) {
                    // An integral message is assigned to the message type,
                    // extended as its own signedness says.
                    let message = match kind {
                        ValueKind::Bits => Expr::Resize {
                            expr: Box::new(message.clone()),
                            width,
                            signed: self.expr_signed(message),
                        },
                        _ => message.clone(),
                    };
                    let (id, _) =
                        self.host_hidden("mailbox_message", &message, width, false, four_state)?;
                    // Keep a handle in a waiting message alive.
                    self.m.variables.get_mut(&id).unwrap().value_kind = kind;
                    values.push(value(id, width, kind));
                } else {
                    let signed = matches!(args.get(3), Some(Some(Expr::Literal(signed))) if signed == "1'd1");
                    let (id, name) = self.m.temp("mailbox_message", width, signed, four_state);
                    self.m.variables.get_mut(&id).unwrap().value_kind = kind;
                    results.push(value(id, width, kind));
                    let message_value = Expr::Ident(name);
                    destination = Some(match message {
                        // A class property; its object is the one of the call.
                        Expr::Call {
                            callee: sv::ir::Callee::Object(object),
                            args: receiver,
                            ..
                        } if matches!(**object, sv::ir::ObjectOp::Load(_)) => {
                            let sv::ir::ObjectOp::Load(field) = **object else {
                                unreachable!()
                            };
                            let [Some(receiver)] = receiver.as_slice() else {
                                return Err(unsupported("class property receiver"));
                            };
                            let (handle, handle_name) =
                                self.m.temp("mailbox_object", 32, false, false);
                            self.m.variables.get_mut(&handle).unwrap().value_kind =
                                ValueKind::ObjRef;
                            // A forked activation keeps its own object.
                            if self.forked {
                                let slot = self.frame_locals.len();
                                self.frame_locals.push(handle);
                                self.with_kernel(|kernel| kernel.local(slot))
                                    .map_err(kernel_error)?;
                            }
                            let object = self.handle(receiver)?;
                            self.store(handle, SIROffset::Static(0), 32, object);
                            Destination::Property(Expr::Call {
                                callee: sv::ir::Callee::Object(Box::new(sv::ir::ObjectOp::Store(
                                    field,
                                ))),
                                args: vec![Some(Expr::Ident(handle_name)), Some(message_value)],
                                arg_order: Vec::new(),
                            })
                        }
                        message => {
                            let (lvalues, _, guards) = self.capture_system_actual(message)?;
                            Destination::Variable(lvalues, message_value, guards)
                        }
                    });
                }
            }
        }
        let index = self
            .host_request_sites
            .iter()
            .filter(|s| s.kind == HostRequestKind::Process)
            .count() as u32;
        self.host_request_sites.push(HostRequestSite {
            process: 0,
            kind: HostRequestKind::Process,
            operation: op as u32,
            args: values,
            results,
            strings: Vec::new(),
        });
        let blocking = matches!(
            op,
            ProcessOp::MailboxPut | ProcessOp::MailboxGet | ProcessOp::MailboxPeek
        );
        let retry = self.b.new_block();
        self.jump(retry);
        self.b.switch_to_block(retry);
        self.with_kernel(|k| k.host_request(HostRequestKind::Process, index))
            .map_err(kernel_error)?;
        let success = Expr::Binary {
            left: Box::new(Expr::Ident(status.clone())),
            op: sv::ir::BinaryOp::Gt,
            right: Box::new(Expr::Literal("32'sd0".into())),
        };
        if blocking {
            let (done, _) = self.eval_truth(&success)?;
            let next = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: done,
                true_block: (next, Vec::new()),
                false_block: (retry, Vec::new()),
            });
            self.b.switch_to_block(next);
        }
        if let Some(destination) = destination {
            let (received, _) = self.eval_truth(&success)?;
            let write = self.b.new_block();
            let join = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: received,
                true_block: (write, Vec::new()),
                false_block: (join, Vec::new()),
            });
            self.b.switch_to_block(write);
            match destination {
                Destination::Variable(lvalues, message, guards) => {
                    self.check_tagged_guards(&guards)?;
                    match lvalues.as_slice() {
                        [one] => self.assign(one, &message)?,
                        parts => self.assign_concat(parts, &message)?,
                    };
                }
                Destination::Property(store) => {
                    self.eval(&store, None)?;
                }
            }
            self.jump(join);
            self.b.switch_to_block(join);
        }
        Ok(Expr::Ident(status))
    }
}

/// Where a mailbox method copies a message out to.
enum Destination {
    /// Variables, as an output actual of a system function: the parts, the
    /// message and the guards of tagged members.
    Variable(Vec<sv::ir::LValue>, sv::ir::Expr, Vec<sv::ir::Expr>),
    /// The store of the message into a class property.
    Property(sv::ir::Expr),
}
