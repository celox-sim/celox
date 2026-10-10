use std::{cell::Cell, fmt::Write, path::Path};

use super::*;

#[test]
fn lazy_types_preserve_width_signedness_and_masks_across_expression_shapes() {
    let mut env = HashMap::from_iter([
        ("A".into(), -2),
        ("B".into(), 1),
        ("W".into(), -1),
        ("index".into(), 3),
    ]);
    for (name, width, signed) in [("A", 8, true), ("B", 4, false), ("W", 129, true)] {
        insert_parameter_type_markers(&mut env, name, ExprType { width, signed });
    }
    // Negative widths are ignored by both the old table and direct lookup.
    env.insert(parameter_width_marker("invalid"), -1);
    for index in 0..4096 {
        insert_parameter_type_markers(
            &mut env,
            &format!("unrelated{index}"),
            ExprType {
                width: 32,
                signed: false,
            },
        );
    }
    let types: HashMap<_, _> = parameter_types_from_const_env(&env)
        .into_iter()
        .map(|(name, ty)| (name, (ty.width, ty.signed)))
        .collect();
    let ident = |name: &str| Expr::Ident(name.into());
    let literal = |value: &str| Expr::Literal(value.into());
    let boxed_a = || Box::new(ident("A"));
    let cases = [
        (ident("A"), 1),
        (ident("W"), 1),
        (ident("unknown"), 1),
        (ident("invalid"), 1),
        (literal("'z"), 0),
        (
            Expr::Unary {
                op: UnaryOp::ToTwoState,
                expr: Box::new(literal("8'b1xz001xz")),
            },
            0,
        ),
        (
            Expr::Unary {
                op: UnaryOp::Minus,
                expr: boxed_a(),
            },
            1,
        ),
        (
            Expr::Binary {
                left: boxed_a(),
                op: BinaryOp::Shr,
                right: Box::new(literal("1")),
            },
            1,
        ),
        (
            Expr::Binary {
                left: boxed_a(),
                op: BinaryOp::Sar,
                right: Box::new(literal("1")),
            },
            1,
        ),
        (
            Expr::Binary {
                left: boxed_a(),
                op: BinaryOp::Add,
                right: Box::new(ident("B")),
            },
            2,
        ),
        (
            Expr::Binary {
                left: Box::new(literal("8'bx1z00000")),
                op: BinaryOp::BitAnd,
                right: Box::new(literal("8'h0f")),
            },
            0,
        ),
        (
            Expr::Resize {
                expr: boxed_a(),
                width: 129,
                signed: true,
            },
            1,
        ),
        (
            Expr::Select {
                expr: boxed_a(),
                msb: ConstExpr::Ident("index".into()),
                lsb: ConstExpr::Literal("0".into()),
                signed: false,
            },
            1,
        ),
        (
            Expr::Select {
                expr: boxed_a(),
                msb: ConstExpr::Literal("9".into()),
                lsb: ConstExpr::Literal("7".into()),
                signed: false,
            },
            1,
        ),
        (Expr::Concat(vec![ident("A"), literal("4'bxz01")]), 1),
        (
            Expr::RepeatConcat {
                count: ConstExpr::Ident("index".into()),
                parts: vec![ident("B"), literal("1'bz")],
            },
            1,
        ),
        (
            Expr::Mux {
                condition: Box::new(literal("1'bx")),
                then_expr: boxed_a(),
                else_expr: Box::new(literal("8'b1111xxxx")),
            },
            1,
        ),
        (
            Expr::Call {
                name: "$countones".into(),
                args: vec![ident("A")],
            },
            1,
        ),
        (
            Expr::Call {
                name: "unresolved_function".into(),
                args: vec![ident("B")],
            },
            1,
        ),
        (
            Expr::Inside {
                expr: boxed_a(),
                items: vec![InsideItem::Value(ident("B"))],
            },
            0,
        ),
    ];
    for (expr, expected_lookups) in cases {
        let lookups = Cell::new(0);
        let direct = eval_const_integral_expr_preserving_mask(&expr, &env, &|name| {
            lookups.set(lookups.get() + 1);
            parameter_type_from_const_env(&env, name).map(|ty| (ty.width, ty.signed))
        });
        let rebuilt =
            eval_const_integral_expr_preserving_mask(&expr, &env, &|name| types.get(name).copied());
        assert_eq!(direct, rebuilt, "{expr:?}");
        assert_eq!(lookups.get(), expected_lookups, "{expr:?}");
        let expected = rebuilt
            .map(|value| Expr::Literal(typecheck::format_integral_literal_binary(&value)))
            .unwrap_or_else(|| expr.clone());
        // Bare literals deliberately bypass folding to retain unbased fills.
        let expected = if matches!(expr, Expr::Literal(_)) {
            expr.clone()
        } else {
            expected
        };
        assert_eq!(
            fold_const_integral_expr_preserving_mask(expr.clone(), &env),
            expected
        );
    }
    // Fixed expected results also guard against losing typed signed shifts.
    let shifted = fold_const_integral_expr_preserving_mask(
        Expr::Binary {
            left: boxed_a(),
            op: BinaryOp::Shr,
            right: Box::new(literal("1")),
        },
        &env,
    );
    let Expr::Literal(shifted) = shifted else {
        panic!("constant shift must fold")
    };
    let shifted = typecheck::parse_integral_literal(&shifted).unwrap();
    assert_eq!(shifted.width, 8);
    assert!(shifted.signed);
    assert_eq!(shifted.value, num_bigint::BigUint::from(127u8));
    let two_state = fold_const_integral_expr_preserving_mask(
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(literal("8'b1xz001xz")),
        },
        &env,
    );
    let Expr::Literal(two_state) = two_state else {
        panic!("two-state value must fold")
    };
    assert_eq!(
        typecheck::parse_integral_literal(&two_state),
        typecheck::parse_integral_literal("8'h84")
    );
    let wide = fold_const_integral_expr_preserving_mask(ident("W"), &env);
    let Expr::Literal(wide) = wide else {
        panic!("wide typed parameter must fold")
    };
    let wide = typecheck::parse_integral_literal(&wide).unwrap();
    assert_eq!(wide.width, 129);
    assert!(wide.signed);
    assert_eq!(
        wide.value,
        (num_bigint::BigUint::from(1u8) << 129) - num_bigint::BigUint::from(1u8)
    );
}

