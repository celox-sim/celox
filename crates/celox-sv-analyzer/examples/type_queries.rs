//! Type-query and parameter scaling, independent of backend compilation.
//! `cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example type_queries`

use std::{fmt::Write, path::Path, time::Instant};

use celox_sv_analyzer::{analyze, ast, syntax};

fn source(kind: &str, count: usize, ranged: bool, four_state: bool) -> String {
    let parameter_type = if ranged { "logic [31:0]" } else { "int" };
    let mut code = String::from("module Top(input logic [1:0][3:0] a);\n");
    if kind == "function_bits" {
        code.push_str(
            "function automatic logic [7:0] f(input logic [7:0] x); return x; endfunction\n",
        );
    }
    if kind == "alias_bits" {
        code.push_str("typedef logic [7:0] byte_t;\n");
    }
    if kind == "parameters" {
        for i in 0..count {
            let value = if i == 0 {
                if four_state { "'x" } else { "1" }.into()
            } else {
                format!("P{} + 1", i - 1)
            };
            writeln!(code, "localparam {parameter_type} P{i} = {value};").unwrap();
        }
    } else if kind == "generate_dependencies" {
        code.push_str("if (1) begin : g\n");
        for i in 0..count {
            let value = if i + 1 == count {
                if four_state { "'x" } else { "1" }.into()
            } else {
                format!("P{} + 1", i + 1)
            };
            writeln!(code, "localparam {parameter_type} P{i} = {value};").unwrap();
        }
        if four_state {
            code.push_str("logic [$bits(P0)-1:0] s; assign s = P0; end\n");
        } else {
            code.push_str("logic [P0-1:0] s; assign s = '0; end\n");
        }
    } else {
        for i in 0..count {
            writeln!(code, "logic [31:0] q{i};").unwrap();
        }
        for i in 0..count {
            let query = match kind {
                "bits" => "$bits(a)",
                "size" => "$size(a)",
                "selected_bits" => "$bits(a[0])",
                "function_bits" => "$bits(f(a))",
                "alias_bits" => "$bits(byte_t)",
                _ => unreachable!(),
            };
            writeln!(code, "assign q{i} = {query};").unwrap();
        }
    }
    code.push_str("endmodule\n");
    code
}

fn main() {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let four_state = args
        .first()
        .is_some_and(|arg| arg == "--four-state-parameters");
    let ranged = four_state || args.first().is_some_and(|arg| arg == "--ranged-parameters");
    let parameters = ranged || args.first().is_some_and(|arg| arg == "--parameters");
    if parameters {
        args.remove(0);
    }
    let counts: Vec<usize> = args
        .into_iter()
        .map(|arg| {
            let count = arg.parse().expect("counts must be positive integers");
            assert!(count > 0, "counts must be positive integers");
            count
        })
        .collect();
    let counts = if counts.is_empty() {
        vec![16, 64, 256]
    } else {
        counts
    };
    let kinds: &[&str] = if parameters {
        &["parameters", "generate_dependencies"]
    } else {
        &[
            "bits",
            "size",
            "selected_bits",
            "function_bits",
            "alias_bits",
        ]
    };
    println!("kind,count,bytes,parse_ms,ast_ms,ir_ms");
    for &kind in kinds {
        for &count in &counts {
            let code = source(kind, count, ranged, four_state);
            let mut samples = Vec::new();
            for _ in 0..3 {
                let start = Instant::now();
                let tree = syntax::parse_source(&code, Path::new("type_queries.sv")).unwrap();
                let parsed = Instant::now();
                let source = ast::Source::from_syntax(&tree).unwrap();
                let lowered = Instant::now();
                let ir = analyze::analyze_source(source).unwrap();
                let finished = Instant::now();
                let module = &ir.modules()[0];
                if kind == "parameters" {
                    assert_eq!(module.parameters().len(), count);
                    assert_eq!(
                        module.parameters()[count - 1].resolved_value(),
                        (!four_state).then_some(count as i128)
                    );
                    if four_state {
                        assert_eq!(module.parameters()[count - 1].resolved_width(), Some(32));
                    }
                } else if kind == "generate_dependencies" {
                    assert_eq!(
                        module.signals()[0].r#type().resolved_width(),
                        Some(if four_state { 32 } else { count })
                    );
                    if four_state {
                        let celox_sv_analyzer::ir::Expr::Literal(value) =
                            module.assignments()[0].rhs()
                        else {
                            panic!("unknown parameter must remain a literal")
                        };
                        let value =
                            celox_sv_analyzer::typecheck::parse_integral_literal(value).unwrap();
                        assert_eq!(value.width, 32);
                        assert_eq!(value.mask, u32::MAX.into());
                    }
                } else {
                    assert_eq!(module.signals().len(), count);
                    assert_eq!(module.assignments().len(), count);
                    let expected = match kind {
                        "size" => "2",
                        "selected_bits" => "4",
                        _ => "8",
                    };
                    for assignment in module.assignments() {
                        assert_eq!(
                            assignment.rhs(),
                            &celox_sv_analyzer::ir::Expr::Literal(expected.into())
                        );
                    }
                }
                samples.push([
                    (parsed - start).as_secs_f64() * 1000.0,
                    (lowered - parsed).as_secs_f64() * 1000.0,
                    (finished - lowered).as_secs_f64() * 1000.0,
                ]);
            }
            let median: Vec<_> = (0..3)
                .map(|phase| {
                    let mut times: Vec<_> = samples.iter().map(|sample| sample[phase]).collect();
                    times.sort_by(f64::total_cmp);
                    times[1]
                })
                .collect();
            println!(
                "{kind},{count},{},{:.3},{:.3},{:.3}",
                code.len(),
                median[0],
                median[1],
                median[2]
            );
        }
    }
}
