use super::*;

use crate::{
    context_width::get_context_width,
    parser::{bitaccess::celox_value_from_comptime, case::case_arm_condition_expr},
};
use num_traits::ToPrimitive as _;
use veryl_analyzer::ir::{CasePattern, Type, ValueVariant};

use super::state::{FunctionControlState, FunctionLoopControlState};

pub(super) fn eval_array_literal_expression(
    module: &Module,
    store: &SymbolicStore<VarId>,
    items: &[ArrayLiteralItem],
    expected_width: Option<usize>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let mut parts = Vec::new();
    let mut all_bounds = BoundaryMap::default();
    let mut total_sources = HashSet::default();

    let mut explicit_width = 0usize;
    let mut default_part: Option<(NodeId, usize)> = None;

    for item in items {
        match item {
            ArrayLiteralItem::Value(sub_expr, repeat) => {
                let ((part_expr, part_sources), p_bounds) =
                    eval_expression(module, store, sub_expr, arena, None)?;
                all_bounds = merge_boundaries(all_bounds, p_bounds);
                total_sources.extend(part_sources);

                let width = get_width(part_expr, arena);
                let rep_count = if let Some(rep_expr) = repeat {
                    let Some(rep_count) = eval_constexpr(rep_expr).and_then(|x| x.to_u64()) else {
                        return Err(ParserError::unsupported(
                            43,
                            LoweringPhase::CombLowering,
                            "array literal non-constant repeat",
                            format!("{:?}", rep_expr),
                            Some(&rep_expr.token_range()),
                        ));
                    };
                    rep_count
                } else {
                    1
                };

                for _ in 0..rep_count {
                    parts.push((part_expr, width));
                }
                explicit_width += width * rep_count as usize;
            }
            ArrayLiteralItem::Defaul(default_expr) => {
                if default_part.is_some() {
                    let token = default_expr.token_range();
                    return Err(ParserError::unsupported(
                        43,
                        LoweringPhase::CombLowering,
                        "array literal multiple default",
                        format!("{:?}", items),
                        Some(&token),
                    ));
                }

                let ((part_expr, part_sources), p_bounds) =
                    eval_expression(module, store, default_expr, arena, None)?;
                all_bounds = merge_boundaries(all_bounds, p_bounds);
                total_sources.extend(part_sources);
                let width = get_width(part_expr, arena);
                default_part = Some((part_expr, width));
            }
        }
    }

    if let Some((default_expr, default_width)) = default_part {
        let Some(target_width) = expected_width else {
            let token = items.first().map(|i| i.token_range());
            return Err(ParserError::unsupported(
                43,
                LoweringPhase::CombLowering,
                "array literal default without context width",
                format!("{:?}", items),
                token.as_ref(),
            ));
        };

        if explicit_width > target_width {
            let token = items.first().map(|i| i.token_range());
            return Err(ParserError::unsupported(
                43,
                LoweringPhase::CombLowering,
                "array literal width overflow",
                format!("explicit_width={explicit_width}, target_width={target_width}"),
                token.as_ref(),
            ));
        }

        let remaining = target_width - explicit_width;
        if default_width == 0 || !remaining.is_multiple_of(default_width) {
            return Err(ParserError::unsupported(
                43,
                LoweringPhase::CombLowering,
                "array literal default width mismatch",
                format!(
                    "remaining={remaining}, default_width={default_width}, target_width={target_width}"
                ),
                None,
            ));
        }

        for _ in 0..(remaining / default_width) {
            parts.push((default_expr, default_width));
        }
    }

    Ok((
        (arena.alloc(SLTNode::Concat(parts)), total_sources),
        all_bounds,
    ))
}

