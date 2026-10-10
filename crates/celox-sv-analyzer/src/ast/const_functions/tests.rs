use super::*;
use std::{hint::black_box, time::Instant};

fn ident(name: &str) -> Expr {
    Expr::Ident(name.into())
}

fn literal(value: &str) -> Expr {
    Expr::Literal(value.into())
}

fn binary(left: Expr, op: BinaryOp, right: Expr) -> Expr {
    Expr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    }
}

fn ty(width: ConstExpr, signed: bool) -> crate::ir::Type {
    let mut ty = Type::new(TypeKind::Logic);
    ty.is_signed = signed;
    ty.packed_ranges.push(PackedRange {
        left: binary_const(width, BinaryOp::Sub, ConstExpr::Literal("1".into())),
        right: ConstExpr::Literal("0".into()),
    });
    crate::ir::Type::from_ast(ty, &HashMap::default())
}

fn binary_const(left: ConstExpr, op: BinaryOp, right: ConstExpr) -> ConstExpr {
    ConstExpr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    }
}

fn fixed_ty(width: usize, signed: bool) -> crate::ir::Type {
    ty(ConstExpr::Literal(width.to_string()), signed)
}

fn function(name: &str, params: Vec<(&str, crate::ir::Type)>, body: Vec<Stmt>) -> ConstantFunction {
    ConstantFunction {
        params: params
            .into_iter()
            .map(|(name, ty)| (name.into(), ty))
            .collect(),
        return_var: Some((name.into(), fixed_ty(16, true))),
        body,
    }
}

fn functions() -> ConstantFunctions {
    let read = function(
        "read",
        vec![],
        vec![Stmt::Return(Some(binary(
            ident("GLOBAL"),
            BinaryOp::Sar,
            literal("1"),
        )))],
    );
    let shadow = function(
        "shadow",
        vec![("GLOBAL", fixed_ty(4, false))],
        read.body.clone(),
    );
    let nested = function(
        "nested",
        vec![("x", fixed_ty(32, true))],
        vec![Stmt::Return(Some(binary(
            Expr::Call {
                name: "read".into(),
                args: vec![],
            },
            BinaryOp::Add,
            ident("x"),
        )))],
    );
    let recursive = function(
        "recursive",
        vec![("n", fixed_ty(32, true))],
        vec![Stmt::If {
            condition: binary(ident("n"), BinaryOp::Eq, literal("0")),
            then_body: vec![Stmt::Return(Some(ident("GLOBAL")))],
            else_body: vec![Stmt::Return(Some(binary(
                Expr::Call {
                    name: "recursive".into(),
                    args: vec![binary(ident("n"), BinaryOp::Sub, literal("1"))],
                },
                BinaryOp::Add,
                literal("1"),
            )))],
        }],
    );
    let range = function(
        "range",
        vec![("x", ty(ConstExpr::Ident("W".into()), false))],
        vec![Stmt::Return(Some(ident("x")))],
    );
    let selected = function(
        "selected",
        vec![("x", fixed_ty(16, false))],
        vec![
            Stmt::Local {
                name: "low".into(),
                init: Some(Expr::Select {
                    expr: Box::new(ident("x")),
                    msb: ConstExpr::Literal("7".into()),
                    lsb: ConstExpr::Literal("0".into()),
                    signed: false,
                }),
            },
            Stmt::Assign {
                lhs: LValue::Select {
                    name: "low".into(),
                    msb: ConstExpr::Literal("3".into()),
                    lsb: ConstExpr::Literal("0".into()),
                    signed: false,
                    array_slice_width: None,
                    array_slice_reversed: false,
                    is_2state: false,
                },
                rhs: literal("4'ha"),
                nonblocking: false,
            },
            Stmt::Return(Some(Expr::Resize {
                expr: Box::new(ident("low")),
                width: 8,
                signed: false,
            })),
        ],
    );
    let self_name = function(
        "self_name",
        vec![],
        vec![Stmt::Return(Some(literal("300")))],
    );
    ConstantFunctions {
        functions: Arc::new(HashMap::from_iter([
            ("read".into(), read),
            ("shadow".into(), shadow),
            ("nested".into(), nested),
            ("recursive".into(), recursive),
            ("range".into(), range),
            ("selected".into(), selected),
            ("self_name".into(), self_name),
        ])),
        locals: Arc::new(HashMap::from_iter([("low".into(), fixed_ty(8, false))])),
        ..ConstantFunctions::default()
    }
}

