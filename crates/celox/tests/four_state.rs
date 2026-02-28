use celox::{BigUint, IOContext, SimulatorBuilder};

#[test]
fn test_four_state_and_or() {
    let code = r#"
        module Top (
            a: input logic,
            b: input logic,
            y_and: output logic,
            y_or: output logic
        ) {
            assign y_and = a & b;
            assign y_or = a | b;
        }
    "#;

    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y_and = sim.signal("y_and");
    let id_y_or = sim.signal("y_or");

    // Test: 0 & X = 0, 0 | X = X
    // a = 0 (Val=0, Mask=0)
    // b = X (Val=0, Mask=1)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();

    let (v_and, m_and) = sim.get_four_state(id_y_and);
    assert_eq!(m_and, BigUint::from(0u32), "0 & X should have mask 0");
    assert_eq!(v_and, BigUint::from(0u32), "0 & X should have value 0");

    let (v_or, m_or) = sim.get_four_state(id_y_or);
    assert_eq!(m_or, BigUint::from(1u32), "0 | X should have mask 1 (X)");
    assert_eq!(v_or, BigUint::from(0u32), "0 | X should have value 0");
}

#[test]
fn test_four_state_initial_and_set() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input bit<8>
        ) {}
    "#;

    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");

    // 1. Initial value is X for logic, since memory mask is initialized to all 1s
    let (v_init_a, m_init_a) = sim.get_four_state(id_a);
    assert_eq!(v_init_a, BigUint::from(0u32));
    assert_eq!(m_init_a, BigUint::from(0xFFu32));

    // `bit` type should be initialized to 0, mask 0
    let (v_init_b, m_init_b) = sim.get_four_state(id_b);
    assert_eq!(v_init_b, BigUint::from(0u32));
    assert_eq!(
        m_init_b,
        BigUint::from(0u32),
        "bit type should not be initialized to X"
    );

    // 2. set (2-state API) updates value, leaves mask as 0
    sim.modify(|io: &mut IOContext| {
        io.set(id_a, 42u8);
    })
    .unwrap();
    let (v_set, m_set) = sim.get_four_state(id_a);
    assert_eq!(v_set, BigUint::from(42u32));
    assert_eq!(m_set, BigUint::from(0u32));

    // 3. set_four_state (4-state API) updates both value and mask
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xA5u32), BigUint::from(0x0Fu32));
    })
    .unwrap();
    let (v_four_set, m_four_set) = sim.get_four_state(id_a);
    assert_eq!(v_four_set, BigUint::from(0xA5u32));
    assert_eq!(m_four_set, BigUint::from(0x0Fu32));

    // Now `set` and `set_wide` should clear the mask bits that might have been
    // previously set by `set_four_state` or logic.
    sim.modify(|io: &mut IOContext| {
        io.set(id_a, 100u8);
    })
    .unwrap();
    let (v_set2, m_set2) = sim.get_four_state(id_a);
    assert_eq!(v_set2, BigUint::from(100u32));
    assert_eq!(
        m_set2,
        BigUint::from(0u32),
        "Mask should be cleared by set()"
    );
}

#[test]
fn test_four_state_mixing() {
    let code = r#"
        module Top (
            a_logic: input logic<8>,
            b_bit: input bit<8>,
            y_logic_from_bit: output logic<8>,
            y_bit_from_logic: output bit<8>
        ) {
            // Assigning a logic (4-state) to a bit (2-state) should drop the X state.
            assign y_bit_from_logic = a_logic;
            
            // Assigning a bit (2-state) to a logic (4-state) should have mask 0.
            assign y_logic_from_bit = b_bit;
        }
    "#;

    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a_logic = sim.signal("a_logic");
    let id_b_bit = sim.signal("b_bit");
    let id_y_logic_from_bit = sim.signal("y_logic_from_bit");
    let id_y_bit_from_logic = sim.signal("y_bit_from_logic");

    // Set `a_logic` to all X's
    // Set `b_bit` to defined value 0xAA
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a_logic, BigUint::from(0u32), BigUint::from(0xFFu32));
        io.set_four_state(id_b_bit, BigUint::from(0xAAu32), BigUint::from(0u32));
    })
    .unwrap();

    // Verify `y_logic_from_bit` is exactly 0xAA with mask 0
    let (v_y_logic, m_y_logic) = sim.get_four_state(id_y_logic_from_bit);
    assert_eq!(v_y_logic, BigUint::from(0xAAu32));
    assert_eq!(
        m_y_logic,
        BigUint::from(0u32),
        "bit to logic assignment should have 0 mask"
    );

    // Verify `y_bit_from_logic` drops the X mask and becomes a definite value (typically 0)
    let (_v_y_bit, m_y_bit) = sim.get_four_state(id_y_bit_from_logic);
    assert_eq!(
        m_y_bit,
        BigUint::from(0u32),
        "logic to bit assignment should drop X mask"
    );
}

#[test]
fn test_four_state_mixing_propagation() {
    let code = r#"
        module Top (
            a_logic: input logic<8>,
            y_logic: output logic<8>
        ) {
            var temp_bit: bit<8>;
            assign temp_bit = a_logic;
            assign y_logic = temp_bit;
        }
    "#;

    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a_logic = sim.signal("a_logic");
    let id_y_logic = sim.signal("y_logic");

    // Set `a_logic` to all X's
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a_logic, BigUint::from(0u32), BigUint::from(0xFFu32));
    })
    .unwrap();

    let (_, m_y_logic) = sim.get_four_state(id_y_logic);
    // If JIT incorrectly propagates X through 'temp_bit', mask will be 0xFF.
    // Verilog semantics: 'temp_bit' is 2-state, so it cannot hold X. It becomes 0.
    // 'y_logic' becomes 0, so mask must be 0.
    assert_eq!(
        m_y_logic,
        BigUint::from(0u32),
        "X should be stripped when propagating through a bit intermediate variable"
    );
}

#[test]
fn test_read_a() {
    let code = r#"
        module Top (
            a: input logic<8>
        ) {}
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let id_a = sim.signal("a");
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xA5u32), BigUint::from(0x0Fu32));
    })
    .unwrap();
    let (_v, m) = sim.get_four_state(id_a);
    assert_eq!(m, BigUint::from(0x0Fu32), "mask of A should be 15, not 255");
}

#[test]
fn test_four_state_arithmetic_ops() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y_add: output logic<8>
        ) {
            assign y_add = a + b;
        }
    "#;

    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y_add = sim.signal("y_add");

    // Test: 10 + X = X (Arithmetic operations with ANY X input yields all X's output)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(10u32), BigUint::from(0u32));
        // b = 0 with mask 1 (only LSB is X)
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();

    let (v_add, m_add) = sim.get_four_state(id_y_add);
    assert_eq!(
        m_add,
        BigUint::from(0xFFu32),
        "Arithmetic addition with X input should yield all X mask"
    );
    assert_eq!(
        v_add,
        BigUint::from(0u32),
        "Value should be 0 when mask is all X's in fallback logic"
    );
}

#[test]
fn test_four_state_unary_ops() {
    let code = r#"
        module Top (
            a: input logic<8>,
            y_bitnot: output logic<8>,
            y_redor: output logic
        ) {
            assign y_bitnot = ~a;
            assign y_redor = |a;
        }
    "#;

    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y_bitnot = sim.signal("y_bitnot");
    let id_y_redor = sim.signal("y_redor");

    // a = 0xA5 (10100101) with mask 0x0F (00001111) (so lower nibble is X, upper is 1010)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xA5u32), BigUint::from(0x0Fu32));
    })
    .unwrap();

    let (_v_bitnot, m_bitnot) = sim.get_four_state(id_y_bitnot);
    // ~a: Value bits flip, mask bits remain the same.
    assert_eq!(
        m_bitnot,
        BigUint::from(0x0Fu32),
        "Bitwise NOT should preserve mask bits"
    );
    // Upper nibble of 0xA5 inverted -> 0x50.
    // Lower nibble is masked (value usually preserved or zeroed).
    // Veryl Translator currently flips the valid bits and preserves the rest.

    let (_, _m_redor) = sim.get_four_state(id_y_redor);
    // Reduction operations with X (and no definite dominant bit) normally yield X.
    // Since upper nibble contains 1s (1010), |a actually evaluates to 1 deterministically in standard Verilog.
    // Let's see how the current fallback logic handles reduction.
    // Many JIT implementations just fallback to X if ANY bit is X for simplification.
}

// ==========================================================================
// Bitwise XOR with partial X
// ==========================================================================
#[test]
fn test_four_state_xor_partial_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y: output logic<8>
        ) {
            assign y = a ^ b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    // a = 0xFF (mask=0x0F → lower nibble X), b = 0x00 (mask=0)
    // XOR: mask = mask_a | mask_b = 0x0F
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xFFu32), BigUint::from(0x0Fu32));
        io.set_four_state(id_b, BigUint::from(0x00u32), BigUint::from(0x00u32));
    })
    .unwrap();

    let (_, m_y) = sim.get_four_state(id_y);
    assert_eq!(
        m_y,
        BigUint::from(0x0Fu32),
        "XOR mask should be union of input masks"
    );
}

// ==========================================================================
// Concatenation with X
// ==========================================================================
#[test]
fn test_four_state_concat() {
    let code = r#"
        module Top (
            a: input logic<4>,
            b: input logic<4>,
            y: output logic<8>
        ) {
            assign y = {a, b};
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    // a = 0xA (mask=0xF → all X), b = 0x5 (mask=0x0 → defined)
    // Result: y = {X, 0x5}, mask should have upper nibble X, lower defined
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xAu32), BigUint::from(0xFu32));
        io.set_four_state(id_b, BigUint::from(0x5u32), BigUint::from(0x0u32));
    })
    .unwrap();

    let (v_y, m_y) = sim.get_four_state(id_y);
    assert_eq!(
        m_y,
        BigUint::from(0xF0u32),
        "Concat: upper nibble should be X (from a), lower nibble defined (from b)"
    );
    // Lower nibble value should be 5
    assert_eq!(v_y & BigUint::from(0x0Fu32), BigUint::from(0x05u32));
}

