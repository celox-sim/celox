//! Constant mux simplification and four-state literal folding.

use super::*;

const MAX_CONSTANT_CONCAT_BITS: usize = 65_536;

pub(super) fn simplify_constant_mux_conditions(
    expr: Expr,
    const_env: &HashMap<String, i128>,
) -> Expr {
    match expr {
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(simplify_constant_mux_conditions(*expr, const_env)),
            msb,
            lsb,
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| simplify_constant_mux_conditions(part, const_env))
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts
                .into_iter()
                .map(|part| simplify_constant_mux_conditions(part, const_env))
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => {
            let expr = simplify_constant_mux_conditions(*expr, const_env);
            if let Some(constant) = expr_to_const(expr.clone()) {
                let parameter_types = parameter_types_from_const_env(const_env)
                    .into_iter()
                    .map(|(name, r#type)| (name, (r#type.width, r#type.signed)))
                    .collect();
                if let Some(literal) = typecheck::context_size_const_integral_literal(
                    &crate::ir::ConstExpr::from(constant.clone()),
                    const_env,
                    &parameter_types,
                    width,
                    signed,
                ) {
                    return Expr::Literal(typecheck::format_integral_literal_binary(&literal));
                }
                if let Some(value) = eval_ast_const_expr(&constant, const_env) {
                    return Expr::Literal(format_typed_parameter_literal(value, width, signed));
                }
            }
            Expr::Resize {
                expr: Box::new(expr),
                width,
                signed,
            }
        }
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(simplify_constant_mux_conditions(*expr, const_env)),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(simplify_constant_mux_conditions(*left, const_env)),
            op,
            right: Box::new(simplify_constant_mux_conditions(*right, const_env)),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let condition = simplify_constant_mux_conditions(*condition, const_env);
            let mut then_expr = simplify_constant_mux_conditions(*then_expr, const_env);
            let mut else_expr = simplify_constant_mux_conditions(*else_expr, const_env);
            let parameter_types = parameter_types_from_const_env(const_env);
            let arm_type = |arm: &Expr| {
                if let Expr::Literal(literal) = arm
                    && resize_unbased_fill_literal_for_cast(literal, 1, false).is_some()
                {
                    Some(ExprType {
                        width: 1,
                        signed: false,
                    })
                } else if let Expr::Resize { width, signed, .. } = arm {
                    Some(ExprType {
                        width: *width,
                        signed: *signed,
                    })
                } else {
                    infer_const_expr_type(&expr_to_const(arm.clone())?, &parameter_types)
                }
            };
            let result_type =
                arm_type(&then_expr)
                    .zip(arm_type(&else_expr))
                    .map(|(then_type, else_type)| ExprType {
                        width: then_type.width.max(else_type.width),
                        signed: then_type.signed && else_type.signed,
                    });
            // Repeated-condition mux folding is only valid for a condition
            // that cannot be X/Z. Procedural guards are explicitly coerced
            // to two state, but source-level ternaries need not be.
            if expr_is_intrinsically_two_state(&condition, const_env) {
                if let Expr::Mux {
                    condition: nested_condition,
                    then_expr: nested_then,
                    ..
                } = &then_expr
                    && **nested_condition == condition
                {
                    then_expr = (**nested_then).clone();
                }
                if let Expr::Mux {
                    condition: nested_condition,
                    else_expr: nested_else,
                    ..
                } = &else_expr
                    && **nested_condition == condition
                {
                    else_expr = (**nested_else).clone();
                }
            }
            if then_expr == else_expr {
                return then_expr;
            }
            if let Some(value) = equivalent_constant_mux_value(&then_expr, &else_expr, const_env) {
                return value;
            }
            match expr_to_const(condition.clone())
                .and_then(|condition| eval_ast_const_expr(&condition, const_env))
            {
                Some(value) => {
                    let selected = if value == 0 { else_expr } else { then_expr };
                    let Some(result_type) = result_type else {
                        return selected;
                    };
                    if let Expr::Literal(literal) = &selected {
                        if let Some(resized) = resize_unbased_fill_literal_for_cast(
                            literal,
                            result_type.width,
                            result_type.signed,
                        ) {
                            return Expr::Literal(resized);
                        }
                    }
                    let Some(selected_type) = arm_type(&selected) else {
                        return selected;
                    };
                    // Both arms determine a ternary's type, even when its
                    // condition is known. Set that signedness before extending
                    // the chosen value so an unsigned peer prevents sign extension.
                    let selected = Expr::Resize {
                        expr: Box::new(selected),
                        width: selected_type.width,
                        signed: result_type.signed,
                    };
                    simplify_constant_mux_conditions(
                        Expr::Resize {
                            expr: Box::new(selected),
                            width: result_type.width,
                            signed: result_type.signed,
                        },
                        const_env,
                    )
                }
                None => Expr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                },
            }
        }
        Expr::Inside { expr, items } => Expr::Inside {
            expr: Box::new(simplify_constant_mux_conditions(*expr, const_env)),
            items: items
                .into_iter()
                .map(|item| {
                    item.map(&mut |operand| simplify_constant_mux_conditions(operand, const_env))
                })
                .collect(),
        },
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args
                .into_iter()
                .map(|arg| simplify_constant_mux_conditions(arg, const_env))
                .collect(),
        },
        Expr::Ident(_) | Expr::Literal(_) => expr,
    }
}

