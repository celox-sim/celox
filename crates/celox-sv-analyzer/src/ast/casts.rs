//! Cast target resolution and constant literal conversion.

use super::*;

pub(super) fn expr_type_from_type(
    state: &AnalysisState,
    r#type: &Type,
    const_env: &HashMap<String, i128>,
) -> Option<ExprType> {
    if !r#type.unpacked_ranges().is_empty() {
        return None;
    }
    let width = r#type
        .packed_ranges()
        .iter()
        .try_fold(1usize, |width, range| {
            let left = eval_ast_const_expr(state, range.left(), const_env)?;
            let right = eval_ast_const_expr(state, range.right(), const_env)?;
            width.checked_mul(usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?)
        })?;
    Some(ExprType {
        width: width.max(1),
        signed: r#type.is_signed(),
    })
}

#[derive(Clone, Copy)]
pub(super) enum RuntimeCastTarget {
    Real {
        short: bool,
    },
    /// `string'(x)` (IEEE 1800-2023 6.24.1).
    String,
    Zero(ExprType),
    Signing(bool),
    Integral {
        target: ExprType,
        numeric_size: bool,
        two_state: bool,
    },
}

/// Lower a runtime cast of `expr`.
///
/// A size cast `N'(x)` and `signed'(x)` / `unsigned'(x)` keep or set the
/// operand's own signedness, while a type cast `T'(x)` yields `T`'s signedness.
/// In every case the operand is extended according to its own signedness, as in
/// an assignment to a variable of the target type. Casts to two-state types
/// turn unknown bits into zero.
pub(super) fn runtime_cast_target_expr(
    state: &AnalysisState,
    target: RuntimeCastTarget,
    expr: Expr,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let const_env = &packed_dimensions.const_env;
    let (target, numeric_size, two_state) = match target {
        RuntimeCastTarget::String => {
            return Ok(expression_binding::string_operand(expr, packed_dimensions));
        }
        RuntimeCastTarget::Real { short } => {
            let call = |function, value| Expr::Call {
                callee: Callee::System(function),
                args: vec![Some(value)],
                arg_order: Vec::new(),
            };
            return Ok(if short {
                call(
                    SystemFunction::Bitstoshortreal,
                    call(SystemFunction::Shortrealtobits, expr),
                )
            } else {
                call(
                    SystemFunction::Bitstoreal,
                    call(SystemFunction::Realtobits, expr),
                )
            });
        }
        RuntimeCastTarget::Zero(target) => {
            return Ok(Expr::Resize {
                expr: Box::new(expr),
                width: target.width,
                signed: target.signed,
            });
        }
        RuntimeCastTarget::Signing(signed) => {
            let width = expr_static_width(state, &expr, packed_dimensions).ok_or_else(|| {
                unsupported("signedness cast of an operand without a static width")
            })?;
            return Ok(Expr::Resize {
                expr: Box::new(expr),
                width,
                signed,
            });
        }
        RuntimeCastTarget::Integral {
            target,
            numeric_size,
            two_state,
        } => (target, numeric_size, two_state),
    };
    let operand_signed = dimensions::expression_signedness(&expr, packed_dimensions, const_env)
        .ok_or_else(|| unsupported("cast of an operand whose signedness is unknown"))?;
    let expr = if two_state {
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(expr),
        }
    } else {
        expr
    };
    let resized = Expr::Resize {
        expr: Box::new(expr),
        width: target.width,
        signed: operand_signed,
    };
    if numeric_size || target.signed == operand_signed {
        Ok(resized)
    } else {
        Ok(Expr::Resize {
            expr: Box::new(resized),
            width: target.width,
            signed: target.signed,
        })
    }
}