pub(super) fn eval_function_body_return(
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

    fn for_range_is_dynamic(range: &ForRange) -> bool {
        match range {
            ForRange::Forward { start, end, .. }
            | ForRange::Reverse { start, end, .. }
            | ForRange::Stepped { start, end, .. } => {
                matches!(start, ForBound::Expression(_)) || matches!(end, ForBound::Expression(_))
            }
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
            | SystemFunctionKind::Size(input)
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
            _ => Err(ParserError::unsupported(
                59,
                LoweringPhase::CombLowering,
                "system function call in comb function body",
                format!("module `{}`: {call}", module.name),
                Some(&call.comptime.token),
            )),
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
                if for_range_is_dynamic(&for_stmt.range)
                    && for_stmt.body.iter().any(statement_contains_break)
                {
                    return Err(ParserError::unsupported(
                        57,
                        LoweringPhase::CombLowering,
                        "break in dynamic function-local for",
                        format!("module `{}`", module.name),
                        Some(&for_stmt.token),
                    ));
                }

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
            ParserError::illegal_context(
                "function return value",
                "return variable is absent from the symbolic store",
                None,
            )
        })?;
        let ret_parts = range_store
            .get_parts(ret_access)
            .map_err(|error| super::range_store_error("function return value", error, None))?;
        Ok(combine_parts_with_default(ret_id, 0, ret_parts, arena))
    }

    fn function_control_target(
        module: &Module,
        ret_id: VarId,
    ) -> Result<VarAtomBase<VarId>, ParserError> {
        let ret_width = resolve_total_width(module, &module.variables[&ret_id])?;
        Ok(VarAtomBase::new(ret_id, ret_width, ret_width))
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
                let merged_store = merge_symbolic_stores(
                    module,
                    &next_store,
                    &state.store,
                    state.live_expr,
                    &state.live_sources,
                    arena,
                )?;
                let merged_live_expr = match constant_bool(arena, next_live_expr) {
                    Some(true) => state.live_expr,
                    Some(false) => bool_node(arena, false),
                    None => arena.alloc(SLTNode::Binary(
                        state.live_expr,
                        BinaryOp::And,
                        next_live_expr,
                    )),
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

    fn eval_function_if(
        module: &Module,
        state: FunctionControlState,
        if_stmt: &IfStatement,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<FunctionControlState, ParserError> {
        let ((cond_expr, cond_sources), cond_bounds) =
            eval_expression(module, &state.store, &if_stmt.cond, arena, Some(1))?;
        let boundaries = merge_boundaries(state.boundaries, cond_bounds);

        if let Some(cond_val) = constant_bool(arena, cond_expr) {
            let side = if cond_val {
                &if_stmt.true_side
            } else {
                &if_stmt.false_side
            };
            return eval_function_statements(
                module,
                FunctionControlState {
                    boundaries,
                    ..state
                },
                side,
                ret_id,
                arena,
            );
        }

        let then_state = eval_function_statements(
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
        )?;
        let else_state = eval_function_statements(
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
        )?;

        let mut live_sources = cond_sources;
        live_sources.extend(then_state.live_sources);
        live_sources.extend(else_state.live_sources);

        Ok(FunctionControlState {
            store: merge_symbolic_stores(
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
            ),
            live_sources,
        })
    }

    fn eval_function_for(
        module: &Module,
        state: FunctionControlState,
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
                let merged_store = merge_symbolic_stores(
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
                        ),
                        live_sources: merged_live_sources,
                    },
                    ..state
                })
            }
        }

        fn eval_function_loop_if(
            module: &Module,
            state: FunctionLoopControlState,
            if_stmt: &IfStatement,
            ret_id: VarId,
            arena: &mut SLTNodeArena<VarId>,
        ) -> Result<FunctionLoopControlState, ParserError> {
            let ((cond_expr, cond_sources), cond_bounds) =
                eval_expression(module, &state.function.store, &if_stmt.cond, arena, Some(1))?;
            let boundaries = merge_boundaries(state.function.boundaries, cond_bounds);

            if let Some(cond_val) = constant_bool(arena, cond_expr) {
                let side = if cond_val {
                    &if_stmt.true_side
                } else {
                    &if_stmt.false_side
                };
                return side.iter().try_fold(
                    FunctionLoopControlState {
                        function: FunctionControlState {
                            boundaries,
                            ..state.function
                        },
                        ..state
                    },
                    |s, step| eval_function_loop_statement(module, s, step, ret_id, arena),
                );
            }

            let then_state = if_stmt.true_side.iter().try_fold(
                FunctionLoopControlState {
                    function: FunctionControlState {
                        store: state.function.store.clone(),
                        boundaries: boundaries.clone(),
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
                        boundaries,
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
                    store: merge_symbolic_stores(
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
                    ),
                    live_sources,
                },
                continue_expr: merge_control_expr(
                    cond_expr,
                    then_state.continue_expr,
                    else_state.continue_expr,
                    arena,
                ),
                continue_sources: merged_sources,
            })
        }

        fn eval_function_loop_case(
            module: &Module,
            state: FunctionLoopControlState,
            case_stmt: &CaseStatement,
            ret_id: VarId,
            arena: &mut SLTNodeArena<VarId>,
        ) -> Result<FunctionLoopControlState, ParserError> {
            fn eval_from_arm(
                module: &Module,
                state: FunctionLoopControlState,
                case_stmt: &CaseStatement,
                arm_index: usize,
                ret_id: VarId,
                arena: &mut SLTNodeArena<VarId>,
            ) -> Result<FunctionLoopControlState, ParserError> {
                let Some(arm) = case_stmt.arms.get(arm_index) else {
                    return case_stmt.default.iter().try_fold(state, |s, step| {
                        eval_function_loop_statement(module, s, step, ret_id, arena)
                    });
                };

                let ((cond_expr, cond_sources), cond_bounds) = eval_expression(
                    module,
                    &state.function.store,
                    &case_arm_condition_expr(&case_stmt.case_target, &arm.patterns),
                    arena,
                    Some(1),
                )?;
                let boundaries = merge_boundaries(state.function.boundaries, cond_bounds);

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
                        eval_from_arm(module, state, case_stmt, arm_index + 1, ret_id, arena)
                    };
                }

                let then_state = arm.body.iter().try_fold(
                    FunctionLoopControlState {
                        function: FunctionControlState {
                            store: state.function.store.clone(),
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
                        store: merge_symbolic_stores(
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
                        ),
                        live_sources,
                    },
                    continue_expr: merge_control_expr(
                        cond_expr,
                        then_state.continue_expr,
                        else_state.continue_expr,
                        arena,
                    ),
                    continue_sources: merged_sources,
                })
            }

            eval_from_arm(module, state, case_stmt, 0, ret_id, arena)
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
                | Statement::Null => {
                    let guard_state = state.clone();
                    let next_function =
                        eval_function_statement(module, state.function, stmt, ret_id, arena)?;
                    apply_function_loop_continue_guard(module, guard_state, next_function, arena)
                }
                Statement::Break => Ok(FunctionLoopControlState {
                    continue_sources: HashSet::default(),
                    continue_expr: bool_node(arena, false),
                    ..state
                }),
                Statement::IfReset(ir) => Err(ParserError::illegal_context(
                    "statement in comb function body",
                    format!("{stmt}"),
                    Some(&ir.token),
                )),
                Statement::SystemFunctionCall(_) => Ok(state),
                Statement::TbMethodCall(_) | Statement::Unsupported(_) => {
                    Err(ParserError::illegal_context(
                        "statement in comb function body",
                        format!("{stmt}"),
                        None,
                    ))
                }
            }
        }

        let Some(loop_width) = for_stmt.var_type.total_width() else {
            return Err(ParserError::unsupported(
                65,
                LoweringPhase::CombLowering,
                "for loop variable width",
                format!("{:?}", for_stmt.var_name),
                Some(&for_stmt.token),
            ));
        };
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

        let mut symbolic_store = state.store.clone();
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
                let parts = original.get_parts(access).map_err(|error| {
                    super::range_store_error(
                        "function for-loop state",
                        error,
                        Some(&for_stmt.token),
                    )
                })?;
                let (expr, sources) = combine_parts_with_default(*id, access.lsb, parts, arena);
                loop_store
                    .update(access, Some((expr, sources)))
                    .map_err(|error| {
                        super::range_store_error(
                            "function for-loop state",
                            error,
                            Some(&for_stmt.token),
                        )
                    })?;
            }
            symbolic_store.insert(*id, loop_store);
        }
        symbolic_store.insert(for_stmt.var_id, RangeStore::new(None, loop_width));
        let iter_store_before = symbolic_store.clone();

        let iter_state = for_stmt.body.iter().try_fold(
            FunctionLoopControlState {
                function: FunctionControlState {
                    store: symbolic_store,
                    boundaries: state.boundaries.clone(),
                    live_expr: bool_node(arena, true),
                    live_sources: HashSet::default(),
                },
                continue_expr: bool_node(arena, true),
                continue_sources: HashSet::default(),
            },
            |s, stmt| eval_function_loop_statement(module, s, stmt, ret_id, arena),
        )?;
        let iter_store_after = iter_state.function.store;
        let mut merged_boundaries = iter_state.function.boundaries;

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
                    eval_for_bound(module, &state.store, range_start, arena)?;
                let (end, end_sources, end_bounds) =
                    eval_for_bound(module, &state.store, range_end, arena)?;
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
                    eval_for_bound(module, &state.store, range_start, arena)?;
                let (end, end_sources, end_bounds) =
                    eval_for_bound(module, &state.store, range_end, arena)?;
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
                    eval_for_bound(module, &state.store, range_start, arena)?;
                let (end, end_sources, end_bounds) =
                    eval_for_bound(module, &state.store, range_end, arena)?;
                let step_op = match op {
                    Op::Mul => SLTStepOp::Mul,
                    Op::LogicShiftL | Op::ArithShiftL => SLTStepOp::Shl,
                    other => {
                        return Err(ParserError::unsupported(
                            65,
                            LoweringPhase::CombLowering,
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

        merged_boundaries = merge_boundaries(merged_boundaries, start_bounds);
        merged_boundaries = merge_boundaries(merged_boundaries, end_bounds);

        let updates = extract_store_updates(&iter_store_before, &iter_store_after, arena);
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
                let range_store = state.store.get(&target.id).ok_or_else(|| {
                    ParserError::illegal_context(
                        "function for-loop initial state",
                        "state variable is absent from the symbolic store",
                        Some(&for_stmt.token),
                    )
                })?;
                let parts = range_store.get_parts(target.access).map_err(|error| {
                    super::range_store_error(
                        "function for-loop initial state",
                        error,
                        Some(&for_stmt.token),
                    )
                })?;
                let (expr, _) =
                    combine_parts_with_default(target.id, target.access.lsb, parts, arena);
                Ok(SLTForUpdate {
                    target: *target,
                    expr,
                })
            })
            .collect::<Result<Vec<_>, ParserError>>()?;

        let mut result_store = state.store.clone();
        let loop_effective_continue = arena.alloc(SLTNode::Binary(
            iter_state.continue_expr,
            BinaryOp::And,
            iter_state.function.live_expr,
        ));
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
                result: *target,
                initials: initial_updates.clone(),
                updates: folded_updates.clone(),
                effects: Vec::new(),
                continue_cond: loop_effective_continue,
            });

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
                    super::range_store_error(
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

        let next_live_expr = if statement_contains_return(&Statement::For(for_stmt.clone()), ret_id)
        {
            let control_target = function_control_target(module, ret_id)?;
            let mut control_initials = initial_updates.clone();
            control_initials.push(SLTForUpdate {
                target: control_target,
                expr: bool_node(arena, true),
            });
            let mut control_updates = folded_updates.clone();
            control_updates.push(SLTForUpdate {
                target: control_target,
                expr: iter_state.function.live_expr,
            });
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
                result: control_target,
                initials: control_initials,
                updates: control_updates,
                effects: Vec::new(),
                continue_cond: iter_state.continue_expr,
            })
        } else {
            bool_node(arena, true)
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
            let merged_store = merge_symbolic_stores(
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
                    ),
                    live_sources: merged_live_sources,
                },
                ..state
            })
        }
    }

    fn eval_function_break_if(
        module: &Module,
        state: FunctionLoopControlState,
        if_stmt: &IfStatement,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<FunctionLoopControlState, ParserError> {
        let outer_state = state.clone();
        let ((cond_expr, cond_sources), cond_bounds) =
            eval_expression(module, &state.function.store, &if_stmt.cond, arena, Some(1))?;
        let boundaries = merge_boundaries(state.function.boundaries, cond_bounds);

        let executed_state = if let Some(cond_val) = constant_bool(arena, cond_expr) {
            let side = if cond_val {
                &if_stmt.true_side
            } else {
                &if_stmt.false_side
            };
            side.iter().try_fold(
                FunctionLoopControlState {
                    function: FunctionControlState {
                        boundaries,
                        ..state.function
                    },
                    ..state
                },
                |s, step| eval_function_break_statement(module, s, step, ret_id, arena),
            )?
        } else {
            let then_state = if_stmt.true_side.iter().try_fold(
                FunctionLoopControlState {
                    function: FunctionControlState {
                        store: state.function.store.clone(),
                        boundaries: boundaries.clone(),
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
                        boundaries,
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
                    store: merge_symbolic_stores(
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
                    ),
                    live_sources,
                },
                continue_expr: merge_control_expr(
                    cond_expr,
                    then_state.continue_expr,
                    else_state.continue_expr,
                    arena,
                ),
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
            Some(false) => bool_node(arena, false),
            None => arena.alloc(SLTNode::Binary(
                outer_state.continue_expr,
                BinaryOp::And,
                executed_state.continue_expr,
            )),
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
        state: FunctionLoopControlState,
        case_stmt: &CaseStatement,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<FunctionLoopControlState, ParserError> {
        fn eval_from_arm(
            module: &Module,
            state: FunctionLoopControlState,
            case_stmt: &CaseStatement,
            arm_index: usize,
            ret_id: VarId,
            arena: &mut SLTNodeArena<VarId>,
        ) -> Result<FunctionLoopControlState, ParserError> {
            let Some(arm) = case_stmt.arms.get(arm_index) else {
                return case_stmt.default.iter().try_fold(state, |s, step| {
                    eval_function_break_statement(module, s, step, ret_id, arena)
                });
            };

            let ((cond_expr, cond_sources), cond_bounds) = eval_expression(
                module,
                &state.function.store,
                &case_arm_condition_expr(&case_stmt.case_target, &arm.patterns),
                arena,
                Some(1),
            )?;
            let boundaries = merge_boundaries(state.function.boundaries, cond_bounds);

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
                    eval_from_arm(module, state, case_stmt, arm_index + 1, ret_id, arena)
                };
            }

            let then_state = arm.body.iter().try_fold(
                FunctionLoopControlState {
                    function: FunctionControlState {
                        store: state.function.store.clone(),
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
                    store: merge_symbolic_stores(
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
                    ),
                    live_sources,
                },
                continue_expr: merge_control_expr(
                    cond_expr,
                    then_state.continue_expr,
                    else_state.continue_expr,
                    arena,
                ),
                continue_sources: merged_sources,
            })
        }

        let outer_state = state.clone();
        let executed_state = eval_from_arm(module, state, case_stmt, 0, ret_id, arena)?;

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
            Some(false) => bool_node(arena, false),
            None => arena.alloc(SLTNode::Binary(
                outer_state.continue_expr,
                BinaryOp::And,
                executed_state.continue_expr,
            )),
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
            | Statement::Null => {
                let guard_state = state.clone();
                let next_function =
                    eval_function_statement(module, state.function, stmt, ret_id, arena)?;
                apply_function_break_guard(module, guard_state, next_function, arena)
            }
            Statement::Break => Ok(FunctionLoopControlState {
                continue_sources: HashSet::default(),
                continue_expr: bool_node(arena, false),
                ..state
            }),
            Statement::IfReset(ir) => Err(ParserError::illegal_context(
                "statement in comb function body",
                format!("{stmt}"),
                Some(&ir.token),
            )),
            Statement::SystemFunctionCall(_) => Ok(state),
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
                continue_expr: bool_node(arena, true),
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
        state: FunctionControlState,
        case_stmt: &CaseStatement,
        ret_id: VarId,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<FunctionControlState, ParserError> {
        fn eval_from_arm(
            module: &Module,
            state: FunctionControlState,
            case_stmt: &CaseStatement,
            arm_index: usize,
            ret_id: VarId,
            arena: &mut SLTNodeArena<VarId>,
        ) -> Result<FunctionControlState, ParserError> {
            let Some(arm) = case_stmt.arms.get(arm_index) else {
                return eval_function_statements(module, state, &case_stmt.default, ret_id, arena);
            };

            let ((cond_expr, cond_sources), cond_bounds) = eval_expression(
                module,
                &state.store,
                &case_arm_condition_expr(&case_stmt.case_target, &arm.patterns),
                arena,
                Some(1),
            )?;
            let boundaries = merge_boundaries(state.boundaries, cond_bounds);

            if let Some(cond_val) = constant_bool(arena, cond_expr) {
                let state = FunctionControlState {
                    boundaries,
                    ..state
                };
                return if cond_val {
                    eval_function_statements(module, state, &arm.body, ret_id, arena)
                } else {
                    eval_from_arm(module, state, case_stmt, arm_index + 1, ret_id, arena)
                };
            }

            let then_state = eval_function_statements(
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
                arm_index + 1,
                ret_id,
                arena,
            )?;

            let mut live_sources = cond_sources;
            live_sources.extend(then_state.live_sources);
            live_sources.extend(else_state.live_sources);

            Ok(FunctionControlState {
                store: merge_symbolic_stores(
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
                ),
                live_sources,
            })
        }

        eval_from_arm(module, state, case_stmt, 0, ret_id, arena)
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
                    bool_node(arena, false)
                } else {
                    bool_node(arena, true)
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
                    LoweringPhase::CombLowering,
                )?;
                apply_function_guard(
                    module,
                    guard_state,
                    next_store,
                    next_bounds,
                    bool_node(arena, true),
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
            Statement::SystemFunctionCall(_) => Ok(state),
            Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                Err(ParserError::illegal_context(
                    "statement in comb function body",
                    format!("{stmt}"),
                    None,
                ))
            }
        }
    }

    let mut local_store = caller_store.clone();
    let local_bounds = BoundaryMap::default();
    let mut written = HashMap::default();

    for stmt in &body.statements {
        validate_function_body_statement(module, stmt)?;
    }

    collect_written_accesses(module, &body.statements, &mut written)?;
    written.entry(ret_id).or_default();

    for var_id in written.keys() {
        let Some(var) = module.variables.get(var_id) else {
            return Err(ParserError::unsupported(
                67,
                LoweringPhase::CombLowering,
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
            live_expr: bool_node(arena, true),
            live_sources: HashSet::default(),
        },
        &body.statements,
        ret_id,
        arena,
    )?;
    if !matches!(constant_bool(arena, final_state.live_expr), Some(false)) {
        return Err(ParserError::unsupported(
            67,
            LoweringPhase::CombLowering,
            "function return expression",
            format!("function return var id: {:?}", ret_id),
            None,
        ));
    }
    let (ret_expr, ret_sources) = function_return_value(module, &final_state.store, ret_id, arena)?;
    Ok((
        (ret_expr, ret_sources),
        final_state.boundaries,
        final_state.store,
    ))
}

fn eval_function_call_expression(
    module: &Module,
    store: &SymbolicStore<VarId>,
    call: &veryl_analyzer::ir::FunctionCall,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    if !call.outputs.is_empty() {
        return Err(ParserError::unsupported(
            60,
            LoweringPhase::CombLowering,
            "function call with output arguments",
            format!("{call}"),
            Some(&call.comptime.token),
        ));
    }

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

    let Some(ret_id) = function_body.ret else {
        return Err(ParserError::illegal_context(
            "void function call in comb expression",
            format!("{call}"),
            Some(&call.comptime.token),
        ));
    };

    let mut local_store = store.clone();
    let mut arg_bounds = BoundaryMap::default();
    for (arg_path, arg_id) in &function_body.arg_map {
        let Some(arg_expr) = call.inputs.get(arg_path) else {
            return Err(ParserError::unsupported(
                61,
                LoweringPhase::CombLowering,
                "function call missing argument",
                format!("{call}"),
                Some(&call.comptime.token),
            ));
        };

        let Some(arg_var) = module.variables.get(arg_id) else {
            return Err(ParserError::unsupported(
                67,
                LoweringPhase::CombLowering,
                "function argument variable",
                format!("unknown arg id: {:?}", arg_id),
                Some(&call.comptime.token),
            ));
        };
        let arg_width = resolve_total_width(module, arg_var)?;
        let ((arg_node, sources), bounds) =
            eval_assignment_expression(module, store, arg_expr, arena, arg_width)?;
        arg_bounds = merge_boundaries(arg_bounds, bounds);
        local_store.insert(
            *arg_id,
            RangeStore::new(Some((arg_node, sources)), arg_width),
        );
    }

    let ((ret_node, ret_sources), ret_bounds, _) =
        eval_function_body_return(module, &local_store, &function_body, ret_id, arena)?;
    Ok((
        (ret_node, ret_sources),
        merge_boundaries(arg_bounds, ret_bounds),
    ))
}

pub fn eval_expression(
    module: &Module,
    store: &SymbolicStore<VarId>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    context_width: Option<usize>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    // Short-circuit: compile-time constant compound expression → emit Constant node.
    // The folded value still participates in its enclosing width context.  Skipping
    // that coercion used to produce mismatched wildcard operands for enum cases and
    // can also lose carry bits when a folded operand is widened by its parent.
    if !matches!(expr, Expression::Term(_)) {
        let ct = expr.comptime();
        if ct.is_const {
            if let Some((celox_value, mask_xz, width, signed)) = celox_value_from_comptime(ct) {
                let node = arena.alloc(SLTNode::Constant(celox_value, mask_xz, width, signed));
                let node = coerce_node_width(arena, node, context_width, signed);
                return Ok(((node, HashSet::default()), BoundaryMap::default()));
            }
        }
    }

    match expr {
        Expression::Term(factor) => eval_factor(module, store, factor, arena, context_width),
        Expression::Binary(lhs, op, rhs, _) => {
            let (lhs_context_width, rhs_context_width) = if matches!(op, Op::As) {
                // `as` cast: LHS inherits target width from RHS type/numeric, RHS is metadata
                let target_width = if let Expression::Term(f) = rhs.as_ref() {
                    if let Factor::Value(v) = f.as_ref() {
                        match &v.value {
                            ValueVariant::Type(ty) => ty.total_width(),
                            ValueVariant::Numeric(n) => n.to_usize(),
                            _ => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                (target_width, None)
            } else if matches!(
                op,
                Op::LogicShiftL | Op::LogicShiftR | Op::ArithShiftL | Op::ArithShiftR | Op::Pow
            ) {
                (get_context_width(lhs, context_width), None)
            } else {
                let context_width = if matches!(
                    op,
                    Op::Less
                        | Op::LessEq
                        | Op::Greater
                        | Op::GreaterEq
                        | Op::Eq
                        | Op::Ne
                        | Op::EqWildcard
                        | Op::NeWildcard
                        | Op::LogicAnd
                        | Op::LogicOr
                ) {
                    None
                } else {
                    context_width
                };
                let lw = get_context_width(lhs, context_width);
                let rw = get_context_width(rhs, context_width);
                let w = lw.and_then(|lw| rw.map(|rw| lw.max(rw)));
                (w, w)
            };

            // `as` cast: use RHS type for context width and signedness.
            if matches!(op, Op::As) {
                let ((l_expr, l_sources), l_bounds) =
                    eval_expression(module, store, lhs, arena, lhs_context_width)?;

                // For RHS, if it's a type or numeric width, we don't evaluate it as an expression.
                let r_bounds = if let Expression::Term(f) = rhs.as_ref() {
                    if let Factor::Value(v) = f.as_ref() {
                        if matches!(v.value, ValueVariant::Type(_) | ValueVariant::Numeric(_)) {
                            BoundaryMap::default()
                        } else {
                            eval_expression(module, store, rhs, arena, rhs_context_width)?.1
                        }
                    } else {
                        eval_expression(module, store, rhs, arena, rhs_context_width)?.1
                    }
                } else {
                    eval_expression(module, store, rhs, arena, rhs_context_width)?.1
                };

                // Extract signedness and width from RHS type/numeric
                let (target_width, target_signed) = match rhs.as_ref() {
                    Expression::Term(f) => match f.as_ref() {
                        Factor::Value(v) => match &v.value {
                            ValueVariant::Type(ty) => (ty.total_width(), ty.signed),
                            ValueVariant::Numeric(n) => (n.to_usize(), false),
                            _ => (None, false),
                        },
                        _ => (None, false),
                    },
                    _ => (None, false),
                };
                let Some(target_width) = target_width else {
                    return Err(ParserError::unsupported(
                        67,
                        LoweringPhase::CombLowering,
                        "as cast target",
                        format!("{:?}", rhs),
                        Some(&rhs.token_range()),
                    ));
                };

                let result_node = coerce_node_width(
                    arena,
                    l_expr,
                    Some(target_width),
                    target_signed || is_signed(module, l_expr, arena),
                );
                return Ok((
                    (result_node, l_sources),
                    merge_boundaries(l_bounds, r_bounds),
                ));
            }
            // `pow`: currently lowered for constant exponent only.
            if matches!(op, Op::Pow) {
                let ((l_expr, l_sources), l_bounds) =
                    eval_expression(module, store, lhs, arena, lhs_context_width)?;
                let Some(exp) = eval_constexpr(rhs).and_then(|x| x.to_u64().map(|v| v as usize))
                else {
                    return Err(ParserError::unsupported(
                        67,
                        LoweringPhase::CombLowering,
                        "pow non-constant exponent",
                        format!("{:?}", rhs),
                        Some(&rhs.token_range()),
                    ));
                };
                let lhs_width = get_width(l_expr, arena);
                let result_node = if exp == 0 {
                    arena.alloc(SLTNode::Constant(
                        BigUint::from(1u8),
                        BigUint::from(0u32),
                        lhs_width,
                        false,
                    ))
                } else {
                    let mut acc = l_expr;
                    for _ in 1..exp {
                        acc = arena.alloc(SLTNode::Binary(acc, BinaryOp::Mul, l_expr));
                    }
                    acc
                };
                return Ok(((result_node, l_sources), l_bounds));
            }
            let ((l_expr, l_sources), l_bounds) =
                eval_expression(module, store, lhs, arena, lhs_context_width)?;
            let ((r_expr, r_sources), r_bounds) =
                eval_expression(module, store, rhs, arena, rhs_context_width)?;

            let mut sources = l_sources;
            sources.extend(r_sources);

            // BitXnor/BitNand/BitNor は既存演算に分解
            let result_node = match op {
                Op::BitXnor => {
                    let xor_node = arena.alloc(SLTNode::Binary(l_expr, BinaryOp::Xor, r_expr));
                    arena.alloc(SLTNode::Unary(UnaryOp::BitNot, xor_node))
                }
                Op::BitNand => {
                    let and_node = arena.alloc(SLTNode::Binary(l_expr, BinaryOp::And, r_expr));
                    arena.alloc(SLTNode::Unary(UnaryOp::BitNot, and_node))
                }
                Op::BitNor => {
                    let or_node = arena.alloc(SLTNode::Binary(l_expr, BinaryOp::Or, r_expr));
                    arena.alloc(SLTNode::Unary(UnaryOp::BitNot, or_node))
                }
                Op::Sub => {
                    let lhs_signed = expression_signed_override(lhs)
                        .unwrap_or_else(|| is_signed(module, l_expr, arena));
                    let rhs_signed = expression_signed_override(rhs)
                        .unwrap_or_else(|| is_signed(module, r_expr, arena));
                    let signed = lhs_signed && rhs_signed;
                    let bin_op = convert_binary_op(op, signed);
                    let sub_node = arena.alloc(SLTNode::Binary(l_expr, bin_op, r_expr));
                    let width = {
                        let lw = get_width(l_expr, arena);
                        let rw = get_width(r_expr, arena);
                        lw.max(rw)
                    };
                    arena.alloc(SLTNode::Slice {
                        expr: sub_node,
                        access: BitAccess::new(0, width - 1),
                    })
                }
                _ => {
                    let lhs_signed = expression_signed_override(lhs)
                        .unwrap_or_else(|| is_signed(module, l_expr, arena));
                    let rhs_signed = expression_signed_override(rhs)
                        .unwrap_or_else(|| is_signed(module, r_expr, arena));
                    let signed = lhs_signed && rhs_signed;
                    let bin_op = if matches!(op, Op::ArithShiftR) {
                        if lhs_signed {
                            BinaryOp::Sar
                        } else {
                            BinaryOp::Shr
                        }
                    } else {
                        convert_binary_op(op, signed)
                    };
                    let res = arena.alloc(SLTNode::Binary(l_expr, bin_op, r_expr));
                    if matches!(
                        op,
                        Op::Less
                            | Op::LessEq
                            | Op::Greater
                            | Op::GreaterEq
                            | Op::Eq
                            | Op::Ne
                            | Op::EqWildcard
                            | Op::NeWildcard
                    ) && context_width.map(|cw| cw != 1).unwrap_or(false)
                    {
                        let width = context_width.unwrap();
                        let zero = arena.alloc(SLTNode::Constant(
                            BigUint::from(0u8),
                            BigUint::from(0u32),
                            width - 1,
                            false,
                        ));
                        arena.alloc(SLTNode::Concat(vec![(zero, width - 1), (res, 1)]))
                    } else if matches!(
                        op,
                        Op::ArithShiftL
                            | Op::ArithShiftR
                            | Op::LogicShiftL
                            | Op::LogicShiftR
                            | Op::Pow
                    ) {
                        let res_width = get_width(res, arena);
                        let width = context_width.unwrap_or(res_width);
                        if res_width > width {
                            arena.alloc(SLTNode::Slice {
                                expr: res,
                                access: BitAccess::new(0, width - 1),
                            })
                        } else if res_width < width {
                            let zero = arena.alloc(SLTNode::Constant(
                                BigUint::from(0u8),
                                BigUint::from(0u32),
                                res_width - 1,
                                false,
                            ));
                            arena.alloc(SLTNode::Concat(vec![
                                (zero, width - res_width),
                                (res, res_width),
                            ]))
                        } else {
                            res
                        }
                    } else {
                        res
                    }
                }
            };

            Ok(((result_node, sources), merge_boundaries(l_bounds, r_bounds)))
        }
        Expression::Concatenation(exprs, _) => {
            let mut parts = Vec::new();
            let mut all_bounds = BoundaryMap::default();
            let mut total_sources = HashSet::default();

            for (sub_expr, repeat) in exprs {
                let ((part_expr, part_sources), p_bounds) =
                    eval_expression(module, store, sub_expr, arena, None)?;
                all_bounds = merge_boundaries(all_bounds, p_bounds);
                let width = get_width(part_expr, arena);

                total_sources.extend(part_sources);

                let rep_count = if let Some(rep_expr) = repeat {
                    let v = eval_constexpr(rep_expr);
                    v.ok_or_else(|| {
                        ParserError::unsupported(
                            67,
                            LoweringPhase::CombLowering,
                            "concatenation non-constant repeat",
                            format!("{:?}", rep_expr),
                            Some(&rep_expr.token_range()),
                        )
                    })?
                    .iter_u64_digits()
                    .next()
                    .unwrap()
                } else {
                    1
                };
                for _ in 0..rep_count {
                    parts.push((part_expr, width));
                }
            }
            Ok((
                (arena.alloc(SLTNode::Concat(parts)), total_sources),
                all_bounds,
            ))
        }
        Expression::Unary(op, expr, _) => {
            let ((expr, sources), bounds) = eval_expression(module, store, expr, arena, None)?;
            // Reduction Nand/Nor/Xnor は既存のリダクション + Not に分解
            let result_node = match op {
                Op::BitNand => {
                    let and_node = arena.alloc(SLTNode::Unary(UnaryOp::And, expr));
                    arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, and_node))
                }
                Op::BitNor => {
                    let or_node = arena.alloc(SLTNode::Unary(UnaryOp::Or, expr));
                    arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, or_node))
                }
                Op::BitXnor => {
                    let xor_node = arena.alloc(SLTNode::Unary(UnaryOp::Xor, expr));
                    arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, xor_node))
                }
                _ => arena.alloc(SLTNode::Unary(convert_unary_op(op), expr)),
            };
            Ok(((result_node, sources), bounds))
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            let ((cond_expr, cond_sources), cond_bounds) =
                eval_expression(module, store, cond, arena, context_width)?;
            let ((then_expr, then_sources), then_bounds) =
                eval_expression(module, store, then_expr, arena, context_width)?;
            let ((else_expr, else_sources), else_bounds) =
                eval_expression(module, store, else_expr, arena, context_width)?;

            let mut sources = cond_sources;
            sources.extend(then_sources);
            sources.extend(else_sources);

            Ok((
                (
                    arena.alloc(SLTNode::Mux {
                        cond: cond_expr,
                        then_expr,
                        else_expr,
                    }),
                    sources,
                ),
                merge_boundaries(cond_bounds, merge_boundaries(then_bounds, else_bounds)),
            ))
        }
        Expression::StructConstructor(ty, fields, _) => {
            let mut parts = Vec::new();
            let mut all_bounds = BoundaryMap::default();
            let mut total_sources = HashSet::default();

            for (name, field_expr) in fields {
                let ((mut part_expr, part_sources), p_bounds) =
                    eval_expression(module, store, field_expr, arena, context_width)?;
                all_bounds = merge_boundaries(all_bounds, p_bounds);
                total_sources.extend(part_sources);

                let Some(member_type) = ty.get_member_type(*name) else {
                    return Err(ParserError::unsupported(
                        67,
                        LoweringPhase::CombLowering,
                        "struct constructor member",
                        format!("unknown member: {:?} in {:?}", name, ty),
                        Some(&field_expr.token_range()),
                    ));
                };
                let Some(member_width) = member_type.total_width() else {
                    return Err(ParserError::unsupported(
                        67,
                        LoweringPhase::CombLowering,
                        "struct constructor member width",
                        format!("member: {:?}, type: {:?}", name, member_type),
                        Some(&field_expr.token_range()),
                    ));
                };

                let part_width = get_width(part_expr, arena);
                if part_width > member_width {
                    part_expr = arena.alloc(SLTNode::Slice {
                        expr: part_expr,
                        access: BitAccess::new(0, member_width - 1),
                    });
                } else if part_width < member_width {
                    let pad_width = member_width - part_width;
                    let pad = arena.alloc(SLTNode::Constant(
                        BigUint::from(0u8),
                        BigUint::from(0u32),
                        pad_width,
                        false,
                    ));
                    part_expr = arena.alloc(SLTNode::Concat(vec![
                        (pad, pad_width),
                        (part_expr, part_width),
                    ]));
                }

                parts.push((part_expr, member_width));
            }

            Ok((
                (arena.alloc(SLTNode::Concat(parts)), total_sources),
                all_bounds,
            ))
        }
        Expression::ArrayLiteral(items, _) => {
            eval_array_literal_expression(module, store, items, None, arena)
        }
    }
}

