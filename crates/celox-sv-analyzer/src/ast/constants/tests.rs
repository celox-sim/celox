use super::*;
use std::{fmt::Write, path::Path};

fn environment(count: usize) -> HashMap<String, i128> {
    let mut parameter = Parameter::new(
        "P".into(),
        Some(parameters::const_expr_from_i128(-2)),
        Some(8),
        Some(true),
        false,
        true,
        true,
    );
    parameter.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal("7".into()),
        ConstExpr::Literal("0".into()),
    ));
    let mut env = const_env_from_parameters(&[parameter]);
    env.insert("W".into(), 3);
    env.insert("Q".into(), 9);
    for i in 0..count {
        let name = format!("unrelated{i}");
        env.insert(name.clone(), i as i128);
        insert_parameter_type_markers(
            &mut env,
            &name,
            ExprType {
                width: 32,
                signed: true,
            },
        );
    }
    env
}

fn erase_sites(expr: &mut ConstExpr) {
    match expr {
        ConstExpr::Function { args, site, .. } => {
            *site = None;
            for arg in args {
                erase_sites(arg);
            }
        }
        ConstExpr::Select { expr, bit } => {
            erase_sites(expr);
            erase_sites(bit);
        }
        ConstExpr::Unary { expr, .. } => erase_sites(expr),
        ConstExpr::Binary { left, right, .. } => {
            erase_sites(left);
            erase_sites(right);
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            erase_sites(condition);
            erase_sites(then_expr);
            erase_sites(else_expr);
        }
        _ => {}
    }
}

#[test]
fn scalar_calls_match_full_context_and_preserve_contextual_operands() {
    let env = environment(4096);
    let mut alias = Type::new(TypeKind::Logic);
    alias.is_signed = true;
    alias.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal("7".into()),
        ConstExpr::Literal("0".into()),
    ));
    let aliases = HashMap::from_iter([("byte_t".into(), alias)]);
    for (args, context_free) in [
        ("", true),
        ("P", true),
        ("8'hf", true),
        ("-(P + 3)", true),
        ("P ~^ 7", true),
        ("(P * 3 + Q) >> 1", true),
        ("g(P + 1)", true),
        ("g()", true),
        ("P + 1 / 0", true),
        ("P, Q + 1", true),
        ("P[W-1]", false),
        ("P[0 +: W]", false),
        ("W'(P)", false),
        ("byte_t'(P)", false),
        ("$bits(P)", false),
        ("$clog2(W)", false),
        ("P ? 1 : 2", false),
        ("{P[7:0], 8'h12}", false),
        ("'{default: 1}", false),
        (".x(P)", false),
        ("P, ", false),
    ] {
        let code = format!("module Top; localparam int RESULT = f({args}); endmodule");
        let tree = crate::syntax::parse_source(&code, Path::new("call_context.sv")).unwrap();
        let primary = (&tree)
            .into_iter()
            .find_map(|node| match node {
                RefNode::ConstantPrimary(primary)
                    if matches!(primary, sv_parser::ConstantPrimary::ConstantFunctionCall(_)) =>
                {
                    Some(primary)
                }
                _ => None,
            })
            .unwrap();
        let sv_parser::ConstantPrimary::ConstantFunctionCall(call) = primary else {
            unreachable!()
        };
        let sv_parser::SubroutineCall::TfCall(tf_call) = &call.nodes.0.nodes.0 else {
            unreachable!()
        };
        assert_eq!(
            call_arguments_are_context_free(tf_call),
            context_free,
            "{args}"
        );
        // Execute the old path independently; keep the same expression lowering
        // and compare representations, including existing unsupported cases.
        let mut copied = expr_from_function_subroutine_call(
            &call.nodes.0,
            &tree,
            &PackedDimensions::new(HashMap::default(), &env, &aliases),
        )
        .ok()
        .and_then(expr_to_const);
        CALL_CONTEXT_COPIES.with(|count| count.set(0));
        let mut lazy = const_expr_from_ref_node_with_env(
            RefNode::ConstantPrimary(primary),
            &tree,
            &env,
            &aliases,
        )
        .unwrap();
        CALL_CONTEXT_COPIES.with(|count| {
            if context_free {
                assert_eq!(count.get(), 0, "{args}");
            } else {
                assert!(count.get() > 0, "{args}");
            }
        });
        if let Some(expr) = &mut copied {
            erase_sites(expr);
        }
        if let Some(expr) = &mut lazy {
            erase_sites(expr);
        }
        assert_eq!(lazy, copied, "{args}");
    }
}

