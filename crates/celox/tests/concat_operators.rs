use celox::{Simulator, SimulatorBuilder};

#[test]
fn test_arithmetic_in_concat() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            o: output logic<40>
        ) {
            assign o = {a + b, a - b, a * b, a / b, a % b};
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 10u8);
        io.set(b, 3u8);
    })
    .unwrap();

    // 10 + 3 = 13 (0x0D)
    // 10 - 3 = 7  (0x07)
    // 10 * 3 = 30 (0x1E)
    // 10 / 3 = 3  (0x03)
    // 10 % 3 = 1  (0x01)
    // Expected: {0x0D, 0x07, 0x1E, 0x03, 0x01} = 0x0D_07_1E_03_01
    assert_eq!(sim.get(o), 0x0D071E0301u64.into());
}

#[test]
fn test_shift_in_concat() {
    let code = r#"
        module Top (
            a: input logic<8>,
            o: output logic<32>
        ) {
            assign o = {a << 2, a >> 2, a <<< 2, a >>> 2};
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .trace_analyzer_ir()
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .build_with_trace();
    let trace = result.trace;
    println!("{}", trace.format_analyzer_ir().unwrap());
    println!("{}", trace.format_slt().unwrap());
    println!("{}", trace.format_post_optimized_sir().unwrap());
    let mut sim = result.res.unwrap();
    let a = sim.signal("a");
    let o = sim.signal("o");
    sim.modify(|io| io.set(a, 0x40u8)).unwrap();
    let expected = 0x00100010u32;
    assert_eq!(sim.get(o), expected.into());
}

#[test]
fn test_comparison_in_concat() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            o: output logic<6>
        ) {
            assign o = {a == b, a != b, a <: b, a >: b, a <= b, a >= b};
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 10u8);
        io.set(b, 20u8);
    })
    .unwrap();

    // 10 == 20 -> 0
    // 10 != 20 -> 1
    // 10 < 20  -> 1
    // 10 > 20  -> 0
    // 10 <= 20 -> 1
    // 10 >= 20 -> 0
    // Expected: {0, 1, 1, 0, 1, 0} = 6'b011010 = 0x1A
    assert_eq!(sim.get(o), 0x1Au8.into());
}

#[test]
fn test_bitwise_and_logical_in_concat() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            o: output logic<32>,
            o2: output logic<3>
        ) {
            assign o = {a & b, a | b, a ^ b, (~a)};
            assign o2 = {a && b, a || b, (!a)};
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");
    let o2 = sim.signal("o2");

    sim.modify(|io| {
        io.set(a, 0xAAu8); // 10101010
        io.set(b, 0x55u8); // 01010101
    })
    .unwrap();

    // a & b = 0x00 (8bit)
    // a | b = 0xFF (8bit)
    // a ^ b = 0xFF (8bit)
    // ~a = 0x55 (8bit)
    // a && b = 1 (1bit)
    // a || b = 1 (1bit)
    // !a = 0 (1bit)
    // Expected: {0x00, 0xFF, 0xFF, 0x55, 1, 1, 0}
    // {8'h00, 8'hFF, 8'hFF, 8'h55, 1'b1, 1'b1, 1'b0}
    let expected = (0x00u32 << (8 + 8 + 8)) | (0xFFu32 << (8 + 8)) | (0xFFu32 << (8)) | (0x55u32);
    let expected2 = (1u8 << 2) | (1u8 << 1) | 0u8;
    assert_eq!(sim.get(o), expected.into());
    assert_eq!(sim.get(o2), expected2.into());
}

#[test]
fn test_ternary_in_concat() {
    let code = r#"
        module Top (
            sel: input logic,
            a: input logic<8>,
            b: input logic<8>,
            o: output logic<16>
        ) {
            assign o = {(if sel ? a : b), (if !sel ? a : b)};
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 0xAAu8);
        io.set(b, 0xBBu8);
    })
    .unwrap();

    // sel = 1 -> {a, b} = {0xAA, 0xBB} = 0xAABB
    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(o), 0xAABBu16.into());

    // sel = 0 -> {b, a} = {0xBB, 0xAA} = 0xBBAA
    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(o), 0xBBAAu16.into());
}

#[test]
fn test_as_cast_in_concat() {
    let code = r#"
        module Top (
            a: input logic<16>,
            o: output logic<16>
        ) {
            assign o = {a[15:8] as u8, a[7:0] as u16};
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.modify(|io| io.set(a, 0x1234u16)).unwrap();

    // 0x12 as u8 -> 0x12 (8bit)
    // 0x34 as u16 -> 0x0034 (16bit)
    // Expected: {0x12, 0x0034} = 24bit value 0x120034.
    // o is logic<16>, so it takes [15:0] -> 0x0034.
    assert_eq!(sim.get(o), 0x0034u16.into());
}

#[test]
fn test_nested_concat_and_repeat() {
    let code = r#"
        module Top (
            a: input logic<4>,
            o: output logic<16>
        ) {
            assign o = {{a, a} repeat 2};
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.modify(|io| io.set(a, 0x5u8)).unwrap();

    // {a, a} = {0x5, 0x5} = 0x55 (8bit)
    // 0x55 repeat 2 = {0x55, 0x55} = 0x5555
    assert_eq!(sim.get(o), 0x5555u16.into());
}