/// Evaluate an expression in an assignment-like width context and guarantee
/// that the returned root has exactly `target_width` bits.
///
/// Passing the width into [`eval_expression`] is necessary for operations whose
/// operands inherit the surrounding width (for example an addition connected
/// to a wider instance port).  The final coercion is still required because
/// self-determined expressions and folded compound constants do not all consume
/// that context internally.
pub fn eval_assignment_expression(
    module: &Module,
    store: &SymbolicStore<VarId>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    target_width: usize,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    if target_width == 0 {
        return Err(ParserError::illegal_context(
            "assignment expression",
            "target width must be nonzero",
            Some(&expr.token_range()),
        ));
    }

    let ((node, sources), boundaries) =
        eval_expression(module, store, expr, arena, Some(target_width))?;
    let source_width = get_width(node, arena);
    if source_width == 0 {
        return Err(ParserError::illegal_context(
            "assignment expression",
            "a zero-width expression cannot be assigned",
            Some(&expr.token_range()),
        ));
    }

    // The RHS expression controls extension.  The destination type does not
    // turn an unsigned value into a sign-extended one.  Explicit casts and the
    // signed/unsigned system functions are retained by the override helper.
    let signed = expression_signed_override(expr).unwrap_or(expr.comptime().expr_context.signed);
    let node = coerce_node_width(arena, node, Some(target_width), signed);
    Ok(((node, sources), boundaries))
}

