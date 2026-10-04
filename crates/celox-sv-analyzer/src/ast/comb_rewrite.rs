//! Combinational value substitution and overlapping selected writes.

use super::*;

const COMB_PREVIOUS_VALUE: &str = "\0celox_comb_previous_value";

/// Substitute the value established by earlier writes to `target` into reads
/// that occur before the merged write is emitted. This handles procedural
/// sequences such as `x = 0; y = x; if (c) x = 1;` without making `y` observe
/// the value of `x` from the preceding process activation.
pub(super) fn substitute_intermediate_comb_value_reads(
    guarded: &mut [ConditionalAssignment],
    indices: &[usize],
    target: &LValue,
    initial: Option<Expr>,
    packed_dimensions: &PackedDimensions,
    preserve_target_writes: bool,
) -> Result<(bool, HashSet<usize>), AnalyzerError> {
    let first = *indices.first().expect("group is non-empty");
    let mut initialized = initial.is_some();
    let mut established = initial.unwrap_or_else(comb_previous_value_placeholder);
    let mut write_index = 0;
    let mut prior_target_writes: Vec<(usize, ConditionalAssignment)> = Vec::new();
    let mut guard_values: HashMap<usize, Expr> = HashMap::default();
    let mut path_values: HashMap<usize, Expr> = HashMap::default();
    let mut lvalues_changed = false;
    let mut changed_chains = HashSet::default();
    let whole_target = match target {
        LValue::Select { name, .. } => whole_packed_lvalue(name, packed_dimensions),
        LValue::Ident(_) => None,
    };
    let mut whole_established = whole_target.as_ref().and_then(|whole_target| {
        overlapping_value_before(guarded, first, whole_target, packed_dimensions)
    });

    // A merged group can be emitted after a later write to one of the values
    // used by its original guard or RHS. If that source is not established
    // before its first write, there is no expression that can snapshot the
    // entry value without introducing hidden process state.
    if guarded[..first].iter().any(|assignment| {
        assignment.condition().is_some_and(|condition| {
            expr_references_overlapping_lvalue(condition, target, &packed_dimensions.const_env)
        }) || expr_references_overlapping_lvalue(
            assignment.assignment().rhs(),
            target,
            &packed_dimensions.const_env,
        )
    }) {
        return Err(AnalyzerError::Unsupported(
            "read-before-write dependency inside always_comb".to_string(),
        ));
    }

    for (index, guarded_assignment) in guarded.iter_mut().enumerate().skip(first) {
        let is_target_write = indices.get(write_index) == Some(&index);
        let original_target_assignment = (is_target_write && preserve_target_writes)
            .then(|| guarded_assignment.assignment().clone());
        if let Some(condition) = guarded_assignment.condition.take() {
            let condition = if let Some(boundary) = guarded_assignment.guard_boundary {
                let value = guard_values
                    .entry(boundary)
                    .or_insert_with(|| established.clone());
                if initialized {
                    substitute_comb_value_reads(
                        condition,
                        target,
                        value,
                        whole_established.as_ref(),
                        packed_dimensions,
                    )
                } else {
                    condition
                }
            } else if initialized {
                substitute_comb_value_reads(
                    condition,
                    target,
                    &established,
                    whole_established.as_ref(),
                    packed_dimensions,
                )
            } else {
                condition
            };
            guarded_assignment.condition = Some(condition);
        }

        let path_value = guarded_assignment
            .path_epochs
            .iter()
            .find_map(|epoch| path_values.get(epoch));
        if initialized || path_value.is_some() {
            let value = path_value.unwrap_or(&established);
            let assignment = guarded_assignment.assignment.clone();
            let lhs = substitute_comb_lvalue_reads(
                assignment.lhs_value().clone(),
                target,
                value,
                whole_established.as_ref(),
                packed_dimensions,
            );
            if &lhs != assignment.lhs_value() && !(is_target_write && preserve_target_writes) {
                lvalues_changed = true;
                changed_chains.extend(guarded_assignment.guard_boundary);
                changed_chains.extend(guarded_assignment.exhaustive_fallback_start);
            }
            guarded_assignment.assignment = Assignment::new(
                lhs,
                substitute_comb_value_reads(
                    assignment.rhs,
                    target,
                    value,
                    whole_established.as_ref(),
                    packed_dimensions,
                ),
            );
        } else if !is_target_write
            && (guarded_assignment.condition().is_some_and(|condition| {
                expr_references_overlapping_lvalue(condition, target, &packed_dimensions.const_env)
            }) || expr_references_overlapping_lvalue(
                guarded_assignment.assignment().rhs(),
                target,
                &packed_dimensions.const_env,
            ))
        {
            return Err(AnalyzerError::Unsupported(
                "read-before-write dependency inside always_comb".to_string(),
            ));
        }

        if !is_target_write {
            // Keep the tracked value current across writes to an overlapping
            // whole object or subrange. Otherwise a later read could be
            // rewritten with the value from before that intervening write.
            // Track the value after procedural assignment conversion: an
            // unsized RHS such as `wide = 0` has only 32 self-determined bits,
            // but subsequent selected writes and reads observe the full LHS.
            let write_value = coerce_procedural_assignment_rhs(
                guarded_assignment.assignment().rhs().clone(),
                guarded_assignment.assignment().lhs_value(),
                packed_dimensions,
            );
            let tracked_target = match target {
                LValue::Ident(name) => whole_packed_lvalue(name, packed_dimensions),
                LValue::Select { .. } => Some(target.clone()),
            };
            if let Some(tracked_target) = tracked_target
                && let Some((updated, covers_target)) = selected_value_after_write(
                    &established,
                    &tracked_target,
                    guarded_assignment.assignment().lhs_value(),
                    &write_value,
                    packed_dimensions,
                )
            {
                established = match guarded_assignment.condition() {
                    None => {
                        initialized |= covers_target;
                        updated
                    }
                    Some(condition) => Expr::Mux {
                        condition: Box::new(condition.clone()),
                        then_expr: Box::new(updated),
                        else_expr: Box::new(established),
                    },
                };
            }
            continue;
        }
        write_index += 1;
        let write = &*guarded_assignment;
        let value = coerce_procedural_assignment_rhs(
            write.assignment().rhs().clone(),
            write.assignment().lhs_value(),
            packed_dimensions,
        );
        let mut tracked_write = write.clone();
        tracked_write.assignment =
            Assignment::new(write.assignment().lhs_value().clone(), value.clone());
        if let (Some(whole_target), Some(current_whole)) =
            (whole_target.as_ref(), whole_established.clone())
            && let Some((updated, _)) = selected_value_after_write(
                &current_whole,
                whole_target,
                target,
                &value,
                packed_dimensions,
            )
        {
            whole_established = Some(match write.condition() {
                None => updated,
                Some(condition) => Expr::Mux {
                    condition: Box::new(condition.clone()),
                    then_expr: Box::new(updated),
                    else_expr: Box::new(current_whole),
                },
            });
        }
        if let Some(chain_start) = write.exhaustive_fallback_start {
            established = value.clone();
            for (prior_index, prior_write) in &prior_target_writes {
                if *prior_index >= chain_start
                    && prior_write.path_epochs != tracked_write.path_epochs
                {
                    established = fold_conditional_assignment_over(established, prior_write);
                }
            }
            initialized = true;
        } else {
            if write.condition().is_none() {
                initialized = true;
            }
            established = fold_conditional_assignment_over(established, &tracked_write);
        }
        for (depth, epoch) in tracked_write.path_epochs.iter().enumerate() {
            if depth == 0 {
                path_values.insert(*epoch, value.clone());
            } else {
                let current = path_values
                    .get(epoch)
                    .cloned()
                    .unwrap_or_else(|| established.clone());
                path_values.insert(
                    *epoch,
                    fold_conditional_assignment_over(current, &tracked_write),
                );
            }
        }
        prior_target_writes.push((index, tracked_write));
        if let Some(assignment) = original_target_assignment {
            guarded_assignment.assignment = assignment;
        }
    }
    Ok((lvalues_changed, changed_chains))
}

