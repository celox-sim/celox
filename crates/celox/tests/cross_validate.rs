//! Cross-validation: run the same Veryl module with both Native and Cranelift
//! backends, feed identical inputs, and assert identical outputs.

use celox::{BigUint, Simulator};

/// Build same module with native and Cranelift, set inputs via closures,
/// compare all listed output signals.
macro_rules! cross_validate {
    ($code:expr, $top:expr, $setup_n:expr, $setup_c:expr, $signals:expr) => {{
        let mut sim_n = Simulator::builder($code, $top).build().unwrap();
        let mut sim_c = Simulator::builder($code, $top).build_cranelift().unwrap();
        $setup_n(&mut sim_n);
        $setup_c(&mut sim_c);
        for &sig_name in $signals {
            let sn = sim_n.signal(sig_name);
            let sc = sim_c.signal(sig_name);
            let vn: BigUint = sim_n.get(sn);
            let vc: BigUint = sim_c.get(sc);
            assert_eq!(vn, vc,
                "Native vs Cranelift mismatch on '{sig_name}': native={vn:#x}, cranelift={vc:#x}");
        }
    }};
}

// ── Basic arithmetic ──

#[test]
fn xv_add_sub_mul() {
    let code = r#"module Top(a: input logic<64>, b: input logic<64>,
        o_add: output logic<64>, o_sub: output logic<64>, o_mul: output logic<64>) {
        assign o_add = a + b;
        assign o_sub = a - b;
        assign o_mul = a * b;
    }"#;
    cross_validate!(code, "Top",
        |sim: &mut Simulator<_>| {
            sim.set(sim.signal("a"), 0xDEAD_BEEF_u64);
            sim.set(sim.signal("b"), 0xCAFE_BABE_u64);
        },
        |sim: &mut Simulator<_>| {
            sim.set(sim.signal("a"), 0xDEAD_BEEF_u64);
            sim.set(sim.signal("b"), 0xCAFE_BABE_u64);
        },
        &["o_add", "o_sub", "o_mul"]
    );
}

// ── Wide arithmetic ──

#[test]
fn xv_wide_add_128() {
    let code = r#"module Top(a: input logic<128>, b: input logic<128>, o: output logic<128>) {
        assign o = a + b;
    }"#;
    let a_val: BigUint = BigUint::from(1u64) << 64 | BigUint::from(0x1234_5678u64);
    let b_val: BigUint = BigUint::from(1u64);
    cross_validate!(code, "Top",
        |sim: &mut Simulator<_>| {
            sim.set_wide(sim.signal("a"), a_val.clone());
            sim.set_wide(sim.signal("b"), b_val.clone());
        },
        |sim: &mut Simulator<_>| {
            sim.set_wide(sim.signal("a"), a_val.clone());
            sim.set_wide(sim.signal("b"), b_val.clone());
        },
        &["o"]
    );
}

#[test]
fn xv_wide_add_carry() {
    let code = r#"module Top(a: input logic<128>, b: input logic<128>, o: output logic<128>) {
        assign o = a + b;
    }"#;
    cross_validate!(code, "Top",
        |sim: &mut Simulator<_>| {
            sim.set_wide(sim.signal("a"), BigUint::from(u64::MAX));
            sim.set_wide(sim.signal("b"), BigUint::from(1u64));
        },
        |sim: &mut Simulator<_>| {
            sim.set_wide(sim.signal("a"), BigUint::from(u64::MAX));
            sim.set_wide(sim.signal("b"), BigUint::from(1u64));
        },
        &["o"]
    );
}

#[test]
fn xv_wide_sub_128() {
    let code = r#"module Top(a: input logic<128>, b: input logic<128>, o: output logic<128>) {
        assign o = a - b;
    }"#;
    cross_validate!(code, "Top",
        |sim: &mut Simulator<_>| {
            sim.set_wide(sim.signal("a"), BigUint::from(1u64) << 64);
            sim.set_wide(sim.signal("b"), BigUint::from(1u64));
        },
        |sim: &mut Simulator<_>| {
            sim.set_wide(sim.signal("a"), BigUint::from(1u64) << 64);
            sim.set_wide(sim.signal("b"), BigUint::from(1u64));
        },
        &["o"]
    );
}

// ── Wide shifts ──

