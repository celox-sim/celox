use celox::{BigUint, Simulator};

#[test]
fn test_wide_addition_128bit() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            o: output logic<128>
        ) {
            assign o = a + b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    // Simple addition
    let val_a = BigUint::from(1u64) << 64 | BigUint::from(0x1234_5678u64);
    let val_b = BigUint::from(0u64) << 64 | BigUint::from(0x0000_0001u64);
    let expected = &val_a + &val_b;

    sim.modify(|io| {
        io.set_wide(a, val_a);
        io.set_wide(b, val_b);
    })
    .unwrap();
    assert_eq!(sim.get(o), expected);
}

#[test]
fn test_wide_addition_carry_propagation() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            o: output logic<128>
        ) {
            assign o = a + b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    // Carry from lower 64 bits into upper 64 bits
    let val_a = BigUint::from(u64::MAX);
    let val_b = BigUint::from(1u64);
    let expected = BigUint::from(1u128 << 64);

    sim.modify(|io| {
        io.set_wide(a, val_a);
        io.set_wide(b, val_b);
    })
    .unwrap();
    assert_eq!(sim.get(o), expected);
}

#[test]
fn test_wide_subtraction_128bit() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            o: output logic<128>
        ) {
            assign o = a - b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    let val_a = BigUint::from(1u128 << 64) + BigUint::from(100u64);
    let val_b = BigUint::from(50u64);
    let expected = BigUint::from(1u128 << 64) + BigUint::from(50u64);

    sim.modify(|io| {
        io.set_wide(a, val_a);
        io.set_wide(b, val_b);
    })
    .unwrap();
    assert_eq!(sim.get(o), expected);
}

#[test]
fn test_wide_subtraction_borrow() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            o: output logic<128>
        ) {
            assign o = a - b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    // Borrow from upper chunk: (1 << 64) - 1 = 0xFFFF_FFFF_FFFF_FFFF
    let val_a = BigUint::from(1u128 << 64);
    let val_b = BigUint::from(1u64);
    let expected = BigUint::from(u64::MAX as u128);

    sim.modify(|io| {
        io.set_wide(a, val_a);
        io.set_wide(b, val_b);
    })
    .unwrap();
    assert_eq!(sim.get(o), expected);
}

#[test]
fn test_wide_bitwise_operations() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            o_and: output logic<128>,
            o_or:  output logic<128>,
            o_xor: output logic<128>
        ) {
            assign o_and = a & b;
            assign o_or  = a | b;
            assign o_xor = a ^ b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o_and = sim.signal("o_and");
    let o_or = sim.signal("o_or");
    let o_xor = sim.signal("o_xor");

    let val_a: BigUint = BigUint::from(0xFF00_FF00u64) | (BigUint::from(0xAAAA_AAAAu64) << 64);
    let val_b: BigUint = BigUint::from(0x0FF0_0FF0u64) | (BigUint::from(0x5555_5555u64) << 64);

    sim.modify(|io| {
        io.set_wide(a, val_a.clone());
        io.set_wide(b, val_b.clone());
    })
    .unwrap();

    assert_eq!(sim.get(o_and), &val_a & &val_b);
    assert_eq!(sim.get(o_or), &val_a | &val_b);
    assert_eq!(sim.get(o_xor), &val_a ^ &val_b);
}

#[test]
fn test_wide_comparison_eq() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            o_eq: output logic,
            o_ne: output logic
        ) {
            assign o_eq = a == b;
            assign o_ne = a != b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o_eq = sim.signal("o_eq");
    let o_ne = sim.signal("o_ne");

    let val: BigUint = BigUint::from(0xDEAD_BEEFu64) | (BigUint::from(0xCAFE_BABEu64) << 64);

    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set_wide(b, val.clone());
    })
    .unwrap();
    assert_eq!(sim.get(o_eq), 1u8.into());
    assert_eq!(sim.get(o_ne), 0u8.into());

    // Differ only in upper chunk
    let val2 = BigUint::from(0xDEAD_BEEFu64) | (BigUint::from(0xCAFE_BABFu64) << 64);
    sim.modify(|io| {
        io.set_wide(b, val2);
    })
    .unwrap();
    assert_eq!(sim.get(o_eq), 0u8.into());
    assert_eq!(sim.get(o_ne), 1u8.into());
}

