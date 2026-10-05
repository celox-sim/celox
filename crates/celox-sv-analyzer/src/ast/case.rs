//! Case statement construction and case-item reachability analysis.

use super::*;

pub(super) fn conditional_assignments_from_case_statement(
    stmt: &sv_parser::CaseStatement,
    parent_condition: Option<Expr>,
    exhaustive_fallback: bool,
    retain_unreachable_writes: bool,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    assignments: &mut Vec<ConditionalAssignment>,
) -> Result<(), AnalyzerError> {
    let chain_start = assignments.len();
    let sv_parser::CaseStatement::Normal(stmt) = stmt else {
        return Err(AnalyzerError::Unsupported(
            "casez, casex, or pattern case inside always_comb".to_string(),
        ));
    };
    let case_expr = expr_from_expression_with_types(
        &stmt.nodes.2.nodes.1.nodes.0,
        syntax_tree,
        packed_dimensions,
    )
    .ok_or_else(|| AnalyzerError::Unsupported("always_ff case selector lowering".to_string()))?;
    let item_reachability =
        two_state_case_item_reachability(stmt, syntax_tree, const_env, packed_dimensions);
    let complete_two_state_case = item_reachability
        .as_ref()
        .is_some_and(|(_, covered)| *covered);
    let preserve_unmatched_writes =
        item_reachability
            .as_ref()
            .is_some_and(|(reachable, covered)| {
                !*covered && !reachable.iter().any(|reachable| *reachable)
            });

    let mut branches = Vec::new();
    let mut default_branch = None;
    for (item_index, item) in std::iter::once(&stmt.nodes.3)
        .chain(stmt.nodes.4.iter())
        .enumerate()
    {
        if item_reachability
            .as_ref()
            .is_some_and(|(reachable, _)| !reachable[item_index])
            && !preserve_unmatched_writes
        {
            continue;
        }
        match item {
            sv_parser::CaseItem::NonDefault(item) => {
                let mut conditions = Vec::new();
                for expr in item.nodes.0.contents() {
                    let expr = expr_from_expression_with_types(
                        &expr.nodes.0,
                        syntax_tree,
                        packed_dimensions,
                    )
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported(
                            "always_ff case item expression lowering".to_string(),
                        )
                    })?;
                    conditions.push(case_item_condition(
                        case_expr.clone(),
                        expr,
                        case_keyword_is_wildcard(&stmt.nodes.1),
                    ));
                }
                if let Some(condition) = conditions.into_iter().reduce(|left, right| Expr::Binary {
                    left: Box::new(left),
                    op: BinaryOp::LogicOr,
                    right: Box::new(right),
                }) {
                    branches.push((condition, &item.nodes.2));
                }
            }
            sv_parser::CaseItem::Default(item) => {
                default_branch = Some(&item.nodes.2);
            }
        }
    }

    let mut prior_false = Vec::new();
    let mut definitely_assigned_branches = Vec::new();
    let branch_count = branches.len();
    for (branch_index, (branch_condition, branch)) in branches.into_iter().enumerate() {
        let mut terms = prior_false.clone();
        terms.push(branch_condition.clone());
        let condition = combine_expr_condition_terms(parent_condition.clone(), terms);
        // Case-item guards are never tautological, so statements nested in
        // a branch must keep the item condition.
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
        if complete_two_state_case
            && exhaustive_fallback
            && parent_condition.is_none()
            && branch_index + 1 == branch_count
        {
            mark_exhaustive_fallback(
                &mut assignments[branch_start..],
                &definitely_assigned_branches,
                chain_start,
                packed_dimensions,
            );
        }
        prior_false.push(Expr::Unary {
            op: UnaryOp::LogicNot,
            expr: Box::new(branch_condition),
        });
    }

    if let Some(branch) = default_branch {
        // As with an else branch, retain the selector guard until all case
        // arms are known to assign the same target.
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

pub(super) fn expr_is_two_state(expr: &Expr, packed_dimensions: &PackedDimensions) -> bool {
    match expr {
        Expr::Ident(name) => {
            packed_dimensions
                .get(name)
                .is_some_and(|dimensions| dimensions.is_2state)
                || packed_dimensions.const_env.contains_key(name)
        }
        Expr::Literal(value) => typecheck::parse_integral_literal(value)
            .is_some_and(|literal| literal.mask == num_bigint::BigUint::default()),
        Expr::Select { expr, msb, lsb, .. } => {
            expr_is_two_state(expr, packed_dimensions)
                && select_bounds_are_statically_valid(expr, msb, lsb, packed_dimensions)
        }
        Expr::Resize { expr, .. } => expr_is_two_state(expr, packed_dimensions),
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => parts
            .iter()
            .all(|part| expr_is_two_state(part, packed_dimensions)),
        Expr::Unary { op, expr } => {
            *op == UnaryOp::ToTwoState || expr_is_two_state(expr, packed_dimensions)
        }
        Expr::Binary { left, op, right } => {
            matches!(op, BinaryOp::EqCase | BinaryOp::NeCase)
                || (expr_is_two_state(left, packed_dimensions)
                    && expr_is_two_state(right, packed_dimensions)
                    && (!matches!(op, BinaryOp::Div | BinaryOp::Mod)
                        || expr_to_const((**right).clone())
                            .and_then(|right| {
                                eval_ast_const_expr(&right, &packed_dimensions.const_env)
                            })
                            .is_some_and(|right| right != 0)))
        }
        Expr::Inside { expr, items } => {
            expr_is_two_state(expr, packed_dimensions)
                && items.iter().all(|item| {
                    item.exprs()
                        .into_iter()
                        .all(|operand| expr_is_two_state(operand, packed_dimensions))
                })
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_is_two_state(then_expr, packed_dimensions)
                && (then_expr == else_expr
                    || (expr_is_two_state(condition, packed_dimensions)
                        && expr_is_two_state(else_expr, packed_dimensions)))
        }
        Expr::Call { name, args } => {
            typecheck::bit_vector_function_return_type(name, args.len()).is_some()
                || packed_dimensions
                    .function_return_types
                    .get(name)
                    .is_some_and(|metadata| metadata.is_2state)
        }
    }
}

fn select_bounds_are_statically_valid(
    expr: &Expr,
    msb: &ConstExpr,
    lsb: &ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> bool {
    let (Some(msb), Some(lsb)) = (
        eval_ast_const_expr(msb, &packed_dimensions.const_env),
        eval_ast_const_expr(lsb, &packed_dimensions.const_env),
    ) else {
        return false;
    };
    let (valid_low, valid_high) = if let Expr::Ident(name) = expr {
        let Some(LValue::Select { msb, lsb, .. }) = whole_packed_lvalue(name, packed_dimensions)
        else {
            return false;
        };
        let (Some(msb), Some(lsb)) = (
            eval_ast_const_expr(&msb, &packed_dimensions.const_env),
            eval_ast_const_expr(&lsb, &packed_dimensions.const_env),
        ) else {
            return false;
        };
        (msb.min(lsb), msb.max(lsb))
    } else {
        let Some(width) = expr_static_width(expr, packed_dimensions)
            .and_then(|width| width.checked_sub(1))
            .and_then(|high| i128::try_from(high).ok())
        else {
            return false;
        };
        (0, width)
    };
    msb.min(lsb) >= valid_low && msb.max(lsb) <= valid_high
}

pub(super) fn two_state_case_item_reachability(
    stmt: &sv_parser::CaseStatementNormal,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
) -> Option<(Vec<bool>, bool)> {
    if !matches!(&stmt.nodes.1, sv_parser::CaseKeyword::Case(_)) {
        return None;
    }
    let selector = expr_from_expression_with_types(
        &stmt.nodes.2.nodes.1.nodes.0,
        syntax_tree,
        packed_dimensions,
    )?;
    let selector = expand_expr_calls(
        selector,
        &packed_dimensions.functions,
        &packed_dimensions.expression_signedness,
        0,
        true,
    );
    let parameter_values = packed_dimensions
        .parameter_values
        .iter()
        // A generate-local numeric constant may shadow a module parameter.
        .filter(|(name, _)| !const_env.contains_key(*name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let selector =
        substitute_expr_constants_with_parameter_literals(selector, const_env, &parameter_values);
    // Folding evaluates an expression at its own width and signedness; the
    // unfolded selector and labels keep the operands for the case's common
    // comparison context.
    let unfolded_selector = expr_to_const(selector.clone());
    let selector = simplify_constant_mux_conditions(selector, const_env);
    let selector = fold_const_integral_expr_preserving_mask(selector, const_env);
    let mut labels_by_item = Vec::new();
    let mut unfolded_labels_by_item: Vec<Option<Vec<ConstExpr>>> = Vec::new();
    let mut default_index = None;
    for item in std::iter::once(&stmt.nodes.3).chain(stmt.nodes.4.iter()) {
        match item {
            sv_parser::CaseItem::NonDefault(item) => {
                let labels = item
                    .nodes
                    .0
                    .contents()
                    .into_iter()
                    .map(|label| {
                        expr_from_expression_with_types(
                            &label.nodes.0,
                            syntax_tree,
                            packed_dimensions,
                        )
                        .map(|label| {
                            expand_expr_calls(
                                label,
                                &packed_dimensions.functions,
                                &packed_dimensions.expression_signedness,
                                0,
                                true,
                            )
                        })
                        .map(|label| {
                            substitute_expr_constants_with_parameter_literals(
                                label,
                                const_env,
                                &parameter_values,
                            )
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                unfolded_labels_by_item.push(
                    labels
                        .iter()
                        .map(|label| expr_to_const(label.clone()))
                        .collect::<Option<Vec<_>>>(),
                );
                let labels = labels
                    .into_iter()
                    .map(|label| simplify_constant_mux_conditions(label, const_env))
                    .map(|label| fold_const_integral_expr_preserving_mask(label, const_env))
                    .map(expr_to_const)
                    .collect::<Option<Vec<_>>>()?;
                labels_by_item.push(Some(labels));
            }
            sv_parser::CaseItem::Default(_) => {
                default_index = Some(labels_by_item.len());
                labels_by_item.push(None);
                unfolded_labels_by_item.push(None);
            }
        }
    }
    let (mut duplicate_reachability, mut has_duplicate) =
        case_item_duplicate_reachability(&labels_by_item, const_env, None);
    // A constant selector picks its item at analysis time. Compare the
    // unfolded operands in their common context when every label could be
    // kept, and the folded ones otherwise.
    let unfolded_labels_complete = unfolded_labels_by_item
        .iter()
        .zip(&labels_by_item)
        .all(|(unfolded, folded)| unfolded.is_some() || folded.is_none());
    let constant = match unfolded_selector.as_ref() {
        Some(unfolded) if unfolded_labels_complete && is_constant(unfolded, const_env) => {
            let unfolded_labels: Vec<Option<Vec<ConstExpr>>> = unfolded_labels_by_item
                .iter()
                .zip(&labels_by_item)
                .map(|(unfolded, folded)| folded.as_ref().and(unfolded.clone()))
                .collect();
            constant_case_item_reachability(unfolded, &unfolded_labels, default_index, const_env)
        }
        _ => None,
    };
    if let Some(reachability) = constant {
        return Some(reachability);
    }
    let constant_selector = expr_to_const(selector.clone());
    if let Some(constant_selector) = constant_selector.as_ref()
        && eval_ast_const_expr(constant_selector, const_env).is_none()
        && let Some(reachability) = constant_case_item_reachability(
            constant_selector,
            &labels_by_item,
            default_index,
            const_env,
        )
    {
        return Some(reachability);
    }
    let Some(width) = expr_static_width(&selector, packed_dimensions) else {
        return has_duplicate.then_some((duplicate_reachability, default_index.is_some()));
    };
    let mut identifiers = packed_dimensions
        .iter()
        .map(|(name, dimensions)| (name.clone(), dimensions.signed))
        .collect::<HashMap<_, _>>();
    identifiers.extend(
        parameter_types_from_const_env(const_env)
            .into_iter()
            .map(|(name, r#type)| (name, r#type.signed)),
    );
    let selector_signed = expr_signedness_with_return_types(
        &selector,
        &identifiers,
        &HashMap::default(),
        &packed_dimensions.function_return_types,
    );
    let Some(selector_signed) = selector_signed else {
        return has_duplicate.then_some((duplicate_reachability, default_index.is_some()));
    };
    (duplicate_reachability, has_duplicate) = case_item_duplicate_reachability(
        &labels_by_item,
        const_env,
        Some(ExprType {
            width,
            signed: selector_signed,
        }),
    );
    if !expr_is_two_state(&selector, packed_dimensions) {
        if let Some(reachability) = finite_four_state_case_item_reachability(
            &labels_by_item,
            default_index,
            width,
            selector_signed,
            const_env,
        ) {
            return Some(reachability);
        }
        return has_duplicate.then_some((duplicate_reachability, default_index.is_some()));
    }
    // Constant evaluation and the type model use i128 values. Wider dynamic
    // selectors still lower normally, but duplicate constant items can still
    // be excluded from definite-assignment analysis.
    if width > 128 {
        return has_duplicate.then_some((duplicate_reachability, default_index.is_some()));
    }
    let constant_selector_value = constant_selector
        .as_ref()
        .and_then(|selector| eval_ast_const_expr(selector, const_env));
    let mut reachable = vec![false; labels_by_item.len()];
    if let Some(value) = constant_selector_value {
        let selector = ConstExpr::Literal(format_typed_parameter_literal(
            value,
            width,
            selector_signed,
        ));
        let mut matched = None;
        for (index, labels) in labels_by_item.iter().enumerate() {
            let Some(labels) = labels else {
                continue;
            };
            for label in labels {
                let equal = eval_ast_const_expr(
                    &ConstExpr::Binary {
                        left: Box::new(selector.clone()),
                        op: BinaryOp::EqCase,
                        right: Box::new(label.clone()),
                    },
                    const_env,
                )?;
                if equal != 0 {
                    matched = Some(index);
                    break;
                }
            }
            if matched.is_some() {
                break;
            }
        }
        let covered = if let Some(index) = matched {
            reachable[index] = true;
            true
        } else if let Some(index) = default_index {
            reachable[index] = true;
            true
        } else {
            false
        };
        return Some((reachable, covered));
    }

    // A constant case label can match at most one bit pattern of a two-state
    // selector. Track those patterns directly instead of materializing every
    // value in the selector's 2^width domain.
    let mut matched_values = HashSet::default();
    for (index, labels) in labels_by_item.iter().enumerate() {
        let Some(labels) = labels else {
            continue;
        };
        for label in labels {
            let Some(label_value) = eval_ast_const_expr(label, const_env) else {
                // Only proven X/Z constants are unreachable. An unresolved
                // runtime label requires conservative branch reachability.
                let constant: crate::ir::ConstExpr = label.clone().into();
                let literal = typecheck::eval_const_integral_literal_with_types(
                    &constant,
                    const_env,
                    &parameter_types_from_const_env(const_env)
                        .into_iter()
                        .map(|(name, ty)| (name, (ty.width, ty.signed)))
                        .collect(),
                )?;
                if literal.mask == num_bigint::BigUint::default() {
                    return None;
                }
                continue;
            };
            let pattern = if width == 128 {
                label_value as u128
            } else {
                let mask = 1u128.checked_shl(u32::try_from(width).ok()?)? - 1;
                label_value as u128 & mask
            };
            let candidate = ConstExpr::Literal(format_typed_parameter_literal(
                pattern as i128,
                width,
                selector_signed,
            ));
            let equal = eval_ast_const_expr(
                &ConstExpr::Binary {
                    left: Box::new(candidate),
                    op: BinaryOp::EqCase,
                    right: Box::new(label.clone()),
                },
                const_env,
            )?;
            if equal != 0 && matched_values.insert(pattern) {
                reachable[index] = true;
            }
        }
    }
    let domain_is_covered = u32::try_from(width)
        .ok()
        .and_then(|width| 1usize.checked_shl(width))
        .is_some_and(|value_count| matched_values.len() == value_count);
    if let Some(index) = default_index {
        reachable[index] = !domain_is_covered;
        Some((reachable, true))
    } else {
        Some((reachable, domain_is_covered))
    }
}

fn constant_case_item_reachability(
    selector: &ConstExpr,
    labels_by_item: &[Option<Vec<ConstExpr>>],
    default_index: Option<usize>,
    const_env: &HashMap<String, i128>,
) -> Option<(Vec<bool>, bool)> {
    // The selector and every label share one width and signing context
    // (IEEE 1800-2023 12.5): an unsigned label makes the whole comparison
    // unsigned, down to the operands of a conditional selector.
    let parameter_types = parameter_types_from_const_env(const_env);
    let types: HashMap<String, (usize, bool)> = parameter_types
        .iter()
        .map(|(name, r#type)| (name.clone(), (r#type.width, r#type.signed)))
        .collect();
    let typed = |expr: &ConstExpr| -> crate::ir::ConstExpr {
        substitute_typed_parameter_literals(expr.clone(), const_env, &parameter_types).into()
    };
    let self_type = |expr: &ConstExpr| -> Option<(usize, bool)> {
        let literal =
            typecheck::eval_const_integral_literal_with_types(&typed(expr), const_env, &types)?;
        let fill = matches!(expr, ConstExpr::Literal(literal)
            if resize_unbased_fill_literal_for_cast(literal, 1, false).is_some());
        Some((if fill { 1 } else { literal.width }, literal.signed))
    };
    let (mut width, mut signed) = self_type(selector)?;
    for label in labels_by_item.iter().flatten().flatten() {
        let (label_width, label_signed) = self_type(label)?;
        width = width.max(label_width);
        signed &= label_signed;
    }
    let normalize = |expr: &ConstExpr| {
        typecheck::eval_generate_case_operand(&typed(expr), const_env, &types, width, signed)
    };
    let selector = normalize(selector)?;
    let mut reachable = vec![false; labels_by_item.len()];
    let mut matched = None;
    for (index, labels) in labels_by_item.iter().enumerate() {
        let Some(labels) = labels else {
            continue;
        };
        for label in labels {
            let label = normalize(label)?;
            if label.value == selector.value && label.mask == selector.mask {
                matched = Some(index);
                break;
            }
        }
        if matched.is_some() {
            break;
        }
    }
    let covered = if let Some(index) = matched.or(default_index) {
        reachable[index] = true;
        true
    } else {
        false
    };
    Some((reachable, covered))
}

fn finite_four_state_case_item_reachability(
    labels_by_item: &[Option<Vec<ConstExpr>>],
    default_index: Option<usize>,
    width: usize,
    selector_signed: bool,
    const_env: &HashMap<String, i128>,
) -> Option<(Vec<bool>, bool)> {
    const MAX_PATTERNS: usize = 65_536;
    let state_bits = width.checked_mul(2)?;
    let pattern_count = 1usize.checked_shl(u32::try_from(state_bits).ok()?)?;
    if pattern_count > MAX_PATTERNS {
        return None;
    }
    let mut reachable = vec![false; labels_by_item.len()];
    let mut covered = true;
    for pattern in 0..pattern_count {
        let bits = (0..width)
            .rev()
            .map(|bit| match (pattern >> (bit * 2)) & 3 {
                0 => '0',
                1 => '1',
                2 => 'x',
                3 => 'z',
                _ => unreachable!(),
            })
            .collect::<String>();
        let signing = if selector_signed { "s" } else { "" };
        let selector = ConstExpr::Literal(format!("{width}'{signing}b{bits}"));
        let mut matched = None;
        for (index, labels) in labels_by_item.iter().enumerate() {
            let Some(labels) = labels else {
                continue;
            };
            for label in labels {
                let equal = eval_ast_const_expr(
                    &ConstExpr::Binary {
                        left: Box::new(selector.clone()),
                        op: BinaryOp::EqCase,
                        right: Box::new(label.clone()),
                    },
                    const_env,
                )?;
                if equal != 0 {
                    matched = Some(index);
                    break;
                }
            }
            if matched.is_some() {
                break;
            }
        }
        if let Some(index) = matched.or(default_index) {
            reachable[index] = true;
        } else {
            covered = false;
        }
    }
    Some((reachable, covered))
}

fn case_item_duplicate_reachability(
    labels_by_item: &[Option<Vec<ConstExpr>>],
    const_env: &HashMap<String, i128>,
    selector_type: Option<ExprType>,
) -> (Vec<bool>, bool) {
    let constant_types = parameter_types_from_const_env(const_env)
        .into_iter()
        .map(|(name, r#type)| (name, (r#type.width, r#type.signed)))
        .collect::<HashMap<_, _>>();
    let mut reachable = vec![false; labels_by_item.len()];
    let mut prior_labels: Vec<&ConstExpr> = Vec::new();
    let mut has_duplicate = false;
    for (index, labels) in labels_by_item.iter().enumerate() {
        let Some(labels) = labels else {
            reachable[index] = true;
            continue;
        };
        for label in labels {
            let duplicate = prior_labels.iter().any(|prior| {
                let normalized_equal = selector_type.is_some_and(|selector_type| {
                    let prior = case_label_selector_pattern(
                        prior,
                        const_env,
                        &constant_types,
                        selector_type,
                    );
                    let label = case_label_selector_pattern(
                        label,
                        const_env,
                        &constant_types,
                        selector_type,
                    );
                    prior.zip(label).is_some_and(|(prior, label)| {
                        prior.value == label.value && prior.mask == label.mask
                    })
                });
                normalized_equal
                    || *prior == label
                    || eval_ast_const_expr(
                        &ConstExpr::Binary {
                            left: Box::new((*prior).clone()),
                            op: BinaryOp::EqCase,
                            right: Box::new(label.clone()),
                        },
                        const_env,
                    ) == Some(1)
            });
            has_duplicate |= duplicate;
            reachable[index] |= !duplicate;
            prior_labels.push(label);
        }
    }
    (reachable, has_duplicate)
}

fn case_label_selector_pattern(
    label: &ConstExpr,
    const_env: &HashMap<String, i128>,
    constant_types: &HashMap<String, (usize, bool)>,
    selector_type: ExprType,
) -> Option<typecheck::IntegralLiteral> {
    let label_expr: crate::ir::ConstExpr = label.clone().into();
    if let ConstExpr::Literal(value) = label
        && let Some(resized) =
            resize_unbased_fill_literal_for_cast(value, selector_type.width, selector_type.signed)
    {
        return typecheck::parse_integral_literal(&resized);
    }
    let mut literal =
        typecheck::eval_const_integral_literal_with_types(&label_expr, const_env, constant_types)?;
    let comparison_signed = selector_type.signed && literal.signed;
    if literal.width > selector_type.width {
        let extension_bit = u64::try_from(selector_type.width.checked_sub(1)?).ok()?;
        let extension_value = comparison_signed && literal.value.bit(extension_bit);
        let extension_mask = comparison_signed && literal.mask.bit(extension_bit);
        for bit in selector_type.width..literal.width {
            let bit = bit as u64;
            if literal.value.bit(bit) != extension_value || literal.mask.bit(bit) != extension_mask
            {
                return None;
            }
        }
    }
    literal.signed = comparison_signed;
    let resized =
        resize_integral_literal_for_cast(literal, selector_type.width, selector_type.signed);
    typecheck::parse_integral_literal(&resized)
}

pub(super) fn mark_condition_context(
    assignments: &mut [ConditionalAssignment],
    start: usize,
    guard_boundary: usize,
) {
    let epoch = assignments
        .iter()
        .flat_map(|assignment| assignment.path_epochs.iter().copied())
        .max()
        .map_or(0, |epoch| epoch + 1);
    for assignment in &mut assignments[start..] {
        if assignment.condition.is_some() {
            assignment.guard_boundary.get_or_insert(guard_boundary);
            assignment.path_epochs.push(epoch);
        }
    }
}

pub(super) fn mark_exhaustive_fallback(
    fallback_assignments: &mut [ConditionalAssignment],
    branch_targets: &[Vec<LValue>],
    chain_start: usize,
    packed_dimensions: &PackedDimensions,
) {
    let mut definite_targets = Vec::new();
    for assignment in fallback_assignments.iter() {
        let target = assignment.assignment().lhs_value().clone();
        if branch_targets
            .iter()
            .all(|targets| lvalue_is_covered_by(&target, targets, packed_dimensions))
            && !definite_targets.contains(&target)
        {
            definite_targets.push(target);
        }
    }
    for target in definite_targets {
        let candidates = fallback_assignments
            .iter()
            .enumerate()
            .filter(|(_, assignment)| assignment.assignment().lhs_value() == &target)
            .collect::<Vec<_>>();
        let marked = candidates
            .iter()
            .find(|(_, assignment)| assignment.exhaustive_fallback_start.is_some())
            .map(|(index, _)| *index);
        let shallowest = candidates
            .iter()
            .min_by_key(|(index, assignment)| (assignment.path_epochs.len(), *index))
            .map(|(index, _)| *index);
        if let Some(index) = marked.or(shallowest) {
            fallback_assignments[index].exhaustive_fallback_start = Some(chain_start);
        }
    }
}

/// Whether `expr` has a value at analysis time, unknown bits included.
fn is_constant(expr: &ConstExpr, const_env: &HashMap<String, i128>) -> bool {
    let parameter_types = parameter_types_from_const_env(const_env);
    let types: HashMap<String, (usize, bool)> = parameter_types
        .iter()
        .map(|(name, r#type)| (name.clone(), (r#type.width, r#type.signed)))
        .collect();
    let typed: crate::ir::ConstExpr =
        substitute_typed_parameter_literals(expr.clone(), const_env, &parameter_types).into();
    typecheck::eval_const_integral_literal_with_types(&typed, const_env, &types).is_some()
}
