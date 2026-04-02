use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// ============================================================
// Division / Modulo (always_comb)
// ============================================================

// ============================================================
// BitXnor (binary)
// ============================================================

// ============================================================
// Reduction BitNand / BitNor / BitXnor (unary)
// ============================================================

all_backends! {

    fn test_ternary_operator(sim) {
        @setup { let code = r#"
module Top (sel: input logic, a: input logic<8>, b: input logic<8>, o: output logic<8>) {
assign o = if sel ? a : b ;
}
"#; }
        @build Simulator::builder(code, "Top");
    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 0xAAu8);
        io.set(b, 0xBBu8);
    })
    .unwrap();

    // sel = 1 -> o = a
    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(o), 0xAAu64.into());

    // sel = 0 -> o = b
    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(o), 0xBBu64.into());

    }

    fn test_nested_ternary(sim) {
        @setup { let code = r#"
module Top (s1: input logic, s2: input logic, a: input logic<8>, b: input logic<8>, c: input logic<8>, o: output logic<8>) {
assign o = if s1 ? (if s2 ? a : b) : c;
}
"#; }
        @build Simulator::builder(code, "Top");
    let s1 = sim.signal("s1");
    let s2 = sim.signal("s2");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 0xAAu8);
        io.set(b, 0xBBu8);
    })
    .unwrap();

    // s1=1, s2=0 -> b (0xBB)
    sim.modify(|io| {
        io.set(s1, 1u8);
        io.set(s2, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0xBBu64.into());

    }

    fn test_bitwise_operations(sim) {
        @setup { let code = r#"
module Top (a: input logic<8>, b: input logic<8>, o_and: output logic<8>, o_or: output logic<8>, o_xor: output logic<8>) {
always_comb {
o_and = a & b;
o_or  = a | b;
o_xor = a ^ b;
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o_and = sim.signal("o_and");
    let o_or = sim.signal("o_or");
    let o_xor = sim.signal("o_xor");

    sim.modify(|io| {
        io.set(a, 0xA5u8);
        io.set(b, 0x5Au8);
    })
    .unwrap();

    assert_eq!(sim.get(o_and), 0x00u8.into());
    assert_eq!(sim.get(o_or), 0xFFu8.into());
    assert_eq!(sim.get(o_xor), 0xFFu8.into());

    }

    fn test_shift_logical_vs_arithmetic(sim) {
        @setup { let code = r#"
module Top (a: input logic<8>, o_shr: output logic<8>, o_sar: output logic<8>) {
always_comb {
o_shr = a >> 2;
o_sar = a >>> 2;
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let o_shr = sim.signal("o_shr");
    let o_sar = sim.signal("o_sar");

    sim.modify(|io| io.set(a, 0x80u8)).unwrap();

    assert_eq!(sim.get(o_shr), 0x20u8.into());
    // `a` is unsigned logic<8>, so arithmetic-right shift should behave like logical shift.
    assert_eq!(sim.get(o_sar), 0x20u8.into());

    }

    fn test_signed_arithmetic_shift_right(sim) {
        @setup { let code = r#"
module Top (a: input i8, o_sar: output i8) {
always_comb {
o_sar = a >>> 2;
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let o_sar = sim.signal("o_sar");

    sim.modify(|io| io.set(a, 0x80u8)).unwrap(); // -128
    assert_eq!(sim.get(o_sar), 0xE0u8.into()); // -32

    }

    fn test_subtraction_underflow(sim) {
        @setup { let code = r#"
module Top (a: input logic<8>, b: input logic<8>, o: output logic<8>) {
assign o = a - b;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 0x05u8);
        io.set(b, 0x0Au8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0xFBu8.into());

    }

    fn test_unary_operations(sim) {
        @setup { let code = r#"
module Top (a: input logic<8>, o_bitnot: output logic<8>) {
always_comb {
o_bitnot = ~a;
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let o = sim.signal("o_bitnot");

    sim.modify(|io| io.set(a, 0x55u8)).unwrap();
    assert_eq!(sim.get(o), 0xAAu8.into());

    }

    fn test_unary_plus_operator(sim) {
        @setup { let code = r#"
module Top (a: input logic<8>, o: output logic<8>) {
always_comb {
o = +a;
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.modify(|io| io.set(a, 0xA5u8)).unwrap();
    assert_eq!(sim.get(o), 0xA5u8.into());

    }

    fn test_comparisons(sim) {
        @setup { let code = r#"
module Top (a: input logic<8>, b: input logic<8>, o_lt: output logic<1>, o_ge: output logic<1>) {
always_comb {
o_lt = a <: b;
o_ge = a >= b;
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o_lt = sim.signal("o_lt");
    let o_ge = sim.signal("o_ge");

    sim.modify(|io| {
        io.set(a, 10u8);
        io.set(b, 20u8);
    })
    .unwrap();

    assert_eq!(sim.get(o_lt), 1u8.into());
    assert_eq!(sim.get(o_ge), 0u8.into());

    }

    fn test_signed_comparison_and_extension(sim) {
        @setup { let code = r#"
module Top (a: input i8, b: input i8, o_lt: output logic) {
assign o_lt = a <: b;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o_lt = sim.signal("o_lt");

    sim.modify(|io| {
        io.set(a, 0xFBu8); // -5
        io.set(b, 0x02u8); // 2
    })
    .unwrap();
    assert_eq!(sim.get(o_lt), 1u8.into());

    }

    fn test_logical_operators_execution(sim) {
        @setup { let code = r#"
module Top (
a: input logic<8>,
b: input logic<8>,
o_and: output logic,
o_or:  output logic
) {
assign o_and = (|a) && (|b);
assign o_or  = (|a) || (|b);
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o_and = sim.signal("o_and");
    let o_or = sim.signal("o_or");

    sim.modify(|io| {
        io.set(a, 0x55u8);
        io.set(b, 0x00u8);
    })
    .unwrap();

    assert_eq!(sim.get(o_and), 0u8.into());
    assert_eq!(sim.get(o_or), 1u8.into());

    }

    fn test_reduction_operators_execution(sim) {
        @setup { let code = r#"
module Top (
a: input logic<4>,
o_and: output logic,
o_or:  output logic
) {
assign o_and = &a;
assign o_or  = |a;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let o_and = sim.signal("o_and");
    let o_or = sim.signal("o_or");

    sim.modify(|io| io.set(a, 0xEu8)).unwrap();

    assert_eq!(sim.get(o_and), 0u8.into());
    assert_eq!(sim.get(o_or), 1u8.into());

    }

    fn test_pow_operator_constant_exponent(sim) {
        @setup { let code = r#"
module Top (a: input logic<8>, o: output logic<8>) {
assign o = a ** 3;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.modify(|io| io.set(a, 2u8)).unwrap();
    assert_eq!(sim.get(o), 8u8.into());

    sim.modify(|io| io.set(a, 3u8)).unwrap();
    assert_eq!(sim.get(o), 27u8.into());

    }

    fn test_as_operator_passthrough(sim) {
        @setup { let code = r#"
module Top (a: input logic<8>, o: output logic<8>) {
assign o = a as u8;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.modify(|io| io.set(a, 0xA5u8)).unwrap();
    assert_eq!(sim.get(o), 0xA5u8.into());

    }

    fn test_pow_operator_constant_exponent_ff(sim) {
        @setup { let code = r#"
module Top (clk: input clock, a: input logic<8>, o: output logic<8>) {
var r: logic<8>;
always_ff {
r = a ** 2;
}
assign o = r;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.modify(|io| io.set(a, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), 25u8.into());

    }

    fn test_signed_comparison_after_as_cast(sim) {
        @ignore_on(wasm, veryl);
        @setup { let code = r#"
module Top (a: input logic<8>, b: input logic<8>, y: output logic) {
assign y = (a as i8) <: (b as i8);
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    // -1 < 1 should be true after signed cast.
    sim.modify(|io| {
        io.set(a, 0xFFu8);
        io.set(b, 0x01u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 1u8.into());

    }

    fn test_cast_signed_to_unsigned_affects_comparison(sim) {
        @setup { let code = r#"
module Top (a: input i8, b: input i8, y: output logic) {
assign y = (a as u8) <: (b as u8);
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    // Signed: -1 < 1 is true, but after cast to u8: 255 < 1 is false.
    sim.modify(|io| {
        io.set(a, 0xFFu8);
        io.set(b, 0x01u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0u8.into());

    }

    // Basic unsigned division in always_comb.
    fn test_comb_div(sim) {
        @setup { let code = r#"
module Top (
a: input  logic<16>,
b: input  logic<16>,
q: output logic<16>
) {
assign q = a / b;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(a, 100u16);
        io.set(b, 7u16);
    })
    .unwrap();
    assert_eq!(sim.get(q), 14u16.into()); // 100/7 = 14

    sim.modify(|io| {
        io.set(a, 255u16);
        io.set(b, 16u16);
    })
    .unwrap();
    assert_eq!(sim.get(q), 15u16.into()); // 255/16 = 15

    }

    // Basic unsigned modulo in always_comb.
    fn test_comb_rem(sim) {
        @setup { let code = r#"
module Top (
a: input  logic<16>,
b: input  logic<16>,
r: output logic<16>
) {
assign r = a % b;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let r = sim.signal("r");

    sim.modify(|io| {
        io.set(a, 100u16);
        io.set(b, 7u16);
    })
    .unwrap();
    assert_eq!(sim.get(r), 2u16.into()); // 100%7 = 2

    sim.modify(|io| {
        io.set(a, 255u16);
        io.set(b, 16u16);
    })
    .unwrap();
    assert_eq!(sim.get(r), 15u16.into()); // 255%16 = 15

    }

    // Division in always_ff.
    fn test_ff_div(sim) {
        @setup { let code = r#"
module Top (
clk: input  clock,
rst: input  reset,
a:   input  logic<16>,
b:   input  logic<16>,
q:   output logic<16>
) {
var r: logic<16>;
always_ff (clk, rst) {
if_reset {
r = 16'd0;
} else {
r = a / b;
}
}
assign q = r;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(a, 0u16);
        io.set(b, 1u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u16.into());

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(a, 42u16);
        io.set(b, 5u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 8u16.into()); // 42/5 = 8

    }

    // Modulo in always_ff.
    fn test_ff_rem(sim) {
        @setup { let code = r#"
module Top (
clk: input  clock,
rst: input  reset,
a:   input  logic<16>,
b:   input  logic<16>,
q:   output logic<16>
) {
var r: logic<16>;
always_ff (clk, rst) {
if_reset {
r = 16'd0;
} else {
r = a % b;
}
}
assign q = r;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(a, 0u16);
        io.set(b, 1u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u16.into());

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(a, 42u16);
        io.set(b, 5u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 2u16.into()); // 42%5 = 2

    }

    // XNOR in always_comb: ~(a ^ b)
    fn test_comb_bitxnor(sim) {
        @setup { let code = r#"
module Top (
a: input  logic<8>,
b: input  logic<8>,
y: output logic<8>
) {
assign y = a ~^ b;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    sim.modify(|io| {
        io.set(a, 0xF0u8);
        io.set(b, 0xFFu8);
    })
    .unwrap();
    // XNOR: ~(0xF0 ^ 0xFF) = ~0x0F = 0xF0
    assert_eq!(sim.get(y), 0xF0u8.into());

    sim.modify(|io| {
        io.set(a, 0xAAu8);
        io.set(b, 0x55u8);
    })
    .unwrap();
    // XNOR: ~(0xAA ^ 0x55) = ~0xFF = 0x00
    assert_eq!(sim.get(y), 0x00u8.into());

    }

    // XNOR in always_ff.
    fn test_ff_bitxnor(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (
clk: input  clock,
rst: input  reset,
a:   input  logic<8>,
b:   input  logic<8>,
y:   output logic<8>
) {
var r: logic<8>;
always_ff (clk, rst) {
if_reset {
r = 8'd0;
} else {
r = a ~^ b;
}
}
assign y = r;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(a, 0u8);
        io.set(b, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), 0u8.into());

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(a, 0xF0u8);
        io.set(b, 0xFFu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), 0xF0u8.into());

    }

    // Reduction NAND: ~&a  (0 if all bits 1, else 1)
    fn test_comb_reduction_nand(sim) {
        @setup { let code = r#"
module Top (
a: input  logic<8>,
y: output logic
) {
assign y = ~&a;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.modify(|io| io.set(a, 0xFFu8)).unwrap();
    assert_eq!(sim.get(y), 0u8.into()); // all 1s -> NAND = 0

    sim.modify(|io| io.set(a, 0xFEu8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into()); // not all 1s -> NAND = 1

    sim.modify(|io| io.set(a, 0x00u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into()); // all 0s -> NAND = 1

    }

    // Reduction NOR: ~|a  (1 if all bits 0, else 0)
    fn test_comb_reduction_nor(sim) {
        @setup { let code = r#"
module Top (
a: input  logic<8>,
y: output logic
) {
assign y = ~|a;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.modify(|io| io.set(a, 0x00u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into()); // all 0s -> NOR = 1

    sim.modify(|io| io.set(a, 0x01u8)).unwrap();
    assert_eq!(sim.get(y), 0u8.into()); // not all 0s -> NOR = 0

    sim.modify(|io| io.set(a, 0xFFu8)).unwrap();
    assert_eq!(sim.get(y), 0u8.into()); // NOR = 0

    }

    // Reduction XNOR: ~^a  (1 if even number of 1s, i.e. even parity)
    fn test_comb_reduction_xnor(sim) {
        @setup { let code = r#"
module Top (
a: input  logic<8>,
y: output logic
) {
assign y = ~^a;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.modify(|io| io.set(a, 0x00u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into()); // 0 ones (even) -> 1

    sim.modify(|io| io.set(a, 0x01u8)).unwrap();
    assert_eq!(sim.get(y), 0u8.into()); // 1 one (odd) -> 0

    sim.modify(|io| io.set(a, 0x03u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into()); // 2 ones (even) -> 1

    }

    // Reduction NAND in always_ff.
    fn test_ff_reduction_nand(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (
clk: input  clock,
rst: input  reset,
a:   input  logic<8>,
y:   output logic
) {
var r: logic;
always_ff (clk, rst) {
if_reset {
r = 1'b0;
} else {
r = ~&a;
}
}
assign y = r;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(a, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), 0u8.into());

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(a, 0xFFu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), 0u8.into()); // all 1s -> 0

    sim.modify(|io| io.set(a, 0xFEu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), 1u8.into()); // not all 1s -> 1

    }

    fn test_ff_comb_constant_folding_consistency(sim) {
        @setup { let code = r#"
module Top (
clk: input clock,
o_ff: output logic<128>,
o_comb: output logic<128>
) {
always_ff (clk) {
o_ff = 32'hffff_ffff + 1;
}
always_comb {
o_comb = 32'hffff_ffff + 1;
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let o_ff = sim.signal("o_ff");
    let o_comb = sim.signal("o_comb");

    // Before tick: o_comb is evaluated, o_ff is 0
    let expected = BigUint::from(1u32) << 32;
    assert_eq!(
        sim.get(o_comb),
        expected,
        "always_comb constant folding failed"
    );
    assert_eq!(sim.get(o_ff), BigUint::from(0u8));

    // After tick: o_ff is evaluated
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_ff), expected, "always_ff constant folding failed");
    assert_eq!(sim.get(o_comb), expected);

    }

    // Reduction NOR in always_ff.
    fn test_ff_reduction_nor(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (
clk: input  clock,
rst: input  reset,
a:   input  logic<8>,
y:   output logic
) {
var r: logic;
always_ff (clk, rst) {
if_reset {
r = 1'b0;
} else {
r = ~|a;
}
}
assign y = r;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(a, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), 0u8.into());

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(a, 0x00u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), 1u8.into()); // all 0s -> 1

    sim.modify(|io| io.set(a, 0x01u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(y), 0u8.into()); // not all 0s -> 0

    }

    fn test_mixed_signed_unsigned_comparison(sim) {
        @ignore_on(veryl);
        @setup { // Mixed signed/unsigned should be treated as unsigned (Clause 11.8.1)
let code = r#"
module Top (
o_const: output logic,
o_var:   output logic,
o_signed_op: output logic
) {
// -8'sd1 (255) >: 8'd1 (1) -> true (1)
assign o_const = -8'sd1 >: 8'd1;
var a: i8;
var b: u8;
always_comb {
a = -8'sd1;
b = 8'd1;
// Treated as unsigned: 255 > 1 -> true (1)
o_var = a >: b;
// To force signed comparison (if desired), both sides must be signed
// but Veryl doesn't have a direct "signed comparison" operator that
// overrides 11.8.1 other than casting both to signed.
o_signed_op = a >: (b as i8);
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let o_const = sim.signal("o_const");
    let o_var = sim.signal("o_var");
    let o_signed_op = sim.signal("o_signed_op");

    assert_eq!(
        sim.get(o_const),
        1u8.into(),
        "Mixed signed/unsigned constant comparison failed"
    );
    assert_eq!(
        sim.get(o_var),
        1u8.into(),
        "Mixed signed/unsigned variable comparison failed"
    );
    assert_eq!(
        sim.get(o_signed_op),
        0u8.into(),
        "Mixed signed/unsigned (cast to signed) comparison failed"
    );

    }
}

#[test]
fn test_nested_ternary_concat_hybrid() {
    let code = r#"
        module Top (sel: input logic, a: input logic<4>, b: input logic<4>, c: input logic<8>, o: output logic<8>) {
            always_comb {
                o = if sel ? {a, b} : (if (a == b) ? c : 8'hEE);
            }
        }
    "#;
    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_ok(),
        "Should handle deeply nested expression structures"
    );
}