fn environment(count: usize) -> HashMap<String, i128> {
    let mut env = HashMap::from_iter([
        ("GLOBAL".into(), -1),
        ("W".into(), 8),
        ("self_name".into(), 7),
        ("low".into(), 999),
    ]);
    for (name, width, signed) in [
        ("GLOBAL", 8, true),
        ("self_name", 3, false),
        ("low", 32, true),
    ] {
        insert_parameter_type_markers(&mut env, name, ExprType { width, signed });
    }
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

// The former eager initialization, retained only for result and timing comparisons.
fn copied_call(
    function: &ConstantFunction,
    args: &[i128],
    constants: &HashMap<String, i128>,
) -> Option<i128> {
    let empty = HashMap::default();
    call_in_frame(
        function,
        args,
        Frame {
            constants: &empty,
            values: constants.clone(),
            types: parameter_types_from_const_env(constants)
                .into_iter()
                .map(|(name, ty)| (name, (ty.width, ty.signed)))
                .collect(),
            temporaries: 0,
            iterations: 0,
        },
    )
}

#[test]
fn borrowed_calls_match_copied_frames_and_preserve_caller_values() {
    let functions = functions();
    let _installed = install(functions.clone());
    for size in [16, 256, 4096] {
        let env = environment(size);
        let before = env.clone();
        for (name, args, expected) in [
            ("read", vec![], Some(-1)),
            ("shadow", vec![31], Some(7)),
            ("nested", vec![7], Some(6)),
            ("recursive", vec![6], Some(5)),
            ("range", vec![257], Some(1)),
            ("selected", vec![0x12ff], Some(250)),
            ("self_name", vec![], Some(300)),
            ("read", vec![1], None),
        ] {
            let function = &functions.functions[name];
            let borrowed = call(function, &args, &env);
            assert_eq!(borrowed, expected, "{name}");
            assert_eq!(borrowed, copied_call(function, &args, &env), "{name}");
        }
        assert_eq!(env, before);
    }
}

struct CountedEnvironment<'a> {
    values: &'a HashMap<String, i128>,
    lookups: Cell<usize>,
}

impl ConstantEnvironment for CountedEnvironment<'_> {
    fn get(&self, name: &str) -> Option<&i128> {
        self.lookups.set(self.lookups.get() + 1);
        self.values.get(name)
    }
}

#[test]
fn calls_query_only_referenced_values_and_types() {
    let functions = functions();
    let _installed = install(functions.clone());
    let mut expected = None;
    for size in [16, 256, 4096] {
        let env = environment(size);
        let counted = CountedEnvironment {
            values: &env,
            lookups: Cell::new(0),
        };
        dimensions::PARAMETER_TYPE_SCAN_ENTRIES.with(|count| count.set(0));
        for (name, args) in [
            ("read", vec![]),
            ("nested", vec![7]),
            ("recursive", vec![6]),
            ("range", vec![257]),
        ] {
            assert!(call(&functions.functions[name], &args, &counted).is_some());
        }
        dimensions::PARAMETER_TYPE_SCAN_ENTRIES.with(|count| assert_eq!(count.get(), 0));
        let lookups = counted.lookups.get();
        assert!(lookups > 0);
        assert_eq!(*expected.get_or_insert(lookups), lookups);
    }
}

