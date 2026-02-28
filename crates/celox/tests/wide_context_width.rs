use celox::{BigUint, Simulator};

#[test]
fn test_wide_context_addition_carry() {
    // (64-bit + 64-bit) in 65-bit context should preserve carry
    let code = r#"
        module Top (
            a: input  logic<64>,
            b: input  logic<64>,
            o: output logic<65>
        ) {
            assign o = a + b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    let val_a = u64::MAX;
    let val_b = 1u64;
    // Expected: 2^64
    let expected = BigUint::from(1u32) << 64;

    sim.modify(|io| {
        io.set(a, val_a);
        io.set(b, val_b);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        expected,
        "Carry bit should be preserved in 65-bit context"
    );
}

#[test]
fn test_wide_context_subtraction_underflow() {
    // (64-bit - 64-bit) in 65-bit context
    let code = r#"
        module Top (
            a: input  logic<64>,
            b: input  logic<64>,
            o: output logic<65>
        ) {
            assign o = a - b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    let val_a = 0u64;
    let val_b = 1u64;
    // (0 - 1) & ((1 << 65) - 1) = (1 << 65) - 1
    let expected = (BigUint::from(1u32) << 65) - 1u32;

    sim.modify(|io| {
        io.set(a, val_a);
        io.set(b, val_b);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        expected,
        "Underflow in 65-bit context should result in 65-bit all-ones"
    );
}

#[test]
fn test_wide_context_nested_propagation() {
    // (120-bit + 120-bit) * 2'd2 in 122-bit context
    let code = r#"
        module Top (
            a: input  logic<120>,
            b: input  logic<120>,
            o: output logic<122>
        ) {
            assign o = (a + b) * 2'd2;
        }
    "#;
    let code_top = "Top";
    let mut sim = celox::SimulatorBuilder::new(code, code_top)
        .build()
        .expect("Build should succeed");

    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    // a = 2^119, b = 2^119
    // a + b = 2^120
    // (a + b) * 2 = 2^121
    let val_a = BigUint::from(1u32) << 119;
    let val_b = BigUint::from(1u32) << 119;
    let expected = BigUint::from(1u32) << 121;

    sim.modify(|io| {
        io.set_wide(a, val_a);
        io.set_wide(b, val_b);
    })
    .unwrap();
    assert_eq!(sim.get(o), expected, "Nested 122-bit context width failed");
}

#[test]
fn test_wide_context_shift_left() {
    let code = r#"
        module Top (
            i: input  logic<64>,
            s: input  logic<8>,
            o: output logic<130>
        ) {
            // i as 130 ensures the shift happens in 130-bit context
            assign o = (i as 130) << s;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let i = sim.signal("i");
    let s = sim.signal("s");
    let o = sim.signal("o");

    let val_i = 1u64;
    let val_s = 65u8;
    let expected = BigUint::from(1u32) << 65;

    sim.modify(|io| {
        io.set(i, val_i);
        io.set(s, val_s);
    })
    .unwrap();
    assert_eq!(sim.get(o), expected, "Shift left with wide cast failed");
}

#[test]
fn test_wide_context_constant_folding() {
    let code = r#"
        module Top (
            o: output logic<65>
        ) {
            always_comb {
                o = 64'hffff_ffff_ffff_ffff + 64'h1;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let o = sim.signal("o");

    let expected = BigUint::from(1u32) << 64;
    assert_eq!(
        sim.get(o),
        expected,
        "Constant folding in 65-bit context failed"
    );
}

#[test]
fn test_wide_runtime_shift_width_behavior() {
    let code = r#"
        module Top (
            i: input  logic<64>,
            s: input  logic<8>,
            o1: output logic<130>,
            o2: output logic<130>
        ) {
            always_comb {
                // The shift result width is determined by lhs.
                o1 = i << s;

                // To keep bits, 'i' must be cast to the target width before shifting.
                o2 = (i as 130) << s;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();

    let i = sim.signal("i");
    let s = sim.signal("s");
    let o1 = sim.signal("o1");
    let o2 = sim.signal("o2");

    // Case 1: i=2^63 + 2^62, s=1
    // o1: (i << 1) in 64-bit context = 2^63 (the 2^64 bit is lost) -> zero-extended to 130-bit
    // o2: (i as 130 << 1) in 130-bit context = 2^64 + 2^63
    let val_i: BigUint = (BigUint::from(1u32) << 63) | (BigUint::from(1u32) << 62);
    let val_s = 1u8;
    let expected_o1 = val_i.clone() << val_s;
    let expected_o2 = val_i.clone() << val_s;

    sim.modify(|io| {
        io.set_wide(i, val_i);
        io.set(s, val_s);
    })
    .unwrap();

    assert_eq!(sim.get(o1), expected_o1, "Upper bit should be preserved");
    assert_eq!(
        sim.get(o2),
        expected_o2,
        "Upper bit should be preserved after 130-bit cast"
    );

    let val_i: BigUint = BigUint::from(1u32) << 63;
    let val_s = 2u8;
    let expected_o1 = val_i.clone() << val_s;
    let expected_o2 = val_i.clone() << val_s;

    sim.modify(|io| {
        io.set_wide(i, val_i);
        io.set(s, val_s);
    })
    .unwrap();

    assert_eq!(sim.get(o1), expected_o1);
    assert_eq!(sim.get(o2), expected_o2);
}

#[test]
fn test_wide_context_constant_folding_128bit() {
    let code = r#"
        module Top (
            o: output logic<128>
        ) {
            always_comb {
                o = 32'hffff_ffff + 1;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let o = sim.signal("o");

    let expected = BigUint::from(1u32) << 32;
    assert_eq!(
        sim.get(o),
        expected,
        "Constant folding in 128-bit context failed: 32'hffff_ffff + 1 should be 32'h1_0000_0000"
    );
}

#[test]
fn test_wide_context_multiplication_boundary() {
    // 64-bit * 64-bit in 128-bit context
    let code = r#"
        module Top (
            a: input  logic<64>,
            b: input  logic<64>,
            o: output logic<128>
        ) {
            assign o = (a as 128) * (b as 128);
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    let val_a = u64::MAX;
    let val_b = u64::MAX;
    // (2^64 - 1)^2 = 2^128 - 2*2^64 + 1
    let expected = (BigUint::from(1u32) << 128) - (BigUint::from(2u32) << 64) + 1u32;

    sim.modify(|io| {
        io.set(a, val_a);
        io.set(b, val_b);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        expected,
        "64-bit * 64-bit multiplication should not truncate in 128-bit context"
    );
}

#[test]
fn test_wide_context_addition_mixed_boundary() {
    // 64-bit + 1-bit in 65-bit context
    let code = r#"
        module Top (
            a: input  logic<64>,
            b: input  logic<1>,
            o: output logic<65>
        ) {
            assign o = a + b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    let val_a = u64::MAX;
    let val_b = 1u8;
    // Expected: 2^64
    let expected = BigUint::from(1u32) << 64;

    sim.modify(|io| {
        io.set(a, val_a);
        io.set(b, val_b);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        expected,
        "Mixed 64-bit + 1-bit addition should not truncate in 65-bit context"
    );
}



