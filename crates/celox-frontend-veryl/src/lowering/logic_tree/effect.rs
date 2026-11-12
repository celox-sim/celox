use super::state::FunctionLoopControlState;
use super::*;
use celox_design::Severity;

mod function_body;
mod statements;
use function_body::*;
pub(in crate::lowering::logic_tree) use statements::*;

pub(crate) fn subtract_written_sensitivity<A: Copy + Eq + std::hash::Hash>(
    atoms: impl IntoIterator<Item = VarAtomBase<A>>,
    written_atoms: &[VarAtomBase<A>],
) -> HashSet<VarAtomBase<A>> {
    let mut result = HashSet::default();
    for atom in atoms {
        let mut ranges = vec![(atom.access.lsb, atom.access.msb)];
        for written in written_atoms {
            if written.id != atom.id {
                continue;
            }
            ranges = ranges
                .into_iter()
                .flat_map(|(lsb, msb)| {
                    if written.access.msb < lsb || written.access.lsb > msb {
                        return vec![(lsb, msb)];
                    }
                    let mut kept = Vec::new();
                    if lsb < written.access.lsb {
                        kept.push((lsb, written.access.lsb - 1));
                    }
                    if written.access.msb < msb {
                        kept.push((written.access.msb + 1, msb));
                    }
                    kept
                })
                .collect();
        }
        for (lsb, msb) in ranges {
            result.insert(VarAtomBase::new(atom.id, lsb, msb));
        }
    }
    result
}

#[derive(Default)]
pub(crate) struct CombEffectCollector {
    pub(crate) observers: Vec<CombObserver<VarId>>,
    pub(crate) sites: Vec<RuntimeEventSite>,
    pub(crate) sensitivity: HashSet<VarAtomBase<VarId>>,
    active_guard: Option<NodeId>,
    active_guard_sources: HashSet<VarAtomBase<VarId>>,
    loop_effects: Option<Vec<SLTForEffect>>,
    capture_namespace: u32,
    next_capture_id: u32,
}

impl CombEffectCollector {
    pub(crate) fn with_capture_namespace(capture_namespace: u32) -> Self {
        Self {
            capture_namespace,
            ..Self::default()
        }
    }

    fn next_capture_key(
        &mut self,
        token: &veryl_parser::token_range::TokenRange,
    ) -> Result<u64, ParserError> {
        let capture_id = self.next_capture_id;
        self.next_capture_id = capture_id.checked_add(1).ok_or_else(|| {
            ParserError::illegal_context(
                "combinational observer capture",
                "capture count exceeds the capture-key range",
                Some(token),
            )
        })?;
        Ok((u64::from(self.capture_namespace) << 32) | u64::from(capture_id))
    }
}

fn static_string_expr(expr: &Expression) -> Option<String> {
    if !expr.comptime().r#type.is_string() {
        return None;
    }
    let value = expr.comptime().get_value().ok()?;
    byte_value_to_string(value)
}

