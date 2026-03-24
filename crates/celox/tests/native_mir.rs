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