#[test]
fn unresolved_values_and_recursion_limits_restore_the_call_context() {
    let functions = functions();
    let _installed = install(functions);
    let mut env = environment(0);
    env.remove("GLOBAL");
    assert_eq!(eval_call("read", &[], &env), None);
    env.insert("GLOBAL".into(), -1);
    let arg = |value: &str| crate::ir::ConstExpr::Literal(value.into());
    FUNCTION_CALLS.with(|count| count.set(0));
    assert_eq!(eval_call("recursive", &[arg("65")], &env), None);
    FUNCTION_CALLS.with(|count| assert_eq!(count.get(), MAX_DEPTH));
    DEPTH.with(|depth| assert_eq!(depth.get(), 0));
    assert_eq!(eval_call("recursive", &[arg("6")], &env), Some(5));
    assert_eq!(eval_call("read", &[], &env), Some(-1));
}

#[test]
#[ignore = "manual paired timing probe; no timing thresholds"]
fn compare_borrowed_and_copied_function_environments() {
    let functions = functions();
    let function = &functions.functions["read"];
    println!("parameters,calls,copied_ms,borrowed_ms");
    for count in [32, 128, 512, 2048] {
        let env = environment(count);
        let run = |copied: bool| {
            let start = Instant::now();
            for _ in 0..count {
                let value = if copied {
                    copied_call(function, &[], black_box(&env))
                } else {
                    call(function, &[], black_box(&env))
                };
                assert_eq!(black_box(value), Some(-1));
            }
            start.elapsed().as_secs_f64() * 1000.0
        };
        let mut copied = Vec::new();
        let mut borrowed = Vec::new();
        for repetition in 0..7 {
            if repetition % 2 == 0 {
                copied.push(run(true));
                borrowed.push(run(false));
            } else {
                borrowed.push(run(false));
                copied.push(run(true));
            }
        }
        copied.sort_by(f64::total_cmp);
        borrowed.sort_by(f64::total_cmp);
        println!("{count},{count},{:.3},{:.3}", copied[3], borrowed[3]);
    }
}

#[test]
fn literal_probes_do_not_execute_user_functions_and_recursion_is_not_repeated() {
    let _installed = install(functions());
    let env = environment(0);
    let expr = crate::ir::ConstExpr::Function {
        name: "recursive".into(),
        args: vec![crate::ir::ConstExpr::Literal("12".into())],
        site: None,
    };
    FUNCTION_CALLS.with(|count| count.set(0));
    assert!(crate::typecheck::eval_const_integral_literal_in_env(&expr, &env, &|_| None).is_none());
    FUNCTION_CALLS.with(|count| assert_eq!(count.get(), 0));
    assert_eq!(crate::typecheck::eval_const_expr(&expr, &env), Some(11));
    FUNCTION_CALLS.with(|count| assert_eq!(count.get(), 13));
}

#[test]
fn function_parameter_chains_retain_values_through_ast_and_ir() {
    // The unchanged AST parameter substitution also overflows a default
    // test thread's stack on this 256-element function-call chain. Use the
    // main-thread-sized stack for the full source-to-IR scaling probe.
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(check_function_parameter_chains)
        .unwrap()
        .join()
        .unwrap();
}

fn check_function_parameter_chains() {
    use std::{fmt::Write, path::Path};

    for count in [16, 64, 256] {
        let mut code = String::from(
            "module Top; function automatic int next_value(input int value); return value + 1; endfunction\n",
        );
        for i in 0..count {
            let arg = if i == 0 {
                "0".into()
            } else {
                format!("P{}", i - 1)
            };
            writeln!(code, "localparam int P{i} = next_value({arg});").unwrap();
        }
        code.push_str("endmodule\n");
        let tree = crate::syntax::parse_source(&code, Path::new("function_parameters.sv")).unwrap();
        let source = Source::from_syntax(&tree).unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        let parameters = ir.modules()[0].parameters();
        assert_eq!(parameters.len(), count);
        for (i, parameter) in parameters.iter().enumerate() {
            assert_eq!(parameter.resolved_value(), Some(i as i128 + 1));
        }
    }
}
