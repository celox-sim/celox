//! Procedural assignments, conditional statements, and condition construction.

use super::*;

pub(super) fn conditional_assignments_from_statement_or_null(
    stmt: &sv_parser::StatementOrNull,
    condition: Option<Expr>,
    exhaustive_fallback: bool,
    retain_unreachable_writes: bool,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    assignments: &mut Vec<ConditionalAssignment>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::StatementOrNull::Statement(stmt) = stmt {
        conditional_assignments_from_statement(
            stmt,
            condition,
            exhaustive_fallback,
            retain_unreachable_writes,
            syntax_tree,
            const_env,
            packed_dimensions,
            assignments,
        )?;
    }
    Ok(())
}

fn push_procedural_assignment(
    assignments: &mut Vec<ConditionalAssignment>,
    condition: Option<Expr>,
    lhs: LValue,
    rhs: Expr,
    packed_dimensions: &PackedDimensions,
) {
    let rhs = if condition.is_some()
        || matches!(
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
    assignments.push(ConditionalAssignment::new(
        condition,
        Assignment::new(lhs, rhs),
    ));
}

pub(super) fn conditional_assignments_from_statement(
    stmt: &sv_parser::Statement,
    condition: Option<Expr>,
    exhaustive_fallback: bool,
    retain_unreachable_writes: bool,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    assignments: &mut Vec<ConditionalAssignment>,
) -> Result<(), AnalyzerError> {
    match &stmt.nodes.2 {
        sv_parser::StatementItem::BlockingAssignment(assignment) => {
            let lowered = match &assignment.0 {
                sv_parser::BlockingAssignment::Variable(assignment) => {
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)
                        .zip(expr_from_expression_with_types(
                            &assignment.nodes.3,
                            syntax_tree,
                            packed_dimensions,
                        ))
                }
                sv_parser::BlockingAssignment::OperatorAssignment(assignment) => {
                    let op = syntax_tree.get_str(&assignment.nodes.1.nodes.0.nodes.0);
                    let lhs = variable_lvalue_from_node(
                        &assignment.nodes.0,
                        syntax_tree,
                        packed_dimensions,
                    );
                    let rhs = expr_from_expression_with_types(
                        &assignment.nodes.2,
                        syntax_tree,
                        packed_dimensions,
                    );
                    match (lhs, rhs, op) {
                        (Some(lhs), Some(rhs), Some("=")) => Some((lhs, rhs)),
                        (Some(lhs), Some(rhs), Some(op)) => {
                            assignment_op_expr(&lhs, op, rhs.clone(), packed_dimensions)
                                .map(|rhs| (lhs, rhs))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            let Some((lhs, rhs)) = lowered else {
                return Err(AnalyzerError::Unsupported(
                    "always_comb assignment expression".to_string(),
                ));
            };
            push_procedural_assignment(assignments, condition, lhs, rhs, packed_dimensions);
        }
        sv_parser::StatementItem::NonblockingAssignment(assignment) => {
            let lhs =
                variable_lvalue_from_node(&assignment.0.nodes.0, syntax_tree, packed_dimensions)
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported("always_ff assignment lowering".to_string())
                    })?;
            let rhs = expr_from_expression_with_types(
                &assignment.0.nodes.3,
                syntax_tree,
                packed_dimensions,
            )
            .ok_or_else(|| {
                AnalyzerError::Unsupported("always_ff assignment lowering".to_string())
            })?;
            push_procedural_assignment(assignments, condition, lhs, rhs, packed_dimensions);
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            for stmt in &block.nodes.3 {
                conditional_assignments_from_statement_or_null(
                    stmt,
                    condition.clone(),
                    exhaustive_fallback,
                    retain_unreachable_writes,
                    syntax_tree,
                    const_env,
                    packed_dimensions,
                    assignments,
                )?;
            }
        }
        sv_parser::StatementItem::ConditionalStatement(stmt) => {
            conditional_assignments_from_conditional_statement(
                stmt,
                condition,
                exhaustive_fallback,
                retain_unreachable_writes,
                syntax_tree,
                const_env,
                packed_dimensions,
                assignments,
            )?;
        }
        sv_parser::StatementItem::CaseStatement(stmt) => {
            conditional_assignments_from_case_statement(
                stmt,
                condition,
                exhaustive_fallback,
                retain_unreachable_writes,
                syntax_tree,
                const_env,
                packed_dimensions,
                assignments,
            )?;
        }
        sv_parser::StatementItem::LoopStatement(loop_statement) => {
            let (name, values) = static_for_loop_iterations(loop_statement, syntax_tree, const_env)
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("unsupported procedural for loop".to_string())
                })?;
            let body = match &**loop_statement {
                sv_parser::LoopStatement::For(loop_statement) => &loop_statement.nodes.2,
                _ => unreachable!(),
            };
            let iterations = if values.is_empty() && retain_unreachable_writes {
                let sv_parser::LoopStatement::For(loop_statement) = &**loop_statement else {
                    unreachable!();
                };
                let (_, initial_value) =
                    static_for_loop_initial_value(loop_statement, syntax_tree, const_env)
                        .ok_or_else(|| {
                            AnalyzerError::Unsupported(
                                "unsupported procedural for loop".to_string(),
                            )
                        })?;
                vec![(initial_value, false)]
            } else {
                values.into_iter().map(|value| (value, true)).collect()
            };
            for (value, reachable) in iterations {
                let mut loop_env = const_env.clone();
                loop_env.insert(name.clone(), value);
                let mut loop_packed_dimensions = packed_dimensions.clone();
                let mut loop_const_env = loop_env.clone();
                insert_parameter_type_markers(
                    &mut loop_const_env,
                    &name,
                    ExprType {
                        width: 32,
                        signed: true,
                    },
                );
                loop_packed_dimensions.const_env = loop_const_env.clone();
                let start = assignments.len();
                let iteration_condition = if reachable {
                    condition.clone()
                } else {
                    combine_expr_conditions(condition.clone(), Expr::Literal("1'b0".to_string()))
                };
                conditional_assignments_from_statement_or_null(
                    body,
                    iteration_condition,
                    exhaustive_fallback && reachable,
                    retain_unreachable_writes,
                    syntax_tree,
                    &loop_const_env,
                    &loop_packed_dimensions,
                    assignments,
                )?;
                for assignment in &mut assignments[start..] {
                    assignment.condition = assignment.condition.take().map(|condition| {
                        substitute_expr_constants_with_parameter_literals(
                            condition,
                            &loop_env,
                            &HashMap::default(),
                        )
                    });
                    assignment.assignment =
                        substitute_assignment_constants(assignment.assignment.clone(), &loop_env);
                }
            }
        }
        _ => {
            return Err(AnalyzerError::Unsupported(
                "unsupported statement inside always_ff".to_string(),
            ));
        }
    }
    Ok(())
}

