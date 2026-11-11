//! Runtime effects of the `for` statements of a function body.

use super::*;

fn apply_function_loop_continue_guard(
    module: &Module,
    state: FunctionLoopControlState,
    next_function: FunctionControlState,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<FunctionLoopControlState, ParserError> {
    let boundaries = merge_boundaries(state.function.boundaries.clone(), next_function.boundaries);
    if matches!(constant_bool(arena, state.continue_expr), Some(true)) {
        return Ok(FunctionLoopControlState {
            function: FunctionControlState {
                boundaries,
                ..next_function
            },
            ..state
        });
    }

    let store = merge_symbolic_versions(
        module,
        &next_function.store,
        &state.function.store,
        state.continue_expr,
        &state.continue_sources,
        arena,
    )?;
    let mut live_sources = state.continue_sources.clone();
    live_sources.extend(next_function.live_sources);
    live_sources.extend(state.function.live_sources);
    Ok(FunctionLoopControlState {
        function: FunctionControlState {
            store,
            boundaries,
            live_expr: merge_control_expr(
                state.continue_expr,
                next_function.live_expr,
                state.function.live_expr,
                arena,
            )?,
            live_sources,
        },
        ..state
    })
}

fn collect_function_loop_statements(
    module: &Module,
    mut state: FunctionLoopControlState,
    statements: &[Statement],
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<FunctionLoopControlState, ParserError> {
    for statement in statements {
        if matches!(constant_bool(arena, state.function.live_expr), Some(false))
            || matches!(constant_bool(arena, state.continue_expr), Some(false))
        {
            break;
        }
        state =
            collect_function_loop_statement(module, state, statement, ret_id, arena, collector)?;
    }
    Ok(state)
}

fn collect_function_loop_if(
    module: &Module,
    mut state: FunctionLoopControlState,
    if_stmt: &IfStatement,
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<FunctionLoopControlState, ParserError> {
    let guard_state = state.clone();
    let store = state.function.store.fork();
    with_collector_guard(
        collector,
        arena,
        state.continue_expr,
        state.continue_sources.clone(),
        |collector, arena| {
            with_collector_guard(
                collector,
                arena,
                state.function.live_expr,
                state.function.live_sources.clone(),
                |collector, arena| {
                    collect_expression_effects(module, &store, &if_stmt.cond, arena, collector)
                },
            )
        },
    )?;
    let (function, cond_node, cond_sources) = eval_guarded_function_expression(
        module,
        state.function.clone(),
        &if_stmt.cond,
        None,
        arena,
    )?;
    state = apply_function_loop_continue_guard(module, guard_state, function, arena)?;
    let cond_node = procedural_condition(arena, cond_node)?;

    if let Some(cond_value) = constant_bool(arena, cond_node) {
        let side = if cond_value {
            &if_stmt.true_side
        } else {
            &if_stmt.false_side
        };
        return collect_function_loop_statements(module, state, side, ret_id, arena, collector);
    }

    let true_guard = arena.alloc(SLTNode::Binary(
        state.function.live_expr,
        BinaryOp::LogicAnd,
        cond_node,
    ))?;
    let true_guard = arena.alloc(SLTNode::Binary(
        state.continue_expr,
        BinaryOp::LogicAnd,
        true_guard,
    ))?;
    let mut true_sources = state.function.live_sources.clone();
    true_sources.extend(state.continue_sources.iter().copied());
    true_sources.extend(cond_sources.iter().copied());
    let then_state = with_collector_guard(
        collector,
        arena,
        true_guard,
        true_sources,
        |collector, arena| {
            collect_function_loop_statements(
                module,
                FunctionLoopControlState {
                    function: state.function.clone(),
                    continue_expr: state.continue_expr,
                    continue_sources: state.continue_sources.clone(),
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
        state.function.live_expr,
        BinaryOp::LogicAnd,
        false_cond,
    ))?;
    let false_guard = arena.alloc(SLTNode::Binary(
        state.continue_expr,
        BinaryOp::LogicAnd,
        false_guard,
    ))?;
    let mut false_sources = state.function.live_sources.clone();
    false_sources.extend(state.continue_sources.iter().copied());
    false_sources.extend(cond_sources.iter().copied());
    let else_state = with_collector_guard(
        collector,
        arena,
        false_guard,
        false_sources,
        |collector, arena| {
            collect_function_loop_statements(
                module,
                FunctionLoopControlState {
                    function: state.function,
                    continue_expr: state.continue_expr,
                    continue_sources: state.continue_sources,
                },
                &if_stmt.false_side,
                ret_id,
                arena,
                collector,
            )
        },
    )?;

    let mut continue_sources = cond_sources;
    continue_sources.extend(then_state.continue_sources);
    continue_sources.extend(else_state.continue_sources);
    let mut live_sources = continue_sources.clone();
    live_sources.extend(then_state.function.live_sources);
    live_sources.extend(else_state.function.live_sources);
    Ok(FunctionLoopControlState {
        function: FunctionControlState {
            store: merge_symbolic_versions(
                module,
                &then_state.function.store,
                &else_state.function.store,
                cond_node,
                &live_sources,
                arena,
            )?,
            boundaries: merge_boundaries(
                then_state.function.boundaries,
                else_state.function.boundaries,
            ),
            live_expr: merge_control_expr(
                cond_node,
                then_state.function.live_expr,
                else_state.function.live_expr,
                arena,
            )?,
            live_sources,
        },
        continue_expr: merge_control_expr(
            cond_node,
            then_state.continue_expr,
            else_state.continue_expr,
            arena,
        )?,
        continue_sources,
    })
}

fn collect_function_loop_case(
    module: &Module,
    mut state: FunctionLoopControlState,
    case_stmt: &CaseStatement,
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<FunctionLoopControlState, ParserError> {
    fn collect_from_arm(
        module: &Module,
        mut state: FunctionLoopControlState,
        case_stmt: &CaseStatement,
        target: &expr::EvaluatedCaseTarget,
        arm_index: usize,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
        collector: &mut CombEffectCollector,
    ) -> Result<FunctionLoopControlState, ParserError> {
        let Some(arm) = case_stmt.arms.get(arm_index) else {
            return collect_function_loop_statements(
                module,
                state,
                &case_stmt.default,
                ret_id,
                arena,
                collector,
            );
        };

        let store = state.function.store.fork();
        with_collector_guard(
            collector,
            arena,
            state.continue_expr,
            state.continue_sources.clone(),
            |collector, arena| {
                with_collector_guard(
                    collector,
                    arena,
                    state.function.live_expr,
                    state.function.live_sources.clone(),
                    |collector, arena| {
                        collect_case_pattern_effects(
                            module,
                            &store,
                            &case_stmt.case_target,
                            target,
                            &arm.patterns,
                            arena,
                            collector,
                        )
                    },
                )
            },
        )?;
        let mut next_store = state.function.store.fork();
        let ((cond_node, cond_sources), cond_bounds) = eval_case_arm_condition_effectful(
            module,
            &mut next_store,
            &case_stmt.case_target,
            target,
            &arm.patterns,
            arena,
        )?;
        let next_function = apply_function_guard(
            module,
            state.function.clone(),
            next_store,
            cond_bounds,
            bool_node(arena, true)?,
            HashSet::default(),
            arena,
        )?;
        let guard_state = state.clone();
        state = apply_function_loop_continue_guard(module, guard_state, next_function, arena)?;
        let cond_node = procedural_condition(arena, cond_node)?;

        if let Some(cond_value) = constant_bool(arena, cond_node) {
            return if cond_value {
                collect_function_loop_statements(module, state, &arm.body, ret_id, arena, collector)
            } else {
                collect_from_arm(
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
            state.function.live_expr,
            BinaryOp::LogicAnd,
            cond_node,
        ))?;
        let true_guard = arena.alloc(SLTNode::Binary(
            state.continue_expr,
            BinaryOp::LogicAnd,
            true_guard,
        ))?;
        let mut true_sources = state.function.live_sources.clone();
        true_sources.extend(state.continue_sources.iter().copied());
        true_sources.extend(cond_sources.iter().copied());
        let then_state = with_collector_guard(
            collector,
            arena,
            true_guard,
            true_sources,
            |collector, arena| {
                collect_function_loop_statements(
                    module,
                    state.clone(),
                    &arm.body,
                    ret_id,
                    arena,
                    collector,
                )
            },
        )?;

        let false_cond = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, cond_node))?;
        let false_guard = arena.alloc(SLTNode::Binary(
            state.function.live_expr,
            BinaryOp::LogicAnd,
            false_cond,
        ))?;
        let false_guard = arena.alloc(SLTNode::Binary(
            state.continue_expr,
            BinaryOp::LogicAnd,
            false_guard,
        ))?;
        let mut false_sources = state.function.live_sources.clone();
        false_sources.extend(state.continue_sources.iter().copied());
        false_sources.extend(cond_sources.iter().copied());
        let else_state = with_collector_guard(
            collector,
            arena,
            false_guard,
            false_sources,
            |collector, arena| {
                collect_from_arm(
                    module,
                    state,
                    case_stmt,
                    target,
                    arm_index + 1,
                    ret_id,
                    arena,
                    collector,
                )
            },
        )?;

        let mut continue_sources = cond_sources;
        continue_sources.extend(then_state.continue_sources);
        continue_sources.extend(else_state.continue_sources);
        let mut live_sources = continue_sources.clone();
        live_sources.extend(then_state.function.live_sources);
        live_sources.extend(else_state.function.live_sources);
        Ok(FunctionLoopControlState {
            function: FunctionControlState {
                store: merge_symbolic_versions(
                    module,
                    &then_state.function.store,
                    &else_state.function.store,
                    cond_node,
                    &live_sources,
                    arena,
                )?,
                boundaries: merge_boundaries(
                    then_state.function.boundaries,
                    else_state.function.boundaries,
                ),
                live_expr: merge_control_expr(
                    cond_node,
                    then_state.function.live_expr,
                    else_state.function.live_expr,
                    arena,
                )?,
                live_sources,
            },
            continue_expr: merge_control_expr(
                cond_node,
                then_state.continue_expr,
                else_state.continue_expr,
                arena,
            )?,
            continue_sources,
        })
    }

    let guard_state = state.clone();
    let store = state.function.store.fork();
    with_collector_guard(
        collector,
        arena,
        state.continue_expr,
        state.continue_sources.clone(),
        |collector, arena| {
            with_collector_guard(
                collector,
                arena,
                state.function.live_expr,
                state.function.live_sources.clone(),
                |collector, arena| {
                    collect_expression_effects(
                        module,
                        &store,
                        &case_stmt.case_target,
                        arena,
                        collector,
                    )
                },
            )
        },
    )?;
    let (function, target_node, target_sources) = eval_guarded_function_expression(
        module,
        state.function.clone(),
        &case_stmt.case_target,
        None,
        arena,
    )?;
    state = apply_function_loop_continue_guard(module, guard_state, function, arena)?;
    let target = expr::EvaluatedCaseTarget::new(
        &case_stmt.case_target,
        target_node,
        target_sources,
        &state.function.store,
    );
    collect_from_arm(
        module, state, case_stmt, &target, 0, ret_id, arena, collector,
    )
}

fn collect_function_loop_statement(
    module: &Module,
    state: FunctionLoopControlState,
    statement: &Statement,
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<FunctionLoopControlState, ParserError> {
    match statement {
        Statement::If(if_stmt) => {
            collect_function_loop_if(module, state, if_stmt, ret_id, arena, collector)
        }
        Statement::Case(case_stmt) => {
            collect_function_loop_case(module, state, case_stmt, ret_id, arena, collector)
        }
        Statement::Break => Ok(FunctionLoopControlState {
            continue_expr: bool_node(arena, false)?,
            continue_sources: HashSet::default(),
            ..state
        }),
        _ => {
            let guard_state = state.clone();
            let next_function = with_collector_guard(
                collector,
                arena,
                state.continue_expr,
                state.continue_sources.clone(),
                |collector, arena| {
                    collect_statement(module, state.function, statement, ret_id, arena, collector)
                },
            )?;
            apply_function_loop_continue_guard(module, guard_state, next_function, arena)
        }
    }
}

pub(super) fn collect_function_for(
    module: &Module,
    state: FunctionControlState,
    for_stmt: &ForStatement,
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<FunctionControlState, ParserError> {
    let Some(loop_width) = for_stmt.var_type.total_width() else {
        return Err(ParserError::internal(
            "for loop variable width",
            format!("{:?}", for_stmt.var_name),
            Some(&for_stmt.token),
        ));
    };

    // Return-aware loops use this collector instead of
    // collect_comb_effects_for. Mirror bound evaluation here so bound
    // events are retained and the body sees output writes from both bounds.
    let mut iter_store = state.store.fork();
    let (start_bound, end_bound) = for_range_bounds(&for_stmt.range);
    for bound in [start_bound, end_bound] {
        let ForBound::Expression(expression) = bound else {
            continue;
        };
        let store = iter_store.fork();
        with_collector_guard(
            collector,
            arena,
            state.live_expr,
            state.live_sources.clone(),
            |collector, arena| {
                collect_expression_effects(module, &store, expression, arena, collector)
            },
        )?;
        let _ = eval_expression_effectful(module, &mut iter_store, expression, arena, None)?;
    }

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
        let original = iter_store
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
            let parts = original.get_parts_ref(access).map_err(|error| {
                super::super::super::range_store_error(
                    "function observer for-loop state",
                    error,
                    Some(&for_stmt.token),
                )
            })?;
            let (expr, sources) = combine_parts_with_default(id, access.lsb, parts, arena)?;
            loop_store
                .update(access, Some((expr, sources)))
                .map_err(|error| {
                    super::super::super::range_store_error(
                        "function observer for-loop state",
                        error,
                        Some(&for_stmt.token),
                    )
                })?;
        }
        iter_store.insert(id, loop_store);
    }
    iter_store.insert(for_stmt.var_id, RangeStore::new(None, loop_width));
    let mut loop_range = super::super::super::LoopRangeScope::enter(arena, for_stmt);
    let arena = &mut *loop_range;
    let iter_store_before = iter_store.fork();

    let observer_start = collector.observers.len();
    let saved_loop_effects = collector.loop_effects.take();
    collector.loop_effects = Some(Vec::new());
    let iter_state = with_collector_guard(
        collector,
        arena,
        state.live_expr,
        state.live_sources.clone(),
        |collector, arena| {
            collect_function_loop_statements(
                module,
                FunctionLoopControlState {
                    function: FunctionControlState {
                        store: iter_store,
                        boundaries: state.boundaries.clone(),
                        live_expr: bool_node(arena, true)?,
                        live_sources: HashSet::default(),
                    },
                    continue_expr: bool_node(arena, true)?,
                    continue_sources: HashSet::default(),
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

    // Use the ordinary loop fold only as a geometry and initial-state
    // template. Its data updates are replaced below with the return-aware
    // iteration state collected for function execution.
    let (mut result_store, boundaries) = eval_for(
        module,
        state.store.fork(),
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
        ..
    } = arena.get(template).clone()
    else {
        unreachable!();
    };
    let effective_continue = arena.alloc(SLTNode::Binary(
        iter_state.continue_expr,
        BinaryOp::And,
        iter_state.function.live_expr,
    ))?;
    let return_aware_updates = super::super::super::extract_store_updates(
        &iter_store_before,
        &iter_state.function.store,
        arena,
    )?
    .into_iter()
    .map(|(target, expr, _)| SLTForUpdate { target, expr })
    .collect::<Vec<_>>();
    let return_aware_updates = if return_aware_updates.is_empty() {
        updates.clone()
    } else {
        return_aware_updates
    };

    // The ordinary fold is useful as a geometry/template builder, but its
    // body keeps evaluating after a function return. Replace every data
    // result with the return-aware iteration updates collected above so
    // output-formal previews match ordinary function evaluation.
    for range_store in result_store.values_mut() {
        for (value, _, _) in range_store.ranges.values_mut() {
            let Some((node, _)) = value else {
                continue;
            };
            let SLTNode::ForFold {
                loop_var: node_loop_var,
                loop_width: node_loop_width,
                loop_signed: node_loop_signed,
                start: node_start,
                end: node_end,
                inclusive: node_inclusive,
                step: node_step,
                step_op: node_step_op,
                reverse: node_reverse,
                result: node_result,
                initials: node_initials,
                effects: node_effects,
                ..
            } = arena.get(*node).clone()
            else {
                continue;
            };
            if node_loop_var != for_stmt.var_id {
                continue;
            }
            *node = arena.alloc(SLTNode::ForFold {
                loop_var: node_loop_var,
                loop_width: node_loop_width,
                loop_signed: node_loop_signed,
                start: node_start,
                end: node_end,
                inclusive: node_inclusive,
                step: node_step,
                step_op: node_step_op,
                reverse: node_reverse,
                result: node_result,
                initials: node_initials,
                updates: return_aware_updates.clone(),
                effects: node_effects,
                continue_cond: effective_continue,
            })?;
        }
    }

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
            updates: return_aware_updates.clone(),
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
            update: iter_state.function.live_expr,
        },
        initials,
        updates: return_aware_updates,
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
    let mut live_sources = iter_state.continue_sources;
    live_sources.extend(iter_state.function.live_sources);
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
