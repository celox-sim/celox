use test_case::test_case;
use celox::{BigUint, IOContext, Simulator};

// ---------------------------------------------------------------------------
// Helper: combinational binary operator  (assign o = a {op} b)
// ---------------------------------------------------------------------------
fn check_comb_binary(op: &str, in_type: &str, out_type: &str, a: u64, b: u64, expected: u64) {
    let code = format!(
        r#"
        module Top (a: input {in_type}, b: input {in_type}, o: output {out_type}) {{
            assign o = a {op} b;
        }}
    "#
    );
    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let sig_a = sim.signal("a");
    let sig_b = sim.signal("b");
    let sig_o = sim.signal("o");

    sim.modify(|io| {
        io.set_wide(sig_a, BigUint::from(a));
        io.set_wide(sig_b, BigUint::from(b));
    })
    .unwrap();

    assert_eq!(
        sim.get(sig_o),
        BigUint::from(expected),
        "comb {op}: {a} {op} {b} — expected {expected}"
    );
}

// ---------------------------------------------------------------------------
// Helper: ff binary operator  (always_ff { r = a {op} b; })
// ---------------------------------------------------------------------------
fn check_ff_binary(op: &str, in_type: &str, out_type: &str, a: u64, b: u64, expected: u64) {
    let code = format!(
        r#"
        module Top (clk: input clock, a: input {in_type}, b: input {in_type}, o: output {out_type}) {{
            var r: {out_type};
            always_ff (clk) {{
                r = a {op} b;
            }}
            assign o = r;
        }}
    "#
    );
    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let sig_a = sim.signal("a");
    let sig_b = sim.signal("b");
    let sig_o = sim.signal("o");

    sim.modify(|io| {
        io.set_wide(sig_a, BigUint::from(a));
        io.set_wide(sig_b, BigUint::from(b));
    })
    .unwrap();
    sim.tick(clk).unwrap();

    assert_eq!(
        sim.get(sig_o),
        BigUint::from(expected),
        "ff {op}: {a} {op} {b} — expected {expected}"
    );
}

// ---------------------------------------------------------------------------
// Helper: combinational unary operator  (assign o = {op}a)
// ---------------------------------------------------------------------------
fn check_comb_unary(op: &str, in_type: &str, out_type: &str, a: u64, expected: u64) {
    let code = format!(
        r#"
        module Top (a: input {in_type}, o: output {out_type}) {{
            assign o = {op}a;
        }}
    "#
    );
    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let sig_a = sim.signal("a");
    let sig_o = sim.signal("o");

    sim.modify(|io| io.set_wide(sig_a, BigUint::from(a)))
        .unwrap();

    assert_eq!(
        sim.get(sig_o),
        BigUint::from(expected),
        "comb unary {op}: {op}{a} — expected {expected}"
    );
}

// ---------------------------------------------------------------------------
// Helper: ff unary operator  (always_ff { r = {op}a; })
// ---------------------------------------------------------------------------
fn check_ff_unary(op: &str, in_type: &str, out_type: &str, a: u64, expected: u64) {
    let code = format!(
        r#"
        module Top (clk: input clock, a: input {in_type}, o: output {out_type}) {{
            var r: {out_type};
            always_ff (clk) {{
                r = {op}a;
            }}
            assign o = r;
        }}
    "#
    );
    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let sig_a = sim.signal("a");
    let sig_o = sim.signal("o");

    sim.modify(|io| io.set_wide(sig_a, BigUint::from(a)))
        .unwrap();
    sim.tick(clk).unwrap();

    assert_eq!(
        sim.get(sig_o),
        BigUint::from(expected),
        "ff unary {op}: {op}{a} — expected {expected}"
    );
}

// ===================================================================
// Arithmetic (unsigned) — comb
// ===================================================================

#[test_case("+",  "logic<8>", "logic<8>", 100, 55, 155  ; "add basic")]
#[test_case("+",  "logic<8>", "logic<8>", 200, 100, 44  ; "add overflow wraps")]
#[test_case("+",  "logic<8>", "logic<8>", 0, 0, 0       ; "add zeros")]
#[test_case("-",  "logic<8>", "logic<8>", 200, 55, 145  ; "sub basic")]
#[test_case("-",  "logic<8>", "logic<8>", 5, 10, 251    ; "sub underflow wraps")]
#[test_case("-",  "logic<8>", "logic<8>", 0, 0, 0       ; "sub zeros")]
#[test_case("*",  "logic<8>", "logic<8>", 7, 6, 42      ; "mul basic")]
#[test_case("*",  "logic<8>", "logic<8>", 16, 16, 0     ; "mul overflow wraps")]
#[test_case("*",  "logic<8>", "logic<8>", 255, 1, 255   ; "mul identity")]
#[test_case("/",  "logic<16>", "logic<16>", 100, 7, 14  ; "div basic")]
#[test_case("/",  "logic<16>", "logic<16>", 255, 16, 15 ; "div truncates")]
#[test_case("/",  "logic<16>", "logic<16>", 0, 5, 0     ; "div zero dividend")]
#[test_case("%",  "logic<16>", "logic<16>", 100, 7, 2   ; "rem basic")]
#[test_case("%",  "logic<16>", "logic<16>", 255, 16, 15 ; "rem basic 2")]
#[test_case("%",  "logic<16>", "logic<16>", 42, 5, 2    ; "rem small")]
fn comb_arith_unsigned(op: &str, in_ty: &str, out_ty: &str, a: u64, b: u64, exp: u64) {
    check_comb_binary(op, in_ty, out_ty, a, b, exp);
}

