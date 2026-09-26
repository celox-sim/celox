use crate::BigUint;
use crate::Design;

cases! { Sequential, "flip_flop";


fn test_ff_nonblocking(sim) {
    @setup { let code = r#"
        module Top (clk: input clock, a: input logic<32>, q: output logic<32>) {
            var r: logic<32>;
            always_ff (clk) {
                r = a;
                q = r;
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let q = sim.signal("q");

    sim.modify(|io| io.set(a, 0x11111111u32)).unwrap();
    sim.tick(clk).unwrap();
    // After 1st tick: r = 0x11111111, q = 0x0
    assert_eq!(sim.get(q), 0x0u32.into());

    sim.tick(clk).unwrap();
    // After 2nd tick: q = 0x11111111
    assert_eq!(sim.get(q), 0x11111111u32.into());
}



fn test_ff_statement_after_if_reset_keeps_source_order(sim) {
    @setup { let code = r#"
        module Top (
            clk: input clock,
            rst: input reset,
            rearm: input logic,
            o_pending: output logic,
            o_filling: output logic
        ) {
            var pending: logic;
            var filling: logic;
            always_ff (clk, rst) {
                if_reset {
                    pending = 1'b0;
                    filling = 1'b0;
                } else {
                    if filling {
                        filling = 1'b0;
                    } else if pending {
                        filling = 1'b1;
                        pending = 1'b0;
                    }
                }
                if rearm {
                    pending = 1'b1;
                }
            }
            assign o_pending = pending;
            assign o_filling = filling;
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let rearm = sim.signal("rearm");
    let pending = sim.signal("o_pending");
    let filling = sim.signal("o_filling");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(rearm, 0u8);
    }).unwrap();
    sim.tick(clk).unwrap();

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(rearm, 1u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(pending), 1u8.into());
    assert_eq!(sim.get(filling), 0u8.into());

    sim.tick(clk).unwrap();

    assert_eq!(sim.get(pending), 1u8.into());
    assert_eq!(sim.get(filling), 1u8.into());
}



fn test_ff_static_and_dynamic_writes_share_sparse_state(sim) {
    @setup { let code = r#"
        module Top (
            clk: input clock,
            use_static: input logic,
            index: input logic<2>,
            value: input logic<8>,
            q: output logic<8>
        ) {
            var state: logic<8> [4];
            always_ff (clk) {
                if use_static {
                    state[0] = value;
                } else {
                    state[index] = value;
                }
                q = state[0];
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let use_static = sim.signal("use_static");
    let index = sim.signal("index");
    let value = sim.signal("value");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(use_static, 1u8);
        io.set(value, 0x5au8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());

    sim.modify(|io| {
        io.set(use_static, 0u8);
        io.set(index, 1u8);
        io.set(value, 0xa5u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x5au8.into());
}



fn test_ff_assert_message_output_argument_is_eager(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            ok: input logic,
            d: input logic<8>,
            effect: output logic<8>
        ) {
            function message_value (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x + 8'd2;
            }

            always_ff (clk) {
                $assert_continue(ok, "value=%0d", message_value(d, effect));
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let ok = sim.signal("ok");
    let d = sim.signal("d");
    let effect = sim.signal("effect");

    sim.modify(|io| {
        io.set(ok, 1u8);
        io.set(d, 10u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 11u8.into());

    sim.modify(|io| {
        io.set(ok, 0u8);
        io.set(d, 20u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 21u8.into());
}



fn test_ff_runtime_function_snapshots_input_before_callee_nonlocal_write(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            global_value: output logic<8>,
            q: output logic<8>
        ) {
            function update_global (value: input logic<8>) -> logic<8> {
                global_value = 8'd9;
                if value == 8'd1 {
                    return 8'd7;
                } else {
                    return 8'd8;
                }
            }

            always_ff (clk) {
                q = update_global(global_value);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");
    let q = sim.signal("q");

    sim.modify(|io| io.set(global_value, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 9u8.into());
    assert_eq!(sim.get(q), 7u8.into());
}



fn test_ff_runtime_function_snapshots_helper_input_before_callee_nonlocal_write(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            global_value: output logic<8>,
            q: output logic<8>
        ) {
            function read_global () -> logic<8> {
                return global_value;
            }

            function update_global (value: input logic<8>) -> logic<8> {
                global_value = 8'd9;
                if value == 8'd1 {
                    return 8'd7;
                } else {
                    return 8'd8;
                }
            }

            always_ff (clk) {
                q = update_global(read_global());
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");
    let q = sim.signal("q");

    sim.modify(|io| io.set(global_value, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 9u8.into());
    assert_eq!(sim.get(q), 7u8.into());
}



fn test_ff_statement_function_direct_nonlocal_assignment_is_observable(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            global_value: output logic<8>
        ) {
            function set_global (value: input logic<8>) {
                global_value = value;
            }

            always_ff (clk) {
                set_global(d);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let global_value = sim.signal("global_value");

    sim.modify(|io| io.set(d, 0x5au8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 0x5au8.into());
}



fn test_ff_skipped_conditional_nonlocal_write_preserves_prior_ff_assignment(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            gate: input logic,
            global_value: output logic<8>
        ) {
            function maybe_write (gate: input logic) {
                if gate {
                    global_value = 8'h01;
                }
            }

            always_ff (clk) {
                global_value = 8'h05;
                maybe_write(gate);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let gate = sim.signal("gate");
    let global_value = sim.signal("global_value");

    sim.modify(|io| io.set(gate, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 5u8.into());

    sim.modify(|io| io.set(gate, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 1u8.into());
}



fn test_ff_nonlocal_write_precedes_aliased_formal_output_copyout(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            global_value: output logic<8>,
            q: output logic
        ) {
            function update (written: output logic<8>) -> logic {
                global_value = 8'h01;
                written = 8'h02;
                return 0;
            }

            always_ff (clk) {
                q = update(global_value);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 2u8.into());
}



fn test_ff_outputless_wrapper_nested_copyout_to_nonlocal_is_observable(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (clk: input clock, global_value: output logic<8>) {
            function set (written: output logic<8>) {
                written = 8'h5a;
            }

            function outer () {
                set(global_value);
            }

            always_ff (clk) {
                outer();
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 0x5au8.into());
}



fn test_ff_outputless_wrapper_expression_copyout_to_nonlocal_is_observable(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (clk: input clock, global_value: output logic<8>) {
            function set (
                value: input logic<8>,
                written: output logic<8>
            ) -> logic {
                written = value;
                return 1'b0;
            }

            function outer () {
                var ignored: logic;
                ignored = set(8'h5a, global_value);
            }

            always_ff (clk) {
                outer();
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 0x5au8.into());
}



fn test_ff_outputless_wrapper_indexed_copyout_to_nonlocal_is_observable(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (clk: input clock, global_value: output logic<8>) {
            function set (
                value: input logic,
                written: output logic
            ) -> logic {
                written = value;
                return 1'b0;
            }

            function outer () {
                var ignored: logic;
                ignored = set(1'b1, global_value[0]);
            }

            always_ff (clk) {
                outer();
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 1u8.into());
}



fn test_ff_outputless_wrapper_dynamic_indexed_copyout_to_nonlocal_is_observable(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            index: input logic,
            global_value: output logic<8>
        ) {
            function set (
                value: input logic,
                written: output logic
            ) -> logic {
                written = value;
                return 1'b0;
            }

            function outer (index: input logic) {
                var ignored: logic;
                ignored = set(1'b1, global_value[index]);
            }

            always_ff (clk) {
                outer(index);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let global_value = sim.signal("global_value");

    sim.modify(|io| io.set(index, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 2u8.into());
}



fn test_ff_outputless_wrapper_direct_dynamic_nonlocal_assignment_is_observable(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            index: input logic<3>,
            value: input logic,
            global_value: output logic<8>
        ) {
            function write_at (
                index: input logic<3>,
                value: input logic
            ) -> logic {
                global_value[index] = value;
                return 1'b0;
            }

            function outer (
                index: input logic<3>,
                value: input logic
            ) {
                var ignored: logic;
                ignored = write_at(index, value);
            }

            always_ff (clk) {
                outer(index, value);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let value = sim.signal("value");
    let global_value = sim.signal("global_value");

    sim.modify(|io| {
        io.set(index, 2u8);
        io.set(value, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 4u8.into());
}



fn test_ff_dynamic_nonlocal_store_follows_pending_whole_write(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            index: input logic<3>,
            global_value: output logic<8>
        ) {
            function write_at (index: input logic<3>) {
                global_value = 8'hff;
                global_value[index] = 1'b0;
            }

            always_ff (clk) {
                write_at(index);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let global_value = sim.signal("global_value");

    sim.modify(|io| io.set(index, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 0xfdu8.into());
}



fn test_ff_nonlocal_source_ternary_preserves_unknown_merge(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            select: input logic,
            global_value: output logic<8>
        ) {
            function write_selected (select: input logic) {
                global_value = if select ? 8'hf0 : 8'h0f;
            }

            always_ff (clk) {
                write_selected(select);
            }
        }
    "#; }
    @build Design::new(code, "Top").four_state(true);
    let clk = sim.event("clk");
    let select = sim.signal("select");
    let global_value = sim.signal("global_value");

    sim.modify(|io| {
        io.set_four_state(select, BigUint::from(0u8), BigUint::from(1u8));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get_four_state(global_value),
        (BigUint::from(0xffu8), BigUint::from(0xffu8)),
    );
}



fn test_ff_function_output_index_uses_final_nonlocal_state(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            index: output logic,
            entries: output logic<8>[2]
        ) {
            function set (written: output logic<8>) {
                index = 1'b1;
                written = 8'ha5;
            }

            always_ff (clk) {
                set(entries[index]);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let entries = sim.signal("entries");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(index), 1u8.into());
    assert_eq!(sim.get(entries), 0xa500u16.into());
}



fn test_ff_short_circuit_runtime_write_preserves_later_state_source(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            gate: input logic,
            d: input logic<8>,
            state: output logic<8>,
            q: output logic<8>
        ) {
            var ignored: logic;

            function write_state (x: input logic<8>) -> logic {
                state = x;
                return 1'b1;
            }

            always_ff (clk) {
                ignored = gate && write_state(d);
                q = state;
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let gate = sim.signal("gate");
    let d = sim.signal("d");
    let state = sim.signal("state");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(gate, 1u8);
        io.set(d, 0x5au8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(state), 0x5au8.into());
    assert_eq!(sim.get(q), 0u8.into());

    sim.modify(|io| {
        io.set(gate, 0u8);
        io.set(d, 0xa5u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(state), 0x5au8.into());
    assert_eq!(sim.get(q), 0x5au8.into());
}



fn test_ff_bits_and_size_operands_do_not_alias_earlier_array_argument(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            q: output logic<8>
        ) {
            var samples: logic<8>[2];

            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x;
            }

            function pick (
                values: input logic<8>[2],
                shape: input logic<32>
            ) -> logic<8> {
                return values[0] + shape[0];
            }

            always_ff (clk) {
                samples = '{8'h12, 8'h34};
                q = pick(
                    samples,
                    $bits(update(samples[0], samples[0]))
                        + $size(update(samples[0], samples[0]))
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let q = sim.signal("q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x12u8.into());
}



fn test_ff_bits_and_size_array_dependencies_do_not_alias_later_write(sim) {


    @setup { let code = r#"
        module Top (
            clk: input clock,
            q: output logic<32>
        ) {
            var samples: logic<8>[2];

            function set_sample (written: output logic<8>) -> logic<32> {
                written = 8'h5a;
                return 0;
            }

            function pick (
                values: input logic<32>[2],
                ignored: input logic<32>
            ) -> logic<32> {
                return values[0];
            }

            always_ff (clk) {
                q = pick(
                    '{$bits(samples), default: 0},
                    set_sample(samples[0])
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let q = sim.signal("q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 16u32.into());
}



fn test_ff_pure_input_is_snapshotted_before_later_effectful_input(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (clk: input clock, effect: output logic<8>, q: output logic<8>) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x + 8'd1;
            }

            function combine (
                first: input logic<8>,
                second: input logic<8>
            ) -> logic<8> {
                return first;
            }

            always_ff (clk) {
                q = combine(effect, update(effect, effect));
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(effect, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 6u8.into());
    assert_eq!(sim.get(q), 5u8.into());
}



fn test_ff_effectful_array_item_output_is_not_a_read_alias(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function make (written: output logic<8>) -> logic<8> {
                written = 8'h5a;
                return 8'h11;
            }

            function pick (
                values: input logic<8>[1],
                written: output logic<8>
            ) -> logic<8> {
                written = 8'ha5;
                return values[0];
            }

            always_ff (clk) {
                q = pick('{make(effect)}, effect);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 0xa5u8.into());
    assert_eq!(sim.get(q), 0x11u8.into());
}



fn test_ff_runtime_for_bounds(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            count: input logic<8>,
            step_start: input logic<32>,
            q_fwd: output logic<8>,
            q_rev: output logic<8>,
            q_inc: output logic<8>,
            q_step: output logic<8>
        ) {
            always_ff (clk) {
                q_fwd = 8'hee;
                for i in 0..count {
                    q_fwd = i as 8;
                }

                q_rev = 8'hee;
                for i in rev 0..count {
                    q_rev = i as 8;
                }

                q_inc = 8'hee;
                for i in 0..=count {
                    q_inc = i as 8;
                }

                q_step = 8'hee;
                for i in step_start..(count + 4) step *= 2 {
                    q_step = i as 8;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let count = sim.signal("count");
    let step_start = sim.signal("step_start");
    let q_fwd = sim.signal("q_fwd");
    let q_rev = sim.signal("q_rev");
    let q_inc = sim.signal("q_inc");
    let q_step = sim.signal("q_step");

    sim.modify(|io| {
        io.set(count, 4u8);
        io.set(step_start, 1u32);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_fwd), 3u32.into());
    assert_eq!(sim.get(q_rev), 0u32.into());
    assert_eq!(sim.get(q_inc), 4u32.into());
    assert_eq!(sim.get(q_step), 4u32.into());

    sim.modify(|io| io.set(count, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_fwd), 4u32.into());
    assert_eq!(sim.get(q_rev), 0u32.into());
    assert_eq!(sim.get(q_inc), 5u32.into());
    assert_eq!(sim.get(q_step), 8u32.into());
}



fn test_ff_runtime_for_bitwise_steps(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            or_end: input logic<8>,
            xor_end: input logic<8>,
            q_or: output logic<8>,
            q_xor: output logic<8>
        ) {
            always_ff (clk) {
                q_or = 0;
                for i in 3..=or_end step |= 6 {
                    q_or = i as 8;
                    if i == or_end {
                        break;
                    }
                }

                q_xor = 0;
                for i in 3..=xor_end step ^= 6 {
                    q_xor = i as 8;
                    if i == xor_end {
                        break;
                    }
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let or_end = sim.signal("or_end");
    let xor_end = sim.signal("xor_end");
    let q_or = sim.signal("q_or");
    let q_xor = sim.signal("q_xor");

    sim.modify(|io| {
        io.set(or_end, 7u8);
        io.set(xor_end, 5u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_or), 7u8.into());
    assert_eq!(sim.get(q_xor), 5u8.into());
}



fn test_ff_signed_xor_step_uses_loop_counter_width(sim) {
    // The Veryl simulator currently converts the signed dynamic start bound to an
    // unsigned runtime counter and executes zero iterations.

    @setup { let code = r#"
        module Top (
            clk: input clock,
            wide_start: input signed logic<32>,
            wide_end: input signed logic<128>,
            q: output signed logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in wide_start..=wide_end step ^= 2147483648 {
                    q = i;
                    if i == 2147483640 {
                        break;
                    }
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let wide_start = sim.signal("wide_start");
    let wide_end = sim.signal("wide_end");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(wide_start, -8i32);
        io.set_wide(wide_end, BigUint::from(2_147_483_640u32));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x7fff_fff8u32.into());
}



fn test_ff_i32_bitwise_steps_discard_bits_above_the_counter_width(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input signed logic<32>,
            or_end: input signed logic<128>,
            xor_end: input signed logic<128>,
            q_or: output signed logic<32>,
            q_xor: output signed logic<32>
        ) {
            always_ff (clk) {
                q_or = 0;
                for i in start..=or_end step |= 4294967302 {
                    q_or = i;
                    if i == 7 {
                        break;
                    }
                }

                q_xor = 0;
                for i in start..=xor_end step ^= 4294967302 {
                    q_xor = i;
                    if i == 5 {
                        break;
                    }
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let or_end = sim.signal("or_end");
    let xor_end = sim.signal("xor_end");
    let q_or = sim.signal("q_or");
    let q_xor = sim.signal("q_xor");

    sim.modify(|io| {
        io.set(start, 3i32);
        io.set_wide(or_end, BigUint::from(7u8));
        io.set_wide(xor_end, BigUint::from(5u8));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_or), 7u32.into());
    assert_eq!(sim.get(q_xor), 5u32.into());
}



















fn test_ff_runtime_for_break(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            count: input logic<8>,
            q: output logic<8>
        ) {
            always_ff (clk) {
                q = 8'hee;
                for i in 0..count {
                    if i == 3 {
                        break;
                    }
                    q = i as 8;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let count = sim.signal("count");
    let q = sim.signal("q");

    sim.modify(|io| io.set(count, 8u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 2u8.into());
}




fn test_ff_constant_signed_bounds_in_unrolled_loops(sim) {
    // Constant signed reverse bounds are currently broken in the upstream
    // Veryl analyzer unroller, so this regression is parked until upstream
    // Veryl is fixed.
    @setup { let code = r#"
        module Top (
            clk: input clock,
            q_fwd: output logic<32>,
            q_rev_last: output logic<32>
        ) {
            always_ff (clk) {
                q_fwd = 0;
                for i in (0 - 1)..=1 {
                    q_fwd += i as 32;
                }

                q_rev_last = 32'hdead_beef;
                for i in rev (0 - 1)..=1 {
                    q_rev_last = (i + 1) as 32;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let q_fwd = sim.signal("q_fwd");
    let q_rev_last = sim.signal("q_rev_last");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_fwd), 0u32.into());
    assert_eq!(sim.get(q_rev_last), 0u32.into());
}



fn test_ff_runtime_for_zero_iteration_mul_loop_is_allowed(sim) {
    @setup { let code = r#"
        module Top (
            clk: input clock,
            q: output logic<8>
        ) {
            always_ff (clk) {
                q = 8'haa;
                for i in 0..0 step *= 2 {
                    q = i;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let q = sim.signal("q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xaau32.into());
}



fn test_ff_runtime_reverse_step_matches_emitted_sv_order(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input signed logic<64>,
            end_bound: input signed logic<64>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in rev start..end_bound step += 2 {
                    q = i as 32;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(start, 0u64);
        io.set(end_bound, 10u64);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    // The last iteration is i = 1 for the emitted 9,7,5,3,1 order.
    assert_eq!(sim.get(q), 1u32.into());
}



fn test_ff_runtime_reverse_exclusive_i32_upper_sentinel(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input signed logic<64>,
            end_bound: input signed logic<64>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in rev start..end_bound {
                    q = i as 32;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(start, 2147483640u64);
        io.set(end_bound, 2147483648u64);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 2147483640u32.into());
}



fn test_ff_runtime_reverse_min_i32_end_wraps_before_range_check(sim) {
    // veryl-simulator 0.20.2's non-JIT interpreter evaluates `end - 1` as i64
    // without truncating it to the signed 32-bit loop-counter width. It therefore
    // gets -2147483649 instead of wrapping to i32::MAX and skips the loop body.

    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input signed logic<64>,
            end_bound: input signed logic<64>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in rev start..end_bound {
                    q = i as 32;
                    break;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(start, (-2147483648i64) as u64);
        io.set(end_bound, (-2147483648i64) as u64);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x7fff_ffffu32.into());
}







fn test_ff_runtime_for_reverse_singleton_exits_cleanly(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input logic<8>,
            count: input logic<8>,
            q: output logic<8>
        ) {
            always_ff (clk) {
                q = 8'hee;
                for i in rev start..=count {
                    q = i as 8;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let count = sim.signal("count");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(start, 4u8);
        io.set(count, 4u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 4u32.into());
}



fn test_ff_runtime_for_signed_inclusive_range_preserves_negative_bounds(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input signed logic<32>,
            count: input signed logic<32>,
            q_last: output logic<32>
        ) {
            always_ff (clk) {
                q_last = 32'hdead_beef;
                for i in start..=count {
                    q_last = i as 32;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let count = sim.signal("count");
    let q_last = sim.signal("q_last");

    sim.modify(|io| {
        io.set(start, 0xffff_ffffu32);
        io.set(count, 1u32);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_last), 1u32.into());
}



fn test_ff_runtime_for_forward_overshoot_exits_without_wraparound(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input logic<8>,
            q_hits: output logic<8>,
            q_last: output logic<8>
        ) {
            always_ff (clk) {
                q_hits = 0;
                q_last = 8'hee;
                for i in start..255 step += 10 {
                    q_hits += 1;
                    q_last = i;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let q_hits = sim.signal("q_hits");
    let q_last = sim.signal("q_last");

    sim.modify(|io| io.set(start, 250u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_hits), 1u32.into());
    assert_eq!(sim.get(q_last), 250u32.into());
}



fn test_ff_runtime_for_unsigned_slice_bound_zero_extends_signed_source(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input signed logic<16>,
            q_last: output logic<8>
        ) {
            always_ff (clk) {
                q_last = 8'hee;
                for i in start[7:0]..=8'hff {
                    q_last = i as 8;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let q_last = sim.signal("q_last");

    sim.modify(|io| io.set(start, 0xffffu16)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_last), 255u32.into());
}



fn test_ff_if_reset_basic(sim) {
    @setup { let code = r#"
        module Top (clk: input clock, rst: input reset, d: input logic<8>, q: output logic<8>) {
            always_ff (clk, rst) {
                if_reset {
                    q = 0;
                } else {
                    q = d;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    // Reset (AsyncLow: active when rst=0)
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 0xAAu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x0u32.into());

    // Normal operation (deactivate reset)
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xAAu32.into());
}



fn test_async_reset(sim) {

    @setup { let code = r#"
        module Top (clk: input clock, rst: input reset_async_high, d: input logic<8>, q: output logic<8>) {
            always_ff (clk, rst) {
                if_reset {
                    q = 8'h55;
                } else {
                    q = d;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let rst_event = sim.event("rst");
    let rst_port = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    // Async reset trigger
    sim.modify(|io| io.set(rst_port, 1u8)).unwrap();
    sim.tick(rst_event).unwrap();
    assert_eq!(sim.get(q), 0x55u32.into());

    // Stay reset even if d changes
    sim.modify(|io| io.set(d, 0xFFu8)).unwrap();
    assert_eq!(sim.get(q), 0x55u32.into());

    // Release reset (should stay 0x55 because no clock or active reset edge)
    sim.modify(|io| io.set(rst_port, 0u8)).unwrap();
    assert_eq!(sim.get(q), 0x55u32.into());
}



fn test_ff_swap_correctness(sim) {
    @setup { let code = r#"
        module Top (clk: input clock, rst: input reset, a: output logic<8>, b: output logic<8>) {
            var r1: logic<8>;
            var r2: logic<8>;
            always_ff (clk, rst) {
                if_reset {
                    r1 = 8'hAA;
                    r2 = 8'h55;
                } else {
                    r1 = r2;
                    r2 = r1;
                }
            }
            assign a = r1;
            assign b = r2;
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let b = sim.signal("b");

    // Reset to initialize (AsyncLow: active when rst=0)
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(a), 0xAAu32.into());
    assert_eq!(sim.get(b), 0x55u32.into());

    // Tick to swap (deactivate reset)
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();

    assert_eq!(sim.get(a), 0x55u32.into());
    assert_eq!(sim.get(b), 0xAAu32.into());
}



fn test_multiple_clocks(sim) {
    @setup { let code = r#"
        module Top (clk1: input 'a clock, clk2: input 'b clock, d1: input 'a logic<8>, d2: input 'b logic<8>, q1: output 'a logic<8>, q2: output 'b logic<8>) {
            always_ff (clk1) { q1 = d1; }
            always_ff (clk2) { q2 = d2; }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk1 = sim.event("clk1");
    let clk2 = sim.event("clk2");
    let d1 = sim.signal("d1");
    let d2 = sim.signal("d2");
    let q1 = sim.signal("q1");
    let q2 = sim.signal("q2");

    sim.modify(|io| {
        io.set(d1, 0x11u8);
        io.set(d2, 0x22u8);
    })
    .unwrap();

    sim.tick(clk1).unwrap();
    assert_eq!(sim.get(q1), 0x11u32.into());
    assert_eq!(sim.get(q2), 0x0u32.into());

    sim.tick(clk2).unwrap();
    assert_eq!(sim.get(q2), 0x22u32.into());
}



fn test_hierarchical_clocks(sim) {
    @setup { let code = r#"
        module Sub (clk: input clock, d: input logic<8>, q: output logic<8>) {
            always_ff (clk) { q = d; }
        }
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            inst s: Sub (clk, d, q);
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0xFEu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xFEu32.into());
}



fn test_multiple_async_resets(sim) {

    @setup { let code = r#"
        module Top (clk: input clock, rst1: input reset_async_high, rst2: input reset_async_high, d: input logic<8>, q: output logic<8>) {
            var r1: logic<8>;
            var r2: logic<8>;

            always_ff (clk, rst1) {
                if_reset {
                    r1 = 8'h0A;
                } else {
                    r1 = d;
                }
            }
            always_ff (clk, rst2) {
                if_reset {
                    r2 = 8'h0B;
                } else {
                    r2 = d;
                }
            }
            assign q = r1 | r2; // dummy use
        }
    "#; }
    @build Design::new(code, "Top");
    let rst1_event = sim.event("rst1");
    let rst1_port = sim.signal("rst1");
    let rst2_event = sim.event("rst2");
    let rst2_port = sim.signal("rst2");
    let r1 = sim.signal("r1");
    let r2 = sim.signal("r2");

    sim.modify(|io| io.set(rst2_port, 1u8)).unwrap();
    sim.tick(rst2_event).unwrap();
    assert_eq!(sim.get(r2), 0x0Bu32.into());

    sim.modify(|io| io.set(rst1_port, 1u8)).unwrap();
    sim.tick(rst1_event).unwrap();
    assert_eq!(sim.get(r1), 0x0Au32.into());
}



fn test_ff_if_reset_multi_cycle(sim) {
    @setup { let code = r#"
        module Top (clk: input clock, rst: input reset, q: output logic<8>) {
            always_ff (clk, rst) {
                if_reset {
                    q = 0;
                } else {
                    q = q + 1;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let q = sim.signal("q");

    // Deactivate reset first (AsyncLow: rst=1 means inactive)
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u32.into());
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 2u32.into());

    // Activate reset (AsyncLow: rst=0 means active)
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u32.into());
}



fn test_ff_if_reset_with_nested_if(sim) {
    @setup { let code = r#"
        module Top (clk: input clock, rst: input reset, en: input logic, q: output logic<8>) {
            always_ff (clk, rst) {
                if_reset {
                    q = 0;
                } else {
                    if en {
                        q = q + 1;
                    }
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let en = sim.signal("en");
    let q = sim.signal("q");

    // Deactivate reset (AsyncLow: rst=1 means inactive)
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(en, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u32.into());

    sim.modify(|io| io.set(en, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u32.into());
}



fn test_ff_struct_constructor_expression(sim) {

    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, in_b: input logic<8>, out_a: output logic<8>, out_b: output logic<8>) {
            struct S {
                a: logic<8>,
                b: logic<8>,
            }
            var r: S;
            always_ff (clk) {
                r.a = in_a;
                r.b = in_b;
            }
            assign out_a = r.a;
            assign out_b = r.b;
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let in_b = sim.signal("in_b");
    let out_a = sim.signal("out_a");
    let out_b = sim.signal("out_b");

    sim.modify(|io| {
        io.set(in_a, 0x12u8);
        io.set(in_b, 0x34u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_a), 0x12u32.into());
    assert_eq!(sim.get(out_b), 0x34u32.into());
}



fn test_ff_struct_constructor_expression_literal_order(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            in_a: input logic<8>,
            in_b: input logic<8>,
            out_a: output logic<8>,
            out_b: output logic<8>
        ) {
            struct S {
                a: logic<8>,
                b: logic<8>,
            }
            var r: S;
            always_ff (clk) {
                r = S'{a: in_a, b: in_b};
            }
            assign out_a = r.a;
            assign out_b = r.b;
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let in_b = sim.signal("in_b");
    let out_a = sim.signal("out_a");
    let out_b = sim.signal("out_b");

    sim.modify(|io| {
        io.set(in_a, 0x12u8);
        io.set(in_b, 0x34u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_a), 0x12u32.into());
    assert_eq!(sim.get(out_b), 0x34u32.into());
}



fn test_ff_struct_constructor_signed_member_extension(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            in_neg: input i8,
            out_pad: output i16
        ) {
            struct S {
                x: i16,
            }
            var r: S;
            always_ff (clk) {
                r = S'{x: in_neg};
            }
            assign out_pad = r.x;
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_neg = sim.signal("in_neg");
    let out_pad = sim.signal("out_pad");

    sim.modify(|io| io.set(in_neg, 0xFFu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_pad), 0xFFFFu32.into());
}



fn test_ff_array_literal_expression_order(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            o0: output logic<8>,
            o1: output logic<8>
        ) {
            var r: logic<8>[2];
            always_ff (clk) {
                r = '{8'h12, 8'h34};
            }
            assign o0 = r[0];
            assign o1 = r[1];
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o0), 0x12u32.into());
    assert_eq!(sim.get(o1), 0x34u32.into());
}



fn test_ff_array_literal_default_expression(sim) {

    @setup { let code = r#"
        module Top (clk: input clock, in_data: input logic<8>, out_data: output logic<8>[4]) {
            var r: logic<8>[4];
            always_ff (clk) {
                r = '{default: in_data};
            }
            assign out_data = r;
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_data = sim.signal("in_data");
    let out_data = sim.signal("out_data");

    sim.modify(|io| io.set(in_data, 0x55u8)).unwrap();
    sim.tick(clk).unwrap();
    let q_val = sim.get(out_data);
    for i in 0..4 {
        let bit_val = (q_val.clone() >> (i * 8)) & BigUint::from(0xFFu32);
        assert_eq!(bit_val, 0x55u32.into());
    }
}



fn test_ff_array_literal_nested_default_multidim_expression(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            in_data: input logic<8>,
            o00: output logic<8>,
            o01: output logic<8>,
            o10: output logic<8>,
            o11: output logic<8>
        ) {
            var r: logic<8> [2, 2];
            always_ff (clk) {
                r = '{default: '{default: in_data}};
            }
            assign o00 = r[0][0];
            assign o01 = r[0][1];
            assign o10 = r[1][0];
            assign o11 = r[1][1];
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_data = sim.signal("in_data");
    let o00 = sim.signal("o00");

    sim.modify(|io| io.set(in_data, 0xAAu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o00), 0xAAu32.into());
}



fn test_ff_function_call_expression(sim) {
    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q: output logic<8>) {
            function f (x: input logic<8>) -> logic<8> {
                return x + 1;
            }
            always_ff (clk) {
                out_q = f(in_a);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 10u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 11u32.into());
}



fn test_ff_function_call_statement_with_output_argument(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q: output logic<8>) {
            function f (x: input logic<8>, y: output logic<8>) {
                y = x + 2;
            }
            var tmp: logic<8>;
            always_ff (clk) {
                f(in_a, tmp);
                out_q = tmp;
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 10u8)).unwrap();
    sim.tick(clk).unwrap();
    // 1st tick: tmp becomes (10+2)=12, out_q reads OLD tmp (0)
    assert_eq!(sim.get(out_q), 0u32.into());
    sim.tick(clk).unwrap();
    // 2nd tick: out_q reads 12
    assert_eq!(sim.get(out_q), 12u32.into());
}



fn test_ff_function_call_statement_with_output_argument_and_return_value(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q1: output logic<8>, out_q2: output logic<8>) {
            function f (x: input logic<8>, y: output logic<8>) -> logic<8> {
                y = x + 3;
                return x + 4;
            }
            var tmp: logic<8>;
            always_ff (clk) {
                out_q1 = f(in_a, tmp);
                out_q2 = tmp;
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q1 = sim.signal("out_q1");
    let out_q2 = sim.signal("out_q2");

    sim.modify(|io| io.set(in_a, 100u8)).unwrap();
    sim.tick(clk).unwrap();
    // After 1st tick: out_q1=104, tmp=103, out_q2=0 (old tmp)
    assert_eq!(sim.get(out_q1), 104u32.into());
    assert_eq!(sim.get(out_q2), 0u32.into());
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q2), 103u32.into());
}



fn test_ff_function_call_expression_with_output_argument_and_return_value(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q1: output logic<8>, out_q2: output logic<8>) {
            function f (x: input logic<8>, y: output logic<8>) -> logic<8> {
                y = x + 5;
                return x + 6;
            }
            var tmp: logic<8>;
            always_ff (clk) {
                out_q1 = f(in_a, tmp) + 1;
                out_q2 = tmp + 1;
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q1 = sim.signal("out_q1");
    let out_q2 = sim.signal("out_q2");

    sim.modify(|io| io.set(in_a, 50u8)).unwrap();
    sim.tick(clk).unwrap();
    // 1st tick: out_q1 = (50+6)+1 = 57, out_q2 = 0+1 = 1
    assert_eq!(sim.get(out_q1), 57u32.into());
    assert_eq!(sim.get(out_q2), 1u32.into());
    sim.tick(clk).unwrap();
    // 2nd tick: out_q2 = (50+5)+1 = 56
    assert_eq!(sim.get(out_q2), 56u32.into());
}



fn test_ff_function_call_expression_with_if(sim) {
    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, sel: input logic, out_q: output logic<8>) {
            function f (x: input logic<8>) -> logic<8> {
                return x + 1;
            }
            always_ff (clk) {
                if sel {
                    out_q = f(in_a);
                } else {
                    out_q = 0;
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let sel = sim.signal("sel");
    let out_q = sim.signal("out_q");

    sim.modify(|io| {
        io.set(in_a, 20u8);
        io.set(sel, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 21u32.into());

    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0u32.into());
}



fn test_ff_nested_function_call_expression(sim) {
    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q: output logic<8>) {
            function f (x: input logic<8>) -> logic<8> {
                return x + 1;
            }
            function g (x: input logic<8>) -> logic<8> {
                return f(x) * 2;
            }
            always_ff (clk) {
                out_q = g(in_a);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    // (5+1)*2 = 12
    assert_eq!(sim.get(out_q), 12u32.into());
}



fn test_ff_function_call_multistatement_body(sim) {
    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q: output logic<8>) {
            function f (x: input logic<8>) -> logic<8> {
                var tmp: logic<8>;
                tmp = x + 1;
                tmp = tmp * 2;
                return tmp;
            }
            always_ff (clk) {
                out_q = f(in_a);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 3u8)).unwrap();
    sim.tick(clk).unwrap();
    // (3+1)*2 = 8
    assert_eq!(sim.get(out_q), 8u32.into());
}



fn test_ff_function_call_indexed_argument_access(sim) {

    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<8>[4], out_q: output logic<8>) {
            function f (x: input logic<8>[4]) -> logic<8> {
                return x[2];
            }
            always_ff (clk) {
                out_q = f(in_a);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| {
        let mut val = BigUint::from(0u32);
        val |= BigUint::from(0xBEu32) << 16;
        io.set_wide(in_a, val);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0xBEu32.into());
}



fn test_ff_function_call_nested_output_statement_in_function_body(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q: output logic<8>) {
            function f (x: input logic<8>, y: output logic<8>) {
                y = x + 1;
            }
            function g (x: input logic<8>, y: output logic<8>) {
                f(x, y);
            }
            var tmp: logic<8>;
            always_ff (clk) {
                g(in_a, tmp);
                out_q = tmp;
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 7u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0u32.into());
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 8u32.into());
}



fn test_ff_function_call_indexed_nonvariable_argument_expression(sim) {
    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<4>, out_q: output logic) {
            function f (x: input logic<4>) -> logic {
                return x[1];
            }
            always_ff (clk) {
                out_q = f(in_a + 4'b0001);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 0b0010u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 1u32.into());

    sim.modify(|io| io.set(in_a, 0b0101u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 1u32.into());
}



fn test_ff_function_call_chained_range_access_on_argument(sim) {
    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q: output logic<4>) {
            function f (x: input logic<8>) -> logic<4> {
                return x[5:2];
            }
            always_ff (clk) {
                out_q = f(in_a[7:0]);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 0b1101_0110u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0b0101u32.into());
}



fn test_ff_function_call_step_access_on_nonvariable_argument(sim) {

    @setup { let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q: output logic<4>) {
            function f (x: input logic<8>) -> logic<4> {
                return x[1 step 4];
            }
            always_ff (clk) {
                out_q = f(in_a + 8'b0000_0001);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 0b1010_0100u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0b1010u32.into());
}



fn test_ff_function_call_nonvariable_argument_uses_formal_width_before_slice(sim) {
    @setup { let code = r#"
        module Top (clk: input clock, out_q: output logic) {
            function f (x: input logic<4>) -> logic {
                return x[3];
            }
            always_ff (clk) {
                out_q = f('1);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 1u32.into());
}



fn test_ff_function_call_nonvariable_argument_preserves_self_sized_overflow_before_coercion(sim) {
    // Legacy ID retained. IEEE 1800-2023 10.8 and 11.8.2 make the formal an
    // assignment-like context: widen the operands before adding, yielding 4.

    @setup { let code = r#"
        module Top (clk: input clock, out_q: output logic) {
            function f (x: input logic<4>) -> logic {
                return x[2];
            }
            always_ff (clk) {
                out_q = f(2'b11 + 2'b01);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 1u32.into());
}



fn test_ff_function_call_part_select_of_signed_formal_is_unsigned(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            in_a: input signed logic<8>,
            out_direct: output signed logic<8>,
            out_expr: output signed logic<8>
        ) {
            function f (x: input signed logic<8>) -> signed logic<8> {
                return x[7:0] >>> 1;
            }
            always_ff (clk) {
                out_direct = f(in_a);
                out_expr = f(in_a + 0);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_direct = sim.signal("out_direct");
    let out_expr = sim.signal("out_expr");

    sim.modify(|io| io.set(in_a, 0xFEu8)).unwrap();
    sim.tick(clk).unwrap();
    // A packed part-select is unsigned even when its base is signed, so >>>
    // performs a logical shift here.
    assert_eq!(sim.get(out_direct), 0x7Fu32.into());
    assert_eq!(sim.get(out_expr), 0x7Fu32.into());
}



fn test_ff_function_call_sign_extends_narrow_signed_actual_before_slice(sim) {
    @setup { let code = r#"
        module Top (
            clk: input clock,
            out_q: output signed logic<8>
        ) {
            function f (x: input signed logic<8>) -> signed logic<8> {
                return x >>> 4;
            }
            always_ff (clk) {
                out_q = f(4'shf);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0xFFu32.into());
}



fn test_ff_function_call_preserves_unsigned_actual_when_widening_to_signed_formal(sim) {
    @setup { let code = r#"
        module Top (
            clk: input clock,
            out_q: output logic
        ) {
            function f (x: input signed logic<8>) -> logic {
                return x[7];
            }
            always_ff (clk) {
                out_q = f(4'hf);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0u32.into());
}



fn test_ff_function_call_preserves_unsigned_formal_signedness_for_nonvariable_actual(sim) {
    @setup { let code = r#"
        module Top (
            clk: input clock,
            in_a: input signed logic<8>,
            out_q: output logic<8>
        ) {
            function f (x: input logic<8>) -> logic<8> {
                return x[7:0] >>> 1;
            }
            always_ff (clk) {
                out_q = f(in_a + 0);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 0xFEu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x7Fu32.into());
}



fn test_ff_function_call_nonvariable_argument_uses_formal_shape_for_indexing(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            in_hi: input logic<4>,
            in_lo: input logic<4>,
            out_q: output logic<4>
        ) {
            function f (x: input logic<4>[2]) -> logic<4> {
                return x[1];
            }
            always_ff (clk) {
                out_q = f('{in_hi, in_lo});
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_hi = sim.signal("in_hi");
    let in_lo = sim.signal("in_lo");
    let out_q = sim.signal("out_q");

    sim.modify(|io| {
        io.set(in_hi, 0xAu8);
        io.set(in_lo, 0x3u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x3u32.into());
}



fn test_ff_function_call_array_literal_element_uses_formal_context_width(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            in0: input logic<4>,
            in1: input logic<4>,
            out_q: output logic<8>
        ) {
            function f (x: input logic<8>[1]) -> logic<8> {
                return x[0];
            }
            always_ff (clk) {
                out_q = f('{in0 + in1});
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in0 = sim.signal("in0");
    let in1 = sim.signal("in1");
    let out_q = sim.signal("out_q");

    sim.modify(|io| {
        io.set(in0, 0xFu8);
        io.set(in1, 0xFu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x1Eu32.into());
}



fn test_ff_function_call_array_literal_supports_dynamic_multidim_indexing(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            row: input logic,
            col: input logic,
            out_q: output logic<8>
        ) {
            function f (
                x: input logic<8>[2, 2],
                i: input logic,
                j: input logic
            ) -> logic<8> {
                return x[i][j];
            }
            always_ff (clk) {
                out_q = f(
                    '{'{8'h11, 8'h22} repeat 1, default: '{8'h33, 8'h44}},
                    row,
                    col
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let row = sim.signal("row");
    let col = sim.signal("col");
    let out_q = sim.signal("out_q");

    for (i, j, expected) in [
        (0u8, 0u8, 0x11u32),
        (0, 1, 0x22),
        (1, 0, 0x33),
        (1, 1, 0x44),
    ] {
        sim.modify(|io| {
            io.set(row, i);
            io.set(col, j);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(out_q), expected.into());
    }
}



fn test_ff_function_call_array_literal_view_dominates_conditional_access(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            out_q: output logic<8>
        ) {
            function f (x: input logic<8>[2], guard: input logic) -> logic<8> {
                var first: logic<8>;
                first = if guard ? x[0] : 8'h00;
                return first + x[1];
            }
            always_ff (clk) {
                out_q = f('{8'h11, 8'h22}, guard);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(guard, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x22u32.into());

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x33u32.into());
}



fn test_ff_function_call_array_literal_effect_is_eager_in_ternary_arm(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            in0: input logic<8>,
            out_q: output logic<8>,
            side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function pick_if (
                x: input logic<8>[2],
                index: input logic,
                guard: input logic
            ) -> logic<8> {
                return if guard ? x[index] : 8'h00;
            }
            always_ff (clk) {
                out_q = pick_if(
                    '{observe(in0, side), default: 8'h00},
                    0,
                    guard
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let in0 = sim.signal("in0");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.modify(|io| {
        io.set(guard, 0u8);
        io.set(in0, 0x5au8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0u32.into());
    assert_eq!(sim.get(side), 0x5au32.into());

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x5au32.into());
    assert_eq!(sim.get(side), 0x5au32.into());
}



fn test_ff_function_call_array_literal_effect_is_eager_in_short_circuit_rhs(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            in0: input logic<8>,
            out_and: output logic,
            out_or: output logic,
            and_side: output logic<8>,
            or_side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function pick_and (
                x: input logic<8>[2],
                index: input logic,
                guard: input logic
            ) -> logic {
                return guard && x[index] != 0;
            }
            function pick_or (
                x: input logic<8>[2],
                index: input logic,
                guard: input logic
            ) -> logic {
                return guard || x[index] != 0;
            }
            always_ff (clk) {
                and_side = 0;
                or_side = 0;
                out_and = pick_and(
                    '{observe(in0, and_side), default: 8'h00},
                    0,
                    guard
                );
                out_or = pick_or(
                    '{observe(in0, or_side), default: 8'h00},
                    0,
                    guard
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let in0 = sim.signal("in0");
    let out_and = sim.signal("out_and");
    let out_or = sim.signal("out_or");
    let and_side = sim.signal("and_side");
    let or_side = sim.signal("or_side");

    sim.modify(|io| {
        io.set(guard, 0u8);
        io.set(in0, 0x5au8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_and), 0u32.into());
    assert_eq!(sim.get(and_side), 0x5au32.into());
    assert_eq!(sim.get(out_or), 1u32.into());
    assert_eq!(sim.get(or_side), 0x5au32.into());

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_and), 1u32.into());
    assert_eq!(sim.get(and_side), 0x5au32.into());
    assert_eq!(sim.get(out_or), 1u32.into());
    assert_eq!(sim.get(or_side), 0x5au32.into());
}



fn test_ff_function_call_array_literal_view_preserves_expression_order(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            out_q: output logic<8>,
            side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function ordered (
                x: input logic<8>[2],
                index: input logic,
                left: input logic<8>
            ) -> logic<8> {
                return left + x[index];
            }
            always_ff (clk) {
                out_q = ordered(
                    '{observe(8'h22, side), default: 8'h00},
                    0,
                    observe(8'h11, side)
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x33u32.into());
    assert_eq!(sim.get(side), 0x11u32.into());
}



fn test_ff_function_call_array_literal_snapshots_scalar_before_later_write(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            changing: output logic<8>,
            out_q: output logic<8>
        ) {
            function pick (
                values: input logic<8>[2],
                ignored: input logic<8>
            ) -> logic<8> {
                return values[0];
            }

            function update (
                value: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = value + 8'd1;
                return 8'h00;
            }

            always_ff (clk) {
                out_q = pick('{changing, default: 8'h00}, update(changing, changing));
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let changing = sim.signal("changing");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(changing, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(changing), 6u8.into());
    assert_eq!(sim.get(out_q), 5u8.into());
}



fn test_ff_function_call_array_literal_snapshots_scalar_before_callee_write(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            global_value: output logic<8>,
            out_q: output logic<8>
        ) {
            function mutate_global () -> logic {
                global_value = 8'd9;
                return 1'b0;
            }

            function pick (values: input logic<8>[2]) -> logic<8> {
                var ignored: logic;
                ignored = mutate_global();
                return values[0];
            }

            always_ff (clk) {
                out_q = pick('{global_value, default: 8'h00});
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(global_value, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 9u8.into());
    assert_eq!(sim.get(out_q), 5u8.into());
}



fn test_ff_function_call_array_literal_branch_view_is_reused_after_merge(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            in0: input logic<8>,
            out_q: output logic<8>
        ) {
            function pick_then_first (
                x: input logic<8>[2],
                index: input logic,
                guard: input logic,
                first: output logic<8>
            ) -> logic<8> {
                first = if guard ? x[index] : 8'h00;
                return x[0];
            }
            var first: logic<8>;
            always_ff (clk) {
                out_q = pick_then_first(
                    '{in0, default: 8'h00},
                    0,
                    guard,
                    first
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let in0 = sim.signal("in0");
    let out_q = sim.signal("out_q");

    for guard_value in [0u8, 1u8] {
        sim.modify(|io| {
            io.set(guard, guard_value);
            io.set(in0, 0x5au8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(out_q), 0x5au32.into());
    }
}



fn test_ff_function_call_effectful_array_items_are_eager_before_conditional_access(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            out_q: output logic<8>,
            side0: output logic<8>,
            side1: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function static_then_dynamic (
                x: input logic<8>[2],
                index: input logic,
                guard: input logic,
                first: output logic<8>
            ) -> logic<8> {
                first = x[0];
                return if guard ? x[index] : 8'h00;
            }
            var first: logic<8>;
            always_ff (clk) {
                side0 = 0;
                side1 = 0;
                out_q = static_then_dynamic(
                    '{observe(8'h11, side0), observe(8'h22, side1)},
                    1,
                    guard,
                    first
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let out_q = sim.signal("out_q");
    let side0 = sim.signal("side0");
    let side1 = sim.signal("side1");

    sim.modify(|io| io.set(guard, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0u32.into());
    assert_eq!(sim.get(side0), 0x11u32.into());
    assert_eq!(sim.get(side1), 0x22u32.into());

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x22u32.into());
    assert_eq!(sim.get(side0), 0x11u32.into());
    assert_eq!(sim.get(side1), 0x22u32.into());
}



fn test_ff_function_call_carries_branch_local_static_array_item_cache(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            out_q: output logic<8>,
            side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function branch_then_static (
                x: input logic<8>[2],
                guard: input logic,
                first: output logic<8>
            ) -> logic<8> {
                first = if guard ? x[0] : 8'h00;
                return x[0];
            }
            var first: logic<8>;
            always_ff (clk) {
                side = 0;
                out_q = branch_then_static(
                    '{observe(side + 1, side), default: 8'h00},
                    guard,
                    first
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 1u32.into());
    assert_eq!(sim.get(side), 1u32.into());
}



fn test_ff_function_call_tracks_nested_static_array_read_through_branch(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            out_q: output logic<8>,
            side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function inner (x: input logic<8>[2]) -> logic<8> {
                return x[0];
            }
            function nested_then_static (
                x: input logic<8>[2],
                guard: input logic,
                first: output logic<8>
            ) -> logic<8> {
                first = if guard ? inner(x) : 8'h00;
                return x[0];
            }
            var first: logic<8>;
            always_ff (clk) {
                side = 0;
                out_q = nested_then_static(
                    '{observe(side + 1, side), default: 8'h00},
                    guard,
                    first
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 1u32.into());
    assert_eq!(sim.get(side), 1u32.into());
}



fn test_ff_function_call_tracks_array_view_hidden_in_bound_literal(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            out_q: output logic<8>,
            side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function middle (
                x: input logic<8>[2],
                index: input logic,
                guard: input logic
            ) -> logic<8> {
                return if guard ? x[index] : 8'h00;
            }
            function outer (
                y: input logic<8>[2],
                index: input logic,
                guard: input logic
            ) -> logic<8> {
                return middle('{y[index], default: 8'h00}, 0, guard) + y[0];
            }
            always_ff (clk) {
                side = 0;
                out_q = outer(
                    '{observe(side + 1, side), default: 8'h00},
                    0,
                    guard
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 2u32.into());
    assert_eq!(sim.get(side), 1u32.into());
}



fn test_ff_function_call_merges_nested_array_state_at_cache_completion(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            out_q: output logic<8>,
            side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function middle (
                x: input logic<8>[2],
                index: input logic,
                guard: input logic
            ) -> logic<8> {
                return (if guard ? x[0] : 8'h00) + x[index];
            }
            function outer (
                y: input logic<8>[2],
                index: input logic,
                guard: input logic
            ) -> logic<8> {
                return middle(
                    '{y[index], default: 8'h00},
                    0,
                    guard
                ) + y[0];
            }
            always_ff (clk) {
                side = 0;
                out_q = outer(
                    '{observe(side + 1, side), default: 8'h00},
                    0,
                    guard
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 3u32.into());
    assert_eq!(sim.get(side), 1u32.into());
}



fn test_ff_function_call_merges_nested_array_state_at_static_cache_completion(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            out_q: output logic<8>,
            side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function middle (
                x: input logic<8>[2],
                guard: input logic
            ) -> logic<8> {
                return (if guard ? x[0] : 8'h00) + x[0];
            }
            function outer (
                y: input logic<8>[2],
                index: input logic,
                guard: input logic
            ) -> logic<8> {
                return middle(
                    '{y[index], default: 8'h00},
                    guard
                ) + y[0];
            }
            always_ff (clk) {
                side = 0;
                out_q = outer(
                    '{observe(side + 1, side), default: 8'h00},
                    0,
                    guard
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 3u32.into());
    assert_eq!(sim.get(side), 1u32.into());
}



fn test_ff_function_call_merges_directly_forwarded_array_cache(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            out_q: output logic<8>,
            side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function inner (
                x: input logic<8>[2],
                guard: input logic
            ) -> logic<8> {
                return if guard ? x[0] : 8'h00;
            }
            function outer (
                y: input logic<8>[2],
                guard: input logic,
                middle: input logic<8>
            ) -> logic<8> {
                return inner(y, guard) + middle + y[0];
            }
            always_ff (clk) {
                out_q = outer(
                    '{observe(8'h11, side), default: 8'h00},
                    guard,
                    observe(8'h55, side)
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.modify(|io| io.set(guard, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x66u32.into());
    assert_eq!(sim.get(side), 0x55u32.into());

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x77u32.into());
    // Both arguments are eager effects, so declaration order makes the
    // later scalar actual supply the final write.
    assert_eq!(sim.get(side), 0x55u32.into());
}



fn test_ff_function_call_tracks_array_reads_in_output_indices(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            out_q: output logic<8>,
            side: output logic<8>
        ) {
            var scratch: logic<8>[2];
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function write (
                value: input logic<8>,
                dst: output logic<8>
            ) -> logic<8> {
                dst = value;
                return 0;
            }
            function outer (
                x: input logic<8>[2],
                guard: input logic,
                middle: input logic<8>
            ) -> logic<8> {
                return (if guard ? write(0, scratch[x[0]]) : 0) + middle + x[0];
            }
            always_ff (clk) {
                out_q = outer(
                    '{observe(8'h01, side), default: 8'h00},
                    guard,
                    observe(8'h55, side)
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.modify(|io| io.set(guard, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x56u32.into());
    assert_eq!(sim.get(side), 0x55u32.into());

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x56u32.into());
    assert_eq!(sim.get(side), 0x55u32.into());
}



fn test_ff_function_call_tracks_nested_array_reads_in_output_indices(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            out_q: output logic<8>,
            side: output logic<8>
        ) {
            var scratch: logic<8>[2];
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function write (
                value: input logic<8>,
                dst: output logic<8>
            ) -> logic<8> {
                dst = value;
                return 0;
            }
            function helper (x: input logic<8>[2]) -> logic<8> {
                return write(0, scratch[x[0]]);
            }
            function outer (
                x: input logic<8>[2],
                guard: input logic,
                middle: input logic<8>
            ) -> logic<8> {
                return (if guard ? helper(x) : 0) + middle + x[0];
            }
            always_ff (clk) {
                out_q = outer(
                    '{observe(8'h01, side), default: 8'h00},
                    guard,
                    observe(8'h55, side)
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.modify(|io| io.set(guard, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x56u32.into());
    assert_eq!(sim.get(side), 0x55u32.into());

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x56u32.into());
    assert_eq!(sim.get(side), 0x55u32.into());
}



fn test_ff_function_call_restores_initialized_forwarded_alias_view(sim) {
     // https://github.com/veryl-lang/veryl/pull/3131
    @setup { let code = r#"
        module Top (
            clk: input clock,
            select: input logic,
            out_q: output logic<8>
        ) {
            function alias_use (
                a: input logic<8>[2],
                b: input logic<8>[2],
                index: input logic,
                select: input logic,
                clobber: input logic<8>
            ) -> logic<8> {
                return (if select ? a[index] : b[index])
                    + clobber
                    + (if select ? a[0] : b[0]);
            }
            function forward (
                x: input logic<8>[2],
                index: input logic,
                select: input logic,
                clobber: input logic<8>
            ) -> logic<8> {
                return alias_use(x, x, index, select, clobber);
            }
            always_ff (clk) {
                out_q = forward(
                    '{8'h11, 8'h22},
                    1,
                    select,
                    forward('{8'haa, 8'hbb}, 1, 0, 0)
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let select = sim.signal("select");
    let out_q = sim.signal("out_q");

    for select_value in [0u8, 1u8] {
        sim.modify(|io| io.set(select, select_value)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(out_q), 0x98u32.into());
    }
}



fn test_ff_function_call_merges_outer_array_view_across_nested_short_circuit(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            guard: input logic,
            in0: input logic<8>,
            out_q: output logic<8>,
            side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function inner (y: input logic[2], index: input logic) -> logic {
                return y[index];
            }
            function outer (
                x: input logic<8>[2],
                index: input logic,
                guard: input logic
            ) -> logic<8> {
                return inner('{guard && x[index] != 0, default: 0}, 0) + x[0];
            }
            always_ff (clk) {
                side = 0;
                out_q = outer(
                    '{observe(in0, side), default: 8'h00},
                    0,
                    guard
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let guard = sim.signal("guard");
    let in0 = sim.signal("in0");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.modify(|io| {
        io.set(guard, 0u8);
        io.set(in0, 0x5au8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x5au32.into());
    assert_eq!(sim.get(side), 0x5au32.into());

    sim.modify(|io| io.set(guard, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x5bu32.into());
    assert_eq!(sim.get(side), 0x5au32.into());
}



fn test_ff_function_call_forwards_array_literal_view_to_nested_call(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            index: input logic,
            out_q: output logic<8>
        ) {
            function inner (x: input logic<8>[2], index: input logic) -> logic<8> {
                return x[index];
            }
            function middle (x: input logic<8>[2], index: input logic) -> logic<8> {
                return inner(x, index);
            }
            function outer (x: input logic<8>[2], index: input logic) -> logic<8> {
                return middle(x, index);
            }
            always_ff (clk) {
                out_q = outer('{8'h11, 8'h22}, index);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(index, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x11u32.into());

    sim.modify(|io| io.set(index, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x22u32.into());
}



fn test_ff_function_call_keeps_array_view_active_for_output_index(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            out_q: output logic<8>
        ) {
            function pick (
                x: input logic<8>[2],
                index: input logic,
                selected: output logic<8>
            ) -> logic<8> {
                selected = x[index];
                return x[0];
            }
            var selected: logic<8>[2];
            var inner_selected: logic<8>;
            always_ff (clk) {
                out_q = pick(
                    '{8'h11, 8'h22},
                    1,
                    selected[pick('{8'h01, 8'h00}, 0, inner_selected)]
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x11u32.into());
}



fn test_ff_function_call_restores_array_literal_view_after_reentrant_call(sim) {
     // https://github.com/veryl-lang/veryl/pull/3131
    @setup { let code = r#"
        module Top (
            clk: input clock,
            out_q: output logic<8>
        ) {
            function pick (x: input logic<8>[2], index: input logic) -> logic<8> {
                return x[index];
            }
            always_ff (clk) {
                out_q = pick('{8'h11, 8'h22}, pick('{8'h00, 8'h01}, 0));
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x11u32.into());
}



fn test_ff_function_call_restores_nearest_array_view_after_deep_reentrant_call(sim) {
     // https://github.com/veryl-lang/veryl/pull/3131
    @setup { let code = r#"
        module Top (
            clk: input clock,
            out_q: output logic<8>
        ) {
            function pick (x: input logic<8>[2], index: input logic) -> logic<8> {
                return x[index];
            }
            always_ff (clk) {
                out_q = pick(
                    '{8'h11, 8'h22},
                    pick(
                        '{8'h00, 8'h00},
                        pick('{8'h00, 8'h00}, 0)
                    )
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x11u32.into());
}



fn test_ff_function_call_bits_and_size_evaluate_effectful_array_argument(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            in0: input logic<8>,
            out_bits: output logic<32>,
            out_size: output logic<32>,
            bits_side: output logic<8>,
            size_side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function array_bits (x: input logic<8>[2]) -> logic<32> {
                return $bits(x);
            }
            function array_size (x: input logic<8>[2]) -> logic<32> {
                return $size(x);
            }
            always_ff (clk) {
                out_bits = array_bits('{observe(in0, bits_side), default: 0});
                out_size = array_size('{observe(in0, size_side), default: 0});
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in0 = sim.signal("in0");
    let out_bits = sim.signal("out_bits");
    let out_size = sim.signal("out_size");
    let bits_side = sim.signal("bits_side");
    let size_side = sim.signal("size_side");

    sim.modify(|io| io.set(in0, 0x5au8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_bits), 16u32.into());
    assert_eq!(sim.get(out_size), 2u32.into());
    assert_eq!(sim.get(bits_side), 0x5au32.into());
    assert_eq!(sim.get(size_side), 0x5au32.into());
}



fn test_ff_function_call_nested_bits_evaluates_effectful_array_argument(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            in0: input logic<8>,
            out_q: output logic<32>,
            side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function inner (x: input logic<8>[2]) -> logic<32> {
                return $bits(x);
            }
            function outer (x: input logic<8>[2]) -> logic<32> {
                return inner(x);
            }
            always_ff (clk) {
                out_q = outer('{observe(in0, side), default: 8'h00});
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in0 = sim.signal("in0");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.modify(|io| io.set(in0, 0x5au8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 16u32.into());
    assert_eq!(sim.get(side), 0x5au32.into());
}



fn test_ff_function_call_array_literal_view_preserves_source_order(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (
            clk: input clock,
            index: input logic,
            out_q: output logic<8>,
            side: output logic<8>
        ) {
            function observe (
                x: input logic<8>,
                side: output logic<8>
            ) -> logic<8> {
                side = x;
                return x;
            }
            function pick (x: input logic<8>[2], index: input logic) -> logic<8> {
                return x[index];
            }
            always_ff (clk) {
                out_q = pick(
                    '{default: observe(8'h11, side), observe(8'h22, side)},
                    index
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let side = sim.signal("side");

    sim.modify(|io| io.set(index, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(side), 0x22u32.into());
}



fn test_ff_function_call_snapshots_pure_array_items_before_later_effect(sim) {
    // Celox opt-in FF function effects are rejected by the Veryl analyzer.


    @setup { let code = r#"
        module Top (clk: input clock, q: output logic<8>, changing: output logic<8>) {
            function update (
                value: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = value + 1;
                return 0;
            }
            function pick (x: input logic<8>[2]) -> logic<8> {
                return x[0];
            }
            always_ff (clk) {
                q = pick('{changing, update(changing, changing)});
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let q = sim.signal("q");
    let changing = sim.signal("changing");

    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u32.into());
    assert_eq!(sim.get(changing), 2u32.into());
}



fn test_ff_function_call_converts_array_literal_view_for_wider_nested_formal(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            index: input logic,
            out_q: output logic<8>
        ) {
            function inner (x: input logic<8>[2], index: input logic) -> logic<8> {
                return x[index];
            }
            function outer (x: input logic<4>[2], index: input logic) -> logic<8> {
                return inner(x, index);
            }
            always_ff (clk) {
                out_q = outer('{4'ha, 4'h3}, index);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(index, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x0au32.into());

    sim.modify(|io| io.set(index, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x03u32.into());
}



fn test_ff_function_call_converts_forwarded_static_array_element(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            out_q: output logic<8>
        ) {
            function inner (x: input logic<8>[2]) -> logic<8> {
                return x[0];
            }
            function outer (x: input logic<4>[2]) -> logic<8> {
                return inner(x);
            }
            always_ff (clk) {
                out_q = outer('{4'ha, 4'h3});
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x0au32.into());
}



fn test_ff_function_call_array_literal_element_uses_element_width(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            out_q: output signed logic<4>
        ) {
            function f (x: input signed logic<4>[2]) -> signed logic<4> {
                return x[1] >>> 3;
            }
            always_ff (clk) {
                out_q = f('{4'sh1, 4'sh8});
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0xFu32.into());
}



fn test_ff_function_array_element_assignment_preserves_signedness(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            out_q: output signed logic<8>
        ) {
            function f () -> signed logic<8> {
                var values: signed logic<8>[2];
                values[0] = 4'sh8;
                return values[0] >>> 3;
            }
            always_ff (clk) {
                out_q = f();
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0xffu32.into());
}



fn test_ff_function_call_array_literal_default_fill_matches_formal_shape(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            out_q: output logic<8>
        ) {
            function f (x: input logic<8>[3]) -> logic<8> {
                return x[2];
            }
            always_ff (clk) {
                out_q = f('{default: 8'h55});
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x55u32.into());
}



fn test_ff_function_call_multidim_array_literal_default_fill_matches_formal_shape(sim) {
     // https://github.com/veryl-lang/veryl/pull/3131
    @setup { let code = r#"
        module Top (
            clk: input clock,
            out_q: output logic<8>
        ) {
            function f (x: input logic<8>[2, 2]) -> logic<8> {
                return x[1][1];
            }
            always_ff (clk) {
                out_q = f('{default: 8'h55});
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x55u32.into());
}



fn test_ff_function_call_multidim_array_literal_indexing_preserves_element_order(sim) {

    @setup { let code = r#"
        module Top (
            clk: input clock,
            out_q: output logic<8>
        ) {
            function f (x: input logic<8>[2, 2]) -> logic<8> {
                return x[0][0];
            }
            always_ff (clk) {
                out_q = f('{'{8'h11, 8'h22}, '{8'h33, 8'h44}});
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x11u32.into());
}



fn test_ff_function_call_dynamic_multidim_indexing_accepts_array_valued_items(sim) {
     // https://github.com/veryl-lang/veryl/pull/3131
    @setup { let code = r#"
        module Top (
            clk: input clock,
            row: input logic,
            col: input logic,
            out_q: output logic<8>
        ) {
            function pick (
                x: input logic<8>[2, 2],
                row: input logic,
                col: input logic
            ) -> logic<8> {
                return x[row][col];
            }
            function pass_rows (
                row0: input logic<4>[2],
                row1: input logic<4>[2],
                row: input logic,
                col: input logic
            ) -> logic<8> {
                return pick('{row0, row1}, row, col);
            }
            always_ff (clk) {
                out_q = pass_rows(
                    '{4'h1, 4'h2},
                    '{4'h3, 4'h4},
                    row,
                    col
                );
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let row = sim.signal("row");
    let col = sim.signal("col");
    let out_q = sim.signal("out_q");

    for (i, j, expected) in [
        (0u8, 0u8, 0x01u32),
        (0, 1, 0x02),
        (1, 0, 0x03),
        (1, 1, 0x04),
    ] {
        sim.modify(|io| {
            io.set(row, i);
            io.set(col, j);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(out_q), expected.into());
    }
}



fn test_ff_function_call_bit_select_on_nonvariable_one_bit_formal(sim) {
    @setup { let code = r#"
        module Top (
            clk: input clock,
            in_a: input logic,
            out_q: output logic
        ) {
            function f (x: input logic) -> logic {
                return x[0];
            }
            always_ff (clk) {
                out_q = f(in_a | 1'b0);
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 1u32.into());
}
}
