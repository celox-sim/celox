//! Executable adaptations of Veryl's compiler fixtures.
//! See ../../UPSTREAM_CASES.md for the pinned sources and attribution.
use crate::Design;

cases! { Types, "veryl_language";
    fn inside_outside_range_endpoints(sim) {
        @build Design::new(r#"
module Top (
    value: input logic<8>,
    included: output logic,
    excluded: output logic,
    low: input logic<8>,
    high: input logic<8>,
    dynamic_inside: output logic,
    dynamic_outside: output logic,
    inclusive_case: output logic,
) {
    assign included = inside value {0, 3..6, 9..=11};
    assign excluded = outside value {0, 3..6, 9..=11};
    assign dynamic_inside = inside value {low..=high};
    assign dynamic_outside = outside value {low..=high};
    always_comb {
        case value {
            low..=high: inclusive_case = 1'b1;
            default: inclusive_case = 1'b0;
        }
    }
}
"#, "Top");
        let value = sim.signal("value");
        let included = sim.signal("included");
        let excluded = sim.signal("excluded");
        // Veryl's half-open 3..6 includes 3,4,5; 9..=11 includes both ends.
        for input in 0u8..=255 {
            sim.modify(|io| io.set(value, input)).unwrap();
            let expected = input == 0 || (3..6).contains(&input) || (9..=11).contains(&input);
            assert_eq!(sim.get(included), u8::from(expected).into(), "input={input}");
            assert_eq!(sim.get(excluded), u8::from(!expected).into(), "input={input}");
        }
        let low = sim.signal("low");
        let high = sim.signal("high");
        let dynamic_inside = sim.signal("dynamic_inside");
        let dynamic_outside = sim.signal("dynamic_outside");
        let inclusive_case = sim.signal("inclusive_case");
        for (lo, hi) in [(0u8, 0u8), (0, 255), (5, 9), (9, 5), (255, 255)] {
            sim.modify(|io| {
                io.set(low, lo);
                io.set(high, hi);
            }).unwrap();
            for input in 0u8..=255 {
                sim.modify(|io| io.set(value, input)).unwrap();
                let inclusive = lo <= input && input <= hi;
                assert_eq!(sim.get(dynamic_inside), u8::from(inclusive).into());
                assert_eq!(sim.get(dynamic_outside), u8::from(!inclusive).into());
                assert_eq!(sim.get(inclusive_case), u8::from(inclusive).into());
            }
        }
    }

    fn parameter_expression_type_cast_widths(sim) {
        @build Design::new(r#"
module Top #(
    param W: u32 = 8,
    param Q: u32 = 16,
) (
    i_x: input logic<16>,
    o_a: output logic<32>,
    o_b: output logic<32>,
    o_c: output logic<32>,
    expr_a: output logic<32>,
    expr_b: output logic<32>,
    expr_c: output logic<32>,
) {
    // Check Veryl 0.22 expression casts against the equivalent named types.
    type A = logic<W + 1>;
    type B = logic<Q + 1>;
    type C = logic<W * 2 - 3>;
    assign o_a = i_x as A;
    assign o_b = (i_x - 2) as B;
    assign o_c = i_x as C;
    assign expr_a = i_x as (W + 1);
    assign expr_b = (i_x - 2) as (Q + 1);
    assign expr_c = i_x as (W * 2 - 3);
}
"#, "Top");
        let input = sim.signal("i_x");
        let a = sim.signal("o_a");
        let b = sim.signal("o_b");
        let c = sim.signal("o_c");
        for value in [0u16, 1, 2, 0x1ff, 0x200, 0x1fff, 0x2000, 0xffff] {
            sim.modify(|io| io.set(input, value)).unwrap();
            assert_eq!(sim.get(a), (u32::from(value) & 0x1ff).into());
            assert_eq!(sim.get(b), (u32::from(value).wrapping_sub(2) & 0x1ffff).into());
            assert_eq!(sim.get(c), (u32::from(value) & 0x1fff).into());
            for (name, named) in [("expr_a", a), ("expr_b", b), ("expr_c", c)] {
                assert_eq!(sim.get(sim.signal(name)), sim.get(named), "{name}: {value}");
            }
        }
    }

    fn packed_union_members_alias(sim) {
        @build Design::new(r#"
module Top (
    input_bits: input logic<8>,
    raw: output logic<8>,
    upper: output logic<4>,
    lower: output logic<4>,
) {
    struct Nibbles { high: logic<4>, low: logic<4>, }
    union Overlay { bits: logic<8>, parts: Nibbles, }
    var data: Overlay;
    assign data.bits = input_bits;
    assign raw = data.bits;
    assign upper = data.parts.high;
    assign lower = data.parts.low;
}
"#, "Top");
        let input = sim.signal("input_bits");
        let raw = sim.signal("raw");
        let upper = sim.signal("upper");
        let lower = sim.signal("lower");
        for value in 0u8..=255 {
            sim.modify(|io| io.set(input, value)).unwrap();
            assert_eq!(sim.get(raw), value.into());
            assert_eq!(sim.get(upper), (value >> 4).into());
            assert_eq!(sim.get(lower), (value & 15).into());
        }
    }
}