fn register_comb_runtime_event_site<'a>(
    collector: &mut CombEffectCollector,
    kind: RuntimeEventKind,
    args: &'a [SystemFunctionInput],
) -> (
    u32,
    Option<&'a SystemFunctionInput>,
    &'a [SystemFunctionInput],
) {
    let (template, template_arg, value_args) =
        if let Some(template) = args.first().and_then(|arg| static_string_expr(&arg.0)) {
            (Some(template), args.first(), &args[1..])
        } else {
            (None, None, args)
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
            .map(|arg| arg.0.comptime().r#type.total_width().unwrap_or(1))
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
    let id = collector.sites.len() as u32;
    collector.sites.push(site);
    (id, template_arg, value_args)
}

fn collect_system_function_effect(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    call: &SystemFunctionCall,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<BoundaryMap<VarId>, ParserError> {
    match &call.kind {
        SystemFunctionKind::Clog2(input)
        | SystemFunctionKind::Onehot(input)
        | SystemFunctionKind::Signed(input)
        | SystemFunctionKind::Unsigned(input) => {
            collect_expression_effects(module, store, &input.0, arena, collector)?;
            let ((_, sources), boundaries) =
                eval_expression_effectful(module, store, &input.0, arena, None)?;
            collector.sensitivity.extend(sources);
            return Ok(boundaries);
        }
        SystemFunctionKind::Bits(_)
        | SystemFunctionKind::Size(..)
        | SystemFunctionKind::Readmemh(_, _) => return Ok(BoundaryMap::default()),
        SystemFunctionKind::Display(_)
        | SystemFunctionKind::Write(_)
        | SystemFunctionKind::Assert { .. }
        | SystemFunctionKind::Finish => {}
    }

    let (kind, cond, args) = match &call.kind {
        SystemFunctionKind::Display(args) => (RuntimeEventKind::Display, None, args.as_slice()),
        SystemFunctionKind::Write(args) => (RuntimeEventKind::Write, None, args.as_slice()),
        SystemFunctionKind::Finish => (RuntimeEventKind::Finish, None, [].as_slice()),
        SystemFunctionKind::Assert { kind, cond, args } => {
            let event_kind = match kind {
                veryl_analyzer::ir::AssertKind::Fatal => RuntimeEventKind::Report(Severity::Fatal),
                veryl_analyzer::ir::AssertKind::Continue => {
                    RuntimeEventKind::Report(Severity::Error)
                }
            };
            (event_kind, Some(&cond.0), args.as_slice())
        }
        _ => unreachable!("non-event system functions return before event collection"),
    };
    let (site_id, template_arg, value_args) =
        register_comb_runtime_event_site(collector, kind, args);
    // Event operands are evaluated left to right.  Keep the environment from
    // the start of the event so writes performed by later operands cannot
    // retroactively rebind inputs in an earlier operand or in the assertion
    // condition.  Reads after an operand write are already substituted by
    // `eval_expression_effectful` as that operand is lowered.
    let observer_store = store.fork();
    let mut boundaries = BoundaryMap::default();
    let mut observer_args = Vec::new();
    let mut observed_inputs = HashSet::default();
    let mut position_inputs = HashSet::default();
    let explicit_guard = if let Some(cond) = cond {
        collect_expression_effects(module, store, cond, arena, collector)?;
        let ((cond_node, cond_sources), cond_boundaries) =
            eval_expression_effectful(module, store, cond, arena, None)?;
        boundaries = merge_boundaries(boundaries, cond_boundaries);
        observed_inputs.extend(cond_sources.iter().copied());
        collector.sensitivity.extend(cond_sources);
        collect_expression_position_inputs(module, cond, &mut position_inputs)?;
        Some(procedural_condition(arena, cond_node)?)
    } else {
        None
    };
    // A statically known format is omitted from the emitted runtime-event
    // operands, but its expression still executes before the value operands.
    // Preserve nested effects and output writeback from that evaluation.
    if let Some(template_arg) = template_arg {
        collect_expression_effects(module, store, &template_arg.0, arena, collector)?;
        let ((_, sources), template_boundaries) =
            eval_expression_effectful(module, store, &template_arg.0, arena, None)?;
        boundaries = merge_boundaries(boundaries, template_boundaries);
        collector.sensitivity.extend(sources);
    }
    for arg in value_args {
        collect_expression_effects(module, store, &arg.0, arena, collector)?;
        let ((node, sources), arg_boundaries) =
            eval_expression_effectful(module, store, &arg.0, arena, None)?;
        boundaries = merge_boundaries(boundaries, arg_boundaries);
        observed_inputs.extend(sources.iter().copied());
        collector.sensitivity.extend(sources);
        collect_expression_position_inputs(module, &arg.0, &mut position_inputs)?;
        observer_args.push(arena.alloc(SLTNode::Capture {
            expr: node,
            key: collector.next_capture_key(&call.comptime.token)?,
        })?);
    }
    observed_inputs.extend(collector.active_guard_sources.iter().copied());
    let guard = match (kind, collector.active_guard, explicit_guard) {
        (
            RuntimeEventKind::Display
            | RuntimeEventKind::Write
            | RuntimeEventKind::Finish
            | RuntimeEventKind::Stop,
            active,
            None,
        ) => active,
        (RuntimeEventKind::Report(_), None, explicit) => explicit,
        (RuntimeEventKind::Report(_), Some(active), Some(explicit)) => {
            let inactive = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, active))?;
            Some(arena.alloc(SLTNode::Binary(inactive, BinaryOp::LogicOr, explicit))?)
        }
        (RuntimeEventKind::Report(_), Some(active), None) => Some(active),
        (
            RuntimeEventKind::Display
            | RuntimeEventKind::Write
            | RuntimeEventKind::Finish
            | RuntimeEventKind::Stop,
            _,
            Some(_),
        ) => {
            unreachable!("display/write/finish/stop has no explicit guard")
        }
    }
    .map(|node| -> Result<NodeId, ParserError> {
        Ok(arena.alloc(SLTNode::Capture {
            expr: node,
            key: collector.next_capture_key(&call.comptime.token)?,
        })?)
    })
    .transpose()?;
    let loop_effect = collector
        .loop_effects
        .as_ref()
        .map(|_| SLTForEffect::Event {
            site_id,
            guard,
            emit_on_true: matches!(
                kind,
                RuntimeEventKind::Display
                    | RuntimeEventKind::Write
                    | RuntimeEventKind::Finish
                    | RuntimeEventKind::Stop
            ),
            args: observer_args.clone(),
            fatal_error_code: kind.is_fatal().then_some(1_000_000 + site_id as i64),
        });
    let observed_ids: HashSet<_> = observed_inputs.iter().map(|atom| atom.id).collect();
    let position_ids: HashSet<_> = position_inputs.iter().map(|atom| atom.id).collect();
    let mut preceding_writes = Vec::new();
    let mut written_before = Vec::new();
    for (id, range_store) in store.iter() {
        for (&lsb, (value, width, _)) in &range_store.ranges {
            if value.is_none() {
                continue;
            }
            let atom = VarAtomBase::new(*id, lsb, lsb + width - 1);
            preceding_writes.push(atom);
            if position_ids.contains(id) {
                written_before.push(atom);
            }
        }
    }
    let mut local_inputs = Vec::new();
    for id in &observed_ids {
        let Some(range_store) = observer_store.get(id) else {
            continue;
        };
        if guard.is_none() {
            let overlaps_observed_input =
                range_store.ranges.iter().any(|(&lsb, (value, width, _))| {
                    let Some(msb) = width.checked_sub(1).and_then(|span| lsb.checked_add(span))
                    else {
                        return false;
                    };
                    value.is_some()
                        && observed_inputs
                            .iter()
                            .chain(position_inputs.iter())
                            .any(|atom| {
                                atom.id == *id && atom.access.overlaps(&BitAccess::new(lsb, msb))
                            })
                });
            if !overlaps_observed_input {
                continue;
            }
        }
        let Some(variable) = module.variables.get(id) else {
            return Err(ParserError::illegal_context(
                "combinational observer capture",
                "observed variable is absent from the semantic module",
                Some(&call.comptime.token),
            ));
        };
        let width = resolve_total_width(module, variable)?;
        if width == 0 {
            continue;
        }
        let parts = range_store
            .get_parts_ref(BitAccess::new(0, width - 1))
            .map_err(|error| {
                super::range_store_error(
                    "combinational observer capture",
                    error,
                    Some(&call.comptime.token),
                )
            })?;
        if !parts.iter().any(|(value, _)| value.is_some()) {
            continue;
        }
        let (node, _) = combine_parts_with_default(*id, 0, parts, arena)?;
        local_inputs.push((*id, node));
    }
    collector.observers.push(CombObserver {
        site_id,
        activation_group: 0,
        guard,
        args: observer_args,
        loop_runner: None,
        sensitivity: Vec::new(),
        sensitivity_counters: Vec::new(),
        local_inputs,
        observed_inputs: observed_inputs.into_iter().collect(),
        position_inputs: position_inputs.into_iter().collect(),
        preceding_writes,
        written_before,
        written_input_atoms: Vec::new(),
        written_inputs: Vec::new(),
        captured_in_loop: loop_effect.is_some(),
    });
    if let (Some(loop_effects), Some(loop_effect)) = (&mut collector.loop_effects, loop_effect) {
        loop_effects.push(loop_effect);
    }
    Ok(boundaries)
}

fn with_collector_guard<T, F>(
    collector: &mut CombEffectCollector,
    arena: &mut SLTNodeArena<VarId>,
    guard: NodeId,
    guard_sources: HashSet<VarAtomBase<VarId>>,
    f: F,
) -> Result<T, ParserError>
where
    F: FnOnce(&mut CombEffectCollector, &mut SLTNodeArena<VarId>) -> Result<T, ParserError>,
{
    let saved_guard = collector.active_guard;
    let saved_guard_sources = collector.active_guard_sources.clone();
    let active_guard = if let Some(active) = saved_guard {
        arena.alloc(SLTNode::Binary(active, BinaryOp::LogicAnd, guard))?
    } else {
        guard
    };
    let mut active_guard_sources = saved_guard_sources.clone();
    active_guard_sources.extend(guard_sources);
    collector.active_guard = Some(active_guard);
    collector.active_guard_sources = active_guard_sources;
    let result = f(collector, arena);
    collector.active_guard = saved_guard;
    collector.active_guard_sources = saved_guard_sources;
    result
}

fn collect_function_call_effects(
    module: &Module,
    store: &SymbolicStore<VarId>,
    call: &veryl_analyzer::ir::FunctionCall,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<(), ParserError> {
    let Some(function) = module.functions.get(&call.id) else {
        return Err(ParserError::internal(
            "function call",
            format!("unknown function id: {:?}", call.id),
            Some(&call.comptime.token),
        ));
    };

    let Some(function_body) = (if let Some(index) = &call.index {
        function.get_function(index)
    } else {
        function.get_function(&[])
    }) else {
        return Err(ParserError::internal(
            "function call specialization",
            format!("{call}"),
            Some(&call.comptime.token),
        ));
    };

    let mut actual_store = store.fork();
    let mut formal_bindings = Vec::new();
    let ordered_inputs = super::ordered_function_inputs(function, &function_body, call)?;
    let mut later_input_writes = vec![HashMap::default(); ordered_inputs.len()];
    let mut accumulated_writes = HashMap::default();
    for (index, (_, expression)) in ordered_inputs.iter().enumerate().rev() {
        later_input_writes[index] = accumulated_writes.clone();
        super::collect_written_expression(module, expression, &mut accumulated_writes)?;
    }
    for (input_index, (arg_id, arg_expr)) in ordered_inputs.into_iter().enumerate() {
        collect_expression_effects(module, &actual_store, arg_expr, arena, collector)?;
        let mut actual_inputs = HashSet::default();
        collect_expression_position_inputs(module, arg_expr, &mut actual_inputs)?;
        collector.sensitivity.extend(actual_inputs);

        let formal = &module.variables[&arg_id];
        let arg_width = resolve_total_width(module, formal)?;
        let ((arg_node, arg_sources), _) = expr::eval_function_input_assignment_effectful(
            module,
            &mut actual_store,
            arg_expr,
            &formal.r#type,
            arg_width,
            arena,
        )?;
        let arg_node = if formal.r#type.is_2state() && !arg_expr.comptime().r#type.is_2state() {
            arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, arg_node))?
        } else {
            arg_node
        };
        let needs_snapshot = arg_sources.iter().any(|source| {
            later_input_writes[input_index]
                .get(&source.id)
                .is_some_and(|writes| writes.iter().any(|write| write.overlaps(&source.access)))
        });
        let arg_node = if needs_snapshot {
            arena.alloc(SLTNode::Capture {
                expr: arg_node,
                key: collector.next_capture_key(&call.comptime.token)?,
            })?
        } else {
            arg_node
        };
        // A runtime effect inside the callee executes as part of the caller's
        // always_comb process.  Its formal is only a local symbolic binding;
        // sensitivity must therefore retain the actual argument's sources.
        collector.sensitivity.extend(arg_sources.iter().copied());
        formal_bindings.push((
            arg_id,
            RangeStore::new(Some((arg_node, arg_sources)), arg_width),
        ));
    }

    // Actuals are evaluated in caller order and may write module-scoped
    // variables through nested output arguments.  Callee runtime effects must
    // therefore start from that advanced caller state, just like value
    // evaluation does, before formal bindings shadow their module entries.
    let mut local_store = actual_store.fork();
    local_store.extend(formal_bindings);

    let final_local_store = if let Some(ret_id) = function_body.ret {
        collect_function_body_effects(
            module,
            local_store,
            &function_body,
            ret_id,
            arena,
            collector,
        )?
    } else {
        collect_comb_effects_statements(
            module,
            local_store,
            &function_body.statements,
            arena,
            collector,
        )?
    };

    // Output destinations are resolved after the callee body. Collect their
    // nested effects against a preview store, then apply each output once to
    // the real collector-local caller store so later outputs observe earlier
    // writebacks without evaluating a destination effect twice.
    for (arg_id, destinations) in super::ordered_function_outputs(function, &function_body, call)? {
        let mut destination_store = actual_store.fork();
        let destination_width = checked_destination_width(
            module,
            destinations,
            "function output destination",
            destinations.first().map(|destination| &destination.token),
        )?;
        let (output_expr, output_sources, output_is_2state) =
            super::function_output_value(module, arg_id, call, &final_local_store, arena)?;
        let output_signed = module.variables[&arg_id].r#type.signed;
        let output_expr =
            coerce_node_width(arena, output_expr, Some(destination_width), output_signed)?;
        let mut current_offset = 0;
        for destination in destinations.iter().rev() {
            let mut collection_store = destination_store.fork();
            for expression in destination
                .index
                .0
                .iter()
                .chain(destination.select.0.iter())
            {
                collect_and_advance_expression(
                    module,
                    &mut collection_store,
                    expression,
                    arena,
                    collector,
                )?;
            }

            let part_width = crate::bitaccess::get_access_width(
                module,
                destination.id,
                &destination.index,
                &destination.select,
            )?;
            let (slice_access, next_offset) = checked_assignment_slice(
                current_offset,
                part_width,
                destination_width,
                destination,
            )?;
            let destination_expr = if destinations.len() == 1 {
                output_expr
            } else {
                arena.alloc(SLTNode::Slice {
                    expr: output_expr,
                    access: slice_access,
                })?
            };
            (destination_store, _) = super::apply_assignment_destination(
                module,
                destination_store,
                BoundaryMap::default(),
                destination,
                destination_expr,
                output_sources.clone(),
                output_is_2state,
                arena,
            )?;
            current_offset = next_offset;
        }
        (actual_store, _) = super::apply_function_output(
            module,
            actual_store,
            BoundaryMap::default(),
            arg_id,
            destinations,
            call,
            &final_local_store,
            arena,
        )?;
    }
    Ok(())
}