// ===================================================================
// Arithmetic (unsigned) — ff (representative subset)
// ===================================================================

#[test_case("+",  "logic<8>", "logic<8>", 100, 55, 155  ; "ff add")]
#[test_case("-",  "logic<8>", "logic<8>", 200, 55, 145  ; "ff sub")]
#[test_case("*",  "logic<8>", "logic<8>", 7, 6, 42      ; "ff mul")]
#[test_case("/",  "logic<16>", "logic<16>", 100, 7, 14  ; "ff div")]
#[test_case("%",  "logic<16>", "logic<16>", 100, 7, 2   ; "ff rem")]
fn ff_arith_unsigned(op: &str, in_ty: &str, out_ty: &str, a: u64, b: u64, exp: u64) {
    check_ff_binary(op, in_ty, out_ty, a, b, exp);
}

// ===================================================================
// Bitwise — comb
// ===================================================================

#[test_case("&",  "logic<8>", "logic<8>", 0xA5, 0x5A, 0x00  ; "and complementary")]
#[test_case("&",  "logic<8>", "logic<8>", 0xFF, 0xA5, 0xA5  ; "and with all ones")]
#[test_case("|",  "logic<8>", "logic<8>", 0xA5, 0x5A, 0xFF  ; "or complementary")]
#[test_case("|",  "logic<8>", "logic<8>", 0x00, 0xA5, 0xA5  ; "or with zero")]
#[test_case("^",  "logic<8>", "logic<8>", 0xA5, 0x5A, 0xFF  ; "xor complementary")]
#[test_case("^",  "logic<8>", "logic<8>", 0xFF, 0xFF, 0x00  ; "xor same cancels")]
#[test_case("~^", "logic<8>", "logic<8>", 0xF0, 0xFF, 0xF0  ; "xnor basic")]
#[test_case("~^", "logic<8>", "logic<8>", 0xAA, 0x55, 0x00  ; "xnor complementary")]
fn comb_bitwise(op: &str, in_ty: &str, out_ty: &str, a: u64, b: u64, exp: u64) {
    check_comb_binary(op, in_ty, out_ty, a, b, exp);
}

// ===================================================================
// Bitwise — ff (representative)
// ===================================================================

#[test_case("&",  "logic<8>", "logic<8>", 0xFF, 0xA5, 0xA5  ; "ff and")]
#[test_case("|",  "logic<8>", "logic<8>", 0xA5, 0x5A, 0xFF  ; "ff or")]
#[test_case("^",  "logic<8>", "logic<8>", 0xA5, 0x5A, 0xFF  ; "ff xor")]
#[test_case("~^", "logic<8>", "logic<8>", 0xF0, 0xFF, 0xF0  ; "ff xnor")]
fn ff_bitwise(op: &str, in_ty: &str, out_ty: &str, a: u64, b: u64, exp: u64) {
    check_ff_binary(op, in_ty, out_ty, a, b, exp);
}

// ===================================================================
// Shift — comb (unsigned)
// ===================================================================

#[test_case("<<",  "logic<8>", "logic<8>", 0x01, 4, 0x10   ; "shl basic")]
#[test_case("<<",  "logic<8>", "logic<8>", 0x80, 1, 0x00   ; "shl overflow")]
#[test_case(">>",  "logic<8>", "logic<8>", 0x80, 2, 0x20   ; "shr basic")]
#[test_case(">>",  "logic<8>", "logic<8>", 0x01, 1, 0x00   ; "shr underflow")]
#[test_case(">>>", "logic<8>", "logic<8>", 0x80, 2, 0x20   ; "sar unsigned same as shr")]
fn comb_shift_unsigned(op: &str, in_ty: &str, out_ty: &str, a: u64, b: u64, exp: u64) {
    check_comb_binary(op, in_ty, out_ty, a, b, exp);
}

// ===================================================================
// Shift — comb (signed arithmetic right shift)
// ===================================================================

#[test_case(">>>", "i8", "i8", 0x80, 2, 0xE0 ; "sar negative sign extends")]
#[test_case(">>>", "i8", "i8", 0x40, 2, 0x10 ; "sar positive no extend")]
#[test_case(">>>", "i8", "i8", 0xFF, 4, 0xFF ; "sar all ones stays")]
fn comb_shift_signed(op: &str, in_ty: &str, out_ty: &str, a: u64, b: u64, exp: u64) {
    check_comb_binary(op, in_ty, out_ty, a, b, exp);
}

// ===================================================================
// Shift — ff (representative)
// ===================================================================

#[test_case("<<",  "logic<8>", "logic<8>", 0x01, 4, 0x10   ; "ff shl")]
#[test_case(">>",  "logic<8>", "logic<8>", 0x80, 2, 0x20   ; "ff shr")]
#[test_case(">>>", "i8", "i8", 0x80, 2, 0xE0               ; "ff sar signed")]
fn ff_shift(op: &str, in_ty: &str, out_ty: &str, a: u64, b: u64, exp: u64) {
    check_ff_binary(op, in_ty, out_ty, a, b, exp);
}

