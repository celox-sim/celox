//! System tasks in `always_ff`: runtime errors, runtime events and `$readmem`.

use super::*;

/// Runtime errors and event sites registered by the block, and how their ids are relocated
/// into the ids of the whole design.
pub(super) struct RuntimeSites {
    pub(super) runtime_errors: HashMap<i64, RuntimeErrorInfo<VarId>>,
    pub(super) runtime_event_sites: Vec<RuntimeEventSite>,
    /// The directories the block's memory file loads look relative names up
    /// in, by key.
    pub(super) memory_file_bases: Vec<celox_design::MemoryFileBase>,
    pub(super) next_runtime_error_code: i64,
    pub(super) runtime_error_code_map: Option<HashMap<i64, i64>>,
    pub(super) runtime_event_site_base: u32,
}

impl<'a> FfParser<'a> {
    pub(super) fn runtime_error(&mut self, message: impl Into<String>, signals: Vec<VarId>) -> i64 {
        let local_code = self.runtime.next_runtime_error_code;
        self.runtime.next_runtime_error_code += 1;
        let code = self
            .runtime
            .runtime_error_code_map
            .as_ref()
            .and_then(|mapping| mapping.get(&local_code).copied())
            .unwrap_or(local_code);
        self.runtime.runtime_errors.insert(
            code,
            RuntimeErrorInfo {
                message: message.into(),
                signals,
            },
        );
        code
    }

