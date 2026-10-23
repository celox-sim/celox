use celox::SimulatorBuilder;
use insta::assert_snapshot;

fn mir_trace(code: &str, top: &str) -> String {
    let result = SimulatorBuilder::new(code, top)
        .optimize(true)
        .trace_mir()
        .build_with_trace();
    result.trace.mir.unwrap_or_default()
}

/// Test with high register pressure (many simultaneous live values).
/// 16 inputs all used in a single expression → >13 VRegs live simultaneously.
#[test]
fn high_pressure_comb_mir() {
    let code = r#"
        module Top (
            a0: input logic<32>, a1: input logic<32>,
            a2: input logic<32>, a3: input logic<32>,
            a4: input logic<32>, a5: input logic<32>,
            a6: input logic<32>, a7: input logic<32>,
            a8: input logic<32>, a9: input logic<32>,
            a10: input logic<32>, a11: input logic<32>,
            a12: input logic<32>, a13: input logic<32>,
            a14: input logic<32>, a15: input logic<32>,
            o: output logic<32>,
        ) {
            assign o = a0 + a1 + a2 + a3 + a4 + a5 + a6 + a7
                     + a8 + a9 + a10 + a11 + a12 + a13 + a14 + a15;
        }
    "#;
    let output = mir_trace(code, "Top");
    assert_snapshot!(output);
}

/// Generate a large comb circuit where each output depends on many inputs,
/// forcing many values to be live simultaneously.
/// 20 inputs, 20 outputs, each output = (all inputs XORed) + input_i
/// This creates cross-dependencies that prevent the scheduler from
/// reducing pressure below ~20.
#[test]
fn large_comb_pressure_mir() {
    let n = 20;
    let mut ports = String::new();
    for i in 0..n {
        ports.push_str(&format!("    a{i}: input logic<32>,\n"));
    }
    for i in 0..n {
        ports.push_str(&format!("    o{i}: output logic<32>,\n"));
    }
    // Remove trailing comma+newline, replace with newline
    ports = ports.trim_end_matches(",\n").to_string() + "\n";

    let mut body = String::new();
    // xor_all = a0 ^ a1 ^ ... ^ a19
    body.push_str("    var xor_all: logic<32>;\n");
    body.push_str("    assign xor_all = a0");
    for i in 1..n {
        body.push_str(&format!(" ^ a{i}"));
    }
    body.push_str(";\n");
    // Each output depends on xor_all + its own input
    for i in 0..n {
        body.push_str(&format!("    assign o{i} = xor_all + a{i};\n"));
    }

    let code = format!("module Top (\n{ports}) {{\n{body}}}");
    let output = mir_trace(&code, "Top");
    assert_snapshot!(output);
}

#[test]
fn rle_comb_mir() {
    let code = r#"
        module Top (
            x: input logic<32>,
            y: input logic<32>,
            temp: output logic<32>,
            z: output logic<32>,
        ) {
            assign temp = x + y;
            assign z = x + y;
        }
    "#;
    let output = mir_trace(code, "Top");
    assert_snapshot!(output);
}

#[test]
fn shared_expression_mir() {
    let code = r#"
        module Top (
            a: input logic<32>,
            b: input logic<32>,
            x: output logic<32>,
            y: output logic<32>,
        ) {
            assign x = (a + b) & 32'd1;
            assign y = (a + b) | 32'd2;
        }
    "#;
    let output = mir_trace(code, "Top");
    assert_snapshot!(output);
}

#[test]
fn ff_branch_mir() {
    let code = r#"
        module Top (
            clk: input '_ clock,
            rst: input '_ reset,
            d: input logic<8>,
            q: output logic<8>,
        ) {
            always_ff(clk) {
                if_reset {
                    q = 0;
                } else {
                    q = d;
                }
            }
        }
    "#;
    let output = mir_trace(code, "Top");
    assert_snapshot!(output);
}

#[test]
fn trace_contains_the_exact_fused_native_pipeline() {
    let code = r#"
        module Top (
            clk: input '_ clock,
            d: input logic<8>,
            q: output logic<8>,
            y: output logic<8>,
        ) {
            always_ff(clk) {
                q = d;
            }
            assign y = q + 1;
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .optimize(true)
        .trace_mir()
        .build_with_trace();
    result.res.expect("traced native build should succeed");
    let native_sir = result
        .trace
        .native_optimized_sir
        .expect("native optimized SIR should be captured");
    let mir = result.trace.mir.expect("native MIR should be captured");

    assert!(native_sir.contains("=== Native function eval_comb ==="));
    assert!(native_sir.contains("=== Native function eval_comb_apply_ff[0] ==="));
    assert!(mir.contains("=== Native function eval_comb_apply_ff[0] ==="));
    assert!(
        !mir.contains("Execution Unit eval_apply_ffs"),
        "trace must not independently re-lower source execution units"
    );
}

/// FP ports keep the test free of integer-conversion helpers: the complete
/// frontend -> optimized SIR -> MIR path must preserve numeric FP operations.
#[test]
fn scalar_float_arithmetic_stays_explicit_in_mir() {
    for (ty, precision) in [("f32", "F32"), ("f64", "F64")] {
        for optimize in [false, true] {
            let source = format!(
                r#"module Top(a: input {ty}, b: input {ty},
                    y_add: output {ty}, y_sub: output {ty},
                    y_mul: output {ty}, y_div: output {ty},
                    y_neg: output {ty}, y_lt: output bit, y_ne: output bit) {{
                    assign y_add = a + b;
                    assign y_sub = a - b;
                    assign y_mul = a * b;
                    assign y_div = a / b;
                    assign y_neg = -a;
                    assign y_lt = a <: b;
                    assign y_ne = a != b;
                }}"#
            );
            let result = SimulatorBuilder::new(&source, "Top")
                .optimize(optimize)
                .trace_mir()
                .build_with_trace();
            if let Err(error) = &result.res {
                panic!("FP source compilation failed: {error:?}");
            }
            let mir = result.trace.mir.as_deref().expect("native MIR trace");
            for op in ["Add", "Sub", "Mul", "Div", "Neg", "Lt", "Ne"] {
                assert!(
                    mir.contains(&format!("fp.{op}.{precision}"))
                        || (mir.contains(&format!("op: {op},"))
                            && mir.contains(&format!("precision: {precision},"))),
                    "missing {op} ({ty}, optimize={optimize}):\n{mir}"
                );
            }
            assert!(
                !mir.contains("extern") && !mir.contains("CallExtern"),
                "unexpected FP helper:\n{mir}"
            );
            let mut sim = result.unwrap();
            let a = sim.signal("a");
            let b = sim.signal("b");
            let bits = |x: f64| {
                if ty == "f32" {
                    (x as f32).to_bits() as u64
                } else {
                    x.to_bits()
                }
            };
            sim.modify(|io| {
                io.set(a, bits(3.5));
                io.set(b, bits(-2.0));
            })
            .unwrap();
            for (name, expected) in [
                ("y_add", 1.5),
                ("y_sub", 5.5),
                ("y_mul", -7.0),
                ("y_div", -1.75),
                ("y_neg", -3.5),
            ] {
                assert_eq!(
                    sim.get(sim.signal(name)),
                    bits(expected).into(),
                    "{name} ({ty})"
                );
            }
            assert_eq!(sim.get(sim.signal("y_lt")), 0u32.into());
            assert_eq!(sim.get(sim.signal("y_ne")), 1u32.into());
        }
    }
}