// ===================================================================
// Comparison (unsigned) — comb
// ===================================================================

#[test_case("<:",  "logic<8>", "logic", 10, 20, 1  ; "lt true")]
#[test_case("<:",  "logic<8>", "logic", 20, 10, 0  ; "lt false")]
#[test_case("<:",  "logic<8>", "logic", 10, 10, 0  ; "lt equal")]
#[test_case("<=",  "logic<8>", "logic", 10, 20, 1  ; "le true")]
#[test_case("<=",  "logic<8>", "logic", 10, 10, 1  ; "le equal")]
#[test_case("<=",  "logic<8>", "logic", 20, 10, 0  ; "le false")]
#[test_case(">:",  "logic<8>", "logic", 20, 10, 1  ; "gt true")]
#[test_case(">:",  "logic<8>", "logic", 10, 20, 0  ; "gt false")]
#[test_case(">:",  "logic<8>", "logic", 10, 10, 0  ; "gt equal")]
#[test_case(">=",  "logic<8>", "logic", 20, 10, 1  ; "ge true")]
#[test_case(">=",  "logic<8>", "logic", 10, 10, 1  ; "ge equal")]
#[test_case("==",  "logic<8>", "logic", 42, 42, 1  ; "eq true")]
#[test_case("==",  "logic<8>", "logic", 42, 43, 0  ; "eq false")]
#[test_case("!=",  "logic<8>", "logic", 42, 43, 1  ; "ne true")]
#[test_case("!=",  "logic<8>", "logic", 42, 42, 0  ; "ne false")]
fn comb_compare_unsigned(op: &str, in_ty: &str, out_ty: &str, a: u64, b: u64, exp: u64) {
    check_comb_binary(op, in_ty, out_ty, a, b, exp);
}

// ===================================================================
// Comparison (signed) — comb
// ===================================================================

#[test_case("<:",  "i8", "logic", 0xFB, 0x02, 1  ; "signed lt neg vs pos")]
#[test_case("<:",  "i8", "logic", 0x02, 0xFB, 0  ; "signed lt pos vs neg")]
#[test_case(">:",  "i8", "logic", 0x02, 0xFB, 1  ; "signed gt pos vs neg")]
#[test_case(">=",  "i8", "logic", 0xFB, 0xFB, 1  ; "signed ge equal neg")]
#[test_case("==",  "i8", "logic", 0xFF, 0xFF, 1  ; "signed eq neg ones")]
#[test_case("!=",  "i8", "logic", 0xFF, 0x01, 1  ; "signed ne neg vs pos")]
fn comb_compare_signed(op: &str, in_ty: &str, out_ty: &str, a: u64, b: u64, exp: u64) {
    check_comb_binary(op, in_ty, out_ty, a, b, exp);
}

// ===================================================================
// Logical — comb
// ===================================================================

#[test_case("&&", "logic<8>", "logic", 0x55, 0x00, 0  ; "and true false")]
#[test_case("&&", "logic<8>", "logic", 0x55, 0xAA, 1  ; "and true true")]
#[test_case("&&", "logic<8>", "logic", 0x00, 0x00, 0  ; "and false false")]
#[test_case("||", "logic<8>", "logic", 0x55, 0x00, 1  ; "or true false")]
#[test_case("||", "logic<8>", "logic", 0x00, 0x00, 0  ; "or false false")]
#[test_case("||", "logic<8>", "logic", 0x01, 0x01, 1  ; "or true true")]
fn comb_logical(op: &str, in_ty: &str, out_ty: &str, a: u64, b: u64, exp: u64) {
    check_comb_binary(op, in_ty, out_ty, a, b, exp);
}

// ===================================================================
// Unary — comb
// ===================================================================

#[test_case("~", "logic<8>", "logic<8>", 0x55, 0xAA ; "bitnot basic")]
#[test_case("~", "logic<8>", "logic<8>", 0x00, 0xFF ; "bitnot zeros")]
#[test_case("~", "logic<8>", "logic<8>", 0xFF, 0x00 ; "bitnot ones")]
#[test_case("!", "logic<8>", "logic",    0x55, 0    ; "lognot nonzero")]
#[test_case("!", "logic<8>", "logic",    0x00, 1    ; "lognot zero")]
#[test_case("+", "logic<8>", "logic<8>", 0xA5, 0xA5 ; "unary plus passthrough")]
fn comb_unary(op: &str, in_ty: &str, out_ty: &str, a: u64, exp: u64) {
    check_comb_unary(op, in_ty, out_ty, a, exp);
}

// ===================================================================
// Unary — ff (representative)
// ===================================================================

#[test_case("~", "logic<8>", "logic<8>", 0x55, 0xAA ; "ff bitnot")]
#[test_case("!", "logic<8>", "logic",    0x55, 0    ; "ff lognot nonzero")]
#[test_case("!", "logic<8>", "logic",    0x00, 1    ; "ff lognot zero")]
#[test_case("+", "logic<8>", "logic<8>", 0xA5, 0xA5 ; "ff unary plus")]
fn ff_unary(op: &str, in_ty: &str, out_ty: &str, a: u64, exp: u64) {
    check_ff_unary(op, in_ty, out_ty, a, exp);
}