    pub(super) fn static_string_expr(expr: &Expression) -> Option<String> {
        if !expr.comptime().r#type.is_string() {
            return None;
        }
        let value = expr.comptime().get_value().ok()?;
        byte_value_to_string(value)
    }

    pub(super) fn register_runtime_event_site(
        &mut self,
        kind: RuntimeEventKind,
        args: &[SystemFunctionInput],
    ) -> u32 {
        let (template, value_args) = if args
            .first()
            .and_then(|arg| Self::static_string_expr(&arg.0))
            .is_some()
        {
            (
                args.first()
                    .and_then(|arg| Self::static_string_expr(&arg.0)),
                &args[1..],
            )
        } else {
            (None, args)
        };
        let site = RuntimeEventSite {
            kind,
            template,
            sizing: DisplaySizing::Veryl,
            scope: None,
            location: None,
            assertion: None,
            deferred: None,
            timestamp_arg: false,
            time_format_arg: false,
            arg_widths: value_args
                .iter()
                .map(|arg| self.get_expression_width(&arg.0))
                .collect(),
            arg_signed: value_args
                .iter()
                .map(|arg| arg.0.comptime().expr_context.signed)
                .collect(),
            arg_float_precision: value_args
                .iter()
                .map(|arg| crate::float::precision(crate::float::type_of(&arg.0)))
                .collect(),
            arg_is_string: value_args
                .iter()
                .map(|arg| arg.0.comptime().r#type.is_string())
                .collect(),
            arg_is_handle: Vec::new(),
        };
        let id = self
            .runtime
            .runtime_event_site_base
            .checked_add(self.runtime.runtime_event_sites.len() as u32)
            .expect("runtime event site identifier overflow");
        self.runtime.runtime_event_sites.push(site);
        id
    }

    pub(super) fn parse_runtime_event_expression<A>(
        &mut self,
        expr: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let state = self
            .get_bound_function_event_arg_state(expr.token_range())
            .cloned();
        let has_state = state.is_some();
        if let Some(state) = state {
            self.frames.function_arg_stack.push(state);
            self.frames
                .function_array_view_stack
                .push(HashMap::default());
            self.frames.function_array_view_enabled_stack.push(false);
        }
        let result =
            self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None);
        if has_state {
            self.frames.function_array_view_enabled_stack.pop();
            self.frames.function_array_view_stack.pop();
            self.frames.function_arg_stack.pop();
        }
        result
    }

    pub(super) fn emit_runtime_event<A>(
        &mut self,
        site_id: u32,
        args: &[SystemFunctionInput],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let value_args = if args
            .first()
            .and_then(|arg| Self::static_string_expr(&arg.0))
            .is_some()
        {
            &args[1..]
        } else {
            args
        };
        let mut regs = Vec::new();
        for arg in value_args {
            self.parse_runtime_event_expression(
                &arg.0, targets, domain, convert, sources, ir_builder,
            )?;
            regs.push(self.stack.pop_back().unwrap());
        }
        ir_builder.emit(SIRInstruction::RuntimeEvent {
            site_id,
            args: regs,
        });
        Ok(())
    }

    pub(super) fn prepare_effectful_runtime_event_args<A>(
        &mut self,
        args: &[SystemFunctionInput],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Vec<Option<RegisterId>>, ParserError> {
        let value_args = if args
            .first()
            .and_then(|arg| Self::static_string_expr(&arg.0))
            .is_some()
        {
            &args[1..]
        } else {
            args
        };
        let last_effectful_arg = value_args.iter().rposition(|arg| {
            expression::expression_has_side_effect(&arg.0)
                || self.expression_has_runtime_effect(&arg.0)
        });
        let Some(last_effectful_arg) = last_effectful_arg else {
            return Ok(vec![None; value_args.len()]);
        };

        // Evaluate only through the last effectful argument. Earlier pure reads
        // must be snapshotted before a later effect can change them, while pure
        // trailing arguments can stay lazy in the assertion failure block.
        value_args
            .iter()
            .enumerate()
            .map(|(index, arg)| {
                if index <= last_effectful_arg {
                    self.parse_runtime_event_expression(
                        &arg.0, targets, domain, convert, sources, ir_builder,
                    )?;
                    Ok(Some(self.stack.pop_back().unwrap()))
                } else {
                    Ok(None)
                }
            })
            .collect()
    }

    pub(super) fn emit_runtime_event_with_prepared_args<A>(
        &mut self,
        site_id: u32,
        args: &[SystemFunctionInput],
        prepared: Vec<Option<RegisterId>>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let value_args = if args
            .first()
            .and_then(|arg| Self::static_string_expr(&arg.0))
            .is_some()
        {
            &args[1..]
        } else {
            args
        };
        debug_assert_eq!(value_args.len(), prepared.len());
        let mut regs = Vec::with_capacity(value_args.len());
        for (arg, prepared) in value_args.iter().zip(prepared) {
            if let Some(reg) = prepared {
                regs.push(reg);
            } else {
                self.parse_runtime_event_expression(
                    &arg.0, targets, domain, convert, sources, ir_builder,
                )?;
                regs.push(self.stack.pop_back().unwrap());
            }
        }
        ir_builder.emit(SIRInstruction::RuntimeEvent {
            site_id,
            args: regs,
        });
        Ok(())
    }

    pub(super) fn parse_system_task_statement<A>(
        &mut self,
        call: &SystemFunctionCall,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        match &call.kind {
            SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                let kind = if matches!(call.kind, SystemFunctionKind::Display(_)) {
                    RuntimeEventKind::Display
                } else {
                    RuntimeEventKind::Write
                };
                let site_id = self.register_runtime_event_site(kind, args);
                self.emit_runtime_event(
                    site_id, args, targets, domain, convert, sources, ir_builder,
                )?;
                Ok(ControlFlow::Continue)
            }
            SystemFunctionKind::Finish => {
                // The host is notified through a runtime event and decides
                // when to stop (IEEE 1800-2023 20.2).
                let site_id = self.register_runtime_event_site(RuntimeEventKind::Finish, &[]);
                self.emit_runtime_event(
                    site_id,
                    &[],
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                Ok(ControlFlow::Continue)
            }
            SystemFunctionKind::Assert { kind, cond, args } => {
                self.parse_runtime_event_expression(
                    &cond.0, targets, domain, convert, sources, ir_builder,
                )?;
                let cond_reg = self.stack.pop_back().unwrap();
                let cond_reg = self.lower_procedural_condition(cond_reg, ir_builder);
                let pass_bb = ir_builder.new_block();
                let fail_bb = ir_builder.new_block();
                let event_kind = match kind {
                    AssertKind::Fatal => RuntimeEventKind::Report(Severity::Fatal),
                    AssertKind::Continue => RuntimeEventKind::Report(Severity::Error),
                };
                let site_id = self.register_runtime_event_site(event_kind, args);
                // Assertion message arguments are observationally eager: any
                // caller-visible effect must happen even when the assertion
                // passes. Keep pure arguments in the failure block so the
                // common passing path remains lazy.
                let prepared_args = self.prepare_effectful_runtime_event_args(
                    args, targets, domain, convert, sources, ir_builder,
                )?;
                ir_builder.seal_block(SIRTerminator::Branch {
                    cond: cond_reg,
                    true_block: (pass_bb, vec![]),
                    false_block: (fail_bb, vec![]),
                });
                ir_builder.switch_to_block(fail_bb);
                self.emit_runtime_event_with_prepared_args(
                    site_id,
                    args,
                    prepared_args,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                match kind {
                    AssertKind::Fatal => {
                        let message = if let Some(template) = args
                            .first()
                            .and_then(|arg| Self::static_string_expr(&arg.0))
                        {
                            template
                        } else {
                            "assertion failed".to_string()
                        };
                        let code = self.runtime_error(message, Vec::new());
                        ir_builder.seal_block(SIRTerminator::Error(code));
                    }
                    AssertKind::Continue => {
                        ir_builder.seal_block(SIRTerminator::Jump(pass_bb, vec![]));
                    }
                }
                ir_builder.switch_to_block(pass_bb);
                Ok(ControlFlow::Continue)
            }
            SystemFunctionKind::Readmemh(filename, output) => {
                self.parse_readmem_statement(
                    call, filename, output, targets, domain, convert, ir_builder,
                )?;
                Ok(ControlFlow::Continue)
            }
            SystemFunctionKind::Bits(_)
            | SystemFunctionKind::Size(..)
            | SystemFunctionKind::Clog2(_)
            | SystemFunctionKind::Onehot(_)
            | SystemFunctionKind::Signed(_)
            | SystemFunctionKind::Unsigned(_) => Err(ParserError::illegal_context(
                "system function call as statement",
                format!("{call}"),
                Some(&call.comptime.token),
            )),
        }
    }

    /// `$readmemh` in a clocked block loads the file into the destination
    /// each time the statement executes (IEEE 1800-2023 21.4): the heap's
    /// memory-file reader reads the file then, and the words it gives are
    /// stored, so locations the file does not name keep their values. A
    /// warning or an error of the load is reported then; an error
    /// terminates the load, keeping the words loaded before it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn parse_readmem_statement<A>(
        &mut self,
        call: &SystemFunctionCall,
        filename: &SystemFunctionInput,
        output: &veryl_analyzer::ir::SystemFunctionOutput,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let veryl_analyzer::ir::SystemFunctionOutput::Local(destinations) = output else {
            return Err(ParserError::illegal_context(
                "$readmemh destination",
                "a hierarchical destination is only valid in a testbench",
                Some(&call.comptime.token),
            ));
        };
        let (var_id, load) =
            crate::module::readmem_load(self.module, filename, destinations, 16, |dst| {
                dst.index
                    .0
                    .iter()
                    .map(|index| eval_constexpr(index)?.to_usize())
                    .collect()
            })?;
        // The code names the directory a relative file is looked up in by
        // its key in the design, whose paths are remapped with the rest.
        let mut load = load;
        if let Some(base) = load.base.take() {
            let base = celox_design::MemoryFileBase::new(base);
            load.base_key = Some(base.key);
            if !self.runtime.memory_file_bases.contains(&base) {
                self.runtime.memory_file_bases.push(base);
            }
        }
        let variable = &self.module.variables[&var_id];
        let four_state = !variable.r#type.is_2state();
        let element_width = load.element_width;
        let Some((lsb, msb)) = load.written_bits() else {
            return Ok(());
        };
        let access = BitAccess::new(lsb, msb);
        let store_region = if self.placement.sparse_write_vars.contains(&var_id) {
            SPARSE_WORKING_REGION
        } else {
            domain.region()
        };
        let direct_write = self
            .placement
            .direct_static_write_ranges
            .get(&var_id)
            .is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|range| range.lsb <= access.lsb && range.msb >= access.msb)
            });
        let store_region = if direct_write {
            STABLE_REGION
        } else {
            store_region
        };
        let messages = celox_frontend_core::memory_file::lowering::lower_memory_file_load(
            ir_builder,
            |b| b,
            &load,
            None,
            None,
            four_state,
            |b, position, word| {
                b.emit(SIRInstruction::Store(
                    convert(var_id, store_region),
                    SIROffset::Element {
                        index: position,
                        element_width,
                        bit_offset: 0,
                        dynamic_bit_offset: None,
                    },
                    element_width,
                    word,
                    Vec::new(),
                    Vec::new(),
                ));
            },
        );
        let bits = self.defined.defined_ranges.entry(var_id).or_default();
        for bit in access.lsb..=access.msb {
            bits.insert(bit);
        }
        self.defined.dynamic_defined_vars.insert(var_id);
        targets.push(VarAtomBase::new(
            convert(var_id, WORKING_REGION),
            access.lsb,
            access.msb,
        ));
        for (severity, message) in [
            (celox_design::Severity::Warning, messages.warning),
            (celox_design::Severity::Error, messages.error),
        ] {
            self.report_memory_file_message(severity, message, ir_builder);
        }
        Ok(())
    }

    /// Report string `message`, a 64-bit handle, at `severity` unless it is
    /// empty.
    fn report_memory_file_message<A>(
        &mut self,
        severity: celox_design::Severity,
        message: RegisterId,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        let present =
            celox_frontend_core::memory_file::lowering::message_present(ir_builder, message);
        let report = ir_builder.new_block();
        let done = ir_builder.new_block();
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: present,
            true_block: (report, Vec::new()),
            false_block: (done, Vec::new()),
        });
        ir_builder.switch_to_block(report);
        let handle = ir_builder.alloc_bit(celox_design::HANDLE_WIDTH, false);
        ir_builder.emit(SIRInstruction::Slice(
            handle,
            message,
            0,
            celox_design::HANDLE_WIDTH,
        ));
        let id = self
            .runtime
            .runtime_event_site_base
            .checked_add(self.runtime.runtime_event_sites.len() as u32)
            .expect("runtime event site identifier overflow");
        self.runtime.runtime_event_sites.push(RuntimeEventSite {
            kind: celox_design::RuntimeEventKind::Report(severity),
            template: Some("%s".to_string()),
            sizing: DisplaySizing::Veryl,
            scope: None,
            // As the other sites of the code, which name no source file.
            location: None,
            assertion: None,
            deferred: None,
            timestamp_arg: false,
            time_format_arg: false,
            arg_widths: vec![celox_design::HANDLE_WIDTH],
            arg_signed: vec![false],
            arg_float_precision: vec![None],
            arg_is_string: vec![false],
            arg_is_handle: vec![true],
        });
        ir_builder.emit(SIRInstruction::RuntimeEvent {
            site_id: id,
            args: vec![handle],
        });
        ir_builder.seal_block(SIRTerminator::Jump(done, Vec::new()));
        ir_builder.switch_to_block(done);
    }
}
