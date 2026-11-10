//! Event controls (IEEE 1800-2023 9.4.2): the terms a process waits on,
//! their `iff` gates, and the `$event[k]` variables of event expressions.

use super::*;

impl Ff<'_, '_> {
    /// `@(items)` (IEEE 1800-2023 9.4.2): the process waits for an
    /// occurrence of any item, which the runtime detects on the watched
    /// signals themselves, so nothing is left to check when it resumes.
    ///
    /// An item other than a variable of the module is watched through the
    /// variable `$event[k]` of a continuous assignment of the expression:
    /// each update of its value is an occurrence, and an edge is one of the
    /// least significant bit of the expression (Table 9-2). Before it
    /// suspends, the process copies the process-local operands the
    /// assignment reads and stores the expression's value, which the event
    /// control samples as it begins.
    pub(super) fn wait_event(
        &mut self,
        items: &[sv::ir::EventItem],
    ) -> Result<(), sv::AnalyzerError> {
        if self.property_event(items)? {
            return Ok(());
        }
        if self.kernel.is_none() && self.can_suspend_unit() {
            return self.unit_wait_event(items);
        }
        let mut terms = Vec::new();
        let mut gates = Vec::new();
        let mut samples = Vec::new();
        self.event_terms(items, &mut terms, &mut gates, &mut samples)?;
        self.suspend_on(terms, gates, samples)
    }