// ===================================================================
// Reduction — comb
// ===================================================================

#[test_case("&",  "logic<8>", "logic", 0xFF, 1 ; "red and all ones")]
#[test_case("&",  "logic<8>", "logic", 0xFE, 0 ; "red and not all ones")]
#[test_case("&",  "logic<4>", "logic", 0x0F, 1 ; "red and 4bit all ones")]
#[test_case("|",  "logic<8>", "logic", 0x00, 0 ; "red or all zeros")]
#[test_case("|",  "logic<8>", "logic", 0x01, 1 ; "red or one bit")]
#[test_case("^",  "logic<8>", "logic", 0x01, 1 ; "red xor odd parity")]
#[test_case("^",  "logic<8>", "logic", 0x03, 0 ; "red xor even parity")]
#[test_case("~&", "logic<8>", "logic", 0xFF, 0 ; "red nand all ones")]
#[test_case("~&", "logic<8>", "logic", 0xFE, 1 ; "red nand not all ones")]
#[test_case("~|", "logic<8>", "logic", 0x00, 1 ; "red nor all zeros")]
#[test_case("~|", "logic<8>", "logic", 0x01, 0 ; "red nor has bit")]
#[test_case("~^", "logic<8>", "logic", 0x00, 1 ; "red xnor even zero")]
#[test_case("~^", "logic<8>", "logic", 0x01, 0 ; "red xnor odd one")]
#[test_case("~^", "logic<8>", "logic", 0x03, 1 ; "red xnor even two")]
fn comb_reduction(op: &str, in_ty: &str, out_ty: &str, a: u64, exp: u64) {
    check_comb_unary(op, in_ty, out_ty, a, exp);
}

// ===================================================================
// Reduction — ff (representative)
// ===================================================================

#[test_case("&",  "logic<8>", "logic", 0xFF, 1 ; "ff red and all ones")]
#[test_case("&",  "logic<8>", "logic", 0xFE, 0 ; "ff red and not all ones")]
#[test_case("|",  "logic<8>", "logic", 0x00, 0 ; "ff red or zeros")]
#[test_case("~&", "logic<8>", "logic", 0xFF, 0 ; "ff red nand all ones")]
#[test_case("~|", "logic<8>", "logic", 0x00, 1 ; "ff red nor zeros")]
#[test_case("~^", "logic<8>", "logic", 0x03, 1 ; "ff red xnor even")]
fn ff_reduction(op: &str, in_ty: &str, out_ty: &str, a: u64, exp: u64) {
    check_ff_unary(op, in_ty, out_ty, a, exp);
}

// ###################################################################
// 4-state (X-propagation) tests
// ###################################################################

// ---------------------------------------------------------------------------
// Helper: comb binary with 4-state inputs
// ---------------------------------------------------------------------------
fn check_comb_binary_4s(
    op: &str,
    in_type: &str,
    out_type: &str,
    a_val: u64,
    a_mask: u64,
    b_val: u64,
    b_mask: u64,
    exp_val: u64,
    exp_mask: u64,
) {
    let code = format!(
        r#"
        module Top (a: input {in_type}, b: input {in_type}, o: output {out_type}) {{
            assign o = a {op} b;
        }}
    "#
    );
    let mut sim = Simulator::builder(&code, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let sig_a = sim.signal("a");
    let sig_b = sim.signal("b");
    let sig_o = sim.signal("o");

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(sig_a, BigUint::from(a_val), BigUint::from(a_mask));
        io.set_four_state(sig_b, BigUint::from(b_val), BigUint::from(b_mask));
    })
    .unwrap();

    let (v, m) = sim.get_four_state(sig_o);
    assert_eq!(m, BigUint::from(exp_mask), "4s comb {op}: mask mismatch");
    assert_eq!(v, BigUint::from(exp_val), "4s comb {op}: value mismatch");
}

// ---------------------------------------------------------------------------
// Helper: ff binary with 4-state inputs
// ---------------------------------------------------------------------------
fn check_ff_binary_4s(
    op: &str,
    in_type: &str,
    out_type: &str,
    a_val: u64,
    a_mask: u64,
    b_val: u64,
    b_mask: u64,
    exp_val: u64,
    exp_mask: u64,
) {
    let code = format!(
        r#"
        module Top (clk: input clock, a: input {in_type}, b: input {in_type}, o: output {out_type}) {{
            var r: {out_type};
            always_ff (clk) {{
                r = a {op} b;
            }}
            assign o = r;
        }}
    "#
    );
    let mut sim = Simulator::builder(&code, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let clk = sim.event("clk");
    let sig_a = sim.signal("a");
    let sig_b = sim.signal("b");
    let sig_o = sim.signal("o");

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(sig_a, BigUint::from(a_val), BigUint::from(a_mask));
        io.set_four_state(sig_b, BigUint::from(b_val), BigUint::from(b_mask));
    })
    .unwrap();
    sim.tick(clk).unwrap();

    let (v, m) = sim.get_four_state(sig_o);
    assert_eq!(m, BigUint::from(exp_mask), "4s ff {op}: mask mismatch");
    assert_eq!(v, BigUint::from(exp_val), "4s ff {op}: value mismatch");
}