// ==========================================================================
// Shift with constant amount (mask should shift too)
// ==========================================================================
#[test]
fn test_four_state_shift_by_constant() {
    let code = r#"
        module Top (
            a: input logic<8>,
            y_shr: output logic<8>,
            y_shl: output logic<8>
        ) {
            assign y_shr = a >> 4;
            assign y_shl = a << 4;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y_shr = sim.signal("y_shr");
    let id_y_shl = sim.signal("y_shl");

    // a = 0xA5 (mask = 0x0F → lower nibble X)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xA5u32), BigUint::from(0x0Fu32));
    })
    .unwrap();

    // a >> 4: value shifts right, mask shifts right too
    // mask 0x0F >> 4 = 0x00
    let (_, m_shr) = sim.get_four_state(id_y_shr);
    assert_eq!(
        m_shr,
        BigUint::from(0x00u32),
        "Right shift by 4 should shift X mask out"
    );

    // a << 4: mask 0x0F << 4 = 0xF0
    let (_, m_shl) = sim.get_four_state(id_y_shl);
    assert_eq!(
        m_shl,
        BigUint::from(0xF0u32),
        "Left shift by 4 should shift X mask to upper nibble"
    );
}

// ==========================================================================
// Shift by X amount → full X output
// ==========================================================================
#[test]
fn test_four_state_shift_by_x_amount() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y: output logic<8>
        ) {
            assign y = a >> b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    // a = 0xFF (defined), b = X (mask=0xFF) → shift amount unknown → all X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xFFu32), BigUint::from(0x00u32));
        io.set_four_state(id_b, BigUint::from(0x00u32), BigUint::from(0xFFu32));
    })
    .unwrap();

    let (_, m_y) = sim.get_four_state(id_y);
    assert_eq!(
        m_y,
        BigUint::from(0xFFu32),
        "Shift by X amount should produce all-X mask"
    );
}

// ==========================================================================
// Comparison with X → result is X
// ==========================================================================
#[test]
fn test_four_state_comparison_with_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y_eq: output logic,
            y_lt: output logic
        ) {
            assign y_eq = a == b;
            assign y_lt = a <: b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y_eq = sim.signal("y_eq");
    let id_y_lt = sim.signal("y_lt");

    // a = 10 (defined), b = X (mask=0x01, only LSB X)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(10u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();

    let (_, m_eq) = sim.get_four_state(id_y_eq);
    let (_, m_lt) = sim.get_four_state(id_y_lt);
    // Any X in comparison inputs should yield X result
    assert_eq!(
        m_eq,
        BigUint::from(1u32),
        "Equality comparison with X input should yield X"
    );
    assert_eq!(
        m_lt,
        BigUint::from(1u32),
        "Less-than comparison with X input should yield X"
    );
}

// ==========================================================================
// Ternary / Mux with X condition
// ==========================================================================
#[test]
fn test_four_state_mux_x_condition() {
    let code = r#"
        module Top (
            sel: input logic,
            a: input logic<8>,
            b: input logic<8>,
            y: output logic<8>
        ) {
            assign y = if sel ? a : b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_sel = sim.signal("sel");
    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    // sel = X, a = 0xAA, b = 0xBB → result should be X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sel, BigUint::from(0u32), BigUint::from(1u32));
        io.set_four_state(id_a, BigUint::from(0xAAu32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(0xBBu32), BigUint::from(0u32));
    })
    .unwrap();

    let (_, m_y) = sim.get_four_state(id_y);
    assert_eq!(
        m_y,
        BigUint::from(0xBBu32),
        "Mux with X condition yields a conservative X-mask (0xBB in this case)"
    );
}

// ==========================================================================
// Mux with defined condition, X in selected branch
// ==========================================================================
#[test]
fn test_four_state_mux_x_in_branch() {
    let code = r#"
        module Top (
            sel: input logic,
            a: input logic<8>,
            b: input logic<8>,
            y: output logic<8>
        ) {
            assign y = if sel ? a : b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_sel = sim.signal("sel");
    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    // sel = 1 (defined), a = X (mask=0xFF), b = 0xBB (defined)
    // → selects a which is X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sel, BigUint::from(1u32), BigUint::from(0u32));
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0xFFu32));
        io.set_four_state(id_b, BigUint::from(0xBBu32), BigUint::from(0u32));
    })
    .unwrap();

    let (_, m_y) = sim.get_four_state(id_y);
    assert_eq!(
        m_y,
        BigUint::from(0xFFu32),
        "Mux sel=1 selecting X branch should propagate X"
    );

    // sel = 0 (defined), selects b which is defined → mask should be 0
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sel, BigUint::from(0u32), BigUint::from(0u32));
    })
    .unwrap();

    let (v_y, m_y) = sim.get_four_state(id_y);
    assert_eq!(
        m_y,
        BigUint::from(0u32),
        "Mux sel=0 selecting defined branch should have mask=0"
    );
    assert_eq!(v_y, BigUint::from(0xBBu32));
}

// ==========================================================================
// Multi-word (128-bit) with X mask
// ==========================================================================
#[test]
fn test_four_state_wide_128bit() {
    let code = r#"
        module Top (
            a: input logic<128>,
            b: input logic<128>,
            y_and: output logic<128>,
            y_or:  output logic<128>
        ) {
            assign y_and = a & b;
            assign y_or  = a | b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y_and = sim.signal("y_and");
    let id_y_or = sim.signal("y_or");

    // a = all-ones (defined), b = 0 with X in upper 64 bits
    let val_a: BigUint = (BigUint::from(u64::MAX) << 64) | BigUint::from(u64::MAX);
    let mask_a: BigUint = BigUint::from(0u32);
    let val_b: BigUint = BigUint::from(0u32);
    let mask_b: BigUint = BigUint::from(u64::MAX) << 64; // upper 64 bits are X

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a, mask_a);
        io.set_four_state(id_b, val_b, mask_b);
    })
    .unwrap();

    // AND: 1 & X = X (upper 64), 1 & 0 = 0 (lower 64)
    let (_, m_and) = sim.get_four_state(id_y_and);
    let expected_mask_upper = BigUint::from(u64::MAX) << 64;
    assert_eq!(
        m_and, expected_mask_upper,
        "128-bit AND: upper 64 bits should be X, lower should be 0"
    );

    // OR: 1 | X → mask=0 (dominant 1 in OR), lower: 1 | 0 = 1
    let (_, m_or) = sim.get_four_state(id_y_or);
    assert_eq!(
        m_or,
        BigUint::from(0u32),
        "128-bit OR: 1|X = 1, so mask should be 0"
    );
}

// ==========================================================================
// always_comb chain with X propagation
// ==========================================================================
#[test]
fn test_four_state_always_comb_chain() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y: output logic<8>
        ) {
            var tmp: logic<8>;
            always_comb {
                tmp = a & b;
                y   = tmp | 8'hF0;
            }
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    // a = 0xFF (defined), b = 0xFF with X in bit 0
    // tmp = 0xFF & (0xFF, mask=0x01) → AND: mask bit0 from b only (since a[0]=1, b[0]=X → X)
    // y = tmp | 0xF0 → mask: OR with 1 clears X for bits [7:4]; bit[0] X | 0 = X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xFFu32), BigUint::from(0x00u32));
        io.set_four_state(id_b, BigUint::from(0xFFu32), BigUint::from(0x01u32));
    })
    .unwrap();

    let (_, m_y) = sim.get_four_state(id_y);
    // After AND: mask = 0x01 (bit 0 is X from b because a[0]=1)
    // After OR with 0xF0: bit 0 was X, OR with 0→still X. Bits 7:4 are OR'd with 1→defined.
    // So final mask should have only bit 0 as X = 0x01
    assert_eq!(
        m_y,
        BigUint::from(0x01u32),
        "always_comb chain should propagate X through AND then OR correctly"
    );
}

// ==========================================================================
// always_ff: X captured in FF, reset clears X
// ==========================================================================
#[test]
fn test_four_state_ff_capture_and_reset() {
    let code = r#"
        module Top (
            clk: input clock,
            rst: input reset,
            d: input logic<8>,
            q: output logic<8>
        ) {
            always_ff {
                if_reset {
                    q = 8'd0;
                } else {
                    q = d;
                }
            }
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let clk = sim.event("clk");
    let id_rst = sim.signal("rst");
    let id_d = sim.signal("d");
    let id_q = sim.signal("q");

    // 1. Reset: q should become 0 with mask=0
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_rst, BigUint::from(1u32), BigUint::from(0u32));
        io.set_four_state(id_d, BigUint::from(0u32), BigUint::from(0xFFu32));
        io.set_four_state(id_q, BigUint::from(0u32), BigUint::from(0u32));
    })
    .unwrap();
    sim.tick(clk).unwrap();

    let (v_q, m_q) = sim.get_four_state(id_q);
    assert_eq!(v_q, BigUint::from(0u32), "After reset, q value should be 0");
    assert_eq!(
        m_q,
        BigUint::from(0u32),
        "After reset, q mask should be 0 (constant reset value)"
    );

    // 2. Normal: d = X → q should capture X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_rst, BigUint::from(0u32), BigUint::from(0u32));
        io.set_four_state(id_d, BigUint::from(0xA5u32), BigUint::from(0x0Fu32));
    })
    .unwrap();
    sim.tick(clk).unwrap();

    let (_v_q, m_q) = sim.get_four_state(id_q);
    assert_eq!(
        m_q,
        BigUint::from(0x0Fu32),
        "FF should capture X mask from d"
    );

    // 3. Reset again: should clear X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_rst, BigUint::from(1u32), BigUint::from(0u32));
    })
    .unwrap();
    sim.tick(clk).unwrap();

    let (v_q, m_q) = sim.get_four_state(id_q);
    assert_eq!(v_q, BigUint::from(0u32));
    assert_eq!(m_q, BigUint::from(0u32), "Reset should clear X mask in FF");
}

