use super::*;
use celox_design::{HostRequestKind, HostRequestSite, HostRequestValue};

impl Ff<'_, '_> {
    pub(super) fn file_io_arg(
        &mut self,
        arg: &sv::ir::SystemTaskArg<sv::ir::Expr>,
    ) -> Result<HostRequestValue<SourceVarId>, sv::AnalyzerError> {
        let (reg, signed, is_string, float_precision) = match arg {
            sv::ir::SystemTaskArg::Expr(expr) => {
                let expr = if self.m.calls(expr) {
                    self.hoist(expr)?
                } else {
                    expr.clone()
                };
                let precision = floating::precision(&expr, self.m.variables, self.m.name_to_id);
                (
                    self.eval(&expr, None)?,
                    self.expr_signed(&expr),
                    false,
                    precision,
                )
            }
            sv::ir::SystemTaskArg::Str(text) => {
                let bytes = unescape(text).into_bytes();
                let reg = self.b.alloc_bit((bytes.len() * 8).max(8), false);
                self.b.emit(SIRInstruction::Imm(
                    reg,
                    SIRValue::new(BigUint::from_bytes_be(&bytes)),
                ));
                (reg, false, true, None)
            }
            _ => return Err(unsupported("empty file I/O argument")),
        };
        let width = self.b.register(&reg).width();
        let four_state = matches!(self.b.register(&reg), celox_sir::RegisterType::Logic { .. });
        let (id, _) = self.m.temp("file_arg", width, signed, four_state);
        self.store(id, SIROffset::Static(0), width, reg);
        Ok(HostRequestValue {
            value_kind: celox_design::ValueKind::Bits,
            id,
            width,
            signed,
            is_string,
            float_precision,
        })
    }

    pub(super) fn file_descriptor_arg(
        &mut self,
        arg: &sv::ir::SystemTaskArg<sv::ir::Expr>,
    ) -> Result<HostRequestValue<SourceVarId>, sv::AnalyzerError> {
        match arg {
            sv::ir::SystemTaskArg::Expr(expr) => {
                let expr = self.int_formal(expr.clone());
                self.file_io_arg(&sv::ir::SystemTaskArg::Expr(expr))
            }
            _ => self.file_io_arg(arg),
        }
    }

    pub(super) fn file_io_request(
        &mut self,
        operation: u32,
        args: Vec<HostRequestValue<SourceVarId>>,
        results: Vec<HostRequestValue<SourceVarId>>,
        strings: Vec<String>,
    ) -> Result<(), sv::AnalyzerError> {
        if self.kernel.is_none() {
            return Err(unsupported("file I/O outside a scheduled process"));
        }
        let site = self
            .host_request_sites
            .iter()
            .filter(|site| site.kind == HostRequestKind::FileIo)
            .count() as u32;
        self.host_request_sites.push(HostRequestSite {
            process: 0,
            kind: HostRequestKind::FileIo,
            operation,
            args,
            results,
            strings,
        });
        self.with_kernel(|kernel| kernel.host_request(HostRequestKind::FileIo, site))
            .map_err(|error| unsupported(error.to_string()))
    }

    pub(super) fn fopen(
        &mut self,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let mut values = Vec::new();
        for arg in args {
            let expr = arg
                .as_ref()
                .ok_or_else(|| unsupported("omitted $fopen argument"))?;
            values.push(self.file_io_arg(&sv::ir::SystemTaskArg::Expr(expr.clone()))?);
        }
        let (id, name) = self.m.temp("fopen", 32, false, false);
        self.file_io_request(
            0,
            values,
            vec![HostRequestValue {
                value_kind: celox_design::ValueKind::Bits,
                id,
                width: 32,
                signed: false,
                is_string: false,
                float_precision: None,
            }],
            Vec::new(),
        )?;
        Ok(sv::ir::Expr::Ident(name))
    }

