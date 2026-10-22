//! Constant real expressions keep their floating type until an integer context.
use super::*;

pub(crate) fn has_real_type(expr: &ConstExpr) -> bool {
    match expr {
        ConstExpr::Literal(text) => {
            text.contains(['.', 'e', 'E']) && text.replace('_', "").parse::<f64>().is_ok()
        }
        ConstExpr::Function {
            callee: Callee::System(function),
            ..
        } => {
            function.is_real_math()
                || matches!(
                    function,
                    SystemFunction::Itor
                        | SystemFunction::Bitstoreal
                        | SystemFunction::Bitstoshortreal
                )
        }
        ConstExpr::Unary {
            op: UnaryOp::Plus | UnaryOp::Minus | UnaryOp::ToTwoState,
            expr,
        } => has_real_type(expr),
        ConstExpr::Binary {
            left,
            op: BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Pow,
            right,
        } => has_real_type(left) || has_real_type(right),
        ConstExpr::Mux {
            then_expr,
            else_expr,
            ..
        } => has_real_type(then_expr) || has_real_type(else_expr),
        _ => false,
    }
}

pub(super) fn needs_real_evaluation(expr: &ConstExpr) -> bool {
    has_real_type(expr)
        || match expr {
            ConstExpr::Unary {
                op: UnaryOp::LogicNot,
                expr,
            } => has_real_type(expr),
            ConstExpr::Binary {
                left,
                right,
                op:
                    BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
                    | BinaryOp::LogicAnd
                    | BinaryOp::LogicOr,
            } => has_real_type(left) || has_real_type(right),
            _ => false,
        }
}

/// Integer assignment/cast conversion, matching the runtime conversion's
/// modular low bits and known-zero result for non-finite values.
pub(crate) fn round_to_i128(value: f64) -> i128 {
    if !value.is_finite() {
        return 0;
    }
    let value = value.round();
    let magnitude = (value.abs() % 2f64.powi(128)) as u128;
    (if value.is_sign_negative() {
        magnitude.wrapping_neg()
    } else {
        magnitude
    }) as i128
}