// ---------------------------------------------------------------------------
// Helper: comb unary with 4-state input
// ---------------------------------------------------------------------------
fn check_comb_unary_4s(
    op: &str,
    in_type: &str,
    out_type: &str,
    a_val: u64,
    a_mask: u64,
    exp_val: u64,
    exp_mask: u64,
) {
    let code = format!(
        r#"
        module Top (a: input {in_type}, o: output {out_type}) {{
            assign o = {op}a;
        }}
    "#
    );
    let mut sim = Simulator::builder(&code, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let sig_a = sim.signal("a");
    let sig_o = sim.signal("o");

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(sig_a, BigUint::from(a_val), BigUint::from(a_mask));
    })
    .unwrap();

    let (v, m) = sim.get_four_state(sig_o);
    assert_eq!(m, BigUint::from(exp_mask), "4s comb {op}: mask mismatch");
    assert_eq!(v, BigUint::from(exp_val), "4s comb {op}: value mismatch");
}

// ---------------------------------------------------------------------------
// Helper: ff unary with 4-state input
// ---------------------------------------------------------------------------
fn check_ff_unary_4s(
    op: &str,
    in_type: &str,
    out_type: &str,
    a_val: u64,
    a_mask: u64,
    exp_val: u64,
    exp_mask: u64,
) {
    let code = format!(
        r#"
        module Top (clk: input clock, a: input {in_type}, o: output {out_type}) {{
            var r: {out_type};
            always_ff (clk) {{
                r = {op}a;
            }}
            assign o = r;
        }}
    "#
    );
    let mut sim = Simulator::builder(&code, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let clk = sim.event("clk");
    let sig_a = sim.signal("a");
    let sig_o = sim.signal("o");

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(sig_a, BigUint::from(a_val), BigUint::from(a_mask));
    })
    .unwrap();
    sim.tick(clk).unwrap();

    let (v, m) = sim.get_four_state(sig_o);
    assert_eq!(m, BigUint::from(exp_mask), "4s ff {op}: mask mismatch");
    assert_eq!(v, BigUint::from(exp_val), "4s ff {op}: value mismatch");
}

// ===================================================================
// 4-state: Bitwise — comb
// Masks: AND/OR per-bit precise, XOR = l_m|r_m, XNOR = l_m|r_m
// ===================================================================

//                  op   in_ty       out_ty      a_v   a_m   b_v   b_m   e_v   e_m
#[test_case("&",  "logic<8>", "logic<8>", 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00 ; "4s and 0 and X is 0")]
#[test_case("&",  "logic<8>", "logic<8>", 0xFF, 0x00, 0x00, 0xFF, 0x00, 0xFF ; "4s and 1 and X is X")]
#[test_case("&",  "logic<8>", "logic<8>", 0xFF, 0x00, 0xA5, 0x00, 0xA5, 0x00 ; "4s and defined")]
#[test_case("|",  "logic<8>", "logic<8>", 0xFF, 0x00, 0x00, 0xFF, 0xFF, 0x00 ; "4s or 1 or X is 1")]
#[test_case("|",  "logic<8>", "logic<8>", 0x00, 0x00, 0x00, 0xFF, 0x00, 0xFF ; "4s or 0 or X is X")]
#[test_case("|",  "logic<8>", "logic<8>", 0xA5, 0x00, 0x5A, 0x00, 0xFF, 0x00 ; "4s or defined")]
// IEEE: XOR mask = l_m|r_m. Value at X positions must be 0.
#[test_case("^",  "logic<8>", "logic<8>", 0x55, 0x00, 0x00, 0xFF, 0x00, 0xFF ; "4s xor with X")]
#[test_case("^",  "logic<8>", "logic<8>", 0x55, 0x00, 0xAA, 0x00, 0xFF, 0x00 ; "4s xor defined")]
#[test_case("~^", "logic<8>", "logic<8>", 0xF0, 0x00, 0x00, 0xFF, 0x00, 0xFF ; "4s xnor with X")]
#[test_case("~^", "logic<8>", "logic<8>", 0xF0, 0x00, 0xFF, 0x00, 0xF0, 0x00 ; "4s xnor defined")]
fn comb_bitwise_4s(
    op: &str,
    in_ty: &str,
    out_ty: &str,
    a_v: u64,
    a_m: u64,
    b_v: u64,
    b_m: u64,
    e_v: u64,
    e_m: u64,
) {
    check_comb_binary_4s(op, in_ty, out_ty, a_v, a_m, b_v, b_m, e_v, e_m);
}

// ===================================================================
// 4-state: Arithmetic — comb  (any X → all X, value zeroed)
// ===================================================================

#[test_case("+",  "logic<8>", "logic<8>", 10, 0x00, 0, 0x01, 0x00, 0xFF ; "4s add X input")]
#[test_case("+",  "logic<8>", "logic<8>", 10, 0x00, 5, 0x00, 15,   0x00 ; "4s add defined")]
#[test_case("-",  "logic<8>", "logic<8>", 10, 0x00, 0, 0x01, 0x00, 0xFF ; "4s sub X input")]
#[test_case("*",  "logic<8>", "logic<8>",  7, 0x00, 0, 0x01, 0x00, 0xFF ; "4s mul X input")]
fn comb_arith_4s(
    op: &str,
    in_ty: &str,
    out_ty: &str,
    a_v: u64,
    a_m: u64,
    b_v: u64,
    b_m: u64,
    e_v: u64,
    e_m: u64,
) {
    check_comb_binary_4s(op, in_ty, out_ty, a_v, a_m, b_v, b_m, e_v, e_m);
}

