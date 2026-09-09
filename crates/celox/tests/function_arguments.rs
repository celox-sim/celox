use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

// The SV frontend currently rejects output/inout function arguments.
all_backends! {

// Keep direct-syntax regressions ready for an upstream fix: Veryl 0.21.0's
// conv_function matches only Input/Output for scalar formals and panics on Inout.
#[ignore = "Veryl 0.21.0 conv_function panics on scalar inout arguments"]
fn test_comb_inout_statement_copies_input_before_mutating_formal(sim) {
    @build Simulator::builder(r#"
        module Top (d: input logic<8>, q: output logic<8>, original: output logic<8>) {
            function update (
                value: inout tri logic<8>,
                snapshot: input logic<8>,
                observed: output logic<8>,
            ) {
                value += 8'd3;
                observed = snapshot;
                value += snapshot;
            }

            always_comb {
                q = d;
                update(q, q, original);
            }
        }
    "#, "Top");
    let d = sim.signal("d");
    let q = sim.signal("q");
    let original = sim.signal("original");

    for value in [0u8, 7, 126, 255, 7] {
        sim.modify(|io| io.set(d, value)).unwrap();
        assert_eq!(sim.get(q), value.wrapping_mul(2).wrapping_add(3).into());
        assert_eq!(sim.get(original), value.into());
    }
}

#[ignore = "Veryl 0.21.0 conv_function panics on scalar inout arguments"]
fn test_ff_inout_expression_copyout_commits_with_nonblocking_assignments(sim) {
    @build Simulator::builder(r#"
        module Top (
            clk: input clock,
            increment: input logic<8>,
            state: output logic<8>,
            returned: output logic<8>,
            sampled: output logic<8>,
        ) {
            function advance (value: inout tri logic<8>, delta: input logic<8>) -> logic<8> {
                let previous: logic<8> = value;
                value += delta;
                return previous;
            }

            always_ff (clk) {
                returned = advance(state, increment);
                sampled = state;
            }
        }
    "#, "Top");
    let clk = sim.event("clk");
    let increment = sim.signal("increment");
    let state = sim.signal("state");
    let returned = sim.signal("returned");
    let sampled = sim.signal("sampled");

    let mut expected = 0u8;
    for delta in [7u8, 9, 250, 0, 3] {
        sim.modify(|io| io.set(increment, delta)).unwrap();
        assert_eq!(sim.get(state), expected.into());
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(returned), expected.into());
        assert_eq!(sim.get(sampled), expected.into());
        expected = expected.wrapping_add(delta);
        assert_eq!(sim.get(state), expected.into());
    }
}

fn test_comb_output_copyout_freezes_aliased_inputs_and_return(sim) {
    @ignore_on(sv);
    @build Simulator::builder(r#"
        module Top (
            a: input logic<8>, b: input logic<8>,
            first: output logic<8>, second: output logic<8>, sum: output logic<8>,
        ) {
            function swap (
                old_first: input logic<8>, old_second: input logic<8>,
                new_first: output logic<8>, new_second: output logic<8>,
            ) -> logic<8> {
                new_first = old_second;
                new_second = old_first;
                return old_first + old_second;
            }
            always_comb {
                first = a;
                second = b;
                sum = swap(first, second, first, second);
            }
        }
    "#, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let first = sim.signal("first");
    let second = sim.signal("second");
    let sum = sim.signal("sum");

    for (x, y) in [(7u8, 9u8), (255, 2), (0, 128), (9, 7)] {
        sim.modify(|io| {
            io.set(a, x);
            io.set(b, y);
        }).unwrap();
        assert_eq!(sim.get(first), y.into());
        assert_eq!(sim.get(second), x.into());
        assert_eq!(sim.get(sum), x.wrapping_add(y).into());
    }
}

fn test_comb_statement_output_copyout_obeys_named_argument_order(sim) {
    @ignore_on(sv);
    @build Simulator::builder(r#"
        module Top (d: input logic<8>, positional: output logic<8>, named: output logic<8>) {
            function split (x: input logic<8>, first: output logic<8>, second: output logic<8>) {
                first = x + 8'd1;
                second = x + 8'd2;
            }
            always_comb {
                split(d, positional, positional);
                split(second: named, x: d, first: named);
            }
        }
    "#, "Top");
    let d = sim.signal("d");
    let positional = sim.signal("positional");
    let named = sim.signal("named");

    for value in [0u8, 7, 254, 255] {
        sim.modify(|io| io.set(d, value)).unwrap();
        assert_eq!(sim.get(positional), value.wrapping_add(2).into());
        assert_eq!(sim.get(named), value.wrapping_add(1).into());
    }
}

fn test_comb_nested_output_copyout_stops_at_early_return(sim) {
    // Veryl 0.21.0 loses the nested output on the early-return path (0 vs 11).
    @ignore_on(veryl, sv);
    @build Simulator::builder(r#"
        module Top (
            d: input logic<8>, early: input logic,
            copied: output logic<8>, result: output logic<8>,
        ) {
            function inner (x: input logic<8>, stop: input logic, dst: output logic<8>) -> logic<8> {
                dst = x + 8'd1;
                if stop {
                    return x + 8'd2;
                }
                dst = x + 8'd3;
                return x + 8'd4;
            }
            function outer (x: input logic<8>, stop: input logic, dst: output logic<8>) -> logic<8> {
                let returned: logic<8> = inner(x, stop, dst);
                return returned + dst;
            }
            always_comb {
                result = outer(d, early, copied);
            }
        }
    "#, "Top");
    let d = sim.signal("d");
    let early = sim.signal("early");
    let copied = sim.signal("copied");
    let result = sim.signal("result");

    for (value, stop) in [(10u8, 1u8), (10, 0), (254, 1), (255, 0), (10, 1)] {
        sim.modify(|io| {
            io.set(d, value);
            io.set(early, stop);
        }).unwrap();
        let written = value.wrapping_add(if stop != 0 { 1 } else { 3 });
        let returned = value.wrapping_add(if stop != 0 { 2 } else { 4 });
        assert_eq!(sim.get(copied), written.into());
        assert_eq!(sim.get(result), written.wrapping_add(returned).into());
    }
}

fn test_comb_output_copyout_to_concat_preserves_unselected_bits_and_elements(sim) {
    // Veryl 0.21.0 writes the low byte into the high concatenation destination.
    @ignore_on(veryl, sv);
    @build Simulator::builder(r#"
        module Top (
            d: input logic<16>, index: input logic<2>,
            word: output logic<32>, items: output logic<8>[4],
        ) {
            function write (x: input logic<16>, dst: output logic<16>) {
                dst = x;
            }
            always_comb {
                word = 32'h89ab_cdef;
                items = '{8'ha0, 8'hb1, 8'hc2, 8'hd3};
                write(d, {word[index step 8], items[index]});
            }
        }
    "#, "Top");
    let d = sim.signal("d");
    let index = sim.signal("index");
    let word = sim.signal("word");
    let items = sim.signal("items");

    for slot in 0u8..4 {
        for value in [0x1234u16, 0xff00, 0x00ff, 0] {
            sim.modify(|io| {
                io.set(d, value);
                io.set(index, slot);
            }).unwrap();
            let shift = slot * 8;
            let expected_word = (0x89ab_cdefu32 & !(0xff << shift))
                | (u32::from(value >> 8) << shift);
            let expected_items = (0xd3c2_b1a0u32 & !(0xff << shift))
                | (u32::from(value & 0xff) << shift);
            assert_eq!(
                sim.get(word), expected_word.into(),
                "slot={slot}, value={value:#06x}",
            );
            assert_eq!(
                sim.get(items), expected_items.into(),
                "slot={slot}, value={value:#06x}",
            );
        }
    }
}

fn test_output_copyout_converts_formal_width_and_signedness_in_comb_and_ff(sim) {
    // Veryl 0.21.0 zero-extends the signed output (0x0080 vs 0xff80).
    @ignore_on(veryl, sv);
    @build Simulator::builder(r#"
        module Top (
            clk: input clock, d: input logic<8>, narrow: output logic<4>,
            signed_wide: output logic<16>, unsigned_wide: output logic<16>,
            ff_narrow: output logic<4>,
            ff_signed_wide: output logic<16>, ff_unsigned_wide: output logic<16>,
        ) {
            function split (
                x: input logic<8>, truncated: output logic<8>,
                negative: output signed logic<8>, positive: output logic<8>,
            ) {
                truncated = x;
                negative = x;
                positive = x;
            }
            always_comb {
                split(d, narrow, signed_wide, unsigned_wide);
            }
            always_ff (clk) {
                split(d, ff_narrow, ff_signed_wide, ff_unsigned_wide);
            }
        }
    "#, "Top").four_state(true);
    let clk = sim.event("clk");
    let d = sim.signal("d");

    // Both X and Z sign bits must extend their payload and unknown mask.
    for (value, mask) in [
        (0u8, 0u8), (0x7f, 0), (0x80, 0), (0xf3, 0), (0xff, 0),
        (0x80, 0x80), (0, 0x80), (0xa5, 0x0c),
    ] {
        sim.modify(|io| io.set_four_state(d, value.into(), mask.into()))
            .unwrap();
        sim.tick(clk).unwrap();
        for (names, expected_value, expected_mask) in [
            (["narrow", "ff_narrow"], u16::from(value & 0xf), u16::from(mask & 0xf)),
            (["signed_wide", "ff_signed_wide"], value as i8 as i16 as u16, mask as i8 as i16 as u16),
            (["unsigned_wide", "ff_unsigned_wide"], u16::from(value), u16::from(mask)),
        ] {
            for name in names {
                let signal = sim.signal(name);
                assert_eq!(
                    sim.get_four_state(signal), (expected_value.into(), expected_mask.into()),
                    "{name}: value={value:#04x}, mask={mask:#04x}",
                );
            }
        }
    }
}

fn test_comb_expression_output_copyout_uses_unsigned_formal_for_signed_body(sim) {
    @ignore_on(sv);
    @build Simulator::builder(r#"
        module Top (choose: input logic, copied: output logic<16>, returned: output logic) {
            function produce (x: input logic, dst: output logic<8>) -> logic {
                dst = if x ? 8'shff : 8'sh80;
                return x;
            }
            always_comb {
                returned = produce(choose, copied);
            }
        }
    "#, "Top");
    let choose = sim.signal("choose");
    let copied = sim.signal("copied");
    let returned = sim.signal("returned");

    for value in [0u8, 1, 0, 1] {
        sim.modify(|io| io.set(choose, value)).unwrap();
        let expected = if value != 0 { 0xffu16 } else { 0x80 };
        assert_eq!(sim.get(copied), expected.into());
        assert_eq!(sim.get(returned), value.into());
    }
}

fn test_comb_output_copyout_observer_sees_formal_sign_extension(sim) {
    @omit_veryl;
    @ignore_on(wasm, sv);
    @build Simulator::builder(r#"
        module Top (
            d: input logic<8>, copied: output logic<16>,
            bits: output logic<2>, returned: output logic,
        ) {
            function observe (x: input logic<16>) -> logic {
                $display("copied=%0d", x);
                return x[15];
            }
            function produce (x: input logic<8>, dst: output signed logic<8>) -> logic {
                dst = x;
                return 1'b1;
            }
            always_comb {
                copied = 16'd0;
                bits = 2'b00;
                returned = produce(d, {bits[observe(copied)], copied});
            }
        }
    "#, "Top");
    let d = sim.signal("d");
    let copied = sim.signal("copied");
    let bits = sim.signal("bits");
    let returned = sim.signal("returned");
    sim.drain_runtime_events();

    for value in [0x80u8, 0x7f, 0xff, 0] {
        sim.modify(|io| io.set(d, value)).unwrap();
        let expected = value as i8 as i16 as u16;
        assert_eq!(sim.get(copied), expected.into());
        let expected_bits = if value & 0x80 != 0 { 2u8 } else { 0 };
        assert_eq!(sim.get(bits), expected_bits.into());
        assert_eq!(sim.get(returned), 1u8.into());
        assert_eq!(
            sim.drain_runtime_events(),
            vec![celox::RuntimeEvent::Display {
                message: format!("copied={expected}"),
            }],
        );
    }
}

fn test_ff_expression_output_copyout_extends_before_splitting_concat(sim) {
    // Veryl 0.21.0 does not extend/split the formal into concatenated actuals.
    @ignore_on(veryl, sv);
    @build Simulator::builder(r#"
        module Top (
            clk: input clock, d: input logic<8>,
            high: output logic<8>, low: output logic<8>, returned: output logic<8>,
        ) {
            function produce (x: input logic<8>, dst: output signed logic<8>) -> logic<8> {
                dst = x;
                return x;
            }
            always_ff (clk) {
                returned = produce(d, {high, low});
            }
        }
    "#, "Top").four_state(true);
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let high = sim.signal("high");

    for (value, mask) in [
        (0x7fu8, 0u8), (0x80, 0), (0x80, 0x80), (0, 0x80), (0xa5, 0x0c),
    ] {
        sim.modify(|io| io.set_four_state(d, value.into(), mask.into()))
            .unwrap();
        sim.tick(clk).unwrap();
        let high_value = if value & 0x80 != 0 { 0xffu8 } else { 0 };
        let high_mask = if mask & 0x80 != 0 { 0xffu8 } else { 0 };
        assert_eq!(
            sim.get_four_state(high), (high_value.into(), high_mask.into()),
        );
        for name in ["low", "returned"] {
            let signal = sim.signal(name);
            assert_eq!(
                sim.get_four_state(signal), (value.into(), mask.into()), "{name}",
            );
        }
    }
}

fn test_ff_statement_output_copyout_freezes_all_inputs(sim) {
    @ignore_on(sv);
    @build Simulator::builder(r#"
        module Top (
            clk: input clock, first: output logic<8>,
            second: output logic<8>, sampled: output logic<8>,
        ) {
            function swap (
                old_first: input logic<8>, old_second: input logic<8>,
                new_first: output logic<8>, new_second: output logic<8>,
            ) {
                new_first = old_second;
                new_second = old_first;
            }
            always_ff (clk) {
                swap(first, second, first, second);
                sampled = first;
            }
        }
    "#, "Top");
    let clk = sim.event("clk");
    let first = sim.signal("first");
    let second = sim.signal("second");
    let sampled = sim.signal("sampled");
    sim.modify(|io| {
        io.set(first, 0x35u8);
        io.set(second, 0xa7u8);
    }).unwrap();

    for (old_first, old_second) in [(0x35u8, 0xa7u8), (0xa7, 0x35), (0x35, 0xa7)] {
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(first), old_second.into());
        assert_eq!(sim.get(second), old_first.into());
        assert_eq!(sim.get(sampled), old_first.into());
    }
}

fn test_ff_output_copyout_to_dynamic_slice_preserves_other_bits(sim) {
    // Veryl 0.21.0 keeps writing byte 0 after the destination index changes.
    @ignore_on(veryl, sv);
    @build Simulator::builder(r#"
        module Top (
            clk: input clock, d: input logic<8>, index: input logic<2>,
            word: output logic<32>, sampled: output logic<32>, returned: output logic<8>,
        ) {
            function write (x: input logic<8>, dst: output logic<8>) -> logic<8> {
                dst = x;
                return x ^ 8'hff;
            }
            always_ff (clk) {
                returned = write(d, word[index step 8]);
                sampled = word;
            }
        }
    "#, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let index = sim.signal("index");
    let word = sim.signal("word");
    let sampled = sim.signal("sampled");
    let returned = sim.signal("returned");
    let mut expected = 0x89ab_cdefu32;
    sim.modify(|io| io.set(word, expected)).unwrap();

    for (slot, value) in [(0u8, 0x12u8), (3, 0x34), (1, 0xff), (2, 0), (0, 0x56)] {
        sim.modify(|io| {
            io.set(index, slot);
            io.set(d, value);
        }).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sampled), expected.into());
        expected = (expected & !(0xff << (slot * 8))) | (u32::from(value) << (slot * 8));
        assert_eq!(sim.get(word), expected.into());
        assert_eq!(sim.get(returned), (value ^ 0xff).into());
    }
}

fn test_ff_nested_output_copyout_is_visible_before_outer_copyout(sim) {
    @ignore_on(sv);
    @build Simulator::builder(r#"
        module Top (
            clk: input clock, d: input logic<8>, copied: output logic<8>,
            returned: output logic<8>, sampled: output logic<8>,
        ) {
            function inner (x: input logic<8>, dst: output logic<8>) {
                dst = x + 8'd1;
            }
            function outer (x: input logic<8>, dst: output logic<8>) -> logic<8> {
                inner(x, dst);
                dst += 8'd2;
                return dst + 8'd4;
            }
            always_ff (clk) {
                returned = outer(d, copied);
                sampled = copied;
            }
        }
    "#, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let copied = sim.signal("copied");
    let returned = sim.signal("returned");
    let sampled = sim.signal("sampled");
    let mut previous = 0u8;

    for value in [7u8, 254, 0, 7] {
        sim.modify(|io| io.set(d, value)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sampled), previous.into());
        previous = value.wrapping_add(3);
        assert_eq!(sim.get(copied), previous.into());
        assert_eq!(sim.get(returned), value.wrapping_add(7).into());
    }
}

}