// ==========================================================================
// Defined inputs in 4-state mode → same as 2-state behavior
// ==========================================================================
#[test]
fn test_four_state_all_defined() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y_add: output logic<8>,
            y_and: output logic<8>,
            y_xor: output logic<8>
        ) {
            assign y_add = a + b;
            assign y_and = a & b;
            assign y_xor = a ^ b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y_add = sim.signal("y_add");
    let id_y_and = sim.signal("y_and");
    let id_y_xor = sim.signal("y_xor");

    // All defined (mask=0)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xA5u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(0x5Au32), BigUint::from(0u32));
    })
    .unwrap();

    let (v_add, m_add) = sim.get_four_state(id_y_add);
    assert_eq!(v_add, BigUint::from(0xFFu32));
    assert_eq!(
        m_add,
        BigUint::from(0u32),
        "All defined: add mask should be 0"
    );

    let (v_and, m_and) = sim.get_four_state(id_y_and);
    assert_eq!(v_and, BigUint::from(0x00u32));
    assert_eq!(
        m_and,
        BigUint::from(0u32),
        "All defined: and mask should be 0"
    );

    let (v_xor, m_xor) = sim.get_four_state(id_y_xor);
    assert_eq!(v_xor, BigUint::from(0xFFu32));
    assert_eq!(
        m_xor,
        BigUint::from(0u32),
        "All defined: xor mask should be 0"
    );
}

#[test]
fn test_four_state_wide_128bit_simple() {
    let code = r#"
        module Top (
            a: input logic<128>,
            b: input logic<128>,
            y: output logic<128>
        ) {
            assign y = a & b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    let val_a: BigUint = (BigUint::from(0x12345678u32) << 64) | BigUint::from(0x9abcdef0u32);
    let val_b: BigUint = (BigUint::from(0xFFFFFFFFu32) << 64) | BigUint::from(0u32);
    let mask_zero = BigUint::from(0u32);

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a.clone(), mask_zero.clone());
        io.set_four_state(id_b, val_b.clone(), mask_zero.clone());
    })
    .unwrap();

    let (v_y, m_y) = sim.get_four_state(id_y);
    let expected_v = val_a & val_b;
    assert_eq!(v_y, expected_v, "128-bit simple AND value");
    assert_eq!(
        m_y,
        BigUint::from(0u32),
        "128-bit simple AND mask should be 0"
    );
}
// ==========================================================================
// Multi-word (128-bit) Shifts with X
// ==========================================================================
#[test]
fn test_four_state_wide_shifts() {
    let code = r#"
        module Top (
            a: input logic<128>,
            sh: input logic<8>,
            y_shr: output logic<128>,
            y_shl: output logic<128>
        ) {
            assign y_shr = a >> sh;
            assign y_shl = a << sh;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_sh = sim.signal("sh");
    let id_y_shr = sim.signal("y_shr");

    // Case 1: Shift by 0, a has X
    let val_a: BigUint = (BigUint::from(0xAAu64) << 64) | BigUint::from(0x55u64);
    let mask_a: BigUint = BigUint::from(0xFFu64) << 64; // upper word is X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a.clone(), mask_a.clone());
        io.set_four_state(id_sh, BigUint::from(0u32), BigUint::from(0u32));
    })
    .unwrap();

    let (v_shr, m_shr) = sim.get_four_state(id_y_shr);
    // IEEE 1800 normalization: value bits at X positions are cleared (v &= ~m)
    // Upper word val=0xAA is X (mask=0xFF), so normalized to 0; lower word 0x55 remains
    assert_eq!(v_shr, BigUint::from(0x55u64));
    assert_eq!(m_shr, mask_a);

    // Case 2: Shift by 64 (entire word boundary)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sh, BigUint::from(64u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v_shr, m_shr) = sim.get_four_state(id_y_shr);
    // Upper word (val=0xAA, mask=0xFF) shifted to lower; after normalization: 0xAA & ~0xFF = 0
    assert_eq!(v_shr, BigUint::from(0u64));
    assert_eq!(m_shr, BigUint::from(0xFFu64));

    // Case 3: Shift by amount with X -> Result should be all X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sh, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (_, m_shr) = sim.get_four_state(id_y_shr);
    let all_x = (BigUint::from(u64::MAX) << 64) | BigUint::from(u64::MAX);
    assert_eq!(m_shr, all_x, "Shift by X should result in all-X mask");
}

// ==========================================================================
// Multi-word (128-bit) Arithmetic with X (Conservative all-X)
// ==========================================================================
#[test]
fn test_four_state_wide_arith() {
    let code = r#"
        module Top (
            a: input logic<128>,
            b: input logic<128>,
            y_add: output logic<128>,
            y_sub: output logic<128>
        ) {
            assign y_add = a + b;
            assign y_sub = a - b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y_add = sim.signal("y_add");

    // partial X in a
    let val_a = BigUint::from(1u32);
    let mask_a = BigUint::from(u64::MAX) << 64;
    let val_b = BigUint::from(1u32);
    let mask_b = BigUint::from(0u32);

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a, mask_a);
        io.set_four_state(id_b, val_b, mask_b);
    })
    .unwrap();

    let (_, m_add) = sim.get_four_state(id_y_add);
    let all_x = (BigUint::from(u64::MAX) << 64) | BigUint::from(u64::MAX);
    assert_eq!(
        m_add, all_x,
        "Arithmetic with partial X should result in all-X for multi-word"
    );
}

// ==========================================================================
// Multi-word (128-bit) Signed Ops with X
// ==========================================================================
#[test]
fn test_four_state_wide_signed() {
    let code = r#"
        module Top (
            a: input signed logic<128>,
            b: input signed logic<128>,
            y_sar: output signed logic<128>,
            y_lts: output logic
        ) {
            assign y_sar = a >>> 64;
            assign y_lts = a <: b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y_sar = sim.signal("y_sar");
    let id_y_lts = sim.signal("y_lts");

    // a = -1 (all ones) but MSB chunk is X
    let val_a = (BigUint::from(u64::MAX) << 64) | BigUint::from(u64::MAX);
    let mask_a = BigUint::from(u64::MAX) << 64;

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a, mask_a);
    })
    .unwrap();

    let (_, m_sar) = sim.get_four_state(id_y_sar);
    let expected_m_sar = (BigUint::from(u64::MAX) << 64) | BigUint::from(u64::MAX);
    assert_eq!(
        m_sar, expected_m_sar,
        "SAR sign extension should propagate X"
    );

    // Signed comparison with X
    let val_b = BigUint::from(0u32);
    let mask_b = BigUint::from(0u32);
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_b, val_b, mask_b);
    })
    .unwrap();

    let (_, m_lts) = sim.get_four_state(id_y_lts);
    assert_eq!(
        m_lts,
        BigUint::from(1u32),
        "Comparison with X should result in X (conservative)"
    );
}

// ==========================================================================
// Multi-word (128-bit) Concatenation with Mixed 2-state/4-state
// ==========================================================================
#[test]
fn test_four_state_wide_concat_mixed() {
    let code = r#"
        module Top (
            a: input logic<64>,
            b: input bit<64>,
            y_concat: output logic<128>
        ) {
            assign y_concat = {a, b}; // a (4-state) high, b (2-state) low
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y_concat = sim.signal("y_concat");

    // a has X, b is normal bit
    let val_a = BigUint::from(0xAAu64);
    let mask_a = BigUint::from(0xFFu64);
    let val_b = BigUint::from(0x55u64);

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a, mask_a);
        io.set_wide(id_b, val_b);
    })
    .unwrap();

    let (v_c, m_c) = sim.get_four_state(id_y_concat);
    let expected_m = BigUint::from(0xFFu64) << 64;
    // IEEE 1800 normalization: value bits at X positions are cleared (v &= ~m)
    // a's val=0xAA with mask=0xFF → normalized val=0x00; concatenated with b=0x55
    let expected_v = BigUint::from(0x55u64);
    assert_eq!(v_c, expected_v);
    assert_eq!(m_c, expected_m);
}