fn eval_factor(
    module: &Module,
    store: &SymbolicStore<VarId>,
    factor: &Factor,
    arena: &mut SLTNodeArena<VarId>,
    context_width: Option<usize>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    match factor {
        Factor::Variable(var_id, index, select, comptime) => {
            // Compile-time constant (e.g. genvar inside generate block): emit a
            // constant node directly instead of loading from memory.
            // Also handles constant[const_index] (e.g. IDX[p] in generate loops).
            if comptime.is_const {
                let is_bare = index.0.is_empty() && select.0.is_empty() && select.1.is_none();
                let is_static_sel =
                    !is_bare && crate::parser::bitaccess::is_static_access(index, select);

                if (is_bare || is_static_sel)
                    && let Some((celox_value, mask_xz, full_width, signed)) =
                        celox_value_from_comptime(comptime)
                {
                    let (val, mask, width) = if is_bare {
                        (celox_value, mask_xz, full_width)
                    } else {
                        // Evaluate the static bit-select on the constant value
                        let access = eval_var_select(module, *var_id, index, select)?;
                        let extracted_width = access.msb - access.lsb + 1;
                        let extracted_val = (&celox_value >> access.lsb)
                            & ((BigUint::from(1u64) << extracted_width) - BigUint::from(1u64));
                        let extracted_mask = (&mask_xz >> access.lsb)
                            & ((BigUint::from(1u64) << extracted_width) - BigUint::from(1u64));
                        (extracted_val, extracted_mask, extracted_width)
                    };

                    let expr = arena.alloc(SLTNode::Constant(val, mask, width, signed && is_bare));
                    let expr = coerce_node_width(arena, expr, context_width, signed && is_bare);
                    return Ok(((expr, HashSet::default()), BoundaryMap::default()));
                }
            }

            let is_static_access = crate::parser::bitaccess::is_static_access(index, select);
            if is_static_access {
                let access = eval_var_select(module, *var_id, index, select)?;

                let mut bounds = BoundaryMap::default();
                let access_end = access.msb.checked_add(1).ok_or_else(|| {
                    ParserError::illegal_context(
                        "static variable read",
                        "source boundary overflows usize",
                        Some(&comptime.token),
                    )
                })?;
                let var_bounds = bounds.entry(*var_id).or_default();
                var_bounds.insert(access.lsb);
                var_bounds.insert(access_end);

                let range_store = store.get(var_id).ok_or_else(|| {
                    ParserError::illegal_context(
                        "static variable read",
                        "source variable is absent from the symbolic store",
                        Some(&comptime.token),
                    )
                })?;
                let parts = range_store.get_parts(access).map_err(|error| {
                    super::range_store_error("static variable read", error, Some(&comptime.token))
                })?;
                // Check if any part of the requested access is unassigned (None)
                // If so, we must depend on the variable's previous value (input).
                // If all parts are Some(...), we only depend on the sources of those expressions.
                let has_unassigned = parts.iter().any(|(val, _)| val.is_none());
                let (expr, mut sources) =
                    combine_parts_with_default(*var_id, access.lsb, parts, arena);
                if has_unassigned {
                    sources.insert(VarAtomBase::new(*var_id, access.lsb, access.msb));
                }
                let expr =
                    coerce_node_width(arena, expr, context_width, is_signed(module, expr, arena));
                Ok(((expr, sources), bounds))
            } else {
                let mut all_sources = HashSet::default();

                let var = &module.variables[var_id];
                let width = resolve_total_width(module, var)?;
                if width == 0 {
                    return Err(ParserError::illegal_context(
                        "dynamic variable read",
                        "source variable has zero width",
                        Some(&comptime.token),
                    ));
                }
                let DynamicSelectOffset {
                    node: offset_node,
                    indices: dynamic_indices,
                    sources: offset_sources,
                    boundaries: all_bounds,
                } = super::eval_dynamic_select_offset(
                    module,
                    store,
                    *var_id,
                    index,
                    select,
                    arena,
                    Some(&comptime.token),
                )?;
                all_sources.extend(offset_sources);

                // 2. Check SymbolicStore to determine if "already written"
                let range_store = store.get(var_id).ok_or_else(|| {
                    ParserError::illegal_context(
                        "dynamic variable read",
                        "source variable is absent from the symbolic store",
                        Some(&comptime.token),
                    )
                })?;
                let access_full = BitAccess::new(0, width - 1);
                let parts = range_store.get_parts(access_full).map_err(|error| {
                    super::range_store_error("dynamic variable read", error, Some(&comptime.token))
                })?;
                let is_unmodified = parts.iter().all(|(val, _)| val.is_none());

                let element_width =
                    crate::parser::bitaccess::get_access_width(module, *var_id, index, select)?;
                if element_width == 0 || element_width > width {
                    return Err(ParserError::illegal_context(
                        "dynamic variable read",
                        format!("selected width {element_width} must be in 1..={width}"),
                        Some(&comptime.token),
                    ));
                }

                let extracted_expr = if is_unmodified {
                    // --- Code for the approach of aligning at load time ---
                    // Keep the SLT input footprint conservative for dependency analysis.
                    // The SIR lowerer recognizes the following Slice(Input(dynamic)) shape
                    // and emits a narrow dynamic load.
                    let raw_input = arena.alloc(SLTNode::Input {
                        variable: *var_id,
                        signed: module.variables[var_id].r#type.signed,
                        index: dynamic_indices,
                        access: BitAccess::new(0, width - 1),
                    });
                    arena.alloc(SLTNode::Slice {
                        expr: raw_input,
                        access: BitAccess::new(0, element_width - 1),
                    })
                } else {
                    // --- If already written ---
                    // Combine latest values in register and align with Shr
                    let (current_expr, current_sources) =
                        combine_parts_with_default(*var_id, 0, parts, arena);
                    all_sources.extend(current_sources);

                    let shifted =
                        arena.alloc(SLTNode::Binary(current_expr, BinaryOp::Shr, offset_node));
                    arena.alloc(SLTNode::Slice {
                        expr: shifted,
                        access: BitAccess::new(0, element_width - 1),
                    })
                };

                let extracted_expr = coerce_node_width(
                    arena,
                    extracted_expr,
                    context_width,
                    is_signed(module, extracted_expr, arena),
                );

                let prefix_access = eval_var_select(module, *var_id, index, select)?;
                all_sources.insert(VarAtomBase::new(
                    *var_id,
                    prefix_access.lsb,
                    prefix_access.msb,
                ));
                Ok(((extracted_expr, all_sources), all_bounds))
            }
        }
        Factor::Value(v) => {
            let (celox_value, mask_xz, width, signed) = celox_value_from_comptime(v)
                .expect("Factor::Value should always have a numeric value");
            // Fill-literals (`'0`, `'1`, `'x`, `'z`) have width 0 from the
            // analyzer (context-dependent).  Per IEEE 1800-2023 §5.7.1:
            //   "All bits of the unsized value shall be set to the value of
            //    the specified bit. In a self-determined context, it shall
            //    have a width of 1 bit."
            // Use context_width when available; fall back to 1 bit for
            // self-determined contexts.  Without this, a 0-width Constant
            // in the RangeStore causes Mux lowering to produce 0-bit masks
            // that zero out the else-arm.
            //
            // The analyzer stores fill-literals with only bit 0 set in
            // payload and mask_xz (e.g. '1 → payload=1, mask_xz=0).
            // We replicate bit 0 across the full width so '1 → all-ones,
            // 'x → all-X, etc.
            let (celox_value, mask_xz, effective_width) = if width == 0 {
                let ew = context_width.unwrap_or(1);
                let fill_mask = (BigUint::from(1u64) << ew) - BigUint::from(1u64);
                let cv = if celox_value.bit(0) {
                    fill_mask.clone()
                } else {
                    BigUint::from(0u8)
                };
                let mxz = if mask_xz.bit(0) {
                    fill_mask
                } else {
                    BigUint::from(0u8)
                };
                (cv, mxz, ew)
            } else {
                (celox_value, mask_xz, width)
            };
            let node = arena.alloc(SLTNode::Constant(
                celox_value,
                mask_xz,
                effective_width,
                signed,
            ));
            let node = coerce_node_width(arena, node, context_width, signed);
            Ok(((node, HashSet::default()), BoundaryMap::default()))
        }
        Factor::SystemFunctionCall(call) => {
            eval_system_function_call(module, store, call, arena, context_width)
        }
        Factor::FunctionCall(call) => eval_function_call_expression(module, store, call, arena),
        Factor::Anonymous(_) | Factor::Unknown(_) => Err(ParserError::unsupported(
            67,
            LoweringPhase::CombLowering,
            "unresolved factor in comb expression",
            format!("{:?}", factor),
            Some(&factor.token_range()),
        )),
    }
}
pub fn get_width<A: Hash + Eq + Clone>(expr: NodeId, arena: &SLTNodeArena<A>) -> usize {
    match arena.get(expr) {
        SLTNode::Input { access, .. } => access.msb - access.lsb + 1,
        SLTNode::Constant(_, _, width, _) => *width,
        SLTNode::Binary(lhs, op, rhs) => match op {
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::LtU
            | BinaryOp::LtS
            | BinaryOp::LeU
            | BinaryOp::LeS
            | BinaryOp::GtU
            | BinaryOp::GtS
            | BinaryOp::GeU
            | BinaryOp::GeS
            | BinaryOp::LogicAnd
            | BinaryOp::LogicOr
            | BinaryOp::EqWildcard
            | BinaryOp::NeWildcard => 1,
            BinaryOp::Sub => {
                let lw = get_width(*lhs, arena);
                let rw = get_width(*rhs, arena);
                lw.max(rw)
            }
            BinaryOp::Sar | BinaryOp::Shl | BinaryOp::Shr => get_width(*lhs, arena),
            _ => {
                let lw = get_width(*lhs, arena);
                let rw = get_width(*rhs, arena);
                lw.max(rw)
            }
        },
        SLTNode::Unary(op, inner) => match op {
            UnaryOp::LogicNot => 1,
            UnaryOp::And | UnaryOp::Or | UnaryOp::Xor => 1,
            _ => get_width(*inner, arena),
        },
        SLTNode::Mux {
            then_expr,
            else_expr,
            ..
        } => get_width(*then_expr, arena).max(get_width(*else_expr, arena)),
        SLTNode::ForFold { result, .. } => result.access.msb - result.access.lsb + 1,
        SLTNode::Concat(parts) => parts.iter().map(|(_, w)| *w).sum(),
        SLTNode::Slice { access, .. } => access.msb - access.lsb + 1,
    }
}