pub(super) fn fold_const_integral_expr_preserving_mask(
    expr: Expr,
    const_env: &HashMap<String, i128>,
) -> Expr {
    // Keep unbased fill literals available for their later comparison context.
    // Literal expressions already retain their value, type, and X/Z mask.
    if matches!(expr, Expr::Literal(_)) {
        return expr;
    }
    let parameter_types = parameter_types_from_const_env(const_env)
        .into_iter()
        .map(|(name, r#type)| (name, (r#type.width, r#type.signed)))
        .collect();
    eval_const_integral_expr_preserving_mask(&expr, const_env, &parameter_types)
        .map(|literal| Expr::Literal(typecheck::format_integral_literal_binary(&literal)))
        .unwrap_or(expr)
}

fn eval_const_integral_expr_preserving_mask(
    expr: &Expr,
    const_env: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<typecheck::IntegralLiteral> {
    match expr {
        Expr::Select { expr, msb, lsb, .. } => {
            let operand =
                eval_const_integral_expr_preserving_mask(expr, const_env, parameter_types)?;
            let msb = eval_ast_const_expr(msb, const_env)?;
            let lsb = eval_ast_const_expr(lsb, const_env)?;
            let width = usize::try_from(msb.checked_sub(lsb)?.checked_add(1)?).ok()?;
            let mut value = num_bigint::BigUint::default();
            let mut mask = num_bigint::BigUint::default();
            for offset in 0..width {
                let bit = lsb.checked_add(i128::try_from(offset).ok()?)?;
                let index = usize::try_from(bit).ok().filter(|bit| *bit < operand.width);
                let (value_bit, mask_bit) = match index {
                    Some(index) => (
                        operand.value.bit(index as u64),
                        operand.mask.bit(index as u64),
                    ),
                    // Out-of-range bits of a constant part-select are X.
                    None => (true, true),
                };
                value.set_bit(offset as u64, value_bit);
                mask.set_bit(offset as u64, mask_bit);
            }
            Some(typecheck::IntegralLiteral {
                width,
                signed: false,
                value,
                mask,
            })
        }

        Expr::Resize {
            expr,
            width,
            signed,
        } => {
            let operand =
                eval_const_integral_expr_preserving_mask(expr, const_env, parameter_types)?;
            typecheck::parse_integral_literal(&resize_integral_literal_for_cast(
                operand, *width, *signed,
            ))
        }
        Expr::Concat(parts) => concat_integral_literals(
            parts
                .iter()
                .map(|part| {
                    eval_const_integral_expr_preserving_mask(part, const_env, parameter_types)
                })
                .collect::<Option<Vec<_>>>()?,
        ),
        Expr::RepeatConcat { count, parts } => {
            let count = usize::try_from(eval_ast_const_expr(count, const_env)?).ok()?;
            let part = concat_integral_literals(
                parts
                    .iter()
                    .map(|part| {
                        eval_const_integral_expr_preserving_mask(part, const_env, parameter_types)
                    })
                    .collect::<Option<Vec<_>>>()?,
            )?;
            if count > MAX_CONSTANT_CONCAT_BITS
                || part.width.checked_mul(count)? > MAX_CONSTANT_CONCAT_BITS
            {
                return None;
            }
            concat_integral_literals(std::iter::repeat_n(part, count))
        }
        _ => {
            let constant: crate::ir::ConstExpr =
                constant_with_folded_selections(expr, const_env, parameter_types)?.into();
            typecheck::eval_const_integral_literal_with_types(&constant, const_env, parameter_types)
        }
    }
}

// Fold only self-determined operands. Keep arithmetic nodes intact so the
// constant evaluator can propagate expression widths across compound trees.
pub(super) fn constant_with_folded_selections(
    expr: &Expr,
    const_env: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<ConstExpr> {
    let convert = |expr: &Expr| constant_with_folded_selections(expr, const_env, parameter_types);
    match expr {
        Expr::Select { .. } | Expr::Concat(_) | Expr::RepeatConcat { .. } | Expr::Resize { .. } => {
            let literal =
                eval_const_integral_expr_preserving_mask(expr, const_env, parameter_types)?;
            Some(ConstExpr::Literal(
                typecheck::format_integral_literal_binary(&literal),
            ))
        }
        Expr::Unary { op, expr } => Some(ConstExpr::Unary {
            op: *op,
            expr: Box::new(convert(expr)?),
        }),
        Expr::Binary { left, op, right } => Some(ConstExpr::Binary {
            left: Box::new(convert(left)?),
            op: *op,
            right: Box::new(convert(right)?),
        }),
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Some(ConstExpr::Mux {
            condition: Box::new(convert(condition)?),
            then_expr: Box::new(convert(then_expr)?),
            else_expr: Box::new(convert(else_expr)?),
        }),
        Expr::Call { name, args } => Some(ConstExpr::Function {
            name: name.clone(),
            args: args.iter().map(convert).collect::<Option<Vec<_>>>()?,
        }),
        _ => expr_to_const(expr.clone()),
    }
}

fn concat_integral_literals(
    parts: impl IntoIterator<Item = typecheck::IntegralLiteral>,
) -> Option<typecheck::IntegralLiteral> {
    let mut width = 0usize;
    let mut value = num_bigint::BigUint::default();
    let mut mask = num_bigint::BigUint::default();
    for part in parts {
        width = width.checked_add(part.width)?;
        if width > MAX_CONSTANT_CONCAT_BITS {
            return None;
        }
        value = (value << part.width) | part.value;
        mask = (mask << part.width) | part.mask;
    }
    Some(typecheck::IntegralLiteral {
        width,
        signed: false,
        value,
        mask,
    })
}

fn expr_is_intrinsically_two_state(expr: &Expr, const_env: &HashMap<String, i128>) -> bool {
    match expr {
        Expr::Ident(name) => const_env.contains_key(name),
        Expr::Literal(value) => typecheck::parse_integral_literal(value)
            .is_some_and(|literal| literal.mask == num_bigint::BigUint::default()),
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            ..
        } => true,
        Expr::Unary { expr, .. } => expr_is_intrinsically_two_state(expr, const_env),
        Expr::Binary {
            op: BinaryOp::EqCase | BinaryOp::NeCase,
            ..
        } => true,
        Expr::Binary {
            left,
            op: BinaryOp::LogicAnd | BinaryOp::LogicOr,
            right,
        } => {
            expr_is_intrinsically_two_state(left, const_env)
                && expr_is_intrinsically_two_state(right, const_env)
        }
        _ => false,
    }
}

fn equivalent_constant_mux_value(
    then_expr: &Expr,
    else_expr: &Expr,
    const_env: &HashMap<String, i128>,
) -> Option<Expr> {
    let parameter_types = parameter_types_from_const_env(const_env);
    let ir_parameter_types = parameter_types
        .iter()
        .map(|(name, r#type)| (name.clone(), (r#type.width, r#type.signed)))
        .collect::<HashMap<_, _>>();
    let then_value = typecheck::eval_const_integral_literal_with_types(
        &expr_to_const(then_expr.clone())?.into(),
        const_env,
        &ir_parameter_types,
    )?;
    let else_value = typecheck::eval_const_integral_literal_with_types(
        &expr_to_const(else_expr.clone())?.into(),
        const_env,
        &ir_parameter_types,
    )?;
    let width = then_value.width.max(else_value.width);
    let signed = then_value.signed && else_value.signed;
    let then_value = resize_integral_literal_for_cast(then_value, width, signed);
    let else_value = resize_integral_literal_for_cast(else_value, width, signed);
    (then_value == else_value).then_some(Expr::Literal(then_value))
}

#[cfg(test)]
mod constant_concat_limits_tests {
    use super::*;

    #[test]
    fn bounds_repeated_constant_concatenation() {
        for (count, accepted) in [(65_536, true), (65_537, false), (1_000_000_000, false)] {
            let expr = Expr::RepeatConcat {
                count: ConstExpr::Literal(count.to_string()),
                parts: vec![Expr::Literal("1'bx".to_string())],
            };
            let result = eval_const_integral_expr_preserving_mask(
                &expr,
                &HashMap::default(),
                &HashMap::default(),
            );
            assert_eq!(result.is_some(), accepted);
            if let Some(result) = result {
                assert_eq!(result.width, count);
                assert!(result.mask.bit((count - 1) as u64));
            }
        }
        let expr = Expr::RepeatConcat {
            count: ConstExpr::Literal("40000".to_string()),
            parts: vec![Expr::Literal("2'bxz".to_string())],
        };
        assert!(
            eval_const_integral_expr_preserving_mask(
                &expr,
                &HashMap::default(),
                &HashMap::default(),
            )
            .is_none()
        );
    }
}