// ===================================================================
// 4-state: Comparison — comb  (any X → result X)
// ===================================================================

#[test_case("==", "logic<8>", "logic", 42, 0x00, 0, 0x01, 0, 1 ; "4s eq X input")]
#[test_case("<:", "logic<8>", "logic", 10, 0x00, 0, 0x01, 0, 1 ; "4s lt X input")]
#[test_case("==", "logic<8>", "logic", 42, 0x00, 42, 0x00, 1, 0 ; "4s eq defined")]
fn comb_compare_4s(
    op: &str,
    in_ty: &str,
    out_ty: &str,
    a_v: u64,
    a_m: u64,
    b_v: u64,
    b_m: u64,
    e_v: u64,
    e_m: u64,
) {
    check_comb_binary_4s(op, in_ty, out_ty, a_v, a_m, b_v, b_m, e_v, e_m);
}

// ===================================================================
// 4-state: Shift — comb  (X in amount → all X)
// ===================================================================

// IEEE 1800: X in shift amount → result is all-X (value=0, mask=all-ones)
#[test_case("<<", "logic<8>", "logic<8>", 0x01, 0x00, 0, 0xFF, 0x00, 0xFF ; "4s shl X amount")]
#[test_case(">>", "logic<8>", "logic<8>", 0xFF, 0x00, 0, 0xFF, 0x00, 0xFF ; "4s shr X amount")]
fn comb_shift_4s(
    op: &str,
    in_ty: &str,
    out_ty: &str,
    a_v: u64,
    a_m: u64,
    b_v: u64,
    b_m: u64,
    e_v: u64,
    e_m: u64,
) {
    check_comb_binary_4s(op, in_ty, out_ty, a_v, a_m, b_v, b_m, e_v, e_m);
}

// ===================================================================
// 4-state: Unary — comb
// BitNot: mask preserved, value bits flipped
// Reduction / LogicNot: any X → all X (conservative)
// ===================================================================

// IEEE: value bits at X positions must be 0. ~0xA5=0x5A, masked by ~0x0F → 0x50
#[test_case("~",  "logic<8>", "logic<8>", 0xA5, 0x0F, 0x50, 0x0F ; "4s bitnot partial X")]
#[test_case("~",  "logic<8>", "logic<8>", 0xFF, 0x00, 0x00, 0x00 ; "4s bitnot defined")]
#[test_case("!",  "logic<8>", "logic",    0x55, 0x01, 0x00, 0x01 ; "4s lognot X input")]
fn comb_unary_4s(
    op: &str,
    in_ty: &str,
    out_ty: &str,
    a_v: u64,
    a_m: u64,
    e_v: u64,
    e_m: u64,
) {
    check_comb_unary_4s(op, in_ty, out_ty, a_v, a_m, e_v, e_m);
}

// ===================================================================
// 4-state: Reduction — comb  (any X → all X conservative)
// ===================================================================

#[test_case("&",  "logic<8>", "logic", 0xFF, 0x01, 0x00, 0x01 ; "4s red and X")]
#[test_case("&",  "logic<8>", "logic", 0xFF, 0x00, 0x01, 0x00 ; "4s red and defined")]
#[test_case("|",  "logic<8>", "logic", 0x01, 0x02, 0x01, 0x00 ; "4s red or dominant")]
#[test_case("|",  "logic<8>", "logic", 0x00, 0x02, 0x00, 0x01 ; "4s red or X")]
#[test_case("^",  "logic<8>", "logic", 0x03, 0x01, 0x00, 0x01 ; "4s red xor X")]
#[test_case("^",  "logic<8>", "logic", 0x03, 0x00, 0x00, 0x00 ; "4s red xor defined")]
#[test_case("~&", "logic<8>", "logic", 0xFF, 0x01, 0x00, 0x01 ; "4s red nand X")]
#[test_case("~|", "logic<8>", "logic", 0x00, 0x01, 0x00, 0x01 ; "4s red nor X")]
#[test_case("~^", "logic<8>", "logic", 0x03, 0x01, 0x00, 0x01 ; "4s red xnor X")]
#[test_case("~^", "logic<8>", "logic", 0x03, 0x00, 0x01, 0x00 ; "4s red xnor defined")]
fn comb_reduction_4s(
    op: &str,
    in_ty: &str,
    out_ty: &str,
    a_v: u64,
    a_m: u64,
    e_v: u64,
    e_m: u64,
) {
    check_comb_unary_4s(op, in_ty, out_ty, a_v, a_m, e_v, e_m);
}

// ===================================================================
// 4-state: FF path (representative)
// ===================================================================

#[test_case("&",  "logic<8>", "logic<8>", 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00 ; "4s ff and 0 X")]
#[test_case("|",  "logic<8>", "logic<8>", 0xFF, 0x00, 0x00, 0xFF, 0xFF, 0x00 ; "4s ff or 1 X")]
#[test_case("+",  "logic<8>", "logic<8>",   10, 0x00,    5, 0x00,   15, 0x00 ; "4s ff add defined")]
#[test_case("+",  "logic<8>", "logic<8>",   10, 0x00,    0, 0x01, 0x00, 0xFF ; "4s ff add X")]
fn ff_binary_4s(
    op: &str,
    in_ty: &str,
    out_ty: &str,
    a_v: u64,
    a_m: u64,
    b_v: u64,
    b_m: u64,
    e_v: u64,
    e_m: u64,
) {
    check_ff_binary_4s(op, in_ty, out_ty, a_v, a_m, b_v, b_m, e_v, e_m);
}