pub(super) fn merge_boundaries(
    mut base: BoundaryMap<VarId>,
    other: BoundaryMap<VarId>,
) -> BoundaryMap<VarId> {
    for (id, bits) in other {
        base.entry(id).or_default().extend(bits);
    }
    base
}

pub(crate) fn coerce_node_width<A: Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    expr: NodeId,
    target_width: Option<usize>,
    sign_extend: bool,
) -> NodeId {
    let Some(target_width) = target_width else {
        return expr;
    };
    let expr_width = get_width(expr, arena);
    if expr_width < target_width {
        let pad_width = target_width - expr_width;
        let pad = if sign_extend {
            let msb_slice = arena.alloc(SLTNode::Slice {
                expr,
                access: BitAccess::new(expr_width - 1, expr_width - 1),
            });
            let pad = if pad_width == 1 {
                msb_slice
            } else {
                arena.alloc(SLTNode::Concat(
                    std::iter::repeat_n((msb_slice, 1), pad_width).collect(),
                ))
            };
            (pad, pad_width)
        } else {
            let zero = arena.alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u32),
                pad_width,
                false,
            ));
            (zero, pad_width)
        };
        arena.alloc(SLTNode::Concat(vec![pad, (expr, expr_width)]))
    } else if expr_width > target_width {
        arena.alloc(SLTNode::Slice {
            expr,
            access: BitAccess::new(0, target_width - 1),
        })
    } else {
        expr
    }
}