#[test]
fn scalar_parameter_calls_make_no_context_copies_through_ast_and_ir() {
    // Existing recursive parameter substitution requires more than the default
    // Rust test stack for large function-call chains on the baseline too.
    std::thread::Builder::new().stack_size(8 * 1024 * 1024).spawn(|| {
        for count in [16, 64, 256] {
            let mut code = String::from("module Top; function automatic int f(input int value); return value + 1; endfunction\n");
            for i in 0..count {
                let arg = if i == 0 { "0".into() } else { format!("P{} + 0", i-1) };
                writeln!(code, "localparam int P{i} = f({arg});").unwrap();
            }
            code.push_str("endmodule\n");
            let tree = crate::syntax::parse_source(&code, Path::new("scalar_calls.sv")).unwrap();
            CALL_CONTEXT_COPIES.with(|count| count.set(0));
            let source = Source::from_syntax(&tree).unwrap();
            let ir = crate::analyze::analyze_source(source).unwrap();
            CALL_CONTEXT_COPIES.with(|count| assert_eq!(count.get(), 0));
            assert_eq!(ir.modules()[0].parameters().len(), count);
            for (i, parameter) in ir.modules()[0].parameters().iter().enumerate() {
                assert_eq!(parameter.resolved_value(), Some(i as i128 + 1));
            }
        }
    }).unwrap().join().unwrap();
}

#[test]
#[ignore = "manual paired timing probe; no timing thresholds"]
fn compare_lazy_and_copied_scalar_call_contexts() {
    use std::{hint::black_box, time::Instant};

    let tree = crate::syntax::parse_source(
        "module Top; localparam int RESULT = f(P + 1); endmodule",
        Path::new("scalar_call_probe.sv"),
    )
    .unwrap();
    let primary = (&tree)
        .into_iter()
        .find_map(|node| match node {
            RefNode::ConstantPrimary(primary)
                if matches!(primary, sv_parser::ConstantPrimary::ConstantFunctionCall(_)) =>
            {
                Some(primary)
            }
            _ => None,
        })
        .unwrap();
    let sv_parser::ConstantPrimary::ConstantFunctionCall(call) = primary else {
        unreachable!()
    };
    let aliases = HashMap::default();
    println!("parameters,calls,copied_ms,lazy_ms");
    for count in [32, 128, 512, 2048] {
        let env = environment(count);
        let run = |copied: bool| {
            let start = Instant::now();
            for _ in 0..count {
                let result = if copied {
                    expr_from_function_subroutine_call(
                        &call.nodes.0,
                        &tree,
                        &PackedDimensions::new(HashMap::default(), black_box(&env), &aliases),
                    )
                    .ok()
                    .and_then(expr_to_const)
                } else {
                    const_expr_from_ref_node_with_env(
                        RefNode::ConstantPrimary(primary),
                        &tree,
                        black_box(&env),
                        &aliases,
                    )
                    .unwrap()
                };
                let Some(ConstExpr::Function { name, args, .. }) = black_box(result) else {
                    panic!("call was not retained")
                };
                assert_eq!(name, "f");
                assert_eq!(args.len(), 1);
            }
            start.elapsed().as_secs_f64() * 1000.0
        };
        let mut copied = Vec::new();
        let mut lazy = Vec::new();
        for repetition in 0..7 {
            if repetition % 2 == 0 {
                copied.push(run(true));
                lazy.push(run(false));
            } else {
                lazy.push(run(false));
                copied.push(run(true));
            }
        }
        copied.sort_by(f64::total_cmp);
        lazy.sort_by(f64::total_cmp);
        println!("{count},{count},{:.3},{:.3}", copied[3], lazy[3]);
    }
}
