//! Statements: sequencing, `if`, `case` and system function calls.

use super::*;

pub(super) fn eval_statement(
    module: &Module,
    store: SymbolicStore<VarId>,
    boundaries: HashMap<VarId, BTreeSet<usize>>,
    stmt: &Statement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, HashMap<VarId, BTreeSet<usize>>), ParserError> {
    match stmt {
        Statement::Assign(assign) => eval_assign(module, store, boundaries, assign, arena),
        Statement::If(if_stmt) => eval_if(module, store, boundaries, if_stmt, arena),
        Statement::Case(case_stmt) => eval_case(module, store, boundaries, case_stmt, arena),
        Statement::For(for_stmt) => eval_for(module, store, boundaries, for_stmt, arena),
        Statement::IfReset(ir) => Err(ParserError::illegal_context(
            "statement in always_comb",
            "if_reset".to_string(),
            Some(&ir.token),
        )),
        Statement::SystemFunctionCall(call) => {
            eval_system_function_call_side_effects(module, store, boundaries, call, arena)
        }
        Statement::FunctionCall(fc) => {
            eval_statement_form_function_call(module, store, boundaries, fc, arena)
        }
        Statement::TbMethodCall(_) => Err(ParserError::illegal_context(
            "statement in always_comb",
            "testbench method call".to_string(),
            None,
        )),
        Statement::Break => Err(ParserError::illegal_context(
            "statement in always_comb",
            "break".to_string(),
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

pub(super) fn eval_statement_with_recovery(
    module: &Module,
    store: SymbolicStore<VarId>,
    boundaries: HashMap<VarId, BTreeSet<usize>>,
    stmt: &Statement,
    arena: &mut SLTNodeArena<VarId>,
    loop_candidates: &[LoopRecoveryCandidate],
    active_guard: Option<&ActiveGuard>,
) -> Result<(SymbolicStore<VarId>, HashMap<VarId, BTreeSet<usize>>), ParserError> {
    match stmt {
        Statement::If(if_stmt) => eval_if_with_recovery(
            module,
            store,
            boundaries,
            if_stmt,
            arena,
            loop_candidates,
            active_guard,
        ),
        Statement::Case(case_stmt) => eval_case_with_recovery(
            module,
            store,
            boundaries,
            case_stmt,
            arena,
            loop_candidates,
            active_guard,
        ),
        _ => eval_statement(module, store, boundaries, stmt, arena),
    }
}

pub(super) fn eval_system_function_call_side_effects(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    mut boundaries: BoundaryMap<VarId>,
    call: &SystemFunctionCall,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    fn eval_input(
        module: &Module,
        store: &mut SymbolicStore<VarId>,
        boundaries: &mut BoundaryMap<VarId>,
        input: &SystemFunctionInput,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<(), ParserError> {
        let (_, input_boundaries) =
            eval_expression_effectful(module, store, &input.0, arena, None)?;
        *boundaries = merge_boundaries(std::mem::take(boundaries), input_boundaries);
        Ok(())
    }

    match &call.kind {
        SystemFunctionKind::Display(inputs) | SystemFunctionKind::Write(inputs) => {
            for input in inputs {
                eval_input(module, &mut store, &mut boundaries, input, arena)?;
            }
        }
        SystemFunctionKind::Assert { cond, args, .. } => {
            eval_input(module, &mut store, &mut boundaries, cond, arena)?;
            for input in args {
                eval_input(module, &mut store, &mut boundaries, input, arena)?;
            }
        }
        SystemFunctionKind::Clog2(input)
        | SystemFunctionKind::Onehot(input)
        | SystemFunctionKind::Signed(input)
        | SystemFunctionKind::Unsigned(input) => {
            eval_input(module, &mut store, &mut boundaries, input, arena)?;
        }
        SystemFunctionKind::Readmemh(filename, output) => {
            apply_readmem_writes(module, &mut store, call, filename, output, arena)?;
        }
        SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) | SystemFunctionKind::Finish => {
        }
    }

    Ok((store, boundaries))
}

/// `$readmemh` in a combinational block. A block that reads no signal runs
/// once, at time zero (IEEE 1800-2023 9.2.2.2), so its load reads the file
/// when a simulator instance starts (21.4): the words it may load hold that
/// state instead of being driven, and the values the block wrote to them
/// before are written before the load. [`super::comb`] rejects a load in a
/// block that reads signals, which would read the file again whenever they
/// change.
pub(super) fn apply_readmem_writes(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    call: &SystemFunctionCall,
    filename: &SystemFunctionInput,
    output: &veryl_analyzer::ir::SystemFunctionOutput,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(), ParserError> {
    let veryl_analyzer::ir::SystemFunctionOutput::Local(destinations) = output else {
        return Err(ParserError::illegal_context(
            "$readmemh destination",
            "a hierarchical destination is only valid in a testbench",
            Some(&call.comptime.token),
        ));
    };
    let (var_id, load) = crate::module::readmem_load(module, filename, destinations, 16, |dst| {
        dst.index
            .0
            .iter()
            .map(|index| crate::bitaccess::eval_constexpr(index)?.to_usize())
            .collect()
    })?;
    let Some((lsb, msb)) = load.written_bits() else {
        return Ok(());
    };
    let access = BitAccess::new(lsb, msb);
    let variable = &module.variables[&var_id];
    let width = resolve_total_width(module, variable)?;
    let range_store = store
        .entry(var_id)
        .or_insert_with(|| RangeStore::new(None, width));
    // The values written before, which the words the file does not give keep.
    let mut before = Vec::new();
    let mut bit = access.lsb;
    for (value, relative) in range_store.get_parts(access).map_err(|error| {
        range_store_error("$readmemh destination", error, Some(&call.comptime.token))
    })? {
        let part_width = relative.msb - relative.lsb + 1;
        let part_lsb = bit;
        bit += part_width;
        let Some((node, _)) = value else { continue };
        let constant = match arena.get(node) {
            SLTNode::Constant(value, mask, _, _) => {
                let keep = (BigUint::from(1u8) << part_width) - 1u8;
                Some((
                    (value >> relative.lsb) & &keep,
                    (mask >> relative.lsb) & keep,
                ))
            }
            _ => None,
        };
        let Some((value, mask)) = constant else {
            return Err(ParserError::illegal_context(
                "$readmemh destination",
                "words written before the load in an always_comb must be constants",
                Some(&call.comptime.token),
            ));
        };
        before.push(celox_design::InitialStateWriteRun {
            bit_offset: part_lsb,
            bit_width: part_width,
            value_bytes: celox_frontend_core::memory_file::biguint_to_fixed_le_bytes(
                &value, part_width,
            ),
            mask_bytes: celox_frontend_core::memory_file::biguint_to_fixed_le_bytes(
                &mask, part_width,
            ),
        });
    }
    range_store.update(access, None).map_err(|error| {
        range_store_error("$readmemh destination", error, Some(&call.comptime.token))
    })?;
    if !before.is_empty() {
        arena.memory_loads.push(celox_design::InitialStateValue {
            address: var_id,
            data: celox_design::InitialStateData::Writes(before),
        });
    }
    arena.memory_loads.push(celox_design::InitialStateValue {
        address: var_id,
        data: celox_design::InitialStateData::MemoryFile(load),
    });
    arena.memory_load_call = Some(call.comptime.token);
    Ok(())
}

pub(super) fn eval_statements(
    module: &Module,
    store: SymbolicStore<VarId>,
    boundaries: BoundaryMap<VarId>,
    statements: &[Statement],
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    statements
        .iter()
        .try_fold((store, boundaries), |(store, boundaries), stmt| {
            eval_statement(module, store, boundaries, stmt, arena)
        })
}

pub(super) fn eval_statements_with_recovery(
    module: &Module,
    store: SymbolicStore<VarId>,
    boundaries: BoundaryMap<VarId>,
    statements: &[Statement],
    arena: &mut SLTNodeArena<VarId>,
    loop_candidates: &[LoopRecoveryCandidate],
    active_guard: Option<&ActiveGuard>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    recover_unrolled::eval_statements(
        module,
        store,
        boundaries,
        statements,
        arena,
        loop_candidates,
        active_guard,
    )
}

pub(super) fn eval_case_with_recovery(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    boundaries: BoundaryMap<VarId>,
    case_stmt: &CaseStatement,
    arena: &mut SLTNodeArena<VarId>,
    loop_candidates: &[LoopRecoveryCandidate],
    active_guard: Option<&ActiveGuard>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    fn eval_from_arm(
        module: &Module,
        mut store: SymbolicStore<VarId>,
        boundaries: BoundaryMap<VarId>,
        case_stmt: &CaseStatement,
        target: &expr::EvaluatedCaseTarget,
        arm_index: usize,
        arena: &mut SLTNodeArena<VarId>,
        loop_candidates: &[LoopRecoveryCandidate],
        active_guard: Option<&ActiveGuard>,
    ) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
        let Some(arm) = case_stmt.arms.get(arm_index) else {
            return eval_statements_with_recovery(
                module,
                store,
                boundaries,
                &case_stmt.default,
                arena,
                loop_candidates,
                active_guard,
            );
        };

        let ((cond_expr, cond_sources), cond_bounds) = eval_case_arm_condition_effectful(
            module,
            &mut store,
            &case_stmt.case_target,
            target,
            &arm.patterns,
            arena,
        )?;
        let cond_expr = procedural_condition(arena, cond_expr)?;
        let boundaries = merge_boundaries(boundaries, cond_bounds);

        if let Some(cond_val) = constant_bool(arena, cond_expr) {
            return if cond_val {
                eval_statements_with_recovery(
                    module,
                    store,
                    boundaries,
                    &arm.body,
                    arena,
                    loop_candidates,
                    active_guard,
                )
            } else {
                eval_from_arm(
                    module,
                    store,
                    boundaries,
                    case_stmt,
                    target,
                    arm_index + 1,
                    arena,
                    loop_candidates,
                    active_guard,
                )
            };
        }

        let then_guard = combine_active_guard(arena, active_guard, cond_expr, &cond_sources)?;
        let false_condition = invert_active_condition(arena, cond_expr)?;
        let else_guard = combine_active_guard(arena, active_guard, false_condition, &cond_sources)?;

        let (then_store, then_boundaries) = eval_statements_with_recovery(
            module,
            store.fork(),
            boundaries.clone(),
            &arm.body,
            arena,
            loop_candidates,
            Some(&then_guard),
        )?;
        let (else_store, else_boundaries) = eval_from_arm(
            module,
            store,
            boundaries,
            case_stmt,
            target,
            arm_index + 1,
            arena,
            loop_candidates,
            Some(&else_guard),
        )?;

        Ok((
            merge_symbolic_versions(
                module,
                &then_store,
                &else_store,
                cond_expr,
                &cond_sources,
                arena,
            )?,
            merge_boundaries(then_boundaries, else_boundaries),
        ))
    }

    let (target, target_boundaries) =
        eval_case_target_effectful(module, &mut store, &case_stmt.case_target, arena)?;
    let boundaries = merge_boundaries(boundaries, target_boundaries);
    eval_from_arm(
        module,
        store,
        boundaries,
        case_stmt,
        &target,
        0,
        arena,
        loop_candidates,
        active_guard,
    )
}

pub(super) fn eval_case(
    module: &Module,
    store: SymbolicStore<VarId>,
    boundaries: BoundaryMap<VarId>,
    case_stmt: &CaseStatement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    fn eval_from_arm(
        module: &Module,
        mut store: SymbolicStore<VarId>,
        boundaries: BoundaryMap<VarId>,
        case_stmt: &CaseStatement,
        target: &expr::EvaluatedCaseTarget,
        arm_index: usize,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
        let Some(arm) = case_stmt.arms.get(arm_index) else {
            return eval_statements(module, store, boundaries, &case_stmt.default, arena);
        };

        let ((cond_expr, cond_sources), cond_bounds) = eval_case_arm_condition_effectful(
            module,
            &mut store,
            &case_stmt.case_target,
            target,
            &arm.patterns,
            arena,
        )?;
        let cond_expr = procedural_condition(arena, cond_expr)?;
        let boundaries = merge_boundaries(boundaries, cond_bounds);

        if let Some(cond_val) = constant_bool(arena, cond_expr) {
            return if cond_val {
                eval_statements(module, store, boundaries, &arm.body, arena)
            } else {
                eval_from_arm(
                    module,
                    store,
                    boundaries,
                    case_stmt,
                    target,
                    arm_index + 1,
                    arena,
                )
            };
        }

        let (then_store, then_boundaries) =
            eval_statements(module, store.fork(), boundaries.clone(), &arm.body, arena)?;
        let (else_store, else_boundaries) = eval_from_arm(
            module,
            store,
            boundaries,
            case_stmt,
            target,
            arm_index + 1,
            arena,
        )?;

        Ok((
            merge_symbolic_versions(
                module,
                &then_store,
                &else_store,
                cond_expr,
                &cond_sources,
                arena,
            )?,
            merge_boundaries(then_boundaries, else_boundaries),
        ))
    }

    let mut store = store;
    let (target, target_boundaries) =
        eval_case_target_effectful(module, &mut store, &case_stmt.case_target, arena)?;
    eval_from_arm(
        module,
        store,
        merge_boundaries(boundaries, target_boundaries),
        case_stmt,
        &target,
        0,
        arena,
    )
}

pub(super) fn eval_if_with_recovery(
    module: &Module,
    mut initial_store: SymbolicStore<VarId>,
    mut boundaries: HashMap<VarId, BTreeSet<usize>>,
    stmt: &IfStatement,
    arena: &mut SLTNodeArena<VarId>,
    loop_candidates: &[LoopRecoveryCandidate],
    active_guard: Option<&ActiveGuard>,
) -> Result<(SymbolicStore<VarId>, HashMap<VarId, BTreeSet<usize>>), ParserError> {
    let ((cond_expr, cond_sources), cond_bounds) =
        eval_expression_effectful(module, &mut initial_store, &stmt.cond, arena, None)?;
    let cond_expr = procedural_condition(arena, cond_expr)?;
    boundaries.extend(cond_bounds);

    // Constant folding: if condition is a constant, inline the appropriate side
    if let SLTNode::Constant(val, _, _, _) = arena.get(cond_expr) {
        let side = if *val != BigUint::from(0u32) {
            &stmt.true_side
        } else {
            &stmt.false_side
        };
        return eval_statements_with_recovery(
            module,
            initial_store,
            boundaries,
            side,
            arena,
            loop_candidates,
            active_guard,
        );
    }

    // Evaluate Then and Else paths independently
    let then_guard = combine_active_guard(arena, active_guard, cond_expr, &cond_sources)?;
    let false_condition = invert_active_condition(arena, cond_expr)?;
    let else_guard = combine_active_guard(arena, active_guard, false_condition, &cond_sources)?;
    let (then_store, b_then) = eval_statements_with_recovery(
        module,
        initial_store.fork(),
        boundaries.clone(),
        &stmt.true_side,
        arena,
        loop_candidates,
        Some(&then_guard),
    )?;
    let (else_store, b_else) = eval_statements_with_recovery(
        module,
        initial_store,
        b_then,
        &stmt.false_side,
        arena,
        loop_candidates,
        Some(&else_guard),
    )?;

    Ok((
        merge_symbolic_versions(
            module,
            &then_store,
            &else_store,
            cond_expr,
            &cond_sources,
            arena,
        )?,
        b_else,
    ))
}

pub(super) fn eval_if(
    module: &Module,
    mut initial_store: SymbolicStore<VarId>,
    mut boundaries: HashMap<VarId, BTreeSet<usize>>,
    stmt: &IfStatement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, HashMap<VarId, BTreeSet<usize>>), ParserError> {
    let ((cond_expr, cond_sources), cond_bounds) =
        eval_expression_effectful(module, &mut initial_store, &stmt.cond, arena, None)?;
    let cond_expr = procedural_condition(arena, cond_expr)?;
    boundaries.extend(cond_bounds);

    if let SLTNode::Constant(val, _, _, _) = arena.get(cond_expr) {
        let side = if *val != BigUint::from(0u32) {
            &stmt.true_side
        } else {
            &stmt.false_side
        };
        return side
            .iter()
            .try_fold((initial_store, boundaries), |(s, b), step| {
                eval_statement(module, s, b, step, arena)
            });
    }

    let (then_store, b_then) = stmt.true_side.iter().try_fold(
        (initial_store.fork(), boundaries.clone()),
        |(s, b), step| eval_statement(module, s, b, step, arena),
    )?;
    let (else_store, b_else) = stmt
        .false_side
        .iter()
        .try_fold((initial_store, b_then), |(s, b), step| {
            eval_statement(module, s, b, step, arena)
        })?;

    Ok((
        merge_symbolic_versions(
            module,
            &then_store,
            &else_store,
            cond_expr,
            &cond_sources,
            arena,
        )?,
        b_else,
    ))
}
