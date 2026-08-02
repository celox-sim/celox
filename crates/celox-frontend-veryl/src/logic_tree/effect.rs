use super::*;

pub(super) fn subtract_written_sensitivity<A: Copy + Eq + std::hash::Hash>(
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
pub(super) struct CombEffectCollector {
    pub(super) observers: Vec<CombObserver<VarId>>,
    pub(super) sites: Vec<RuntimeEventSite>,
    pub(super) sensitivity: HashSet<VarAtomBase<VarId>>,
    active_guard: Option<NodeId>,
    active_guard_sources: HashSet<VarAtomBase<VarId>>,
    loop_effects: Option<Vec<SLTForEffect>>,
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
) -> (u32, &'a [SystemFunctionInput]) {
    let (template, value_args) = if args
        .first()
        .and_then(|arg| static_string_expr(&arg.0))
        .is_some()
    {
        (
            args.first().and_then(|arg| static_string_expr(&arg.0)),
            &args[1..],
        )
    } else {
        (None, args)
    };
    let site = RuntimeEventSite {
        kind,
        template,
        arg_widths: value_args
            .iter()
            .map(|arg| arg.0.comptime().r#type.total_width().unwrap_or(1))
            .collect(),
        arg_signed: value_args
            .iter()
            .map(|arg| arg.0.comptime().expr_context.signed)
            .collect(),
        arg_is_string: value_args
            .iter()
            .map(|arg| arg.0.comptime().r#type.is_string())
            .collect(),
    };
    let id = collector.sites.len() as u32;
    collector.sites.push(site);
    (id, value_args)
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
        | SystemFunctionKind::Size(_)
        | SystemFunctionKind::Readmemh(_, _)
        | SystemFunctionKind::Finish => return Ok(BoundaryMap::default()),
        SystemFunctionKind::Display(_)
        | SystemFunctionKind::Write(_)
        | SystemFunctionKind::Assert { .. } => {}
    }

    let (kind, cond, args) = match &call.kind {
        SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
            (RuntimeEventKind::Display, None, args.as_slice())
        }
        SystemFunctionKind::Assert { kind, cond, args } => {
            let event_kind = match kind {
                veryl_analyzer::ir::AssertKind::Fatal => RuntimeEventKind::AssertFatal,
                veryl_analyzer::ir::AssertKind::Continue => RuntimeEventKind::AssertContinue,
            };
            (event_kind, Some(&cond.0), args.as_slice())
        }
        _ => unreachable!("non-event system functions return before event collection"),
    };
    let (site_id, value_args) = register_comb_runtime_event_site(collector, kind, args);
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
    for arg in value_args {
        collect_expression_effects(module, store, &arg.0, arena, collector)?;
        let ((node, sources), arg_boundaries) =
            eval_expression_effectful(module, store, &arg.0, arena, None)?;
        boundaries = merge_boundaries(boundaries, arg_boundaries);
        observed_inputs.extend(sources.iter().copied());
        collector.sensitivity.extend(sources);
        collect_expression_position_inputs(module, &arg.0, &mut position_inputs)?;
        observer_args.push(node);
    }
    observed_inputs.extend(collector.active_guard_sources.iter().copied());
    let guard = match (kind, collector.active_guard, explicit_guard) {
        (RuntimeEventKind::Display, active, None) => active,
        (RuntimeEventKind::AssertContinue | RuntimeEventKind::AssertFatal, None, explicit) => {
            explicit
        }
        (
            RuntimeEventKind::AssertContinue | RuntimeEventKind::AssertFatal,
            Some(active),
            Some(explicit),
        ) => {
            let inactive = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, active))?;
            Some(arena.alloc(SLTNode::Binary(inactive, BinaryOp::LogicOr, explicit))?)
        }
        (RuntimeEventKind::AssertContinue | RuntimeEventKind::AssertFatal, Some(active), None) => {
            Some(active)
        }
        (RuntimeEventKind::Display, _, Some(_)) => unreachable!("display has no explicit guard"),
    };
    let loop_effect = collector
        .loop_effects
        .as_ref()
        .map(|_| SLTForEffect::Event {
            site_id,
            guard,
            emit_on_true: matches!(kind, RuntimeEventKind::Display),
            args: observer_args.clone(),
            fatal_error_code: matches!(kind, RuntimeEventKind::AssertFatal)
                .then_some(1_000_000 + site_id as i64),
        });
    let observed_ids: HashSet<_> = observed_inputs.iter().map(|atom| atom.id).collect();
    let position_ids: HashSet<_> = position_inputs.iter().map(|atom| atom.id).collect();
    let preceding_writes: Vec<_> = store
        .iter()
        .flat_map(|(id, range_store)| {
            range_store
                .ranges
                .iter()
                .filter_map(move |(&lsb, (value, width, _))| {
                    value
                        .is_some()
                        .then_some(VarAtomBase::new(*id, lsb, lsb + width - 1))
                })
        })
        .collect();
    let mut local_inputs = Vec::new();
    for (id, range_store) in store.iter() {
        if !observed_ids.contains(id) {
            continue;
        }
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
            .get_parts(BitAccess::new(0, width - 1))
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
        local_inputs,
        observed_inputs: observed_inputs.into_iter().collect(),
        position_inputs: position_inputs.into_iter().collect(),
        preceding_writes: preceding_writes.clone(),
        written_before: store
            .iter()
            .filter(|(id, _)| position_ids.contains(id))
            .flat_map(|(id, range_store)| {
                range_store
                    .ranges
                    .iter()
                    .filter_map(move |(&lsb, (value, width, _))| {
                        value
                            .is_some()
                            .then_some(VarAtomBase::new(*id, lsb, lsb + width - 1))
                    })
            })
            .collect(),
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
        return Err(ParserError::unsupported(
            62,
            LoweringPhase::CombLowering,
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
        return Err(ParserError::unsupported(
            62,
            LoweringPhase::CombLowering,
            "function call specialization",
            format!("{call}"),
            Some(&call.comptime.token),
        ));
    };

    let mut actual_store = store.clone();
    let mut local_store = store.clone();
    for (arg_id, arg_expr) in super::ordered_function_inputs(function, &function_body, call)? {
        collect_expression_effects(module, &actual_store, arg_expr, arena, collector)?;
        let mut actual_inputs = HashSet::default();
        collect_expression_position_inputs(module, arg_expr, &mut actual_inputs)?;
        collector.sensitivity.extend(actual_inputs);

        let formal = &module.variables[&arg_id];
        let arg_width = resolve_total_width(module, formal)?;
        let ((arg_node, arg_sources), _) = eval_assignment_expression_effectful(
            module,
            &mut actual_store,
            arg_expr,
            arena,
            arg_width,
        )?;
        // A runtime effect inside the callee executes as part of the caller's
        // always_comb process.  Its formal is only a local symbolic binding;
        // sensitivity must therefore retain the actual argument's sources.
        collector.sensitivity.extend(arg_sources.iter().copied());
        local_store.insert(
            arg_id,
            RangeStore::new(Some((arg_node, arg_sources)), arg_width),
        );
    }

    if !statements_contain_runtime_effect(module, &function_body.statements) {
        return Ok(());
    }

    if let Some(ret_id) = function_body.ret {
        collect_function_body_effects(
            module,
            local_store,
            &function_body.statements,
            ret_id,
            arena,
            collector,
        )?;
    } else {
        let _ = collect_comb_effects_statements(
            module,
            local_store,
            &function_body.statements,
            arena,
            collector,
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
            | SystemFunctionKind::Assert { .. } => true,
            SystemFunctionKind::Clog2(input)
            | SystemFunctionKind::Onehot(input)
            | SystemFunctionKind::Signed(input)
            | SystemFunctionKind::Unsigned(input) => {
                expression_contains_runtime_effect(module, &input.0)
            }
            SystemFunctionKind::Bits(_)
            | SystemFunctionKind::Size(_)
            | SystemFunctionKind::Readmemh(_, _)
            | SystemFunctionKind::Finish => false,
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
        Statement::FunctionCall(call) => module
            .functions
            .get(&call.id)
            .and_then(|function| {
                if let Some(index) = &call.index {
                    function.get_function(index)
                } else {
                    function.get_function(&[])
                }
            })
            .is_some_and(|body| statements_contain_runtime_effect(module, &body.statements)),
        Statement::IfReset(_)
        | Statement::TbMethodCall(_)
        | Statement::Break
        | Statement::Unsupported(_)
        | Statement::Null => false,
    }
}

fn destination_contains_runtime_effect(
    module: &Module,
    destination: &veryl_analyzer::ir::AssignDestination,
) -> bool {
    destination
        .index
        .0
        .iter()
        .chain(destination.select.0.iter())
        .chain(
            destination
                .select
                .1
                .iter()
                .map(|(_, expression)| expression),
        )
        .any(|expression| expression_contains_runtime_effect(module, expression))
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
        ForBound::Const(_) => false,
        ForBound::Expression(expression) => expression_contains_runtime_effect(module, expression),
    })
}

