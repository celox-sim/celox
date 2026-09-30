//! Adapted from veryl-lang/veryl simulator regressions at
//! ba7daead7d69f95988f25807795956bf89827b32.
//! See ../../UPSTREAM_CASES.md for source mapping and license attribution.
use crate::{BigUint, Design};

cases! { Regression, "veryl_regressions";

    fn wide_shift_amount_out_of_range(sim) {
        @build Design::new(r#"
    module Top (
        v:   input  logic<8>,
        amt: input  logic<70>,
        r:   output logic<8>,
    ) {
        assign r = v >> amt;
    }
    "#, "Top");
        let v = sim.signal("v"); let amt = sim.signal("amt"); let r = sim.signal("r");
        for value in [0u8, 0xa5, 0xff] {
            for amount in [BigUint::from(0u8), 1u8.into(), 7u8.into(), 8u8.into(), 256u16.into(), (BigUint::from(1u8) << 64usize) + 1u8] {
                sim.modify(|io| { io.set(v, value); io.set_wide(amt, amount.clone()); }).unwrap();
                let expected = if amount >= BigUint::from(8u8) { 0 } else { value >> amount.to_u32_digits().first().copied().unwrap_or(0) };
                assert_eq!(sim.get(r), expected.into(), "value={value:#x}, amount={amount}");
            }
        }
    }

    fn unary_minus_as_shift_amount(sim) {
        @build Design::new(r#"
    module Top (
        base: input  logic<28>,
        exp:  input  logic<10>,
        r:    output logic<28>,
    ) {
        assign r = base << -exp;
    }
    "#, "Top");
        let base = sim.signal("base"); let exp = sim.signal("exp"); let r = sim.signal("r");
        for value in [0u32, 1, 0x0555_5555, 0x0fff_ffff] {
            for exponent in [0u16, 1, 2, 1023] {
                sim.modify(|io| { io.set(base, value); io.set(exp, exponent); }).unwrap();
                let shift = (0u16.wrapping_sub(exponent)) & 1023;
                let expected = if shift >= 28 { 0 } else { (value << shift) & 0x0fff_ffff };
                assert_eq!(sim.get(r), expected.into(), "base={value:#x}, exp={exponent}");
            }
        }
    }

    fn struct_bit_field_rhs_no_spill(sim) {
        @build Design::new(r#"
    module Top (
        a: input  logic<8> ,
        b: input  logic<8> ,
        o: output logic<10>,
    ) {
        struct s_t {
            lo : logic<4>,
            ext: logic<2>,
            hi : logic<4>,
        }
        var m: s_t;
        assign m.lo     = 4'hf;
        assign m.ext[0] = ~(a == b);
        assign m.ext[1] = 1'b0;
        assign m.hi     = 4'hf;
        assign o        = m;
    }
    "#, "Top");
        let a = sim.signal("a"); let b = sim.signal("b"); let o = sim.signal("o");
        for (left, right) in [(0u8, 1u8), (1, 1), (0xff, 0), (0, 0)] {
            sim.modify(|io| { io.set(a, left); io.set(b, right); }).unwrap();
            let expected = 0x3cfu16 | (u16::from(left != right) << 4);
            assert_eq!(sim.get(o), expected.into());
        }
    }

    fn wide_struct_bit_field_rhs_no_spill(sim) {
        @build Design::new(r#"
    module Top (
        a: input  logic<8>  ,
        b: input  logic<8>  ,
        o: output logic<200>,
    ) {
        struct w_t {
            lo : logic<100>,
            ext: logic<2>  ,
            hi : logic<98> ,
        }
        var m: w_t;
        assign m.lo     = '1;
        assign m.ext[0] = ~(a == b);
        assign m.ext[1] = 1'b0;
        assign m.hi     = '1;
        assign o        = m;
    }
    "#, "Top");
        let a = sim.signal("a"); let b = sim.signal("b"); let o = sim.signal("o");
        let all = (BigUint::from(1u8) << 200usize) - 1u8;
        for (left, right) in [(0u8, 1u8), (1, 1), (0xff, 0), (0, 0)] {
            sim.modify(|io| { io.set(a, left); io.set(b, right); }).unwrap();
            // IEEE 1800-2023 7.2.1: the first packed member is most significant.
            let mut expected = all.clone(); expected.set_bit(99, false); expected.set_bit(98, left != right);
            assert_eq!(sim.get(o), expected);
        }
    }

    fn wide_ternary_narrow_branch_no_spill(sim) {
        @build Design::new(r#"
    module Top (
        sel:  input  logic     ,
        data: input  logic<64> ,
        o:    output logic<256>,
    ) {
        assign o = if sel ? {data, {1'b0 repeat 72}} : 256'd0;
    }
    "#, "Top");
        let sel = sim.signal("sel"); let data = sim.signal("data"); let o = sim.signal("o");
        for value in [0u64, 1, 0xf7f6_f5f4_f3f2_f1f0, u64::MAX] {
            for selected in [0u8, 1, 0] {
                sim.modify(|io| { io.set(sel, selected); io.set(data, value); }).unwrap();
                let expected = if selected == 1 { BigUint::from(value) << 72usize } else { BigUint::default() };
                assert_eq!(sim.get(o), expected);
            }
        }
    }

    fn nested_array_index_const_array(sim) {
        @build Design::new(r#"
    module Top (
        idx:    input  logic<8>,
        nested: output logic<8>,
        inner:  output logic<8>,
    ) {
        const A:   logic<8> [2] = '{1, 3};
        var   mem: logic<8> [8];
        always_comb {
            mem[0] = 0;
            mem[1] = 11;
            mem[2] = 22;
            mem[3] = 33;
            mem[4] = 44;
            mem[5] = 55;
            mem[6] = 66;
            mem[7] = 77;
        }
        assign inner  = A[idx];
        assign nested = mem[A[idx]];
    }
    "#, "Top");
        let idx = sim.signal("idx"); let inner = sim.signal("inner"); let nested = sim.signal("nested");
        for index in [1u8, 0, 1, 0] {
            sim.modify(|io| io.set(idx, index)).unwrap();
            let mapped = if index == 0 { 1u8 } else { 3u8 };
            assert_eq!(sim.get(inner), mapped.into());
            assert_eq!(sim.get(nested), (mapped * 11).into());
        }
    }

    fn inst_port_default_value_connected_not_folded(sim) {
        @build Design::new(r#"
    module Sub (
        i_cfg : input  logic<5>  = 5'd0,
        o_mask: output logic<16>,
    ) {
        assign o_mask = (16'd1 << i_cfg) - 16'd1;
    }

    module Top (
        o_conn: output logic<16>,
        o_open: output logic<16>,
    ) {
        inst s_conn: Sub (
            i_cfg : 5'd2  ,
            o_mask: o_conn,
        );
        inst s_open: Sub (
            o_mask: o_open,
        );
    }
    "#, "Top");
        assert_eq!(sim.get(sim.signal("o_conn")), 3u16.into());
        assert_eq!(sim.get(sim.signal("o_open")), 0u16.into());
    }

    fn inlined_function_per_callsite_scratch_in_continuous_assign(sim) {
        @build Design::new(r#"
    module Top (
        x: input  logic<32>,
        y: input  logic<32>,
        o: output logic<12>,
    ) {
        function clz32 (
            val: input logic<32>,
        ) -> logic<6> {
            var ret: logic<6>;
            ret = 6'd32;
            for i in 0..32 {
                if val[31 - i] == 1'b1 && ret == 6'd32 {
                    ret = i as 6;
                }
            }
            return ret;
        }
        assign o = {clz32(x), clz32(y)};
    }
    "#, "Top");
        let x = sim.signal("x"); let y = sim.signal("y"); let o = sim.signal("o");
        for (left, right) in [(0x0080_0000u32, 0x10u32), (0x10, 0x0080_0000), (0, u32::MAX), (1, 0), (0x8000_0000, 0x8000_0000)] {
            sim.modify(|io| { io.set(x, left); io.set(y, right); }).unwrap();
            let expected = (left.leading_zeros() << 6) | right.leading_zeros();
            assert_eq!(sim.get(o), expected.into());
        }
    }

    fn wide_shift_count_preserves_unknowns_and_sign_fill(sim) {
        @build Design::new(r#"
module Top (
    value: input logic<130>,
    signed_value: input signed logic<8>,
    amount: input logic<70>,
    left: output logic<130>,
    right: output logic<130>,
    arithmetic: output signed logic<8>,
) {
    assign left = value << amount;
    assign right = value >> amount;
    assign arithmetic = signed_value >>> amount;
}
"#, "Top").four_state(true);
        let value = sim.signal("value");
        let signed_value = sim.signal("signed_value");
        let amount = sim.signal("amount");
        let left = sim.signal("left");
        let right = sim.signal("right");
        let arithmetic = sim.signal("arithmetic");
        let all = (BigUint::from(1u8) << 130usize) - 1u8;
        let high = BigUint::from(1u8) << 129usize;
        for payload in [BigUint::from(0x55u8), &high | BigUint::from(0x55u8)] {
            for count in [BigUint::from(0u8), 1u8.into(), 129u8.into(), 130u8.into(),
                          256u16.into(), (BigUint::from(1u8) << 64usize) + 1u8] {
                sim.modify(|io| { io.set_wide(value, payload.clone());
                    io.set(signed_value, 0x85u8); io.set_wide(amount, count.clone()); }).unwrap();
                let shift = count.to_u32_digits();
                let shift = shift.first().copied().unwrap_or(0) as usize;
                let oversized = count >= BigUint::from(130u8);
                let (expected_left, expected_right) = if oversized {
                    (BigUint::default(), BigUint::default())
                } else {
                    ((&payload << shift) & &all, &payload >> shift)
                };
                let arithmetic_shift = if count >= BigUint::from(8u8) { 7 } else { shift };
                let expected_arithmetic = BigUint::from(((-123i8) >> arithmetic_shift) as u8);
                for (output, expected) in [(left, expected_left), (right, expected_right), (arithmetic, expected_arithmetic)] {
                    assert_eq!(sim.get_four_state(output), (expected, BigUint::default()), "count={count}");
                }
            }
            for low in [BigUint::default(), BigUint::from(u64::MAX)] {
                for unknown_bit in [2usize, 64, 69] {
                    let mask = BigUint::from(1u8) << unknown_bit;
                    for unknown_payload in [BigUint::default(), mask.clone()] {
                        sim.modify(|io| {
                            io.set_wide(value, payload.clone());
                            io.set(signed_value, 0x85u8);
                            io.set_four_state(amount, (&low & ((BigUint::from(1u8) << 70usize) - 1u8 ^ &mask)) | unknown_payload.clone(), mask.clone());
                        }).unwrap();
                        for (output, unknown) in [(left, all.clone()), (right, all.clone()), (arithmetic, BigUint::from(255u8))] {
                            assert_eq!(sim.get_four_state(output), (unknown.clone(), unknown), "unknown count bit={unknown_bit}, low={low}, output={output:?}");
                        }
                    }
                }
            }
        }
    }

    fn constant_arrays_initialize_comb_and_ff_reads(sim) {
        @build Design::new(r#"
module Top (
    clk: input clock,
    index: input logic<2>,
    comb: output logic<3>,
    registered: output logic<3>,
    repeated: output logic<3>,
    nested: output logic<3>,
    known: output bit<3>,
) {
    const TABLE: logic<3> [4] = '{3'b111, 3'b010, 3'bx01, 3'bz10};
    const REPEATED: logic<3> [4] = '{default: 3'b101};
    const NESTED: logic<3> [2, 2] = '{'{1, 2}, '{3, 4}};
    const KNOWN: bit<3> [4] = '{0, 1, 2, 7};
    assign comb = TABLE[index];
    assign repeated = REPEATED[index];
    assign nested = NESTED[index[1]][index[0]];
    assign known = KNOWN[index];
    always_ff (clk) { registered = TABLE[index]; }
}
"#, "Top").four_state(true);
        let index = sim.signal("index");
        let comb = sim.signal("comb");
        let registered = sim.signal("registered");
        let repeated = sim.signal("repeated");
        let nested = sim.signal("nested");
        let known = sim.signal("known");
        let clk = sim.event("clk");
        for idx in [3u8, 0, 2, 1, 3, 2, 0] {
            sim.modify(|io| io.set(index, idx)).unwrap();
            let (payload, mask) = [(7u8, 0u8), (2, 0), (5, 4), (2, 4)][idx as usize];
            let expected = (BigUint::from(payload), BigUint::from(mask));
            assert_eq!(sim.get_four_state(comb), expected);
            assert_eq!(sim.get_four_state(repeated), (5u8.into(), 0u8.into()));
            assert_eq!(sim.get_four_state(nested), ((idx + 1).into(), 0u8.into()));
            assert_eq!(sim.get_four_state(known), ([0u8, 1, 2, 7][idx as usize].into(), 0u8.into()));
            sim.tick(clk).unwrap();
            assert_eq!(sim.get_four_state(registered), expected);
        }
    }
}
