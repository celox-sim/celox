//! Evaluating a function body to its return value.

use super::*;

mod break_statements;
mod for_statement;
mod validate;
use break_statements::*;
use for_statement::*;
use validate::*;

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
    let ret_parts = range_store
        .get_parts_ref(ret_access)
        .map_err(|error| super::super::range_store_error("function return value", error, None))?;
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
        Statement::Case(case_stmt) => eval_function_case(module, state, case_stmt, ret_id, arena),
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
            let (next_store, next_bounds) = super::super::eval_system_function_call_side_effects(
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