fn expression_contains_runtime_effect(module: &Module, expression: &Expression) -> bool {
    match expression {
        Expression::Term(factor) => match &**factor {
            Factor::FunctionCall(call) => module
                .functions
                .get(&call.id)
                .and_then(|function| {
                    if let Some(index) = &call.index {
                        function.get_function(index)
                    } else {
                        function.get_function(&[])
                    }
                })
                .is_some_and(|body| statements_contain_runtime_effect(module, &body.statements)),
            Factor::Variable(_, index, select, _) => index
                .0
                .iter()
                .chain(select.0.iter())
                .chain(select.1.iter().map(|(_, expression)| expression))
                .any(|expression| expression_contains_runtime_effect(module, expression)),
            Factor::SystemFunctionCall(call) => match &call.kind {
                SystemFunctionKind::Clog2(input)
                | SystemFunctionKind::Onehot(input)
                | SystemFunctionKind::Signed(input)
                | SystemFunctionKind::Unsigned(input) => {
                    expression_contains_runtime_effect(module, &input.0)
                }
                SystemFunctionKind::Bits(_)
                | SystemFunctionKind::Size(_)
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
        Expression::Concatenation(items, _) => items.iter().any(|(expression, repeat)| {
            expression_contains_runtime_effect(module, expression)
                || repeat
                    .as_ref()
                    .is_some_and(|repeat| expression_contains_runtime_effect(module, repeat))
        }),
        Expression::ArrayLiteral(items, _) => items.iter().any(|item| match item {
            ArrayLiteralItem::Value(expression, repeat) => {
                expression_contains_runtime_effect(module, expression)
                    || repeat
                        .as_ref()
                        .is_some_and(|repeat| expression_contains_runtime_effect(module, repeat))
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

fn collect_expression_effects(
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
            let mut rhs_store = store.clone();
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
            collect_expression_effects(module, store, rhs, arena, collector)
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            collect_expression_effects(module, store, cond, arena, collector)?;
            collect_expression_effects(module, store, then_expr, arena, collector)?;
            collect_expression_effects(module, store, else_expr, arena, collector)
        }
        Expression::Concatenation(items, _) => {
            for (item_expr, repeat_expr) in items {
                collect_expression_effects(module, store, item_expr, arena, collector)?;
                if let Some(repeat_expr) = repeat_expr {
                    collect_expression_effects(module, store, repeat_expr, arena, collector)?;
                }
            }
            Ok(())
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(item_expr, repeat_expr) => {
                        collect_expression_effects(module, store, item_expr, arena, collector)?;
                        if let Some(repeat_expr) = repeat_expr {
                            collect_expression_effects(
                                module,
                                store,
                                repeat_expr,
                                arena,
                                collector,
                            )?;
                        }
                    }
                    ArrayLiteralItem::Defaul(default_expr) => {
                        collect_expression_effects(module, store, default_expr, arena, collector)?;
                    }
                }
            }
            Ok(())
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, field_expr) in fields {
                collect_expression_effects(module, store, field_expr, arena, collector)?;
            }
            Ok(())
        }
    }
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
    let mut destination_store = store.clone();
    let _ = eval_assignment_rhs_effectful(module, &mut destination_store, assign, arena)?;
    for destination in assign.dst.iter().rev() {
        for expression in destination
            .index
            .0
            .iter()
            .chain(destination.select.0.iter())
            .chain(
                destination
                    .select
                    .1
                    .iter()
                    .map(|(_, expression)| expression),
            )
        {
            collect_expression_effects(module, &destination_store, expression, arena, collector)?;
            let _ =
                eval_expression_effectful(module, &mut destination_store, expression, arena, None)?;
        }
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
                let mut hi_store = store.clone();
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

    let mut pattern_store = store.clone();
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

        let base_store = pattern_store.clone();
        let mut evaluated_store = base_store.clone();
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
            merge_symbolic_stores(
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
            for expr in index.0.iter().chain(select.0.iter()) {
                collect_expression_effects(module, store, expr, arena, collector)?;
            }
            if let Some((_, expr)) = &select.1 {
                collect_expression_effects(module, store, expr, arena, collector)?;
            }
            Ok(())
        }
        Factor::FunctionCall(call) => {
            collect_function_call_effects(module, store, call, arena, collector)
        }
        Factor::SystemFunctionCall(call) => match &call.kind {
            SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_) => Ok(()),
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

fn collect_function_body_effects(
    module: &Module,
    local_store: SymbolicStore<VarId>,
    statements: &[Statement],
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<(), ParserError> {
    fn collect_statements(
        module: &Module,
        mut state: FunctionControlState,
        statements: &[Statement],
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
        collector: &mut CombEffectCollector,
    ) -> Result<FunctionControlState, ParserError> {
        for stmt in statements {
            if matches!(constant_bool(arena, state.live_expr), Some(false)) {
                break;
            }
            state = collect_statement(module, state, stmt, ret_id, arena, collector)?;
        }
        Ok(state)
    }

    fn collect_case_from_arm(
        module: &Module,
        mut state: FunctionControlState,
        case_stmt: &CaseStatement,
        target: &expr::EvaluatedCaseTarget,
        arm_index: usize,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
        collector: &mut CombEffectCollector,
    ) -> Result<FunctionControlState, ParserError> {
        let Some(arm) = case_stmt.arms.get(arm_index) else {
            return collect_statements(module, state, &case_stmt.default, ret_id, arena, collector);
        };

        let store = state.store.clone();
        let live = state.live_expr;
        let live_sources = state.live_sources.clone();
        with_collector_guard(collector, arena, live, live_sources, |collector, arena| {
            collect_case_pattern_effects(
                module,
                &store,
                &case_stmt.case_target,
                target,
                &arm.patterns,
                arena,
                collector,
            )
        })?;

        let mut next_store = state.store.clone();
        let ((cond_node, cond_sources), cond_bounds) = eval_case_arm_condition_effectful(
            module,
            &mut next_store,
            &case_stmt.case_target,
            target,
            &arm.patterns,
            arena,
        )?;
        state = apply_function_guard(
            module,
            state,
            next_store,
            cond_bounds,
            bool_node(arena, true)?,
            HashSet::default(),
            arena,
        )?;
        let cond_node = procedural_condition(arena, cond_node)?;
        let boundaries = state.boundaries.clone();

        if let Some(cond_val) = constant_bool(arena, cond_node) {
            let state = FunctionControlState {
                boundaries,
                ..state
            };
            return if cond_val {
                collect_statements(module, state, &arm.body, ret_id, arena, collector)
            } else {
                collect_case_from_arm(
                    module,
                    state,
                    case_stmt,
                    target,
                    arm_index + 1,
                    ret_id,
                    arena,
                    collector,
                )
            };
        }

        let true_guard = arena.alloc(SLTNode::Binary(
            state.live_expr,
            BinaryOp::LogicAnd,
            cond_node,
        ))?;
        let mut true_sources = state.live_sources.clone();
        true_sources.extend(cond_sources.iter().copied());
        let then_state = with_collector_guard(
            collector,
            arena,
            true_guard,
            true_sources,
            |collector, arena| {
                collect_statements(
                    module,
                    FunctionControlState {
                        store: state.store.clone(),
                        boundaries: boundaries.clone(),
                        live_expr: state.live_expr,
                        live_sources: state.live_sources.clone(),
                    },
                    &arm.body,
                    ret_id,
                    arena,
                    collector,
                )
            },
        )?;

        let false_cond = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, cond_node))?;
        let false_guard = arena.alloc(SLTNode::Binary(
            state.live_expr,
            BinaryOp::LogicAnd,
            false_cond,
        ))?;
        let mut false_sources = state.live_sources.clone();
        false_sources.extend(cond_sources.iter().copied());
        let else_state = with_collector_guard(
            collector,
            arena,
            false_guard,
            false_sources,
            |collector, arena| {
                collect_case_from_arm(
                    module,
                    FunctionControlState {
                        store: state.store,
                        boundaries,
                        live_expr: state.live_expr,
                        live_sources: state.live_sources,
                    },
                    case_stmt,
                    target,
                    arm_index + 1,
                    ret_id,
                    arena,
                    collector,
                )
            },
        )?;

        let mut live_sources = cond_sources;
        live_sources.extend(then_state.live_sources);
        live_sources.extend(else_state.live_sources);

        Ok(FunctionControlState {
            store: merge_symbolic_stores(
                module,
                &then_state.store,
                &else_state.store,
                cond_node,
                &live_sources,
                arena,
            )?,
            boundaries: merge_boundaries(then_state.boundaries, else_state.boundaries),
            live_expr: merge_control_expr(
                cond_node,
                then_state.live_expr,
                else_state.live_expr,
                arena,
            )?,
            live_sources,
        })
    }

    fn apply_function_guard(
        module: &Module,
        state: FunctionControlState,
        next_store: SymbolicStore<VarId>,
        next_boundaries: BoundaryMap<VarId>,
        next_live_expr: NodeId,
        next_live_sources: HashSet<VarAtomBase<VarId>>,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<FunctionControlState, ParserError> {
        match constant_bool(arena, state.live_expr) {
            Some(true) => Ok(FunctionControlState {
                store: next_store,
                boundaries: merge_boundaries(state.boundaries, next_boundaries),
                live_expr: next_live_expr,
                live_sources: next_live_sources,
            }),
            Some(false) => Ok(state),
            None => {
                let store = merge_symbolic_stores(
                    module,
                    &next_store,
                    &state.store,
                    state.live_expr,
                    &state.live_sources,
                    arena,
                )?;
                let live_expr = match constant_bool(arena, next_live_expr) {
                    Some(true) => state.live_expr,
                    Some(false) => bool_node(arena, false)?,
                    None => arena.alloc(SLTNode::Binary(
                        state.live_expr,
                        BinaryOp::And,
                        next_live_expr,
                    ))?,
                };
                let mut live_sources = state.live_sources;
                live_sources.extend(next_live_sources);
                Ok(FunctionControlState {
                    store,
                    boundaries: merge_boundaries(state.boundaries, next_boundaries),
                    live_expr,
                    live_sources,
                })
            }
        }
    }

    fn eval_guarded_function_expression(
        module: &Module,
        state: FunctionControlState,
        expression: &Expression,
        context_width: Option<usize>,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<(FunctionControlState, NodeId, HashSet<VarAtomBase<VarId>>), ParserError> {
        let mut next_store = state.store.clone();
        let ((node, sources), boundaries) =
            eval_expression_effectful(module, &mut next_store, expression, arena, context_width)?;
        let state = apply_function_guard(
            module,
            state,
            next_store,
            boundaries,
            bool_node(arena, true)?,
            HashSet::default(),
            arena,
        )?;
        Ok((state, node, sources))
    }

    fn statement_contains_return(stmt: &Statement, ret_id: VarId) -> bool {
        match stmt {
            Statement::Assign(assign) => function_assigns_whole_var(assign, ret_id),
            Statement::If(if_stmt) => if_stmt
                .true_side
                .iter()
                .chain(&if_stmt.false_side)
                .any(|stmt| statement_contains_return(stmt, ret_id)),
            Statement::Case(case_stmt) => case_stmt
                .arms
                .iter()
                .flat_map(|arm| &arm.body)
                .chain(&case_stmt.default)
                .any(|stmt| statement_contains_return(stmt, ret_id)),
            Statement::For(for_stmt) => for_stmt
                .body
                .iter()
                .any(|stmt| statement_contains_return(stmt, ret_id)),
            Statement::IfReset(if_reset) => if_reset
                .true_side
                .iter()
                .chain(&if_reset.false_side)
                .any(|stmt| statement_contains_return(stmt, ret_id)),
            Statement::SystemFunctionCall(_)
            | Statement::FunctionCall(_)
            | Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => false,
        }
    }

    fn collect_function_for(
        module: &Module,
        state: FunctionControlState,
        for_stmt: &ForStatement,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
        collector: &mut CombEffectCollector,
    ) -> Result<FunctionControlState, ParserError> {
        let Some(loop_width) = for_stmt.var_type.total_width() else {
            return Err(ParserError::unsupported(
                65,
                LoweringPhase::CombLowering,
                "for loop variable width",
                format!("{:?}", for_stmt.var_name),
                Some(&for_stmt.token),
            ));
        };

        let mut iter_store = state.store.clone();
        let mut written_accesses = HashMap::default();
        collect_written_accesses(module, &for_stmt.body, &mut written_accesses)?;
        for (id, accesses) in written_accesses {
            let width = resolve_total_width(module, &module.variables[&id])?;
            let mut loop_store = RangeStore::new(None, width);
            let mut covered = vec![false; width];
            for access in accesses {
                for slot in covered.iter_mut().take(access.msb + 1).skip(access.lsb) {
                    *slot = true;
                }
            }
            let original = state
                .store
                .get(&id)
                .cloned()
                .unwrap_or_else(|| RangeStore::new(None, width));
            let mut bit = 0;
            while bit < width {
                if covered[bit] {
                    bit += 1;
                    continue;
                }
                let start = bit;
                while bit < width && !covered[bit] {
                    bit += 1;
                }
                let access = BitAccess::new(start, bit - 1);
                let parts = original.get_parts(access).map_err(|error| {
                    super::range_store_error(
                        "function observer for-loop state",
                        error,
                        Some(&for_stmt.token),
                    )
                })?;
                let (expr, sources) = combine_parts_with_default(id, access.lsb, parts, arena)?;
                loop_store
                    .update(access, Some((expr, sources)))
                    .map_err(|error| {
                        super::range_store_error(
                            "function observer for-loop state",
                            error,
                            Some(&for_stmt.token),
                        )
                    })?;
            }
            iter_store.insert(id, loop_store);
        }
        iter_store.insert(for_stmt.var_id, RangeStore::new(None, loop_width));

        let observer_start = collector.observers.len();
        let saved_loop_effects = collector.loop_effects.take();
        collector.loop_effects = Some(Vec::new());
        let iter_state = with_collector_guard(
            collector,
            arena,
            state.live_expr,
            state.live_sources.clone(),
            |collector, arena| {
                collect_statements(
                    module,
                    FunctionControlState {
                        store: iter_store,
                        boundaries: state.boundaries.clone(),
                        live_expr: bool_node(arena, true)?,
                        live_sources: HashSet::default(),
                    },
                    &for_stmt.body,
                    ret_id,
                    arena,
                    collector,
                )
            },
        )?;
        let loop_effects = collector.loop_effects.take().unwrap_or_default();
        collector.loop_effects = saved_loop_effects;

        // Effect arguments after the loop only consume this store while the
        // function remains live. On that path no return was taken, so the
        // ordinary loop fold and the function-aware fold have identical data
        // state; the separate transient result below carries the control state.
        let (result_store, boundaries) = eval_for(
            module,
            state.store.clone(),
            state.boundaries.clone(),
            for_stmt,
            arena,
        )?;
        let template = result_store
            .values()
            .flat_map(|range_store| range_store.ranges.values())
            .filter_map(|(value, _, _)| value.as_ref().map(|(node, _)| *node))
            .find(|node| {
                matches!(
                    arena.get(*node),
                    SLTNode::ForFold { loop_var, .. } if *loop_var == for_stmt.var_id
                )
            })
            .ok_or_else(|| {
                ParserError::illegal_context(
                    "function observer for-loop control",
                    "loop result fold is absent from the symbolic store",
                    Some(&for_stmt.token),
                )
            })?;
        let SLTNode::ForFold {
            loop_var,
            loop_width,
            loop_signed,
            start,
            end,
            inclusive,
            step,
            step_op,
            reverse,
            result,
            initials,
            updates,
            continue_cond,
            ..
        } = arena.get(template).clone()
        else {
            unreachable!();
        };
        let effective_continue = arena.alloc(SLTNode::Binary(
            continue_cond,
            BinaryOp::And,
            iter_state.live_expr,
        ))?;

        if !loop_effects.is_empty() {
            let runner = arena.alloc(SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start: start.clone(),
                end: end.clone(),
                inclusive,
                step,
                step_op,
                reverse,
                result: result.clone(),
                initials: initials.clone(),
                updates: updates.clone(),
                effects: loop_effects,
                continue_cond: effective_continue,
            })?;
            if let Some(parent_effects) = &mut collector.loop_effects {
                parent_effects.push(SLTForEffect::Runner(runner));
            } else {
                attach_loop_runner_to_first_observer(collector, observer_start, Some(runner));
            }
        }

        let initial = bool_node(arena, true)?;
        let live_expr = arena.alloc(SLTNode::ForFold {
            loop_var,
            loop_width,
            loop_signed,
            start,
            end,
            inclusive,
            step,
            step_op,
            reverse,
            result: SLTForFoldResult::Transient {
                initial,
                update: iter_state.live_expr,
            },
            initials,
            updates,
            effects: Vec::new(),
            continue_cond: effective_continue,
        })?;

        let (start_bound, end_bound) = match &for_stmt.range {
            ForRange::Forward { start, end, .. }
            | ForRange::Reverse { start, end, .. }
            | ForRange::Stepped { start, end, .. } => (start, end),
        };
        let (_, start_sources, _) = eval_for_bound(module, &state.store, start_bound, arena)?;
        let (_, end_sources, _) = eval_for_bound(module, &state.store, end_bound, arena)?;
        let mut live_sources = iter_state.live_sources;
        live_sources.extend(start_sources);
        live_sources.extend(end_sources);

        apply_function_guard(
            module,
            state,
            result_store,
            boundaries,
            live_expr,
            live_sources,
            arena,
        )
    }

    fn collect_statement(
        module: &Module,
        mut state: FunctionControlState,
        stmt: &Statement,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
        collector: &mut CombEffectCollector,
    ) -> Result<FunctionControlState, ParserError> {
        if matches!(constant_bool(arena, state.live_expr), Some(false)) {
            return Ok(state);
        }
        match stmt {
            Statement::Assign(assign) => {
                let guard_state = state.clone();
                let store = state.store.clone();
                let live = state.live_expr;
                let live_sources = state.live_sources.clone();
                with_collector_guard(collector, arena, live, live_sources, |collector, arena| {
                    collect_assignment_effects(module, &store, assign, arena, collector)
                })?;
                let (store, boundaries) =
                    eval_assign(module, state.store, state.boundaries, assign, arena)?;
                let live_expr = if function_assigns_whole_var(assign, ret_id) {
                    bool_node(arena, false)?
                } else {
                    bool_node(arena, true)?
                };
                apply_function_guard(
                    module,
                    guard_state,
                    store,
                    boundaries,
                    live_expr,
                    HashSet::default(),
                    arena,
                )
            }
            Statement::SystemFunctionCall(call) => {
                let guard_state = state.clone();
                let mut next_store = state.store.clone();
                let live = state.live_expr;
                let live_sources = state.live_sources.clone();
                let effect_boundaries = with_collector_guard(
                    collector,
                    arena,
                    live,
                    live_sources,
                    |collector, arena| {
                        collect_system_function_effect(
                            module,
                            &mut next_store,
                            call,
                            arena,
                            collector,
                        )
                    },
                )?;
                apply_function_guard(
                    module,
                    guard_state,
                    next_store,
                    effect_boundaries,
                    bool_node(arena, true)?,
                    HashSet::default(),
                    arena,
                )
            }
            Statement::FunctionCall(call) => {
                let guard_state = state.clone();
                let store = state.store.clone();
                let live = state.live_expr;
                let live_sources = state.live_sources.clone();
                with_collector_guard(collector, arena, live, live_sources, |collector, arena| {
                    collect_function_call_effects(module, &store, call, arena, collector)
                })?;
                let (store, boundaries) = eval_statement_form_function_call(
                    module,
                    state.store,
                    state.boundaries,
                    call,
                    arena,
                    LoweringPhase::CombLowering,
                )?;
                apply_function_guard(
                    module,
                    guard_state,
                    store,
                    boundaries,
                    bool_node(arena, true)?,
                    HashSet::default(),
                    arena,
                )
            }
            Statement::If(if_stmt) => {
                let store = state.store.clone();
                let live = state.live_expr;
                let live_sources = state.live_sources.clone();
                with_collector_guard(collector, arena, live, live_sources, |collector, arena| {
                    collect_expression_effects(module, &store, &if_stmt.cond, arena, collector)
                })?;

                let (next_state, cond_node, cond_sources) =
                    eval_guarded_function_expression(module, state, &if_stmt.cond, None, arena)?;
                state = next_state;
                let cond_node = procedural_condition(arena, cond_node)?;
                let boundaries = state.boundaries.clone();
                let true_guard = arena.alloc(SLTNode::Binary(
                    state.live_expr,
                    BinaryOp::LogicAnd,
                    cond_node,
                ))?;
                let mut true_sources = state.live_sources.clone();
                true_sources.extend(cond_sources.iter().copied());
                let true_state = with_collector_guard(
                    collector,
                    arena,
                    true_guard,
                    true_sources,
                    |collector, arena| {
                        collect_statements(
                            module,
                            FunctionControlState {
                                store: state.store.clone(),
                                boundaries: boundaries.clone(),
                                live_expr: state.live_expr,
                                live_sources: state.live_sources.clone(),
                            },
                            &if_stmt.true_side,
                            ret_id,
                            arena,
                            collector,
                        )
                    },
                )?;

                let false_cond = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, cond_node))?;
                let false_guard = arena.alloc(SLTNode::Binary(
                    state.live_expr,
                    BinaryOp::LogicAnd,
                    false_cond,
                ))?;
                let mut false_sources = state.live_sources.clone();
                false_sources.extend(cond_sources.iter().copied());
                let false_state = with_collector_guard(
                    collector,
                    arena,
                    false_guard,
                    false_sources,
                    |collector, arena| {
                        collect_statements(
                            module,
                            FunctionControlState {
                                store: state.store,
                                boundaries,
                                live_expr: state.live_expr,
                                live_sources: state.live_sources,
                            },
                            &if_stmt.false_side,
                            ret_id,
                            arena,
                            collector,
                        )
                    },
                )?;

                let mut live_sources = cond_sources;
                live_sources.extend(true_state.live_sources.iter().copied());
                live_sources.extend(false_state.live_sources.iter().copied());

                Ok(FunctionControlState {
                    store: merge_symbolic_stores(
                        module,
                        &true_state.store,
                        &false_state.store,
                        cond_node,
                        &live_sources,
                        arena,
                    )?,
                    boundaries: merge_boundaries(true_state.boundaries, false_state.boundaries),
                    live_expr: merge_control_expr(
                        cond_node,
                        true_state.live_expr,
                        false_state.live_expr,
                        arena,
                    )?,
                    live_sources,
                })
            }
            Statement::Case(case_stmt) => {
                let store = state.store.clone();
                let live = state.live_expr;
                let live_sources = state.live_sources.clone();
                with_collector_guard(collector, arena, live, live_sources, |collector, arena| {
                    collect_expression_effects(
                        module,
                        &store,
                        &case_stmt.case_target,
                        arena,
                        collector,
                    )
                })?;
                let (state, target_node, target_sources) = eval_guarded_function_expression(
                    module,
                    state,
                    &case_stmt.case_target,
                    None,
                    arena,
                )?;
                let target = expr::EvaluatedCaseTarget {
                    node: target_node,
                    sources: target_sources,
                };
                collect_case_from_arm(
                    module, state, case_stmt, &target, 0, ret_id, arena, collector,
                )
            }
            Statement::For(for_stmt) => {
                if statement_contains_return(&Statement::For(for_stmt.clone()), ret_id) {
                    return collect_function_for(module, state, for_stmt, ret_id, arena, collector);
                }
                let guard_state = state.clone();
                let store = state.store.clone();
                let live = state.live_expr;
                let live_sources = state.live_sources.clone();
                with_collector_guard(collector, arena, live, live_sources, |collector, arena| {
                    let _ = collect_comb_effects_for(module, store, for_stmt, arena, collector)?;
                    Ok(())
                })?;
                let (store, boundaries) =
                    eval_for(module, state.store, state.boundaries, for_stmt, arena)?;
                apply_function_guard(
                    module,
                    guard_state,
                    store,
                    boundaries,
                    bool_node(arena, true)?,
                    HashSet::default(),
                    arena,
                )
            }
            Statement::Null => Ok(state),
            Statement::IfReset(ir) => Err(ParserError::illegal_context(
                "statement in comb function body",
                format!("{stmt}"),
                Some(&ir.token),
            )),
            Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                Err(ParserError::illegal_context(
                    "statement in comb function body",
                    format!("{stmt}"),
                    None,
                ))
            }
        }
    }

    let _ = collect_statements(
        module,
        FunctionControlState {
            store: local_store,
            boundaries: BoundaryMap::default(),
            live_expr: bool_node(arena, true)?,
            live_sources: HashSet::default(),
        },
        statements,
        ret_id,
        arena,
        collector,
    )?;
    Ok(())
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
            for (part, repeat) in parts {
                collect_expression_position_inputs(module, part, out)?;
                if let Some(repeat) = repeat {
                    collect_expression_position_inputs(module, repeat, out)?;
                }
            }
            Ok(())
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expr, repeat) => {
                        collect_expression_position_inputs(module, expr, out)?;
                        if let Some(repeat) = repeat {
                            collect_expression_position_inputs(module, repeat, out)?;
                        }
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
            if let Some((_, expr)) = &select.1 {
                collect_expression_position_inputs(module, expr, out)?;
            }
            Ok(())
        }
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

pub(super) fn collect_comb_effects_statements(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    statements: &[Statement],
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<SymbolicStore<VarId>, ParserError> {
    for stmt in statements {
        match stmt {
            Statement::Assign(assign) => {
                collect_assignment_effects(module, &store, assign, arena, collector)?;
                let (next_store, _) =
                    eval_assign(module, store, BoundaryMap::default(), assign, arena)?;
                store = next_store;
            }
            Statement::SystemFunctionCall(call) => {
                collect_system_function_effect(module, &mut store, call, arena, collector)?;
            }
            Statement::FunctionCall(call) => {
                collect_function_call_effects(module, &store, call, arena, collector)?;
                let (next_store, _) = eval_statement_form_function_call(
                    module,
                    store,
                    BoundaryMap::default(),
                    call,
                    arena,
                    LoweringPhase::CombLowering,
                )?;
                store = next_store;
            }
            Statement::For(for_stmt) => {
                store = collect_comb_effects_for(module, store, for_stmt, arena, collector)?;
            }
            Statement::If(if_stmt) => {
                collect_expression_effects(module, &store, &if_stmt.cond, arena, collector)?;
                let ((cond_node, sources), _) =
                    eval_expression_effectful(module, &mut store, &if_stmt.cond, arena, None)?;
                let cond_node = procedural_condition(arena, cond_node)?;
                collector.sensitivity.extend(sources.iter().copied());
                let saved_guard = collector.active_guard;
                let saved_guard_sources = collector.active_guard_sources.clone();
                let true_guard = if let Some(active) = saved_guard {
                    arena.alloc(SLTNode::Binary(active, BinaryOp::LogicAnd, cond_node))?
                } else {
                    cond_node
                };
                let mut true_sources = saved_guard_sources.clone();
                true_sources.extend(sources.iter().copied());
                collector.active_guard = Some(true_guard);
                collector.active_guard_sources = true_sources;
                let side_store = collect_comb_effects_statements(
                    module,
                    store.clone(),
                    &if_stmt.true_side,
                    arena,
                    collector,
                )?;
                let false_cond = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, cond_node))?;
                let false_guard = if let Some(active) = saved_guard {
                    arena.alloc(SLTNode::Binary(active, BinaryOp::LogicAnd, false_cond))?
                } else {
                    false_cond
                };
                let mut false_sources = saved_guard_sources.clone();
                false_sources.extend(sources.iter().copied());
                collector.active_guard = Some(false_guard);
                collector.active_guard_sources = false_sources;
                let else_store = collect_comb_effects_statements(
                    module,
                    store,
                    &if_stmt.false_side,
                    arena,
                    collector,
                )?;
                collector.active_guard = saved_guard;
                collector.active_guard_sources = saved_guard_sources;
                store = merge_symbolic_stores(
                    module,
                    &side_store,
                    &else_store,
                    bool_node(arena, true)?,
                    &HashSet::default(),
                    arena,
                )?;
            }
            Statement::Case(case_stmt) => {
                store = collect_comb_effects_case(module, store, case_stmt, arena, collector)?;
            }
            Statement::IfReset(_)
            | Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => {}
        }
    }
    Ok(store)
}