pub(super) fn statements_contain_runtime_effect(module: &Module, statements: &[Statement]) -> bool {
    statements
        .iter()
        .any(|stmt| statement_contains_runtime_effect(module, stmt))
}

fn statement_contains_runtime_effect(module: &Module, stmt: &Statement) -> bool {
    match stmt {
        Statement::Assign(assign) => {
            expression_contains_runtime_effect(module, &assign.expr)
                || assign
                    .dst
                    .iter()
                    .any(|destination| destination_contains_runtime_effect(module, destination))
        }
        Statement::SystemFunctionCall(call) => match &call.kind {
            SystemFunctionKind::Display(_)
            | SystemFunctionKind::Write(_)
            | SystemFunctionKind::Assert { .. }
            | SystemFunctionKind::Finish => true,
            SystemFunctionKind::Clog2(input)
            | SystemFunctionKind::Onehot(input)
            | SystemFunctionKind::Signed(input)
            | SystemFunctionKind::Unsigned(input) => {
                expression_contains_runtime_effect(module, &input.0)
            }
            SystemFunctionKind::Bits(_)
            | SystemFunctionKind::Size(..)
            | SystemFunctionKind::Readmemh(_, _) => false,
        },
        Statement::If(if_stmt) => {
            expression_contains_runtime_effect(module, &if_stmt.cond)
                || statements_contain_runtime_effect(module, &if_stmt.true_side)
                || statements_contain_runtime_effect(module, &if_stmt.false_side)
        }
        Statement::Case(case_stmt) => {
            expression_contains_runtime_effect(module, &case_stmt.case_target)
                || case_stmt.arms.iter().any(|arm| {
                    arm.patterns
                        .iter()
                        .any(|pattern| case_pattern_contains_runtime_effect(module, pattern))
                })
                || case_stmt
                    .arms
                    .iter()
                    .any(|arm| statements_contain_runtime_effect(module, &arm.body))
                || statements_contain_runtime_effect(module, &case_stmt.default)
        }
        Statement::For(for_stmt) => {
            for_range_contains_runtime_effect(module, &for_stmt.range)
                || statements_contain_runtime_effect(module, &for_stmt.body)
        }
        Statement::FunctionCall(call) => function_call_contains_runtime_effect(module, call),
        Statement::IfReset(_)
        | Statement::TbMethodCall(_)
        | Statement::Break
        | Statement::Unsupported(_)
        | Statement::Null => false,
    }
}

