//! Combinational process collection and assignment normalization.

use super::*;

pub(super) fn comb_processes_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
) -> Result<Vec<CombProcess>, AnalyzerError> {
    let mut processes = Vec::new();
    for item in generate::items(
        node,
        syntax_tree,
        const_env,
        &packed_dimensions.type_aliases,
    )? {
        let start = processes.len();
        let mut base_dimensions = packed_dimensions.clone();
        base_dimensions.functions = Arc::new(functions.clone());
        base_dimensions.expression_signedness = Arc::new(expression_signedness.clone());
        let dimensions = item.dimensions(&base_dimensions);
        let literals = item.parameter_literals(parameter_literals);
        comb_processes_from_module_or_generate_item(
            item.node,
            None,
            syntax_tree,
            &item.env,
            &dimensions,
            &dimensions.functions,
            &dimensions.expression_signedness,
            &literals,
            &mut processes,
        )?;
        for process in &mut processes[start..] {
            for assignment in &mut process.assignments {
                item.assignment(assignment);
            }
        }
    }
    Ok(processes)
}

fn comb_processes_from_module_or_generate_item(
    item: &sv_parser::ModuleOrGenerateItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
    processes: &mut Vec<CombProcess>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = item {
        comb_processes_from_module_common_item(
            &item.nodes.1,
            condition,
            syntax_tree,
            const_env,
            packed_dimensions,
            functions,
            expression_signedness,
            parameter_literals,
            processes,
        )?;
    }
    Ok(())
}

fn comb_processes_from_module_common_item(
    item: &sv_parser::ModuleCommonItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
    processes: &mut Vec<CombProcess>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::ModuleCommonItem::ContinuousAssign(assign) => {
            processes.extend(
                assignments_from_continuous_assign(assign, syntax_tree, packed_dimensions)?
                    .into_iter()
                    .map(|assignment| {
                        substitute_assignment_constants_with_parameter_literals(
                            assignment,
                            const_env,
                            parameter_literals,
                        )
                    })
                    .map(|assignment| {
                        CombProcess::new(
                            CombProcessKind::ContinuousAssign,
                            condition.clone().map(|condition| {
                                substitute_const_expr_constants(condition, const_env)
                            }),
                            vec![assignment],
                        )
                    }),
            );
        }
        sv_parser::ModuleCommonItem::AlwaysConstruct(always) => {
            let mut local_packed_dimensions = packed_dimensions.clone();
            local_packed_dimensions.const_env = const_env.clone();
            local_packed_dimensions.parameter_values = parameter_literals.clone();
            if let Some(process) = comb_process_from_always_construct(
                always,
                condition,
                syntax_tree,
                &local_packed_dimensions,
                functions,
                expression_signedness,
                parameter_literals,
            )? {
                processes.push(substitute_process_constants(process, const_env));
            }
        }
        sv_parser::ModuleCommonItem::NetAlias(_) => {
            return Err(AnalyzerError::Unsupported(
                "module-level net alias".to_string(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn assignments_from_continuous_assign(
    assign: &sv_parser::ContinuousAssign,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<Assignment>, AnalyzerError> {
    match assign {
        sv_parser::ContinuousAssign::Net(assign) => assign
            .nodes
            .3
            .nodes
            .0
            .contents()
            .into_iter()
            .map(|assignment| {
                let lhs = net_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported("continuous assignment lvalue".to_string())
                    })?;
                let rhs = expr_from_expression_with_types(
                    &assignment.nodes.2,
                    syntax_tree,
                    packed_dimensions,
                )
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("continuous assignment expression".to_string())
                })?;
                let rhs = if matches!(
                    lhs,
                    LValue::Select {
                        is_2state: true,
                        ..
                    }
                ) {
                    coerce_procedural_assignment_rhs(rhs, &lhs, packed_dimensions)
                } else {
                    rhs
                };
                Ok(Assignment::new(lhs, rhs))
            })
            .collect(),
        sv_parser::ContinuousAssign::Variable(assign) => assign
            .nodes
            .2
            .nodes
            .0
            .contents()
            .into_iter()
            .map(|assignment| {
                let lhs =
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)
                        .ok_or_else(|| {
                            AnalyzerError::Unsupported("continuous assignment lvalue".to_string())
                        })?;
                let rhs = expr_from_expression_with_types(
                    &assignment.nodes.2,
                    syntax_tree,
                    packed_dimensions,
                )
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("continuous assignment expression".to_string())
                })?;
                let rhs = if matches!(
                    lhs,
                    LValue::Select {
                        is_2state: true,
                        ..
                    }
                ) {
                    coerce_procedural_assignment_rhs(rhs, &lhs, packed_dimensions)
                } else {
                    rhs
                };
                Ok(Assignment::new(lhs, rhs))
            })
            .collect(),
    }
}

