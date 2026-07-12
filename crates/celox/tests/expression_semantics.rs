use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn cast_binary_semantics_match_between_comb_and_ff(sim) {
        // Veryl 0.20.2's runtime loses signedness across a numeric size cast,
        // unlike SystemVerilog 6.24.1 and the SystemVerilog Veryl emits.
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    a5: input signed logic<5>,
    b8: input i8,
    ub: input logic<8>,

    c_num_cast: output logic<16>,
    c_u_cast: output logic<16>,
    c_i_cast: output logic<16>,
    c_num_mul: output logic<16>,
    c_num_div: output logic<16>,
    c_num_rem: output logic<16>,
    c_u_mul: output logic<16>,
    c_i_mul: output logic<16>,
    c_i_div: output logic<16>,
    c_i_rem: output logic<16>,
    c_mixed_div: output logic<16>,
    c_mixed_rem: output logic<16>,
    c_num_lt: output logic,
    c_i_lt: output logic,
    c_i_lt_wide: output logic<16>,
    c_mixed_lt: output logic,

    f_num_cast: output logic<16>,
    f_u_cast: output logic<16>,
    f_i_cast: output logic<16>,
    f_num_mul: output logic<16>,
    f_num_div: output logic<16>,
    f_num_rem: output logic<16>,
    f_u_mul: output logic<16>,
    f_i_mul: output logic<16>,
    f_i_div: output logic<16>,
    f_i_rem: output logic<16>,
    f_mixed_div: output logic<16>,
    f_mixed_rem: output logic<16>,
    f_num_lt: output logic,
    f_i_lt: output logic,
    f_i_lt_wide: output logic<16>,
    f_mixed_lt: output logic,
) {
    // Resizing a cast uses the source signedness. A numeric size cast preserves
    // that signedness, while a type cast takes the target type's signedness.
    assign c_num_cast = a5 as 8;
    assign c_u_cast = a5 as u8;
    assign c_i_cast = a5 as i8;
    assign c_num_mul = (a5 as 8) * (b8 as 8);
    assign c_num_div = (a5 as 8) / (b8 as 8);
    assign c_num_rem = (a5 as 8) % (b8 as 8);
    assign c_u_mul = (a5 as u8) * (b8 as u8);
    assign c_i_mul = (a5 as i8) * (b8 as i8);
    assign c_i_div = (a5 as i8) / (b8 as i8);
    assign c_i_rem = (a5 as i8) % (b8 as i8);
    assign c_mixed_div = a5 / ub;
    assign c_mixed_rem = a5 % ub;
    assign c_num_lt = (a5 as 8) <: (b8 as 8);
    assign c_i_lt = (a5 as i8) <: (b8 as i8);
    assign c_i_lt_wide = (a5 as i8) <: (b8 as i8);
    assign c_mixed_lt = a5 <: ub;

    always_ff (clk) {
        f_num_cast = a5 as 8;
        f_u_cast = a5 as u8;
        f_i_cast = a5 as i8;
        f_num_mul = (a5 as 8) * (b8 as 8);
        f_num_div = (a5 as 8) / (b8 as 8);
        f_num_rem = (a5 as 8) % (b8 as 8);
        f_u_mul = (a5 as u8) * (b8 as u8);
        f_i_mul = (a5 as i8) * (b8 as i8);
        f_i_div = (a5 as i8) / (b8 as i8);
        f_i_rem = (a5 as i8) % (b8 as i8);
        f_mixed_div = a5 / ub;
        f_mixed_rem = a5 % ub;
        f_num_lt = (a5 as 8) <: (b8 as 8);
        f_i_lt = (a5 as i8) <: (b8 as i8);
        f_i_lt_wide = (a5 as i8) <: (b8 as i8);
        f_mixed_lt = a5 <: ub;
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let a5 = sim.signal("a5");
        let b8 = sim.signal("b8");
        let ub = sim.signal("ub");

        sim.modify(|io| {
            io.set(a5, 0x19u8); // -7 in 5 bits
            io.set(b8, 0x02u8);
            io.set(ub, 0x02u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();

        let expected = [
            ("num_cast", 0xfff9u16),
            ("u_cast", 0x00f9u16),
            ("i_cast", 0xfff9u16),
            ("num_mul", 0xfff2u16),
            ("num_div", 0xfffdu16),
            ("num_rem", 0xffffu16),
            ("u_mul", 0x01f2u16),
            ("i_mul", 0xfff2u16),
            ("i_div", 0xfffdu16),
            ("i_rem", 0xffffu16),
            ("mixed_div", 0x000cu16),
            ("mixed_rem", 0x0001u16),
        ];
        for (suffix, value) in expected {
            for prefix in ["c", "f"] {
                let name = format!("{prefix}_{suffix}");
                assert_eq!(sim.get(sim.signal(&name)), value.into(), "{name}");
            }
        }

        let expected_predicates = [("num_lt", 1u8), ("i_lt", 1u8), ("mixed_lt", 0u8)];
        for (suffix, value) in expected_predicates {
            for prefix in ["c", "f"] {
                let name = format!("{prefix}_{suffix}");
                assert_eq!(sim.get(sim.signal(&name)), value.into(), "{name}");
            }
        }
        for name in ["c_i_lt_wide", "f_i_lt_wide"] {
            assert_eq!(sim.get(sim.signal(name)), 1u16.into(), "{name}");
        }
    }

    fn parent_context_and_self_determined_boundaries_match_between_comb_and_ff(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    a: input i8,
    b: input i8,
    u: input u8,
    c_div: output logic<8>,
    c_sar: output logic<8>,
    c_lt: output logic<8>,
    f_div: output logic<8>,
    f_sar: output logic<8>,
    f_lt: output logic<8>,
) {
    // Arithmetic operands inherit the unsigned context introduced by `u`.
    // Comparisons are self-determined, so their signed operand comparison is
    // unaffected by the unsigned sibling and their one-bit result zero-extends.
    assign c_div = (a / b) + u;
    assign c_sar = (a >>> 1) + u;
    assign c_lt = (a <: b) + u;
    always_ff (clk) {
        f_div = (a / b) + u;
        f_sar = (a >>> 1) + u;
        f_lt = (a <: b) + u;
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let a = sim.signal("a");
        let b = sim.signal("b");
        let u = sim.signal("u");
        sim.modify(|io| {
            io.set(a, 0xf9u8); // -7 as i8, 249 as u8
            io.set(b, 0x02u8);
            io.set(u, 0u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();

        for (suffix, expected) in [("div", 124u8), ("sar", 124u8), ("lt", 1u8)] {
            for prefix in ["c", "f"] {
                let name = format!("{prefix}_{suffix}");
                assert_eq!(sim.get(sim.signal(&name)), expected.into(), "{name}");
            }
        }
    }

    #[ignore = "Veryl 0.20.2 folds signed numeric casts before Celox receives AIR"]
    fn constant_and_runtime_casts_use_the_same_resize_rule(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    a: input signed logic<5>,
    c_const_cast: output logic<16>,
    c_const_div: output logic<16>,
    c_const_rem: output logic<16>,
    c_const_lt: output logic,
    c_runtime_cast: output logic<16>,
    c_runtime_div: output logic<16>,
    c_runtime_rem: output logic<16>,
    c_runtime_lt: output logic,
    f_const_cast: output logic<16>,
    f_const_div: output logic<16>,
    f_const_rem: output logic<16>,
    f_const_lt: output logic,
    f_runtime_cast: output logic<16>,
    f_runtime_div: output logic<16>,
    f_runtime_rem: output logic<16>,
    f_runtime_lt: output logic,
) {
    const A: signed logic<5> = -7;
    assign c_const_cast = A as 8;
    assign c_const_div = (A as 8) / (2 as 8);
    assign c_const_rem = (A as 8) % (2 as 8);
    assign c_const_lt = (A as 8) <: (2 as 8);
    assign c_runtime_cast = a as 8;
    assign c_runtime_div = (a as 8) / (2 as 8);
    assign c_runtime_rem = (a as 8) % (2 as 8);
    assign c_runtime_lt = (a as 8) <: (2 as 8);
    always_ff (clk) {
        f_const_cast = A as 8;
        f_const_div = (A as 8) / (2 as 8);
        f_const_rem = (A as 8) % (2 as 8);
        f_const_lt = (A as 8) <: (2 as 8);
        f_runtime_cast = a as 8;
        f_runtime_div = (a as 8) / (2 as 8);
        f_runtime_rem = (a as 8) % (2 as 8);
        f_runtime_lt = (a as 8) <: (2 as 8);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x19u8)).unwrap(); // -7 in 5 bits
        sim.tick(clk).unwrap();

        for (suffix, expected) in [
            ("cast", 0xfff9u16),
            ("div", 0xfffdu16),
            ("rem", 0xffffu16),
        ] {
            for prefix in ["c_const", "c_runtime", "f_const", "f_runtime"] {
                let name = format!("{prefix}_{suffix}");
                assert_eq!(sim.get(sim.signal(&name)), expected.into(), "{name}");
            }
        }
        for prefix in ["c_const", "c_runtime", "f_const", "f_runtime"] {
            let name = format!("{prefix}_lt");
            assert_eq!(sim.get(sim.signal(&name)), 1u8.into(), "{name}");
        }
    }

    fn folded_and_runtime_builtin_selects_are_unsigned(sim) {
        @build Simulator::builder(r#"
module Top (
    a: input signed logic<8>,
    const_part: output logic<16>,
    runtime_part: output logic<16>,
) {
    const VALUE: signed logic<8> = 8'sh8f;
    assign const_part = VALUE[3:0];
    assign runtime_part = a[3:0];
}
"#, "Top");

        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x8fu8)).unwrap();
        for name in ["const_part", "runtime_part"] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), 0x000fu16.into(), "{name}");
        }
    }

    fn wildcard_predicates_remain_one_bit_in_ternaries_and_concats(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    sel: input logic,
    a: input logic<8>,
    b: input logic<8>,
    c: output logic<2>,
    f: output logic<2>,
) {
    assign c = {1'b1, (if sel ? (a ==? b) : 1'b0)};
    always_ff (clk) {
        f = {1'b1, (if sel ? (a ==? b) : 1'b0)};
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let sel = sim.signal("sel");
        let a = sim.signal("a");
        let b = sim.signal("b");
        sim.modify(|io| {
            io.set(sel, 1u8);
            io.set(a, 0x5au8);
            io.set(b, 0x5au8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        for name in ["c", "f"] {
            assert_eq!(sim.get(sim.signal(name)), 3u8.into(), "{name}");
        }
    }

    fn function_actuals_are_converted_at_the_formal_boundary(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    actual: input logic<8>,
    c: output logic<9>,
    f: output logic<9>,
) {
    function pack (x: input logic<5>) -> logic<9> {
        return {1'b1, x};
    }
    assign c = pack(actual);
    always_ff (clk) {
        f = pack(actual);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let actual = sim.signal("actual");
        sim.modify(|io| io.set(actual, 0xe1u8)).unwrap();
        sim.tick(clk).unwrap();
        for name in ["c", "f"] {
            assert_eq!(sim.get(sim.signal(name)), 0x21u16.into(), "{name}");
        }
    }

    fn unary_expression_context_signedness_matches_veryl(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    signed_value: input signed logic<8>,
    signed_minus_value: input signed logic<8>,
    signed_divisor: input signed logic<16>,
    signed_small_divisor: input signed logic<8>,
    unsigned_value: input logic<8>,

    c_not_wide: output logic<16>,
    c_not_lt: output logic,
    c_not_div: output logic<16>,
    c_unsigned_minus: output logic<16>,
    c_signed_minus: output logic<16>,
    c_unsigned_minus_lt: output logic,
    c_unsigned_minus_div: output logic<8>,
    f_not_wide: output logic<16>,
    f_not_lt: output logic,
    f_not_div: output logic<16>,
    f_unsigned_minus: output logic<16>,
    f_signed_minus: output logic<16>,
    f_unsigned_minus_lt: output logic,
    f_unsigned_minus_div: output logic<8>,
) {
    // Unary +, -, and bitwise-not preserve the operand's expression-context
    // signedness. Assignment width reaches the unary operand before evaluation;
    // the comparison and division below distinguish signedness at a
    // self-determined operator boundary. This is separate from result type metadata.
    assign c_not_wide = ~signed_value;
    assign c_not_lt = (~signed_value) <: signed_divisor;
    assign c_not_div = (~signed_value) / signed_divisor;
    assign c_unsigned_minus = -unsigned_value;
    assign c_signed_minus = -signed_minus_value;
    assign c_unsigned_minus_lt = (-unsigned_value) <: signed_small_divisor;
    assign c_unsigned_minus_div = (-unsigned_value) / signed_small_divisor;
    always_ff (clk) {
        f_not_wide = ~signed_value;
        f_not_lt = (~signed_value) <: signed_divisor;
        f_not_div = (~signed_value) / signed_divisor;
        f_unsigned_minus = -unsigned_value;
        f_signed_minus = -signed_minus_value;
        f_unsigned_minus_lt = (-unsigned_value) <: signed_small_divisor;
        f_unsigned_minus_div = (-unsigned_value) / signed_small_divisor;
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let signed_value = sim.signal("signed_value");
        let signed_minus_value = sim.signal("signed_minus_value");
        let signed_divisor = sim.signal("signed_divisor");
        let signed_small_divisor = sim.signal("signed_small_divisor");
        let unsigned_value = sim.signal("unsigned_value");
        sim.modify(|io| {
            io.set(signed_value, 0u8);
            io.set(signed_minus_value, 1u8);
            io.set(signed_divisor, 0xffffu16); // -1 as signed logic<16>
            io.set(signed_small_divisor, 2u8);
            io.set(unsigned_value, 1u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();

        for prefix in ["c", "f"] {
            let not_wide = sim.signal(&format!("{prefix}_not_wide"));
            let not_lt = sim.signal(&format!("{prefix}_not_lt"));
            let not_div = sim.signal(&format!("{prefix}_not_div"));
            let unsigned_minus = sim.signal(&format!("{prefix}_unsigned_minus"));
            let signed_minus = sim.signal(&format!("{prefix}_signed_minus"));
            let unsigned_minus_lt = sim.signal(&format!("{prefix}_unsigned_minus_lt"));
            let unsigned_minus_div = sim.signal(&format!("{prefix}_unsigned_minus_div"));
            assert_eq!(
                sim.get(not_wide),
                0xffffu16.into()
            );
            assert_eq!(sim.get(not_lt), 0u8.into());
            assert_eq!(sim.get(not_div), 1u16.into());
            assert_eq!(
                sim.get(unsigned_minus),
                0xffffu16.into()
            );
            assert_eq!(sim.get(signed_minus), 0xffffu16.into());
            assert_eq!(sim.get(unsigned_minus_lt), 0u8.into());
            assert_eq!(sim.get(unsigned_minus_div), 127u8.into());
        }
    }

    fn signed_type_cast_keeps_comparison_operands_signed(sim) {
        // Veryl simulator 0.20.2 loses the signed result of the same-width
        // `as i8` while lowering this comparison. `~a` itself is not the
        // failure: comparing it with a signed variable works there.
        @ignore_on(veryl);
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    a: input signed logic<8>,
    c_lt: output logic,
    f_lt: output logic,
) {
    assign c_lt = (~a) <: (1 as i8);
    always_ff (clk) {
        f_lt = (~a) <: (1 as i8);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0u8)).unwrap();
        sim.tick(clk).unwrap();

        // ~8'sh00 is signed 8'shff (-1), therefore it is less than +1.
        for name in ["c_lt", "f_lt"] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), 1u8.into(), "{name}");
        }
    }

    fn aggregate_results_consume_the_unary_parent_context(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    c_concat: output logic<8>,
    c_struct: output logic<8>,
    f_concat: output logic<8>,
    f_struct: output logic<8>,
) {
    struct Nibble {
        value: logic<4>,
    }
    assign c_concat = ~{4'b0000};
    assign c_struct = ~Nibble'{value: 4'b0000};
    always_ff (clk) {
        f_concat = ~{4'b0000};
        f_struct = ~Nibble'{value: 4'b0000};
    }
}
"#, "Top");

        let clk = sim.event("clk");
        sim.tick(clk).unwrap();
        for name in ["c_concat", "c_struct", "f_concat", "f_struct"] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), 0xffu8.into(), "{name}");
        }
    }

    fn system_function_results_obey_ternary_width_contexts(sim) {
        // Veryl 0.20.2 leaves these calls unresolved in its simulator IR.
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    sel: input logic,
    a: input logic<5>,
    signed_arm: input signed logic<8>,
    wide_arm: input logic<40>,
    c_natural: output logic<32>,
    c_wide: output logic<40>,
    f_natural: output logic<32>,
    f_wide: output logic<40>,
) {
    assign c_natural = if sel ? $bits(a) : signed_arm;
    assign c_wide = if sel ? $bits(a) : wide_arm;
    always_ff (clk) {
        f_natural = if sel ? $bits(a) : signed_arm;
        f_wide = if sel ? $bits(a) : wide_arm;
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let sel = sim.signal("sel");
        let signed_arm = sim.signal("signed_arm");
        let wide_arm = sim.signal("wide_arm");
        sim.modify(|io| {
            io.set(sel, 0u8);
            io.set(signed_arm, 0xffu8);
            io.set(wide_arm, 0x3456_789au32);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        for prefix in ["c", "f"] {
            let natural = sim.signal(&format!("{prefix}_natural"));
            let wide = sim.signal(&format!("{prefix}_wide"));
            assert_eq!(
                sim.get(natural),
                0xffu32.into()
            );
            assert_eq!(
                sim.get(wide),
                BigUint::from(0x3456_789au32)
            );
        }

        sim.modify(|io| io.set(sel, 1u8)).unwrap();
        sim.tick(clk).unwrap();
        for prefix in ["c", "f"] {
            let natural = sim.signal(&format!("{prefix}_natural"));
            let wide = sim.signal(&format!("{prefix}_wide"));
            assert_eq!(sim.get(natural), 5u32.into());
            assert_eq!(sim.get(wide), 5u64.into());
        }
    }

    fn short_circuit_operators_skip_effectful_operands(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    and_state: output logic,
    or_state: output logic,
    ternary_then_state: output logic,
    ternary_else_state: output logic,
    and_result: output logic,
    or_result: output logic,
    true_result: output logic,
    false_result: output logic,
    and_followup: output logic,
    or_followup: output logic,
    ternary_then_followup: output logic,
    ternary_else_followup: output logic,
) {
    var and_side_effect: logic;
    var or_side_effect: logic;
    var ternary_then_side_effect: logic;
    var ternary_else_side_effect: logic;
    function set_side_effect (y: output logic) -> logic {
        y = 1'b1;
        return 1'b1;
    }
    always_ff (clk) {
        and_result = 1'b0 && set_side_effect(and_side_effect);
        or_result = 1'b1 || set_side_effect(or_side_effect);
        true_result = if 1'b1 ? 1'b0 : set_side_effect(ternary_else_side_effect);
        false_result = if 1'b0 ? set_side_effect(ternary_then_side_effect) : 1'b0;
        and_followup = and_side_effect;
        or_followup = or_side_effect;
        ternary_then_followup = ternary_then_side_effect;
        ternary_else_followup = ternary_else_side_effect;
    }
    assign and_state = and_side_effect;
    assign or_state = or_side_effect;
    assign ternary_then_state = ternary_then_side_effect;
    assign ternary_else_state = ternary_else_side_effect;
}
"#, "Top");

        let clk = sim.event("clk");
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sim.signal("and_state")), 0u8.into());
        assert_eq!(sim.get(sim.signal("or_state")), 0u8.into());
        assert_eq!(sim.get(sim.signal("ternary_then_state")), 0u8.into());
        assert_eq!(sim.get(sim.signal("ternary_else_state")), 0u8.into());
        assert_eq!(sim.get(sim.signal("and_result")), 0u8.into());
        assert_eq!(sim.get(sim.signal("or_result")), 1u8.into());
        assert_eq!(sim.get(sim.signal("true_result")), 0u8.into());
        assert_eq!(sim.get(sim.signal("false_result")), 0u8.into());
        for name in [
            "and_followup",
            "or_followup",
            "ternary_then_followup",
            "ternary_else_followup",
        ] {
            assert_eq!(sim.get(sim.signal(name)), 0u8.into(), "{name}");
        }
    }

    fn numeric_cast_preserves_four_state_sign_extension(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    a: input signed logic<5>,
    c_num: output logic<8>,
    f_num: output logic<8>,
) {
    assign c_num = a as 8;
    always_ff (clk) {
        f_num = a as 8;
    }
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        let a = sim.signal("a");

        // X in the signed source's sign bit is replicated while resizing to
        // eight bits. Numeric `as 8` remains a four-state logic value.
        sim.modify(|io| {
            io.set_four_state(a, BigUint::from(0x11u8), BigUint::from(0x10u8));
        })
        .unwrap();
        sim.tick(clk).unwrap();
        for name in ["c_num", "f_num"] {
            assert_eq!(
                sim.get_four_state(sim.signal(name)),
                (BigUint::from(0xf1u8), BigUint::from(0xf0u8)),
                "{name} with X sign bit"
            );
        }

        // Z uses value=0/mask=1. Its sign extension therefore only extends
        // the mask; it must not be silently converted to a two-state value.
        sim.modify(|io| {
            io.set_four_state(a, BigUint::from(0x01u8), BigUint::from(0x10u8));
        })
        .unwrap();
        sim.tick(clk).unwrap();
        for name in ["c_num", "f_num"] {
            assert_eq!(
                sim.get_four_state(sim.signal(name)),
                (BigUint::from(0x01u8), BigUint::from(0xf0u8)),
                "{name} with Z sign bit"
            );
        }
    }

    fn narrow_signed_comparison_sign_extends_both_operands(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    a: input signed logic<5>,
    b: input signed logic<5>,
    c_lt: output logic,
    c_gt: output logic,
    f_lt: output logic,
    f_gt: output logic,
) {
    assign c_lt = a <: b;
    assign c_gt = a >: b;
    always_ff (clk) {
        f_lt = a <: b;
        f_gt = a >: b;
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let a = sim.signal("a");
        let b = sim.signal("b");
        sim.modify(|io| {
            io.set(a, 1u8);
            io.set(b, 0x1fu8); // -1 in signed logic<5>
        })
        .unwrap();
        sim.tick(clk).unwrap();

        for name in ["c_lt", "f_lt"] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), 0u8.into(), "{name}");
        }
        for name in ["c_gt", "f_gt"] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), 1u8.into(), "{name}");
        }
    }
}