// IEEE: ~0xA5=0x5A, mask=0x0F → value at X positions zeroed → 0x50
#[test_case("~",  "logic<8>", "logic<8>", 0xA5, 0x0F, 0x50, 0x0F ; "4s ff bitnot partial X")]
#[test_case("^",  "logic<8>", "logic",    0x03, 0x01, 0x00, 0x01 ; "4s ff red xor X")]
#[test_case("~^", "logic<8>", "logic",    0x03, 0x00, 0x01, 0x00 ; "4s ff red xnor defined")]
#[test_case("~^", "logic<8>", "logic",    0x03, 0x01, 0x00, 0x01 ; "4s ff red xnor X")]
fn ff_unary_4s(
    op: &str,
    in_ty: &str,
    out_ty: &str,
    a_v: u64,
    a_m: u64,
    e_v: u64,
    e_m: u64,
) {
    check_ff_unary_4s(op, in_ty, out_ty, a_v, a_m, e_v, e_m);
}

// ###################################################################
// Wide (>64-bit) 4-state tests — exercises the multi-chunk path
// ###################################################################

/// Helper for wide 4-state comb binary.
fn check_wide_comb_binary_4s(
    op: &str,
    width: usize,
    a_val: &BigUint,
    a_mask: &BigUint,
    b_val: &BigUint,
    b_mask: &BigUint,
    exp_val: &BigUint,
    exp_mask: &BigUint,
) {
    let ty = format!("logic<{width}>");
    let code = format!(
        r#"
        module Top (a: input {ty}, b: input {ty}, o: output {ty}) {{
            assign o = a {op} b;
        }}
    "#
    );
    let mut sim = Simulator::builder(&code, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let sig_a = sim.signal("a");
    let sig_b = sim.signal("b");
    let sig_o = sim.signal("o");

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(sig_a, a_val.clone(), a_mask.clone());
        io.set_four_state(sig_b, b_val.clone(), b_mask.clone());
    })
    .unwrap();

    let (v, m) = sim.get_four_state(sig_o);
    assert_eq!(&m, exp_mask, "wide 4s comb {op}: mask mismatch");
    assert_eq!(&v, exp_val, "wide 4s comb {op}: value mismatch");
}

/// Helper for wide 4-state comb unary.
fn check_wide_comb_unary_4s(
    op: &str,
    width: usize,
    a_val: &BigUint,
    a_mask: &BigUint,
    exp_val: &BigUint,
    exp_mask: &BigUint,
) {
    let ty = format!("logic<{width}>");
    let code = format!(
        r#"
        module Top (a: input {ty}, o: output {ty}) {{
            assign o = {op}a;
        }}
    "#
    );
    let mut sim = Simulator::builder(&code, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let sig_a = sim.signal("a");
    let sig_o = sim.signal("o");

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(sig_a, a_val.clone(), a_mask.clone());
    })
    .unwrap();

    let (v, m) = sim.get_four_state(sig_o);
    assert_eq!(&m, exp_mask, "wide 4s comb {op}: mask mismatch");
    assert_eq!(&v, exp_val, "wide 4s comb {op}: value mismatch");
}

// ===================================================================
// Wide 4-state: XOR — value at X positions must be 0 (IEEE)
// ===================================================================

#[test]
fn wide_4s_xor_with_x() {
    // 128-bit: a is defined, b is all-X → result all-X, value must be 0
    let a_val = BigUint::from(0xDEAD_BEEF_CAFE_BABEu64) << 64 | BigUint::from(0x1234_5678_9ABC_DEF0u64);
    let a_mask = BigUint::from(0u64);
    let b_val = BigUint::from(0u64);
    let b_mask = (BigUint::from(1u64) << 128) - BigUint::from(1u64); // all-ones 128-bit
    let exp_val = BigUint::from(0u64); // IEEE: value=0 when mask=all-X
    let exp_mask = (BigUint::from(1u64) << 128) - BigUint::from(1u64);
    check_wide_comb_binary_4s("^", 128, &a_val, &a_mask, &b_val, &b_mask, &exp_val, &exp_mask);
}

#[test]
fn wide_4s_xor_defined() {
    // 128-bit: both defined → normal XOR, no X
    let a_val = BigUint::from(0xFFFF_FFFF_FFFF_FFFFu64) << 64 | BigUint::from(0u64);
    let b_val = BigUint::from(0u64) << 64 | BigUint::from(0xFFFF_FFFF_FFFF_FFFFu64);
    let a_mask = BigUint::from(0u64);
    let b_mask = BigUint::from(0u64);
    let exp_val = (BigUint::from(1u64) << 128) - BigUint::from(1u64); // all ones
    let exp_mask = BigUint::from(0u64);
    check_wide_comb_binary_4s("^", 128, &a_val, &a_mask, &b_val, &b_mask, &exp_val, &exp_mask);
}

