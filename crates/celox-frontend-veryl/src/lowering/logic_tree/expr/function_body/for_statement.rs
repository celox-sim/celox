//! `for` statements of a function body.

use super::*;

fn apply_function_loop_continue_guard(
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

fn eval_function_loop_if(
    module: &Module,
    mut state: FunctionLoopControlState,
    if_stmt: &IfStatement,
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<FunctionLoopControlState, ParserError> {
    let guard_state = state.clone();
    let (function, cond_expr, cond_sources) =
        eval_function_condition(module, state.function.clone(), &if_stmt.cond, arena)?;
    state = apply_function_loop_continue_guard(module, guard_state, function, arena)?;

    if let Some(cond_val) = constant_bool(arena, cond_expr) {
        let side = if cond_val {
            &if_stmt.true_side
        } else {
            &if_stmt.false_side
        };
        return side.iter().try_fold(state, |s, step| {
            eval_function_loop_statement(module, s, step, ret_id, arena)
        });
    }

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
        |s, step| eval_function_loop_statement(module, s, step, ret_id, arena),
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
        |s, step| eval_function_loop_statement(module, s, step, ret_id, arena),
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

fn eval_function_loop_case(
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
                eval_function_loop_statement(module, s, step, ret_id, arena)
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
                    eval_function_loop_statement(module, s, step, ret_id, arena)
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
            |s, step| eval_function_loop_statement(module, s, step, ret_id, arena),
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

    let guard_state = state.clone();
    let (function, target_node, target_sources) = eval_function_expression(
        module,
        state.function.clone(),
        &case_stmt.case_target,
        None,
        arena,
    )?;
    state = apply_function_loop_continue_guard(module, guard_state, function, arena)?;
    let target = EvaluatedCaseTarget::new(
        &case_stmt.case_target,
        target_node,
        target_sources,
        &state.function.store,
    );
    eval_from_arm(module, state, case_stmt, &target, 0, ret_id, arena)
}

fn eval_function_loop_statement(
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
        Statement::If(if_stmt) => eval_function_loop_if(module, state, if_stmt, ret_id, arena),
        Statement::Case(case_stmt) => {
            eval_function_loop_case(module, state, case_stmt, ret_id, arena)
        }
        Statement::Assign(_)
        | Statement::For(_)
        | Statement::FunctionCall(_)
        | Statement::SystemFunctionCall(_)
        | Statement::Null => {
            let guard_state = state.clone();
            let next_function =
                eval_function_statement(module, state.function, stmt, ret_id, arena)?;
            apply_function_loop_continue_guard(module, guard_state, next_function, arena)
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

/// The bounds of a function `for` loop with their sources and boundaries, then whether the
/// range is inclusive, its step, its step operator and whether it runs in reverse.
fn eval_function_for_range(
    module: &Module,
    bound_store: &mut SymbolicStore<VarId>,
    for_stmt: &ForStatement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<
    (
        SLTLoopBound,
        SLTLoopBound,
        HashSet<VarAtomBase<VarId>>,
        HashSet<VarAtomBase<VarId>>,
        BoundaryMap<VarId>,
        BoundaryMap<VarId>,
        bool,
        usize,
        SLTStepOp,
        bool,
    ),
    ParserError,
> {
    Ok(match &for_stmt.range {
        ForRange::Forward {
            start: range_start,
            end: range_end,
            inclusive,
            step,
        } => {
            let (start, start_sources, start_bounds) =
                eval_for_bound_effectful(module, bound_store, range_start, arena)?;
            let (end, end_sources, end_bounds) =
                eval_for_bound_effectful(module, bound_store, range_end, arena)?;
            (
                start,
                end,
                start_sources,
                end_sources,
                start_bounds,
                end_bounds,
                *inclusive,
                *step,
                SLTStepOp::Add,
                false,
            )
        }
        ForRange::Reverse {
            start: range_start,
            end: range_end,
            inclusive,
            step,
        } => {
            let (start, start_sources, start_bounds) =
                eval_for_bound_effectful(module, bound_store, range_start, arena)?;
            let (end, end_sources, end_bounds) =
                eval_for_bound_effectful(module, bound_store, range_end, arena)?;
            (
                start,
                end,
                start_sources,
                end_sources,
                start_bounds,
                end_bounds,
                *inclusive,
                *step,
                SLTStepOp::Add,
                true,
            )
        }
        ForRange::Stepped {
            start: range_start,
            end: range_end,
            inclusive,
            step,
            op,
        } => {
            let (start, start_sources, start_bounds) =
                eval_for_bound_effectful(module, bound_store, range_start, arena)?;
            let (end, end_sources, end_bounds) =
                eval_for_bound_effectful(module, bound_store, range_end, arena)?;
            let step_op = match op {
                Op::Mul => SLTStepOp::Mul,
                Op::LogicShiftL | Op::ArithShiftL => SLTStepOp::Shl,
                Op::BitOr => SLTStepOp::BitOr,
                Op::BitXor => SLTStepOp::BitXor,
                other => {
                    return Err(ParserError::illegal_context(
                        "for loop step operator",
                        format!("{other:?}"),
                        Some(&for_stmt.token),
                    ));
                }
            };
            (
                start,
                end,
                start_sources,
                end_sources,
                start_bounds,
                end_bounds,
                *inclusive,
                *step,
                step_op,
                false,
            )
        }
    })
}

pub(super) fn eval_function_for(
    module: &Module,
    mut state: FunctionControlState,
    for_stmt: &ForStatement,
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<FunctionControlState, ParserError> {
    let loop_width = resolve_total_width(module, &module.variables[&for_stmt.var_id])?;
    let (start_bound, end_bound) = match &for_stmt.range {
        ForRange::Forward { start, end, .. }
        | ForRange::Reverse { start, end, .. }
        | ForRange::Stepped { start, end, .. } => (start, end),
    };
    let start_status = loop_bound_status(start_bound, loop_width, for_stmt.var_type.signed);
    let end_status = loop_bound_status(end_bound, loop_width, for_stmt.var_type.signed);
    if matches!(
        start_status,
        Some(LoopBoundStatus::OutOfRange | LoopBoundStatus::ExclusiveUpperSentinel)
    ) || matches!(end_status, Some(LoopBoundStatus::OutOfRange))
        || (inclusive_of(&for_stmt.range)
            && end_status == Some(LoopBoundStatus::ExclusiveUpperSentinel))
    {
        return Err(ParserError::illegal_context(
            "for loop bound exceeding i32 loop variable",
            format!("{:?}", for_stmt.var_name),
            Some(&for_stmt.token),
        ));
    }

    let guard_state = state.clone();
    let mut bound_store = state.store.fork();
    let (
        start,
        end,
        start_sources,
        end_sources,
        start_bounds,
        end_bounds,
        inclusive,
        step,
        step_op,
        reverse,
    ) = eval_function_for_range(module, &mut bound_store, for_stmt, arena)?;
    let bound_boundaries = merge_boundaries(start_bounds, end_bounds);
    state = apply_function_guard(
        module,
        guard_state,
        bound_store,
        bound_boundaries,
        bool_node(arena, true)?,
        HashSet::default(),
        arena,
    )?;

    let mut symbolic_store = state.store.fork();
    let mut written_accesses = HashMap::default();
    collect_written_accesses(module, &for_stmt.body, &mut written_accesses)?;
    for (id, accesses) in &written_accesses {
        let width = resolve_total_width(module, &module.variables[id])?;
        let mut loop_store = RangeStore::new(None, width);
        let mut covered = vec![false; width];
        for access in accesses {
            for slot in covered.iter_mut().take(access.msb + 1).skip(access.lsb) {
                *slot = true;
            }
        }
        let original = state
            .store
            .get(id)
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
                super::super::super::range_store_error(
                    "function for-loop state",
                    error,
                    Some(&for_stmt.token),
                )
            })?;
            let (expr, sources) = combine_parts_with_default(*id, access.lsb, parts, arena)?;
            loop_store
                .update(access, Some((expr, sources)))
                .map_err(|error| {
                    super::super::super::range_store_error(
                        "function for-loop state",
                        error,
                        Some(&for_stmt.token),
                    )
                })?;
        }
        symbolic_store.insert(*id, loop_store);
    }
    symbolic_store.insert(for_stmt.var_id, RangeStore::new(None, loop_width));
    let mut loop_range = super::super::super::LoopRangeScope::enter(arena, for_stmt);
    let arena = &mut *loop_range;
    let iter_store_before = symbolic_store.fork();

    let iter_state = for_stmt.body.iter().try_fold(
        FunctionLoopControlState {
            function: FunctionControlState {
                store: symbolic_store,
                boundaries: state.boundaries.clone(),
                live_expr: bool_node(arena, true)?,
                live_sources: HashSet::default(),
            },
            continue_expr: bool_node(arena, true)?,
            continue_sources: HashSet::default(),
        },
        |s, stmt| eval_function_loop_statement(module, s, stmt, ret_id, arena),
    )?;
    let iter_store_after = iter_state.function.store;
    let merged_boundaries = iter_state.function.boundaries;

    let updates = extract_store_updates(&iter_store_before, &iter_store_after, arena)?;
    let folded_updates: Vec<_> = updates
        .iter()
        .map(|(target, expr, _)| SLTForUpdate {
            target: *target,
            expr: *expr,
        })
        .collect();
    let loop_updated_vars: HashSet<_> = folded_updates
        .iter()
        .map(|update| update.target.id)
        .collect();
    let initial_updates: Vec<_> = updates
        .iter()
        .map(|(target, _, _)| {
            let parts = super::super::super::symbolic_parts_or_default(
                &state.store,
                target.id,
                target.access,
                "function for-loop initial state",
                Some(&for_stmt.token),
            )?;
            let (expr, _) = combine_parts_with_default(
                target.id,
                target.access.lsb,
                parts.iter().map(|(value, access)| (value, *access)),
                arena,
            )?;
            Ok(SLTForUpdate {
                target: *target,
                expr,
            })
        })
        .collect::<Result<Vec<_>, ParserError>>()?;

    let mut result_store = state.store.fork();
    let loop_effective_continue = arena.alloc(SLTNode::Binary(
        iter_state.continue_expr,
        BinaryOp::And,
        iter_state.function.live_expr,
    ))?;
    for (target, _expr, sources) in &updates {
        let mut all_sources = start_sources.clone();
        all_sources.extend(end_sources.iter().copied());
        all_sources.extend(
            iter_state
                .continue_sources
                .iter()
                .copied()
                .filter(|src| src.id != for_stmt.var_id && !loop_updated_vars.contains(&src.id)),
        );
        all_sources.extend(
            iter_state
                .function
                .live_sources
                .iter()
                .copied()
                .filter(|src| src.id != for_stmt.var_id && !loop_updated_vars.contains(&src.id)),
        );
        all_sources.extend(
            sources
                .iter()
                .copied()
                .filter(|src| src.id != for_stmt.var_id && !loop_updated_vars.contains(&src.id)),
        );
        all_sources.retain(|src| src.id != target.id);

        let folded_expr = arena.alloc(SLTNode::ForFold {
            loop_var: for_stmt.var_id,
            loop_width,
            loop_signed: for_stmt.var_type.signed,
            start: start.clone(),
            end: end.clone(),
            inclusive,
            step,
            step_op,
            reverse,
            result: SLTForFoldResult::State(*target),
            initials: initial_updates.clone(),
            updates: folded_updates.clone(),
            effects: Vec::new(),
            continue_cond: loop_effective_continue,
        })?;

        let variable = module.variables.get(&target.id).ok_or_else(|| {
            ParserError::illegal_context(
                "function for-loop result state",
                "state variable is absent from the semantic module",
                Some(&for_stmt.token),
            )
        })?;
        let width = resolve_total_width(module, variable)?;
        result_store
            .entry(target.id)
            .or_insert_with(|| RangeStore::new(None, width))
            .update(target.access, Some((folded_expr, all_sources)))
            .map_err(|error| {
                super::super::super::range_store_error(
                    "function for-loop result state",
                    error,
                    Some(&for_stmt.token),
                )
            })?;
    }
    result_store.remove(&for_stmt.var_id);

    let mut next_live_sources = iter_state.continue_sources.clone();
    next_live_sources.extend(iter_state.function.live_sources.iter().copied());
    next_live_sources.extend(start_sources.iter().copied());
    next_live_sources.extend(end_sources.iter().copied());

    let next_live_expr =
        if statement_contains_return(&Statement::For(Box::new(for_stmt.clone())), ret_id) {
            let initial = bool_node(arena, true)?;
            arena.alloc(SLTNode::ForFold {
                loop_var: for_stmt.var_id,
                loop_width,
                loop_signed: for_stmt.var_type.signed,
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
                initials: initial_updates,
                updates: folded_updates,
                effects: Vec::new(),
                continue_cond: loop_effective_continue,
            })?
        } else {
            bool_node(arena, true)?
        };

    apply_function_guard(
        module,
        state,
        result_store,
        merged_boundaries,
        next_live_expr,
        next_live_sources,
        arena,
    )
}