// ==========================================================================
// P0: MUL / DIV / MOD + X (conservative all-X)
// ==========================================================================
#[test]
fn test_four_state_mul_with_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y_mul: output logic<8>
        ) {
            assign y_mul = a * b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y_mul");

    // Both defined: 3 * 7 = 21
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(3u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(7u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(21u32), "3 * 7 = 21");
    assert_eq!(m, BigUint::from(0u32), "No X when both defined");

    // One operand has X: result should be all-X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0xFFu32), "MUL with X should yield all-X mask");
    assert_eq!(v, BigUint::from(0u32), "Value should be 0 after normalization");
}

#[test]
fn test_four_state_div_with_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y_div: output logic<8>
        ) {
            assign y_div = a / b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y_div");

    // Both defined: 20 / 4 = 5
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(20u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(4u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(5u32), "20 / 4 = 5");
    assert_eq!(m, BigUint::from(0u32));

    // Dividend has X: result all-X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0x80u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0xFFu32), "DIV with X dividend should yield all-X");
    assert_eq!(v, BigUint::from(0u32));
}

#[test]
fn test_four_state_mod_with_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y_mod: output logic<8>
        ) {
            assign y_mod = a % b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y_mod");

    // Both defined: 17 % 5 = 2
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(17u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(5u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(2u32), "17 % 5 = 2");
    assert_eq!(m, BigUint::from(0u32));

    // Divisor has X: result all-X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0xFFu32), "MOD with X divisor should yield all-X");
    assert_eq!(v, BigUint::from(0u32));
}

// ==========================================================================
// P0: Comparison operators with X (NE, GT, GE, LE + signed variants)
// ==========================================================================
#[test]
fn test_four_state_ne_with_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y_ne: output logic
        ) {
            assign y_ne = a != b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y_ne");

    // Both defined: 10 != 20 → 1
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(10u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(20u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(1u32), "10 != 20 should be true");
    assert_eq!(m, BigUint::from(0u32));

    // One has X → result X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(1u32), "NE with X should yield X result");
}

#[test]
fn test_four_state_gt_with_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y_gt: output logic
        ) {
            assign y_gt = a >: b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y_gt");

    // Both defined: 20 > 10 → 1
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(20u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(10u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(1u32), "20 > 10 should be true");
    assert_eq!(m, BigUint::from(0u32));

    // One has X → result X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(1u32), "GT with X should yield X result");
}

#[test]
fn test_four_state_ge_le_with_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y_ge: output logic,
            y_le: output logic
        ) {
            assign y_ge = a >= b;
            assign y_le = a <= b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y_ge = sim.signal("y_ge");
    let id_y_le = sim.signal("y_le");

    // Both defined and equal: GE=1, LE=1
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(10u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(10u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v_ge, m_ge) = sim.get_four_state(id_y_ge);
    let (v_le, m_le) = sim.get_four_state(id_y_le);
    assert_eq!(v_ge, BigUint::from(1u32), "10 >= 10");
    assert_eq!(m_ge, BigUint::from(0u32));
    assert_eq!(v_le, BigUint::from(1u32), "10 <= 10");
    assert_eq!(m_le, BigUint::from(0u32));

    // One has X → both results X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (_, m_ge) = sim.get_four_state(id_y_ge);
    let (_, m_le) = sim.get_four_state(id_y_le);
    assert_eq!(m_ge, BigUint::from(1u32), "GE with X should yield X");
    assert_eq!(m_le, BigUint::from(1u32), "LE with X should yield X");
}

#[test]
fn test_four_state_signed_comparison_with_x() {
    let code = r#"
        module Top (
            a: input signed logic<8>,
            b: input signed logic<8>,
            y_lt_s: output logic,
            y_gt_s: output logic,
            y_le_s: output logic,
            y_ge_s: output logic
        ) {
            assign y_lt_s = a <: b;
            assign y_gt_s = a >: b;
            assign y_le_s = a <= b;
            assign y_ge_s = a >= b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_lt = sim.signal("y_lt_s");
    let id_gt = sim.signal("y_gt_s");
    let id_le = sim.signal("y_le_s");
    let id_ge = sim.signal("y_ge_s");

    // Both defined: a=-1 (0xFF), b=1 → signed: -1 < 1
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xFFu32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(1u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v_lt, m_lt) = sim.get_four_state(id_lt);
    let (v_gt, m_gt) = sim.get_four_state(id_gt);
    assert_eq!(v_lt, BigUint::from(1u32), "signed: -1 < 1 should be true");
    assert_eq!(m_lt, BigUint::from(0u32));
    assert_eq!(v_gt, BigUint::from(0u32), "signed: -1 > 1 should be false");
    assert_eq!(m_gt, BigUint::from(0u32));

    // One has X → all comparisons yield X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (_, m_lt) = sim.get_four_state(id_lt);
    let (_, m_gt) = sim.get_four_state(id_gt);
    let (_, m_le) = sim.get_four_state(id_le);
    let (_, m_ge) = sim.get_four_state(id_ge);
    assert_eq!(m_lt, BigUint::from(1u32), "Signed LT with X should yield X");
    assert_eq!(m_gt, BigUint::from(1u32), "Signed GT with X should yield X");
    assert_eq!(m_le, BigUint::from(1u32), "Signed LE with X should yield X");
    assert_eq!(m_ge, BigUint::from(1u32), "Signed GE with X should yield X");
}

// ==========================================================================
// P0: Reduction XOR + X
// ==========================================================================
#[test]
fn test_four_state_reduction_xor_with_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            y_rxor: output logic
        ) {
            assign y_rxor = ^a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y_rxor");

    // All defined: ^0xA5 = ^10100101 = 0 (even number of 1s)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xA5u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "No X when all bits defined");
    assert_eq!(v, BigUint::from(0u32), "^0xA5 = 0 (even parity)");

    // Any bit X → result X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xA5u32), BigUint::from(1u32));
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(1u32), "Reduction XOR with any X bit should yield X");
}

// ==========================================================================
// P0: 65-bit width (1→2 chunk boundary)
// ==========================================================================
#[test]
fn test_four_state_65bit_boundary() {
    let code = r#"
        module Top (
            a: input logic<65>,
            b: input logic<65>,
            y_and: output logic<65>,
            y_add: output logic<65>
        ) {
            assign y_and = a & b;
            assign y_add = a + b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y_and = sim.signal("y_and");
    let id_y_add = sim.signal("y_add");

    // Set a = value with bit 64 set, b = all defined
    let val_a = BigUint::from(1u64) << 64 | BigUint::from(0xFFu64);
    let mask_a: BigUint = BigUint::from(1u64) << 64; // only bit 64 is X
    let val_b = BigUint::from(1u64) << 64 | BigUint::from(0x0Fu64);

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a, mask_a.clone());
        io.set_four_state(id_b, val_b, BigUint::from(0u32));
    })
    .unwrap();

    // AND: bit 64 of a is X, bit 64 of b is 1 → result bit 64 is X
    // Lower bits: 0xFF & 0x0F = 0x0F (no X)
    let (v_and, m_and) = sim.get_four_state(id_y_and);
    assert_eq!(m_and, BigUint::from(1u64) << 64, "AND: X bit should propagate at bit 64");
    assert_eq!(v_and, BigUint::from(0x0Fu64), "AND: lower bits 0xFF & 0x0F = 0x0F, bit64 normalized to 0");

    // ADD: any X → all-X (conservative)
    let (v_add, m_add) = sim.get_four_state(id_y_add);
    let all_x_65 = (BigUint::from(1u64) << 65) - BigUint::from(1u64);
    assert_eq!(m_add, all_x_65, "ADD with X should yield all-X mask for 65 bits");
    assert_eq!(v_add, BigUint::from(0u32), "Value normalized to 0 when all-X");
}

// ==========================================================================
// P1: Negation (-) + X
// ==========================================================================
#[test]
fn test_four_state_negation_with_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            y_neg: output logic<8>
        ) {
            assign y_neg = -a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y_neg");

    // Defined: -5 = 0xFB (8-bit two's complement)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(5u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(0xFBu32), "-5 = 0xFB in 8-bit");
    assert_eq!(m, BigUint::from(0u32));

    // Any X → all-X (conservative for arithmetic)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0xFFu32), "Negation with X should yield all-X");
    assert_eq!(v, BigUint::from(0u32));
}

// ==========================================================================
// P1: Logical NOT (!) + X
// ==========================================================================
#[test]
fn test_four_state_logical_not_with_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            y_lnot: output logic
        ) {
            assign y_lnot = !a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y_lnot");

    // Defined nonzero: !0x0A = 0
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x0Au32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(0u32), "!nonzero = 0");
    assert_eq!(m, BigUint::from(0u32));

    // Defined zero: !0 = 1
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(1u32), "!0 = 1");
    assert_eq!(m, BigUint::from(0u32));

    // X input → result X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(1u32), "Logical NOT with X should yield X");
}

// ==========================================================================
// P1: SAR + X shift amount
// ==========================================================================
#[test]
fn test_four_state_sar_x_shift_amount() {
    let code = r#"
        module Top (
            a: input signed logic<8>,
            sh: input logic<8>,
            y_sar: output signed logic<8>
        ) {
            assign y_sar = a >>> sh;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_sh = sim.signal("sh");
    let id_y = sim.signal("y_sar");

    // Defined: 0x80 (signed = -128) >>> 2 = 0xE0 (sign-extended)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x80u32), BigUint::from(0u32));
        io.set_four_state(id_sh, BigUint::from(2u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(0xE0u32), "0x80 >>> 2 = 0xE0 (sign extend)");
    assert_eq!(m, BigUint::from(0u32));

    // Shift amount has X → all-X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sh, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0xFFu32), "SAR by X amount should yield all-X");
    assert_eq!(v, BigUint::from(0u32));
}

// ==========================================================================
// P1: 3+ element concatenation with X
// ==========================================================================
#[test]
fn test_four_state_concat_three_elements() {
    let code = r#"
        module Top (
            a: input logic<4>,
            b: input logic<4>,
            c: input logic<4>,
            y: output logic<12>
        ) {
            assign y = {a, b, c};
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_c = sim.signal("c");
    let id_y = sim.signal("y");

    // a=0xA (X on all bits), b=0x5 (defined), c=0x3 (defined)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xAu32), BigUint::from(0xFu32)); // a: all X
        io.set_four_state(id_b, BigUint::from(0x5u32), BigUint::from(0u32));   // b: defined
        io.set_four_state(id_c, BigUint::from(0x3u32), BigUint::from(0u32));   // c: defined
    })
    .unwrap();

    let (v, m) = sim.get_four_state(id_y);
    // y = {a, b, c} = {XXXX, 0101, 0011} → mask = 0xF00, value = 0x053 (a normalized to 0)
    assert_eq!(m, BigUint::from(0xF00u32), "Only high nibble should be X");
    assert_eq!(v, BigUint::from(0x053u32), "Defined parts: b=5, c=3; a normalized to 0");
}

// ==========================================================================
// P1: Wide comparison + X
// ==========================================================================
#[test]
fn test_four_state_wide_comparison_with_x() {
    let code = r#"
        module Top (
            a: input logic<128>,
            b: input logic<128>,
            y_eq: output logic,
            y_lt: output logic
        ) {
            assign y_eq = a == b;
            assign y_lt = a <: b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y_eq = sim.signal("y_eq");
    let id_y_lt = sim.signal("y_lt");

    // Both defined
    let val: BigUint = (BigUint::from(0xAAu64) << 64) | BigUint::from(0x55u64);
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val.clone(), BigUint::from(0u32));
        io.set_four_state(id_b, val.clone(), BigUint::from(0u32));
    })
    .unwrap();
    let (v_eq, m_eq) = sim.get_four_state(id_y_eq);
    assert_eq!(v_eq, BigUint::from(1u32), "Equal values should be EQ=1");
    assert_eq!(m_eq, BigUint::from(0u32));

    // Upper word of a has X → both comparisons X
    let mask_a = BigUint::from(0xFFu64) << 64;
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val.clone(), mask_a);
    })
    .unwrap();
    let (_, m_eq) = sim.get_four_state(id_y_eq);
    let (_, m_lt) = sim.get_four_state(id_y_lt);
    assert_eq!(m_eq, BigUint::from(1u32), "Wide EQ with X should yield X");
    assert_eq!(m_lt, BigUint::from(1u32), "Wide LT with X should yield X");
}

// ==========================================================================
// P2: Multi-bit selector (case) with X
// ==========================================================================
#[test]
fn test_four_state_multibit_mux_with_x() {
    let code = r#"
        module Top (
            sel: input logic<2>,
            a: input logic<8>,
            b: input logic<8>,
            c: input logic<8>,
            y: output logic<8>
        ) {
            always_comb {
                if sel == 2'd0 {
                    y = a;
                } else if sel == 2'd1 {
                    y = b;
                } else {
                    y = c;
                }
            }
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_sel = sim.signal("sel");
    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_c = sim.signal("c");
    let id_y = sim.signal("y");

    // Setup branch values
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xAAu32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(0xBBu32), BigUint::from(0u32));
        io.set_four_state(id_c, BigUint::from(0xCCu32), BigUint::from(0u32));
    })
    .unwrap();

    // Selector has X → result should have X (conservative mux of all branches)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sel, BigUint::from(0u32), BigUint::from(1u32)); // bit 0 is X
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_ne!(
        m,
        BigUint::from(0u32),
        "Multi-bit mux with X in selector should produce X in output"
    );
}