    /// `@*` (IEEE 1800-2023 9.4.2.2): wait for a change of a variable that
    /// `reads` read. The module variables are those a combinational
    /// `always @*` is sensitive to, from the same analysis, so folded
    /// branches and the defaults of omitted subroutine arguments count. A
    /// process-local variable it reads is sampled like an operand of an
    /// event expression.
    pub(super) fn wait_implicit(
        &mut self,
        reads: &[sv::ir::Stmt],
        always_comb: bool,
    ) -> Result<(), sv::AnalyzerError> {
        // The expressions of timing controls are not part of the event list.
        let reads = without_timing_controls(reads);
        let reads = reads.as_slice();
        if always_comb {
            let sources = self.m.implicit_reads(reads, true);
            let mut writes = Vec::new();
            super::super::comb::static_writes(
                self.m,
                reads,
                &mut HashSet::default(),
                &mut writes,
                true,
            );
            let writes = writes
                .into_iter()
                .map(|(id, access)| VarAtomBase::new(id, access.lsb, access.msb))
                .collect::<Vec<_>>();
            let sources = super::super::comb::subtract_written_sensitivity(sources, &writes);
            let mut sources: Vec<_> = sources
                .into_iter()
                .filter(|atom| !self.m.is_hidden(atom.id))
                .collect();
            sources.sort_by_key(|atom| (atom.id.0, atom.access.lsb, atom.access.msb));
            let mut terms = Vec::new();
            let mut gates = Vec::new();
            let mut samples = Vec::new();
            let mut selected = Vec::new();
            for source in sources {
                if source.access.lsb == 0 && source.access.msb + 1 == self.m.var(source.id).width {
                    terms.push((source.id, CounterKind::Changes));
                    gates.push(None);
                    continue;
                }
                let name = self
                    .m
                    .name_to_id
                    .iter()
                    .filter_map(|(name, id)| (*id == source.id).then_some(name))
                    .min()
                    .cloned()
                    .ok_or_else(|| unsupported("implicit sensitivity variable"))?;
                selected.push(sv::ir::EventItem {
                    edge: sv::ir::EventEdge::Any,
                    expr: sv::ir::Expr::Select {
                        // Sensitivity ranges are storage offsets. A concatenation
                        // normalizes shifted, ascending and multidimensional
                        // declarations to a zero-based packed value.
                        expr: Box::new(sv::ir::Expr::Concat(vec![sv::ir::Expr::Ident(name)])),
                        msb: sv::ir::ConstExpr::Literal(source.access.msb.to_string()),
                        lsb: sv::ir::ConstExpr::Literal(source.access.lsb.to_string()),
                        signed: false,
                    },
                    iff: None,
                });
            }
            self.event_terms(&selected, &mut terms, &mut gates, &mut samples)?;
            return self.suspend_on(terms, gates, samples);
        }
        let mut ids: Vec<SourceVarId> = self
            .m
            .implicit_reads(reads, false)
            .into_iter()
            .map(|atom| atom.id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let mut terms: Vec<_> = ids
            .into_iter()
            .map(|id| (id, CounterKind::Changes))
            .collect();
        let mut names = HashSet::default();
        for stmt in reads {
            stmt.clone().visit_mut(
                &mut |expr: &mut sv::ir::Expr| expr_idents(expr, &mut names),
                &mut |_: &mut sv::ir::LValue| {},
                &mut |_: &mut String| {},
            );
        }
        let mut locals: Vec<String> = names
            .into_iter()
            .filter(|name| self.m.id(name).is_some_and(|id| self.m.is_hidden(id)))
            .collect();
        locals.sort_unstable();
        let locals: Vec<_> = locals
            .into_iter()
            .map(|name| sv::ir::EventItem {
                edge: sv::ir::EventEdge::Any,
                expr: sv::ir::Expr::Ident(name),
                iff: None,
            })
            .collect();
        let mut gates = vec![None; terms.len()];
        let mut samples = Vec::new();
        self.event_terms(&locals, &mut terms, &mut gates, &mut samples)?;
        self.suspend_on(terms, gates, samples)
    }

    /// Reject an edge of event expression `expr` when it is real: an edge
    /// is of the least significant bit of an integral value (IEEE 1800-2023
    /// 9.4.2).
    pub(in crate::lowering) fn check_event_edge(
        &self,
        edge: sv::ir::EventEdge,
        expr: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        let edge = match edge {
            sv::ir::EventEdge::Any => return Ok(()),
            sv::ir::EventEdge::Pos => "posedge",
            sv::ir::EventEdge::Neg => "negedge",
            sv::ir::EventEdge::Both => "edge",
        };
        if self.m.expr_float_precision(expr).is_some() {
            return Err(sv::AnalyzerError::RealEdgeEvent { edge });
        }
        Ok(())
    }

    /// The counter terms of `items`, their `iff` gates, and the samples the
    /// process takes of their expressions before it suspends.
    pub(super) fn event_terms(
        &mut self,
        items: &[sv::ir::EventItem],
        terms: &mut Vec<(SourceVarId, CounterKind)>,
        gates: &mut Vec<Option<WaitGate<SourceVarId>>>,
        samples: &mut Vec<(String, sv::ir::Expr)>,
    ) -> Result<(), sv::AnalyzerError> {
        for item in items {
            let expr = self.propagate(&item.expr);
            self.check_event_edge(item.edge, &expr)?;
            let mut variable = false;
            let id = match &expr {
                sv::ir::Expr::Ident(name) => match self.m.id(name) {
                    Some(id)
                        if !self.m.is_hidden(id)
                            || (self.forked && self.frame_locals.contains(&id)) =>
                    {
                        variable = self.m.var(id).array_dims.is_empty();
                        id
                    }
                    Some(_) => self.event_expression(&expr, samples)?,
                    // A constant never changes.
                    None if self.m.constants.contains_key(name) => continue,
                    None => return Err(unsupported(format!("event expression `{name}`"))),
                },
                _ => self.event_expression(&expr, samples)?,
            };
            let gate = match &item.iff {
                Some(iff) => Some(self.event_gate(iff, samples)?),
                None => None,
            };
            let kinds: &[CounterKind] = match item.edge {
                sv::ir::EventEdge::Any => &[CounterKind::Changes],
                sv::ir::EventEdge::Pos => &[CounterKind::Rises],
                sv::ir::EventEdge::Neg => &[CounterKind::Falls],
                sv::ir::EventEdge::Both => &[CounterKind::Rises, CounterKind::Falls],
            };
            for &kind in kinds {
                terms.push((id, kind));
                gates.push(gate.map(|(signal, gate_variable)| {
                    WaitGate {
                        signal,
                        at_write: variable && gate_variable,
                        // Shared module storage has counters; activation-local
                        // operands are read at each process assignment boundary.
                        counters: (variable
                            && gate_variable
                            && !self.m.is_hidden(id)
                            && !self.m.is_hidden(signal))
                        .then(|| self.gate_counters()),
                    }
                }));
            }
        }
        Ok(())
    }

    /// Suspend until one of `terms` counts while its gate in `gates` holds,
    /// after taking `samples`.
    pub(super) fn suspend_on(
        &mut self,
        terms: Vec<(SourceVarId, CounterKind)>,
        mut gates: Vec<Option<WaitGate<SourceVarId>>>,
        samples: Vec<(String, sv::ir::Expr)>,
    ) -> Result<(), sv::AnalyzerError> {
        if self.kernel.is_none() {
            self.unit_samples(samples)?;
        } else {
            for (target, value) in samples {
                self.assign(&sv::ir::LValue::Ident(target), &value)?;
            }
        }
        if gates.iter().all(Option::is_none) {
            gates.clear();
        }
        let site = self.wait_site(terms, gates);
        if self.kernel.is_none() {
            self.unit_wait(u64::from(site))?;
            self.unit_flush_point();
            return Ok(());
        }
        self.with_kernel(|kernel| -> Result<(), ProcessKernelError> {
            kernel.suspend_wait(site)?;
            let woken = kernel.builder().alloc_bit(1, false);
            kernel
                .builder()
                .emit(SIRInstruction::Imm(woken, SIRValue::new(1u8)));
            kernel.wake_if(woken);
            Ok(())
        })
        .map_err(kernel_error)
    }

    /// The signal whose least significant bit is the `iff` condition `iff`
    /// of an event item (IEEE 1800-2023 9.4.2.3): a one-bit variable itself,
    /// or the variable `$event[k]` of the condition's truth value. Its
    /// operands are not terms of the wait; the runtime reads it when it
    /// detects an occurrence.
    pub(super) fn event_gate(
        &mut self,
        iff: &sv::ir::Expr,
        samples: &mut Vec<(String, sv::ir::Expr)>,
    ) -> Result<(SourceVarId, bool), sv::AnalyzerError> {
        let iff = self.propagate(iff);
        if let sv::ir::Expr::Ident(name) = &iff
            && let Some(id) = self.m.id(name)
            && (!self.m.is_hidden(id) || self.forked && self.frame_locals.contains(&id))
            && self.m.var(id).width == 1
            && self.m.var(id).float.is_none()
            && self.m.var(id).array_dims.is_empty()
        {
            return Ok((id, true));
        }
        Ok((
            self.event_expression(&calls::iff_truth(iff), samples)?,
            false,
        ))
    }

    /// The counters of a new gated term ([`GateCounters`]): `$event[k]`
    /// counts the occurrences while the gate holds, `$event[k].all` all
    /// of them.
    pub(in crate::lowering) fn gate_counters(&mut self) -> GateCounters<SourceVarId> {
        let index = self.m.next_event_index();
        let counter = SvVariable {
            float: None,
            path: Vec::new(),
            width: EVENT_COUNTER_WIDTH,
            signed: false,
            is_4state: false,
            is_unpacked_struct: false,
            packed_ranges: vec![(EVENT_COUNTER_WIDTH as i128 - 1, 0)],
            array_dims: Vec::new(),
            unpacked_ranges: Vec::new(),
            domain_kind: DomainKind::Other,
            kind: VariableKind::Variable,
            type_kind: PortTypeKind::Bit,
            source: None,
            hidden: true,
            force_control: None,
            value_kind: Default::default(),
            sampled_from: None,
            history_of: None,
            activation_scratch: false,
        };
        let (gated, _) = self.m.event_variable(index, None, counter.clone());
        let (all, _) = self
            .m
            .event_variable(index, Some("all".to_string()), counter);
        GateCounters { gated, all }
    }

    /// The variable `$event[k]` of a new event expression `expr`, with its
    /// continuous assignment added to the module. `samples` gets the
    /// assignments the process makes before it suspends: the copies of the
    /// process-local operands, then `$event[k]` itself.
    pub(super) fn event_expression(
        &mut self,
        expr: &sv::ir::Expr,
        samples: &mut Vec<(String, sv::ir::Expr)>,
    ) -> Result<SourceVarId, sv::AnalyzerError> {
        self.check_event_purity(expr)?;
        // A real expression keeps its payload bits, so a change too small
        // to survive an integer conversion still wakes the process.
        let float = self.m.expr_float_precision(expr);
        let width = match float {
            Some(celox_sir::FloatPrecision::F64) => 64,
            Some(celox_sir::FloatPrecision::F32) => 32,
            None => sv_expr_natural_width(
                &self.m.typed_calls(expr),
                self.m.variables,
                self.m.name_to_id,
                self.m.constants,
                self.m.parameter_types,
            )
            .ok_or_else(|| unsupported("event expression of unknown width"))?,
        };
        let signed = self.expr_signed(expr);
        let mut names = HashSet::default();
        expr_idents(expr, &mut names);
        let mut locals: Vec<_> = names
            .into_iter()
            .filter_map(|name| {
                let id = self.m.id(&name)?;
                self.m.is_hidden(id).then_some((name, id))
            })
            .collect();
        locals.sort_unstable();
        if self.forked && locals.iter().any(|(_, id)| self.frame_locals.contains(id)) {
            return self.activation_event_expression(expr, width, signed, float);
        }
        // Another call can copy a new argument into a static formal while
        // the process waits, which a copy taken now would not see.
        if locals.iter().any(|&(_, id)| self.is_static_task_formal(id)) {
            return Err(unsupported(
                "event expression reading a static task argument",
            ));
        }
        let index = self.m.next_event_index();
        let (event, event_name) = self.m.event_variable(
            index,
            None,
            SvVariable {
                float,
                path: Vec::new(),
                width,
                signed,
                is_4state: float.is_none(),
                is_unpacked_struct: false,
                packed_ranges: vec![(width as i128 - 1, 0)],
                array_dims: Vec::new(),
                unpacked_ranges: Vec::new(),
                domain_kind: DomainKind::Other,
                kind: VariableKind::Variable,
                type_kind: if float.is_some() {
                    PortTypeKind::Bit
                } else {
                    PortTypeKind::Logic
                },
                source: None,
                hidden: true,
                force_control: None,
                sampled_from: None,
                history_of: None,
                activation_scratch: false,
                value_kind: Default::default(),
            },
        );
        let mut renames = HashMap::default();
        for (position, (name, id)) in locals.into_iter().enumerate() {
            let variable = self.m.var(id).clone();
            let (copy, copy_name) =
                self.m
                    .event_variable(index, Some(format!("arg[{position}]")), variable);
            if let Some(width) = self.element_widths.get(&stable(id)).copied() {
                self.element_widths.insert(stable(copy), width);
                self.element_widths.insert(working(copy), width);
            }
            samples.push((copy_name.clone(), sv::ir::Expr::Ident(name.clone())));
            renames.insert(name, copy_name);
        }
        let value = rename_idents(expr, &renames);
        samples.push((event_name.clone(), value.clone()));
        self.m.event_assignments.push((
            event,
            vec![sv::ir::Stmt::Assign {
                lhs: sv::ir::LValue::Ident(event_name),
                rhs: value,
                nonblocking: false,
            }],
        ));
        Ok(event)
    }

    /// An event expression is evaluated whenever the host looks for its
    /// occurrences, as often as it likes: one that calls something with an
    /// effect is unsupported, in a kernel and in a unit alike.
    pub(in crate::lowering) fn check_event_purity(
        &self,
        expr: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        match impure_call(self.m, expr) {
            Some(name) => Err(unsupported(format!(
                "impure call `{name}` in an event expression"
            ))),
            None => Ok(()),
        }
    }

    /// An activation-local expression cannot use a module-wide continuous
    /// proxy. Give it a pure dispatch entry and retain only its sampled result
    /// in the waiter's existing baseline.
    pub(super) fn activation_event_expression(
        &mut self,
        expr: &sv::ir::Expr,
        width: usize,
        signed: bool,
        float: Option<celox_sir::FloatPrecision>,
    ) -> Result<SourceVarId, sv::AnalyzerError> {
        let names = level_reads(self.m, expr);
        if names.iter().any(|name| {
            self.m
                .id(name)
                .is_some_and(|id| self.is_static_task_formal(id))
        }) {
            return Err(unsupported(
                "event expression reading a static task argument",
            ));
        }
        let mut inputs: Vec<_> = names
            .iter()
            .filter_map(|name| self.m.id(name))
            .filter(|id| !self.m.is_hidden(*id) || self.frame_locals.contains(id))
            .collect();
        inputs.sort_unstable();
        inputs.dedup();
        if inputs.iter().any(|id| !self.frame_locals.contains(id)) {
            return Err(unsupported(
                "activation-local event expression reading a non-automatic variable",
            ));
        }
        let (result, name) = self.m.temp("event_probe", width, signed, float.is_none());
        self.m.variables.get_mut(&result).unwrap().float = float;
        // Isolate expression code so every clobbered scratch slot can be
        // audited, including function formals declared before this kernel.
        let saved_builder = std::mem::take(&mut self.b);
        let kernel = self.kernel.take();
        let direct = std::mem::replace(&mut self.direct, true);
        // The probe may not call (see below).
        let inline_calls = std::mem::replace(&mut self.inline_calls, true);
        let lowered = self.assign(&sv::ir::LValue::Ident(name.clone()), expr);
        self.inline_calls = inline_calls;
        self.direct = direct;
        self.kernel = kernel;
        let expression_builder = std::mem::replace(&mut self.b, saved_builder);
        lowered?;
        let mut unit = seal_builder(expression_builder);
        let mut scratch = Vec::new();
        for instruction in unit.blocks.values().flat_map(|block| &block.instructions) {
            match instruction {
                SIRInstruction::Store(address, _, _, _, triggers, captures) => {
                    if address.region != STABLE_REGION
                        || !triggers.is_empty()
                        || !captures.is_empty()
                    {
                        return Err(unsupported("event expression with observable stores"));
                    }
                    scratch.push(address.var_id);
                }
                SIRInstruction::Commit(..)
                | SIRInstruction::RuntimeEvent { .. }
                | SIRInstruction::CombCaptureEvent { .. }
                | SIRInstruction::CombCaptureEnableIfChanged { .. }
                | SIRInstruction::ExternCall { .. }
                | SIRInstruction::Call { .. } => {
                    return Err(unsupported("event expression with runtime effects"));
                }
                SIRInstruction::Imm(..)
                | SIRInstruction::Binary(..)
                | SIRInstruction::Unary(..)
                | SIRInstruction::Load(..)
                | SIRInstruction::Concat(..)
                | SIRInstruction::Slice(..)
                | SIRInstruction::Mux(..) => {}
            }
        }
        scratch.sort_unstable();
        scratch.dedup();
        // Function formals and temporaries may predate this process frame.
        // Give the probe exclusive storage, so instrumentation of the normal
        // function cannot add observable counters or capture hooks here.
        let mut copies = HashMap::default();
        for id in &mut scratch {
            if *id == result {
                continue;
            }
            if !self.m.is_hidden(*id) || self.frame_locals.contains(id) {
                return Err(unsupported("event expression modifying persistent state"));
            }
            let variable = self.m.var(*id).clone();
            let (copy, copy_name) = self.m.temp(
                "event_scratch",
                variable.width,
                variable.signed,
                variable.is_4state,
            );
            self.m.variables.insert(
                copy,
                SvVariable {
                    path: vec![copy_name],
                    activation_scratch: true,
                    ..variable
                },
            );
            copies.insert(*id, copy);
            *id = copy;
        }
        for block in unit.blocks.values_mut() {
            for instruction in &mut block.instructions {
                *instruction = instruction.map_addr(|address| Addr {
                    var_id: copies
                        .get(&address.var_id)
                        .copied()
                        .unwrap_or(address.var_id),
                    ..*address
                });
            }
        }
        // AST names alone cannot expose implicit receivers or generated
        // aliases. Validate the complete lowered read set after remapping.
        for instruction in unit.blocks.values().flat_map(|block| &block.instructions) {
            if let SIRInstruction::Load(_, address, _, _) = instruction
                && (address.region != STABLE_REGION
                    || !(self.frame_locals.contains(&address.var_id)
                        || scratch.contains(&address.var_id)))
            {
                return Err(unsupported(
                    "activation-local event expression reading non-automatic storage",
                ));
            }
        }
        inputs = unit
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                SIRInstruction::Load(_, address, _, _)
                    if self.frame_locals.contains(&address.var_id) =>
                {
                    Some(address.var_id)
                }
                _ => None,
            })
            .collect();
        inputs.sort_unstable();
        inputs.dedup();
        let targets: Vec<_> = scratch
            .iter()
            .map(|&id| VarAtomBase::new(id, 0, self.m.var(id).width - 1))
            .collect();
        let widths = scratch
            .iter()
            .map(|&id| (id, self.m.var(id).width))
            .collect();
        if super::super::comb_retention::reads_prior_state(&unit, &targets, &widths) {
            return Err(unsupported(
                "activation-local event expression retaining function scratch state",
            ));
        }
        if super::super::comb_retention::retains_state(
            &unit,
            &[VarAtomBase::new(result, 0, width - 1)],
            &widths,
        ) {
            return Err(unsupported(
                "activation-local event expression without a definite result",
            ));
        }
        let (previous, resume) = self
            .with_kernel(ProcessKernelBuilder::begin_event_probe)
            .map_err(kernel_error)?;
        self.b.inline_execution_unit(&unit);
        self.with_kernel(|kernel| kernel.end_event_probe(previous));
        self.event_probes.push(celox_design::ProcessEventProbe {
            result,
            resume,
            inputs,
            scratch,
        });
        // Use the identical pure code for the initial baseline too. Lowering
        // the call again as ordinary process code could allocate activation
        // cells or suspend inside a function's temporary declarations.
        self.b.inline_execution_unit(&unit);
        Ok(result)
    }

    /// Whether `id` is a static formal of a task, which keeps one
    /// module-wide address for every call.
    pub(in crate::lowering) fn is_static_task_formal(&self, id: SourceVarId) -> bool {
        self.m.subroutines.values().any(|subroutine| {
            subroutine.is_task
                && subroutine.params.iter().any(|param| {
                    !subroutine.automatic_locals.contains(&param.name)
                        && self.m.id(&param.name) == Some(id)
                })
        })
    }
}