fn substitute_comb_value_reads(
    expr: Expr,
    target: &LValue,
    value: &Expr,
    whole_value: Option<&Expr>,
    packed_dimensions: &PackedDimensions,
) -> Expr {
    let substituted =
        if let (LValue::Select { name, .. }, Some(whole_value)) = (target, whole_value) {
            let mut env = HashMap::default();
            env.insert(name.clone(), whole_value.clone());
            substitute_expr_idents(expr, &env)
        } else {
            substitute_expr_lvalue(expr, target, value, packed_dimensions)
        };
    simplify_single_bit_concat_selects(substituted, packed_dimensions)
}

fn substitute_comb_lvalue_reads(
    lvalue: LValue,
    target: &LValue,
    value: &Expr,
    whole_value: Option<&Expr>,
    packed_dimensions: &PackedDimensions,
) -> LValue {
    let LValue::Select {
        name,
        msb,
        lsb,
        signed,
        array_slice_width,
        array_slice_reversed,
        is_2state,
    } = lvalue
    else {
        return lvalue;
    };
    let substitute_bound = |bound: ConstExpr| {
        let original = bound.clone();
        expr_to_const(substitute_comb_value_reads(
            const_expr_to_expr(bound),
            target,
            value,
            whole_value,
            packed_dimensions,
        ))
        .unwrap_or(original)
    };
    LValue::Select {
        name,
        msb: substitute_bound(msb),
        lsb: substitute_bound(lsb),
        signed,
        array_slice_width,
        array_slice_reversed,
        is_2state,
    }
}