pub(super) fn coerce_procedural_assignment_rhs(
    rhs: Expr,
    lhs: &LValue,
    packed_dimensions: &PackedDimensions,
) -> Expr {
    let Some(target_type) = lvalue_expr_type(lhs, packed_dimensions) else {
        return rhs;
    };
    let identifier_signedness = packed_dimensions
        .iter()
        .map(|(name, dimensions)| (name.clone(), dimensions.signed))
        .collect();
    let Some(source_signed) = expr_signedness_with_return_types(
        &rhs,
        &identifier_signedness,
        &HashMap::default(),
        &packed_dimensions.function_return_types,
    ) else {
        return rhs;
    };
    let assigned = if source_signed == target_type.signed
        && expr_static_width(&rhs, packed_dimensions) == Some(target_type.width)
    {
        rhs
    } else {
        let assigned = Expr::Resize {
            expr: Box::new(rhs),
            width: target_type.width,
            signed: source_signed,
        };
        Expr::Resize {
            expr: Box::new(assigned),
            width: target_type.width,
            signed: target_type.signed,
        }
    };
    let name = match lhs {
        LValue::Ident(name) | LValue::Select { name, .. } => name,
    };
    if matches!(
        lhs,
        LValue::Select {
            is_2state: true,
            ..
        }
    ) || packed_dimensions
        .get(name)
        .is_some_and(|dimensions| dimensions.is_2state)
    {
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(assigned),
        }
    } else {
        assigned
    }
}

pub(super) fn lvalue_expr_type(
    value: &LValue,
    packed_dimensions: &PackedDimensions,
) -> Option<ExprType> {
    match value {
        LValue::Ident(name) => {
            let dimensions = packed_dimensions.get(name)?;
            let width = dimensions
                .packed
                .iter()
                .map(|dimension| &dimension.width)
                .chain(dimensions.unpacked.iter().map(|dimension| &dimension.width))
                .try_fold(1usize, |width, dimension| {
                    let dimension_width =
                        eval_ast_const_expr(dimension, &packed_dimensions.const_env)?;
                    width.checked_mul(usize::try_from(dimension_width).ok()?)
                })?;
            Some(ExprType {
                width: width.max(1),
                signed: dimensions.signed,
            })
        }
        LValue::Select {
            msb, lsb, signed, ..
        } => {
            let msb = eval_ast_const_expr(msb, &packed_dimensions.const_env)?;
            let lsb = eval_ast_const_expr(lsb, &packed_dimensions.const_env)?;
            Some(ExprType {
                width: usize::try_from(msb.abs_diff(lsb)).ok()?.checked_add(1)?,
                signed: *signed,
            })
        }
    }
}