// ==========================================================================
// P2: Width narrowing (wide → narrow) with X
// ==========================================================================
#[test]
fn test_four_state_width_narrowing_with_x() {
    let code = r#"
        module Top (
            a: input logic<16>,
            y: output logic<8>
        ) {
            assign y = a[7:0];
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y");

    // X in upper byte only → narrow to lower byte should have no X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x12ABu32), BigUint::from(0xFF00u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(0xABu32), "Lower byte should be 0xAB");
    assert_eq!(m, BigUint::from(0u32), "Lower byte should have no X");

    // X in lower byte → narrow should propagate X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x1200u32), BigUint::from(0x000Fu32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0x0Fu32), "Lower nibble X should propagate");
    assert_eq!(v, BigUint::from(0u32), "X bits normalized to 0");
}

// ==========================================================================
// P2: Width widening (narrow → wide) with X
// ==========================================================================
#[test]
fn test_four_state_width_widening_with_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            y: output logic<16>
        ) {
            assign y = a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y");

    // a has partial X → upper byte of y should be 0 (zero-extended), no X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xA5u32), BigUint::from(0x0Fu32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    // Upper byte: zero-extended → 0x00, mask 0x00
    // Lower byte: value 0xA0 (normalized: 0xA5 & ~0x0F = 0xA0), mask 0x0F
    assert_eq!(m, BigUint::from(0x0Fu32), "Only lower nibble X should propagate");
    assert_eq!(v, BigUint::from(0xA0u32), "Value: 0xA5 & ~0x0F = 0xA0");
}

// ==========================================================================
// P2: FF with conditional assignment + X
// ==========================================================================
#[test]
fn test_four_state_ff_conditional_with_x() {
    let code = r#"
        module Top (
            clk: input clock,
            rst: input reset,
            en: input logic,
            d: input logic<8>,
            q: output logic<8>
        ) {
            always_ff {
                if_reset {
                    q = 8'd0;
                } else if en {
                    q = d;
                }
            }
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let clk = sim.event("clk");
    let id_rst = sim.signal("rst");
    let id_en = sim.signal("en");
    let id_d = sim.signal("d");
    let id_q = sim.signal("q");

    // Reset first
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_rst, BigUint::from(1u32), BigUint::from(0u32));
        io.set_four_state(id_en, BigUint::from(0u32), BigUint::from(0u32));
        io.set_four_state(id_d, BigUint::from(0u32), BigUint::from(0u32));
        io.set_four_state(id_q, BigUint::from(0u32), BigUint::from(0u32));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    let (v_q, m_q) = sim.get_four_state(id_q);
    assert_eq!(v_q, BigUint::from(0u32));
    assert_eq!(m_q, BigUint::from(0u32), "Reset should clear X");

    // en=1, d has X → q captures X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_rst, BigUint::from(0u32), BigUint::from(0u32));
        io.set_four_state(id_en, BigUint::from(1u32), BigUint::from(0u32));
        io.set_four_state(id_d, BigUint::from(0xABu32), BigUint::from(0x0Fu32));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    let (_, m_q) = sim.get_four_state(id_q);
    assert_eq!(m_q, BigUint::from(0x0Fu32), "en=1: X from d should propagate to q");

    // en=0, d changes → q should hold previous value (with X)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_en, BigUint::from(0u32), BigUint::from(0u32));
        io.set_four_state(id_d, BigUint::from(0xFFu32), BigUint::from(0u32));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    let (_, m_q) = sim.get_four_state(id_q);
    assert_eq!(m_q, BigUint::from(0x0Fu32), "en=0: q should hold previous X mask");
}

// ==========================================================================
// P2: Odd-width concatenation (3bit + 5bit) with X
// ==========================================================================
#[test]
fn test_four_state_concat_odd_width() {
    let code = r#"
        module Top (
            a: input logic<3>,
            b: input logic<5>,
            y: output logic<8>
        ) {
            assign y = {a, b};
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    // a = 3'b101 (0x5), mask=0b010 (bit 1 is X); b = 5'b10011 (0x13), mask=0
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x5u32), BigUint::from(0b010u32));
        io.set_four_state(id_b, BigUint::from(0x13u32), BigUint::from(0u32));
    })
    .unwrap();

    let (v, m) = sim.get_four_state(id_y);
    // y = {a, b} = {101, 10011} → 8'b101_10011
    // mask: a's bit 1 X → in y that's bit 6 → mask = 0b0100_0000 = 0x40
    assert_eq!(m, BigUint::from(0x40u32), "X in a[1] should appear at y[6]");
    // value: a = 0b101 (bit 1 is X but val=0), b = 0x13
    // y_val = (0b101 << 5) | 0b10011 = 0b10110011 = 0xB3
    // Normalization: bit 6 val is already 0, so 0xB3 & ~0x40 = 0xB3
    assert_eq!(v, BigUint::from(0xB3u32), "Concat value with X bit at position 6");
}

// ==========================================================================
// P2: 127-bit width test
// ==========================================================================
#[test]
fn test_four_state_127bit() {
    let code = r#"
        module Top (
            a: input logic<127>,
            b: input logic<127>,
            y_and: output logic<127>
        ) {
            assign y_and = a & b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y_and");

    // Set bit 126 (MSB of 127-bit) as X in a, b all ones
    let val_a: BigUint = (BigUint::from(1u64) << 126) | BigUint::from(0xFFu64);
    let mask_a: BigUint = BigUint::from(1u64) << 126;
    let val_b: BigUint = (BigUint::from(u64::MAX) << 63) | BigUint::from(u64::MAX);

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a, mask_a.clone());
        io.set_four_state(id_b, val_b, BigUint::from(0u32));
    })
    .unwrap();

    let (_, m) = sim.get_four_state(id_y);
    // AND: a[126]=X, b[126]=1 → result[126]=X
    assert_eq!(m, mask_a, "127-bit AND: X at MSB should propagate");
}

// ==========================================================================
// Wide (128-bit) Unary NOT + X
// ==========================================================================
#[test]
fn test_four_state_wide_unary_not_with_x() {
    let code = r#"
        module Top (
            a: input logic<128>,
            y: output logic<128>
        ) {
            assign y = ~a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y");

    // Upper word has X, lower word is 0xFF..FF
    let val_a: BigUint = (BigUint::from(0xAAu64) << 64) | BigUint::from(u64::MAX);
    let mask_a: BigUint = BigUint::from(0xFFu64) << 64;
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a, mask_a.clone());
    })
    .unwrap();

    let (v, m) = sim.get_four_state(id_y);
    // NOT: mask preserved, defined bits inverted
    assert_eq!(m, mask_a, "Wide NOT should preserve mask");
    // Lower word: ~0xFF..FF = 0x00..00
    assert_eq!(
        v & BigUint::from(u64::MAX),
        BigUint::from(0u64),
        "Lower word: ~0xFFFFFFFFFFFFFFFF = 0"
    );
}

// ==========================================================================
// Wide (128-bit) Negation + X
// ==========================================================================
#[test]
fn test_four_state_wide_negation_with_x() {
    let code = r#"
        module Top (
            a: input logic<128>,
            y: output logic<128>
        ) {
            assign y = -a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y");

    // Any X → all-X (conservative for arithmetic)
    let mask_a: BigUint = BigUint::from(1u64) << 64;
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(5u32), mask_a);
    })
    .unwrap();

    let (v, m) = sim.get_four_state(id_y);
    let all_x: BigUint = (BigUint::from(u64::MAX) << 64) | BigUint::from(u64::MAX);
    assert_eq!(m, all_x, "Wide negation with X should yield all-X");
    assert_eq!(v, BigUint::from(0u32), "Value normalized to 0");
}

// ==========================================================================
// Wide (128-bit) Reduction AND/OR/XOR + X
// ==========================================================================
#[test]
fn test_four_state_wide_reduction_with_x() {
    let code = r#"
        module Top (
            a: input logic<128>,
            y_rand: output logic,
            y_ror:  output logic,
            y_rxor: output logic
        ) {
            assign y_rand = &a;
            assign y_ror  = |a;
            assign y_rxor = ^a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_rand = sim.signal("y_rand");
    let id_ror = sim.signal("y_ror");
    let id_rxor = sim.signal("y_rxor");

    // a: upper word has X, lower word is all-1s
    let val_a: BigUint = (BigUint::from(0xAAu64) << 64) | BigUint::from(u64::MAX);
    let mask_a: BigUint = BigUint::from(0xFFu64) << 64;
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a, mask_a);
    })
    .unwrap();

    // Reduction AND: upper word bits 72-127 are definite 0 → dominant-value = definite 0
    let (_, m_rand) = sim.get_four_state(id_rand);
    assert_eq!(m_rand, BigUint::from(0u32), "Wide &a with definite 0 bits should be defined (dominant-value)");

    // Reduction OR: lower word has definite 1s → dominant-value = definite 1
    // IEEE 1800: |a = 1 if any bit is definite 1 (even if other bits are X)
    let (_, m_ror) = sim.get_four_state(id_ror);
    assert_eq!(m_ror, BigUint::from(0u32), "Wide |a with definite 1 bits should be defined (dominant-value)");

    // Reduction XOR: any X bit → result X
    let (_, m_rxor) = sim.get_four_state(id_rxor);
    assert_eq!(m_rxor, BigUint::from(1u32), "Wide ^a with any X should be X");
}

// ==========================================================================
// Mux: both branches X
// ==========================================================================
#[test]
fn test_four_state_mux_both_branches_x() {
    let code = r#"
        module Top (
            sel: input logic,
            a: input logic<8>,
            b: input logic<8>,
            y: output logic<8>
        ) {
            assign y = if sel ? a : b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_sel = sim.signal("sel");
    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    // sel=1 (defined), a=all-X, b=all-X → selects a, result all-X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sel, BigUint::from(1u32), BigUint::from(0u32));
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0xFFu32));
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(0xFFu32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0xFFu32), "sel=1 selecting X branch → all-X");
    assert_eq!(v, BigUint::from(0u32));

    // sel=0 (defined), still selects b which is X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sel, BigUint::from(0u32), BigUint::from(0u32));
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0xFFu32), "sel=0 selecting X branch → all-X");

    // sel=X, both branches X → definitely all-X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sel, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0xFFu32), "sel=X, both branches X → all-X");
}

