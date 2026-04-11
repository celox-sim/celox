use celox::{Simulation, Simulator, SimulatorBuilder};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

fn test_comb_mux_i8_vs_i16_correctness(sim) {
    @setup { let code = r#"
module Top (
    en: input logic,
    a9: input logic<9>,
    out: output logic<9>,
) {
    assign out = if en ? en : a9;
}
"#; }
    @build Simulator::builder(code, "Top");
    let en = sim.signal("en");
    let a9 = sim.signal("a9");
    let out = sim.signal("out");

    // en=0: output is a9
    sim.modify(|io| {
        io.set(en, 0u8);
        io.set(a9, 0x155u16);
    })
    .unwrap();
    assert_eq!(sim.get(out), 0x155u16.into(), "en=0: out should equal a9");

    // en=1: output is en (1-bit=1) zero-extended to 9 bits → 1
    sim.modify(|io| {
        io.set(en, 1u8);
        io.set(a9, 0x155u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(out),
        1u16.into(),
        "en=1: out should be 1 (en zero-extended)"
    );
}

// 4-state mode: exercises the mask-cast path in translate_terminator.
// A ternary with i8->i16 boundary must compile and produce correct results
// with X/Z propagation enabled.
fn test_comb_mux_i8_vs_i16_four_state(sim) {
    @setup { let code = r#"
module Top (
    en: input logic,
    a9: input logic<9>,
    out: output logic<9>,
) {
    assign out = if en ? en : a9;
}
"#; }
    @build SimulatorBuilder::new(code, "Top")
        .four_state(true);
    let en = sim.signal("en");
    let a9 = sim.signal("a9");
    let out = sim.signal("out");

    // en=0: output is a9
    sim.modify(|io| {
        io.set(en, 0u8);
        io.set(a9, 0x155u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(out),
        0x155u16.into(),
        "4-state en=0: out should equal a9"
    );

    // en=1: output is 1 (en zero-extended)
    sim.modify(|io| io.set(en, 1u8)).unwrap();
    assert_eq!(sim.get(out), 1u16.into(), "4-state en=1: out should be 1");
}

}

// Tests that use Simulation::builder or just test build success stay as regular #[test]

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
fn test_multi_bit_mux_condition_four_state_cranelift() {
    let code = r#"
module Top (
    sel: input logic<2>,
    d0: input logic<8>,
    d1: input logic<8>,
    y: output logic<8>,
) {
    assign y = if sel ? d1 : d0;
}
"#;

    let mut sim = Simulator::builder(code, "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();

    let sel = sim.signal("sel");
    let d0 = sim.signal("d0");
    let d1 = sim.signal("d1");
    let y = sim.signal("y");

    sim.set_four_state(sel, 0u8.into(), 0b11u8.into());
    sim.set_four_state(d0, 0xAAu8.into(), 0u8.into());
    sim.set_four_state(d1, 0x55u8.into(), 0u8.into());
    sim.eval_comb().unwrap();

    let (_value, mask) = sim.get_four_state(y);
    assert_eq!(
        mask,
        0xFFu8.into(),
        "X in multi-bit mux condition must produce all-X output"
    );
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