fn conditional_assignments_from_conditional_statement(
    stmt: &sv_parser::ConditionalStatement,
    parent_condition: Option<Expr>,
    exhaustive_fallback: bool,
    retain_unreachable_writes: bool,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    assignments: &mut Vec<ConditionalAssignment>,
) -> Result<(), AnalyzerError> {
    let chain_start = assignments.len();
    let if_condition =
        expr_from_cond_predicate(&stmt.nodes.2.nodes.1, syntax_tree, packed_dimensions)
            .ok_or_else(|| {
                AnalyzerError::Unsupported("always_ff predicate lowering".to_string())
            })?;
    let mut prior_false = Vec::new();
    let mut definitely_assigned_branches = Vec::new();
    let then_condition = combine_expr_conditions(
        parent_condition.clone(),
        procedural_truth_condition(if_condition.clone()),
    );
    let then_start = assignments.len();
    conditional_assignments_from_statement_or_null(
        &stmt.nodes.3,
        then_condition,
        false,
        retain_unreachable_writes,
        syntax_tree,
        const_env,
        packed_dimensions,
        assignments,
    )?;
    mark_condition_context(assignments, then_start, chain_start);
    definitely_assigned_branches.push(definitely_assigned_comb_targets_statement_or_null(
        &stmt.nodes.3,
        syntax_tree,
        packed_dimensions,
    ));
    prior_false.push(procedural_false_condition(if_condition));

    for (index, (_, _, predicate, branch)) in stmt.nodes.4.iter().enumerate() {
        let branch_condition =
            expr_from_cond_predicate(&predicate.nodes.1, syntax_tree, packed_dimensions)
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("always_ff predicate lowering".to_string())
                })?;
        let mut terms = prior_false.clone();
        terms.push(procedural_truth_condition(branch_condition.clone()));
        let condition = combine_expr_condition_terms(parent_condition.clone(), terms);
        let branch_start = assignments.len();
        conditional_assignments_from_statement_or_null(
            branch,
            condition,
            false,
            retain_unreachable_writes,
            syntax_tree,
            const_env,
            packed_dimensions,
            assignments,
        )?;
        mark_condition_context(assignments, branch_start, chain_start);
        definitely_assigned_branches.push(definitely_assigned_comb_targets_statement_or_null(
            branch,
            syntax_tree,
            packed_dimensions,
        ));
        if index + 1 == stmt.nodes.4.len()
            && stmt.nodes.5.is_none()
            && exhaustive_fallback
            && parent_condition.is_none()
            && conditional_chain_has_complementary_final_predicate(
                stmt,
                syntax_tree,
                packed_dimensions,
            )
        {
            mark_exhaustive_fallback(
                &mut assignments[branch_start..],
                &definitely_assigned_branches,
                chain_start,
                packed_dimensions,
            );
        }
        prior_false.push(procedural_false_condition(branch_condition));
    }

    if let Some((_, branch)) = &stmt.nodes.5 {
        // Keep the false-path guard while lowering. It can only be removed
        // for targets that are written in every branch of this chain.
        let exhaustive = exhaustive_fallback && parent_condition.is_none();
        let condition = combine_expr_condition_terms(parent_condition, prior_false);
        let branch_start = assignments.len();
        conditional_assignments_from_statement_or_null(
            branch,
            condition.clone(),
            false,
            retain_unreachable_writes,
            syntax_tree,
            const_env,
            packed_dimensions,
            assignments,
        )?;
        mark_condition_context(assignments, branch_start, chain_start);
        definitely_assigned_branches.push(definitely_assigned_comb_targets_statement_or_null(
            branch,
            syntax_tree,
            packed_dimensions,
        ));
        if exhaustive {
            mark_exhaustive_fallback(
                &mut assignments[branch_start..],
                &definitely_assigned_branches,
                chain_start,
                packed_dimensions,
            );
        }
    }
    Ok(())
}

fn procedural_false_condition(condition: Expr) -> Expr {
    Expr::Unary {
        op: UnaryOp::LogicNot,
        expr: Box::new(Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(condition),
        }),
    }
}

pub(super) fn expr_from_cond_predicate(
    predicate: &sv_parser::CondPredicate,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    if cond_predicate_has_conjunction_operator(predicate, syntax_tree) {
        return None;
    }
    let entries = predicate.nodes.0.contents();
    let [sv_parser::ExpressionOrCondPattern::Expression(expr)] = entries.as_slice() else {
        return None;
    };
    let expression = expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)?;
    // Prove this before call expansion, while scoped return metadata can tell
    // two-state functions apart from four-state functions of the same name.
    if let Expr::Binary {
        left,
        op: BinaryOp::LogicOr,
        right,
    } = &expression
        && two_state_conditions_are_complements(left, right, packed_dimensions)
    {
        return Some(Expr::Literal("1'b1".to_string()));
    }
    Some(expression)
}