fn comb_process_from_always_construct(
    always: &sv_parser::AlwaysConstruct,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
) -> Result<Option<CombProcess>, AnalyzerError> {
    if !matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysComb(_)) {
        return Ok(None);
    }
    validate_always_comb_statement(&always.nodes.1)?;
    let mut guarded_assignments = Vec::new();
    conditional_assignments_from_statement(
        &always.nodes.1,
        None,
        true,
        true,
        syntax_tree,
        &packed_dimensions.const_env,
        packed_dimensions,
        &mut guarded_assignments,
    )?;
    for guarded in &mut guarded_assignments {
        guarded.condition = guarded.condition.take().map(|condition| {
            substitute_expr_constants_with_parameter_literals(
                expand_expr_calls(condition, functions, expression_signedness, 0, true),
                &HashMap::default(),
                parameter_literals,
            )
        });
        guarded.assignment = substitute_assignment_constants_with_parameter_literals(
            expand_assignment_calls(
                guarded.assignment.clone(),
                functions,
                expression_signedness,
                true,
            ),
            &HashMap::default(),
            parameter_literals,
        );
    }
    let assignments = comb_assignments_from_guarded(guarded_assignments, packed_dimensions)?;
    Ok((!assignments.is_empty())
        .then(|| CombProcess::new(CombProcessKind::AlwaysComb, condition, assignments)))
}

