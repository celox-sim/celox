//! Runtime effects of the statements of an `always_comb`.

use super::*;

pub(in crate::lowering::logic_tree) fn collect_comb_effects_statements(
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
                    store.fork(),
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
                store = merge_symbolic_versions(
                    module,
                    &side_store,
                    &else_store,
                    cond_node,
                    &sources,
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

pub(super) fn collect_comb_effects_case(
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
            collect_comb_effects_statements(module, store.fork(), &arm.body, arena, collector)?;

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
        merge_symbolic_versions(module, &side_store, &else_store, cond_node, &sources, arena)
    }

    collect_expression_effects(module, &store, &case_stmt.case_target, arena, collector)?;
    let (target, _) =
        eval_case_target_effectful(module, &mut store, &case_stmt.case_target, arena)?;
    collect_from_arm(module, store, case_stmt, &target, 0, arena, collector)
}

pub(super) fn collect_comb_effects_for(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    for_stmt: &ForStatement,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<SymbolicStore<VarId>, ParserError> {
    let loop_width = for_stmt.var_type.total_width().unwrap_or(32);
    let original_store = store.fork();
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
            original_store.fork(),
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
            original_store.fork(),
            BoundaryMap::default(),
            for_stmt,
            arena,
            &loop_effects,
        )?;
        attach_loop_runner_to_first_observer(collector, observer_start, runner);
        return Ok(store);
    };
    // Signed negative starts compare as large unsigned values when the
    // continuation bound is unsigned. Keep that decision in the runtime loop
    // instead of unrolling observable effects with a signed host range.
    let bound_signed = match end {
        ForBound::Const(_, signed) => *signed,
        ForBound::Expression(expr) => crate::context_width::expression_signed(expr),
    };
    let Some(end) = const_for_bound_i64(end).filter(|_| bound_signed || start >= 0) else {
        let (loop_effects, observer_start) =
            collect_dynamic_for_effects(module, &store, for_stmt, arena, collector)?;
        let (store, _, runner) = eval_for_with_effects(
            module,
            original_store.fork(),
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
        let mut iter_store = store.fork();
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

pub(super) fn collect_dynamic_for_effects(
    module: &Module,
    store: &SymbolicStore<VarId>,
    for_stmt: &ForStatement,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<(Vec<SLTForEffect>, usize), ParserError> {
    let Some(loop_width) = for_stmt.var_type.total_width() else {
        return Ok((Vec::new(), collector.observers.len()));
    };
    let mut iter_store = store.fork();
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
            let parts = original.get_parts_ref(access).map_err(|error| {
                super::super::range_store_error(
                    "observer for-loop state",
                    error,
                    Some(&for_stmt.token),
                )
            })?;
            let (expr, sources) = combine_parts_with_default(id, access.lsb, parts, arena)?;
            loop_store
                .update(access, Some((expr, sources)))
                .map_err(|error| {
                    super::super::range_store_error(
                        "observer for-loop state",
                        error,
                        Some(&for_stmt.token),
                    )
                })?;
        }
        iter_store.insert(id, loop_store);
    }
    iter_store.insert(for_stmt.var_id, RangeStore::new(None, loop_width));
    let mut loop_range = super::super::LoopRangeScope::enter(arena, for_stmt);
    let arena = &mut *loop_range;
    let observer_start = collector.observers.len();
    let saved = collector.loop_effects.take();
    collector.loop_effects = Some(Vec::new());
    let _ = collect_comb_effects_statements(module, iter_store, &for_stmt.body, arena, collector)?;
    let effects = collector.loop_effects.take().unwrap_or_default();
    collector.loop_effects = saved;
    Ok((effects, observer_start))
}

pub(super) fn attach_loop_runner_to_first_observer(
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