pub(super) fn destination_contains_runtime_effect(
    module: &Module,
    destination: &veryl_analyzer::ir::AssignDestination,
) -> bool {
    destination
        .index
        .0
        .iter()
        .chain(destination.select.0.iter())
        .any(|expression| expression_contains_runtime_effect(module, expression))
}

fn function_call_contains_runtime_effect(
    module: &Module,
    call: &veryl_analyzer::ir::FunctionCall,
) -> bool {
    call.inputs
        .values()
        .any(|expression| expression_contains_runtime_effect(module, expression))
        || call
            .outputs
            .values()
            .flatten()
            .any(|destination| destination_contains_runtime_effect(module, destination))
        || module
            .functions
            .get(&call.id)
            .and_then(|function| {
                if let Some(index) = &call.index {
                    function.get_function(index)
                } else {
                    function.get_function(&[])
                }
            })
            .is_some_and(|body| statements_contain_runtime_effect(module, &body.statements))
}

fn case_pattern_contains_runtime_effect(module: &Module, pattern: &CasePattern) -> bool {
    match pattern {
        CasePattern::Eq(expression) => expression_contains_runtime_effect(module, expression),
        CasePattern::Range { lo, hi, .. } => {
            expression_contains_runtime_effect(module, lo)
                || expression_contains_runtime_effect(module, hi)
        }
    }
}