    /// Capture writable text outputs once in argument order.
    pub(super) fn string_file_function(
        &mut self,
        function: sv::ir::SystemFunction,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use celox_design::ValueKind;
        use sv::ir::{BinaryOp, Expr};
        let error_status = function == sv::ir::SystemFunction::Ferror;
        let name = function.name();
        if self.kernel.is_none() {
            return Err(unsupported(format!("{name} outside a scheduled process")));
        }
        let destination = args[usize::from(error_status)]
            .as_ref()
            .ok_or_else(|| unsupported(format!("omitted {name} destination")))?;
        let parts = lvalue_from_expr(destination)
            .ok_or_else(|| unsupported(format!("{name} requires a writable text destination")))?;
        let [target] = parts.as_slice() else {
            return Err(unsupported(format!(
                "{name} requires a single text destination"
            )));
        };
        let id = self
            .m
            .id(target.name())
            .ok_or_else(|| unsupported(format!("{name} destination")))?;
        let variable = self.m.var(id);
        let string_target = variable.value_kind == ValueKind::StrRef;
        let width = self.lvalue_width(target)?;
        if matches!(target, sv::ir::LValue::Ident(_)) && !variable.array_dims.is_empty()
            || if string_target {
                width != celox_design::HANDLE_WIDTH
            } else {
                !error_status || variable.value_kind != ValueKind::Bits || width < 640
            }
        {
            return Err(unsupported(format!(
                "{name} requires a string{} destination",
                if error_status {
                    " or packed buffer of at least 640 bits"
                } else {
                    ""
                }
            )));
        }
        let descriptor = args[usize::from(!error_status)]
            .as_ref()
            .ok_or_else(|| unsupported(format!("omitted {name} descriptor")))?;
        // $ferror evaluates fd before its output index; $fgets has reverse order.
        let early_descriptor = if error_status {
            Some(self.file_descriptor_arg(&sv::ir::SystemTaskArg::Expr(descriptor.clone()))?)
        } else {
            None
        };
        let (lvalues, _, guards) = self.capture_system_actual(destination)?;
        let descriptor = match early_descriptor {
            Some(value) => value,
            None => self.file_descriptor_arg(&sv::ir::SystemTaskArg::Expr(descriptor.clone()))?,
        };
        let value_kind = if string_target {
            ValueKind::StrRef
        } else {
            ValueKind::Bits
        };
        let (count, count_name) = self.m.temp("file_result", 32, true, false);
        let (line, line_name) = self.m.temp("file_text", width, false, false);
        self.m
            .variables
            .get_mut(&line)
            .expect("file text result")
            .value_kind = value_kind;
        self.file_io_request(
            if error_status { 15 } else { 14 },
            vec![descriptor],
            vec![
                HostRequestValue {
                    value_kind: ValueKind::Bits,
                    id: count,
                    width: 32,
                    signed: true,
                    is_string: false,
                    float_precision: None,
                },
                HostRequestValue {
                    value_kind,
                    id: line,
                    width,
                    signed: false,
                    is_string: false,
                    float_precision: None,
                },
            ],
            Vec::new(),
        )?;
        let join = if error_status {
            None
        } else {
            let condition = Expr::Binary {
                left: Box::new(Expr::Ident(count_name.clone())),
                op: BinaryOp::Gt,
                right: Box::new(Expr::Literal("0".into())),
            };
            let (condition, _) = self.eval_truth(&condition)?;
            let write = self.b.new_block();
            let join = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: condition,
                true_block: (write, Vec::new()),
                false_block: (join, Vec::new()),
            });
            self.b.switch_to_block(write);
            Some(join)
        };
        self.check_tagged_guards(&guards)?;
        let value = Expr::Ident(line_name);
        match lvalues.as_slice() {
            [one] => self.assign(one, &value)?,
            _ => return Err(unsupported(format!("{name} text destination"))),
        }
        if let Some(join) = join {
            self.jump(join);
            self.b.switch_to_block(join);
        }
        Ok(Expr::Ident(count_name))
    }

    pub(super) fn file_function(
        &mut self,
        function: sv::ir::SystemFunction,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::SystemFunction;
        if self.kernel.is_none() {
            return Err(unsupported(format!(
                "system function `{function}` requires a scheduled process"
            )));
        }
        let operation = match function {
            SystemFunction::Ftell => 7,
            SystemFunction::Fseek => 8,
            SystemFunction::Rewind => 9,
            SystemFunction::Fgetc => 10,
            SystemFunction::Ungetc => 11,
            SystemFunction::Feof => 12,
            _ => unreachable!(),
        };
        let mut values = Vec::new();
        for (index, arg) in args.iter().enumerate() {
            let expr = arg
                .as_ref()
                .ok_or_else(|| unsupported(format!("omitted argument of `{function}`")))?;
            let signed = function == SystemFunction::Fseek && index > 0
                || function == SystemFunction::Ungetc && index == 0;
            // Extend using the actual's signedness before interpreting the
            // resulting 32 bits as the formal int/int-unsigned argument.
            let expr = self.int_formal(expr.clone());
            let mut value = self.file_io_arg(&sv::ir::SystemTaskArg::Expr(expr))?;
            value.signed = signed;
            values.push(value);
        }
        let (id, name) = self.m.temp("file_result", 32, true, false);
        self.file_io_request(
            operation,
            values,
            vec![HostRequestValue {
                value_kind: celox_design::ValueKind::Bits,
                float_precision: None,
                id,
                width: 32,
                signed: true,
                is_string: false,
            }],
            Vec::new(),
        )?;
        Ok(sv::ir::Expr::Ident(name))
    }

    pub(super) fn file_task(
        &mut self,
        task: sv::ir::SystemTask,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
    ) -> Result<bool, sv::AnalyzerError> {
        use sv::ir::SystemTask;
        if self.kernel.is_none() {
            return Err(unsupported(format!("`{task}` outside a scheduled process")));
        }
        match task {
            SystemTask::Fflush => {
                let mut values = Vec::new();
                for arg in args {
                    values.push(self.file_descriptor_arg(arg)?);
                }
                self.file_io_request(6, values, Vec::new(), Vec::new())?;
            }
            SystemTask::Fclose => {
                let descriptor = self.file_io_arg(&args[0])?;
                self.file_io_request(1, vec![descriptor], Vec::new(), Vec::new())?;
            }
            SystemTask::Fdisplay(radix) | SystemTask::Fwrite(radix) => {
                let descriptor = self.file_io_arg(&args[0])?;
                let kind = SystemTaskKind::Print(RuntimeEventKind::Write, radix.letter());
                let (template, mut args) = system_task_template(self.m, &kind, &args[1..]);
                prepare_time_operands(self.m, template.as_deref(), &mut args)?;
                let mut values = vec![descriptor];
                for arg in &args {
                    values.push(self.file_io_arg(arg)?);
                }
                let operation = if matches!(task, SystemTask::Fdisplay(_)) {
                    3
                } else {
                    2
                };
                self.file_io_request(
                    operation,
                    values,
                    Vec::new(),
                    vec![template.unwrap_or_default()],
                )?;
            }
            SystemTask::Writemem(format) => {
                let filename = self.file_io_arg(&args[0])?;
                let Some(sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Ident(name))) = args.get(1)
                else {
                    return Err(unsupported(format!(
                        "`{task}` requires a static unpacked array"
                    )));
                };
                let id = self
                    .m
                    .id(name)
                    .ok_or_else(|| unsupported(format!("`{task}` array `{name}`")))?;
                let (left, right, width) = self.m.file_memory_shape(id)?;
                let mut addresses = Vec::new();
                for arg in args.iter().skip(2) {
                    addresses.push(self.file_io_arg(arg)?);
                }
                // The memory is a reference operand: snapshot after evaluating
                // the value arguments, including any address-expression effects.
                let mut values = vec![filename];
                let array = self.file_io_arg(&sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Ident(
                    name.clone(),
                )))?;
                values.push(array);
                values.extend(addresses);
                self.file_io_request(
                    if format == sv::ir::MemoryFormat::Binary {
                        5
                    } else {
                        4
                    },
                    values,
                    Vec::new(),
                    vec![left.to_string(), right.to_string(), width.to_string()],
                )?;
            }
            _ => unreachable!(),
        }
        Ok(true)
    }
}