fn simplify_single_bit_concat_selects(expr: Expr, packed_dimensions: &PackedDimensions) -> Expr {
    match expr {
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => {
            let expr = simplify_single_bit_concat_selects(*expr, packed_dimensions);
            let bounds = match (
                eval_ast_const_expr(&msb, &packed_dimensions.const_env),
                eval_ast_const_expr(&lsb, &packed_dimensions.const_env),
            ) {
                (Some(msb), Some(lsb)) => Some((msb, lsb)),
                _ => None,
            };
            if let (Some((msb, lsb)), Expr::Concat(parts)) = (bounds, &expr)
                && let (Ok(msb), Ok(lsb)) = (usize::try_from(msb), usize::try_from(lsb))
                && msb >= lsb
            {
                let mut offset = 0usize;
                for part in parts.iter().rev() {
                    let Some(width) = expr_static_width(part, packed_dimensions) else {
                        break;
                    };
                    let end = offset.saturating_add(width);
                    if lsb == offset && msb.checked_add(1) == Some(end) {
                        return Expr::Resize {
                            expr: Box::new(part.clone()),
                            width,
                            signed: false,
                        };
                    }
                    if msb == lsb && msb < end {
                        let selected = msb - offset;
                        return if width == 1 {
                            part.clone()
                        } else {
                            Expr::Select {
                                expr: Box::new(part.clone()),
                                msb: ConstExpr::Literal(selected.to_string()),
                                lsb: ConstExpr::Literal(selected.to_string()),
                                signed: false,
                            }
                        };
                    }
                    offset = end;
                }
            }
            Expr::Select {
                expr: Box::new(expr),
                msb,
                lsb,
                signed,
            }
        }
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| simplify_single_bit_concat_selects(part, packed_dimensions))
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts
                .into_iter()
                .map(|part| simplify_single_bit_concat_selects(part, packed_dimensions))
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(simplify_single_bit_concat_selects(*expr, packed_dimensions)),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(simplify_single_bit_concat_selects(*expr, packed_dimensions)),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(simplify_single_bit_concat_selects(*left, packed_dimensions)),
            op,
            right: Box::new(simplify_single_bit_concat_selects(
                *right,
                packed_dimensions,
            )),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(simplify_single_bit_concat_selects(
                *condition,
                packed_dimensions,
            )),
            then_expr: Box::new(simplify_single_bit_concat_selects(
                *then_expr,
                packed_dimensions,
            )),
            else_expr: Box::new(simplify_single_bit_concat_selects(
                *else_expr,
                packed_dimensions,
            )),
        },
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args
                .into_iter()
                .map(|arg| simplify_single_bit_concat_selects(arg, packed_dimensions))
                .collect(),
        },
        Expr::Ident(_) | Expr::Literal(_) => expr,
    }
}

