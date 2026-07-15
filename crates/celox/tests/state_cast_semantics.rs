use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn type_cast_clears_unknown_bits_in_comb_and_ff(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    narrow: input logic<8>,
    wide: input logic<130>,

    c_u8: output logic<8>,
    c_i8: output logic<8>,
    f_u8: output logic<8>,
    f_i8: output logic<8>,
    c_wide: output logic<130>,
    f_wide: output logic<130>,
) {
    type U130 = bit<130>;

    assign c_u8 = narrow as u8;
    assign c_i8 = narrow as i8;
    assign c_wide = wide as U130;

    always_ff (clk) {
        f_u8 = narrow as u8;
        f_i8 = narrow as i8;
        f_wide = wide as U130;
    }
}
"#, "Top").four_state(true);

        let narrow_value = BigUint::from(0xa9u8);
        let narrow_mask = BigUint::from(0x0cu8);

        let full = (BigUint::from(1u8) << 130usize) - BigUint::from(1u8);
        let wide_mask = (BigUint::from(1u8) << 5usize)
            | (BigUint::from(1u8) << 70usize)
            | (BigUint::from(1u8) << 129usize);
        let wide_value = (BigUint::from(1u8) << 129usize)
            | (BigUint::from(1u8) << 64usize)
            | (BigUint::from(1u8) << 5usize)
            | BigUint::from(0x35u8);
        let narrow = sim.signal("narrow");
        let wide = sim.signal("wide");
        let clk = sim.event("clk");

        sim.modify(|io| {
            io.set_four_state(narrow, narrow_value.clone(), narrow_mask.clone());
            io.set_four_state(wide, wide_value.clone(), wide_mask.clone());
        })
        .unwrap();
        assert_eq!(
            sim.get_four_state(wide),
            (wide_value.clone(), wide_mask.clone()),
            "wide test input"
        );
        let expected_wide = &wide_value & (&full ^ &wide_mask);
        assert_eq!(
            sim.get_four_state(sim.signal("c_wide")),
            (expected_wide.clone(), BigUint::from(0u8)),
            "c_wide before clock"
        );
        sim.tick(clk).unwrap();

        let expected_narrow = &narrow_value & (&BigUint::from(0xffu8) ^ &narrow_mask);
        for name in ["c_u8", "c_i8", "f_u8", "f_i8"] {
            assert_eq!(
                sim.get_four_state(sim.signal(name)),
                (expected_narrow.clone(), BigUint::from(0u8)),
                "{name}"
            );
        }

        for name in ["c_wide", "f_wide"] {
            assert_eq!(
                sim.get_four_state(sim.signal(name)),
                (expected_wide.clone(), BigUint::from(0u8)),
                "{name}"
            );
        }
    }

    fn constant_type_cast_clears_unknown_bits(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    c_compound: output logic<8>,
    c_variable: output logic<8>,
    f_compound: output logic<8>,
    f_variable: output logic<8>,
) {
    const X: logic<8> = 8'b10xz_01zx;
    const U: u8 = X as u8;

    assign c_compound = X as u8;
    assign c_variable = U;
    always_ff (clk) {
        f_compound = X as u8;
        f_variable = U;
    }
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        sim.tick(clk).unwrap();
        for name in ["c_compound", "c_variable", "f_compound", "f_variable"] {
            assert_eq!(
                sim.get_four_state(sim.signal(name)),
                (BigUint::from(0x84u8), BigUint::from(0u8)),
                "{name}"
            );
        }
    }

    fn function_formal_type_clears_unknown_bits(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    a: input logic<8>,
    c: output logic<8>,
    f: output logic<8>,
) {
    function through (x: input bit<8>) -> logic<8> {
        return x;
    }
    assign c = through(a);
    always_ff (clk) {
        f = through(a);
    }
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        let a = sim.signal("a");
        sim.modify(|io| {
            io.set_four_state(
                a,
                BigUint::from(0xa9u8),
                BigUint::from(0x0cu8),
            );
        })
        .unwrap();
        sim.tick(clk).unwrap();
        for name in ["c", "f"] {
            assert_eq!(
                sim.get_four_state(sim.signal(name)),
                (BigUint::from(0xa1u8), BigUint::from(0u8)),
                "{name}"
            );
        }
    }

    fn signed_four_state_function_formal_preserves_unknown_bits(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    a: input logic<8>,
    c: output logic<8>,
    f: output logic<8>,
) {
    function through (x: input signed logic<8>) -> logic<8> {
        return x;
    }
    assign c = through(a);
    always_ff (clk) {
        f = through(a);
    }
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        let a = sim.signal("a");
        sim.modify(|io| {
            io.set_four_state(
                a,
                BigUint::from(0xa9u8),
                BigUint::from(0x0cu8),
            );
        })
        .unwrap();
        sim.tick(clk).unwrap();
        for name in ["c", "f"] {
            let signal = sim.signal(name);
            assert_eq!(
                sim.get_four_state(signal),
                (BigUint::from(0xa9u8), BigUint::from(0x0cu8)),
                "{name}"
            );
        }
    }

    fn implicit_assignment_to_bit_clears_unknowns_without_mutating_source(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    narrow: input logic<8>,
    wide: input logic<130>,
    c_narrow_bit: output bit<8>,
    c_narrow_after: output logic<8>,
    c_wide_bit: output bit<130>,
    c_wide_after: output logic<130>,
    f_narrow_bit: output bit<8>,
    f_narrow_after: output logic<8>,
    f_wide_bit: output bit<130>,
    f_wide_after: output logic<130>,
) {
    var narrow_source: logic<8>;
    var wide_source: logic<130>;
    always_comb {
        narrow_source = narrow;
        wide_source = wide;
        c_narrow_bit = narrow_source;
        c_wide_bit = wide_source;
        c_narrow_after = narrow_source;
        c_wide_after = wide_source;
    }
    always_ff (clk) {
        f_narrow_bit = narrow;
        f_wide_bit = wide;
        f_narrow_after = narrow;
        f_wide_after = wide;
    }
}
"#, "Top").four_state(true);

        let narrow_value = BigUint::from(0xa9u8);
        let narrow_mask = BigUint::from(0x0cu8);
        let wide_mask = (BigUint::from(1u8) << 5usize)
            | (BigUint::from(1u8) << 70usize)
            | (BigUint::from(1u8) << 129usize);
        let wide_value = (BigUint::from(1u8) << 129usize)
            | (BigUint::from(1u8) << 64usize)
            | (BigUint::from(1u8) << 5usize)
            | BigUint::from(0x35u8);
        let full_wide = (BigUint::from(1u8) << 130usize) - BigUint::from(1u8);
        let expected_narrow = &narrow_value & (BigUint::from(0xffu8) ^ &narrow_mask);
        let expected_wide = &wide_value & (&full_wide ^ &wide_mask);

        let narrow = sim.signal("narrow");
        let wide = sim.signal("wide");
        let clk = sim.event("clk");
        sim.modify(|io| {
            io.set_four_state(narrow, narrow_value.clone(), narrow_mask.clone());
            io.set_four_state(wide, wide_value.clone(), wide_mask.clone());
        })
        .unwrap();
        sim.tick(clk).unwrap();

        for name in ["c_narrow_bit", "f_narrow_bit"] {
            let signal = sim.signal(name);
            assert_eq!(
                sim.get_four_state(signal),
                (expected_narrow.clone(), BigUint::from(0u8)),
                "{name}"
            );
        }
        for name in ["c_wide_bit", "f_wide_bit"] {
            let signal = sim.signal(name);
            assert_eq!(
                sim.get_four_state(signal),
                (expected_wide.clone(), BigUint::from(0u8)),
                "{name}"
            );
        }
        for name in ["c_narrow_after", "f_narrow_after"] {
            let signal = sim.signal(name);
            assert_eq!(
                sim.get_four_state(signal),
                (narrow_value.clone(), narrow_mask.clone()),
                "{name}"
            );
        }
        for name in ["c_wide_after", "f_wide_after"] {
            let signal = sim.signal(name);
            assert_eq!(
                sim.get_four_state(signal),
                (wide_value.clone(), wide_mask.clone()),
                "{name}"
            );
        }
    }
}

#[test]
fn state_cast_is_explicit_in_sir() {
    let result = Simulator::builder(
        r#"
module Top (
    a: input logic<130>,
    y: output logic<130>,
) {
    type U130 = bit<130>;
    assign y = a as U130;
}
"#,
        "Top",
    )
    .four_state(true)
    .trace_post_optimized_sir()
    .build_with_trace();
    let sir = result
        .trace
        .format_post_optimized_sir()
        .expect("post-optimized SIR");
    assert!(
        sir.contains("ToTwoState"),
        "the state-conversion boundary disappeared from SIR:\n{sir}"
    );
}