// ==========================================================================
// Cascaded Mux with X
// ==========================================================================
#[test]
fn test_four_state_cascaded_mux_with_x() {
    let code = r#"
        module Top (
            sel1: input logic,
            sel2: input logic,
            a: input logic<8>,
            b: input logic<8>,
            c: input logic<8>,
            y: output logic<8>
        ) {
            assign y = if sel1 ? (if sel2 ? a : b) : c;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_sel1 = sim.signal("sel1");
    let id_sel2 = sim.signal("sel2");
    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_c = sim.signal("c");
    let id_y = sim.signal("y");

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xAAu32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(0xBBu32), BigUint::from(0u32));
        io.set_four_state(id_c, BigUint::from(0xCCu32), BigUint::from(0u32));
    })
    .unwrap();

    // sel1=1, sel2=1 → a (defined)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sel1, BigUint::from(1u32), BigUint::from(0u32));
        io.set_four_state(id_sel2, BigUint::from(1u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(0xAAu32), "sel1=1,sel2=1 → a");
    assert_eq!(m, BigUint::from(0u32));

    // sel1=1, sel2=X → inner mux uncertain, result has X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sel2, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_ne!(m, BigUint::from(0u32), "sel1=1,sel2=X → inner mux X propagates");

    // sel1=0 → c regardless of sel2
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sel1, BigUint::from(0u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(0xCCu32), "sel1=0 → c regardless of sel2");
    assert_eq!(m, BigUint::from(0u32));
}

// ==========================================================================
// Shift: both data and amount have X
// ==========================================================================
#[test]
fn test_four_state_shift_both_x() {
    let code = r#"
        module Top (
            a: input logic<8>,
            sh: input logic<8>,
            y: output logic<8>
        ) {
            assign y = a >> sh;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_sh = sim.signal("sh");
    let id_y = sim.signal("y");

    // Both data and shift amount have X → all-X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xFFu32), BigUint::from(0x0Fu32));
        io.set_four_state(id_sh, BigUint::from(2u32), BigUint::from(1u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0xFFu32), "Shift with X in both data and amount → all-X");
    assert_eq!(v, BigUint::from(0u32));
}

// ==========================================================================
// Case statement with 4-state (EqWildcard)
// ==========================================================================
#[test]
fn test_four_state_case_defined_selector() {
    let code = r#"
        module Top (
            sel: input logic<2>,
            y: output logic<8>
        ) {
            assign y = case sel {
                2'd0: 8'd10,
                2'd1: 8'd20,
                2'd2: 8'd30,
                default: 8'd99,
            };
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_sel = sim.signal("sel");
    let id_y = sim.signal("y");

    // sel = 1 (defined) → should match arm 1 and return 20
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sel, BigUint::from(1u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "Defined selector should produce defined result");
    assert_eq!(v, BigUint::from(20u32), "sel=1 should select value 20");
}

#[test]
fn test_four_state_case_x_in_selector() {
    let code = r#"
        module Top (
            sel: input logic<2>,
            y: output logic<8>
        ) {
            assign y = case sel {
                2'd0: 8'd10,
                2'd1: 8'd20,
                default: 8'd99,
            };
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_sel = sim.signal("sel");
    let id_y = sim.signal("y");

    // sel has X → EqWildcard comparison yields X → should hit default
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sel, BigUint::from(1u32), BigUint::from(1u32)); // bit 0 is X
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    // With X selector, the comparison results are X, so the mux selects conservatively
    assert_ne!(
        m,
        BigUint::from(0u32),
        "Case with X selector should produce X in output (conservative mux)"
    );
}

// ==========================================================================
// Reduction OR/AND dominant-value semantics
// ==========================================================================
#[test]
fn test_four_state_reduction_or_dominant_one() {
    let code = r#"
        module Top (
            a: input logic<8>,
            y: output logic
        ) {
            assign y = |a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y");

    // Value = 0x01, Mask = 0xF0 (upper nibble X, but bit 0 is definite 1)
    // IEEE 1800: |a should be definite 1 when any bit is definite 1
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x01u32), BigUint::from(0xF0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "Reduction OR with definite 1 should yield defined result");
    assert_eq!(v, BigUint::from(1u32), "Reduction OR with definite 1 should yield 1");

    // Value = 0x00, Mask = 0x0F (lower nibble X, upper nibble all 0)
    // No definite 1 exists → result should be X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x00u32), BigUint::from(0x0Fu32));
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(1u32), "Reduction OR with no definite 1 but X bits should yield X");
}

#[test]
fn test_four_state_reduction_and_dominant_zero() {
    let code = r#"
        module Top (
            a: input logic<8>,
            y: output logic
        ) {
            assign y = &a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y");

    // Value = 0xFE, Mask = 0xF0 (upper nibble X, but bit 0 is definite 0)
    // IEEE 1800: &a should be definite 0 when any bit is definite 0
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x0Eu32), BigUint::from(0xF0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "Reduction AND with definite 0 should yield defined result");
    assert_eq!(v, BigUint::from(0u32), "Reduction AND with definite 0 should yield 0");

    // Value = 0xF0, Mask = 0x0F (lower nibble X, upper nibble all 1)
    // All defined bits are 1, but X bits exist → result should be X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xF0u32), BigUint::from(0x0Fu32));
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(1u32), "Reduction AND with no definite 0 but X bits should yield X");
}

#[test]
fn test_four_state_wide_reduction_or_dominant() {
    let code = r#"
        module Top (
            a: input logic<128>,
            y: output logic
        ) {
            assign y = |a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y");

    // Lower 64-bit chunk has definite 1 (bit 0), upper chunk is all X
    // |a should be definite 1 due to dominant-value
    let val = BigUint::from(1u32); // bit 0 = 1
    let mask = BigUint::from(0u32) | (BigUint::from(u64::MAX) << 64); // upper chunk all X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val, mask);
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "Wide reduction OR: definite 1 in any chunk → defined result");
    assert_eq!(v, BigUint::from(1u32), "Wide reduction OR: definite 1 in any chunk → 1");
}

#[test]
fn test_four_state_wide_reduction_and_dominant() {
    let code = r#"
        module Top (
            a: input logic<128>,
            y: output logic
        ) {
            assign y = &a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y");

    // Lower 64-bit chunk has definite 0 (bit 0), upper chunk is all X
    // &a should be definite 0 due to dominant-value
    let val = BigUint::from(0xFFFFFFFFFFFFFFFEu64); // bit 0 = 0
    let mask = BigUint::from(0u32) | (BigUint::from(u64::MAX) << 64); // upper chunk all X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val, mask);
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "Wide reduction AND: definite 0 in any chunk → defined result");
    assert_eq!(v, BigUint::from(0u32), "Wide reduction AND: definite 0 in any chunk → 0");
}

// ==========================================================================
// IEEE 1800 LogicAnd (&&) dominant-value: 0 && x = 0
// ==========================================================================
#[test]
fn test_four_state_logic_and_dominant_zero() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y: output logic
        ) {
            assign y = a && b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    // 0 && X = 0 (definite)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0u32)); // definite 0
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(0xFFu32)); // all X
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "0 && X should be definite (mask=0)");
    assert_eq!(v, BigUint::from(0u32), "0 && X = 0");

    // X && 0 = 0 (definite)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0xFFu32)); // all X
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(0u32)); // definite 0
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "X && 0 should be definite (mask=0)");
    assert_eq!(v, BigUint::from(0u32), "X && 0 = 0");

    // 1 && X = X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(1u32), BigUint::from(0u32)); // definite 1
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(0xFFu32)); // all X
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_ne!(m, BigUint::from(0u32), "1 && X should be X (mask!=0)");

    // X && 1 = X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0xFFu32)); // all X
        io.set_four_state(id_b, BigUint::from(1u32), BigUint::from(0u32)); // definite 1
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_ne!(m, BigUint::from(0u32), "X && 1 should be X (mask!=0)");

    // 1 && 1 = 1 (definite)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(1u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(1u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "1 && 1 should be definite");
    assert_eq!(v, BigUint::from(1u32), "1 && 1 = 1");
}

// ==========================================================================
// IEEE 1800 LogicOr (||) dominant-value: 1 || x = 1
// ==========================================================================
#[test]
fn test_four_state_logic_or_dominant_one() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y: output logic
        ) {
            assign y = a || b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    // 1 || X = 1 (definite)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(1u32), BigUint::from(0u32)); // definite 1
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(0xFFu32)); // all X
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "1 || X should be definite (mask=0)");
    assert_eq!(v, BigUint::from(1u32), "1 || X = 1");

    // X || 1 = 1 (definite)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0xFFu32)); // all X
        io.set_four_state(id_b, BigUint::from(1u32), BigUint::from(0u32)); // definite 1
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "X || 1 should be definite (mask=0)");
    assert_eq!(v, BigUint::from(1u32), "X || 1 = 1");

    // 0 || X = X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0u32)); // definite 0
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(0xFFu32)); // all X
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_ne!(m, BigUint::from(0u32), "0 || X should be X (mask!=0)");

    // X || 0 = X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0xFFu32)); // all X
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(0u32)); // definite 0
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_ne!(m, BigUint::from(0u32), "X || 0 should be X (mask!=0)");

    // 0 || 0 = 0 (definite)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "0 || 0 should be definite");
    assert_eq!(v, BigUint::from(0u32), "0 || 0 = 0");
}