fn for_range_bounds(range: &ForRange) -> (&ForBound, &ForBound) {
    match range {
        ForRange::Forward { start, end, .. }
        | ForRange::Reverse { start, end, .. }
        | ForRange::Stepped { start, end, .. } => (start, end),
    }
}

fn for_range_contains_runtime_effect(module: &Module, range: &ForRange) -> bool {
    let (start, end) = for_range_bounds(range);
    [start, end].into_iter().any(|bound| match bound {
        ForBound::Const(..) => false,
        ForBound::Expression(expression) => expression_contains_runtime_effect(module, expression),
    })
}

pub(crate) fn expression_contains_runtime_effect(module: &Module, expression: &Expression) -> bool {
    match expression {
        Expression::Term(factor) => match &**factor {
            Factor::FunctionCall(call) => function_call_contains_runtime_effect(module, call),
            Factor::Variable(_, index, select, _) => index
                .0
                .iter()
                .chain(select.0.iter())
                .any(|expression| expression_contains_runtime_effect(module, expression)),
            Factor::HierVariable(reference) => reference
                .index
                .0
                .iter()
                .chain(reference.select.0.iter())
                .any(|expression| expression_contains_runtime_effect(module, expression)),
            Factor::SystemFunctionCall(call) => match &call.kind {
                SystemFunctionKind::Clog2(input)
                | SystemFunctionKind::Onehot(input)
                | SystemFunctionKind::Signed(input)
                | SystemFunctionKind::Unsigned(input) => {
                    expression_contains_runtime_effect(module, &input.0)
                }
                SystemFunctionKind::Bits(_)
                | SystemFunctionKind::Size(..)
                | SystemFunctionKind::Display(_)
                | SystemFunctionKind::Write(_)
                | SystemFunctionKind::Assert { .. }
                | SystemFunctionKind::Readmemh(_, _)
                | SystemFunctionKind::Finish => false,
            },
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => false,
        },
        Expression::Unary(_, inner, _) => expression_contains_runtime_effect(module, inner),
        Expression::Binary(lhs, _, rhs, _) => {
            expression_contains_runtime_effect(module, lhs)
                || expression_contains_runtime_effect(module, rhs)
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            expression_contains_runtime_effect(module, cond)
                || expression_contains_runtime_effect(module, then_expr)
                || expression_contains_runtime_effect(module, else_expr)
        }
        Expression::Concatenation(items, _) => items
            .iter()
            .any(|(expression, _)| expression_contains_runtime_effect(module, expression)),
        Expression::ArrayLiteral(items, _) => items.iter().any(|item| match item {
            ArrayLiteralItem::Value(expression, _) => {
                expression_contains_runtime_effect(module, expression)
            }
            ArrayLiteralItem::Defaul(expression) => {
                expression_contains_runtime_effect(module, expression)
            }
        }),
        Expression::StructConstructor(_, fields, _) => fields
            .iter()
            .any(|(_, expression)| expression_contains_runtime_effect(module, expression)),
    }
}

