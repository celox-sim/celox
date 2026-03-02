use celox::Simulation;

/// When then and else have widths that map to different Cranelift types
/// (e.g. 1-bit → i8  vs  9-bit → i16), the block argument passed on the
/// "short" branch had the wrong type, causing a Cranelift verifier error:
///   "arg vN has type i8, expected i16"
/// These tests confirm the fix in translate_terminator.

// 1-bit then vs 9-bit else (i8 → i16 boundary)
#[test]
fn test_comb_mux_i8_vs_i16() {
    let code = r#"
module Top (
    en: input logic,
    a9: input logic<9>,
    out: output logic<9>,
) {
    assign out = if en ? en : a9;
}
"#;
    Simulation::builder(code, "Top").build().unwrap();
}

// 1-bit then vs 17-bit else (i8 → i32 boundary)
#[test]
fn test_comb_mux_i8_vs_i32() {
    let code = r#"
module Top (
    en: input logic,
    a17: input logic<17>,
    out: output logic<17>,
) {
    assign out = if en ? en : a17;
}
"#;
    Simulation::builder(code, "Top").build().unwrap();
}

// 8-bit then vs 9-bit else (i8 → i16 boundary from the other side)
#[test]
fn test_comb_mux_i8_wide_vs_i16() {
    let code = r#"
module Top (
    en: input logic,
    a8: input logic<8>,
    a9: input logic<9>,
    out: output logic<9>,
) {
    assign out = if en ? a8 : a9;
}
"#;
    Simulation::builder(code, "Top").build().unwrap();
}

// FF ternary with mismatched-width operands
#[test]
fn test_ff_mux_mismatched_widths() {
    let code = r#"
module Top (
    clk: input clock,
    rst: input reset_async_high,
    sel: input logic,
    a17: input logic<17>,
    out: output logic<17>,
) {
    var b: logic;
    always_ff (clk, rst) {
        if_reset {
            out = 17'd0;
            b   = 1'b0;
        } else {
            b   = sel;
            out = if sel ? b : a17;
        }
    }
}
"#;
    Simulation::builder(code, "Top").build().unwrap();
}

#[test]
fn test_clock_only_ff_conditional() {
    // Repro: clock-only always_ff with if (no reset)
    let code = r#"
module Top (
    clk: input clock,
    en: input logic,
    val: input logic<8>,
    out: output logic<8>,
) {
    always_ff (clk) {
        if en {
            out = val;
        }
    }
}
"#;
    Simulation::builder(code, "Top").build().unwrap();
}

#[test]
fn test_comb_ternary_mismatched_widths() {
    // Repro: ternary where then and else have different widths
    let code = r#"
module Top (
    en: input logic,
    a: input logic<32>,
    b: input logic<1>,
    out: output logic<32>,
) {
    assign out = if en ? a : {31'b0, b};
}
"#;
    Simulation::builder(code, "Top").build().unwrap();
}

#[test]
fn test_ff_ternary_mismatched_widths() {
    // Repro: FF ternary where then and else have different widths
    let code = r#"
module Top (
    clk: input clock,
    rst: input reset_async_high,
    sel: input logic,
    a: input logic<32>,
    b: input logic<1>,
    out: output logic<32>,
) {
    always_ff (clk, rst) {
        if_reset {
            out = 32'd0;
        } else {
            out = if sel ? a : {31'b0, b};
        }
    }
}
"#;
    Simulation::builder(code, "Top").build().unwrap();
}