pub(super) fn expr_static_width(
    expr: &Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<usize> {
    match expr {
        Expr::Ident(name) => lvalue_expr_type(&LValue::Ident(name.clone()), packed_dimensions)
            .map(|r#type| r#type.width)
            .or_else(|| variable_size_function_width(&packed_dimensions.const_env, name, false))
            .or_else(|| {
                parameter_types_from_const_env(&packed_dimensions.const_env)
                    .get(name)
                    .map(|r#type| r#type.width)
            }),
        Expr::Literal(literal) => {
            typecheck::parse_integral_literal(literal).map(|literal| literal.width)
        }
        Expr::Select { msb, lsb, .. } => {
            let msb = eval_ast_const_expr(msb, &packed_dimensions.const_env)?;
            let lsb = eval_ast_const_expr(lsb, &packed_dimensions.const_env)?;
            usize::try_from(msb.abs_diff(lsb)).ok()?.checked_add(1)
        }
        Expr::Concat(parts) => parts.iter().try_fold(0usize, |width, part| {
            width.checked_add(expr_static_width(part, packed_dimensions)?)
        }),
        Expr::RepeatConcat { count, parts } => {
            let count = eval_ast_const_expr(count, &packed_dimensions.const_env)?;
            let count = usize::try_from(count).ok()?;
            let width = parts.iter().try_fold(0usize, |width, part| {
                width.checked_add(expr_static_width(part, packed_dimensions)?)
            })?;
            width.checked_mul(count)
        }
        Expr::Resize { width, .. } => Some(*width),
        Expr::Unary { op, expr } => {
            if matches!(
                op,
                UnaryOp::LogicNot | UnaryOp::RedAnd | UnaryOp::RedOr | UnaryOp::RedXor
            ) {
                Some(1)
            } else {
                expr_static_width(expr, packed_dimensions)
            }
        }
        Expr::Binary { left, op, right } => {
            if matches!(
                op,
                BinaryOp::LogicAnd
                    | BinaryOp::LogicOr
                    | BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::EqCase
                    | BinaryOp::NeCase
                    | BinaryOp::EqWildcard
                    | BinaryOp::NeWildcard
                    | BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
            ) {
                Some(1)
            } else if matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
                expr_static_width(left, packed_dimensions)
            } else {
                Some(
                    expr_static_width(left, packed_dimensions)?
                        .max(expr_static_width(right, packed_dimensions)?),
                )
            }
        }
        Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => Some(
            expr_static_width(then_expr, packed_dimensions)?
                .max(expr_static_width(else_expr, packed_dimensions)?),
        ),
        Expr::Call { name, args } => typecheck::bit_vector_function_return_type(name, args.len())
            .map(|(width, _)| width)
            .or_else(|| {
                packed_dimensions
                    .function_return_types
                    .get(name)
                    .and_then(|metadata| metadata.width)
            }),
    }
}

pub(super) fn lvalues_overlap(
    left: &LValue,
    right: &LValue,
    const_env: &HashMap<String, i128>,
) -> bool {
    match (left, right) {
        (LValue::Ident(left), LValue::Ident(right)) => left == right,
        (LValue::Ident(left), LValue::Select { name: right, .. })
        | (LValue::Select { name: left, .. }, LValue::Ident(right)) => left == right,
        (
            LValue::Select {
                name: left_name,
                msb: left_msb,
                lsb: left_lsb,
                ..
            },
            LValue::Select {
                name: right_name,
                msb: right_msb,
                lsb: right_lsb,
                ..
            },
        ) => {
            if left_name != right_name {
                return false;
            }
            match (
                eval_ast_const_expr(left_msb, const_env),
                eval_ast_const_expr(left_lsb, const_env),
                eval_ast_const_expr(right_msb, const_env),
                eval_ast_const_expr(right_lsb, const_env),
            ) {
                (Some(left_msb), Some(left_lsb), Some(right_msb), Some(right_lsb)) => {
                    left_msb.min(left_lsb) <= right_msb.max(right_lsb)
                        && right_msb.min(right_lsb) <= left_msb.max(left_lsb)
                }
                _ => true,
            }
        }
    }
}

pub(super) fn overlapping_value_before(
    guarded: &[ConditionalAssignment],
    before: usize,
    target: &LValue,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    if let LValue::Ident(target_name) = target {
        let selected_target = whole_packed_lvalue(target_name, packed_dimensions)?;
        return overlapping_value_before(guarded, before, &selected_target, packed_dimensions);
    }
    let mut current = comb_previous_value_placeholder();
    let mut initialized = false;
    let mut states_before = vec![(current.clone(), initialized)];
    for (index, write) in guarded[..before].iter().enumerate() {
        let Some((updated, covers_target)) = selected_value_after_write(
            &current,
            target,
            write.assignment().lhs_value(),
            write.assignment().rhs(),
            packed_dimensions,
        ) else {
            states_before.push((current.clone(), initialized));
            continue;
        };
        if let Some(chain_start) = write.exhaustive_fallback_start {
            let (base, base_initialized) = states_before
                .get(chain_start)
                .cloned()
                .unwrap_or_else(|| (comb_previous_value_placeholder(), false));
            let Some((mut branch_value, fallback_covers_target)) = selected_value_after_write(
                &base,
                target,
                write.assignment().lhs_value(),
                write.assignment().rhs(),
                packed_dimensions,
            ) else {
                states_before.push((current.clone(), initialized));
                continue;
            };
            for prior in &guarded[chain_start..index] {
                let Some((prior_value, _)) = selected_value_after_write(
                    &branch_value,
                    target,
                    prior.assignment().lhs_value(),
                    prior.assignment().rhs(),
                    packed_dimensions,
                ) else {
                    continue;
                };
                branch_value = match prior.condition() {
                    None => prior_value,
                    Some(condition) => Expr::Mux {
                        condition: Box::new(condition.clone()),
                        then_expr: Box::new(prior_value),
                        else_expr: Box::new(branch_value),
                    },
                };
            }
            current = branch_value;
            initialized = base_initialized || fallback_covers_target;
        } else {
            current = match write.condition() {
                None => {
                    initialized |= covers_target;
                    updated
                }
                Some(condition) => Expr::Mux {
                    condition: Box::new(condition.clone()),
                    then_expr: Box::new(updated),
                    else_expr: Box::new(current),
                },
            };
        }
        states_before.push((current.clone(), initialized));
    }
    initialized.then_some(current)
}

pub(super) fn whole_packed_lvalue(
    name: &str,
    packed_dimensions: &PackedDimensions,
) -> Option<LValue> {
    let dimensions = packed_dimensions.get(name)?;
    let (msb, lsb) = if dimensions.unpacked.is_empty()
        && dimensions.packed.len() == 1
        && !dimensions.packed[0].normalize_single
    {
        (
            dimensions.packed[0].left.clone(),
            dimensions.packed[0].right.clone(),
        )
    } else {
        let width = product_expr(
            &dimensions
                .packed
                .iter()
                .map(|dimension| dimension.width.clone())
                .chain(
                    dimensions
                        .unpacked
                        .iter()
                        .map(|dimension| dimension.width.clone()),
                )
                .collect::<Vec<_>>(),
        );
        (
            ConstExpr::Binary {
                left: Box::new(width),
                op: BinaryOp::Sub,
                right: Box::new(ConstExpr::Literal("1".to_string())),
            },
            ConstExpr::Literal("0".to_string()),
        )
    };
    Some(LValue::Select {
        name: name.to_string(),
        msb,
        lsb,
        signed: dimensions.signed,
        array_slice_width: None,
        array_slice_reversed: false,
        is_2state: false,
    })
}

pub(super) fn selected_value_after_write(
    current: &Expr,
    target: &LValue,
    write_target: &LValue,
    write_value: &Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<(Expr, bool)> {
    let const_env = &packed_dimensions.const_env;
    let LValue::Select {
        name: target_name,
        msb: target_msb_expr,
        lsb: target_lsb_expr,
        signed,
        ..
    } = target
    else {
        return None;
    };
    match write_target {
        LValue::Ident(write_name) => (write_name == target_name).then(|| {
            let (msb, lsb) = whole_write_select_offsets(
                target_name,
                target_msb_expr,
                target_lsb_expr,
                packed_dimensions,
            );
            (
                Expr::Select {
                    expr: Box::new(write_value.clone()),
                    msb,
                    lsb,
                    signed: *signed,
                },
                true,
            )
        }),
        LValue::Select {
            name: write_name,
            msb: write_msb_expr,
            lsb: write_lsb_expr,
            ..
        } => {
            if write_name != target_name {
                return None;
            }
            let target_msb = eval_ast_const_expr(target_msb_expr, const_env)?;
            let target_lsb = eval_ast_const_expr(target_lsb_expr, const_env)?;
            let (write_msb, write_lsb) = match (
                eval_ast_const_expr(write_msb_expr, const_env),
                eval_ast_const_expr(write_lsb_expr, const_env),
            ) {
                (Some(msb), Some(lsb)) => (msb, lsb),
                _ => {
                    return dynamic_selected_value_after_write(
                        current,
                        target,
                        write_target,
                        write_value,
                        packed_dimensions,
                    );
                }
            };
            let target_low = target_msb.min(target_lsb);
            let target_high = target_msb.max(target_lsb);
            let overlap_low = target_low.max(write_msb.min(write_lsb));
            let overlap_high = target_high.min(write_msb.max(write_lsb));
            if overlap_low > overlap_high {
                return None;
            }

            let target_step = if target_msb >= target_lsb { -1 } else { 1 };
            let overlap_first = if target_step < 0 {
                overlap_high
            } else {
                overlap_low
            };
            let overlap_last = if target_step < 0 {
                overlap_low
            } else {
                overlap_high
            };
            let mut parts = Vec::new();
            if target_msb != overlap_first {
                parts.push(selected_value_read(
                    current,
                    target_msb,
                    target_lsb,
                    target_msb,
                    overlap_first.checked_sub(target_step)?,
                    packed_dimensions,
                )?);
            }
            parts.push(selected_value_read(
                write_value,
                write_msb,
                write_lsb,
                overlap_first,
                overlap_last,
                packed_dimensions,
            )?);
            if overlap_last != target_lsb {
                parts.push(selected_value_read(
                    current,
                    target_msb,
                    target_lsb,
                    overlap_last.checked_add(target_step)?,
                    target_lsb,
                    packed_dimensions,
                )?);
            }
            let value = if parts.len() == 1 {
                parts.pop()?
            } else {
                Expr::Concat(parts)
            };
            Some((
                value,
                overlap_low == target_low && overlap_high == target_high,
            ))
        }
    }
}

fn dynamic_selected_value_after_write(
    current: &Expr,
    target: &LValue,
    write_target: &LValue,
    write_value: &Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<(Expr, bool)> {
    let LValue::Select {
        name,
        msb: write_msb,
        lsb: write_lsb,
        signed,
        ..
    } = write_target
    else {
        return None;
    };

    // Runtime indices still describe a fixed-width selection. Evaluate one
    // representative position to recover its direction and width, then use
    // case-equality guards so unknown and out-of-range positions are no-ops.
    let mut sample_env = packed_dimensions.const_env.clone();
    for variable in packed_dimensions.keys() {
        sample_env.entry(variable.clone()).or_insert(0);
    }
    let sample_msb = eval_ast_const_expr(write_msb, &sample_env)?;
    let sample_lsb = eval_ast_const_expr(write_lsb, &sample_env)?;
    let delta = sample_msb.checked_sub(sample_lsb)?;
    let whole = whole_packed_lvalue(name, packed_dimensions)?;
    let LValue::Select {
        msb: whole_msb,
        lsb: whole_lsb,
        ..
    } = whole
    else {
        unreachable!("whole packed lvalue is always a selection");
    };
    let whole_msb = eval_ast_const_expr(&whole_msb, &packed_dimensions.const_env)?;
    let whole_lsb = eval_ast_const_expr(&whole_lsb, &packed_dimensions.const_env)?;
    let whole_low = whole_msb.min(whole_lsb);
    let whole_high = whole_msb.max(whole_lsb);
    let candidate_count = whole_high.abs_diff(whole_low).checked_add(1)?;
    if candidate_count > MAX_DYNAMIC_SELECT_EXPANSION {
        return None;
    }
    let mut result = current.clone();
    let mut matched = false;
    for candidate_lsb in whole_low..=whole_high {
        let Some(candidate_msb) = candidate_lsb.checked_add(delta) else {
            continue;
        };
        if candidate_msb < whole_low || candidate_msb > whole_high {
            continue;
        }
        let candidate = LValue::Select {
            name: name.clone(),
            msb: const_expr_from_i128(candidate_msb),
            lsb: const_expr_from_i128(candidate_lsb),
            signed: *signed,
            array_slice_width: None,
            array_slice_reversed: false,
            is_2state: false,
        };
        let Some((updated, _)) =
            selected_value_after_write(current, target, &candidate, write_value, packed_dimensions)
        else {
            continue;
        };
        let matches_msb = Expr::Binary {
            left: Box::new(const_expr_to_expr(write_msb.clone())),
            op: BinaryOp::EqCase,
            right: Box::new(const_expr_to_expr(const_expr_from_i128(candidate_msb))),
        };
        let matches_lsb = Expr::Binary {
            left: Box::new(const_expr_to_expr(write_lsb.clone())),
            op: BinaryOp::EqCase,
            right: Box::new(const_expr_to_expr(const_expr_from_i128(candidate_lsb))),
        };
        result = Expr::Mux {
            condition: Box::new(Expr::Binary {
                left: Box::new(matches_msb),
                op: BinaryOp::LogicAnd,
                right: Box::new(matches_lsb),
            }),
            then_expr: Box::new(updated),
            else_expr: Box::new(result),
        };
        matched = true;
    }
    matched.then_some((result, false))
}

fn whole_write_select_offsets(
    name: &str,
    msb: &ConstExpr,
    lsb: &ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> (ConstExpr, ConstExpr) {
    let Some(dimensions) = packed_dimensions.get(name) else {
        return (msb.clone(), lsb.clone());
    };
    if dimensions.unpacked.is_empty()
        && dimensions.packed.len() == 1
        && !dimensions.packed[0].normalize_single
    {
        let dimension = &dimensions.packed[0];
        return (
            packed_index_offset(dimension, msb.clone()),
            packed_index_offset(dimension, lsb.clone()),
        );
    }
    (msb.clone(), lsb.clone())
}

fn selected_value_read(
    value: &Expr,
    value_msb: i128,
    value_lsb: i128,
    first_coordinate: i128,
    last_coordinate: i128,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    let msb = selected_target_bit(value_msb, value_lsb, first_coordinate)?;
    let lsb = selected_target_bit(value_msb, value_lsb, last_coordinate)?;
    if let (Ok(msb), Ok(lsb)) = (usize::try_from(msb), usize::try_from(lsb))
        && msb >= lsb
        && let Some(narrowed) = narrow_bit_select(value, msb, lsb, packed_dimensions)
    {
        return Some(narrowed);
    }
    Some(Expr::Select {
        expr: Box::new(value.clone()),
        msb: const_expr_from_i128(msb),
        lsb: const_expr_from_i128(lsb),
        signed: false,
    })
}

/// Select `[msb:lsb]` of `value`, pushing the selection through concatenations
/// and conditional merges so only the bits actually read are retained.
///
/// Successive partial writes build `{value[hi:k+1], new, value[k-1:0]}`, which
/// references the previous value twice. Without narrowing, the tracked value
/// doubles in size on every write, so an unrolled loop of N bit writes costs
/// O(2^N).
fn narrow_bit_select(
    value: &Expr,
    msb: usize,
    lsb: usize,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    let select = |expr: &Expr, msb: usize, lsb: usize| {
        narrow_bit_select(expr, msb, lsb, packed_dimensions).unwrap_or_else(|| Expr::Select {
            expr: Box::new(expr.clone()),
            msb: const_expr_from_i128(msb as i128),
            lsb: const_expr_from_i128(lsb as i128),
            signed: false,
        })
    };
    match value {
        Expr::Concat(parts) => {
            let widths = parts
                .iter()
                .map(|part| expr_static_width(part, packed_dimensions))
                .collect::<Option<Vec<_>>>()?;
            let total = widths
                .iter()
                .try_fold(0usize, |sum, width| sum.checked_add(*width))?;
            if msb >= total {
                return None;
            }
            let mut pieces = Vec::new();
            let mut offset = 0usize;
            for (part, width) in parts.iter().zip(&widths).rev() {
                let end = offset + width;
                if *width != 0 && offset <= msb && end > lsb {
                    let low = lsb.max(offset);
                    let high = msb.min(end - 1);
                    let piece = if low == offset && high == end - 1 {
                        part.clone()
                    } else {
                        select(part, high - offset, low - offset)
                    };
                    match piece {
                        Expr::Concat(inner) => pieces.extend(inner.into_iter().rev()),
                        piece => pieces.push(piece),
                    }
                }
                offset = end;
            }
            pieces.reverse();
            if pieces.len() == 1 {
                Some(Expr::Resize {
                    expr: Box::new(pieces.pop()?),
                    width: msb - lsb + 1,
                    signed: false,
                })
            } else {
                Some(Expr::Concat(pieces))
            }
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let covers = |arm: &Expr| {
                expr_static_width(arm, packed_dimensions).is_some_and(|width| width > msb)
            };
            if !covers(then_expr) || !covers(else_expr) {
                return None;
            }
            Some(Expr::Mux {
                condition: condition.clone(),
                then_expr: Box::new(select(then_expr, msb, lsb)),
                else_expr: Box::new(select(else_expr, msb, lsb)),
            })
        }
        _ => None,
    }
}

pub(super) fn comb_previous_value_placeholder() -> Expr {
    Expr::Ident(COMB_PREVIOUS_VALUE.to_string())
}

pub(super) fn expr_contains_comb_previous_value(expr: &Expr) -> bool {
    match expr {
        Expr::Ident(name) => name == COMB_PREVIOUS_VALUE,
        Expr::Literal(_) => false,
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            expr_contains_comb_previous_value(expr)
        }
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => {
            parts.iter().any(expr_contains_comb_previous_value)
        }
        Expr::Binary { left, right, .. } => {
            expr_contains_comb_previous_value(left) || expr_contains_comb_previous_value(right)
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_contains_comb_previous_value(condition)
                || expr_contains_comb_previous_value(then_expr)
                || expr_contains_comb_previous_value(else_expr)
        }
        Expr::Call { args, .. } => args.iter().any(expr_contains_comb_previous_value),
    }
}

fn substitute_expr_lvalue(
    expr: Expr,
    target: &LValue,
    value: &Expr,
    packed_dimensions: &PackedDimensions,
) -> Expr {
    if expr_matches_lvalue(&expr, target) {
        return value.clone();
    }
    if let Some(replacement) =
        substitute_overlapping_selected_read(&expr, target, value, packed_dimensions)
    {
        return replacement;
    }
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => expr,
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(substitute_expr_lvalue(
                *expr,
                target,
                value,
                packed_dimensions,
            )),
            msb: substitute_const_expr_lvalue(msb, target, value, packed_dimensions),
            lsb: substitute_const_expr_lvalue(lsb, target, value, packed_dimensions),
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| substitute_expr_lvalue(part, target, value, packed_dimensions))
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts
                .into_iter()
                .map(|part| substitute_expr_lvalue(part, target, value, packed_dimensions))
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(substitute_expr_lvalue(
                *expr,
                target,
                value,
                packed_dimensions,
            )),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(substitute_expr_lvalue(
                *expr,
                target,
                value,
                packed_dimensions,
            )),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(substitute_expr_lvalue(
                *left,
                target,
                value,
                packed_dimensions,
            )),
            op,
            right: Box::new(substitute_expr_lvalue(
                *right,
                target,
                value,
                packed_dimensions,
            )),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(substitute_expr_lvalue(
                *condition,
                target,
                value,
                packed_dimensions,
            )),
            then_expr: Box::new(substitute_expr_lvalue(
                *then_expr,
                target,
                value,
                packed_dimensions,
            )),
            else_expr: Box::new(substitute_expr_lvalue(
                *else_expr,
                target,
                value,
                packed_dimensions,
            )),
        },
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args
                .into_iter()
                .map(|arg| substitute_expr_lvalue(arg, target, value, packed_dimensions))
                .collect(),
        },
    }
}