pub(crate) fn evaluate(expr: &ConstExpr, constants: &dyn ConstantEnvironment) -> Option<f64> {
    if !needs_real_evaluation(expr) {
        let integral = ConstExpr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(expr.clone()),
        };
        if let Some(literal) = eval_const_integral_literal_in_env(&integral, constants, &|_| None) {
            let negative = literal.signed
                && literal.width > 0
                && literal.value.bit((literal.width - 1) as u64);
            let magnitude = if negative {
                (BigUint::from(1u8) << literal.width) - literal.value
            } else {
                literal.value
            };
            let value = magnitude.to_str_radix(10).parse::<f64>().ok()?;
            return Some(if negative { -value } else { value });
        }
        return eval_const_expr_in_env(&integral, constants).map(|value| value as f64);
    }
    match expr {
        ConstExpr::Literal(text) => text.replace('_', "").parse::<f64>().ok(),
        ConstExpr::Function {
            callee: Callee::System(function),
            args,
            ..
        } => {
            if function.is_real_math() {
                let args = args
                    .iter()
                    .map(|arg| evaluate(arg, constants))
                    .collect::<Option<Vec<_>>>()?;
                return function.eval_real_math(&args);
            }
            let [arg] = args.as_slice() else {
                return None;
            };
            match function {
                SystemFunction::Itor => {
                    let value = if has_real_type(arg) {
                        celox_design::eval_float_unary(
                            celox_design::FloatUnaryOp::ToSigned {
                                width: 32,
                                round: true,
                            },
                            celox_design::FloatPrecision::F64,
                            evaluate(arg, constants)?.to_bits(),
                        ) as u32 as i32
                    } else {
                        eval_const_expr_in_env(arg, constants)? as i32
                    };
                    Some(f64::from(value))
                }
                SystemFunction::Bitstoreal => Some(f64::from_bits(eval_const_expr_in_env(
                    arg, constants,
                )? as u64)),
                SystemFunction::Bitstoshortreal => Some(f64::from(f32::from_bits(
                    eval_const_expr_in_env(arg, constants)? as u32,
                ))),
                _ => None,
            }
        }
        ConstExpr::Unary { op, expr } => {
            let value = evaluate(expr, constants)?;
            match op {
                UnaryOp::Plus | UnaryOp::ToTwoState => Some(value),
                UnaryOp::Minus => Some(-value),
                UnaryOp::LogicNot => Some(f64::from(u8::from(value == 0.0))),
                _ => None,
            }
        }
        ConstExpr::Binary { left, op, right } => {
            let left = evaluate(left, constants)?;
            if *op == BinaryOp::LogicAnd && left == 0.0 {
                return Some(0.0);
            }
            if *op == BinaryOp::LogicOr && left != 0.0 {
                return Some(1.0);
            }
            let right = evaluate(right, constants)?;
            Some(match op {
                BinaryOp::Add => left + right,
                BinaryOp::Sub => left - right,
                BinaryOp::Mul => left * right,
                BinaryOp::Div => left / right,
                BinaryOp::Pow => f64::from_bits(celox_design::eval_float_binary(
                    celox_design::FloatBinaryOp::Pow,
                    celox_design::FloatPrecision::F64,
                    left.to_bits(),
                    right.to_bits(),
                )),
                BinaryOp::Eq => f64::from(u8::from(left == right)),
                BinaryOp::Ne => f64::from(u8::from(left != right)),
                BinaryOp::Lt => f64::from(u8::from(left < right)),
                BinaryOp::Le => f64::from(u8::from(left <= right)),
                BinaryOp::Gt => f64::from(u8::from(left > right)),
                BinaryOp::Ge => f64::from(u8::from(left >= right)),
                BinaryOp::LogicAnd => f64::from(u8::from(left != 0.0 && right != 0.0)),
                BinaryOp::LogicOr => f64::from(u8::from(left != 0.0 || right != 0.0)),
                _ => return None,
            })
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => evaluate(
            if evaluate(condition, constants)? != 0.0 {
                then_expr
            } else {
                else_expr
            },
            constants,
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn literal(value: &str) -> ConstExpr {
        ConstExpr::Literal(value.into())
    }
    fn call(name: &str, args: Vec<ConstExpr>) -> ConstExpr {
        ConstExpr::Function {
            callee: Callee::System(SystemFunction::from_name(name).unwrap()),
            args,
            site: None,
        }
    }
    #[test]
    fn nested_math_keeps_real_precision_until_integer_context() {
        let constants = HashMap::default();
        let nested = call("$ln", vec![call("$exp", vec![literal("0.25")])]);
        assert!((evaluate(&nested, &constants).unwrap() - 0.25).abs() < 1e-15);
        let sum = ConstExpr::Binary {
            left: Box::new(call("$sqrt", vec![literal("2.0")])),
            op: BinaryOp::Add,
            right: Box::new(call("$sqrt", vec![literal("2.0")])),
        };
        assert_eq!(eval_const_expr_in_env(&sum, &constants), None);
        let resized = ConstExpr::Resize {
            expr: Box::new(sum),
            width: 32,
            signed: true,
        };
        assert_eq!(eval_const_expr_in_env(&resized, &constants), Some(3));
        let relation = ConstExpr::Binary {
            left: Box::new(call("$sqrt", vec![literal("2.0")])),
            op: BinaryOp::Gt,
            right: Box::new(literal("1.4")),
        };
        assert_eq!(eval_const_expr_in_env(&relation, &constants), Some(1));
    }
    #[test]
    fn fractional_real_condition_is_not_rounded_before_selecting_integer_arms() {
        let constants = HashMap::default();
        let condition = call("$ln", vec![call("$exp", vec![literal("0.25")])]);
        let expression = ConstExpr::Mux {
            condition: Box::new(condition),
            then_expr: Box::new(literal("7")),
            else_expr: Box::new(literal("9")),
        };
        assert_eq!(eval_const_expr_in_env(&expression, &constants), Some(7));
    }
    #[test]
    fn unsigned_wide_math_actuals_keep_their_sign() {
        let constants = HashMap::default();
        assert_eq!(
            evaluate(
                &call(
                    "$sqrt",
                    vec![literal("128'h80000000000000000000000000000000")]
                ),
                &constants
            ),
            Some(2f64.powi(127).sqrt())
        );
    }
    #[test]
    fn integer_math_actuals_clear_unknown_bits_before_real_conversion() {
        let constants = HashMap::default();
        for value in ["8'hx9", "8'hz9"] {
            assert_eq!(
                evaluate(&call("$sqrt", vec![literal(value)]), &constants),
                Some(3.0)
            );
        }
    }
    #[test]
    fn integer_actuals_are_evaluated_before_real_promotion() {
        let constants = HashMap::default();
        let quotient = ConstExpr::Binary {
            left: Box::new(literal("1")),
            op: BinaryOp::Div,
            right: Box::new(literal("2")),
        };
        assert_eq!(
            evaluate(&call("$sqrt", vec![quotient]), &constants),
            Some(0.0)
        );
        let huge = call("$pow", vec![literal("2.0"), literal("40.0")]);
        assert_eq!(
            eval_const_expr_in_env(&call("$rtoi", vec![huge]), &constants),
            Some(0)
        );
        let ceil = call("$ceil", vec![literal("2.1")]);
        assert_eq!(eval_const_expr_in_env(&ceil, &constants), None);
        assert_eq!(
            eval_const_expr_in_env(
                &ConstExpr::Resize {
                    expr: Box::new(ceil),
                    width: 32,
                    signed: true,
                },
                &constants
            ),
            Some(3)
        );
    }
    #[test]
    fn integral_only_contexts_reject_real_results_and_preserve_integer_widths() {
        let constants = HashMap::default();
        for expr in [
            literal("1.5"),
            call("$sqrt", vec![literal("2.0")]),
            call("$floor", vec![literal("2.5")]),
        ] {
            assert_eq!(eval_const_expr_in_env(&expr, &constants), None);
        }
        for (literal, expected) in [
            ("8'shff", -1),
            ("8'hff", 255),
            ("128'h80000000000000000000000000000000", i128::MIN),
        ] {
            assert_eq!(
                eval_const_expr_in_env(&ConstExpr::Literal(literal.into()), &constants),
                Some(expected)
            );
        }
        let resized = ConstExpr::Resize {
            expr: Box::new(call("$pow", vec![literal("2.0"), literal("40.0")])),
            width: 8,
            signed: false,
        };
        assert_eq!(eval_const_expr_in_env(&resized, &constants), Some(0));
    }

    #[test]
    fn math_constant_domains_match_runtime_ieee_results() {
        let constants = HashMap::default();
        assert!(
            evaluate(&call("$sqrt", vec![literal("-1.0")]), &constants)
                .unwrap()
                .is_nan()
        );
        assert_eq!(
            evaluate(&call("$ln", vec![literal("0.0")]), &constants),
            Some(f64::NEG_INFINITY)
        );
        assert_eq!(
            evaluate(&call("$sin", vec![literal("-0.0")]), &constants)
                .unwrap()
                .to_bits(),
            (-0.0f64).to_bits()
        );
        assert_eq!(
            evaluate(
                &call("$atan2", vec![literal("1.0"), literal("-1.0")]),
                &constants
            ),
            Some(3.0 * std::f64::consts::FRAC_PI_4)
        );
    }
}