pub(crate) fn collect_expression_effects(
    module: &Module,
    store: &SymbolicStore<VarId>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<(), ParserError> {
    match expr {
        Expression::Term(factor) => collect_factor_effects(module, store, factor, arena, collector),
        Expression::Unary(_, inner, _) => {
            collect_expression_effects(module, store, inner, arena, collector)
        }
        Expression::Binary(lhs, op, rhs, _) if matches!(op, Op::LogicAnd | Op::LogicOr) => {
            collect_expression_effects(module, store, lhs, arena, collector)?;
            let mut rhs_store = store.fork();
            let ((lhs_node, lhs_sources), _) =
                eval_expression_effectful(module, &mut rhs_store, lhs, arena, None)?;
            let execute_rhs =
                expr::short_circuit_rhs_guard(arena, lhs_node, matches!(op, Op::LogicAnd))?;
            with_collector_guard(
                collector,
                arena,
                execute_rhs,
                lhs_sources,
                |collector, arena| {
                    collect_expression_effects(module, &rhs_store, rhs, arena, collector)
                },
            )
        }
        Expression::Binary(lhs, _, rhs, _) => {
            collect_expression_effects(module, store, lhs, arena, collector)?;
            let mut rhs_store = store.fork();
            let _ = eval_expression_effectful(module, &mut rhs_store, lhs, arena, None)?;
            collect_expression_effects(module, &rhs_store, rhs, arena, collector)
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            collect_expression_effects(module, store, cond, arena, collector)?;
            let mut branch_store = store.fork();
            let ((cond_node, cond_sources), _) =
                eval_expression_effectful(module, &mut branch_store, cond, arena, None)?;

            // Match effectful ternary evaluation: a known condition executes
            // one arm, while X/Z executes both arms from left to right.
            let not_cond = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, cond_node))?;
            let known_false = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, not_cond))?;
            let true_truth = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, not_cond))?;
            let known_true = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, true_truth))?;
            let execute_then = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, known_false))?;

            with_collector_guard(
                collector,
                arena,
                execute_then,
                cond_sources.clone(),
                |collector, arena| {
                    collect_expression_effects(module, &branch_store, then_expr, arena, collector)
                },
            )?;
            let mut then_store = branch_store.fork();
            let _ = eval_expression_effectful(module, &mut then_store, then_expr, arena, None)?;
            let after_then = merge_symbolic_versions(
                module,
                &then_store,
                &branch_store,
                execute_then,
                &cond_sources,
                arena,
            )?;

            let execute_else = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, known_true))?;
            with_collector_guard(
                collector,
                arena,
                execute_else,
                cond_sources,
                |collector, arena| {
                    collect_expression_effects(module, &after_then, else_expr, arena, collector)
                },
            )
        }
        Expression::Concatenation(items, _) => {
            let mut item_store = store.fork();
            for (item_expr, _) in items {
                collect_and_advance_expression(
                    module,
                    &mut item_store,
                    item_expr,
                    arena,
                    collector,
                )?;
            }
            Ok(())
        }
        Expression::ArrayLiteral(items, _) => {
            let mut item_store = store.fork();
            for item in items {
                match item {
                    ArrayLiteralItem::Value(item_expr, _) => {
                        collect_and_advance_expression(
                            module,
                            &mut item_store,
                            item_expr,
                            arena,
                            collector,
                        )?;
                    }
                    ArrayLiteralItem::Defaul(default_expr) => {
                        collect_and_advance_expression(
                            module,
                            &mut item_store,
                            default_expr,
                            arena,
                            collector,
                        )?;
                    }
                }
            }
            Ok(())
        }
        Expression::StructConstructor(_, fields, _) => {
            let mut field_store = store.fork();
            for (_, field_expr) in fields {
                collect_and_advance_expression(
                    module,
                    &mut field_store,
                    field_expr,
                    arena,
                    collector,
                )?;
            }
            Ok(())
        }
    }
}