fn eval_system_function_call(
    module: &Module,
    store: &SymbolicStore<VarId>,
    call: &SystemFunctionCall,
    arena: &mut SLTNodeArena<VarId>,
    context_width: Option<usize>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    match &call.kind {
        SystemFunctionKind::Bits(input) => {
            let width = system_function_input_bits_width(module, store, &input.0, arena);
            let result = arena.alloc(SLTNode::Constant(
                BigUint::from(width),
                BigUint::from(0u8),
                32,
                false,
            ));
            Ok(((result, HashSet::default()), HashMap::default()))
        }
        SystemFunctionKind::Size(input) => {
            let size = system_function_input_size(module, store, &input.0, arena);
            let result = arena.alloc(SLTNode::Constant(
                BigUint::from(size),
                BigUint::from(0u8),
                32,
                false,
            ));
            Ok(((result, HashSet::default()), HashMap::default()))
        }
        SystemFunctionKind::Clog2(input) => {
            let ((arg, sources), bounds) = eval_expression(module, store, &input.0, arena, None)?;
            let width = get_width(arg, arena);
            let mut result = arena.alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                32,
                false,
            ));
            for k in 1..=width {
                let threshold = arena.alloc(SLTNode::Constant(
                    BigUint::from(1u8) << (k - 1),
                    BigUint::from(0u8),
                    width,
                    false,
                ));
                let cond = arena.alloc(SLTNode::Binary(arg, BinaryOp::GtU, threshold));
                let value = arena.alloc(SLTNode::Constant(
                    BigUint::from(k),
                    BigUint::from(0u8),
                    32,
                    false,
                ));
                result = arena.alloc(SLTNode::Mux {
                    cond,
                    then_expr: value,
                    else_expr: result,
                });
            }
            Ok(((result, sources), bounds))
        }
        SystemFunctionKind::Onehot(input) => {
            let ((arg, sources), bounds) = eval_expression(module, store, &input.0, arena, None)?;
            let width = get_width(arg, arena);
            let zero = arena.alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                width,
                false,
            ));
            let one = arena.alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                width,
                false,
            ));
            let arg_minus_one = arena.alloc(SLTNode::Binary(arg, BinaryOp::Sub, one));
            let overlap = arena.alloc(SLTNode::Binary(arg, BinaryOp::And, arg_minus_one));
            let non_zero = arena.alloc(SLTNode::Binary(arg, BinaryOp::Ne, zero));
            let no_overlap = arena.alloc(SLTNode::Binary(overlap, BinaryOp::Eq, zero));
            let result = arena.alloc(SLTNode::Binary(non_zero, BinaryOp::LogicAnd, no_overlap));
            Ok(((result, sources), bounds))
        }
        SystemFunctionKind::Signed(input) | SystemFunctionKind::Unsigned(input) => {
            let ((arg, sources), bounds) = eval_expression(module, store, &input.0, arena, None)?;
            let signed = matches!(call.kind, SystemFunctionKind::Signed(_));
            Ok((
                (
                    coerce_node_width(arena, arg, context_width, signed),
                    sources,
                ),
                bounds,
            ))
        }
        _ => Err(ParserError::unsupported(
            59,
            LoweringPhase::CombLowering,
            "system function call in comb expression",
            format!("module `{}`: {call}", module.name),
            Some(&call.comptime.token),
        )),
    }
}