// ===================================================================
// Wide 4-state: BitNot — value at X positions must be 0 (IEEE)
// ===================================================================

#[test]
fn wide_4s_bitnot_partial_x() {
    // 128-bit: lower 64 bits are X, upper 64 bits defined as 0xFF..FF
    // ~(0xFFFF..., mask=0x0000...FFFF...) →
    //   value: ~0xFFFF... = 0x0000... for upper, X for lower → 0x0000... (lower zeroed by IEEE)
    //   mask: 0x0000...FFFF...
    let a_val = (BigUint::from(0xFFFF_FFFF_FFFF_FFFFu64) << 64) | BigUint::from(0xAAAA_BBBB_CCCC_DDDDu64);
    let a_mask = BigUint::from(0xFFFF_FFFF_FFFF_FFFFu64); // lower 64 bits are X
    // ~a: upper 64 bits → 0x0000..., lower 64 bits → ~0xAAAA... but masked to X
    // IEEE: value at X positions = 0
    let exp_val = BigUint::from(0u64); // upper ~0xFFFF = 0x0000, lower is X → 0
    let exp_mask = BigUint::from(0xFFFF_FFFF_FFFF_FFFFu64);
    check_wide_comb_unary_4s("~", 128, &a_val, &a_mask, &exp_val, &exp_mask);
}

// ===================================================================
// Wide 4-state: Arithmetic — any X → all X, value=0 (IEEE)
// ===================================================================

#[test]
fn wide_4s_add_x() {
    let a_val = BigUint::from(1u64) << 100;
    let a_mask = BigUint::from(0u64);
    let b_val = BigUint::from(0u64);
    let b_mask = BigUint::from(1u64); // bit 0 is X
    let exp_val = BigUint::from(0u64);
    let exp_mask = (BigUint::from(1u64) << 128) - BigUint::from(1u64);
    check_wide_comb_binary_4s("+", 128, &a_val, &a_mask, &b_val, &b_mask, &exp_val, &exp_mask);
}

// ===================================================================
// Wide 4-state: Reduction — any X → result X, value=0 (IEEE)
// ===================================================================

#[test]
fn wide_4s_red_xor_x() {
    // 128-bit value with one X bit → reduction XOR result is X
    let a_val = BigUint::from(0x03u64) << 64; // some bits set in upper half
    let a_mask = BigUint::from(1u64); // bit 0 is X
    let exp_val = BigUint::from(0u64);
    let exp_mask = BigUint::from(1u64); // 1-bit result, X
    check_wide_comb_unary_4s("^", 128, &a_val, &a_mask, &exp_val, &exp_mask);
}

// ###################################################################
// Concat 4-state tests
// ###################################################################

#[test]
fn concat_4s_partial_x() {
    // {a, b} where a is 8-bit defined, b is 8-bit with X
    // Result: upper 8 bits defined, lower 8 bits X
    // IEEE: value at X positions must be 0
    let code = r#"
        module Top (a: input logic<8>, b: input logic<8>, o: output logic<16>) {
            assign o = {a, b};
        }
    "#;
    let mut sim = Simulator::builder(code, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let sig_a = sim.signal("a");
    let sig_b = sim.signal("b");
    let sig_o = sim.signal("o");

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(sig_a, BigUint::from(0xABu64), BigUint::from(0u64));
        io.set_four_state(sig_b, BigUint::from(0xCDu64), BigUint::from(0xFFu64)); // all X
    })
    .unwrap();

    let (v, m) = sim.get_four_state(sig_o);
    // mask: upper 8 = 0x00, lower 8 = 0xFF → 0x00FF
    assert_eq!(m, BigUint::from(0x00FFu64), "concat 4s: mask mismatch");
    // IEEE: value at X positions = 0 → upper 0xAB, lower 0x00 → 0xAB00
    assert_eq!(v, BigUint::from(0xAB00u64), "concat 4s: value mismatch");
}

#[test]
fn wide_concat_4s_partial_x() {
    // {a, b} where a is 64-bit defined, b is 64-bit with all X
    // Total: 128 bits — exercises the wide concat path
    let code = r#"
        module Top (a: input logic<64>, b: input logic<64>, o: output logic<128>) {
            assign o = {a, b};
        }
    "#;
    let mut sim = Simulator::builder(code, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let sig_a = sim.signal("a");
    let sig_b = sim.signal("b");
    let sig_o = sim.signal("o");

    sim.modify(|io: &mut IOContext| {
        io.set_four_state(sig_a, BigUint::from(0xCAFE_BABE_DEAD_BEEFu64), BigUint::from(0u64));
        io.set_four_state(sig_b, BigUint::from(0x1234_5678_9ABC_DEF0u64), BigUint::from(0xFFFF_FFFF_FFFF_FFFFu64));
    })
    .unwrap();

    let (v, m) = sim.get_four_state(sig_o);
    let exp_mask = BigUint::from(0xFFFF_FFFF_FFFF_FFFFu64); // lower 64 bits X
    let exp_val = BigUint::from(0xCAFE_BABE_DEAD_BEEFu64) << 64; // upper defined, lower 0
    assert_eq!(m, exp_mask, "wide concat 4s: mask mismatch");
    assert_eq!(v, exp_val, "wide concat 4s: value mismatch");
}
