use celox::{Simulator, SimulatorBuilder};

#[test]
fn test_concatenation_self_determination() {
    // IEEE 1800-2023 Clause 11.6.1: Both operands of a concatenation are self-determined.
    // In Veryl, {exp} is a concatenation item.
    let code = r#"
        module Top (
            a: input  logic<8>,
            b: input  logic<8>,
            o: output logic<16>
        ) {
            // Addition inside {} should be self-determined (8-bit).
            // 8'hff + 8'h1 = 8'h00 (overflow truncated)
            // {8'h00} is then zero-extended to 16-bit -> 16'h0000
            assign o = {a + b};
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 0xFFu8);
        io.set(b, 0x01u8);
    })
    .unwrap();

    assert_eq!(
        sim.get(o),
        0u16.into(),
        "Concatenation failed to isolate addition width (self-determination)"
    );
}

#[test]
fn test_comparison_self_determination() {
    // Comparison results are self-determined.
    // logic<16> y = (a <: b) + c;
    // (a <: b) should be 1-bit, regardless of 16-bit context.
    let code = r#"
        module Top (
            a: input  logic<8>,
            b: input  logic<8>,
            c: input  logic<16>,
            o: output logic<16>
        ) {
            assign o = (a <: b) + c;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let c = sim.signal("c");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 10u8);
        io.set(b, 20u8);
        io.set(c, 1u16);
    })
    .unwrap();

    // true(1) + 1 = 2
    assert_eq!(sim.get(o), 2u16.into());
}

#[test]
fn test_shift_rhs_self_determination() {
    // Shift amount (RHS) is self-determined.
    let code = r#"
        module Top (
            a: input  logic<16>,
            b: input  logic<8>,
            c: input  logic<8>,
            o: output logic<16>
        ) {
            // b + c should be evaluated at 8-bit.
            // 8'hff + 8'h1 = 8'h00.
            // 16'h1 << 0 = 16'h1.
            assign o = a << (b + c);
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let c = sim.signal("c");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 1u16);
        io.set(b, 0xFFu8);
        io.set(c, 0x01u8);
    })
    .unwrap();

    assert_eq!(
        sim.get(o),
        1u16.into(),
        "Shift RHS failed to isolate addition width (self-determination)"
    );
}

#[test]
fn test_concatenation_constant_self_determination() {
    // Constant folding should also respect self-determination.
    let code = r#"
        module Top (
            o: output logic<16>,
            o2: output logic<16>,
            o3: output logic<16>,
        ) {
            // 8'hff + 8'h1 = 8'h00 (self-determined)
            assign o = {8'hff + 8'h1};
            assign o2 = 8'hff + 8'h1;
            assign o3 = {8'hf0, 8'hff + 8'h1};
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .trace_analyzer_ir()
        .trace_on_build()
        .build_with_trace()
        .unwrap();
    let o = sim.signal("o");
    let o2 = sim.signal("o2");
    let o3 = sim.signal("o3");
    assert_eq!(
        sim.get(o),
        0u16.into(),
        "Concatenation should be self-determined"
    );
    assert_eq!(
        sim.get(o2),
        256u16.into(),
        "Normal addition should not be self-determined"
    );
    assert_eq!(
        sim.get(o3),
        0xf000u16.into(),
        "Concatenation should be self-determined"
    );
}
#[test]
fn test_concatenation_constant_self_determination_runtime() {
    // Constant folding should also respect self-determination.
    let code = r#"
        module Top (
            i: input logic<8>,
            o: output logic<16>
        ) {
            // 8'hff + 8'h1 = 8'h00 (self-determined)
            assign o = {8'hff + i};
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .trace_analyzer_ir()
        .trace_on_build()
        .build_with_trace()
        .unwrap();
    let i = sim.signal("i");
    let o = sim.signal("o");
    sim.modify(|io| io.set(i, 0x1u8)).unwrap();
    assert_eq!(
        sim.get(o),
        0u16.into(),
        "Constant concatenation failed to isolate addition width"
    );
}
#[test]
fn test_shift_rhs_constant_self_determination() {
    // Constant shift RHS should be self-determined.
    let code = r#"
        module Top (
            o: output logic<16>
        ) {
            // 8'hff + 8'h1 = 8'h00.
            // 16'h1 << 0 = 16'h1.
            assign o = 16'h1 << (8'hff + 8'h1);
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let o = sim.signal("o");

    assert_eq!(
        sim.get(o),
        1u16.into(),
        "Constant shift RHS failed self-determination boundary"
    );
}