fn substitute_const_expr_lvalue(
    expr: ConstExpr,
    target: &LValue,
    value: &Expr,
    packed_dimensions: &PackedDimensions,
) -> ConstExpr {
    let original = expr.clone();
    expr_to_const(substitute_expr_lvalue(
        const_expr_to_expr(expr),
        target,
        value,
        packed_dimensions,
    ))
    .unwrap_or(original)
}

fn substitute_overlapping_selected_read(
    expr: &Expr,
    target: &LValue,
    value: &Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    let const_env = &packed_dimensions.const_env;
    let LValue::Select {
        name: target_name,
        msb: target_msb,
        lsb: target_lsb,
        ..
    } = target
    else {
        return None;
    };
    if let Expr::Ident(read_name) = expr {
        if read_name != target_name {
            return None;
        }
        let whole = whole_packed_lvalue(read_name, packed_dimensions)?;
        return selected_value_after_write(
            &Expr::Ident(read_name.clone()),
            &whole,
            target,
            value,
            packed_dimensions,
        )
        .map(|(value, _)| value);
    }
    let Expr::Select {
        expr: read_base,
        msb: read_msb,
        lsb: read_lsb,
        ..
    } = expr
    else {
        return None;
    };
    let Expr::Ident(read_name) = &**read_base else {
        return None;
    };
    if read_name != target_name {
        return None;
    }

    let read_msb = eval_ast_const_expr(read_msb, const_env)?;
    let read_lsb = eval_ast_const_expr(read_lsb, const_env)?;
    let target_msb = eval_ast_const_expr(target_msb, const_env)?;
    let target_lsb = eval_ast_const_expr(target_lsb, const_env)?;
    let overlap_low = read_msb.min(read_lsb).max(target_msb.min(target_lsb));
    let overlap_high = read_msb.max(read_lsb).min(target_msb.max(target_lsb));
    if overlap_low > overlap_high {
        return None;
    }

    let read_step = if read_msb >= read_lsb { -1 } else { 1 };
    let overlap_first = if read_step < 0 {
        overlap_high
    } else {
        overlap_low
    };
    let overlap_last = if read_step < 0 {
        overlap_low
    } else {
        overlap_high
    };
    let mut parts = Vec::new();
    if read_msb != overlap_first {
        parts.push(selected_ident_read(
            read_name,
            read_msb,
            overlap_first.checked_sub(read_step)?,
        ));
    }

    let value_msb = selected_target_bit(target_msb, target_lsb, overlap_first)?;
    let value_lsb = selected_target_bit(target_msb, target_lsb, overlap_last)?;
    parts.push(Expr::Select {
        expr: Box::new(value.clone()),
        msb: const_expr_from_i128(value_msb),
        lsb: const_expr_from_i128(value_lsb),
        signed: false,
    });

    if overlap_last != read_lsb {
        parts.push(selected_ident_read(
            read_name,
            overlap_last.checked_add(read_step)?,
            read_lsb,
        ));
    }
    if parts.len() == 1 {
        parts.pop()
    } else {
        Some(Expr::Concat(parts))
    }
}