fn cond_predicate_has_conjunction_operator(
    predicate: &sv_parser::CondPredicate,
    syntax_tree: &SyntaxTree,
) -> bool {
    let mut previous = None;
    for node in RefNode::CondPredicate(predicate) {
        let RefNode::Symbol(symbol) = node else {
            continue;
        };
        let locate = symbol.nodes.0;
        if previous.is_some_and(|previous: sv_parser::Locate| {
            previous.offset + previous.len == locate.offset
                && syntax_tree.get_str(&previous) == Some("&&")
                && syntax_tree.get_str(&locate) == Some("&")
        }) {
            return true;
        }
        previous = Some(locate);
    }
    false
}

fn combine_expr_conditions(parent: Option<Expr>, child: Expr) -> Option<Expr> {
    combine_expr_condition_terms(parent, vec![child])
}

pub(super) fn combine_expr_condition_terms(parent: Option<Expr>, terms: Vec<Expr>) -> Option<Expr> {
    let Some(condition) = terms.into_iter().reduce(|left, right| Expr::Binary {
        left: Box::new(left),
        op: BinaryOp::LogicAnd,
        right: Box::new(right),
    }) else {
        return parent;
    };
    Some(match parent {
        Some(parent) => Expr::Binary {
            left: Box::new(parent),
            op: BinaryOp::LogicAnd,
            right: Box::new(condition),
        },
        None => condition,
    })
}

pub(super) fn assignment_op_expr(
    lhs: &LValue,
    op: &str,
    rhs: Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    let op = match op.strip_suffix('=')? {
        "+" => BinaryOp::Add,
        "-" => BinaryOp::Sub,
        "*" => BinaryOp::Mul,
        "/" => BinaryOp::Div,
        "%" => BinaryOp::Mod,
        "<<" => BinaryOp::Shl,
        "<<<" => BinaryOp::Shl,
        ">>" => BinaryOp::Shr,
        ">>>" => BinaryOp::Sar,
        "&" => BinaryOp::BitAnd,
        "|" => BinaryOp::BitOr,
        "^" => BinaryOp::BitXor,
        _ => return None,
    };
    Some(guard_zero_divisions(Expr::Binary {
        left: Box::new(expr_from_lvalue(lhs, packed_dimensions)),
        op,
        right: Box::new(rhs),
    }))
}

pub(super) fn expr_from_lvalue(lhs: &LValue, packed_dimensions: &PackedDimensions) -> Expr {
    let (base, array_slice_width, array_slice_reversed, signed) = match lhs {
        LValue::Ident(name) => return Expr::Ident(name.clone()),
        LValue::Select {
            name,
            msb,
            lsb,
            signed,
            array_slice_width,
            array_slice_reversed,
            ..
        } => (
            Expr::Select {
                expr: Box::new(Expr::Ident(name.clone())),
                msb: msb.clone(),
                lsb: lsb.clone(),
                signed: *signed,
            },
            array_slice_width,
            *array_slice_reversed,
            *signed,
        ),
    };
    let base = if matches!(
        lhs,
        LValue::Select {
            is_2state: true,
            ..
        }
    ) {
        let Some(r#type) = lvalue_expr_type(lhs, packed_dimensions) else {
            return base;
        };
        Expr::Resize {
            expr: Box::new(Expr::Unary {
                op: UnaryOp::ToTwoState,
                expr: Box::new(base),
            }),
            width: r#type.width,
            signed: r#type.signed,
        }
    } else {
        base
    };
    if !array_slice_reversed {
        return base;
    }
    let Some(array_slice_width) = array_slice_width else {
        return base;
    };
    let Some(element_width) = eval_ast_const_expr(array_slice_width, &packed_dimensions.const_env)
        .and_then(|width| usize::try_from(width).ok())
        .filter(|width| *width != 0)
    else {
        return base;
    };
    let Expr::Select { msb, lsb, .. } = &base else {
        return base;
    };
    let Some(total_width) = eval_ast_const_expr(msb, &packed_dimensions.const_env)
        .and_then(|msb| {
            eval_ast_const_expr(lsb, &packed_dimensions.const_env)
                .and_then(|lsb| usize::try_from(msb.abs_diff(lsb)).ok())
        })
        .and_then(|width| width.checked_add(1))
    else {
        return base;
    };
    if total_width % element_width != 0 || total_width == element_width {
        return base;
    }
    let mut parts = Vec::new();
    for offset in (0..total_width).step_by(element_width) {
        let Some(part_msb) = offset
            .checked_add(element_width)
            .and_then(|value| value.checked_sub(1))
        else {
            return base;
        };
        parts.push(Expr::Select {
            expr: Box::new(base.clone()),
            msb: ConstExpr::Literal(part_msb.to_string()),
            lsb: ConstExpr::Literal(offset.to_string()),
            signed,
        });
    }
    Expr::Concat(parts)
}
