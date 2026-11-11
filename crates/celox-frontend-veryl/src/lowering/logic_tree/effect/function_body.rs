//! Runtime effects of a function body.

use super::*;

mod for_statement;
use for_statement::*;

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

    let store = state.store.fork();
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

    let mut next_store = state.store.fork();
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
                    store: state.store.fork(),
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
        store: merge_symbolic_versions(
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
            let store = merge_symbolic_versions(
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
    let mut next_store = state.store.fork();
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
            let store = state.store.fork();
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
            let mut next_store = state.store.fork();
            let live = state.live_expr;
            let live_sources = state.live_sources.clone();
            let effect_boundaries =
                with_collector_guard(collector, arena, live, live_sources, |collector, arena| {
                    collect_system_function_effect(module, &mut next_store, call, arena, collector)
                })?;
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
            let store = state.store.fork();
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
            let store = state.store.fork();
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
                            store: state.store.fork(),
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
                store: merge_symbolic_versions(
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
            let store = state.store.fork();
            let live = state.live_expr;
            let live_sources = state.live_sources.clone();
            with_collector_guard(collector, arena, live, live_sources, |collector, arena| {
                collect_expression_effects(module, &store, &case_stmt.case_target, arena, collector)
            })?;
            let (state, target_node, target_sources) = eval_guarded_function_expression(
                module,
                state,
                &case_stmt.case_target,
                None,
                arena,
            )?;
            let target = expr::EvaluatedCaseTarget::new(
                &case_stmt.case_target,
                target_node,
                target_sources,
                &state.store,
            );
            collect_case_from_arm(
                module, state, case_stmt, &target, 0, ret_id, arena, collector,
            )
        }
        Statement::For(for_stmt) => {
            if statement_contains_return(&Statement::For(for_stmt.clone()), ret_id) {
                return collect_function_for(module, state, for_stmt, ret_id, arena, collector);
            }
            let guard_state = state.clone();
            let store = state.store.fork();
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
        Statement::Break => Ok(FunctionControlState {
            live_expr: bool_node(arena, false)?,
            live_sources: HashSet::default(),
            ..state
        }),
        Statement::TbMethodCall(_) | Statement::Unsupported(_) => {
            Err(ParserError::illegal_context(
                "statement in comb function body",
                format!("{stmt}"),
                None,
            ))
        }
    }
}

pub(super) fn collect_function_body_effects(
    module: &Module,
    local_store: SymbolicStore<VarId>,
    body: &FunctionBody,
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
    collector: &mut CombEffectCollector,
) -> Result<SymbolicStore<VarId>, ParserError> {
    let body = crate::lowering::function_return::implicit_return_body(body);
    let initial_local_store = local_store.fork();

    let final_state = collect_statements(
        module,
        FunctionControlState {
            store: local_store,
            boundaries: BoundaryMap::default(),
            live_expr: bool_node(arena, true)?,
            live_sources: HashSet::default(),
        },
        &body.statements,
        ret_id,
        arena,
        collector,
    )?;
    if body
        .statements
        .iter()
        .any(super::super::statement_contains_break)
    {
        // The ordinary observer collector deliberately treats `break` as a
        // control-only statement. Re-evaluate the body with the function
        // evaluator's break-aware loop state before exposing output-formal
        // previews to later actual destinations.
        let (_, _, break_aware_store) =
            eval_function_body_return(module, &initial_local_store, &body, ret_id, arena)?;
        Ok(break_aware_store)
    } else {
        Ok(final_state.store)
    }
}