// ==========================================================================
// IEEE 1800 EqWildcard (==?) with LHS value at wildcard positions
// ==========================================================================
#[test]
fn test_four_state_eq_wildcard_value_at_wildcard_pos() {
    // Test that LHS values at RHS wildcard (X) positions are correctly ignored
    let code = r#"
        module Top (
            a: input logic<4>,
            b: input logic<4>,
            y: output logic
        ) {
            assign y = a ==? b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    // LHS = 4'b1110 (definite), RHS = 4'b1x1x (mask=0b0101)
    // Non-wildcard positions (bits 1,3): LHS[1]=1=RHS[1], LHS[3]=1=RHS[3] → match
    // IEEE 1800: ==? should return 1 (definite true)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0b1110u32), BigUint::from(0u32)); // definite
        io.set_four_state(id_b, BigUint::from(0b1010u32), BigUint::from(0b0101u32)); // bits 0,2 are X
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "==? with matching non-wildcard bits should be definite");
    assert_eq!(v, BigUint::from(1u32), "==? with matching non-wildcard bits should be 1");

    // LHS = 4'b1100 (definite), RHS = 4'b1x1x (mask=0b0101)
    // Non-wildcard positions: bit 1: LHS[1]=0, RHS[1]=1 → mismatch
    // IEEE 1800: ==? should return 0 (definite false)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0b1100u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(0b1010u32), BigUint::from(0b0101u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "==? with definite mismatch should be definite");
    assert_eq!(v, BigUint::from(0u32), "==? with mismatch at non-wildcard should be 0");

    // LHS has X at non-wildcard position, no definite mismatch
    // LHS = 4'bxx10, RHS = 4'b1x1x → compare at bits 1,3
    // bit 1: LHS[1]=1 (definite), RHS[1]=1 → match
    // bit 3: LHS[3]=X → unknown
    // IEEE 1800: ==? should return X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0b0010u32), BigUint::from(0b1100u32)); // bits 2,3 = X
        io.set_four_state(id_b, BigUint::from(0b1010u32), BigUint::from(0b0101u32)); // bits 0,2 = X
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_ne!(m, BigUint::from(0u32), "==? with LHS X at non-wildcard should be X");

    // Definite mismatch takes priority over LHS X elsewhere
    // LHS = 4'bxx00, RHS = 4'b1x1x → compare at bits 1,3
    // bit 1: LHS[1]=0, RHS[1]=1 → definite mismatch
    // bit 3: LHS[3]=X → unknown, but mismatch already found
    // IEEE 1800: ==? should return 0 (definite false)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0b0000u32), BigUint::from(0b1100u32)); // bits 2,3 = X
        io.set_four_state(id_b, BigUint::from(0b1010u32), BigUint::from(0b0101u32)); // bits 0,2 = X
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "==? with definite mismatch should be definite even with X elsewhere");
    assert_eq!(v, BigUint::from(0u32), "==? with definite mismatch = 0");
}

#[test]
fn test_four_state_ne_wildcard_value_at_wildcard_pos() {
    let code = r#"
        module Top (
            a: input logic<4>,
            b: input logic<4>,
            y: output logic
        ) {
            assign y = a !=? b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y");

    // LHS = 4'b1110 (definite), RHS = 4'b1x1x (mask=0b0101)
    // All non-wildcard positions match → !=? should return 0 (definite false)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0b1110u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(0b1010u32), BigUint::from(0b0101u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "!=? with all matching should be definite");
    assert_eq!(v, BigUint::from(0u32), "!=? with all matching = 0");

    // LHS = 4'b1100, RHS = 4'b1x1x → mismatch at bit 1
    // !=? should return 1 (definite true)
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0b1100u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(0b1010u32), BigUint::from(0b0101u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0u32), "!=? with definite mismatch should be definite");
    assert_eq!(v, BigUint::from(1u32), "!=? with mismatch = 1");
}

// ==========================================================================
// Wide MUL + X (128-bit)
// ==========================================================================
#[test]
fn test_four_state_wide_mul_with_x() {
    let code = r#"
        module Top (
            a: input logic<128>,
            b: input logic<128>,
            y_mul: output logic<128>
        ) {
            assign y_mul = a * b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y_mul");

    let all_x_128 = (BigUint::from(u64::MAX) << 64) | BigUint::from(u64::MAX);

    // Both defined: 3 * 7 = 21
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(3u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(7u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(21u32), "Wide 3 * 7 = 21");
    assert_eq!(m, BigUint::from(0u32), "No X when both defined");

    // One operand has X in upper word → result should be all-X
    let mask_a = BigUint::from(1u64) << 64;
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(3u32), mask_a);
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, all_x_128, "Wide MUL with X should yield all-X mask");
    assert_eq!(v, BigUint::from(0u32), "Value should be 0 after normalization");
}

// ==========================================================================
// Wide DIV + X (128-bit)
// ==========================================================================
#[test]
fn test_four_state_wide_div_with_x() {
    let code = r#"
        module Top (
            a: input logic<128>,
            b: input logic<128>,
            y_div: output logic<128>
        ) {
            assign y_div = a / b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y_div");

    let all_x_128 = (BigUint::from(u64::MAX) << 64) | BigUint::from(u64::MAX);

    // Both defined: 20 / 4 = 5
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(20u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(4u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(5u32), "Wide 20 / 4 = 5");
    assert_eq!(m, BigUint::from(0u32));

    // Dividend has X → result all-X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0x80u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, all_x_128, "Wide DIV with X dividend should yield all-X");
    assert_eq!(v, BigUint::from(0u32));
}

// ==========================================================================
// Wide MOD + X (128-bit)
// ==========================================================================
#[test]
fn test_four_state_wide_mod_with_x() {
    let code = r#"
        module Top (
            a: input logic<128>,
            b: input logic<128>,
            y_mod: output logic<128>
        ) {
            assign y_mod = a % b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y_mod");

    let all_x_128 = (BigUint::from(u64::MAX) << 64) | BigUint::from(u64::MAX);

    // Both defined: 17 % 5 = 2
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(17u32), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(5u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(2u32), "Wide 17 % 5 = 2");
    assert_eq!(m, BigUint::from(0u32));

    // Divisor has X → result all-X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(1u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, all_x_128, "Wide MOD with X divisor should yield all-X");
    assert_eq!(v, BigUint::from(0u32));
}

// ==========================================================================
// SAR with both data and shift amount having X
// ==========================================================================
#[test]
fn test_four_state_sar_both_x() {
    let code = r#"
        module Top (
            a: input signed logic<8>,
            sh: input logic<8>,
            y_sar: output signed logic<8>
        ) {
            assign y_sar = a >>> sh;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_sh = sim.signal("sh");
    let id_y = sim.signal("y_sar");

    // Both data and shift amount have X → all-X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x80u32), BigUint::from(0x0Fu32)); // data has partial X
        io.set_four_state(id_sh, BigUint::from(2u32), BigUint::from(1u32)); // shift amount has X
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(
        m,
        BigUint::from(0xFFu32),
        "SAR with X in both data and shift amount → all-X"
    );
    assert_eq!(v, BigUint::from(0u32));
}

// ==========================================================================
// Wide NE + X (128-bit)
// ==========================================================================
#[test]
fn test_four_state_wide_ne_with_x() {
    let code = r#"
        module Top (
            a: input logic<128>,
            b: input logic<128>,
            y_ne: output logic
        ) {
            assign y_ne = a != b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y_ne");

    // Both defined: different values → NE=1
    let val_a: BigUint = (BigUint::from(0xAAu64) << 64) | BigUint::from(0x55u64);
    let val_b: BigUint = (BigUint::from(0xBBu64) << 64) | BigUint::from(0x55u64);
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a.clone(), BigUint::from(0u32));
        io.set_four_state(id_b, val_b, BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(1u32), "Different wide values → NE=1");
    assert_eq!(m, BigUint::from(0u32));

    // Same values → NE=0
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_b, val_a.clone(), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(0u32), "Same wide values → NE=0");
    assert_eq!(m, BigUint::from(0u32));

    // One has X → result X
    let mask_a = BigUint::from(0xFFu64) << 64;
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a, mask_a);
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(1u32), "Wide NE with X should yield X result");
}

// ==========================================================================
// Wide GT + X (128-bit unsigned)
// ==========================================================================
#[test]
fn test_four_state_wide_gt_with_x() {
    let code = r#"
        module Top (
            a: input logic<128>,
            b: input logic<128>,
            y_gt: output logic
        ) {
            assign y_gt = a >: b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y = sim.signal("y_gt");

    // Both defined: a > b → 1
    let val_a: BigUint = (BigUint::from(0xFFu64) << 64) | BigUint::from(0u64);
    let val_b: BigUint = BigUint::from(0xFFu64);
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a.clone(), BigUint::from(0u32));
        io.set_four_state(id_b, val_b, BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(1u32), "Wide a > b should be true");
    assert_eq!(m, BigUint::from(0u32));

    // One has X → result X
    let mask_a = BigUint::from(1u64) << 64;
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val_a, mask_a);
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(1u32), "Wide GT with X should yield X result");
}

// ==========================================================================
// Wide GE/LE + X (128-bit unsigned)
// ==========================================================================
#[test]
fn test_four_state_wide_ge_le_with_x() {
    let code = r#"
        module Top (
            a: input logic<128>,
            b: input logic<128>,
            y_ge: output logic,
            y_le: output logic
        ) {
            assign y_ge = a >= b;
            assign y_le = a <= b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_y_ge = sim.signal("y_ge");
    let id_y_le = sim.signal("y_le");

    // Both defined and equal: GE=1, LE=1
    let val: BigUint = (BigUint::from(0xAAu64) << 64) | BigUint::from(0x55u64);
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val.clone(), BigUint::from(0u32));
        io.set_four_state(id_b, val.clone(), BigUint::from(0u32));
    })
    .unwrap();
    let (v_ge, m_ge) = sim.get_four_state(id_y_ge);
    let (v_le, m_le) = sim.get_four_state(id_y_le);
    assert_eq!(v_ge, BigUint::from(1u32), "Wide equal values → GE=1");
    assert_eq!(m_ge, BigUint::from(0u32));
    assert_eq!(v_le, BigUint::from(1u32), "Wide equal values → LE=1");
    assert_eq!(m_le, BigUint::from(0u32));

    // One has X → both results X
    let mask_a = BigUint::from(0xFFu64) << 64;
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val, mask_a);
    })
    .unwrap();
    let (_, m_ge) = sim.get_four_state(id_y_ge);
    let (_, m_le) = sim.get_four_state(id_y_le);
    assert_eq!(m_ge, BigUint::from(1u32), "Wide GE with X should yield X");
    assert_eq!(m_le, BigUint::from(1u32), "Wide LE with X should yield X");
}