pub(crate) fn collect_and_advance_expression(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    expression: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<HashSet<VarAtomBase<VarId>>, ParserError> {
    collect_expression_effects(module, store, expression, arena, collector)?;
    let ((_, sources), _) = eval_expression_effectful(module, store, expression, arena, None)?;
    Ok(sources)
}

fn collect_assignment_effects(
    module: &Module,
    store: &SymbolicStore<VarId>,
    assign: &AssignStatement,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<(), ParserError> {
    collect_expression_effects(module, store, &assign.expr, arena, collector)?;

    // eval_assign evaluates the RHS before resolving dynamic destinations.
    // Mirror that order in a collector-local store so observers in an index
    // see RHS output writes and later destination expressions see earlier ones.
    let mut destination_store = store.fork();
    let ((rhs_expr, rhs_sources), _) =
        eval_assignment_rhs_effectful(module, &mut destination_store, assign, arena)?;
    let rhs_expected_width = checked_destination_width(
        module,
        &assign.dst,
        "assignment destination",
        Some(&assign.expr.token_range()),
    )?;
    let rhs_is_2state = assign.expr.comptime().r#type.is_2state();
    let mut current_offset = 0;
    for destination in assign.dst.iter().rev() {
        let mut collection_store = destination_store.fork();
        for expression in destination
            .index
            .0
            .iter()
            .chain(destination.select.0.iter())
        {
            collect_and_advance_expression(
                module,
                &mut collection_store,
                expression,
                arena,
                collector,
            )?;
        }

        let part_width = crate::bitaccess::get_access_width(
            module,
            destination.id,
            &destination.index,
            &destination.select,
        )?;
        let (slice_access, next_offset) =
            checked_assignment_slice(current_offset, part_width, rhs_expected_width, destination)?;
        let destination_expr = if assign.dst.len() == 1 {
            rhs_expr
        } else {
            arena.alloc(SLTNode::Slice {
                expr: rhs_expr,
                access: slice_access,
            })?
        };
        (destination_store, _) = super::apply_assignment_destination(
            module,
            destination_store,
            BoundaryMap::default(),
            destination,
            destination_expr,
            rhs_sources.clone(),
            rhs_is_2state,
            arena,
        )?;
        current_offset = next_offset;
    }
    Ok(())
}

fn collect_case_pattern_effects(
    module: &Module,
    store: &SymbolicStore<VarId>,
    target_expr: &Expression,
    target: &expr::EvaluatedCaseTarget,
    patterns: &[CasePattern],
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<(), ParserError> {
    fn collect_pattern(
        module: &Module,
        store: &SymbolicStore<VarId>,
        target_expr: &Expression,
        target: &expr::EvaluatedCaseTarget,
        pattern: &CasePattern,
        arena: &mut SLTNodeArena<VarId>,
        collector: &mut CombEffectCollector,
    ) -> Result<(), ParserError> {
        match pattern {
            CasePattern::Eq(expression) => {
                collect_expression_effects(module, store, expression, arena, collector)
            }
            CasePattern::Range { lo, hi, .. } => {
                collect_expression_effects(module, store, lo, arena, collector)?;
                let mut hi_store = store.fork();
                let ((lo_matches, lo_sources), _) = {
                    let mut expression_store = expr::ExpressionStore::Effectful(&mut hi_store);
                    expr::eval_case_comparison(
                        module,
                        &mut expression_store,
                        target_expr,
                        target,
                        lo,
                        false,
                        Op::LessEq,
                        arena,
                    )?
                };
                let execute_hi = expr::short_circuit_rhs_guard(arena, lo_matches, true)?;
                with_collector_guard(
                    collector,
                    arena,
                    execute_hi,
                    lo_sources,
                    |collector, arena| {
                        collect_expression_effects(module, &hi_store, hi, arena, collector)
                    },
                )
            }
        }
    }

    let mut pattern_store = store.fork();
    let mut matched: Option<(NodeId, HashSet<VarAtomBase<VarId>>)> = None;
    for pattern in patterns {
        let execute_pattern = matched
            .as_ref()
            .map(|(condition, _)| expr::short_circuit_rhs_guard(arena, *condition, false))
            .transpose()?;
        if let Some(execute_pattern) = execute_pattern {
            let guard_sources = matched
                .as_ref()
                .map(|(_, sources)| sources.clone())
                .unwrap_or_default();
            with_collector_guard(
                collector,
                arena,
                execute_pattern,
                guard_sources,
                |collector, arena| {
                    collect_pattern(
                        module,
                        &pattern_store,
                        target_expr,
                        target,
                        pattern,
                        arena,
                        collector,
                    )
                },
            )?;
        } else {
            collect_pattern(
                module,
                &pattern_store,
                target_expr,
                target,
                pattern,
                arena,
                collector,
            )?;
        }

        let base_store = pattern_store.fork();
        let mut evaluated_store = base_store.fork();
        let ((pattern_condition, pattern_sources), _) = eval_case_arm_condition_effectful(
            module,
            &mut evaluated_store,
            target_expr,
            target,
            std::slice::from_ref(pattern),
            arena,
        )?;
        pattern_store = if let Some(execute_pattern) = execute_pattern {
            let condition_sources = matched
                .as_ref()
                .map(|(_, sources)| sources)
                .cloned()
                .unwrap_or_default();
            merge_symbolic_versions(
                module,
                &evaluated_store,
                &base_store,
                execute_pattern,
                &condition_sources,
                arena,
            )?
        } else {
            evaluated_store
        };
        matched = Some(if let Some((condition, mut sources)) = matched {
            sources.extend(pattern_sources);
            (
                arena.alloc(SLTNode::Binary(
                    condition,
                    BinaryOp::LogicOr,
                    pattern_condition,
                ))?,
                sources,
            )
        } else {
            (pattern_condition, pattern_sources)
        });
    }
    Ok(())
}

fn collect_factor_effects(
    module: &Module,
    store: &SymbolicStore<VarId>,
    factor: &Factor,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<(), ParserError> {
    match factor {
        Factor::Variable(_, index, select, _) => {
            let mut position_store = store.fork();
            for expr in index.0.iter().chain(select.0.iter()) {
                collect_and_advance_expression(
                    module,
                    &mut position_store,
                    expr,
                    arena,
                    collector,
                )?;
            }
            Ok(())
        }
        Factor::HierVariable(reference) => Err(ParserError::illegal_context(
            "hierarchical variable reference",
            format!(
                "`{}` is only valid in a native testbench block",
                reference.var_path
            ),
            Some(&reference.comptime.token),
        )),
        Factor::FunctionCall(call) => {
            collect_function_call_effects(module, store, call, arena, collector)
        }
        Factor::SystemFunctionCall(call) => match &call.kind {
            SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => Ok(()),
            SystemFunctionKind::Clog2(input)
            | SystemFunctionKind::Onehot(input)
            | SystemFunctionKind::Signed(input)
            | SystemFunctionKind::Unsigned(input) => {
                collect_expression_effects(module, store, &input.0, arena, collector)
            }
            _ => Ok(()),
        },
        Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => Ok(()),
    }
}

fn collect_expression_position_inputs(
    module: &Module,
    expr: &Expression,
    out: &mut HashSet<VarAtomBase<VarId>>,
) -> Result<(), ParserError> {
    match expr {
        Expression::Term(factor) => collect_factor_position_inputs(module, factor, out),
        Expression::Binary(lhs, _, rhs, _) => {
            collect_expression_position_inputs(module, lhs, out)?;
            collect_expression_position_inputs(module, rhs, out)
        }
        Expression::Unary(_, inner, _) => collect_expression_position_inputs(module, inner, out),
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            collect_expression_position_inputs(module, cond, out)?;
            collect_expression_position_inputs(module, then_expr, out)?;
            collect_expression_position_inputs(module, else_expr, out)
        }
        Expression::Concatenation(parts, _) => {
            for (part, _) in parts {
                collect_expression_position_inputs(module, part, out)?;
            }
            Ok(())
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expr, _) => {
                        collect_expression_position_inputs(module, expr, out)?;
                    }
                    ArrayLiteralItem::Defaul(expr) => {
                        collect_expression_position_inputs(module, expr, out)?;
                    }
                }
            }
            Ok(())
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, field_expr) in fields {
                collect_expression_position_inputs(module, field_expr, out)?;
            }
            Ok(())
        }
    }
}

fn collect_factor_position_inputs(
    module: &Module,
    factor: &Factor,
    out: &mut HashSet<VarAtomBase<VarId>>,
) -> Result<(), ParserError> {
    match factor {
        Factor::Variable(var_id, index, select, comptime) => {
            if !comptime.is_const {
                let access = eval_var_select(module, *var_id, index, select)?;
                out.insert(VarAtomBase::new(*var_id, access.lsb, access.msb));
            }
            for expr in index.0.iter().chain(select.0.iter()) {
                collect_expression_position_inputs(module, expr, out)?;
            }
            Ok(())
        }
        Factor::HierVariable(reference) => Err(ParserError::illegal_context(
            "hierarchical variable reference",
            format!(
                "`{}` is only valid in a native testbench block",
                reference.var_path
            ),
            Some(&reference.comptime.token),
        )),
        Factor::Value(_) => Ok(()),
        Factor::FunctionCall(call) => {
            for arg in call.inputs.values() {
                collect_expression_position_inputs(module, arg, out)?;
            }
            Ok(())
        }
        Factor::SystemFunctionCall(_) | Factor::Anonymous(_) | Factor::Unknown(_) => Ok(()),
    }
}
