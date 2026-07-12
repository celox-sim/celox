use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn logical_unknown_truth_table_matches_comb_and_ff(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    u: input logic,

    c_and_one: output logic,
    c_or_zero: output logic,
    c_zero_and: output logic,
    c_one_or: output logic,
    f_and_one: output logic,
    f_or_zero: output logic,
    f_zero_and: output logic,
    f_one_or: output logic,
) {
    assign c_and_one = u && 1'b1;
    assign c_or_zero = u || 1'b0;
    assign c_zero_and = 1'b0 && u;
    assign c_one_or = 1'b1 || u;

    always_ff (clk) {
        f_and_one = u && 1'b1;
        f_or_zero = u || 1'b0;
        f_zero_and = 1'b0 && u;
        f_one_or = 1'b1 || u;
    }
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        let u = sim.signal("u");

        // X and Z are both an unknown boolean when no known 1 is present.
        // False dominates &&, and true dominates ||.
        for (value, label) in [(1u8, "X"), (0u8, "Z")] {
            sim.modify(|io| {
                io.set_four_state(u, BigUint::from(value), BigUint::from(1u8));
            })
            .unwrap();
            sim.tick(clk).unwrap();

            for prefix in ["c", "f"] {
                for suffix in ["and_one", "or_zero"] {
                    let name = format!("{prefix}_{suffix}");
                    let signal = sim.signal(&name);
                    assert_eq!(
                        sim.get_four_state(signal),
                        (BigUint::from(1u8), BigUint::from(1u8)),
                        "{name}, lhs={label}",
                    );
                }

                let zero_and = format!("{prefix}_zero_and");
                assert_eq!(
                    sim.get_four_state(sim.signal(&zero_and)),
                    (BigUint::from(0u8), BigUint::from(0u8)),
                    "{zero_and}, rhs={label}",
                );

                let one_or = format!("{prefix}_one_or");
                assert_eq!(
                    sim.get_four_state(sim.signal(&one_or)),
                    (BigUint::from(1u8), BigUint::from(0u8)),
                    "{one_or}, rhs={label}",
                );
            }
        }
    }

    fn ternary_unknown_condition_merges_branch_bits(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    sel: input logic,
    a: input logic<8>,
    b: input logic<8>,
    c: output logic<8>,
    f: output logic<8>,
) {
    assign c = if sel ? a : b;
    always_ff (clk) {
        f = if sel ? a : b;
    }
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        let sel = sim.signal("sel");
        let a = sim.signal("a");
        let b = sim.signal("b");

        for (cond_value, label) in [(1u8, "X"), (0u8, "Z")] {
            sim.modify(|io| {
                io.set_four_state(sel, BigUint::from(cond_value), BigUint::from(1u8));
                io.set_four_state(a, BigUint::from(0xaau8), BigUint::from(0u8));
                io.set_four_state(b, BigUint::from(0xbbu8), BigUint::from(0u8));
            })
            .unwrap();
            sim.tick(clk).unwrap();

            // 0xaa and 0xbb differ only in bits 0 and 4. Equal bits remain
            // known; differing bits become X in Celox's (value, mask) form.
            for name in ["c", "f"] {
                assert_eq!(
                    sim.get_four_state(sim.signal(name)),
                    (BigUint::from(0xbbu8), BigUint::from(0x11u8)),
                    "{name}, cond={label}",
                );
            }

            sim.modify(|io| {
                io.set_four_state(a, BigUint::from(0xa5u8), BigUint::from(0u8));
                io.set_four_state(b, BigUint::from(0xa5u8), BigUint::from(0u8));
            })
            .unwrap();
            sim.tick(clk).unwrap();

            for name in ["c", "f"] {
                assert_eq!(
                    sim.get_four_state(sim.signal(name)),
                    (BigUint::from(0xa5u8), BigUint::from(0u8)),
                    "{name}, identical branches, cond={label}",
                );
            }
        }
    }

    fn wide_logical_unknown_truth_table_uses_dominant_values(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    u: input logic<130>,
    c_and_one: output logic,
    c_or_zero: output logic,
    c_and_zero: output logic,
    c_or_one: output logic,
    f_and_one: output logic,
    f_or_zero: output logic,
    f_and_zero: output logic,
    f_or_one: output logic,
) {
    assign c_and_one = u && 1'b1;
    assign c_or_zero = u || 1'b0;
    assign c_and_zero = u && 1'b0;
    assign c_or_one = u || 1'b1;
    always_ff (clk) {
        f_and_one = u && 1'b1;
        f_or_zero = u || 1'b0;
        f_and_zero = u && 1'b0;
        f_or_one = u || 1'b1;
    }
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        let u = sim.signal("u");
        let unknown_bit = BigUint::from(1u8) << 100usize;
        for (value, label) in [
            (unknown_bit.clone(), "X in the high chunk"),
            (BigUint::from(0u8), "Z in the high chunk"),
        ] {
            sim.modify(|io| {
                io.set_four_state(u, value.clone(), unknown_bit.clone());
            })
            .unwrap();
            sim.tick(clk).unwrap();

            for prefix in ["c", "f"] {
                for suffix in ["and_one", "or_zero"] {
                    let name = format!("{prefix}_{suffix}");
                    assert_eq!(
                        sim.get_four_state(sim.signal(&name)),
                        (BigUint::from(1u8), BigUint::from(1u8)),
                        "{name}, {label}",
                    );
                }
                let and_zero = format!("{prefix}_and_zero");
                let and_zero_signal = sim.signal(&and_zero);
                assert_eq!(
                    sim.get_four_state(and_zero_signal),
                    (BigUint::from(0u8), BigUint::from(0u8)),
                    "{and_zero}, {label}",
                );
                let or_one = format!("{prefix}_or_one");
                let or_one_signal = sim.signal(&or_one);
                assert_eq!(
                    sim.get_four_state(or_one_signal),
                    (BigUint::from(1u8), BigUint::from(0u8)),
                    "{or_one}, {label}",
                );
            }
        }
    }

    fn logical_not_known_one_dominates_unknown_bits(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    narrow: input logic<8>,
    wide: input logic<130>,
    c_narrow: output logic,
    c_wide: output logic,
    f_narrow: output logic,
    f_wide: output logic,
) {
    assign c_narrow = !narrow;
    assign c_wide = !wide;
    always_ff (clk) {
        f_narrow = !narrow;
        f_wide = !wide;
    }
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        let narrow = sim.signal("narrow");
        let wide = sim.signal("wide");
        let narrow_unknown = BigUint::from(1u8) << 2usize;
        let narrow_known_one = BigUint::from(1u8) << 7usize;
        let wide_unknown = BigUint::from(1u8) << 3usize;
        let wide_known_one = BigUint::from(1u8) << 100usize;

        for (unknown_value, label) in [(1u8, "X"), (0u8, "Z")] {
            let unknown_value = BigUint::from(unknown_value);
            sim.modify(|io| {
                io.set_four_state(
                    narrow,
                    narrow_known_one.clone() | (&narrow_unknown * &unknown_value),
                    narrow_unknown.clone(),
                );
                io.set_four_state(
                    wide,
                    wide_known_one.clone() | (&wide_unknown * &unknown_value),
                    wide_unknown.clone(),
                );
            })
            .unwrap();
            sim.tick(clk).unwrap();

            for name in ["c_narrow", "c_wide", "f_narrow", "f_wide"] {
                let signal = sim.signal(name);
                assert_eq!(
                    sim.get_four_state(signal),
                    (BigUint::from(0u8), BigUint::from(0u8)),
                    "{name}, operand contains a known 1 and {label}",
                );
            }

            sim.modify(|io| {
                io.set_four_state(
                    narrow,
                    &narrow_unknown * &unknown_value,
                    narrow_unknown.clone(),
                );
                io.set_four_state(
                    wide,
                    &wide_unknown * &unknown_value,
                    wide_unknown.clone(),
                );
            })
            .unwrap();
            sim.tick(clk).unwrap();

            for name in ["c_narrow", "c_wide", "f_narrow", "f_wide"] {
                let signal = sim.signal(name);
                assert_eq!(
                    sim.get_four_state(signal),
                    (BigUint::from(1u8), BigUint::from(1u8)),
                    "{name}, operand contains no known 1 and {label}",
                );
            }
        }
    }

    fn effectful_ternary_takes_known_true_branch_despite_unknown_bits(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    clear: input logic,
    cond: input logic<130>,
    side_state: output logic,
    result: output logic,
) {
    var side_effect: logic;
    function set_side_effect (y: output logic) -> logic {
        y = 1'b1;
        return 1'b0;
    }
    always_ff (clk) {
        if clear {
            side_effect = 1'b0;
            result = 1'b0;
        } else {
            result = if cond ? 1'b1 : set_side_effect(side_effect);
        }
    }
    assign side_state = side_effect;
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        let clear = sim.signal("clear");
        let cond = sim.signal("cond");
        let known_one = BigUint::from(1u8) << 100usize;
        let unknown = BigUint::from(1u8) << 3usize;
        sim.modify(|io| {
            io.set(clear, 1u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| {
            io.set(clear, 0u8);
            io.set_four_state(cond, &known_one | &unknown, unknown.clone());
        })
        .unwrap();
        sim.tick(clk).unwrap();

        let result = sim.signal("result");
        let side_state = sim.signal("side_state");
        assert_eq!(
            sim.get_four_state(result),
            (BigUint::from(1u8), BigUint::from(0u8)),
        );
        assert_eq!(
            sim.get_four_state(side_state),
            (BigUint::from(0u8), BigUint::from(0u8)),
            "the unselected effectful arm must not execute",
        );
    }

    fn wide_ternary_condition_known_one_dominates_unknown_bits(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    sel: input logic<130>,
    a: input logic<8>,
    b: input logic<8>,
    c: output logic<8>,
    f: output logic<8>,
) {
    assign c = if sel ? a : b;
    always_ff (clk) {
        f = if sel ? a : b;
    }
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        let sel = sim.signal("sel");
        let a = sim.signal("a");
        let b = sim.signal("b");
        let known_one = BigUint::from(1u8) << 100usize;
        let unknown_bit = BigUint::from(1u8) << 3usize;

        for (unknown_value, label) in [
            (unknown_bit.clone(), "X"),
            (BigUint::from(0u8), "Z"),
        ] {
            sim.modify(|io| {
                io.set_four_state(
                    sel,
                    known_one.clone() | unknown_value.clone(),
                    unknown_bit.clone(),
                );
                io.set_four_state(a, BigUint::from(0xa5u8), BigUint::from(0u8));
                io.set_four_state(b, BigUint::from(0x3cu8), BigUint::from(0u8));
            })
            .unwrap();
            sim.tick(clk).unwrap();

            for name in ["c", "f"] {
                let signal = sim.signal(name);
                assert_eq!(
                    sim.get_four_state(signal),
                    (BigUint::from(0xa5u8), BigUint::from(0u8)),
                    "{name}, lower unknown bit is {label}",
                );
            }
        }
    }

    fn wide_ternary_unknown_condition_merges_every_arm_chunk(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    sel: input logic,
    a: input logic<130>,
    b: input logic<130>,
    c: output logic<130>,
    f: output logic<130>,
) {
    assign c = if sel ? a : b;
    always_ff (clk) {
        f = if sel ? a : b;
    }
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        let sel = sim.signal("sel");
        let a = sim.signal("a");
        let b = sim.signal("b");
        let bit = |index: usize| BigUint::from(1u8) << index;
        let a_value = bit(129) | bit(128) | bit(66) | bit(3) | bit(0);
        let a_mask = bit(66) | bit(65) | bit(3) | bit(2);
        let b_value = bit(128) | bit(64) | bit(3);
        let b_mask = bit(66) | bit(3) | bit(2);
        let full_mask = (BigUint::from(1u8) << 130usize) - BigUint::from(1u8);
        let diff = ((&a_value ^ &b_value) | (&a_mask ^ &b_mask)) & &full_mask;
        let expected_value = (&a_value | &diff) & &full_mask;
        let expected_mask = (&a_mask | &diff) & &full_mask;

        for (cond_value, label) in [(1u8, "X"), (0u8, "Z")] {
            sim.modify(|io| {
                io.set_four_state(sel, BigUint::from(cond_value), BigUint::from(1u8));
                io.set_four_state(a, a_value.clone(), a_mask.clone());
                io.set_four_state(b, b_value.clone(), b_mask.clone());
            })
            .unwrap();
            sim.tick(clk).unwrap();

            for name in ["c", "f"] {
                let signal = sim.signal(name);
                assert_eq!(
                    sim.get_four_state(signal),
                    (expected_value.clone(), expected_mask.clone()),
                    "{name}, cond={label}",
                );
            }
        }
    }

    fn wide_unaligned_partial_store_and_slice_preserve_four_state_bits(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    seed: input logic<192>,
    val: input logic<125>,
    wide: output logic<192>,
    sliced: output logic<125>,
) {
    always_comb {
        wide = seed;
        wide[130:6] = val;
        sliced = wide[130:6];
    }
}
"#, "Top").four_state(true);

        let seed = sim.signal("seed");
        let val = sim.signal("val");
        let wide = sim.signal("wide");
        let sliced = sim.signal("sliced");
        let bit = |index: usize| BigUint::from(1u8) << index;
        let full_mask = (BigUint::from(1u8) << 192usize) - BigUint::from(1u8);
        let val_width_mask = (BigUint::from(1u8) << 125usize) - BigUint::from(1u8);
        let field_mask = &val_width_mask << 6usize;
        let keep_mask = &full_mask ^ &field_mask;
        let seed_mask = bit(190) | bit(129) | bit(63) | bit(4);
        let seed_value = bit(191) | bit(130) | bit(64) | bit(5) | bit(0) | &seed_mask;
        let val_value = bit(124) | bit(65) | bit(64) | bit(1);
        let val_mask = bit(123) | bit(66) | bit(63) | bit(0);
        let expected_value =
            (&seed_value & &keep_mask) | ((&val_value & &val_width_mask) << 6usize);
        let expected_mask =
            (&seed_mask & &keep_mask) | ((&val_mask & &val_width_mask) << 6usize);

        sim.modify(|io| {
            io.set_four_state(seed, seed_value.clone(), seed_mask.clone());
            io.set_four_state(val, val_value.clone(), val_mask.clone());
        })
        .unwrap();

        assert_eq!(
            sim.get_four_state(wide),
            (expected_value, expected_mask),
            "unaligned wide partial store",
        );
        assert_eq!(
            sim.get_four_state(sliced),
            (val_value & val_width_mask, val_mask),
            "slice following the partial store",
        );
    }
}
