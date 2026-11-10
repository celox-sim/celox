//! Evaluating a function body to its return value.

use super::*;

pub(in crate::lowering::logic_tree) fn eval_function_body_return(
    module: &Module,
    caller_store: &SymbolicStore<VarId>,
    body: &veryl_analyzer::ir::FunctionBody,
    ret_id: VarId,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<
    (
        (NodeId, HashSet<VarAtomBase<VarId>>),
        BoundaryMap<VarId>,
        SymbolicStore<VarId>,
    ),
    ParserError,
> {
    fn is_whole_var_assign_to(assign: &AssignStatement, var_id: VarId) -> bool {
        assign.dst.len() == 1
            && assign.dst[0].id == var_id
            && assign.dst[0].index.0.is_empty()
            && assign.dst[0].select.0.is_empty()
            && assign.dst[0].select.1.is_none()
    }

    fn statement_contains_return(stmt: &Statement, ret_id: VarId) -> bool {
        match stmt {
            Statement::Assign(assign) => is_whole_var_assign_to(assign, ret_id),
            Statement::If(if_stmt) => {
                if_stmt
                    .true_side
                    .iter()
                    .any(|stmt| statement_contains_return(stmt, ret_id))
                    || if_stmt
                        .false_side
                        .iter()
                        .any(|stmt| statement_contains_return(stmt, ret_id))
            }
            Statement::Case(case_stmt) => {
                case_stmt.arms.iter().any(|arm| {
                    arm.body
                        .iter()
                        .any(|stmt| statement_contains_return(stmt, ret_id))
                }) || case_stmt
                    .default
                    .iter()
                    .any(|stmt| statement_contains_return(stmt, ret_id))
            }
            Statement::For(for_stmt) => for_stmt
                .body
                .iter()
                .any(|stmt| statement_contains_return(stmt, ret_id)),
            Statement::IfReset(if_reset) => {
                if_reset
                    .true_side
                    .iter()
                    .any(|stmt| statement_contains_return(stmt, ret_id))
                    || if_reset
                        .false_side
                        .iter()
                        .any(|stmt| statement_contains_return(stmt, ret_id))
            }
            Statement::SystemFunctionCall(_)
            | Statement::FunctionCall(_)
            | Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => false,
        }
    }

    fn validate_function_body_expression(
        module: &Module,
        expr: &Expression,
    ) -> Result<(), ParserError> {
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::SystemFunctionCall(call) => {
                    validate_function_body_system_function(module, call)
                }
                Factor::Variable(_, _, _, _)
                | Factor::HierVariable(_)
                | Factor::FunctionCall(_)
                | Factor::Value(_)
                | Factor::Anonymous(_)
                | Factor::Unknown(_) => Ok(()),
            },
            Expression::Unary(_, inner, _) => validate_function_body_expression(module, inner),
            Expression::Binary(lhs, _, rhs, _) => {
                validate_function_body_expression(module, lhs)?;
                validate_function_body_expression(module, rhs)
            }
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                validate_function_body_expression(module, cond)?;
                validate_function_body_expression(module, then_expr)?;
                validate_function_body_expression(module, else_expr)
            }
            Expression::Concatenation(items, _) => {
                for (item_expr, repeat_expr) in items {
                    validate_function_body_expression(module, item_expr)?;
                    if let Some(repeat_expr) = repeat_expr {
                        validate_function_body_expression(module, repeat_expr)?;
                    }
                }
                Ok(())
            }
            Expression::ArrayLiteral(items, _) => {
                for item in items {
                    match item {
                        ArrayLiteralItem::Value(item_expr, repeat_expr) => {
                            validate_function_body_expression(module, item_expr)?;
                            if let Some(repeat_expr) = repeat_expr {
                                validate_function_body_expression(module, repeat_expr)?;
                            }
                        }
                        ArrayLiteralItem::Defaul(default_expr) => {
                            validate_function_body_expression(module, default_expr)?;
                        }
                    }
                }
                Ok(())
            }
            Expression::StructConstructor(_, fields, _) => {
                for (_, field_expr) in fields {
                    validate_function_body_expression(module, field_expr)?;
                }
                Ok(())
            }
        }
    }

    fn validate_function_body_system_function(
        module: &Module,
        call: &SystemFunctionCall,
    ) -> Result<(), ParserError> {
        match &call.kind {
            SystemFunctionKind::Bits(input)
            | SystemFunctionKind::Size(input, _)
            | SystemFunctionKind::Clog2(input)
            | SystemFunctionKind::Onehot(input)
            | SystemFunctionKind::Signed(input)
            | SystemFunctionKind::Unsigned(input) => {
                validate_function_body_expression(module, &input.0)
            }
            SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                for arg in args {
                    validate_function_body_expression(module, &arg.0)?;
                }
                Ok(())
            }
            SystemFunctionKind::Assert { cond, args, .. } => {
                validate_function_body_expression(module, &cond.0)?;
                for arg in args {
                    validate_function_body_expression(module, &arg.0)?;
                }
                Ok(())
            }
            SystemFunctionKind::Readmemh(input, _) => {
                validate_function_body_expression(module, &input.0)
            }
            SystemFunctionKind::Finish => Ok(()),
        }
    }

    fn validate_function_body_statement(
        module: &Module,
        stmt: &Statement,
    ) -> Result<(), ParserError> {
        match stmt {
            Statement::Assign(assign) => {
                validate_function_body_expression(module, &assign.expr)?;
                for dst in &assign.dst {
                    for index_expr in &dst.index.0 {
                        validate_function_body_expression(module, index_expr)?;
                    }
                    for select_expr in &dst.select.0 {
                        validate_function_body_expression(module, select_expr)?;
                    }
                    if let Some((_, range_expr)) = &dst.select.1 {
                        validate_function_body_expression(module, range_expr)?;
                    }
                }
                Ok(())
            }
            Statement::If(if_stmt) => {
                validate_function_body_expression(module, &if_stmt.cond)?;
                for stmt in &if_stmt.true_side {
                    validate_function_body_statement(module, stmt)?;
                }
                for stmt in &if_stmt.false_side {
                    validate_function_body_statement(module, stmt)?;
                }
                Ok(())
            }
            Statement::Case(case_stmt) => {
                validate_function_body_expression(module, &case_stmt.case_target)?;
                for arm in &case_stmt.arms {
                    for pattern in &arm.patterns {
                        match pattern {
                            CasePattern::Eq(expr) => {
                                validate_function_body_expression(module, expr)?
                            }
                            CasePattern::Range { lo, hi, .. } => {
                                validate_function_body_expression(module, lo)?;
                                validate_function_body_expression(module, hi)?;
                            }
                        }
                    }
                    for stmt in &arm.body {
                        validate_function_body_statement(module, stmt)?;
                    }
                }
                for stmt in &case_stmt.default {
                    validate_function_body_statement(module, stmt)?;
                }
                Ok(())
            }
            Statement::For(for_stmt) => {
                // The analyzer preserves loop ownership for source breaks;
                // eval_function_for tracks loop exits separately from returns.
                match &for_stmt.range {
                    ForRange::Forward { start, end, .. }
                    | ForRange::Reverse { start, end, .. }
                    | ForRange::Stepped { start, end, .. } => {
                        if let ForBound::Expression(expr) = start {
                            validate_function_body_expression(module, expr)?;
                        }
                        if let ForBound::Expression(expr) = end {
                            validate_function_body_expression(module, expr)?;
                        }
                    }
                }
                for stmt in &for_stmt.body {
                    validate_function_body_statement(module, stmt)?;
                }
                Ok(())
            }
            Statement::IfReset(ir) => Err(ParserError::illegal_context(
                "statement in comb function body",
                format!("{stmt}"),
                Some(&ir.token),
            )),
            Statement::SystemFunctionCall(fc) => validate_function_body_system_function(module, fc),
            Statement::FunctionCall(call) => {
                for expr in call.inputs.values() {
                    validate_function_body_expression(module, expr)?;
                }
                for dsts in call.outputs.values() {
                    for dst in dsts {
                        for index_expr in &dst.index.0 {
                            validate_function_body_expression(module, index_expr)?;
                        }
                        for select_expr in &dst.select.0 {
                            validate_function_body_expression(module, select_expr)?;
                        }
                        if let Some((_, range_expr)) = &dst.select.1 {
                            validate_function_body_expression(module, range_expr)?;
                        }
                    }
                }
                Ok(())
            }
            Statement::TbMethodCall(_)
            | Statement::Unsupported(_)
            | Statement::Break
            | Statement::Null => Ok(()),
        }
    }

    fn function_return_value(
        module: &Module,
        store: &SymbolicStore<VarId>,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<(NodeId, HashSet<VarAtomBase<VarId>>), ParserError> {
        let ret_var = &module.variables[&ret_id];
        let ret_width = resolve_total_width(module, ret_var)?;
        if ret_width == 0 {
            return Err(ParserError::illegal_context(
                "function return value",
                "return variable has zero width",
                None,
            ));
        }
        let ret_access = BitAccess::new(0, ret_width - 1);
        let range_store = store.get(&ret_id).ok_or_else(|| {
            ParserError::internal(
                "function return value",
                "return variable is absent from the symbolic store",
                None,
            )
        })?;
        let ret_parts = range_store.get_parts_ref(ret_access).map_err(|error| {
            super::super::range_store_error("function return value", error, None)
        })?;
        Ok(combine_parts_with_default(ret_id, 0, ret_parts, arena)?)
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
                let merged_store = merge_symbolic_versions(
                    module,
                    &next_store,
                    &state.store,
                    state.live_expr,
                    &state.live_sources,
                    arena,
                )?;
                let merged_live_expr = match constant_bool(arena, next_live_expr) {
                    Some(true) => state.live_expr,
                    Some(false) => bool_node(arena, false)?,
                    None => arena.alloc(SLTNode::Binary(
                        state.live_expr,
                        BinaryOp::And,
                        next_live_expr,
                    ))?,
                };
                let mut merged_live_sources = state.live_sources;
                merged_live_sources.extend(next_live_sources);
                Ok(FunctionControlState {
                    store: merged_store,
                    boundaries: merge_boundaries(state.boundaries, next_boundaries),
                    live_expr: merged_live_expr,
                    live_sources: merged_live_sources,
                })
            }
        }
    }

    fn eval_function_expression(
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

    fn eval_function_condition(
        module: &Module,
        state: FunctionControlState,
        condition: &Expression,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<(FunctionControlState, NodeId, HashSet<VarAtomBase<VarId>>), ParserError> {
        let (state, condition, sources) =
            eval_function_expression(module, state, condition, Some(1), arena)?;
        Ok((state, procedural_condition(arena, condition)?, sources))
    }

    fn eval_function_if(
        module: &Module,
        state: FunctionControlState,
        if_stmt: &IfStatement,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<FunctionControlState, ParserError> {
        let (state, cond_expr, cond_sources) =
            eval_function_condition(module, state, &if_stmt.cond, arena)?;

        if let Some(cond_val) = constant_bool(arena, cond_expr) {
            let side = if cond_val {
                &if_stmt.true_side
            } else {
                &if_stmt.false_side
            };
            return eval_function_statements(module, state, side, ret_id, arena);
        }

        let then_state = eval_function_statements(
            module,
            FunctionControlState {
                store: state.store.fork(),
                boundaries: state.boundaries.clone(),
                live_expr: state.live_expr,
                live_sources: state.live_sources.clone(),
            },
            &if_stmt.true_side,
            ret_id,
            arena,
        )?;
        let else_state = eval_function_statements(
            module,
            FunctionControlState {
                store: state.store,
                boundaries: state.boundaries,
                live_expr: state.live_expr,
                live_sources: state.live_sources,
            },
            &if_stmt.false_side,
            ret_id,
            arena,
        )?;

        let mut live_sources = cond_sources;
        live_sources.extend(then_state.live_sources);
        live_sources.extend(else_state.live_sources);

        Ok(FunctionControlState {
            store: merge_symbolic_versions(
                module,
                &then_state.store,
                &else_state.store,
                cond_expr,
                &live_sources,
                arena,
            )?,
            boundaries: merge_boundaries(then_state.boundaries, else_state.boundaries),
            live_expr: merge_control_expr(
                cond_expr,
                then_state.live_expr,
                else_state.live_expr,
                arena,
            )?,
            live_sources,
        })
    }

    fn eval_function_for(
        module: &Module,
        mut state: FunctionControlState,
        for_stmt: &ForStatement,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<FunctionControlState, ParserError> {
        fn apply_function_loop_continue_guard(
            module: &Module,
            state: FunctionLoopControlState,
            next_function: FunctionControlState,
            arena: &mut SLTNodeArena<VarId>,
        ) -> Result<FunctionLoopControlState, ParserError> {
            let boundaries =
                merge_boundaries(state.function.boundaries.clone(), next_function.boundaries);

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
                state =
                    apply_function_loop_continue_guard(module, guard_state, next_function, arena)?;
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
                Statement::If(if_stmt) => {
                    eval_function_loop_if(module, state, if_stmt, ret_id, arena)
                }
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
        ) = match &for_stmt.range {
            ForRange::Forward {
                start: range_start,
                end: range_end,
                inclusive,
                step,
            } => {
                let (start, start_sources, start_bounds) =
                    eval_for_bound_effectful(module, &mut bound_store, range_start, arena)?;
                let (end, end_sources, end_bounds) =
                    eval_for_bound_effectful(module, &mut bound_store, range_end, arena)?;
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
                    eval_for_bound_effectful(module, &mut bound_store, range_start, arena)?;
                let (end, end_sources, end_bounds) =
                    eval_for_bound_effectful(module, &mut bound_store, range_end, arena)?;
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
                    eval_for_bound_effectful(module, &mut bound_store, range_start, arena)?;
                let (end, end_sources, end_bounds) =
                    eval_for_bound_effectful(module, &mut bound_store, range_end, arena)?;
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
        };
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
                    super::super::range_store_error(
                        "function for-loop state",
                        error,
                        Some(&for_stmt.token),
                    )
                })?;
                let (expr, sources) = combine_parts_with_default(*id, access.lsb, parts, arena)?;
                loop_store
                    .update(access, Some((expr, sources)))
                    .map_err(|error| {
                        super::super::range_store_error(
                            "function for-loop state",
                            error,
                            Some(&for_stmt.token),
                        )
                    })?;
            }
            symbolic_store.insert(*id, loop_store);
        }
        symbolic_store.insert(for_stmt.var_id, RangeStore::new(None, loop_width));
        let mut loop_range = super::super::LoopRangeScope::enter(arena, for_stmt);
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
                let parts = super::super::symbolic_parts_or_default(
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
                iter_state.continue_sources.iter().copied().filter(|src| {
                    src.id != for_stmt.var_id && !loop_updated_vars.contains(&src.id)
                }),
            );
            all_sources.extend(
                iter_state
                    .function
                    .live_sources
                    .iter()
                    .copied()
                    .filter(|src| {
                        src.id != for_stmt.var_id && !loop_updated_vars.contains(&src.id)
                    }),
            );
            all_sources.extend(
                sources.iter().copied().filter(|src| {
                    src.id != for_stmt.var_id && !loop_updated_vars.contains(&src.id)
                }),
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
                    super::super::range_store_error(
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

    fn apply_function_break_guard(
        module: &Module,
        state: FunctionLoopControlState,
        next_function: FunctionControlState,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<FunctionLoopControlState, ParserError> {
        let boundaries =
            merge_boundaries(state.function.boundaries.clone(), next_function.boundaries);

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

        let guarded = apply_function_break_guard(
            module,
            outer_state.clone(),
            executed_state.function,
            arena,
        )?;
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

        let guarded = apply_function_break_guard(
            module,
            outer_state.clone(),
            executed_state.function,
            arena,
        )?;
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

    fn eval_function_unrolled_loop_statements(
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

    fn eval_function_statements(
        module: &Module,
        mut state: FunctionControlState,
        statements: &[Statement],
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<FunctionControlState, ParserError> {
        let mut i = 0;
        while i < statements.len() {
            if matches!(constant_bool(arena, state.live_expr), Some(false)) {
                break;
            }

            if !statement_contains_break(&statements[i]) {
                state = eval_function_statement(module, state, &statements[i], ret_id, arena)?;
                i += 1;
                continue;
            }

            let start = i;
            i += 1;
            while i < statements.len() && statement_contains_break(&statements[i]) {
                i += 1;
            }

            state = eval_function_unrolled_loop_statements(
                module,
                state,
                &statements[start..i],
                ret_id,
                arena,
            )?;
        }

        Ok(state)
    }

    fn eval_function_case(
        module: &Module,
        mut state: FunctionControlState,
        case_stmt: &CaseStatement,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<FunctionControlState, ParserError> {
        fn eval_from_arm(
            module: &Module,
            mut state: FunctionControlState,
            case_stmt: &CaseStatement,
            target: &EvaluatedCaseTarget,
            arm_index: usize,
            ret_id: VarId,
            arena: &mut SLTNodeArena<VarId>,
        ) -> Result<FunctionControlState, ParserError> {
            let Some(arm) = case_stmt.arms.get(arm_index) else {
                return eval_function_statements(module, state, &case_stmt.default, ret_id, arena);
            };

            let mut next_store = state.store.fork();
            let ((cond_expr, cond_sources), cond_bounds) = eval_case_arm_condition_effectful(
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
            let cond_expr = procedural_condition(arena, cond_expr)?;
            let boundaries = state.boundaries.clone();

            if let Some(cond_val) = constant_bool(arena, cond_expr) {
                let state = FunctionControlState {
                    boundaries,
                    ..state
                };
                return if cond_val {
                    eval_function_statements(module, state, &arm.body, ret_id, arena)
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

            let then_state = eval_function_statements(
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
            )?;
            let else_state = eval_from_arm(
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
            )?;

            let mut live_sources = cond_sources;
            live_sources.extend(then_state.live_sources);
            live_sources.extend(else_state.live_sources);

            Ok(FunctionControlState {
                store: merge_symbolic_versions(
                    module,
                    &then_state.store,
                    &else_state.store,
                    cond_expr,
                    &live_sources,
                    arena,
                )?,
                boundaries: merge_boundaries(then_state.boundaries, else_state.boundaries),
                live_expr: merge_control_expr(
                    cond_expr,
                    then_state.live_expr,
                    else_state.live_expr,
                    arena,
                )?,
                live_sources,
            })
        }

        let (next_state, target_node, target_sources) =
            eval_function_expression(module, state, &case_stmt.case_target, None, arena)?;
        state = next_state;
        let target = EvaluatedCaseTarget::new(
            &case_stmt.case_target,
            target_node,
            target_sources,
            &state.store,
        );
        eval_from_arm(module, state, case_stmt, &target, 0, ret_id, arena)
    }

    fn eval_function_statement(
        module: &Module,
        state: FunctionControlState,
        stmt: &Statement,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<FunctionControlState, ParserError> {
        if matches!(constant_bool(arena, state.live_expr), Some(false)) {
            return Ok(state);
        }

        match stmt {
            Statement::Assign(assign) => {
                let guard_state = state.clone();
                let (next_store, next_bounds) =
                    eval_assign(module, state.store, state.boundaries, assign, arena)?;
                let next_live = if is_whole_var_assign_to(assign, ret_id) {
                    bool_node(arena, false)?
                } else {
                    bool_node(arena, true)?
                };
                apply_function_guard(
                    module,
                    guard_state,
                    next_store,
                    next_bounds,
                    next_live,
                    HashSet::default(),
                    arena,
                )
            }
            Statement::If(if_stmt) => eval_function_if(module, state, if_stmt, ret_id, arena),
            Statement::Case(case_stmt) => {
                eval_function_case(module, state, case_stmt, ret_id, arena)
            }
            Statement::For(for_stmt) => eval_function_for(module, state, for_stmt, ret_id, arena),
            Statement::FunctionCall(call) => {
                let guard_state = state.clone();
                let (next_store, next_bounds) = eval_statement_form_function_call(
                    module,
                    state.store,
                    state.boundaries,
                    call,
                    arena,
                )?;
                apply_function_guard(
                    module,
                    guard_state,
                    next_store,
                    next_bounds,
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
            Statement::SystemFunctionCall(call) => {
                let guard_state = state.clone();
                let (next_store, next_bounds) =
                    super::super::eval_system_function_call_side_effects(
                        module,
                        state.store,
                        state.boundaries,
                        call,
                        arena,
                    )?;
                apply_function_guard(
                    module,
                    guard_state,
                    next_store,
                    next_bounds,
                    bool_node(arena, true)?,
                    HashSet::default(),
                    arena,
                )
            }
            Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                Err(ParserError::illegal_context(
                    "statement in comb function body",
                    format!("{stmt}"),
                    None,
                ))
            }
        }
    }

    let body = crate::lowering::function_return::implicit_return_body(body);
    let mut local_store = caller_store.fork();
    let local_bounds = BoundaryMap::default();
    let mut written = HashMap::default();

    for stmt in &body.statements {
        validate_function_body_statement(module, stmt)?;
    }

    collect_written_accesses(module, &body.statements, &mut written)?;
    written.entry(ret_id).or_default();

    for var_id in written.keys() {
        let Some(var) = module.variables.get(var_id) else {
            return Err(ParserError::internal(
                "function local variable",
                format!("unknown variable id: {:?}", var_id),
                None,
            ));
        };
        let width = resolve_total_width(module, var)?;
        local_store.insert(*var_id, RangeStore::new(None, width));
    }

    for arg_id in body.arg_map.values() {
        if let Some(arg_store) = caller_store.get(arg_id) {
            local_store.insert(*arg_id, arg_store.clone());
        }
    }

    let final_state = eval_function_statements(
        module,
        FunctionControlState {
            store: local_store,
            boundaries: local_bounds,
            live_expr: bool_node(arena, true)?,
            live_sources: HashSet::default(),
        },
        &body.statements,
        ret_id,
        arena,
    )?;
    // Paths that fall off the end return the return variable's current
    // value, which the final store already holds for every live path.
    let (ret_expr, ret_sources) = function_return_value(module, &final_state.store, ret_id, arena)?;
    Ok((
        (ret_expr, ret_sources),
        final_state.boundaries,
        final_state.store,
    ))
}