#[test]
fn test_wide_shift_left() {
    let code = r#"
        module Top (
            a:   input  logic<128>,
            amt: input  logic<8>,
            o:   output logic<128>
        ) {
            assign o = a << amt;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let amt = sim.signal("amt");
    let o = sim.signal("o");

    let val = BigUint::from(1u64);

    // Shift by 0 -> stays in place
    sim.modify(|io| {
        io.set_wide(a, val.clone());
        io.set(amt, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(o), BigUint::from(1u64));

    // Shift by 64 -> moves entirely to upper chunk
    sim.modify(|io| io.set(amt, 64u8)).unwrap();
    assert_eq!(sim.get(o), BigUint::from(1u128 << 64));

    // Shift by 32 -> crosses chunk boundary
    sim.modify(|io| {
        io.set_wide(a, BigUint::from(0xFFFF_FFFFu64));
        io.set(amt, 32u8);
    })
    .unwrap();
    assert_eq!(sim.get(o), BigUint::from(0xFFFF_FFFFu128 << 32));
}

#[test]
fn test_wide_ff_accumulator() {
    let code = r#"
        module Top (
            clk: input clock,
            inc: input logic<128>,
            o:   output logic<128>
        ) {
            var acc: logic<128>;
            always_ff {
                acc = acc + inc;
            }
            assign o = acc;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let inc = sim.signal("inc");
    let o = sim.signal("o");

    let step = BigUint::from(u64::MAX);

    sim.modify(|io| io.set_wide(inc, step.clone())).unwrap();

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), step.clone());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), &step * 2u32);

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), &step * 3u32);
}

// ============================================================
// Wide (128-bit) logical operators (&& / ||)
// ============================================================

/// 128-bit logical and/or in always_comb.
///
/// Veryl syntax:
/// - `a && b` => BinaryOp::LogicAnd
/// - `a || b` => BinaryOp::LogicOr
#[test]
fn test_wide_comb_logic_and_or() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            y_and: output logic,
            y_or:  output logic,
            y_and_w: output logic<128>,
            y_or_w:  output logic<128>
        ) {
            assign y_and   = a && b;
            assign y_or    = a || b;
            assign y_and_w = a && b;
            assign y_or_w  = a || b;
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y_and = sim.signal("y_and");
    let y_or = sim.signal("y_or");
    let y_and_w = sim.signal("y_and_w");
    let y_or_w = sim.signal("y_or_w");

    // 0 && 0 = 0, 0 || 0 = 0
    sim.modify(|io| {
        io.set(a, 0u128);
        io.set(b, 0u128);
    })
    .unwrap();
    assert_eq!(sim.get(y_and), 0u8.into());
    assert_eq!(sim.get(y_or), 0u8.into());
    assert_eq!(sim.get(y_and_w), 0u128.into());
    assert_eq!(sim.get(y_or_w), 0u128.into());

    // nonzero(high chunk) && 0 = 0, nonzero(high chunk) || 0 = 1
    sim.modify(|io| {
        io.set(a, 1u128 << 96);
        io.set(b, 0u128);
    })
    .unwrap();
    assert_eq!(sim.get(y_and), 0u8.into());
    assert_eq!(sim.get(y_or), 1u8.into());
    assert_eq!(sim.get(y_and_w), 0u128.into());
    assert_eq!(sim.get(y_or_w), 1u128.into());

    // nonzero && nonzero = 1, nonzero || nonzero = 1
    sim.modify(|io| {
        io.set(a, 1u128 << 96);
        io.set(b, 1u128 << 4);
    })
    .unwrap();
    assert_eq!(sim.get(y_and), 1u8.into());
    assert_eq!(sim.get(y_or), 1u8.into());
    assert_eq!(sim.get(y_and_w), 1u128.into());
    assert_eq!(sim.get(y_or_w), 1u128.into());
}