/// `stmts` without their delays, event controls and waits, at any depth.
pub(super) fn without_timing_controls(stmts: &[sv::ir::Stmt]) -> Vec<sv::ir::Stmt> {
    use sv::ir::Stmt;
    stmts
        .iter()
        .filter(|stmt| {
            !matches!(
                stmt,
                Stmt::Delay { .. } | Stmt::WaitEvent(_) | Stmt::WaitImplicit { .. } | Stmt::Wait(_)
            )
        })
        .map(|stmt| match stmt {
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => Stmt::If {
                condition: condition.clone(),
                then_body: without_timing_controls(then_body),
                else_body: without_timing_controls(else_body),
            },
            Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => Stmt::Case {
                kind: *kind,
                selector: selector.clone(),
                items: items
                    .iter()
                    .map(|item| sv::ir::CaseItem {
                        labels: item.labels.clone(),
                        body: without_timing_controls(&item.body),
                    })
                    .collect(),
                default: default.as_deref().map(without_timing_controls),
            },
            Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => Stmt::Loop {
                kind: kind.clone(),
                init: without_timing_controls(init),
                condition: condition.clone(),
                step: without_timing_controls(step),
                body: without_timing_controls(body),
            },
            Stmt::Block { id, body } => Stmt::Block {
                id: *id,
                body: without_timing_controls(body),
            },
            stmt => stmt.clone(),
        })
        .collect()
}

