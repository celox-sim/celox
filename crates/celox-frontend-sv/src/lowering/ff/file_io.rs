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
            id,
            width,
            signed,
            is_string,
            float_precision,
        })
    }

    fn file_descriptor_arg(
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
                let (template, args) = system_task_template(self.m, &kind, &args[1..]);
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