#[test]
fn xv_wide_shl_256() {
    let code = r#"module Top(a: input logic<256>, amt: input logic<9>, o: output logic<256>) {
        assign o = a << amt;
    }"#;
    for &amt in &[0u16, 1, 4, 63, 64, 65, 128, 200, 255] {
        cross_validate!(code, "Top",
            |sim: &mut Simulator<_>| {
                sim.set_wide(sim.signal("a"), BigUint::from(0xDEAD_BEEF_CAFE_BABEu64));
                sim.set(sim.signal("amt"), amt);
            },
            |sim: &mut Simulator<_>| {
                sim.set_wide(sim.signal("a"), BigUint::from(0xDEAD_BEEF_CAFE_BABEu64));
                sim.set(sim.signal("amt"), amt);
            },
            &["o"]
        );
    }
}

#[test]
fn xv_wide_shr_256() {
    let code = r#"module Top(a: input logic<256>, amt: input logic<9>, o: output logic<256>) {
        assign o = a >> amt;
    }"#;
    let val: BigUint = BigUint::from(0xABCDu64) << 192 | BigUint::from(0x1234u64);
    for &amt in &[0u16, 1, 4, 64, 128, 192, 255] {
        cross_validate!(code, "Top",
            |sim: &mut Simulator<_>| {
                sim.set_wide(sim.signal("a"), val.clone());
                sim.set(sim.signal("amt"), amt);
            },
            |sim: &mut Simulator<_>| {
                sim.set_wide(sim.signal("a"), val.clone());
                sim.set(sim.signal("amt"), amt);
            },
            &["o"]
        );
    }
}

#[test]
fn xv_wide_shl_512() {
    let code = r#"module Top(a: input logic<512>, amt: input logic<10>, o: output logic<512>) {
        assign o = a << amt;
    }"#;
    for &amt in &[0u16, 1, 64, 65, 200, 511] {
        cross_validate!(code, "Top",
            |sim: &mut Simulator<_>| {
                sim.set_wide(sim.signal("a"), BigUint::from(0xDEADu64));
                sim.set(sim.signal("amt"), amt);
            },
            |sim: &mut Simulator<_>| {
                sim.set_wide(sim.signal("a"), BigUint::from(0xDEADu64));
                sim.set(sim.signal("amt"), amt);
            },
            &["o"]
        );
    }
}

#[test]
fn xv_wide_shl_1024() {
    let code = r#"module Top(a: input logic<1024>, amt: input logic<11>, o: output logic<1024>) {
        assign o = a << amt;
    }"#;
    for &amt in &[0u16, 1, 64, 500, 1023] {
        cross_validate!(code, "Top",
            |sim: &mut Simulator<_>| {
                sim.set_wide(sim.signal("a"), BigUint::from(0xDEADu64));
                sim.set(sim.signal("amt"), amt);
            },
            |sim: &mut Simulator<_>| {
                sim.set_wide(sim.signal("a"), BigUint::from(0xDEADu64));
                sim.set(sim.signal("amt"), amt);
            },
            &["o"]
        );
    }
}

// ── Narrow→wide cast + shift ──

#[test]
fn xv_narrow_to_wide_shl() {
    let code = r#"module Top(a: input logic<256>, amt: input logic<10>, o: output logic<512>) {
        assign o = (a as 512) << amt;
    }"#;
    for &amt in &[0u16, 1, 256, 300] {
        cross_validate!(code, "Top",
            |sim: &mut Simulator<_>| {
                sim.set_wide(sim.signal("a"), BigUint::from(0xDEAD_BEEF_CAFE_BABEu64));
                sim.set(sim.signal("amt"), amt);
            },
            |sim: &mut Simulator<_>| {
                sim.set_wide(sim.signal("a"), BigUint::from(0xDEAD_BEEF_CAFE_BABEu64));
                sim.set(sim.signal("amt"), amt);
            },
            &["o"]
        );
    }
}

// ── Bitwise wide ──

#[test]
fn xv_wide_bitwise() {
    let code = r#"module Top(a: input logic<256>, b: input logic<256>,
        o_and: output logic<256>, o_or: output logic<256>, o_xor: output logic<256>) {
        assign o_and = a & b;
        assign o_or  = a | b;
        assign o_xor = a ^ b;
    }"#;
    cross_validate!(code, "Top",
        |sim: &mut Simulator<_>| {
            sim.set_wide(sim.signal("a"), BigUint::from(0xFF00u64) | (BigUint::from(0xAAu64) << 128));
            sim.set_wide(sim.signal("b"), BigUint::from(0x0FF0u64) | (BigUint::from(0x55u64) << 128));
        },
        |sim: &mut Simulator<_>| {
            sim.set_wide(sim.signal("a"), BigUint::from(0xFF00u64) | (BigUint::from(0xAAu64) << 128));
            sim.set_wide(sim.signal("b"), BigUint::from(0x0FF0u64) | (BigUint::from(0x55u64) << 128));
        },
        &["o_and", "o_or", "o_xor"]
    );
}