/// The name of a call in `expr` that has an effect or reads the state of the
/// simulation, directly or through the functions it calls: an event
/// expression must be a value of the variables it reads.
pub(super) fn impure_call(pm: &ProcModule<'_>, expr: &sv::ir::Expr) -> Option<String> {
    impure_call_reading(pm, expr, |_| false)
}

/// [`impure_call`], where object operation `op` is a value of what it
/// reads when `reads(op)`, besides container element reads.
pub(super) fn impure_call_reading(
    pm: &ProcModule<'_>,
    expr: &sv::ir::Expr,
    reads: impl Fn(&sv::ir::ObjectOp) -> bool,
) -> Option<String> {
    let mut pending = vec![expr.clone()];
    let mut visited = HashSet::default();
    while let Some(expr) = pending.pop() {
        let mut callees = Vec::new();
        expr_calls(&expr, &mut |callee| {
            callees.push(callee.clone());
            false
        });
        for callee in callees {
            let name = match callee {
                sv::ir::Callee::System(function) => {
                    if function.reads_simulation_state() {
                        return Some(function.name().to_string());
                    }
                    continue;
                }
                // A string operation or lookup is a value of its operands.
                sv::ir::Callee::String(_)
                | sv::ir::Callee::Lookup { .. }
                | sv::ir::Callee::Stream(_) => continue,
                // Container contents are immutable: reading an element is a
                // value of the container handle and index, unlike a class
                // property that can change without its handle changing.
                // A record of a structure element is immutable as well.
                sv::ir::Callee::Object(op)
                    if matches!(
                        *op,
                        sv::ir::ObjectOp::Container {
                            op: sv::ir::ContainerOp::Get,
                            ..
                        } | sv::ir::ObjectOp::Unbox(_)
                    ) || matches!(*op, sv::ir::ObjectOp::Load(field) if field.nullable)
                        || reads(&op) =>
                {
                    continue;
                }
                // A property can change without the handle changing.
                sv::ir::Callee::Object(op) => return Some(op.to_string()),
                sv::ir::Callee::Checked { .. } => return Some("tagged union access".into()),
                sv::ir::Callee::Subroutine(name) => name,
            };
            if !visited.insert(name.clone()) {
                continue;
            }
            if let Some(import) = pm.dpi_imports.get(&name) {
                if !import.is_pure() {
                    return Some(name);
                }
                continue;
            }
            let Some(subroutine) = pm.subroutine(&name) else {
                return Some(name);
            };
            if subroutine.is_task
                || subroutine
                    .params
                    .iter()
                    .any(|param| param.direction.is_written())
            {
                return Some(name);
            }
            pending.extend(
                subroutine
                    .params
                    .iter()
                    .filter_map(|param| param.default.clone()),
            );
            // Only the function's own variables may be written.
            let local =
                |lvalue: &sv::ir::LValue| pm.id(lvalue.name()).is_some_and(|id| pm.is_hidden(id));
            let mut effect = false;
            for stmt in &subroutine.body {
                stmt.walk(&mut |stmt| match stmt {
                    sv::ir::Stmt::Assign { lhs, .. } => effect |= !local(lhs),
                    sv::ir::Stmt::AssignConcat { parts, .. } => effect |= !parts.iter().all(local),
                    sv::ir::Stmt::Call {
                        name,
                        args,
                        arg_order,
                    } => pending.push(sv::ir::Expr::Call {
                        callee: sv::ir::Callee::Subroutine(name.clone()),
                        args: args.clone(),
                        arg_order: arg_order.clone(),
                    }),
                    sv::ir::Stmt::DisableFork
                    | sv::ir::Stmt::WaitFork
                    | sv::ir::Stmt::Fork { .. }
                    | sv::ir::Stmt::SystemTask { .. }
                    | sv::ir::Stmt::Delay { .. }
                    | sv::ir::Stmt::WaitEvent(_)
                    | sv::ir::Stmt::WaitImplicit { .. }
                    | sv::ir::Stmt::Wait(_) => effect = true,
                    _ => {}
                });
                stmt.clone().visit_mut(
                    &mut |expr| pending.push(expr.clone()),
                    &mut |_| {},
                    &mut |_| {},
                );
            }
            if effect {
                return Some(name);
            }
        }
    }
    None
}
