//! Building the semantic testbench program from its statements.

use super::*;

// ── Builder ────────────────────────────────────────────────────────────

pub(super) struct SemanticTestbenchBuilder<'a> {
    pub(super) lookup: &'a FrontendLookup,
    pub(super) testbench_source: &'a VerylTestbenchSource,
    pub(super) runtime_event_site_count: usize,
    pub(super) event_map: HashMap<StrId, StateAddr>,
    pub(super) signal_map: HashMap<StrId, SemanticSignal<StateAddr>>,
    pub(super) default_reset_duration: u64,
    pub(super) clock_periods: HashMap<StrId, u64>,
    pub(super) prepared_readmem: super::super::readmem::PreparedReadmem,
}

impl<'a> SemanticTestbenchBuilder<'a> {
    pub(super) fn new(
        lookup: &'a FrontendLookup,
        testbench_source: &'a VerylTestbenchSource,
        runtime_event_site_count: usize,
    ) -> Self {
        Self {
            lookup,
            testbench_source,
            runtime_event_site_count,
            event_map: Default::default(),
            signal_map: Default::default(),
            default_reset_duration: 3,
            clock_periods: Default::default(),
            prepared_readmem: Default::default(),
        }
    }

    /// The `$tb::clock_gen` instances of this source's module.
    pub(super) fn clock_generators(&self) -> impl Iterator<Item = TestbenchClock<StateAddr>> + '_ {
        self.testbench_source
            .clock_periods
            .iter()
            .filter_map(|(&name, &period)| {
                let (event, _) = self.named_variable(name)?;
                Some(TestbenchClock { event, period })
            })
    }

    pub(super) fn named_variable(&self, name: StrId) -> Option<(StateAddr, &VariableInfo)> {
        let instance = self.testbench_source.base_instance(self.lookup);
        let module = self.lookup.instance_module.get(&instance)?;
        let var = self
            .lookup
            .module_var_path_index
            .get(module)?
            .get(&vec![source_name(name)])?
            .as_ref()?;
        self.lookup.instance_variable(instance, *var)
    }

    pub(super) fn resource_name(&self, name: StrId) -> String {
        let local = resource_table::get_str_value(name).unwrap_or_default();
        let instance = self.testbench_source.base_instance(self.lookup);
        let prefix = self
            .lookup
            .instance_ids
            .iter()
            .find_map(|(path, &id)| {
                (id == instance).then(|| self.lookup.instance_path_segments(path).join("."))
            })
            .unwrap_or_default();
        if prefix.is_empty() {
            local
        } else {
            format!("{prefix}.{local}")
        }
    }

    pub(super) fn build_event_map(&mut self, stmts: &[Statement]) {
        let mut clock_insts: Vec<StrId> = Vec::new();
        let mut reset_insts: Vec<StrId> = Vec::new();
        let mut active_functions = FxHashSet::default();
        let mut periods = self.testbench_source.clock_periods.clone();
        self.scan_tb_methods(
            stmts,
            &mut clock_insts,
            &mut reset_insts,
            &mut active_functions,
            &mut periods,
        );
        self.clock_periods = periods;
        for inst in clock_insts.iter().chain(reset_insts.iter()) {
            if let Some((addr, info)) = self.named_variable(*inst) {
                let width = info.width;
                self.event_map.insert(*inst, addr);
                self.signal_map.insert(
                    *inst,
                    SemanticSignal {
                        address: addr,
                        width,
                    },
                );
            }
        }
    }

    pub(super) fn scan_tb_methods(
        &self,
        stmts: &[Statement],
        clks: &mut Vec<StrId>,
        rsts: &mut Vec<StrId>,
        active_functions: &mut FxHashSet<VarId>,
        periods: &mut HashMap<StrId, u64>,
    ) {
        for stmt in stmts {
            match stmt {
                Statement::TbMethodCall(tb) => match &tb.method {
                    TbMethod::ClockNext { period, .. } => {
                        periods.entry(tb.inst).or_insert_with(|| {
                            period
                                .as_deref()
                                .and_then(try_eval_const)
                                .unwrap_or(2)
                                .max(2)
                        });
                        if !clks.contains(&tb.inst) {
                            clks.push(tb.inst);
                        }
                    }
                    TbMethod::ResetAssert { clock, .. } => {
                        if !rsts.contains(&tb.inst) {
                            rsts.push(tb.inst);
                        }
                        if !clks.contains(clock) {
                            clks.push(*clock);
                        }
                    }
                    TbMethod::FileOpen { .. }
                    | TbMethod::FileWrite { .. }
                    | TbMethod::FileClose
                    | TbMethod::FileFlush
                    | TbMethod::Component { .. }
                    | TbMethod::RandomSeed { .. }
                    | TbMethod::RandomGet { .. }
                    | TbMethod::RandomGetRange { .. }
                    | TbMethod::RandomGetSeed => {}
                },
                Statement::If(s) => {
                    self.scan_tb_methods(&s.true_side, clks, rsts, active_functions, periods);
                    self.scan_tb_methods(&s.false_side, clks, rsts, active_functions, periods);
                }
                Statement::IfReset(s) => {
                    self.scan_tb_methods(&s.true_side, clks, rsts, active_functions, periods);
                    self.scan_tb_methods(&s.false_side, clks, rsts, active_functions, periods);
                }
                Statement::Case(s) => {
                    for arm in &s.arms {
                        self.scan_tb_methods(&arm.body, clks, rsts, active_functions, periods);
                    }
                    self.scan_tb_methods(&s.default, clks, rsts, active_functions, periods);
                }
                Statement::For(s) => {
                    self.scan_tb_methods(&s.body, clks, rsts, active_functions, periods);
                }
                Statement::FunctionCall(call) if active_functions.insert(call.id) => {
                    if let Some(body) = function_body(&self.testbench_source.functions, call) {
                        self.scan_tb_methods(
                            &body.statements,
                            clks,
                            rsts,
                            active_functions,
                            periods,
                        );
                    }
                    active_functions.remove(&call.id);
                }
                _ => {}
            }
        }
    }

    pub(super) fn convert(
        &mut self,
        stmts: &[Statement],
    ) -> Vec<Vec<SemanticStatement<StateAddr>>> {
        let base_instance = self.testbench_source.base_instance(self.lookup);
        let ec = ExprCompiler {
            lookup: self.lookup,
            id_map: &self.testbench_source.id_map,
            constant_strings: &self.testbench_source.constant_strings,
            functions: &self.testbench_source.functions,
            base_instance,
        };
        let site_count = count_assert_statements(stmts, &self.testbench_source.functions) as u32;
        let mut next_assert_site_id = self
            .runtime_event_site_count
            .saturating_sub(site_count as usize) as u32;
        self.testbench_source
            .initial_blocks()
            .into_iter()
            .map(|block| {
                block
                    .iter()
                    .filter_map(|s| self.convert_stmt(s, &ec, &mut next_assert_site_id))
                    .collect()
            })
            .collect()
    }

    pub(super) fn convert_component_bindings(
        &self,
    ) -> Result<Vec<SemanticComponentBinding<StateAddr>>, ParserError> {
        self.testbench_source
            .component_bindings
            .iter()
            .map(|component| {
                let ec = ExprCompiler {
                    lookup: self.lookup,
                    id_map: &self.testbench_source.id_map,
                    constant_strings: &self.testbench_source.constant_strings,
                    functions: &component.functions,
                    base_instance: component.parent_instance,
                };
                let connections = component
                    .connections
                    .iter()
                    .map(|connection| {
                        let output = match &connection.output {
                            Some(destination) => Some(ec.resolve_target(destination).ok_or_else(|| {
                                ParserError::illegal_context(
                                    "testbench component elaboration",
                                    format!(
                                        "component `{}` output `{}` has no runtime state binding",
                                        component.instance, connection.port
                                    ),
                                    None,
                                )
                            })?),
                            None => None,
                        };
                        let event = match &connection.event {
                            Some(reference) => Some({
                                let address = match reference {
                                    VerylComponentEventBinding::Root(id) => self
                                        .testbench_source
                                        .id_map
                                        .instance_var(
                                            self.lookup,
                                            component.parent_instance,
                                            *id,
                                        )
                                        .and_then(|source_id| {
                                            self.lookup
                                                .instance_variable(
                                                    component.parent_instance,
                                                    source_id,
                                                )
                                                .map(|(address, _)| address)
                                        }),
                                    VerylComponentEventBinding::Hierarchical(reference) => {
                                        resolve_hierarchical_reference_from(
                                            self.lookup,
                                            component.parent_instance,
                                            reference,
                                        )
                                            .ok()
                                            .map(|(address, _)| address)
                                    }
                                };
                                let address = address.ok_or_else(|| {
                                    ParserError::illegal_context(
                                        "testbench component elaboration",
                                        format!(
                                            "component `{}` event port `{}` has no runtime event binding",
                                            component.instance, connection.port
                                        ),
                                        None,
                                    )
                                })?;
                                self.lookup
                                    .event_aliases
                                    .get(&address)
                                    .copied()
                                    .unwrap_or(address)
                            }),
                            None => None,
                        };
                        Ok(celox_testbench::ComponentConnectionBinding {
                            port: connection.port.clone(),
                            input: connection.input.as_ref().map(|expr| ec.compile(expr)),
                            input_target: match &connection.input_target {
                                Some(input) => Some(
                                    ec.resolve_component_input_target(input).ok_or_else(|| {
                                        ParserError::illegal_context(
                                            "testbench component elaboration",
                                            format!(
                                                "component `{}` input port `{}` has no runtime state binding",
                                                component.instance, connection.port
                                            ),
                                            None,
                                        )
                                    })?,
                                ),
                                None => None,
                            },
                            output,
                            output_rtl_driven: false,
                            event,
                        })
                    })
                    .collect::<Result<Vec<_>, ParserError>>()?;
                Ok(celox_testbench::ComponentBinding {
                    instance: component.instance.clone(),
                    connections,
                })
            })
            .collect()
    }

    pub(super) fn convert_stmt(
        &self,
        stmt: &Statement,
        ec: &ExprCompiler<'_>,
        next_assert_site_id: &mut u32,
    ) -> Option<SemanticStatement<StateAddr>> {
        fn convert_for_bound(
            bound: &ForBound,
            ec: &ExprCompiler<'_>,
        ) -> GenericLoopBound<ExprBytecode<StateLocation<StateAddr>>> {
            match bound {
                ForBound::Const(x, true) => GenericLoopBound::Static(*x),
                ForBound::Const(x, false) => GenericLoopBound::Dynamic {
                    expr: ExprBytecode::new(vec![TbOpcode::ConstU64(*x as u64)]),
                    width: (usize::BITS as usize - x.leading_zeros() as usize).max(32),
                    signed: false,
                },
                ForBound::Expression(expr) => GenericLoopBound::Dynamic {
                    expr: ec.compile(expr.as_ref()),
                    width: ec.root_context(expr).width,
                    signed: expression_signed(expr),
                },
            }
        }

        match stmt {
            Statement::TbMethodCall(tb) => self.convert_tb_method(tb, ec),
            Statement::SystemFunctionCall(sf) => match &sf.kind {
                SystemFunctionKind::Readmemh(_, SystemFunctionOutput::Hier(_)) => {
                    self.prepared_readmem.get(&sf.comptime.token).cloned()
                }
                SystemFunctionKind::Assert { kind, cond, args } => {
                    let site_id = *next_assert_site_id;
                    *next_assert_site_id = next_assert_site_id.saturating_add(1);
                    Some(GenericTestbenchStatement::Assert {
                        expr: ec.compile(&cond.0),
                        site_id,
                        continue_on_fail: matches!(kind, AssertKind::Continue),
                        message: compile_assert_message(args, ec),
                        location: extract_source_location(&sf.comptime.token),
                    })
                }
                SystemFunctionKind::Finish => Some(GenericTestbenchStatement::Finish),
                SystemFunctionKind::Display(args) => Some(GenericTestbenchStatement::Display {
                    message: compile_assert_message(args, ec),
                    newline: true,
                }),
                SystemFunctionKind::Write(args) => Some(GenericTestbenchStatement::Display {
                    message: compile_assert_message(args, ec),
                    newline: false,
                }),
                _ => None,
            },
            Statement::If(s) => Some(GenericTestbenchStatement::If {
                expr: ec.compile(&s.cond),
                then_block: s
                    .true_side
                    .iter()
                    .filter_map(|s| self.convert_stmt(s, ec, next_assert_site_id))
                    .collect(),
                else_block: s
                    .false_side
                    .iter()
                    .filter_map(|s| self.convert_stmt(s, ec, next_assert_site_id))
                    .collect(),
            }),
            Statement::For(s) => {
                let body: Vec<_> = s
                    .body
                    .iter()
                    .filter_map(|s| self.convert_stmt(s, ec, next_assert_site_id))
                    .collect();
                let lv = self
                    .resolve_loop_var(&s.var_id)
                    .map(|(sig, width)| (sig, width, s.var_type.signed));
                match &s.range {
                    ForRange::Forward {
                        start,
                        end,
                        inclusive,
                        step,
                    } => Some(GenericTestbenchStatement::For {
                        loop_var: lv,
                        start: convert_for_bound(start, ec),
                        end: convert_for_bound(end, ec),
                        inclusive: *inclusive,
                        step: *step,
                        step_op: None,
                        reverse: false,
                        body,
                    }),
                    ForRange::Reverse {
                        start,
                        end,
                        inclusive,
                        step,
                    } => Some(GenericTestbenchStatement::For {
                        loop_var: lv,
                        start: convert_for_bound(start, ec),
                        end: convert_for_bound(end, ec),
                        inclusive: *inclusive,
                        step: *step,
                        step_op: None,
                        reverse: true,
                        body,
                    }),
                    ForRange::Stepped {
                        start,
                        end,
                        inclusive,
                        step,
                        op,
                    } => Some(GenericTestbenchStatement::For {
                        loop_var: lv,
                        start: convert_for_bound(start, ec),
                        end: convert_for_bound(end, ec),
                        inclusive: *inclusive,
                        step: *step,
                        step_op: Some(lower_testbench_operator(*op)),
                        reverse: false,
                        body,
                    }),
                }
            }
            Statement::Assign(a) => {
                let dst = if let Some(destination) = &a.hier_dst {
                    ec.resolve_hierarchical_target(destination)?
                } else {
                    ec.resolve_target(a.dst.first()?)?
                };
                let dst_width = dst.width;
                Some(GenericTestbenchStatement::Assign {
                    dst,
                    expr: ec.compile_with_width(
                        &crate::float::converted(
                            &a.expr,
                            a.hier_dst
                                .as_ref()
                                .map(|dst| &dst.comptime.r#type)
                                .unwrap_or_else(|| &a.dst.first().unwrap().comptime.r#type),
                        ),
                        dst_width,
                    ),
                })
            }
            Statement::Break => Some(GenericTestbenchStatement::Break),
            Statement::FunctionCall(fc) => self.convert_function_call(fc, ec, next_assert_site_id),
            _ => None,
        }
    }

    /// Inline-expand a function call by binding arguments and converting
    /// the function body's statements.
    pub(super) fn convert_function_call(
        &self,
        fc: &veryl_analyzer::ir::FunctionCall,
        ec: &ExprCompiler<'_>,
        next_assert_site_id: &mut u32,
    ) -> Option<SemanticStatement<StateAddr>> {
        let func = self.testbench_source.functions.get(&fc.id)?;
        let func_body = if let Some(idx) = &fc.index {
            func.get_function(idx)?
        } else {
            func.get_function(&[])?
        };

        // Build a list of statements: argument assignments + body
        let mut stmts: Vec<SemanticStatement<StateAddr>> = Vec::new();

        // Bind input arguments
        for (arg_path, arg_expr) in &fc.inputs {
            if let Some(&arg_var_id) = func_body.arg_map.get(arg_path) {
                if let Some(sig) = ec.resolve_var(&arg_var_id) {
                    stmts.push(GenericTestbenchStatement::Assign {
                        dst: TestbenchTarget {
                            signal: sig,
                            selection: None,
                            width: sig.width,
                        },
                        expr: ec.compile_with_width(
                            &func
                                .args
                                .iter()
                                .flat_map(|arg| &arg.members)
                                .find(|(path, _, _)| path == arg_path)
                                .map(|(_, ct, _)| crate::float::converted(arg_expr, &ct.r#type))
                                .unwrap_or(std::borrow::Cow::Borrowed(arg_expr)),
                            sig.width,
                        ),
                    });
                }
            }
        }

        // Inline body statements
        for stmt in &func_body.statements {
            if let Some(ts) = self.convert_stmt(stmt, ec, next_assert_site_id) {
                stmts.push(ts);
            }
        }

        if stmts.len() == 1 {
            Some(stmts.into_iter().next().unwrap())
        } else {
            // Wrap multiple statements into an If(true) block as a sequence container
            // (there's no "Block" variant in TestbenchStatement)
            // Actually, we can return None and use a different approach:
            // flatten into the parent's statement list.
            // For now, wrap in an always-true If:
            Some(GenericTestbenchStatement::If {
                expr: ExprBytecode::new(vec![TbOpcode::ConstU64(1)]),
                then_block: stmts,
                else_block: Vec::new(),
            })
        }
    }

    pub(super) fn convert_tb_method(
        &self,
        tb: &TbMethodCall,
        ec: &ExprCompiler<'_>,
    ) -> Option<SemanticStatement<StateAddr>> {
        match &tb.method {
            TbMethod::ClockNext { count, period } => {
                let ev = self.event_map.get(&tb.inst).copied()?;
                let clock_count = match count {
                    Some(expr) => {
                        if let Some(n) = try_eval_const(expr) {
                            GenericClockCount::Static(n)
                        } else {
                            GenericClockCount::Dynamic(ec.compile(expr))
                        }
                    }
                    None => GenericClockCount::Static(1),
                };
                Some(GenericTestbenchStatement::ClockNext {
                    clock_event: ev,
                    count: clock_count,
                    period: period
                        .as_deref()
                        .and_then(try_eval_const)
                        .unwrap_or(2)
                        .max(2),
                })
            }
            TbMethod::ResetAssert { clock, duration } => {
                let reset_signal = self.signal_map.get(&tb.inst).copied()?;
                let reset_event = self.event_map.get(&tb.inst).copied();
                let clock_event = self.event_map.get(clock).copied()?;
                let duration = match duration {
                    Some(expr) => match try_eval_const(expr) {
                        Some(duration) => GenericClockCount::Static(duration),
                        None => GenericClockCount::Dynamic(ec.compile(expr)),
                    },
                    None => GenericClockCount::Static(self.default_reset_duration),
                };
                // Determine reset polarity from the variable's DomainKind
                let (assert_value, deassert_value) = self.resolve_reset_polarity(&tb.inst);
                Some(GenericTestbenchStatement::ResetAssert {
                    reset_signal,
                    reset_event,
                    clock_event,
                    duration,
                    period: self.clock_periods.get(clock).copied().unwrap_or(2),
                    assert_value,
                    deassert_value,
                })
            }
            TbMethod::RandomSeed { value } => Some(GenericTestbenchStatement::RandomSeed {
                handle: self.resource_name(tb.inst),
                value: ec.compile(value),
            }),
            TbMethod::RandomGet { width, signed } => {
                let ret = match tb.ret.as_deref() {
                    Some(dst) => Some(ec.resolve_target(dst)?),
                    None => None,
                };
                Some(GenericTestbenchStatement::RandomGet {
                    handle: self.resource_name(tb.inst),
                    width: *width,
                    signed: *signed,
                    ret,
                })
            }
            TbMethod::RandomGetRange {
                min,
                max,
                width,
                signed,
            } => {
                let ret = match tb.ret.as_deref() {
                    Some(dst) => Some(ec.resolve_target(dst)?),
                    None => None,
                };
                Some(GenericTestbenchStatement::RandomGetRange {
                    handle: self.resource_name(tb.inst),
                    min: ec.compile(min),
                    max: ec.compile(max),
                    width: *width,
                    signed: *signed,
                    ret,
                })
            }
            TbMethod::RandomGetSeed => {
                let ret = match tb.ret.as_deref() {
                    Some(dst) => Some(ec.resolve_target(dst)?),
                    None => None,
                };
                Some(GenericTestbenchStatement::RandomGetSeed {
                    handle: self.resource_name(tb.inst),
                    ret,
                })
            }
            TbMethod::Component { method, args } => {
                let ret = match tb.ret.as_deref() {
                    Some(dst) => Some(ec.resolve_target(dst)?),
                    None => None,
                };
                Some(GenericTestbenchStatement::ComponentMethod {
                    instance: self.resource_name(tb.inst),
                    method: resource_table::get_str_value(*method).unwrap_or_default(),
                    args: args
                        .iter()
                        .map(|arg| compile_component_arg(arg, ec))
                        .collect(),
                    ret,
                    ret_width: tb.ret_width,
                    ret_signed: tb.ret_signed,
                    ret_strict: tb.ret_strict,
                })
            }
            TbMethod::FileOpen { .. }
            | TbMethod::FileWrite { .. }
            | TbMethod::FileClose
            | TbMethod::FileFlush => None,
        }
    }

    /// Determine reset assert/deassert values from the variable's PortTypeKind.
    /// PortTypeKind covers all four reset types (async/sync × high/low),
    /// unlike DomainKind which maps sync resets to Other.
    pub(super) fn resolve_reset_polarity(&self, inst: &StrId) -> (u8, u8) {
        if let Some((address, info)) = self.named_variable(*inst) {
            if let Some(Some(active_low)) = self
                .testbench_source
                .declared_reset_active_low
                .get(&address)
            {
                return if *active_low { (0, 1) } else { (1, 0) };
            }
            return match info.type_kind {
                PortTypeKind::ResetAsyncHigh | PortTypeKind::ResetSyncHigh => (1, 0),
                PortTypeKind::ResetAsyncLow | PortTypeKind::ResetSyncLow => (0, 1),
                _ => (0, 1),
            };
        }
        (0, 1)
    }

    pub(super) fn resolve_loop_var(
        &self,
        var_id: &VarId,
    ) -> Option<(SemanticSignal<StateAddr>, usize)> {
        let root_instance = self.testbench_source.base_instance(self.lookup);
        let source_id =
            self.testbench_source
                .id_map
                .instance_var(self.lookup, root_instance, *var_id)?;
        let (addr, info) = self.lookup.instance_variable(root_instance, source_id)?;
        Some((
            SemanticSignal {
                address: addr,
                width: info.width,
            },
            info.width,
        ))
    }
}