fn system_function_type_bits_width(ty: &Type) -> Option<usize> {
    ty.total_width()
        .map(|width| width * ty.total_array().unwrap_or(1))
}

fn system_function_type_size(ty: &Type) -> Option<usize> {
    if let Some(size) = ty.array.first() {
        *size
    } else if let Some(size) = ty.width_expr().first().and_then(|expr| expr.numeric()) {
        Some(size)
    } else if let Some(size) = ty.width().first() {
        *size
    } else {
        ty.total_width()
    }
}

fn system_function_input_bits_width(
    module: &Module,
    store: &SymbolicStore<VarId>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
) -> usize {
    let comptime = expr.comptime();
    match &comptime.value {
        ValueVariant::Type(ty) => system_function_type_bits_width(ty).unwrap_or(0),
        _ => system_function_type_bits_width(&comptime.r#type).unwrap_or_else(|| {
            eval_expression(module, store, expr, arena, None)
                .map(|((node, _), _)| get_width(node, arena))
                .unwrap_or(0)
        }),
    }
}

fn system_function_input_size(
    module: &Module,
    store: &SymbolicStore<VarId>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
) -> usize {
    let comptime = expr.comptime();
    match &comptime.value {
        ValueVariant::Type(ty) => system_function_type_size(ty).unwrap_or(0),
        _ => system_function_type_size(&comptime.r#type).unwrap_or_else(|| {
            eval_expression(module, store, expr, arena, None)
                .map(|((node, _), _)| get_width(node, arena))
                .unwrap_or(0)
        }),
    }
}

