//! Written/definitely-assigned targets and lvalue coverage analysis.

use super::*;

pub(super) fn definitely_assigned_comb_targets_statement_or_null(
    stmt: &sv_parser::StatementOrNull,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Vec<LValue> {
    match stmt {
        sv_parser::StatementOrNull::Statement(stmt) => {
            definitely_assigned_comb_targets(stmt, syntax_tree, packed_dimensions)
        }
        sv_parser::StatementOrNull::Attribute(_) => Vec::new(),
    }
}

fn written_comb_targets_statement_or_null(
    stmt: &sv_parser::StatementOrNull,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Vec<LValue>> {
    let sv_parser::StatementOrNull::Statement(stmt) = stmt else {
        return Some(Vec::new());
    };
    written_comb_targets(stmt, syntax_tree, packed_dimensions)
}

fn written_comb_targets(
    stmt: &sv_parser::Statement,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Vec<LValue>> {
    let collect = |nested: &sv_parser::StatementOrNull, targets: &mut Vec<LValue>| {
        for target in
            written_comb_targets_statement_or_null(nested, syntax_tree, packed_dimensions)?
        {
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        Some(())
    };
    match &stmt.nodes.2 {
        sv_parser::StatementItem::BlockingAssignment(assignment) => {
            let target = match &assignment.0 {
                sv_parser::BlockingAssignment::Variable(assignment) => {
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)
                }
                sv_parser::BlockingAssignment::OperatorAssignment(assignment) => {
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)
                }
                _ => None,
            }?;
            Some(vec![target])
        }
        sv_parser::StatementItem::NonblockingAssignment(assignment) => {
            Some(vec![variable_lvalue_from_node(
                &assignment.0.nodes.0,
                syntax_tree,
                packed_dimensions,
            )?])
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            let mut targets = Vec::new();
            for stmt in &block.nodes.3 {
                collect(stmt, &mut targets)?;
            }
            Some(targets)
        }
        sv_parser::StatementItem::ConditionalStatement(conditional) => {
            let mut targets = Vec::new();
            collect(&conditional.nodes.3, &mut targets)?;
            for (_, _, _, branch) in &conditional.nodes.4 {
                collect(branch, &mut targets)?;
            }
            if let Some((_, branch)) = &conditional.nodes.5 {
                collect(branch, &mut targets)?;
            }
            Some(targets)
        }
        sv_parser::StatementItem::CaseStatement(case) => {
            let sv_parser::CaseStatement::Normal(case) = &**case else {
                return None;
            };
            let mut targets = Vec::new();
            for item in std::iter::once(&case.nodes.3).chain(case.nodes.4.iter()) {
                let branch = match item {
                    sv_parser::CaseItem::NonDefault(item) => &item.nodes.2,
                    sv_parser::CaseItem::Default(item) => &item.nodes.2,
                };
                collect(branch, &mut targets)?;
            }
            Some(targets)
        }
        sv_parser::StatementItem::LoopStatement(loop_statement) => {
            let (name, values) = static_for_loop_iterations(
                loop_statement,
                syntax_tree,
                &packed_dimensions.const_env,
            )?;
            if values.is_empty() {
                return Some(Vec::new());
            }
            let sv_parser::LoopStatement::For(loop_statement) = &**loop_statement else {
                return None;
            };
            let mut targets = Vec::new();
            for value in values {
                let mut iteration_dimensions = packed_dimensions.clone();
                iteration_dimensions.const_env.insert(name.clone(), value);
                insert_parameter_type_markers(
                    &mut iteration_dimensions.const_env,
                    &name,
                    ExprType {
                        width: 32,
                        signed: true,
                    },
                );
                for target in written_comb_targets_statement_or_null(
                    &loop_statement.nodes.2,
                    syntax_tree,
                    &iteration_dimensions,
                )? {
                    let target =
                        substitute_lvalue_constants(target, &iteration_dimensions.const_env);
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
            }
            Some(targets)
        }
        _ => None,
    }
}

fn definitely_assigned_comb_targets(
    stmt: &sv_parser::Statement,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Vec<LValue> {
    match &stmt.nodes.2 {
        sv_parser::StatementItem::BlockingAssignment(assignment) => {
            let target = match &assignment.0 {
                sv_parser::BlockingAssignment::Variable(assignment) => {
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)
                }
                sv_parser::BlockingAssignment::OperatorAssignment(assignment) => {
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)
                }
                _ => None,
            };
            target.into_iter().collect()
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            let mut targets = Vec::new();
            let mut guarded_targets: Vec<(Expr, Vec<LValue>)> = Vec::new();
            for stmt in &block.nodes.3 {
                let written_targets =
                    written_comb_targets_statement_or_null(stmt, syntax_tree, packed_dimensions);
                let statement_targets = definitely_assigned_comb_targets_statement_or_null(
                    stmt,
                    syntax_tree,
                    packed_dimensions,
                );
                if let Some(written_targets) = &written_targets {
                    guarded_targets.retain(|(condition, _)| {
                        !written_targets.iter().any(|target| {
                            expr_references_overlapping_lvalue(
                                condition,
                                target,
                                &packed_dimensions.const_env,
                            )
                        })
                    });
                } else {
                    guarded_targets.clear();
                }
                for target in statement_targets {
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
                let Some((condition, branch_targets)) =
                    guarded_comb_targets(stmt, syntax_tree, packed_dimensions)
                else {
                    continue;
                };
                guarded_targets.retain(|(prior_condition, _)| {
                    !branch_targets.iter().any(|target| {
                        expr_references_overlapping_lvalue(
                            prior_condition,
                            target,
                            &packed_dimensions.const_env,
                        )
                    })
                });
                for (prior_condition, prior_targets) in &guarded_targets {
                    if !two_state_conditions_are_complements(
                        prior_condition,
                        &condition,
                        packed_dimensions,
                    ) {
                        continue;
                    }
                    for target in intersect_lvalue_sets(
                        vec![prior_targets.clone(), branch_targets.clone()],
                        packed_dimensions,
                    ) {
                        if !targets.contains(&target) {
                            targets.push(target);
                        }
                    }
                }
                guarded_targets.push((condition, branch_targets));
            }
            targets
        }
        sv_parser::StatementItem::ConditionalStatement(conditional) => {
            let mut branches = Vec::new();
            let mut terminal = false;
            for (predicate, branch) in
                std::iter::once((&conditional.nodes.2.nodes.1, &conditional.nodes.3)).chain(
                    conditional
                        .nodes
                        .4
                        .iter()
                        .map(|(_, _, predicate, branch)| (&predicate.nodes.1, branch)),
                )
            {
                let condition = expr_from_cond_predicate(predicate, syntax_tree, packed_dimensions)
                    .map(|condition| {
                        expand_expr_calls(
                            condition,
                            &packed_dimensions.functions,
                            &packed_dimensions.expression_signedness,
                            0,
                            true,
                        )
                    })
                    .map(|condition| {
                        simplify_constant_mux_conditions(
                            substitute_expr_constants_with_parameter_literals(
                                condition,
                                &packed_dimensions.const_env,
                                &packed_dimensions.parameter_values,
                            ),
                            &packed_dimensions.const_env,
                        )
                    })
                    .map(procedural_truth_condition)
                    .and_then(expr_to_const)
                    .and_then(|condition| {
                        eval_ast_const_expr(&condition, &packed_dimensions.const_env)
                    });
                if condition == Some(0) {
                    continue;
                }
                branches.push(definitely_assigned_comb_targets_statement_or_null(
                    branch,
                    syntax_tree,
                    packed_dimensions,
                ));
                if condition.is_some() {
                    terminal = true;
                    break;
                }
            }
            terminal |= conditional_chain_has_complementary_final_predicate(
                conditional,
                syntax_tree,
                packed_dimensions,
            );
            if !terminal {
                let Some((_, else_branch)) = &conditional.nodes.5 else {
                    return Vec::new();
                };
                branches.push(definitely_assigned_comb_targets_statement_or_null(
                    else_branch,
                    syntax_tree,
                    packed_dimensions,
                ));
            }
            intersect_lvalue_sets(branches, packed_dimensions)
        }
        sv_parser::StatementItem::CaseStatement(case) => {
            let sv_parser::CaseStatement::Normal(case) = &**case else {
                return Vec::new();
            };
            let reachability = two_state_case_item_reachability(
                case,
                syntax_tree,
                &packed_dimensions.const_env,
                packed_dimensions,
            );
            let complete_two_state_case =
                reachability.as_ref().is_some_and(|(_, covered)| *covered);
            let mut has_default = false;
            let branches = std::iter::once(&case.nodes.3)
                .chain(case.nodes.4.iter())
                .enumerate()
                .filter_map(|(index, item)| {
                    if reachability
                        .as_ref()
                        .is_some_and(|(reachable, _)| !reachable[index])
                    {
                        return None;
                    }
                    let branch = match item {
                        sv_parser::CaseItem::NonDefault(item) => &item.nodes.2,
                        sv_parser::CaseItem::Default(item) => {
                            has_default = true;
                            &item.nodes.2
                        }
                    };
                    Some(definitely_assigned_comb_targets_statement_or_null(
                        branch,
                        syntax_tree,
                        packed_dimensions,
                    ))
                })
                .collect::<Vec<_>>();
            if has_default || complete_two_state_case {
                intersect_lvalue_sets(branches, packed_dimensions)
            } else {
                Vec::new()
            }
        }
        sv_parser::StatementItem::LoopStatement(loop_statement) => {
            let Some((name, values)) = static_for_loop_iterations(
                loop_statement,
                syntax_tree,
                &packed_dimensions.const_env,
            ) else {
                return Vec::new();
            };
            let sv_parser::LoopStatement::For(loop_statement) = &**loop_statement else {
                return Vec::new();
            };
            let mut targets = Vec::new();
            for value in values {
                let mut iteration_dimensions = packed_dimensions.clone();
                iteration_dimensions.const_env.insert(name.clone(), value);
                insert_parameter_type_markers(
                    &mut iteration_dimensions.const_env,
                    &name,
                    ExprType {
                        width: 32,
                        signed: true,
                    },
                );
                for target in definitely_assigned_comb_targets_statement_or_null(
                    &loop_statement.nodes.2,
                    syntax_tree,
                    &iteration_dimensions,
                ) {
                    let target =
                        substitute_lvalue_constants(target, &iteration_dimensions.const_env);
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
            }
            targets
        }
        _ => Vec::new(),
    }
}

fn guarded_comb_targets(
    stmt: &sv_parser::StatementOrNull,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<(Expr, Vec<LValue>)> {
    let sv_parser::StatementOrNull::Statement(stmt) = stmt else {
        return None;
    };
    let sv_parser::StatementItem::ConditionalStatement(conditional) = &stmt.nodes.2 else {
        return None;
    };
    if !conditional.nodes.4.is_empty() || conditional.nodes.5.is_some() {
        return None;
    }
    let condition =
        expr_from_cond_predicate(&conditional.nodes.2.nodes.1, syntax_tree, packed_dimensions)?;
    let condition = expand_expr_calls(
        condition,
        &packed_dimensions.functions,
        &packed_dimensions.expression_signedness,
        0,
        true,
    );
    let targets = definitely_assigned_comb_targets_statement_or_null(
        &conditional.nodes.3,
        syntax_tree,
        packed_dimensions,
    );
    if targets.is_empty()
        || targets.iter().any(|target| {
            expr_references_overlapping_lvalue(&condition, target, &packed_dimensions.const_env)
        })
    {
        return None;
    }
    Some((condition, targets))
}

/// An else-if predicate is evaluated only when every earlier predicate was
/// false, so a two-state complement of an earlier predicate closes the chain.
pub(super) fn conditional_chain_has_complementary_final_predicate(
    conditional: &sv_parser::ConditionalStatement,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> bool {
    let Some((_, _, final_predicate, _)) = conditional.nodes.4.last() else {
        return false;
    };
    let expand = |condition| {
        simplify_constant_mux_conditions(
            expand_expr_calls(
                condition,
                &packed_dimensions.functions,
                &packed_dimensions.expression_signedness,
                0,
                true,
            ),
            &packed_dimensions.const_env,
        )
    };
    let Some(final_condition) =
        expr_from_cond_predicate(&final_predicate.nodes.1, syntax_tree, packed_dimensions)
            .map(expand)
    else {
        return false;
    };
    std::iter::once(&conditional.nodes.2.nodes.1)
        .chain(
            conditional.nodes.4[..conditional.nodes.4.len() - 1]
                .iter()
                .map(|(_, _, predicate, _)| &predicate.nodes.1),
        )
        .filter_map(|predicate| expr_from_cond_predicate(predicate, syntax_tree, packed_dimensions))
        .any(|condition| {
            two_state_conditions_are_complements(
                &expand(condition),
                &final_condition,
                packed_dimensions,
            )
        })
}

pub(super) fn two_state_conditions_are_complements(
    left: &Expr,
    right: &Expr,
    packed_dimensions: &PackedDimensions,
) -> bool {
    if let (Some((left, left_positive)), Some((right, right_positive))) = (
        normalized_two_state_boolean(left, packed_dimensions),
        normalized_two_state_boolean(right, packed_dimensions),
    ) && left == right
        && left_positive != right_positive
    {
        return true;
    }
    let is_complement = |candidate: &Expr, other: &Expr| {
        matches!(
            candidate,
            Expr::Unary {
                op: UnaryOp::LogicNot,
                expr,
            } if &**expr == other && expr_is_two_state(other, packed_dimensions)
        )
    };
    let are_inverse_equalities = |left: &Expr, right: &Expr| {
        let (
            Expr::Binary {
                left: left_lhs,
                op: left_op,
                right: left_rhs,
            },
            Expr::Binary {
                left: right_lhs,
                op: right_op,
                right: right_rhs,
            },
        ) = (left, right)
        else {
            return false;
        };
        let operands_match = (left_lhs == right_lhs && left_rhs == right_rhs)
            || (left_lhs == right_rhs && left_rhs == right_lhs);
        if !operands_match {
            return false;
        }
        match (left_op, right_op) {
            (BinaryOp::Eq, BinaryOp::Ne) | (BinaryOp::Ne, BinaryOp::Eq) => {
                expr_is_two_state(left_lhs, packed_dimensions)
                    && expr_is_two_state(left_rhs, packed_dimensions)
            }
            (BinaryOp::EqCase, BinaryOp::NeCase) | (BinaryOp::NeCase, BinaryOp::EqCase) => true,
            _ => false,
        }
    };
    is_complement(left, right) || is_complement(right, left) || are_inverse_equalities(left, right)
}

fn normalized_two_state_boolean<'a>(
    expr: &'a Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<(&'a Expr, bool)> {
    match expr {
        Expr::Resize {
            expr: operand,
            width,
            ..
        } if expr_static_width(operand, packed_dimensions)
            .is_some_and(|source| source <= *width)
            && expr_is_two_state(operand, packed_dimensions) =>
        {
            return normalized_two_state_boolean(operand, packed_dimensions);
        }
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: operand,
        } if expr_is_two_state(operand, packed_dimensions) => {
            return normalized_two_state_boolean(operand, packed_dimensions);
        }
        _ => {}
    }
    if let Expr::Unary { op, expr } = expr
        && (*op == UnaryOp::LogicNot
            || (*op == UnaryOp::BitNot
                && expr_static_width(expr, packed_dimensions) == Some(1)
                && expr_is_two_state(expr, packed_dimensions)))
    {
        let (expr, positive) = normalized_two_state_boolean(expr, packed_dimensions)?;
        return Some((expr, !positive));
    }
    if let Expr::Binary { left, op, right } = expr
        && matches!(
            op,
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqCase | BinaryOp::NeCase
        )
    {
        let nonzero_side = if expr_is_constant_zero(left, &packed_dimensions.const_env) {
            &**right
        } else if expr_is_constant_zero(right, &packed_dimensions.const_env) {
            &**left
        } else {
            return expr_is_two_state(expr, packed_dimensions).then_some((expr, true));
        };
        if expr_is_two_state(nonzero_side, packed_dimensions) {
            return Some((nonzero_side, matches!(op, BinaryOp::Ne | BinaryOp::NeCase)));
        }
    }
    expr_is_two_state(expr, packed_dimensions).then_some((expr, true))
}

fn expr_is_constant_zero(expr: &Expr, const_env: &HashMap<String, i128>) -> bool {
    expr_to_const(expr.clone()).and_then(|expr| eval_ast_const_expr(&expr, const_env)) == Some(0)
}

fn intersect_lvalue_sets(
    sets: Vec<Vec<LValue>>,
    packed_dimensions: &PackedDimensions,
) -> Vec<LValue> {
    if sets.is_empty() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for target in sets.iter().flatten() {
        if !candidates.contains(target) {
            candidates.push(target.clone());
        }
    }
    candidates.retain(|target| {
        sets.iter()
            .all(|set| lvalue_is_covered_by(target, set, packed_dimensions))
    });
    candidates
}

pub(super) fn lvalue_is_covered_by(
    target: &LValue,
    writes: &[LValue],
    packed_dimensions: &PackedDimensions,
) -> bool {
    if writes.contains(target) {
        return true;
    }
    let Some((target_name, target_low, target_high)) = lvalue_bit_range(target, packed_dimensions)
    else {
        return false;
    };
    let mut ranges = writes
        .iter()
        .filter_map(|write| lvalue_bit_range(write, packed_dimensions))
        .filter_map(|(name, low, high)| (name == target_name).then_some((low, high)))
        .collect::<Vec<_>>();
    ranges.sort_unstable_by_key(|(low, _)| *low);
    let mut covered_through = target_low.checked_sub(1);
    for (low, high) in ranges {
        let next = covered_through.and_then(|covered| covered.checked_add(1));
        if next.is_some_and(|next| low > next) {
            continue;
        }
        if low <= target_low || next.is_some_and(|next| low <= next) {
            covered_through = Some(covered_through.map_or(high, |covered| covered.max(high)));
        }
        if covered_through.is_some_and(|covered| covered >= target_high) {
            return true;
        }
    }
    false
}

pub(super) fn lvalue_bit_range(
    value: &LValue,
    packed_dimensions: &PackedDimensions,
) -> Option<(String, i128, i128)> {
    let selected;
    let value = match value {
        LValue::Ident(name) => {
            selected = whole_packed_lvalue(name, packed_dimensions)?;
            &selected
        }
        value => value,
    };
    let LValue::Select { name, msb, lsb, .. } = value else {
        unreachable!();
    };
    let msb = eval_ast_const_expr(msb, &packed_dimensions.const_env)?;
    let lsb = eval_ast_const_expr(lsb, &packed_dimensions.const_env)?;
    Some((name.clone(), msb.min(lsb), msb.max(lsb)))
}