fn selected_target_bit(target_msb: i128, target_lsb: i128, coordinate: i128) -> Option<i128> {
    if target_msb >= target_lsb {
        coordinate.checked_sub(target_lsb)
    } else {
        target_lsb.checked_sub(coordinate)
    }
}

fn selected_ident_read(name: &str, msb: i128, lsb: i128) -> Expr {
    Expr::Select {
        expr: Box::new(Expr::Ident(name.to_string())),
        msb: const_expr_from_i128(msb),
        lsb: const_expr_from_i128(lsb),
        signed: false,
    }
}

fn expr_matches_lvalue(expr: &Expr, target: &LValue) -> bool {
    match (expr, target) {
        (Expr::Ident(expr_name), LValue::Ident(target_name)) => expr_name == target_name,
        (
            Expr::Select { expr, msb, lsb, .. },
            LValue::Select {
                name,
                msb: target_msb,
                lsb: target_lsb,
                ..
            },
        ) => {
            matches!(&**expr, Expr::Ident(expr_name) if expr_name == name)
                && msb == target_msb
                && lsb == target_lsb
        }
        _ => false,
    }
}

pub(super) fn expr_references_overlapping_lvalue(
    expr: &Expr,
    target: &LValue,
    const_env: &HashMap<String, i128>,
) -> bool {
    if expr_matches_lvalue(expr, target) {
        return true;
    }
    match expr {
        Expr::Ident(name) => match target {
            LValue::Ident(target_name)
            | LValue::Select {
                name: target_name, ..
            } => name == target_name,
        },
        Expr::Literal(_) => false,
        Expr::Select {
            expr: selected,
            msb,
            lsb,
            ..
        } => {
            let selected_reference = if let Expr::Ident(read_name) = &**selected {
                match target {
                    LValue::Ident(target_name) => read_name == target_name,
                    LValue::Select {
                        name: target_name,
                        msb: target_msb,
                        lsb: target_lsb,
                        ..
                    } if read_name == target_name => match (
                        eval_ast_const_expr(msb, const_env),
                        eval_ast_const_expr(lsb, const_env),
                        eval_ast_const_expr(target_msb, const_env),
                        eval_ast_const_expr(target_lsb, const_env),
                    ) {
                        (Some(msb), Some(lsb), Some(target_msb), Some(target_lsb)) => {
                            msb.min(lsb) <= target_msb.max(target_lsb)
                                && target_msb.min(target_lsb) <= msb.max(lsb)
                        }
                        _ => true,
                    },
                    _ => false,
                }
            } else {
                expr_references_overlapping_lvalue(selected, target, const_env)
            };
            selected_reference
                || expr_references_overlapping_lvalue(
                    &const_expr_to_expr(msb.clone()),
                    target,
                    const_env,
                )
                || expr_references_overlapping_lvalue(
                    &const_expr_to_expr(lsb.clone()),
                    target,
                    const_env,
                )
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            expr_references_overlapping_lvalue(expr, target, const_env)
        }
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => parts
            .iter()
            .any(|part| expr_references_overlapping_lvalue(part, target, const_env)),
        Expr::Binary { left, right, .. } => {
            expr_references_overlapping_lvalue(left, target, const_env)
                || expr_references_overlapping_lvalue(right, target, const_env)
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_references_overlapping_lvalue(condition, target, const_env)
                || expr_references_overlapping_lvalue(then_expr, target, const_env)
                || expr_references_overlapping_lvalue(else_expr, target, const_env)
        }
        Expr::Call { args, .. } => args
            .iter()
            .any(|arg| expr_references_overlapping_lvalue(arg, target, const_env)),
    }
}