pub(super) fn is_signed(module: &Module, expr: NodeId, arena: &SLTNodeArena<VarId>) -> bool {
    match arena.get(expr) {
        SLTNode::Input { variable: id, .. } => module.variables[id].r#type.signed,
        SLTNode::Constant(_, _, _, signed) => *signed,
        SLTNode::Binary(lhs, _, _) => is_signed(module, *lhs, arena),
        SLTNode::Unary(UnaryOp::Minus, _) => true,
        SLTNode::Unary(_, inner) => is_signed(module, *inner, arena),
        SLTNode::Mux { then_expr, .. } => is_signed(module, *then_expr, arena),
        SLTNode::ForFold { result, .. } => module.variables[&result.id].r#type.signed,
        SLTNode::Slice { expr, .. } => is_signed(module, *expr, arena),
        SLTNode::Concat(_) => false,
    }
}

fn expression_signed_override(expr: &Expression) -> Option<bool> {
    match expr {
        Expression::Binary(_, Op::As, rhs, _) => {
            let Expression::Term(factor) = rhs.as_ref() else {
                return None;
            };
            let Factor::Value(comptime) = factor.as_ref() else {
                return None;
            };
            match &comptime.value {
                ValueVariant::Type(ty) => Some(ty.signed),
                ValueVariant::Numeric(_) => Some(false),
                _ => None,
            }
        }
        Expression::Term(factor) => {
            let Factor::SystemFunctionCall(call) = factor.as_ref() else {
                return None;
            };
            match call.kind {
                SystemFunctionKind::Signed(_) => Some(true),
                SystemFunctionKind::Unsigned(_) => Some(false),
                _ => None,
            }
        }
        _ => None,
    }
}

pub fn convert_binary_op(op: &Op, use_signed: bool) -> BinaryOp {
    match op {
        Op::Add => BinaryOp::Add,
        Op::Sub => BinaryOp::Sub,
        Op::Mul => BinaryOp::Mul,
        Op::Div => BinaryOp::Div,
        Op::Rem => BinaryOp::Rem,
        Op::BitAnd => BinaryOp::And,
        Op::BitOr => BinaryOp::Or,
        Op::BitXor => BinaryOp::Xor,
        Op::LogicShiftL | Op::ArithShiftL => BinaryOp::Shl,
        Op::LogicShiftR => BinaryOp::Shr,
        Op::ArithShiftR => BinaryOp::Sar,
        Op::Eq => BinaryOp::Eq,
        Op::EqWildcard => BinaryOp::EqWildcard,
        Op::Ne => BinaryOp::Ne,
        Op::NeWildcard => BinaryOp::NeWildcard,
        Op::Less => {
            if use_signed {
                BinaryOp::LtS
            } else {
                BinaryOp::LtU
            }
        }
        Op::LessEq => {
            if use_signed {
                BinaryOp::LeS
            } else {
                BinaryOp::LeU
            }
        }
        Op::Greater => {
            if use_signed {
                BinaryOp::GtS
            } else {
                BinaryOp::GtU
            }
        }
        Op::GreaterEq => {
            if use_signed {
                BinaryOp::GeS
            } else {
                BinaryOp::GeU
            }
        }
        Op::LogicAnd => BinaryOp::LogicAnd,
        Op::LogicOr => BinaryOp::LogicOr,
        // Unary-only operators
        Op::LogicNot | Op::BitNot => {
            unreachable!(
                "unary operator must not be lowered by convert_binary_op: {:?}",
                op
            )
        }
        // Binary-expression nodes lowered by dedicated paths
        Op::BitXnor | Op::BitNand | Op::BitNor => {
            unreachable!(
                "bitwise derived op must be lowered before convert_binary_op: {:?}",
                op
            )
        }
        Op::Ternary => unreachable!("ternary expression must not be lowered by convert_binary_op"),
        Op::Concatenation => {
            unreachable!("concatenation must be lowered by concat-specific path")
        }
        Op::ArrayLiteral => {
            unreachable!("array literal must not be lowered by convert_binary_op")
        }
        Op::Condition => unreachable!("condition node must not be lowered by convert_binary_op"),
        Op::Repeat => unreachable!("repeat node must be lowered by repeat-specific path"),
        // Handled by pre-lowering in eval_expression.
        Op::Pow | Op::As => {
            unreachable!("operator must be pre-lowered before conversion: {:?}", op)
        }
    }
}
pub fn convert_unary_op(op: &Op) -> UnaryOp {
    match op {
        Op::Add => UnaryOp::Ident,
        Op::Sub => UnaryOp::Minus,
        Op::BitNot => UnaryOp::BitNot,
        Op::LogicNot => UnaryOp::LogicNot,
        // リダクション演算子としての使用
        Op::BitAnd => UnaryOp::And,
        Op::BitOr => UnaryOp::Or,
        Op::BitXor => UnaryOp::Xor,
        // Unary form lowered by decomposition before conversion
        Op::BitXnor | Op::BitNand | Op::BitNor => {
            unreachable!(
                "reduction derived op must be lowered before convert_unary_op: {:?}",
                op
            )
        }
        // Binary-only operators
        Op::Pow
        | Op::Div
        | Op::Rem
        | Op::Mul
        | Op::ArithShiftL
        | Op::ArithShiftR
        | Op::LogicShiftL
        | Op::LogicShiftR
        | Op::LessEq
        | Op::GreaterEq
        | Op::Less
        | Op::Greater
        | Op::Eq
        | Op::EqWildcard
        | Op::Ne
        | Op::NeWildcard
        | Op::LogicAnd
        | Op::LogicOr => {
            unreachable!(
                "binary operator must not be lowered by convert_unary_op: {:?}",
                op
            )
        }
        // Node kinds lowered by dedicated paths
        Op::Ternary => unreachable!("ternary expression must not be lowered by convert_unary_op"),
        Op::Concatenation => {
            unreachable!("concatenation must be lowered by concat-specific path")
        }
        Op::ArrayLiteral => {
            unreachable!("array literal must not be lowered by convert_unary_op")
        }
        Op::Condition => unreachable!("condition node must not be lowered by convert_unary_op"),
        Op::Repeat => unreachable!("repeat node must be lowered by repeat-specific path"),
        Op::As => unreachable!("As is binary and must not be lowered by convert_unary_op"),
    }
}