/// Constant sizing, source-sign preservation and mask conversion independent
/// of the syntax adapter that supplied its target and operand.
pub(super) fn cast_constant_target(
    state: &AnalysisState,
    operand: ConstExpr,
    target: RuntimeCastTarget,
    const_env: &HashMap<String, i128>,
) -> Option<ConstExpr> {
    if let RuntimeCastTarget::Real { short } = target {
        let kind = if short {
            TypeKind::Shortreal
        } else {
            TypeKind::Real
        };
        return expr_to_const(
            state,
            parameters::real_literal(&operand, kind, const_env, &HashMap::default())?,
        );
    }
    fn contains_real(expr: &ConstExpr) -> bool {
        if typecheck::const_expr_has_real_type(&expr.clone().into()) {
            return true;
        }
        match expr {
            ConstExpr::Literal(text) => {
                text.contains(['.', 'e', 'E']) && text.replace('_', "").parse::<f64>().is_ok()
            }
            ConstExpr::Unary { expr, .. } | ConstExpr::Resize { expr, .. } => contains_real(expr),
            ConstExpr::Binary { left, right, .. } => contains_real(left) || contains_real(right),
            _ => false,
        }
    }
    if contains_real(&operand) {
        let value = typecheck::eval_real_const_expr(&operand.clone().into(), const_env)?.round();
        let (width, signed) = match target {
            RuntimeCastTarget::Integral {
                target,
                numeric_size,
                ..
            } => (target.width, numeric_size || target.signed),
            RuntimeCastTarget::Zero(target) => (target.width, target.signed),
            RuntimeCastTarget::Signing(_)
            | RuntimeCastTarget::Real { .. }
            | RuntimeCastTarget::String => return None,
        };
        if width == 0 || width > 128 {
            return None;
        }
        let magnitude = (value.abs() % 2.0f64.powi(width as i32)) as u128;
        let bits = if value.is_sign_negative() {
            0u128.wrapping_sub(magnitude)
        } else {
            magnitude
        };
        let bits = if width == 128 {
            bits
        } else {
            bits & ((1u128 << width) - 1)
        };
        return Some(ConstExpr::Literal(format_typed_parameter_literal(
            bits as i128,
            width,
            signed,
        )));
    }
    let parameter_types = parameter_types_from_const_env(const_env);
    let operand_type = infer_const_expr_type(&operand, &parameter_types)?;
    let literal = if let ConstExpr::Literal(literal) = &operand {
        typecheck::parse_integral_literal(literal)?
    } else {
        let ir_operand: crate::ir::ConstExpr = operand.clone().into();
        let ir_parameter_types = parameter_types
            .iter()
            .map(|(name, r#type)| (name.clone(), (r#type.width, r#type.signed)))
            .collect();
        typecheck::eval_const_integral_literal_with_types(
            &ir_operand,
            const_env,
            &ir_parameter_types,
        )
        .or_else(|| {
            let operand_value = eval_ast_const_expr(state, &operand, const_env)?;
            let operand_literal = format_typed_parameter_literal(
                operand_value,
                operand_type.width,
                operand_type.signed,
            );
            typecheck::parse_integral_literal(&operand_literal)
        })?
    };
    let (target_type, numeric_size, two_state) = match target {
        RuntimeCastTarget::Real { .. } | RuntimeCastTarget::String => return None,
        RuntimeCastTarget::Signing(signed) => (
            ExprType {
                width: literal.width,
                signed,
            },
            false,
            false,
        ),
        RuntimeCastTarget::Integral {
            target,
            numeric_size,
            two_state,
        } => (target, numeric_size, two_state),
        // Native zero-target planning already resolved its source signedness.
        RuntimeCastTarget::Zero(target) => (target, false, false),
    };
    let signed = if numeric_size {
        literal.signed
    } else {
        target_type.signed
    };
    let resized = match &operand {
        ConstExpr::Literal(value) => {
            resize_unbased_fill_literal_for_cast(value, target_type.width, signed).unwrap_or_else(
                || resize_integral_literal_for_cast(literal, target_type.width, signed),
            )
        }
        _ => resize_integral_literal_for_cast(literal, target_type.width, signed),
    };
    let resized = if two_state {
        two_state_integral_literal(&resized, target_type.width, signed)?
    } else {
        resized
    };
    Some(ConstExpr::Literal(resized))
}

fn two_state_integral_literal(value: &str, width: usize, signed: bool) -> Option<String> {
    let mut literal = typecheck::parse_integral_literal(value)?;
    let keep = (num_bigint::BigUint::from(1usize) << width) - num_bigint::BigUint::from(1usize);
    literal.value &= &keep ^ &literal.mask;
    literal.mask = num_bigint::BigUint::default();
    Some(resize_integral_literal_for_cast(literal, width, signed))
}

pub(super) fn resize_unbased_fill_literal_for_cast(
    value: &str,
    width: usize,
    signed: bool,
) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    let mut chars = normalized.chars();
    (chars.next()? == '\'' && chars.clone().count() == 1).then_some(())?;
    let fill = chars.next()?;
    matches!(fill, '0' | '1' | 'x' | 'z' | '?').then_some(())?;
    let signing = if signed { "s" } else { "" };
    Some(format!(
        "{width}'{signing}b{}",
        fill.to_string().repeat(width)
    ))
}

pub(super) fn resize_integral_literal_for_cast(
    literal: typecheck::IntegralLiteral,
    width: usize,
    signed: bool,
) -> String {
    let mut value = literal.value;
    let mut mask = literal.mask;
    if literal.signed && literal.width > 0 && literal.width < width {
        let extension = ((num_bigint::BigUint::from(1usize) << (width - literal.width))
            - num_bigint::BigUint::from(1usize))
            << literal.width;
        if value.bit((literal.width - 1) as u64) {
            value |= &extension;
        }
        if mask.bit((literal.width - 1) as u64) {
            mask |= extension;
        }
    }
    let keep = (num_bigint::BigUint::from(1usize) << width) - num_bigint::BigUint::from(1usize);
    value &= &keep;
    mask &= keep;
    let signing = if signed { "s" } else { "" };
    if mask == num_bigint::BigUint::default() {
        return format!("{width}'{signing}d{value}");
    }
    let bits = (0..width)
        .rev()
        .map(|bit| {
            if mask.bit(bit as u64) {
                if value.bit(bit as u64) { 'x' } else { 'z' }
            } else if value.bit(bit as u64) {
                '1'
            } else {
                '0'
            }
        })
        .collect::<String>();
    format!("{width}'{signing}b{bits}")
}
