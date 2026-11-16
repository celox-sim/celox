//! `for` loops: `break`-aware statements, bounds, and folding a loop into `ForFold` nodes.

use super::*;

pub(super) fn const_for_bound_i64(bound: &ForBound) -> Option<i64> {
    match bound {
        ForBound::Const(v, _) => (*v).try_into().ok(),
        ForBound::Expression(expr) => eval_constexpr(expr)?.to_i64(),
    }
}

pub(super) fn apply_loop_continue_guard(
    module: &Module,
    state: LoopControlState,
    next_store: SymbolicStore<VarId>,
    next_boundaries: BoundaryMap<VarId>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<LoopControlState, ParserError> {
    let base_store = state.store.fork();
    let boundaries = merge_boundaries(state.boundaries, next_boundaries);

    if matches!(constant_bool(arena, state.continue_expr), Some(true)) {
        Ok(LoopControlState {
            store: next_store,
            boundaries,
            ..state
        })
    } else {
        let merged_store = merge_symbolic_versions(
            module,
            &next_store,
            &base_store,
            state.continue_expr,
            &state.continue_sources,
            arena,
        )?;
        Ok(LoopControlState {
            store: merged_store,
            boundaries,
            ..state
        })
    }
}

pub(super) fn statement_contains_break(stmt: &Statement) -> bool {
    match stmt {
        Statement::Break => true,
        Statement::If(if_stmt) => {
            if_stmt.true_side.iter().any(statement_contains_break)
                || if_stmt.false_side.iter().any(statement_contains_break)
        }
        Statement::Case(case_stmt) => {
            case_stmt
                .arms
                .iter()
                .any(|arm| arm.body.iter().any(statement_contains_break))
                || case_stmt.default.iter().any(statement_contains_break)
        }
        Statement::For(for_stmt) => for_stmt.body.iter().any(statement_contains_break),
        Statement::IfReset(if_reset) => {
            if_reset.true_side.iter().any(statement_contains_break)
                || if_reset.false_side.iter().any(statement_contains_break)
        }
        Statement::Assign(_)
        | Statement::SystemFunctionCall(_)
        | Statement::FunctionCall(_)
        | Statement::TbMethodCall(_)
        | Statement::Unsupported(_)
        | Statement::Null => false,
    }
}

pub(super) fn eval_loop_statement(
    module: &Module,
    state: LoopControlState,
    stmt: &Statement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<LoopControlState, ParserError> {
    if matches!(constant_bool(arena, state.continue_expr), Some(false)) {
        return Ok(state);
    }

    match stmt {
        Statement::Assign(assign) => {
            let guard_state = state.clone();
            let (next_store, next_boundaries) =
                eval_assign(module, state.store, state.boundaries, assign, arena)?;
            apply_loop_continue_guard(module, guard_state, next_store, next_boundaries, arena)
        }
        Statement::If(if_stmt) => {
            if statement_contains_break(stmt) {
                eval_loop_if(module, state, if_stmt, arena)
            } else {
                let guard_state = state.clone();
                let (next_store, next_boundaries) =
                    eval_if(module, state.store, state.boundaries, if_stmt, arena)?;
                apply_loop_continue_guard(module, guard_state, next_store, next_boundaries, arena)
            }
        }
        Statement::Case(case_stmt) => eval_loop_case(module, state, case_stmt, arena),
        Statement::For(for_stmt) => {
            let guard_state = state.clone();
            let (next_store, next_boundaries) =
                eval_for(module, state.store, state.boundaries, for_stmt, arena)?;
            apply_loop_continue_guard(module, guard_state, next_store, next_boundaries, arena)
        }
        Statement::Break => Ok(LoopControlState {
            continue_expr: bool_node(arena, false)?,
            continue_sources: HashSet::default(),
            ..state
        }),
        Statement::IfReset(ir) => Err(ParserError::illegal_context(
            "statement in always_comb",
            "if_reset".to_string(),
            Some(&ir.token),
        )),
        Statement::SystemFunctionCall(call) => {
            let guard_state = state.clone();
            let (next_store, next_boundaries) = eval_system_function_call_side_effects(
                module,
                state.store,
                state.boundaries,
                call,
                arena,
            )?;
            apply_loop_continue_guard(module, guard_state, next_store, next_boundaries, arena)
        }
        Statement::FunctionCall(fc) => {
            let guard_state = state.clone();
            let (next_store, next_boundaries) = eval_statement_form_function_call(
                module,
                state.store,
                state.boundaries,
                fc,
                arena,
            )?;
            apply_loop_continue_guard(module, guard_state, next_store, next_boundaries, arena)
        }
        Statement::TbMethodCall(_) => Err(ParserError::illegal_context(
            "statement in always_comb",
            "testbench method call".to_string(),
            None,
        )),
        Statement::Unsupported(_) => Err(ParserError::illegal_context(
            "statement in always_comb",
            "unsupported statement".to_string(),
            None,
        )),
        Statement::Null => Err(ParserError::illegal_context(
            "statement in always_comb",
            "null".to_string(),
            None,
        )),
    }
}

pub(super) fn eval_loop_case(
    module: &Module,
    mut state: LoopControlState,
    case_stmt: &CaseStatement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<LoopControlState, ParserError> {
    fn eval_from_arm(
        module: &Module,
        mut state: LoopControlState,
        case_stmt: &CaseStatement,
        target: &expr::EvaluatedCaseTarget,
        arm_index: usize,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<LoopControlState, ParserError> {
        let Some(arm) = case_stmt.arms.get(arm_index) else {
            return case_stmt
                .default
                .iter()
                .try_fold(state, |s, step| eval_loop_statement(module, s, step, arena));
        };

        let mut cond_store = state.store.fork();
        let ((cond_expr, cond_sources), cond_bounds) = eval_case_arm_condition_effectful(
            module,
            &mut cond_store,
            &case_stmt.case_target,
            target,
            &arm.patterns,
            arena,
        )?;
        state = apply_loop_continue_guard(module, state, cond_store, cond_bounds, arena)?;
        let cond_expr = procedural_condition(arena, cond_expr)?;
        let boundaries = state.boundaries.clone();

        if let Some(cond_val) = constant_bool(arena, cond_expr) {
            let state = LoopControlState {
                boundaries,
                ..state
            };
            return if cond_val {
                arm.body
                    .iter()
                    .try_fold(state, |s, step| eval_loop_statement(module, s, step, arena))
            } else {
                eval_from_arm(module, state, case_stmt, target, arm_index + 1, arena)
            };
        }

        let then_state = arm.body.iter().try_fold(
            LoopControlState {
                store: state.store.fork(),
                boundaries: boundaries.clone(),
                continue_expr: state.continue_expr,
                continue_sources: state.continue_sources.clone(),
            },
            |s, step| eval_loop_statement(module, s, step, arena),
        )?;
        let else_state = eval_from_arm(
            module,
            LoopControlState {
                store: state.store,
                boundaries,
                continue_expr: state.continue_expr,
                continue_sources: state.continue_sources,
            },
            case_stmt,
            target,
            arm_index + 1,
            arena,
        )?;

        let mut merged_sources = cond_sources;
        merged_sources.extend(then_state.continue_sources);
        merged_sources.extend(else_state.continue_sources);

        Ok(LoopControlState {
            store: merge_symbolic_versions(
                module,
                &then_state.store,
                &else_state.store,
                cond_expr,
                &merged_sources,
                arena,
            )?,
            boundaries: merge_boundaries(then_state.boundaries, else_state.boundaries),
            continue_expr: merge_control_expr(
                cond_expr,
                then_state.continue_expr,
                else_state.continue_expr,
                arena,
            )?,
            continue_sources: merged_sources,
        })
    }

    let mut target_store = state.store.fork();
    let (target, target_boundaries) =
        eval_case_target_effectful(module, &mut target_store, &case_stmt.case_target, arena)?;
    state = apply_loop_continue_guard(module, state, target_store, target_boundaries, arena)?;
    eval_from_arm(module, state, case_stmt, &target, 0, arena)
}

pub(super) fn eval_loop_if(
    module: &Module,
    mut state: LoopControlState,
    stmt: &IfStatement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<LoopControlState, ParserError> {
    let mut cond_store = state.store.fork();
    let ((cond_expr, cond_sources), cond_bounds) =
        eval_expression_effectful(module, &mut cond_store, &stmt.cond, arena, None)?;
    state = apply_loop_continue_guard(module, state, cond_store, cond_bounds, arena)?;
    let cond_expr = procedural_condition(arena, cond_expr)?;
    let boundaries = state.boundaries.clone();

    if let Some(cond_val) = constant_bool(arena, cond_expr) {
        let side = if cond_val {
            &stmt.true_side
        } else {
            &stmt.false_side
        };
        return side.iter().try_fold(
            LoopControlState {
                boundaries,
                ..state
            },
            |s, step| eval_loop_statement(module, s, step, arena),
        );
    }

    let then_state = stmt.true_side.iter().try_fold(
        LoopControlState {
            store: state.store.fork(),
            boundaries: boundaries.clone(),
            continue_expr: state.continue_expr,
            continue_sources: state.continue_sources.clone(),
        },
        |s, step| eval_loop_statement(module, s, step, arena),
    )?;
    let else_state = stmt.false_side.iter().try_fold(
        LoopControlState {
            store: state.store,
            boundaries,
            continue_expr: state.continue_expr,
            continue_sources: state.continue_sources,
        },
        |s, step| eval_loop_statement(module, s, step, arena),
    )?;

    let mut merged_sources = cond_sources;
    merged_sources.extend(then_state.continue_sources);
    merged_sources.extend(else_state.continue_sources);

    Ok(LoopControlState {
        store: merge_symbolic_versions(
            module,
            &then_state.store,
            &else_state.store,
            cond_expr,
            &merged_sources,
            arena,
        )?,
        boundaries: merge_boundaries(then_state.boundaries, else_state.boundaries),
        continue_expr: merge_control_expr(
            cond_expr,
            then_state.continue_expr,
            else_state.continue_expr,
            arena,
        )?,
        continue_sources: merged_sources,
    })
}

pub(super) fn extract_store_updates(
    store_before: &SymbolicStore<VarId>,
    store_after: &SymbolicStore<VarId>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<Vec<(VarAtomBase<VarId>, NodeId, HashSet<VarAtomBase<VarId>>)>, SLTNodeFactsError> {
    let mut updates = Vec::new();

    for id in store_before.differing_keys(store_after) {
        let Some(range_store_after) = store_after.get(id) else {
            continue;
        };
        let Some(range_store_before) = store_before.get(id) else {
            continue;
        };

        for (&lsb, (val_opt, width, origin)) in &range_store_after.ranges {
            if range_store_before.ranges.get(&lsb) == Some(&(val_opt.clone(), *width, *origin)) {
                continue;
            }

            let Some((expr, sources)) = val_opt else {
                continue;
            };

            let msb = lsb + width - 1;
            let rel_lsb = lsb - origin;
            let rel_msb = msb - origin;
            let original_width = get_width(*expr, arena);
            let final_expr = if rel_lsb == 0 && *width == original_width {
                *expr
            } else {
                arena.alloc(SLTNode::Slice {
                    expr: *expr,
                    access: BitAccess::new(rel_lsb, rel_msb),
                })?
            };

            updates.push((VarAtomBase::new(*id, lsb, msb), final_expr, sources.clone()));
        }
    }

    Ok(updates)
}

// Keep the bound's own type separate from the signed induction variable.
// Legacy SLT constants use the induction-variable signedness; an unsigned
// Veryl constant must therefore remain a typed expression.
pub(super) fn constant_for_bound(
    value: usize,
    signed: bool,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<SLTLoopBound, ParserError> {
    if signed {
        Ok(SLTLoopBound::Const(value))
    } else {
        let width = (usize::BITS as usize - value.leading_zeros() as usize).max(32);
        Ok(SLTLoopBound::TypedExpr {
            node: arena.alloc(SLTNode::Constant(
                BigUint::from(value),
                BigUint::from(0u8),
                width,
                false,
            ))?,
            signed: false,
        })
    }
}

pub(super) fn eval_for_bound(
    module: &Module,
    store: &SymbolicStore<VarId>,
    bound: &ForBound,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<
    (
        SLTLoopBound,
        HashSet<VarAtomBase<VarId>>,
        BoundaryMap<VarId>,
    ),
    ParserError,
> {
    match bound {
        ForBound::Const(v, signed) => Ok((
            constant_for_bound(*v, *signed, arena)?,
            HashSet::default(),
            BoundaryMap::default(),
        )),
        ForBound::Expression(expr) => {
            // Veryl emits an `int` induction variable. Its comparison context
            // widens bound arithmetic to at least 32 bits before evaluation.
            let width = crate::context_width::get_expr_width(expr)
                .unwrap_or(0)
                .max(32);
            let ((node, sources), bounds) =
                eval_expression(module, store, expr, arena, Some(width))?;
            Ok((
                SLTLoopBound::TypedExpr {
                    node,
                    signed: crate::context_width::expression_signed(expr),
                },
                sources,
                bounds,
            ))
        }
    }
}

pub(super) fn eval_for_bound_effectful(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    bound: &ForBound,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<
    (
        SLTLoopBound,
        HashSet<VarAtomBase<VarId>>,
        BoundaryMap<VarId>,
    ),
    ParserError,
> {
    match bound {
        ForBound::Const(value, signed) => Ok((
            constant_for_bound(*value, *signed, arena)?,
            HashSet::default(),
            BoundaryMap::default(),
        )),
        ForBound::Expression(expression) => {
            let width = crate::context_width::get_expr_width(expression)
                .unwrap_or(0)
                .max(32);
            let ((node, sources), boundaries) =
                eval_expression_effectful(module, store, expression, arena, Some(width))?;
            Ok((
                SLTLoopBound::TypedExpr {
                    node,
                    signed: crate::context_width::expression_signed(expression),
                },
                sources,
                boundaries,
            ))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LoopBoundStatus {
    FitsLoopType,
    ExclusiveUpperSentinel,
    OutOfRange,
}

pub(super) fn loop_bound_status(
    bound: &ForBound,
    width: usize,
    signed: bool,
) -> Option<LoopBoundStatus> {
    let value = match bound {
        ForBound::Const(v, _) => BigInt::from(*v),
        ForBound::Expression(expr) => {
            if !expr.comptime().is_const {
                return None;
            }
            let value = expr.comptime().get_value().ok()?;
            match value {
                Value::U64(v) => {
                    if v.signed {
                        BigInt::from(v.to_i64()?)
                    } else {
                        BigInt::from(v.to_u64()?)
                    }
                }
                Value::BigUint(v) => {
                    if v.signed {
                        v.to_bigint()?
                    } else {
                        BigInt::from_biguint(Sign::Plus, (*v.payload).clone())
                    }
                }
            }
        }
    };

    if signed {
        let max = (BigInt::from(1u8) << (width.saturating_sub(1))) - BigInt::from(1u8);
        let min = -(BigInt::from(1u8) << (width.saturating_sub(1)));
        Some(if value >= min && value <= max {
            LoopBoundStatus::FitsLoopType
        } else if value == max + BigInt::from(1u8) {
            LoopBoundStatus::ExclusiveUpperSentinel
        } else {
            LoopBoundStatus::OutOfRange
        })
    } else {
        let max = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        let max = BigInt::from_biguint(Sign::Plus, max);
        Some(if value.sign() != Sign::Minus && value <= max {
            LoopBoundStatus::FitsLoopType
        } else if value == max + BigInt::from(1u8) {
            LoopBoundStatus::ExclusiveUpperSentinel
        } else {
            LoopBoundStatus::OutOfRange
        })
    }
}

pub(super) fn inclusive_of(range: &ForRange) -> bool {
    match range {
        ForRange::Forward { inclusive, .. }
        | ForRange::Reverse { inclusive, .. }
        | ForRange::Stepped { inclusive, .. } => *inclusive,
    }
}

pub(super) fn eval_for(
    module: &Module,
    store: SymbolicStore<VarId>,
    boundaries: HashMap<VarId, BTreeSet<usize>>,
    for_stmt: &ForStatement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, HashMap<VarId, BTreeSet<usize>>), ParserError> {
    eval_for_with_effects(module, store, boundaries, for_stmt, arena, &[])
        .map(|(store, boundaries, _)| (store, boundaries))
}

pub(super) fn eval_for_with_effects(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    mut boundaries: HashMap<VarId, BTreeSet<usize>>,
    for_stmt: &ForStatement,
    arena: &mut SLTNodeArena<VarId>,
    effects: &[SLTForEffect],
) -> Result<
    (
        SymbolicStore<VarId>,
        HashMap<VarId, BTreeSet<usize>>,
        Option<NodeId>,
    ),
    ParserError,
> {
    let loop_width = resolve_total_width(module, &module.variables[&for_stmt.var_id])?;
    let (start_bound, end_bound) = match &for_stmt.range {
        ForRange::Forward { start, end, .. }
        | ForRange::Reverse { start, end, .. }
        | ForRange::Stepped { start, end, .. } => (start, end),
    };
    let start_status = loop_bound_status(start_bound, loop_width, for_stmt.var_type.signed);
    let end_status = loop_bound_status(end_bound, loop_width, for_stmt.var_type.signed);
    // Keep the exclusive upper sentinel used for full-range iteration such as
    // `0..256` on an 8-bit loop variable, but reject bounds that would
    // actually force the loop variable outside its representable range.
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

    // Bounds are ordinary runtime expressions. Evaluate them once, from left
    // to right, before constructing the loop state so output-argument writes
    // are visible both inside the loop and after it.
    let (start, end, start_sources, end_sources, inclusive, step, step_op, reverse) =
        match &for_stmt.range {
            ForRange::Forward {
                start: range_start,
                end: range_end,
                inclusive,
                step,
            } => {
                let (start, start_sources, start_bounds) =
                    eval_for_bound_effectful(module, &mut store, range_start, arena)?;
                boundaries = merge_boundaries(boundaries, start_bounds);
                let (end, end_sources, end_bounds) =
                    eval_for_bound_effectful(module, &mut store, range_end, arena)?;
                boundaries = merge_boundaries(boundaries, end_bounds);
                (
                    start,
                    end,
                    start_sources,
                    end_sources,
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
                    eval_for_bound_effectful(module, &mut store, range_start, arena)?;
                boundaries = merge_boundaries(boundaries, start_bounds);
                let (end, end_sources, end_bounds) =
                    eval_for_bound_effectful(module, &mut store, range_end, arena)?;
                boundaries = merge_boundaries(boundaries, end_bounds);
                (
                    start,
                    end,
                    start_sources,
                    end_sources,
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
                    eval_for_bound_effectful(module, &mut store, range_start, arena)?;
                boundaries = merge_boundaries(boundaries, start_bounds);
                let (end, end_sources, end_bounds) =
                    eval_for_bound_effectful(module, &mut store, range_end, arena)?;
                boundaries = merge_boundaries(boundaries, end_bounds);
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
                    *inclusive,
                    *step,
                    step_op,
                    false,
                )
            }
        };

    let mut symbolic_store = store.fork();
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
            let parts = original
                .get_parts_ref(access)
                .map_err(|error| range_store_error("for-loop state", error, None))?;
            let (expr, sources) = combine_parts_with_default(id, access.lsb, parts, arena)?;
            loop_store
                .update(access, Some((expr, sources)))
                .map_err(|error| range_store_error("for-loop state", error, None))?;
        }
        symbolic_store.insert(id, loop_store);
    }
    symbolic_store.insert(for_stmt.var_id, RangeStore::new(None, loop_width));
    let mut loop_range = LoopRangeScope::enter(arena, for_stmt);
    let arena = &mut *loop_range;
    let iter_store_before = symbolic_store.fork();

    let loop_state = for_stmt.body.iter().try_fold(
        LoopControlState {
            store: symbolic_store,
            boundaries,
            continue_expr: bool_node(arena, true)?,
            continue_sources: HashSet::default(),
        },
        |state, stmt| eval_loop_statement(module, state, stmt, arena),
    )?;
    let iter_store_after = loop_state.store;
    let merged_boundaries = loop_state.boundaries;

    let updates = extract_store_updates(&iter_store_before, &iter_store_after, arena)?;
    if updates.is_empty() && effects.is_empty() {
        let mut store = store;
        store.remove(&for_stmt.var_id);
        return Ok((store, merged_boundaries, None));
    }

    let folded_updates: Vec<_> = if updates.is_empty() {
        let one = bool_node(arena, true)?;
        vec![SLTForUpdate {
            target: VarAtomBase::new(for_stmt.var_id, 0, loop_width - 1),
            expr: one,
        }]
    } else {
        updates
            .iter()
            .map(|(target, expr, _)| SLTForUpdate {
                target: *target,
                expr: *expr,
            })
            .collect()
    };
    let loop_updated_vars: HashSet<_> = folded_updates
        .iter()
        .map(|update| update.target.id)
        .collect();
    let (initial_updates, initial_sources): (Vec<_>, HashSet<_>) = if updates.is_empty() {
        let one = bool_node(arena, true)?;
        (
            vec![SLTForUpdate {
                target: VarAtomBase::new(for_stmt.var_id, 0, loop_width - 1),
                expr: one,
            }],
            HashSet::default(),
        )
    } else {
        let mut initial_sources = HashSet::default();
        let initial_updates = updates
            .iter()
            .map(|(target, _, _)| {
                let parts = symbolic_parts_or_default(
                    &store,
                    target.id,
                    target.access,
                    "for-loop initial state",
                    Some(&for_stmt.token),
                )?;
                let (expr, sources) = combine_parts_with_default(
                    target.id,
                    target.access.lsb,
                    parts.iter().map(|(value, access)| (value, *access)),
                    arena,
                )?;
                initial_sources.extend(sources);
                Ok(SLTForUpdate {
                    target: *target,
                    expr,
                })
            })
            .collect::<Result<Vec<_>, ParserError>>()?;
        (initial_updates, initial_sources)
    };

    let loop_runner = if effects.is_empty() {
        None
    } else {
        let result = folded_updates[0].target;
        Some(arena.alloc(SLTNode::ForFold {
            loop_var: for_stmt.var_id,
            loop_width,
            loop_signed: for_stmt.var_type.signed,
            start: start.clone(),
            end: end.clone(),
            inclusive,
            step,
            step_op,
            reverse,
            result: SLTForFoldResult::State(result),
            initials: initial_updates.clone(),
            updates: folded_updates.clone(),
            effects: effects.to_vec(),
            continue_cond: loop_state.continue_expr,
        })?)
    };

    if updates.is_empty() {
        let mut store = store;
        store.remove(&for_stmt.var_id);
        return Ok((store, merged_boundaries, loop_runner));
    }

    let mut result_store = store;
    for (target, _expr, sources) in updates {
        let mut all_sources = start_sources.clone();
        all_sources.extend(end_sources.iter().copied());
        all_sources.extend(
            loop_state
                .continue_sources
                .iter()
                .copied()
                .filter(|src| src.id != for_stmt.var_id && !loop_updated_vars.contains(&src.id)),
        );
        all_sources.extend(
            sources
                .into_iter()
                .filter(|src| src.id != for_stmt.var_id && !loop_updated_vars.contains(&src.id)),
        );
        // The fold body reads loop-carried values, but their initial values
        // may come from external state which is absent from every body update
        // source after carried-state filtering.  Keep those dependencies so
        // hierarchy glue and preceding procedural writes are scheduled before
        // the fold.
        all_sources.extend(
            initial_sources
                .iter()
                .copied()
                .filter(|src| src.id != for_stmt.var_id),
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
            result: SLTForFoldResult::State(target),
            initials: initial_updates.clone(),
            updates: folded_updates.clone(),
            effects: Vec::new(),
            continue_cond: loop_state.continue_expr,
        })?;

        let variable = module.variables.get(&target.id).ok_or_else(|| {
            ParserError::illegal_context(
                "for-loop result state",
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
                range_store_error("for-loop result state", error, Some(&for_stmt.token))
            })?;
    }

    result_store.remove(&for_stmt.var_id);
    Ok((result_store, merged_boundaries, loop_runner))
}