#[test]
fn numeric_two_state_parameter_values_do_not_scan_visible_type_tables() {
    for count in [16, 64, 256] {
        let mut code = String::from("module Top;\n");
        for index in 0..count {
            let value = if index == 0 {
                "1".into()
            } else {
                format!("P{} + 1", index - 1)
            };
            writeln!(code, "localparam int P{index} = {value};").unwrap();
        }
        code.push_str("endmodule\n");
        let tree = crate::syntax::parse_source(&code, Path::new("two_state_parameter_scaling.sv"))
            .unwrap();
        let source = Source::from_syntax(&tree).unwrap();
        let parameters = source.modules()[0].parameters();
        let env = const_env_from_parameters(parameters);
        dimensions::PARAMETER_TYPE_SCAN_ENTRIES.with(|counter| counter.set(0));
        let values = parameter_value_env(parameters, &env);
        assert_eq!(
            dimensions::PARAMETER_TYPE_SCAN_ENTRIES.with(|counter| counter.get()),
            0
        );
        assert_eq!(values.len(), count);
        for (index, parameter) in parameters.iter().enumerate() {
            let Expr::Literal(value) = &values[parameter.name()] else {
                panic!("parameter must fold")
            };
            let value = typecheck::parse_integral_literal(value).unwrap();
            assert_eq!(value.width, 32);
            assert!(value.signed);
            assert_eq!(value.value, num_bigint::BigUint::from(index + 1));
        }
        dimensions::PARAMETER_TYPE_SCAN_ENTRIES.with(|counter| counter.set(0));
        let source = Source::from_syntax(&tree).unwrap();
        let scanned = dimensions::PARAMETER_TYPE_SCAN_ENTRIES.with(|counter| counter.get());
        assert!(
            scanned <= 256 * count,
            "{count} parameters scanned {scanned} type entries"
        );
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[count - 1].resolved_value(),
            Some(count as i128)
        );
    }
}