fn collect_comb_effects_case(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    case_stmt: &CaseStatement,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<SymbolicStore<VarId>, ParserError> {
    fn collect_from_arm(
        module: &Module,
        mut store: SymbolicStore<VarId>,
        case_stmt: &CaseStatement,
        target: &expr::EvaluatedCaseTarget,
        arm_index: usize,
        arena: &mut SLTNodeArena<VarId>,
        collector: &mut CombEffectCollector,
    ) -> Result<SymbolicStore<VarId>, ParserError> {
        let Some(arm) = case_stmt.arms.get(arm_index) else {
            return collect_comb_effects_statements(
                module,
                store,
                &case_stmt.default,
                arena,
                collector,
            );
        };

        collect_case_pattern_effects(
            module,
            &store,
            &case_stmt.case_target,
            target,
            &arm.patterns,
            arena,
            collector,
        )?;
        let ((cond_node, sources), _) = eval_case_arm_condition_effectful(
            module,
            &mut store,
            &case_stmt.case_target,
            target,
            &arm.patterns,
            arena,
        )?;
        let cond_node = procedural_condition(arena, cond_node)?;
        collector.sensitivity.extend(sources.iter().copied());

        let saved_guard = collector.active_guard;
        let saved_guard_sources = collector.active_guard_sources.clone();
        let true_guard = if let Some(active) = saved_guard {
            arena.alloc(SLTNode::Binary(active, BinaryOp::LogicAnd, cond_node))?
        } else {
            cond_node
        };
        let mut true_sources = saved_guard_sources.clone();
        true_sources.extend(sources.iter().copied());
        collector.active_guard = Some(true_guard);
        collector.active_guard_sources = true_sources;
        let side_store =
            collect_comb_effects_statements(module, store.clone(), &arm.body, arena, collector)?;

        let false_cond = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, cond_node))?;
        let false_guard = if let Some(active) = saved_guard {
            arena.alloc(SLTNode::Binary(active, BinaryOp::LogicAnd, false_cond))?
        } else {
            false_cond
        };
        let mut false_sources = saved_guard_sources.clone();
        false_sources.extend(sources.iter().copied());
        collector.active_guard = Some(false_guard);
        collector.active_guard_sources = false_sources;
        let else_store = collect_from_arm(
            module,
            store,
            case_stmt,
            target,
            arm_index + 1,
            arena,
            collector,
        )?;

        collector.active_guard = saved_guard;
        collector.active_guard_sources = saved_guard_sources;
        merge_symbolic_stores(module, &side_store, &else_store, cond_node, &sources, arena)
    }

    collect_expression_effects(module, &store, &case_stmt.case_target, arena, collector)?;
    let (target, _) =
        eval_case_target_effectful(module, &mut store, &case_stmt.case_target, arena)?;
    collect_from_arm(module, store, case_stmt, &target, 0, arena, collector)
}