/// Merge guarded always_comb assignments into per-target multiplexer chains.
///
/// Groups made up solely of unconditional writes stay untouched so the
/// downstream ordered-assignment semantics (and its dependency checks) keep
/// applying. Once a conditional write participates, the group folds
/// back-to-front into nested muxes so earlier branches take priority. A
/// conditional write with no later unconditional fallback would keep the
/// previous value, which infers a latch, and is rejected.
fn comb_assignments_from_guarded(
    mut guarded: Vec<ConditionalAssignment>,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<Assignment>, AnalyzerError> {
    validate_dynamic_select_expansion_limits(&guarded, packed_dimensions)?;
    normalize_mixed_whole_selected_comb_writes(&mut guarded, packed_dimensions);
    let const_env = &packed_dimensions.const_env;
    let (mut targets, mut groups) = comb_assignment_target_groups(&guarded, const_env);
    coerce_conditional_comb_groups(&mut guarded, &groups, packed_dimensions);

    // Apply every cross-target blocking-assignment substitution before any
    // group is materialized. A later group may rewrite an assignment that
    // belongs to an earlier group.
    let mut lvalues_changed = false;
    let mut changed_chains = HashSet::default();
    for (target, indices) in targets.iter().zip(&groups) {
        let preserve_target_writes = indices
            .iter()
            .all(|index| guarded[*index].condition().is_none());
        let initial = overlapping_value_before(&guarded, indices[0], target, packed_dimensions);
        let (changed, chains) = substitute_intermediate_comb_value_reads(
            &mut guarded,
            indices,
            target,
            initial,
            packed_dimensions,
            preserve_target_writes,
        )?;
        lvalues_changed |= changed;
        changed_chains.extend(chains);
    }
    if lvalues_changed {
        // A blocking assignment used by a dynamic index can turn one
        // syntactic target into different concrete targets on sibling paths.
        // Any fallback proof for that chain and the old target grouping are
        // stale after the rewrite.
        for write in &mut guarded {
            if write
                .exhaustive_fallback_start
                .is_some_and(|chain| changed_chains.contains(&chain))
            {
                write.exhaustive_fallback_start = None;
            }
        }
        (targets, groups) = comb_assignment_target_groups(&guarded, const_env);
        coerce_conditional_comb_groups(&mut guarded, &groups, packed_dimensions);
    }

    // Merged groups land on the slot of their last write so relative
    // statement ordering across different, non-overlapping targets is
    // preserved.
    let mut slots: Vec<Option<Assignment>> = Vec::with_capacity(guarded.len());
    slots.extend((0..guarded.len()).map(|_| None));
    for (target, indices) in targets.into_iter().zip(groups) {
        if indices
            .iter()
            .all(|index| guarded[*index].condition().is_none())
        {
            for index in indices {
                slots[index] = Some(guarded[index].assignment().clone());
            }
            continue;
        }
        let last = *indices.last().expect("group is non-empty");
        // Fold in source order so writes after an if/else retain procedural
        // priority. A target-specific exhaustive fallback fills the previous
        // value in the branch chain instead of becoming globally
        // unconditional.
        let initial = overlapping_value_before(&guarded, indices[0], &target, packed_dimensions);
        let has_established_initial = initial.is_some();
        let mut current = initial.unwrap_or_else(comb_previous_value_placeholder);
        for (position, index) in indices.iter().enumerate() {
            let write = &guarded[*index];
            let value = write.assignment().rhs().clone();
            current = if let Some(chain_start) = write.exhaustive_fallback_start {
                let mut branch_value = value;
                for prior in indices[..position].iter().copied().filter(|prior| {
                    *prior >= chain_start && guarded[*prior].path_epochs != write.path_epochs
                }) {
                    branch_value = fold_conditional_assignment_over(branch_value, &guarded[prior]);
                }
                branch_value
            } else {
                fold_conditional_assignment_over(current, write)
            };
        }
        let rhs = simplify_constant_mux_conditions(current, const_env);
        if expr_contains_comb_previous_value(&rhs)
            || (!has_established_initial
                && expr_references_overlapping_lvalue(&rhs, &target, const_env))
        {
            return Err(AnalyzerError::Unsupported(
                "latch inference inside always_comb".to_string(),
            ));
        }
        slots[last] = Some(Assignment::new(target, rhs));
    }
    Ok(slots.into_iter().flatten().collect())
}

fn validate_dynamic_select_expansion_limits(
    guarded: &[ConditionalAssignment],
    packed_dimensions: &PackedDimensions,
) -> Result<(), AnalyzerError> {
    for write in guarded {
        let LValue::Select { name, msb, lsb, .. } = write.assignment().lhs_value() else {
            continue;
        };
        if eval_ast_const_expr(msb, &packed_dimensions.const_env).is_some()
            && eval_ast_const_expr(lsb, &packed_dimensions.const_env).is_some()
        {
            continue;
        }
        let Some(LValue::Select {
            msb: whole_msb,
            lsb: whole_lsb,
            ..
        }) = whole_packed_lvalue(name, packed_dimensions)
        else {
            continue;
        };
        let (Some(whole_msb), Some(whole_lsb)) = (
            eval_ast_const_expr(&whole_msb, &packed_dimensions.const_env),
            eval_ast_const_expr(&whole_lsb, &packed_dimensions.const_env),
        ) else {
            continue;
        };
        if whole_msb.abs_diff(whole_lsb).saturating_add(1) > MAX_DYNAMIC_SELECT_EXPANSION {
            return Err(AnalyzerError::Unsupported(
                "dynamic selected write expansion exceeds limit".to_string(),
            ));
        }
    }
    Ok(())
}

fn comb_assignment_target_groups(
    guarded: &[ConditionalAssignment],
    const_env: &HashMap<String, i128>,
) -> (Vec<LValue>, Vec<Vec<usize>>) {
    let mut targets: Vec<LValue> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (index, conditional) in guarded.iter().enumerate() {
        let target = conditional.assignment().lhs_value();
        let reusable_group = targets
            .iter()
            .enumerate()
            .rev()
            .find_map(|(group, existing)| {
                if existing != target {
                    return None;
                }
                let previous = *groups[group].last()?;
                let separated_by_overlap = guarded[previous + 1..index].iter().any(|assignment| {
                    lvalues_overlap(assignment.assignment().lhs_value(), target, const_env)
                        && assignment.assignment().lhs_value() != target
                });
                (!separated_by_overlap).then_some(group)
            });
        match reusable_group {
            Some(group) => groups[group].push(index),
            None => {
                targets.push(target.clone());
                groups.push(vec![index]);
            }
        }
    }
    (targets, groups)
}

fn coerce_conditional_comb_groups(
    guarded: &mut [ConditionalAssignment],
    groups: &[Vec<usize>],
    packed_dimensions: &PackedDimensions,
) {
    // Every arm that will participate in a mux must first undergo its own
    // procedural assignment conversion. This includes an unconditional
    // initializer that becomes the fallback of a later guarded write.
    for indices in groups {
        let has_conditional = indices
            .iter()
            .any(|index| guarded[*index].condition().is_some());
        if !has_conditional {
            continue;
        }
        for index in indices {
            let assignment = guarded[*index].assignment().clone();
            let lhs = assignment.lhs_value().clone();
            let rhs =
                coerce_procedural_assignment_rhs(assignment.rhs().clone(), &lhs, packed_dimensions);
            guarded[*index].assignment = Assignment::new(lhs, rhs);
        }
    }
}

fn normalize_mixed_whole_selected_comb_writes(
    guarded: &mut [ConditionalAssignment],
    packed_dimensions: &PackedDimensions,
) {
    let mut whole_names = HashSet::default();
    let mut selected_targets: HashMap<String, Vec<LValue>> = HashMap::default();
    let mut conditional_selected_names = HashSet::default();
    let mut earlier_selected_names = HashSet::default();
    for write in guarded.iter() {
        match write.assignment().lhs_value() {
            LValue::Ident(name)
                if write.condition().is_some() || earlier_selected_names.contains(name) =>
            {
                whole_names.insert(name.clone());
            }
            LValue::Ident(_) => {}
            target @ LValue::Select { name, .. } => {
                selected_targets
                    .entry(name.clone())
                    .or_default()
                    .push(target.clone());
                if write.condition().is_some() {
                    conditional_selected_names.insert(name.clone());
                }
                earlier_selected_names.insert(name.clone());
            }
        }
    }
    whole_names.retain(|name| selected_targets.contains_key(name));
    let mut fallback_targets = HashMap::<usize, Vec<LValue>>::default();
    for write in guarded.iter() {
        if let Some(chain_start) = write.exhaustive_fallback_start {
            fallback_targets
                .entry(chain_start)
                .or_default()
                .push(write.assignment().lhs_value().clone());
        }
    }
    let mut normalization_targets = HashMap::default();
    for name in &whole_names {
        let Some(whole_target) = whole_packed_lvalue(name, packed_dimensions) else {
            continue;
        };
        normalization_targets.insert(name.clone(), (whole_target, LValue::Ident(name.clone())));
    }
    for name in conditional_selected_names {
        let Some(whole_target) = whole_packed_lvalue(&name, packed_dimensions) else {
            continue;
        };
        if selected_targets
            .get(&name)
            .is_some_and(|targets| lvalue_is_covered_by(&whole_target, targets, packed_dimensions))
        {
            normalization_targets.insert(name.clone(), (whole_target, LValue::Ident(name)));
            continue;
        }
        // Different partitions can exhaustively cover only a subrange of a
        // wider declaration. Use the widest written selection that contains
        // another partition as their common target.
        let Some(targets) = selected_targets.get(&name) else {
            continue;
        };
        let normalization_target = targets
            .iter()
            .filter(|candidate| {
                targets.iter().any(|target| {
                    target != *candidate
                        && lvalue_is_covered_by(
                            target,
                            std::slice::from_ref(*candidate),
                            packed_dimensions,
                        )
                })
            })
            .filter_map(|candidate| {
                let (_, low, high) = lvalue_bit_range(candidate, packed_dimensions)?;
                fallback_targets
                    .values()
                    .any(|targets| lvalue_is_covered_by(candidate, targets, packed_dimensions))
                    .then_some((high.abs_diff(low), candidate.clone()))
            })
            .max_by_key(|(width, _)| *width)
            .map(|(_, target)| target);
        if let Some(target) = normalization_target {
            normalization_targets.insert(name, (target.clone(), target));
        }
    }
    for write in guarded.iter_mut() {
        let target = write.assignment().lhs_value().clone();
        let LValue::Select { name, .. } = &target else {
            continue;
        };
        let Some((normalization_target, assignment_target)) = normalization_targets.get(name)
        else {
            continue;
        };
        if !matches!(assignment_target, LValue::Ident(_))
            && !lvalue_is_covered_by(
                &target,
                std::slice::from_ref(normalization_target),
                packed_dimensions,
            )
        {
            continue;
        }
        let write_value = coerce_procedural_assignment_rhs(
            write.assignment().rhs().clone(),
            &target,
            packed_dimensions,
        );
        let current = match assignment_target {
            LValue::Ident(_) => Expr::Ident(name.clone()),
            LValue::Select { .. } => expr_from_lvalue(normalization_target, packed_dimensions),
        };
        let Some((rhs, _)) = selected_value_after_write(
            &current,
            normalization_target,
            &target,
            &write_value,
            packed_dimensions,
        ) else {
            continue;
        };
        write.assignment = Assignment::new(assignment_target.clone(), rhs);
    }

    // Several selected writes in one fallback branch become writes to the
    // same normalized whole value. Only the final write completes that
    // fallback; earlier writes must remain ordinary guarded updates.
    let mut last_fallback = HashMap::default();
    for (index, write) in guarded.iter().enumerate() {
        if let Some(chain_start) = write.exhaustive_fallback_start {
            last_fallback.insert((chain_start, write.assignment().lhs().to_string()), index);
        }
    }
    for (index, write) in guarded.iter_mut().enumerate() {
        if let Some(chain_start) = write.exhaustive_fallback_start
            && last_fallback.get(&(chain_start, write.assignment().lhs().to_string()))
                != Some(&index)
        {
            write.exhaustive_fallback_start = None;
        }
    }
}

pub(super) fn fold_conditional_assignment_over(
    current: Expr,
    write: &ConditionalAssignment,
) -> Expr {
    let value = write.assignment().rhs().clone();
    match write.condition() {
        None => value,
        Some(condition) => Expr::Mux {
            condition: Box::new(condition.clone()),
            then_expr: Box::new(value),
            else_expr: Box::new(current),
        },
    }
}

fn validate_always_comb_statement(stmt: &sv_parser::Statement) -> Result<(), AnalyzerError> {
    match &stmt.nodes.2 {
        sv_parser::StatementItem::BlockingAssignment(_) => Ok(()),
        sv_parser::StatementItem::ConditionalStatement(conditional) => {
            validate_always_comb_statement_or_null(&conditional.nodes.3)?;
            for (_, _, _, branch) in &conditional.nodes.4 {
                validate_always_comb_statement_or_null(branch)?;
            }
            if let Some((_, branch)) = &conditional.nodes.5 {
                validate_always_comb_statement_or_null(branch)?;
            }
            Ok(())
        }
        sv_parser::StatementItem::CaseStatement(case) => {
            let sv_parser::CaseStatement::Normal(case) = &**case else {
                return Err(AnalyzerError::Unsupported(
                    "casez, casex, or pattern case inside always_comb".to_string(),
                ));
            };
            if !matches!(case.nodes.1, sv_parser::CaseKeyword::Case(_)) {
                return Err(AnalyzerError::Unsupported(
                    "casez or casex inside always_comb".to_string(),
                ));
            }
            for item in std::iter::once(&case.nodes.3).chain(case.nodes.4.iter()) {
                match item {
                    sv_parser::CaseItem::NonDefault(item) => {
                        validate_always_comb_statement_or_null(&item.nodes.2)?;
                    }
                    sv_parser::CaseItem::Default(item) => {
                        validate_always_comb_statement_or_null(&item.nodes.2)?;
                    }
                }
            }
            Ok(())
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            if !block.nodes.2.is_empty() {
                return Err(AnalyzerError::Unsupported(
                    "block-local declaration inside always_comb".to_string(),
                ));
            }
            for stmt in &block.nodes.3 {
                if let sv_parser::StatementOrNull::Statement(stmt) = stmt {
                    validate_always_comb_statement(stmt)?;
                }
            }
            Ok(())
        }
        sv_parser::StatementItem::LoopStatement(loop_statement) => {
            let sv_parser::LoopStatement::For(loop_statement) = &**loop_statement else {
                return Err(AnalyzerError::Unsupported(
                    "unsupported statement inside always_comb".to_string(),
                ));
            };
            validate_always_comb_statement_or_null(&loop_statement.nodes.2)
        }
        _ => Err(AnalyzerError::Unsupported(
            "unsupported statement inside always_comb".to_string(),
        )),
    }
}

fn validate_always_comb_statement_or_null(
    stmt: &sv_parser::StatementOrNull,
) -> Result<(), AnalyzerError> {
    if let sv_parser::StatementOrNull::Statement(stmt) = stmt {
        validate_always_comb_statement(stmt)?;
    }
    Ok(())
}
