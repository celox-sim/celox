//! Runs of statements of a function body that contain `break`.

use super::*;

fn apply_function_break_guard(
    module: &Module,
    state: FunctionLoopControlState,
    next_function: FunctionControlState,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<FunctionLoopControlState, ParserError> {
    let boundaries = merge_boundaries(state.function.boundaries.clone(), next_function.boundaries);

    if matches!(constant_bool(arena, state.continue_expr), Some(true)) {
        Ok(FunctionLoopControlState {
            function: FunctionControlState {
                boundaries,
                ..next_function
            },
            ..state
        })
    } else {
        let merged_store = merge_symbolic_versions(
            module,
            &next_function.store,
            &state.function.store,
            state.continue_expr,
            &state.continue_sources,
            arena,
        )?;
        let mut merged_live_sources = state.continue_sources.clone();
        merged_live_sources.extend(next_function.live_sources);
        merged_live_sources.extend(state.function.live_sources);
        Ok(FunctionLoopControlState {
            function: FunctionControlState {
                store: merged_store,
                boundaries,
                live_expr: merge_control_expr(
                    state.continue_expr,
                    next_function.live_expr,
                    state.function.live_expr,
                    arena,
                )?,
                live_sources: merged_live_sources,
            },
            ..state
        })
    }
}

fn eval_function_break_if(
    module: &Module,
    mut state: FunctionLoopControlState,
    if_stmt: &IfStatement,
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<FunctionLoopControlState, ParserError> {
    let outer_state = state.clone();
    let (function, cond_expr, cond_sources) =
        eval_function_condition(module, state.function.clone(), &if_stmt.cond, arena)?;
    state.function = function;

    let executed_state = if let Some(cond_val) = constant_bool(arena, cond_expr) {
        let side = if cond_val {
            &if_stmt.true_side
        } else {
            &if_stmt.false_side
        };
        side.iter().try_fold(state, |s, step| {
            eval_function_break_statement(module, s, step, ret_id, arena)
        })?
    } else {
        let then_state = if_stmt.true_side.iter().try_fold(
            FunctionLoopControlState {
                function: FunctionControlState {
                    store: state.function.store.fork(),
                    boundaries: state.function.boundaries.clone(),
                    live_expr: state.function.live_expr,
                    live_sources: state.function.live_sources.clone(),
                },
                continue_expr: state.continue_expr,
                continue_sources: state.continue_sources.clone(),
            },
            |s, step| eval_function_break_statement(module, s, step, ret_id, arena),
        )?;
        let else_state = if_stmt.false_side.iter().try_fold(
            FunctionLoopControlState {
                function: FunctionControlState {
                    store: state.function.store,
                    boundaries: state.function.boundaries,
                    live_expr: state.function.live_expr,
                    live_sources: state.function.live_sources,
                },
                continue_expr: state.continue_expr,
                continue_sources: state.continue_sources,
            },
            |s, step| eval_function_break_statement(module, s, step, ret_id, arena),
        )?;

        let mut merged_sources = cond_sources;
        merged_sources.extend(then_state.continue_sources);
        merged_sources.extend(else_state.continue_sources);
        let mut live_sources = merged_sources.clone();
        live_sources.extend(then_state.function.live_sources);
        live_sources.extend(else_state.function.live_sources);

        FunctionLoopControlState {
            function: FunctionControlState {
                store: merge_symbolic_versions(
                    module,
                    &then_state.function.store,
                    &else_state.function.store,
                    cond_expr,
                    &live_sources,
                    arena,
                )?,
                boundaries: merge_boundaries(
                    then_state.function.boundaries,
                    else_state.function.boundaries,
                ),
                live_expr: merge_control_expr(
                    cond_expr,
                    then_state.function.live_expr,
                    else_state.function.live_expr,
                    arena,
                )?,
                live_sources,
            },
            continue_expr: merge_control_expr(
                cond_expr,
                then_state.continue_expr,
                else_state.continue_expr,
                arena,
            )?,
            continue_sources: merged_sources,
        }
    };

    if matches!(constant_bool(arena, outer_state.continue_expr), Some(true)) {
        return Ok(executed_state);
    }

    let guarded =
        apply_function_break_guard(module, outer_state.clone(), executed_state.function, arena)?;
    let continue_expr = match constant_bool(arena, executed_state.continue_expr) {
        Some(true) => outer_state.continue_expr,
        Some(false) => bool_node(arena, false)?,
        None => arena.alloc(SLTNode::Binary(
            outer_state.continue_expr,
            BinaryOp::And,
            executed_state.continue_expr,
        ))?,
    };
    let mut continue_sources = outer_state.continue_sources;
    continue_sources.extend(executed_state.continue_sources);
    Ok(FunctionLoopControlState {
        function: guarded.function,
        continue_expr,
        continue_sources,
    })
}

fn eval_function_break_case(
    module: &Module,
    mut state: FunctionLoopControlState,
    case_stmt: &CaseStatement,
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<FunctionLoopControlState, ParserError> {
    fn eval_from_arm(
        module: &Module,
        mut state: FunctionLoopControlState,
        case_stmt: &CaseStatement,
        target: &EvaluatedCaseTarget,
        arm_index: usize,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<FunctionLoopControlState, ParserError> {
        let Some(arm) = case_stmt.arms.get(arm_index) else {
            return case_stmt.default.iter().try_fold(state, |s, step| {
                eval_function_break_statement(module, s, step, ret_id, arena)
            });
        };

        let mut next_store = state.function.store.fork();
        let ((cond_expr, cond_sources), cond_bounds) = eval_case_arm_condition_effectful(
            module,
            &mut next_store,
            &case_stmt.case_target,
            target,
            &arm.patterns,
            arena,
        )?;
        state.function = apply_function_guard(
            module,
            state.function,
            next_store,
            cond_bounds,
            bool_node(arena, true)?,
            HashSet::default(),
            arena,
        )?;
        let cond_expr = procedural_condition(arena, cond_expr)?;
        let boundaries = state.function.boundaries.clone();

        if let Some(cond_val) = constant_bool(arena, cond_expr) {
            let state = FunctionLoopControlState {
                function: FunctionControlState {
                    boundaries,
                    ..state.function
                },
                ..state
            };
            return if cond_val {
                arm.body.iter().try_fold(state, |s, step| {
                    eval_function_break_statement(module, s, step, ret_id, arena)
                })
            } else {
                eval_from_arm(
                    module,
                    state,
                    case_stmt,
                    target,
                    arm_index + 1,
                    ret_id,
                    arena,
                )
            };
        }

        let then_state = arm.body.iter().try_fold(
            FunctionLoopControlState {
                function: FunctionControlState {
                    store: state.function.store.fork(),
                    boundaries: boundaries.clone(),
                    live_expr: state.function.live_expr,
                    live_sources: state.function.live_sources.clone(),
                },
                continue_expr: state.continue_expr,
                continue_sources: state.continue_sources.clone(),
            },
            |s, step| eval_function_break_statement(module, s, step, ret_id, arena),
        )?;
        let else_state = eval_from_arm(
            module,
            FunctionLoopControlState {
                function: FunctionControlState {
                    store: state.function.store,
                    boundaries,
                    live_expr: state.function.live_expr,
                    live_sources: state.function.live_sources,
                },
                continue_expr: state.continue_expr,
                continue_sources: state.continue_sources,
            },
            case_stmt,
            target,
            arm_index + 1,
            ret_id,
            arena,
        )?;

        let mut merged_sources = cond_sources;
        merged_sources.extend(then_state.continue_sources);
        merged_sources.extend(else_state.continue_sources);
        let mut live_sources = merged_sources.clone();
        live_sources.extend(then_state.function.live_sources);
        live_sources.extend(else_state.function.live_sources);

        Ok(FunctionLoopControlState {
            function: FunctionControlState {
                store: merge_symbolic_versions(
                    module,
                    &then_state.function.store,
                    &else_state.function.store,
                    cond_expr,
                    &live_sources,
                    arena,
                )?,
                boundaries: merge_boundaries(
                    then_state.function.boundaries,
                    else_state.function.boundaries,
                ),
                live_expr: merge_control_expr(
                    cond_expr,
                    then_state.function.live_expr,
                    else_state.function.live_expr,
                    arena,
                )?,
                live_sources,
            },
            continue_expr: merge_control_expr(
                cond_expr,
                then_state.continue_expr,
                else_state.continue_expr,
                arena,
            )?,
            continue_sources: merged_sources,
        })
    }

    let outer_state = state.clone();
    let (function, target_node, target_sources) = eval_function_expression(
        module,
        state.function.clone(),
        &case_stmt.case_target,
        None,
        arena,
    )?;
    state.function = function;
    let target = EvaluatedCaseTarget::new(
        &case_stmt.case_target,
        target_node,
        target_sources,
        &state.function.store,
    );
    let executed_state = eval_from_arm(module, state, case_stmt, &target, 0, ret_id, arena)?;

    if matches!(constant_bool(arena, outer_state.continue_expr), Some(true)) {
        return Ok(executed_state);
    }

    let guarded =
        apply_function_break_guard(module, outer_state.clone(), executed_state.function, arena)?;
    let continue_expr = match constant_bool(arena, executed_state.continue_expr) {
        Some(true) => outer_state.continue_expr,
        Some(false) => bool_node(arena, false)?,
        None => arena.alloc(SLTNode::Binary(
            outer_state.continue_expr,
            BinaryOp::And,
            executed_state.continue_expr,
        ))?,
    };
    let mut continue_sources = outer_state.continue_sources;
    continue_sources.extend(executed_state.continue_sources);
    Ok(FunctionLoopControlState {
        function: guarded.function,
        continue_expr,
        continue_sources,
    })
}

fn eval_function_break_statement(
    module: &Module,
    state: FunctionLoopControlState,
    stmt: &Statement,
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<FunctionLoopControlState, ParserError> {
    if matches!(constant_bool(arena, state.function.live_expr), Some(false))
        || matches!(constant_bool(arena, state.continue_expr), Some(false))
    {
        return Ok(state);
    }

    match stmt {
        Statement::If(if_stmt) => eval_function_break_if(module, state, if_stmt, ret_id, arena),
        Statement::Case(case_stmt) => {
            eval_function_break_case(module, state, case_stmt, ret_id, arena)
        }
        Statement::Assign(_)
        | Statement::For(_)
        | Statement::FunctionCall(_)
        | Statement::SystemFunctionCall(_)
        | Statement::Null => {
            let guard_state = state.clone();
            let next_function =
                eval_function_statement(module, state.function, stmt, ret_id, arena)?;
            apply_function_break_guard(module, guard_state, next_function, arena)
        }
        Statement::Break => Ok(FunctionLoopControlState {
            continue_sources: HashSet::default(),
            continue_expr: bool_node(arena, false)?,
            ..state
        }),
        Statement::IfReset(ir) => Err(ParserError::illegal_context(
            "statement in comb function body",
            format!("{stmt}"),
            Some(&ir.token),
        )),
        Statement::TbMethodCall(_) | Statement::Unsupported(_) => {
            Err(ParserError::illegal_context(
                "statement in comb function body",
                format!("{stmt}"),
                None,
            ))
        }
    }
}

pub(super) fn eval_function_unrolled_loop_statements(
    module: &Module,
    state: FunctionControlState,
    statements: &[Statement],
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<FunctionControlState, ParserError> {
    let loop_state = statements.iter().try_fold(
        FunctionLoopControlState {
            function: state,
            continue_expr: bool_node(arena, true)?,
            continue_sources: HashSet::default(),
        },
        |s, stmt| eval_function_break_statement(module, s, stmt, ret_id, arena),
    )?;
    Ok(loop_state.function)
}
