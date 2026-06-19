use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_wide_int_memory_access(sim) {
        @setup { let code = r#"
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
"#; }
        @build Simulator::builder(code, "Top");
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

    fn test_wide_concatenation(sim) {
        @setup { use num_bigint::ToBigUint;
let code = r#"
module Top (
a: input logic<64>,
b: input logic<64>,
c: input logic<64>,
o: output logic<192>
) {
assign o = {a, b, c};
}
"#; }
        @build Simulator::builder(code, "Top");
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

    fn test_nested_wide_concatenation(sim) {
        @setup { use num_bigint::ToBigUint;
let code = r#"
module Top (
a: input logic<65>,
b: input logic<63>,
c: input logic<1>,
d: input logic<70>,
o_nested: output logic<199>,
o_flat: output logic<199>
) {
var ab: logic<128>;
assign ab = {a, b};
assign o_nested = {ab, c, d};
assign o_flat = {a, b, c, d};
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let c = sim.signal("c");
    let d = sim.signal("d");
    let o_nested = sim.signal("o_nested");
    let o_flat = sim.signal("o_flat");

    let val_a = (1u128 << 64) | 0x0123_4567_89AB_CDEFu128;
    let val_b = 0x2345_6789_ABCD_EF01u64 & ((1u64 << 63) - 1);
    let val_c = 1u8;
    let val_d: BigUint =
        (BigUint::from(1u32) << 69) | BigUint::from(0x3456_789A_BCDE_F012u64);

    sim.modify(|io| {
        io.set_wide(a, val_a.to_biguint().unwrap());
        io.set(b, val_b);
        io.set(c, val_c);
        io.set_wide(d, val_d.clone());
    })
    .unwrap();

    let expected = (val_a.to_biguint().unwrap() << (63 + 1 + 70))
        | (val_b.to_biguint().unwrap() << (1 + 70))
        | (val_c.to_biguint().unwrap() << 70)
        | val_d;

    assert_eq!(sim.get(o_nested), expected);
    assert_eq!(sim.get(o_flat), expected);
    assert_eq!(sim.get(o_nested), sim.get(o_flat));

    }

    fn test_wide_partial_write(sim) {
        @ignore_on(veryl);
        @setup { use num_bigint::ToBigUint;
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
"#; }
        @build Simulator::builder(code, "Top");
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

    fn test_wide_cross_boundary_unaligned_write(sim) {
        @setup { let code = r#"
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
"#; }
        @build Simulator::builder(code, "Top");
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

    fn test_wide_rmw_preserve_neighboring_bits(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
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
"#; }
        @build Simulator::builder(code, "Top");
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
}
