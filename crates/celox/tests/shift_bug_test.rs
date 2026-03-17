use celox::{BigUint, Simulator};

// Test 1: shift inside if_reset else branch
#[test]
fn test_shift_in_if_reset() {
    let code = r#"
        module Top (
            clk: input  clock,
            rst: input  reset_async_high,
            a: input logic<8>,
            o: output logic<8>
        ) {
            var r: logic<8>;
            always_ff (clk, rst) {
                if_reset {
                    r = 0;
                } else {
                    r = a << 2;
                }
            }
            assign o = r;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let o = sim.signal("o");
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(a, 0x0Fu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), BigUint::from(0u64), "should be 0 after reset");
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(0x3Cu64),
        "shift in if_reset: got {:x}",
        sim.get(o)
    );
}

// Test 2: shift inside for loop (non-self-referencing)
#[test]
fn test_shift_in_for_loop() {
    let code = r#"
        module Top (
            clk: input clock,
            a: input logic<32>,
            o: output logic<32>
        ) {
            always_ff (clk) {
                for _i: u32 in 0..1 {
                    o = a << 4;
                }
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let o = sim.signal("o");
    sim.modify(|io| io.set(a, 0x12345678u32)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(0x23456780u32),
        "shift in for: got {:x}",
        sim.get(o)
    );
}

// Test 3: if_reset + for + shift (non-self-referencing)
#[test]
fn test_shift_ifreset_for() {
    let code = r#"
        module Top (
            clk: input  clock,
            rst: input  reset_async_high,
            a: input logic<8>,
            o: output logic<8>
        ) {
            var r: logic<8>;
            always_ff (clk, rst) {
                if_reset {
                    r = 8'h00;
                } else {
                    for _i: u32 in 0..1 {
                        r = a << 1;
                    }
                }
            }
            assign o = r;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let o = sim.signal("o");
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(a, 0x55u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), BigUint::from(0u64), "after reset");
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(0xAAu64),
        "if_reset+for+shift: got {:x}",
        sim.get(o)
    );
}

// Test 4: right shift in if_reset
#[test]
fn test_right_shift_in_if_reset() {
    let code = r#"
        module Top (
            clk: input  clock,
            rst: input  reset_async_high,
            a: input logic<16>,
            o_shr: output logic<16>,
            o_shl: output logic<16>
        ) {
            var r_shr: logic<16>;
            var r_shl: logic<16>;
            always_ff (clk, rst) {
                if_reset {
                    r_shr = 0;
                    r_shl = 0;
                } else {
                    r_shr = a >> 4;
                    r_shl = a << 4;
                }
            }
            assign o_shr = r_shr;
            assign o_shl = r_shl;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let o_shr = sim.signal("o_shr");
    let o_shl = sim.signal("o_shl");
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(a, 0xABCDu16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(o_shr),
        BigUint::from(0x0ABCu64),
        "right shift in if_reset: got {:x}",
        sim.get(o_shr)
    );
    assert_eq!(
        sim.get(o_shl),
        BigUint::from(0xBCD0u64),
        "left shift in if_reset: got {:x}",
        sim.get(o_shl)
    );
}

// Test 5: dynamic shift amount in if_reset
#[test]
fn test_dynamic_shift_in_if_reset() {
    let code = r#"
        module Top (
            clk: input  clock,
            rst: input  reset_async_high,
            a: input logic<16>,
            sh: input logic<4>,
            o: output logic<16>
        ) {
            var r: logic<16>;
            always_ff (clk, rst) {
                if_reset {
                    r = 0;
                } else {
                    r = a << sh;
                }
            }
            assign o = r;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let sh = sim.signal("sh");
    let o = sim.signal("o");
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(a, 0x00FFu16);
        io.set(sh, 4u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), BigUint::from(0u64), "after reset");
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(0x0FF0u64),
        "dynamic shift in if_reset: got {:x}",
        sim.get(o)
    );
}

// Test 6: shift with for loop index (writing to different array elements)
#[test]
fn test_shift_to_array_by_loop_index() {
    let code = r#"
        module Top (
            clk: input clock,
            a: input logic<8>,
            o0: output logic<8>,
            o1: output logic<8>,
            o2: output logic<8>,
            o3: output logic<8>
        ) {
            var arr: logic<8> [4];
            always_ff (clk) {
                for i: u32 in 0..4 {
                    arr[i] = a << i;
                }
            }
            assign o0 = arr[0];
            assign o1 = arr[1];
            assign o2 = arr[2];
            assign o3 = arr[3];
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");
    let o2 = sim.signal("o2");
    let o3 = sim.signal("o3");
    sim.modify(|io| io.set(a, 0x01u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o0), BigUint::from(0x01u64), "arr[0] = a << 0");
    assert_eq!(sim.get(o1), BigUint::from(0x02u64), "arr[1] = a << 1");
    assert_eq!(sim.get(o2), BigUint::from(0x04u64), "arr[2] = a << 2");
    assert_eq!(sim.get(o3), BigUint::from(0x08u64), "arr[3] = a << 3");
}

// Test 7: shift amount wider than value (for loop const is 32-bit)
#[test]
fn test_shift_with_wide_const_amount() {
    // For loop unrolling creates 32-bit const shift amounts.
    // Ensure the shift result width is determined by the LHS, not widened by the RHS.
    let code = r#"
        module Top (
            clk: input clock,
            a: input logic<8>,
            o: output logic<8>
        ) {
            always_ff (clk) {
                for _i: u32 in 0..1 {
                    o = a << 2;
                }
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let o = sim.signal("o");
    sim.modify(|io| io.set(a, 0x0Fu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(o),
        BigUint::from(0x3Cu64),
        "8-bit shift with 32-bit const amount: got {:x}",
        sim.get(o)
    );
}
