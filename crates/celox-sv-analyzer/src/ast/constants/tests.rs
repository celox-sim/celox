use super::*;
use std::{fmt::Write, path::Path};

fn environment(count: usize) -> HashMap<String, i128> {
    let state = &crate::ast::AnalysisState::default();
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
    let mut env = const_env_from_parameters(state, &[parameter]);
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

#[test]
fn native_scalar_parameter_calls_preserve_chained_values_through_ast_and_ir() {
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
            let tree = crate::migration::parse_source(&code, Path::new("scalar_calls.sv")).unwrap();
            let tree = AnalysisContext::unspecialized(&tree);
            let source = Source::from_syntax(&tree).unwrap();
            let ir = crate::analyze::analyze_source(source).unwrap();
            assert_eq!(ir.modules()[0].parameters().len(), count);
            for (i, parameter) in ir.modules()[0].parameters().iter().enumerate() {
                assert_eq!(parameter.resolved_value(), Some(i as i128 + 1));
            }
        }
    }).unwrap().join().unwrap();
}

#[test]
fn nested_array_indices_keep_continuous_drivers_disjoint() {
    // A parser-level regression: the index must select an 8-bit element,
    // otherwise the analyzer incorrectly diagnoses overlapping net drivers.
    let code = r#"
    module Top(input logic [7:0] data, input logic [1:0] idx,
               output logic [7:0] y, output wire [3:0] bits);
        localparam logic [7:0] map [2] = '{1, 3};
        wire [7:0] mem [4];
        assign mem[map[1]] = data;
        assign mem[0] = 8'h10;
        assign mem[1] = 8'h21;
        assign mem[2] = 8'h32;
        assign y = mem[idx];
        assign bits[map[1]] = data[0];
        assign bits[2:0] = 3'b101;
    endmodule
  "#;
    let ir = crate::analyze_source(code, Path::new("nested_net_index.sv")).unwrap();
    let module = &ir.modules()[0];
    let assignment = module.assignments().iter().find(|assignment| {
        matches!(assignment.lhs_value(), crate::ir::LValue::Select { name, .. } if name == "mem")
    }).unwrap();
    let crate::ir::LValue::Select { msb, lsb, .. } = assignment.lhs_value() else {
        panic!("expected a selected net target");
    };
    let constants = module
        .parameters()
        .iter()
        .filter_map(|parameter| Some((parameter.name().to_owned(), parameter.resolved_value()?)))
        .collect();
    // The array localparam is retained as a constant driver in the IR, so
    // these bounds can remain symbolic. Its selected element is still 8 bits.
    assert_eq!(typecheck::select_range_width(msb, lsb, &constants), Some(8));
}

#[test]
fn concat_slice_indices_accept_dynamic_array_elements() {
    // The parser accepts standard concatenation selections independently
    // of Icarus, which does not parse that syntax.
    let code = r#"
        module Top(input logic idx, input logic signed [7:0] data,
                   output logic [7:0] joined);
            logic signed [7:0] map [2];
            logic [7:0] mem [-2:1];
            always_comb begin
                map[0] = data;
                map[1] = ~data;
                mem[-2] = 8'h10;
                mem[-1] = 8'h20;
                mem[0] = 8'h30;
                mem[1] = 8'h40;
            end
            assign joined = mem[$signed({2'b00, map[idx]}[1:0])];
        endmodule
    "#;
    let ir = crate::analyze_source(code, Path::new("nested_concat_index.sv")).unwrap();
    assert!(
        ir.modules()[0]
            .assignments()
            .iter()
            .any(|assignment| assignment.lhs() == "joined")
    );
}

#[test]
fn native_constant_calls_preserve_contextual_operand_shapes() {
    let tree = crate::migration::parse_source(
        "module M;parameter N=f(P[0+:W],byte_t'(P),$bits(P));endmodule",
        Path::new("native-call-context.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let env = environment(4096);
    let mut alias = Type::new(TypeKind::Logic);
    alias.is_signed = true;
    alias.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal("7".into()),
        ConstExpr::Literal("0".into()),
    ));
    let aliases = HashMap::from_iter([("byte_t".into(), alias)]);
    let mut context = PackedDimensions::new(HashMap::default(), &env, &aliases);
    context.constant_indexed_base = true;
    context.scope_types_complete = true;
    context.insert(
        "P".into(),
        procedural::dimensions_from_type(&aliases["byte_t"]),
    );
    let root = tree
        .declarations()
        .expression_nodes()
        .find_map(|(id, expr)| match expr.kind {
            celox_sv_syntax::ExprKind::Call { expression, .. }
                if native_expressions::native_name(
                    &tree.state,
                    expression,
                    tree.declarations(),
                )
                .as_deref()
                    == Some("f") =>
            {
                Some(id)
            }
            _ => None,
        })
        .unwrap();
    let value = native_constants::bind(root, &tree, &context)
        .unwrap()
        .unwrap();
    let ConstExpr::Function { callee, args, .. } = value else {
        panic!()
    };
    assert_eq!(callee, Callee::Subroutine("f".into()));
    assert_eq!(args.len(), 3);
    assert_eq!(
        infer_const_expr_type(&args[0], &parameter_types_from_const_env(&env))
            .unwrap()
            .width,
        3
    );
    assert_eq!(
        infer_const_expr_type(&args[1], &parameter_types_from_const_env(&env))
            .unwrap()
            .width,
        8
    );
    assert_eq!(eval_ast_const_expr(&tree.state, &args[2], &env), Some(8));
}