// ============================================================
// Wide (128-bit) multiplication
// ============================================================

/// 128-bit multiply in always_comb.
#[test]
fn test_wide_comb_mul() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            y: output logic<128>
        ) {
            assign y = a * b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    // 3 * 7 = 21
    sim.modify(|io| {
        io.set(a, 3u128);
        io.set(b, 7u128);
    })
    .unwrap();
    assert_eq!(sim.get(y), 21u128.into());

    // Large: 0x1_0000_0000 * 0x1_0000_0000 = 0x1_0000_0000_0000_0000
    sim.modify(|io| {
        io.set(a, 0x1_0000_0000u128);
        io.set(b, 0x1_0000_0000u128);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0x1_0000_0000_0000_0000u128.into());
}

/// 128-bit multiply in always_ff.
#[test]
fn test_wide_ff_mul() {
    let code = r#"
        module Top (
            clk: input  clock,
            rst: input  reset,
            a:   input  logic<128>,
            b:   input  logic<128>,
            y:   output logic<128>
        ) {
            var r: logic<128>;
            always_ff (clk, rst) {
                if_reset {
                    r = 128'd0;
                } else {
                    r = a * b;
                }
            }
            assign y = r;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(a, 0u128);
        io.set(b, 0u128);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), 0u128.into());

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(a, 12345u128);
        io.set(b, 67890u128);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), (12345u128 * 67890u128).into());
}

// ============================================================
// Wide (128-bit) division / modulo
// ============================================================

/// 128-bit division in always_comb.
#[test]
fn test_wide_comb_div() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            q: output logic<128>
        ) {
            assign q = a / b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(a, 100u128);
        io.set(b, 7u128);
    })
    .unwrap();
    assert_eq!(sim.get(q), 14u128.into());

    // Large dividend spanning multiple chunks
    let big_a: u128 = 1u128 << 100;
    let big_b: u128 = 1u128 << 50;
    sim.modify(|io| {
        io.set(a, big_a);
        io.set(b, big_b);
    })
    .unwrap();
    assert_eq!(sim.get(q), (1u128 << 50).into());
}

/// 128-bit modulo in always_comb.
#[test]
fn test_wide_comb_rem() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            r: output logic<128>
        ) {
            assign r = a % b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let r = sim.signal("r");

    sim.modify(|io| {
        io.set(a, 100u128);
        io.set(b, 7u128);
    })
    .unwrap();
    assert_eq!(sim.get(r), 2u128.into());

    let big_a: u128 = (1u128 << 100) + 42;
    let big_b: u128 = 1u128 << 100;
    sim.modify(|io| {
        io.set(a, big_a);
        io.set(b, big_b);
    })
    .unwrap();
    assert_eq!(sim.get(r), 42u128.into());
}

// ============================================================
// Wide (128-bit) XNOR (binary)
// ============================================================

/// 128-bit XNOR in always_comb.
#[test]
fn test_wide_comb_bitxnor() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            y: output logic<128>
        ) {
            assign y = a ~^ b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    let va: u128 = 0xFFFF_FFFF_FFFF_FFFF_0000_0000_0000_0000;
    let vb: u128 = 0xFFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF;
    let expected = !(va ^ vb);
    sim.modify(|io| {
        io.set(a, va);
        io.set(b, vb);
    })
    .unwrap();
    assert_eq!(sim.get(y), expected.into());
}

// ============================================================
// Wide (128-bit) arithmetic shift right
// ============================================================