fn collect_comb_effects_for(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    for_stmt: &ForStatement,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<SymbolicStore<VarId>, ParserError> {
    let loop_width = for_stmt.var_type.total_width().unwrap_or(32);
    let original_store = store.clone();
    let (start_bound, end_bound) = for_range_bounds(&for_stmt.range);
    for bound in [start_bound, end_bound] {
        let ForBound::Expression(expression) = bound else {
            continue;
        };
        collect_expression_effects(module, &store, expression, arena, collector)?;
        let ((_, sources), _) =
            eval_expression_effectful(module, &mut store, expression, arena, None)?;
        collector.sensitivity.extend(sources);
    }
    let ForRange::Forward {
        start,
        end,
        inclusive,
        ..
    } = &for_stmt.range
    else {
        let (loop_effects, observer_start) =
            collect_dynamic_for_effects(module, &store, for_stmt, arena, collector)?;
        let (store, _, runner) = eval_for_with_effects(
            module,
            original_store.clone(),
            BoundaryMap::default(),
            for_stmt,
            arena,
            &loop_effects,
        )?;
        attach_loop_runner_to_first_observer(collector, observer_start, runner);
        return Ok(store);
    };
    let Some(start) = const_for_bound_i64(start) else {
        let (loop_effects, observer_start) =
            collect_dynamic_for_effects(module, &store, for_stmt, arena, collector)?;
        let (store, _, runner) = eval_for_with_effects(
            module,
            original_store.clone(),
            BoundaryMap::default(),
            for_stmt,
            arena,
            &loop_effects,
        )?;
        attach_loop_runner_to_first_observer(collector, observer_start, runner);
        return Ok(store);
    };
    let Some(end) = const_for_bound_i64(end) else {
        let (loop_effects, observer_start) =
            collect_dynamic_for_effects(module, &store, for_stmt, arena, collector)?;
        let (store, _, runner) = eval_for_with_effects(
            module,
            original_store.clone(),
            BoundaryMap::default(),
            for_stmt,
            arena,
            &loop_effects,
        )?;
        attach_loop_runner_to_first_observer(collector, observer_start, runner);
        return Ok(store);
    };
    let final_end = if *inclusive { end + 1 } else { end };
    for i in start..final_end {
        let mut iter_store = store.clone();
        let node = arena.alloc(SLTNode::Constant(
            BigUint::from(i as u64),
            BigUint::from(0u8),
            loop_width,
            for_stmt.var_type.signed,
        ))?;
        iter_store.insert(
            for_stmt.var_id,
            RangeStore::new(Some((node, HashSet::default())), loop_width),
        );
        store =
            collect_comb_effects_statements(module, iter_store, &for_stmt.body, arena, collector)?;
        store.remove(&for_stmt.var_id);
    }
    eval_for(
        module,
        original_store,
        BoundaryMap::default(),
        for_stmt,
        arena,
    )
    .map(|(s, _)| s)
}

fn collect_dynamic_for_effects(
    module: &Module,
    store: &SymbolicStore<VarId>,
    for_stmt: &ForStatement,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<(Vec<SLTForEffect>, usize), ParserError> {
    let Some(loop_width) = for_stmt.var_type.total_width() else {
        return Ok((Vec::new(), collector.observers.len()));
    };
    let mut iter_store = store.clone();
    let mut written_accesses = HashMap::default();
    collect_written_accesses(module, &for_stmt.body, &mut written_accesses)?;
    for (id, accesses) in written_accesses {
        let width = resolve_total_width(module, &module.variables[&id])?;
        let mut loop_store = RangeStore::new(None, width);
        let mut covered = vec![false; width];
        for access in accesses {
            for slot in covered.iter_mut().take(access.msb + 1).skip(access.lsb) {
                *slot = true;
            }
        }
        let original = store
            .get(&id)
            .cloned()
            .unwrap_or_else(|| RangeStore::new(None, width));
        let mut bit = 0usize;
        while bit < width {
            if covered[bit] {
                bit += 1;
                continue;
            }
            let start = bit;
            while bit < width && !covered[bit] {
                bit += 1;
            }
            let end = bit - 1;
            let access = BitAccess::new(start, end);
            let parts = original.get_parts(access).map_err(|error| {
                super::range_store_error("observer for-loop state", error, Some(&for_stmt.token))
            })?;
            let (expr, sources) = combine_parts_with_default(id, access.lsb, parts, arena)?;
            loop_store
                .update(access, Some((expr, sources)))
                .map_err(|error| {
                    super::range_store_error(
                        "observer for-loop state",
                        error,
                        Some(&for_stmt.token),
                    )
                })?;
        }
        iter_store.insert(id, loop_store);
    }
    iter_store.insert(for_stmt.var_id, RangeStore::new(None, loop_width));
    let observer_start = collector.observers.len();
    let saved = collector.loop_effects.take();
    collector.loop_effects = Some(Vec::new());
    let _ = collect_comb_effects_statements(module, iter_store, &for_stmt.body, arena, collector)?;
    let effects = collector.loop_effects.take().unwrap_or_default();
    collector.loop_effects = saved;
    Ok((effects, observer_start))
}

fn attach_loop_runner_to_first_observer(
    collector: &mut CombEffectCollector,
    observer_start: usize,
    runner: Option<NodeId>,
) {
    let Some(runner) = runner else {
        return;
    };
    if let Some(observer) = collector.observers[observer_start..]
        .iter_mut()
        .find(|observer| observer.captured_in_loop)
    {
        observer.loop_runner = Some(runner);
    }
}
