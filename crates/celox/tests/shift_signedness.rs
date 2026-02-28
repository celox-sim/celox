use num_traits::Num;
use std::str::FromStr;
use celox::{BigUint, Simulator, SimulatorBuilder};

#[test]
fn test_shift_right_arithmetic_native() {
    // IEEE 1800-2023 Clause 11.4.10:
    // "Arithmetic shift right (>>>) ... shall fill the vacated bits at the most significant end
    // with the value of the sign bit if the expression being shifted is signed."
    let code = r#"
        module Top (
            clk: input  clock,
            i:   input  i64,
            o_comb: output i64,
            o_ff:   output i64
        ) {
            assign o_comb = i >>> 1;
            always_ff (clk) {
                o_ff = i >>> 1;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let i = sim.signal("i");
    let o_comb = sim.signal("o_comb");
    let o_ff = sim.signal("o_ff");

    // Negative value: 64'h8000_0000_0000_0000 (-2^63)
    // Shifted right: 64'hc000_0000_0000_0000
    sim.modify(|io| io.set(i, 0x8000_0000_0000_0000u64))
        .unwrap();

    assert_eq!(
        sim.get(o_comb),
        BigUint::from(0xc000_0000_0000_0000u64),
        "Comb arithmetic shift failed"
    );

    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(o_ff),
        BigUint::from(0xc000_0000_0000_0000u64),
        "FF arithmetic shift failed"
    );
}

#[test]
fn test_shift_right_logical_signed_native() {
    // IEEE 1800-2023 Clause 11.4.10:
    // "The logical shift operators shall fill the vacated bits with zeros regardless of whether the expression is signed or unsigned."
    let code = r#"
        module Top (
            clk: input  clock,
            i:   input  i64,
            o_comb: output i64,
            o_ff:   output i64
        ) {
            assign o_comb = i >> 1;
            always_ff (clk) {
                o_ff = i >> 1;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let i = sim.signal("i");
    let o_comb = sim.signal("o_comb");
    let o_ff = sim.signal("o_ff");

    // Negative value: 64'h8000_0000_0000_0000
    // Logical shift: 64'h4000_0000_0000_0000 (zero-filled)
    sim.modify(|io| io.set(i, 0x8000_0000_0000_0000u64))
        .unwrap();

    assert_eq!(
        sim.get(o_comb),
        BigUint::from(0x4000_0000_0000_0000u64),
        "Comb logical shift (signed) failed"
    );

    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(o_ff),
        BigUint::from(0x4000_0000_0000_0000u64),
        "FF logical shift (signed) failed"
    );
}

#[test]
fn test_shift_right_arithmetic_wide() {
    // Verify 128-bit arithmetic shift
    let code = r#"
        module Top (
            clk: input  clock,
            i:   input  signed logic<128>,
            o_comb: output signed logic<128>,
            o_ff:   output signed logic<128>
        ) {
            assign o_comb = i >>> 1;
            always_ff (clk) {
                o_ff = i >>> 1;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let i = sim.signal("i");
    let o_comb = sim.signal("o_comb");
    let o_ff = sim.signal("o_ff");

    // MSB set: 128'h8000...0000
    let val = BigUint::from_str("170141183460469231731687303715884105728").unwrap();
    // Expected: 128'hc000...0000
    let expected = BigUint::from_str("255211775190703847597530955573826158592").unwrap();

    sim.modify(|io| io.set_wide(i, val.clone())).unwrap();

    let result_comb = sim.get(o_comb);
    assert_eq!(result_comb, expected, "Comb wide arithmetic shift failed");

    sim.tick(clk).unwrap();
    let result_ff = sim.get(o_ff);
    assert_eq!(result_ff, expected, "FF wide arithmetic shift failed");
}

#[test]
fn test_shift_right_logical_wide() {
    // Verify 128-bit logical shift (should zero-fill even if signed)
    let code = r#"
        module Top (
            i:   input  signed logic<128>,
            o:   output signed logic<128>
        ) {
            assign o = i >> 1;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let i = sim.signal("i");
    let o = sim.signal("o");

    // MSB set: 128'h8000...0000
    let val = BigUint::from_str("170141183460469231731687303715884105728").unwrap();
    // Expected: 128'h4000...0000
    let expected = BigUint::from_str("85070591730234615865843651857942052864").unwrap();

    sim.modify(|io| io.set_wide(i, val.clone())).unwrap();

    let result = sim.get(o);
    assert_eq!(result, expected, "Wide logical shift failed");
}

#[test]
fn test_shift_constant_folding_wide() {
    let code = r#"
        module Top (
            o: output signed logic<128>,
            o2: output signed logic<128>,
            o3: output logic<128>
        ) {
            assign o = 128'shc000_0000_0000_0000_0000_0000_0000_0000 >>> 1;
            assign o2 = 128'hc000_0000_0000_0000_0000_0000_0000_0000 >>> 1;
            assign o3 = 128'shc000_0000_0000_0000_0000_0000_0000_0000 >>> 1;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let o = sim.signal("o");
    let o2 = sim.signal("o2");
    let o3 = sim.signal("o3");
    let expected = BigUint::from_str_radix("e000_0000_0000_0000_0000_0000_0000_0000", 16).unwrap(); // 128'she000...
    assert_eq!(
        sim.get(o),
        expected,
        "Wide arithmetic constant folding failed"
    );
    let expected2 = BigUint::from_str_radix("6000_0000_0000_0000_0000_0000_0000_0000", 16).unwrap(); // 128'sha000...
    assert_eq!(
        sim.get(o2),
        expected2,
        "Wide arithmetic constant folding failed"
    );
    let expected3 = BigUint::from_str_radix("e000_0000_0000_0000_0000_0000_0000_0000", 16).unwrap(); // 128'she000...
    assert_eq!(
        sim.get(o3),
        expected3,
        "Wide arithmetic constant folding failed"
    );
}

#[test]
fn test_shift_constant_folding_native() {
    let code = r#"
        module Top (
            o: output signed logic<64>,
            o2: output signed logic<64>,
            o3: output logic<64>
        ) {
            assign o = 64'shc000_0000_0000_0000 >>> 1;
            assign o2 = 64'hc000_0000_0000_0000 >>> 1;
            assign o3 = 64'shc000_0000_0000_0000 >>> 1;
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
    let expected = BigUint::from(0xe000_0000_0000_0000u64);
    assert_eq!(
        sim.get(o),
        expected,
        "Native arithmetic constant folding failed"
    );

    let expected2 = BigUint::from(0x6000_0000_0000_0000u64);
    assert_eq!(
        sim.get(o2),
        expected2,
        "Native arithmetic constant folding failed"
    );
    let expected3 = BigUint::from(0xe000_0000_0000_0000u64);
    assert_eq!(
        sim.get(o3),
        expected3,
        "Native arithmetic constant folding failed"
    );
}