/// 128-bit arithmetic shift right in always_comb.
#[test]
fn test_wide_comb_sar() {
    let code = r#"
        module Top (
            a: input  signed logic<128>,
            b: input  logic<7>,
            y: output signed logic<128>
        ) {
            assign y = a >>> b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    // Positive value: arithmetic shift right = logical shift right
    sim.modify(|io| {
        io.set(a, 0x100u128);
        io.set(b, 4u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0x10u128.into());

    // Negative value (MSB set): should sign-extend
    let neg_val: u128 = u128::MAX - 15; // -16 in two's complement
    sim.modify(|io| {
        io.set(a, neg_val);
        io.set(b, 2u8);
    })
    .unwrap();
    // -16 >>> 2 = -4, which is u128::MAX - 3
    assert_eq!(sim.get(y), (u128::MAX - 3).into());
}

// ============================================================
// Wide (128-bit) signed comparisons
// ============================================================

/// 128-bit signed less-than.
#[test]
fn test_wide_comb_signed_lt() {
    let code = r#"
        module Top (
            a: input  signed logic<128>,
            b: input  signed logic<128>,
            y: output logic
        ) {
            assign y = a <: b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    // -1 < 1 should be true
    sim.modify(|io| {
        io.set(a, u128::MAX); // -1
        io.set(b, 1u128);
    })
    .unwrap();
    assert_eq!(sim.get(y), 1u8.into());

    // 1 < -1 should be false
    sim.modify(|io| {
        io.set(a, 1u128);
        io.set(b, u128::MAX); // -1
    })
    .unwrap();
    assert_eq!(sim.get(y), 0u8.into());

    // Equal values: not less than
    sim.modify(|io| {
        io.set(a, 42u128);
        io.set(b, 42u128);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0u8.into());
}

/// 128-bit signed greater-than.
#[test]
fn test_wide_comb_signed_gt() {
    let code = r#"
        module Top (
            a: input  signed logic<128>,
            b: input  signed logic<128>,
            y: output logic
        ) {
            assign y = a >: b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    // 1 > -1 should be true
    sim.modify(|io| {
        io.set(a, 1u128);
        io.set(b, u128::MAX); // -1
    })
    .unwrap();
    assert_eq!(sim.get(y), 1u8.into());

    // -1 > 1 should be false
    sim.modify(|io| {
        io.set(a, u128::MAX); // -1
        io.set(b, 1u128);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0u8.into());
}

// ============================================================
// Wide (128-bit) reduction operators
// ============================================================

/// 128-bit reduction NAND.
#[test]
fn test_wide_comb_reduction_nand() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            y: output logic
        ) {
            assign y = ~&a;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.modify(|io| io.set(a, u128::MAX)).unwrap();
    assert_eq!(sim.get(y), 0u8.into()); // all 1s -> 0

    sim.modify(|io| io.set(a, u128::MAX - 1)).unwrap();
    assert_eq!(sim.get(y), 1u8.into()); // not all 1s -> 1
}

/// 128-bit reduction NOR.
#[test]
fn test_wide_comb_reduction_nor() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            y: output logic
        ) {
            assign y = ~|a;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.modify(|io| io.set(a, 0u128)).unwrap();
    assert_eq!(sim.get(y), 1u8.into()); // all 0s -> 1

    sim.modify(|io| io.set(a, 1u128)).unwrap();
    assert_eq!(sim.get(y), 0u8.into()); // not all 0s -> 0
}

/// 128-bit reduction XNOR.
#[test]
fn test_wide_comb_reduction_xnor() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            y: output logic
        ) {
            assign y = ~^a;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.modify(|io| io.set(a, 0u128)).unwrap();
    assert_eq!(sim.get(y), 1u8.into()); // 0 ones (even) -> 1

    sim.modify(|io| io.set(a, 1u128)).unwrap();
    assert_eq!(sim.get(y), 0u8.into()); // 1 one (odd) -> 0

    sim.modify(|io| io.set(a, 3u128)).unwrap();
    assert_eq!(sim.get(y), 1u8.into()); // 2 ones (even) -> 1
}



