use celox::{BigUint, Simulator};

#[test]
fn test_wide_int_memory_access() {
    let code = r#"
        module Top (
            i: input logic<256>,
            o: output logic<256>
        ) {
            var mem: logic<256>;
            always_comb {
                mem = i;
                o = mem;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let i = sim.signal("i");
    let o = sim.signal("o");

    let input_val: BigUint =
        (BigUint::from(1u32) << 200) | (BigUint::from(1u32) << 100) | BigUint::from(1u32);

    sim.modify(|io| io.set_wide(i, input_val.clone())).unwrap();
    assert_eq!(
        sim.get(o),
        input_val,
        "256-bit value should be preserved through memory"
    );
}

#[test]
fn test_wide_concatenation() {
    use malachite_bigint::ToBigUint;

    let code = r#"
        module Top (
            a: input logic<64>,
            b: input logic<64>,
            c: input logic<64>,
            o: output logic<192>
        ) {
            assign o = {a, b, c};
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let c = sim.signal("c");
    let o = sim.signal("o");

    let val_a = 0xAAAA_AAAA_AAAA_AAAAu64;
    let val_b = 0xBBBB_BBBB_BBBB_BBBBu64;
    let val_c = 0xCCCC_CCCC_CCCC_CCCCu64;

    sim.modify(|io| {
        io.set(a, val_a);
        io.set(b, val_b);
        io.set(c, val_c);
    })
    .unwrap();

    let expected = (val_a.to_biguint().unwrap() << 128)
        | (val_b.to_biguint().unwrap() << 64)
        | val_c.to_biguint().unwrap();

    assert_eq!(sim.get(o), expected);
}

#[test]
fn test_wide_partial_write() {
    use malachite_bigint::ToBigUint;

    let code = r#"
        module Top (
            val: input logic<64>,
            o: output logic<256>
        ) {
            var wide: logic<256>;
            always_comb {
                wide = 0;
                wide[127:64] = val;
                o = wide;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let val = sim.signal("val");
    let o = sim.signal("o");

    let input_val = 0xDEAD_BEEF_CAFE_BABE_u64;
    sim.modify(|io| io.set(val, input_val)).unwrap();

    let expected = input_val.to_biguint().unwrap() << 64;
    assert_eq!(
        sim.get(o),
        expected,
        "Partial write across 64-bit boundaries should work"
    );
}

#[test]
fn test_wide_cross_boundary_unaligned_write() {
    let code = r#"
        module Top (
            val: input logic<125>,
            o: output logic<256>
        ) {
            var wide: logic<256>;
            always_comb {
                wide = 0;
                wide[184:60] = val;
                o = wide;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let val = sim.signal("val");
    let o = sim.signal("o");

    let input_val: BigUint = (BigUint::from(1u32) << 125) - 1u32;
    sim.modify(|io| io.set_wide(val, input_val.clone()))
        .unwrap();

    let expected = input_val << 60;
    assert_eq!(
        sim.get(o),
        expected,
        "Unaligned write crossing 3 chunks failed"
    );
}

#[test]
fn test_wide_rmw_preserve_neighboring_bits() {
    let code = r#"
        module Top (
            val: input logic<16>,
            o: output logic<128>
        ) {
            var wide: logic<128>;
            always_comb {
                wide = 128'hAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA;
                wide[71:56] = val;
                o = wide;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let val = sim.signal("val");
    let o = sim.signal("o");

    sim.modify(|io| io.set(val, 0xFFFFu16)).unwrap();

    let base_val = BigUint::parse_bytes(b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", 16).unwrap();
    let full_mask = (BigUint::from(1u32) << 128) - 1u32;
    let target_mask = ((BigUint::from(1u32) << 16) - 1u32) << 56;
    let inv_target_mask = &full_mask ^ &target_mask;
    let expected = (base_val & inv_target_mask) | (BigUint::from(0xFFFFu32) << 56);

    assert_eq!(
        sim.get(o),
        expected,
        "Neighbors were corrupted during Read-Modify-Write"
    );
}