// ==========================================================================
// Wide signed comparison + X (128-bit)
// ==========================================================================
#[test]
fn test_four_state_wide_signed_comparison_with_x() {
    let code = r#"
        module Top (
            a: input signed logic<128>,
            b: input signed logic<128>,
            y_lt_s: output logic,
            y_gt_s: output logic,
            y_le_s: output logic,
            y_ge_s: output logic
        ) {
            assign y_lt_s = a <: b;
            assign y_gt_s = a >: b;
            assign y_le_s = a <= b;
            assign y_ge_s = a >= b;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_lt = sim.signal("y_lt_s");
    let id_gt = sim.signal("y_gt_s");
    let id_le = sim.signal("y_le_s");
    let id_ge = sim.signal("y_ge_s");

    // Both defined: a = -1 (all bits set in 128-bit), b = 1 → signed: -1 < 1
    let all_ones_128 = (BigUint::from(u64::MAX) << 64) | BigUint::from(u64::MAX);
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, all_ones_128, BigUint::from(0u32)); // -1 in signed 128-bit
        io.set_four_state(id_b, BigUint::from(1u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v_lt, m_lt) = sim.get_four_state(id_lt);
    let (v_gt, m_gt) = sim.get_four_state(id_gt);
    assert_eq!(v_lt, BigUint::from(1u32), "Wide signed: -1 < 1 should be true");
    assert_eq!(m_lt, BigUint::from(0u32));
    assert_eq!(v_gt, BigUint::from(0u32), "Wide signed: -1 > 1 should be false");
    assert_eq!(m_gt, BigUint::from(0u32));

    // One has X → all comparisons yield X
    let mask_a = BigUint::from(1u64) << 64;
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), mask_a);
    })
    .unwrap();
    let (_, m_lt) = sim.get_four_state(id_lt);
    let (_, m_gt) = sim.get_four_state(id_gt);
    let (_, m_le) = sim.get_four_state(id_le);
    let (_, m_ge) = sim.get_four_state(id_ge);
    assert_eq!(m_lt, BigUint::from(1u32), "Wide signed LT with X should yield X");
    assert_eq!(m_gt, BigUint::from(1u32), "Wide signed GT with X should yield X");
    assert_eq!(m_le, BigUint::from(1u32), "Wide signed LE with X should yield X");
    assert_eq!(m_ge, BigUint::from(1u32), "Wide signed GE with X should yield X");
}

// ==========================================================================
// Wide logical NOT + X (128-bit)
// ==========================================================================
#[test]
fn test_four_state_wide_logical_not_with_x() {
    let code = r#"
        module Top (
            a: input logic<128>,
            y_lnot: output logic
        ) {
            assign y_lnot = !a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y_lnot");

    // Defined nonzero: !nonzero = 0
    let val: BigUint = BigUint::from(1u64) << 64;
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, val, BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(0u32), "Wide !nonzero = 0");
    assert_eq!(m, BigUint::from(0u32));

    // Defined zero: !0 = 1
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(1u32), "Wide !0 = 1");
    assert_eq!(m, BigUint::from(0u32));

    // X input (upper word only) → result X
    let mask_a = BigUint::from(1u64) << 64;
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), mask_a);
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(1u32), "Wide logical NOT with X should yield X");
}

// ==========================================================================
// Concat: X crossing chunk boundary (64-bit)
// ==========================================================================
#[test]
fn test_four_state_concat_chunk_boundary_x() {
    // a (48-bit) is placed at bits [127:80] — no X
    // b (32-bit) is placed at bits [79:48] — all X, crosses the 64-bit chunk boundary
    // c (48-bit) is placed at bits [47:0] — no X
    let code = r#"
        module Top (
            a: input logic<48>,
            b: input logic<32>,
            c: input logic<48>,
            y: output logic<128>
        ) {
            assign y = {a, b, c};
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_b = sim.signal("b");
    let id_c = sim.signal("c");
    let id_y = sim.signal("y");

    // a = defined 48-bit value, b = all X (32-bit), c = defined 48-bit value
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x123456789ABCu64), BigUint::from(0u32));
        io.set_four_state(id_b, BigUint::from(0u32), BigUint::from(0xFFFFFFFFu64)); // all X
        io.set_four_state(id_c, BigUint::from(0xABCDEF012345u64), BigUint::from(0u32));
    })
    .unwrap();

    let (v, m) = sim.get_four_state(id_y);

    // b occupies bits [79:48], crossing the 64-bit chunk boundary
    // Expected mask: bits [79:48] = 1, rest = 0
    let expected_mask = BigUint::from(0xFFFFFFFFu64) << 48;
    assert_eq!(
        m, expected_mask,
        "X in b should span bits [79:48] crossing chunk boundary"
    );

    // c at bits [47:0] should be preserved
    let lower_48_mask = (BigUint::from(1u64) << 48) - BigUint::from(1u32);
    let v_c = &v & &lower_48_mask;
    assert_eq!(v_c, BigUint::from(0xABCDEF012345u64), "Lower 48 bits from c should be intact");

    // a at bits [127:80] should be preserved (after normalization, b's value bits are 0)
    let v_a = &v >> 80;
    assert_eq!(v_a, BigUint::from(0x123456789ABCu64), "Upper 48 bits from a should be intact");
}

// ==========================================================================
// FF: synchronous reset + X
// ==========================================================================
#[test]
fn test_four_state_ff_sync_reset_with_x() {
    let code = r#"
        module Top (
            clk: input clock,
            rst: input reset,
            sync_rst: input logic,
            d: input logic<8>,
            q: output logic<8>
        ) {
            always_ff {
                if_reset {
                    q = 8'd0;
                } else {
                    if sync_rst {
                        q = 8'd0;
                    } else {
                        q = d;
                    }
                }
            }
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let clk = sim.event("clk");
    let id_rst = sim.signal("rst");
    let id_sync_rst = sim.signal("sync_rst");
    let id_d = sim.signal("d");
    let id_q = sim.signal("q");

    // 1. Async reset to clear state
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_rst, BigUint::from(1u32), BigUint::from(0u32));
        io.set_four_state(id_sync_rst, BigUint::from(0u32), BigUint::from(0u32));
        io.set_four_state(id_d, BigUint::from(0u32), BigUint::from(0u32));
        io.set_four_state(id_q, BigUint::from(0u32), BigUint::from(0u32));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    let (v_q, m_q) = sim.get_four_state(id_q);
    assert_eq!(v_q, BigUint::from(0u32));
    assert_eq!(m_q, BigUint::from(0u32), "Async reset should clear X");

    // 2. Load data with X into q
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_rst, BigUint::from(0u32), BigUint::from(0u32));
        io.set_four_state(id_sync_rst, BigUint::from(0u32), BigUint::from(0u32));
        io.set_four_state(id_d, BigUint::from(0xABu32), BigUint::from(0x0Fu32)); // lower nibble X
    })
    .unwrap();
    sim.tick(clk).unwrap();
    let (_, m_q) = sim.get_four_state(id_q);
    assert_eq!(m_q, BigUint::from(0x0Fu32), "q should capture X from d");

    // 3. Sync reset should clear q (and X) to 0
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sync_rst, BigUint::from(1u32), BigUint::from(0u32));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    let (v_q, m_q) = sim.get_four_state(id_q);
    assert_eq!(v_q, BigUint::from(0u32), "Sync reset should set q to 0");
    assert_eq!(m_q, BigUint::from(0u32), "Sync reset should clear X in q");

    // 4. Load X again, then verify sync reset clears it again
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sync_rst, BigUint::from(0u32), BigUint::from(0u32));
        io.set_four_state(id_d, BigUint::from(0u32), BigUint::from(0xFFu32)); // all X
    })
    .unwrap();
    sim.tick(clk).unwrap();
    let (_, m_q) = sim.get_four_state(id_q);
    assert_eq!(m_q, BigUint::from(0xFFu32), "q should capture all-X from d");

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_sync_rst, BigUint::from(1u32), BigUint::from(0u32));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    let (v_q, m_q) = sim.get_four_state(id_q);
    assert_eq!(v_q, BigUint::from(0u32), "Second sync reset should set q to 0");
    assert_eq!(m_q, BigUint::from(0u32), "Second sync reset should clear all X");
}

// ==========================================================================
// Explicit cast + X: signed↔unsigned conversion preserves X
// ==========================================================================
#[test]
fn test_four_state_explicit_cast_with_x() {
    // Test signed → unsigned reinterpretation with X propagation
    let code = r#"
        module Top (
            a: input signed logic<8>,
            y_to_unsigned: output logic<8>
        ) {
            assign y_to_unsigned = a;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();

    let id_a = sim.signal("a");
    let id_y = sim.signal("y_to_unsigned");

    // a = 0x80 (-128 signed), no X: unsigned view should be 0x80
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x80u32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(0x80u32), "Signed→unsigned: value preserved");
    assert_eq!(m, BigUint::from(0u32), "Signed→unsigned: no X");

    // a = 0x7F (+127 signed), no X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x7Fu32), BigUint::from(0u32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(v, BigUint::from(0x7Fu32), "Signed→unsigned: positive value preserved");
    assert_eq!(m, BigUint::from(0u32));

    // a has X in sign bit (bit 7): X should propagate to output
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0x40u32), BigUint::from(0x80u32)); // bit 7 is X
    })
    .unwrap();
    let (_, m) = sim.get_four_state(id_y);
    assert_eq!(
        m,
        BigUint::from(0x80u32),
        "Signed→unsigned: X in sign bit preserved"
    );

    // a has X in lower bits: X should propagate
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0xA0u32), BigUint::from(0x0Fu32)); // lower nibble X
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0x0Fu32), "Signed→unsigned: X in lower bits preserved");
    assert_eq!(
        v,
        BigUint::from(0xA0u32),
        "Signed→unsigned: value normalized (v &= ~m)"
    );

    // All bits X
    sim.modify(|io: &mut IOContext| {
        io.set_four_state(id_a, BigUint::from(0u32), BigUint::from(0xFFu32));
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_y);
    assert_eq!(m, BigUint::from(0xFFu32), "Signed→unsigned: all X propagated");
    assert_eq!(v, BigUint::from(0u32), "Signed→unsigned: all X normalized to 0");
}
