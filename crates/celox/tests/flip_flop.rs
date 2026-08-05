use celox::{BigUint, Simulation, Simulator, SimulatorBuilder};
use insta::assert_snapshot;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

fn setup_and_trace(code: &str, top: &str) -> celox::CompilationTrace {
    let result = SimulatorBuilder::new(code, top)
        .optimize(true)
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .build_with_trace();

    result.trace
}

all_backends! {

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
    @build Simulator::builder(code, "Top");
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

fn test_ff_static_and_dynamic_writes_share_sparse_state(sim) {
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
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

fn test_ff_runtime_display_and_assert_continue(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, a: input logic<8>, q: output logic<8>) {
            always_ff (clk) {
                q = a;
                $display("a=%0d", a);
                $assert_continue(a != 8'd3, "bad a=%0d", a);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let a = sim.signal("a");

    sim.modify(|io| io.set(a, 3u8)).unwrap();
    sim.tick(clk).unwrap();
    let events = sim.drain_runtime_events();
    assert_eq!(
        events,
        vec![
            celox::RuntimeEvent::Display {
                message: "a=3".to_string(),
            },
            celox::RuntimeEvent::AssertContinue {
                message: "bad a=3".to_string(),
            },
        ],
    );
}

fn test_ff_assert_message_output_argument_is_eager(sim) {
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
    @build Simulator::builder(code, "Top");
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

fn test_ff_assert_message_runtime_effect_is_eager(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, ok: input logic, d: input logic<8>) {
            function message_value (x: input logic<8>) -> logic<8> {
                $display("inside=%0d", x);
                return x + 8'd2;
            }

            always_ff (clk) {
                $assert_continue(ok, "value=%0d", message_value(d));
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let ok = sim.signal("ok");
    let d = sim.signal("d");

    sim.modify(|io| {
        io.set(ok, 1u8);
        io.set(d, 10u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "inside=10".to_string(),
        }],
    );

    sim.modify(|io| {
        io.set(ok, 0u8);
        io.set(d, 20u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "inside=20".to_string(),
            },
            celox::RuntimeEvent::AssertContinue {
                message: "value=22".to_string(),
            },
        ],
    );
}

fn test_ff_unknown_ternary_retains_then_arm_output_state(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            choose: input logic,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x + 8'd2;
            }

            function observed (
                choose: input logic,
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                var selected: logic<8>;
                written = x;
                selected = if choose ? update(x, written) : x;
                $display("written=%0d", written);
                return written;
            }

            always_ff (clk) {
                q = observed(choose, d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top").four_state(true);
    let clk = sim.event("clk");
    let choose = sim.signal("choose");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set_four_state(choose, BigUint::from(0u8), BigUint::from(1u8));
        io.set(d, 10u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 11u8.into());
    assert_eq!(sim.get(q), 11u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "written=11".to_string(),
        }],
    );
}

fn test_ff_ternary_runtime_effect_only_evaluates_selected_arm(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, choose: input logic, q: output logic<8>) {
            function observed_value (x: input logic<8>) -> logic<8> {
                $display("arm=%0d", x);
                return x;
            }

            always_ff (clk) {
                q = if choose ? observed_value(8'd1) : observed_value(8'd2);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let choose = sim.signal("choose");

    sim.modify(|io| io.set(choose, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "arm=1".to_string(),
        }],
    );

    sim.modify(|io| io.set(choose, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "arm=2".to_string(),
        }],
    );
}

fn test_ff_effectful_function_input_is_evaluated_once(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            function inner (x: input logic<8>) -> logic<8> {
                $display("inner=%0d", x);
                return x;
            }

            function outer (x: input logic<8>) -> logic<8> {
                $display("outer=%0d", x);
                return x;
            }

            always_ff (clk) {
                q = outer(inner(d));
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 7u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 7u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "inner=7".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "outer=7".to_string(),
            },
        ],
    );
}

fn test_ff_assert_message_args_preserve_left_to_right_snapshots(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, effect: output logic<8>) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x + 8'd1;
            }

            always_ff (clk) {
                $assert_continue(1'b0, "%0d %0d", effect, update(effect, effect));
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let effect = sim.signal("effect");

    sim.modify(|io| io.set(effect, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 6u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::AssertContinue {
            message: "5 6".to_string(),
        }],
    );
}

fn test_ff_runtime_effect_function_snapshots_input_that_aliases_output(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, effect: output logic<8>, q: output logic<8>) {
            function observed (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                $display("x=%0d", x);
                written = x + 8'd1;
                return x;
            }

            always_ff (clk) {
                q = observed(effect, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(effect, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 6u8.into());
    assert_eq!(sim.get(q), 5u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "x=5".to_string(),
        }],
    );
}

fn test_ff_case_pattern_runtime_effect_is_eager(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, ok: input logic, d: input logic<8>) {
            function observed_value (x: input logic<8>) -> logic<8> {
                $display("pattern=%0d", x);
                return x;
            }

            function message_value (x: input logic<8>) -> logic<8> {
                case x {
                    observed_value(8'd1): return 8'd11;
                    default: return 8'd22;
                }
            }

            always_ff (clk) {
                $assert_continue(ok, "value=%0d", message_value(d));
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let ok = sim.signal("ok");
    let d = sim.signal("d");

    sim.modify(|io| {
        io.set(ok, 1u8);
        io.set(d, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "pattern=1".to_string(),
        }],
    );
}

fn test_ff_statement_function_materializes_effectful_case_controls(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, d: input logic<8>) {
            function observed (
                tag: input logic<8>,
                x: input logic<8>
            ) -> logic<8> {
                $display("case %0d=%0d", tag, x);
                return x;
            }

            function consume (x: input logic<8>) {
                case observed(8'd1, x) {
                    observed(8'd2, 8'd1): {}
                    observed(8'd3, 8'd1): {}
                    default: {}
                }
            }

            always_ff (clk) {
                consume(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");

    sim.modify(|io| io.set(d, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "case 1=1".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "case 2=1".to_string(),
            },
        ],
    );
}

fn test_ff_case_controls_apply_nested_output_writes_to_function_state(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x;
            }

            function observed (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = 8'd0;
                case update(x, written) {
                    8'd10: {}
                    default: {}
                }
                return x;
            }

            function outer (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                $display("state=%0d %0d", observed(x, written), written);
                return written;
            }

            always_ff (clk) {
                q = outer(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 10u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "state=10 11".to_string(),
        }],
    );
    assert_eq!(sim.get(effect), 11u8.into());
    assert_eq!(sim.get(q), 11u8.into());
}

fn test_ff_case_skips_effectful_patterns_after_matching_arm(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, d: input logic<8>) {
            function observed (
                tag: input logic<8>,
                x: input logic<8>
            ) -> logic<8> {
                $display("pattern %0d=%0d", tag, x);
                return x;
            }

            function consume (x: input logic<8>) {
                case x {
                    observed(8'd1, 8'd1), observed(8'd2, 8'd1): {}
                    observed(8'd3, 8'd1): {}
                    default: {}
                }
            }

            always_ff (clk) {
                consume(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");

    sim.modify(|io| io.set(d, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "pattern 1=1".to_string(),
        }],
    );
}

fn test_ff_assignment_snapshots_dynamic_rhs_access(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic) {
            function observed (value: input logic<8>) -> logic {
                var index: logic<3>;
                var captured: logic;
                index = 3'd0;
                captured = value[index];
                index = 3'd1;
                $display("captured=%0d", captured);
                return captured;
            }

            always_ff (clk) {
                q = observed(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0b0000_0001u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "captured=1".to_string(),
        }],
    );
}

fn test_ff_assignment_substitutes_through_evaluating_system_function(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            function observed (value: input logic<8>) -> logic<8> {
                var changing: logic<8>;
                var captured: logic<8>;
                changing = value;
                captured = $unsigned(changing);
                changing = 8'd9;
                $display("captured=%0d", captured);
                return captured;
            }

            always_ff (clk) {
                q = observed(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 5u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "captured=5".to_string(),
        }],
    );
}

fn test_ff_if_snapshots_dynamic_predicate_before_state_merge(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, d: input logic<2>, q: output logic) {
            function observed (value: input logic<2>) -> logic {
                var index: logic;
                var result: logic;
                index = 1'b0;
                result = 1'b0;
                if value[index] {
                    result = 1'b1;
                }
                index = 1'b1;
                $display("result=%0d", result);
                return result;
            }

            always_ff (clk) {
                q = observed(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0b01u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "result=1".to_string(),
        }],
    );
}

fn test_ff_case_snapshots_dynamic_target_before_state_merge(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, d: input logic<2>, q: output logic) {
            function observed (value: input logic<2>) -> logic {
                var index: logic;
                var result: logic;
                index = 1'b0;
                result = 1'b0;
                case value[index] {
                    1'b1: result = 1'b1;
                    default: {}
                }
                index = 1'b1;
                $display("result=%0d", result);
                return result;
            }

            always_ff (clk) {
                q = observed(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0b01u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "result=1".to_string(),
        }],
    );
}

fn test_ff_case_merges_nested_output_state_from_selected_arm(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            choose: input logic,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x + 8'd2;
            }

            function observed (
                choose: input logic,
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                var temporary: logic<8>;
                written = x;
                case choose {
                    1'b1: temporary = update(x, written);
                    default: temporary = x;
                }
                $display("written=%0d", written);
                return written;
            }

            always_ff (clk) {
                q = observed(choose, d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let choose = sim.signal("choose");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(choose, 1u8);
        io.set(d, 10u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 11u8.into());
    assert_eq!(sim.get(q), 11u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "written=11".to_string(),
        }],
    );

    sim.modify(|io| {
        io.set(choose, 0u8);
        io.set(d, 20u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 20u8.into());
    assert_eq!(sim.get(q), 20u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "written=20".to_string(),
        }],
    );
}

fn test_ff_variable_select_captures_nested_output(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            index: input logic<3>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                selected: input logic<3>,
                written: output logic<8>
            ) -> logic<3> {
                written = selected + 8'd1;
                return selected;
            }

            function observed (
                value: input logic<8>,
                selected: input logic<3>,
                written: output logic<8>
            ) -> logic<8> {
                $display("bit=%0d", value[update(selected, written)]);
                return written;
            }

            always_ff (clk) {
                q = observed(d, index, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let index = sim.signal("index");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(d, 0b0000_0100u8);
        io.set(index, 2u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 3u8.into());
    assert_eq!(sim.get(q), 3u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "bit=1".to_string(),
        }],
    );
}

fn test_ff_effectful_assignment_executes_only_on_selected_if_path(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            enable: input logic,
            d: input logic<8>,
            q: output logic<8>
        ) {
            function observed (x: input logic<8>) -> logic<8> {
                $display("assigned=%0d", x);
                return x + 8'd2;
            }

            function outer (
                enable: input logic,
                x: input logic<8>
            ) -> logic<8> {
                var selected: logic<8>;
                selected = x + 8'd1;
                if enable {
                    selected = observed(x);
                }
                return selected;
            }

            always_ff (clk) {
                q = outer(enable, d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let enable = sim.signal("enable");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(enable, 0u8);
        io.set(d, 5u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 6u8.into());
    assert!(sim.drain_runtime_events().is_empty());

    sim.modify(|io| {
        io.set(enable, 1u8);
        io.set(d, 6u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 8u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "assigned=6".to_string(),
        }],
    );
}

fn test_ff_runtime_effect_after_conditional_return_uses_live_path(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            skip: input logic,
            d: input logic<8>,
            q: output logic<8>
        ) {
            function observed (
                skip: input logic,
                x: input logic<8>
            ) -> logic<8> {
                if skip {
                    return x + 8'd1;
                }
                $display("live=%0d", x);
                return x + 8'd2;
            }

            always_ff (clk) {
                q = observed(skip, d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let skip = sim.signal("skip");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(skip, 1u8);
        io.set(d, 7u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 8u8.into());
    assert!(sim.drain_runtime_events().is_empty());

    sim.modify(|io| {
        io.set(skip, 0u8);
        io.set(d, 8u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 10u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "live=8".to_string(),
        }],
    );
}

fn test_ff_case_after_conditional_return_preserves_returned_path(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            skip: input logic,
            choose: input logic,
            d: input logic<8>,
            q: output logic<8>
        ) {
            function observed (
                skip: input logic,
                choose: input logic,
                x: input logic<8>
            ) -> logic<8> {
                if skip {
                    return x + 8'd1;
                }
                $display("case=%0d", choose);
                case choose {
                    1'b0: return x + 8'd2;
                    default: return x + 8'd3;
                }
            }

            always_ff (clk) {
                q = observed(skip, choose, d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let skip = sim.signal("skip");
    let choose = sim.signal("choose");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(skip, 1u8);
        io.set(choose, 0u8);
        io.set(d, 10u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 11u8.into());
    assert!(sim.drain_runtime_events().is_empty());

    sim.modify(|io| {
        io.set(skip, 1u8);
        io.set(choose, 1u8);
        io.set(d, 20u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 21u8.into());
    assert!(sim.drain_runtime_events().is_empty());

    sim.modify(|io| {
        io.set(skip, 0u8);
        io.set(choose, 0u8);
        io.set(d, 30u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 32u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "case=0".to_string(),
        }],
    );

    sim.modify(|io| {
        io.set(skip, 0u8);
        io.set(choose, 1u8);
        io.set(d, 40u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 43u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "case=1".to_string(),
        }],
    );
}

fn test_ff_statement_call_evaluates_effectful_inputs(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, d: input logic<8>) {
            function observed (x: input logic<8>) -> logic<8> {
                $display("input=%0d", x);
                return x;
            }

            function consume (x: input logic<8>) {
            }

            function outer (x: input logic<8>) {
                consume(observed(x));
            }

            always_ff (clk) {
                outer(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");

    sim.modify(|io| io.set(d, 9u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "input=9".to_string(),
        }],
    );
}

fn test_ff_top_level_statement_call_evaluates_discarded_effectful_input(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, d: input logic<8>) {
            function observed (x: input logic<8>) -> logic<8> {
                $display("direct=%0d", x);
                return x;
            }

            function consume (x: input logic<8>) {}

            always_ff (clk) {
                consume(observed(d));
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");

    sim.modify(|io| io.set(d, 13u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "direct=13".to_string(),
        }],
    );
}

fn test_ff_nested_runtime_event_output_updates_outer_function_state(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x + 8'd2;
            }

            function observed (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                $display("value=%0d", update(x, written));
                return x;
            }

            always_ff (clk) {
                q = observed(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 7u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 8u8.into());
    assert_eq!(sim.get(q), 7u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "value=9".to_string(),
        }],
    );
}

fn test_ff_nested_output_to_module_variable_survives_runtime_function(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            global_value: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                value: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = value + 8'd1;
                return 0;
            }

            function outer (value: input logic<8>) -> logic<8> {
                var temp: logic<8>;
                temp = update(value, global_value);
                $display("outer");
                return temp;
            }

            always_ff (clk) {
                q = outer(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let global_value = sim.signal("global_value");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 7u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 8u8.into());
    assert_eq!(sim.get(q), 0u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "outer".to_string(),
        }],
    );
}

fn test_ff_runtime_function_snapshots_nonlocal_read_before_later_write(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            global_value: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                value: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = value;
                return 0;
            }

            function outer (value: input logic<8>) -> logic<8> {
                var captured: logic<8>;
                var temp: logic<8>;
                captured = global_value;
                temp = update(value, global_value);
                $display("captured=%0d", captured);
                return captured;
            }

            always_ff (clk) {
                q = outer(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let global_value = sim.signal("global_value");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 7u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 7u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "captured=0".to_string(),
        }],
    );
    assert_eq!(sim.get(q), 0u8.into());

    sim.modify(|io| io.set(d, 8u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 8u8.into());
    assert_eq!(sim.get(q), 7u8.into());
}

fn test_ff_runtime_function_snapshots_input_before_callee_nonlocal_write(sim) {
    @omit_veryl;
    @ignore_on(wasm);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");
    let q = sim.signal("q");

    sim.modify(|io| io.set(global_value, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 9u8.into());
    assert_eq!(sim.get(q), 7u8.into());
}

fn test_ff_runtime_function_snapshots_helper_input_before_callee_nonlocal_write(sim) {
    @omit_veryl;
    @ignore_on(wasm);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");
    let q = sim.signal("q");

    sim.modify(|io| io.set(global_value, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 9u8.into());
    assert_eq!(sim.get(q), 7u8.into());
}

fn test_ff_outputless_nested_nonlocal_write_updates_later_event_argument(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            global_value: output logic<8>,
            q: output logic
        ) {
            function set_global () -> logic<8> {
                global_value = 8'd9;
                return 8'd1;
            }

            function observed () -> logic {
                global_value = 8'd0;
                $display("values=%0d,%0d", set_global(), global_value);
                return 1'b0;
            }

            always_ff (clk) {
                q = observed();
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");
    let q = sim.signal("q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 9u8.into());
    assert_eq!(sim.get(q), 0u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "values=1,9".to_string(),
        }],
    );
}

fn test_ff_statement_function_direct_nonlocal_assignment_is_observable(sim) {
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let global_value = sim.signal("global_value");

    sim.modify(|io| io.set(d, 0x5au8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 0x5au8.into());
}

fn test_ff_skipped_conditional_nonlocal_write_preserves_prior_ff_assignment(sim) {
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
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
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 2u8.into());
}

fn test_ff_outputless_wrapper_nested_copyout_to_nonlocal_is_observable(sim) {
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 0x5au8.into());
}

fn test_ff_outputless_wrapper_expression_copyout_to_nonlocal_is_observable(sim) {
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 0x5au8.into());
}

fn test_ff_outputless_wrapper_indexed_copyout_to_nonlocal_is_observable(sim) {
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 1u8.into());
}

fn test_ff_outputless_wrapper_dynamic_indexed_copyout_to_nonlocal_is_observable(sim) {
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let global_value = sim.signal("global_value");

    sim.modify(|io| io.set(index, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 2u8.into());
}

fn test_ff_outputless_wrapper_direct_dynamic_nonlocal_assignment_is_observable(sim) {
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
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

fn test_ff_pure_helper_nonlocal_read_is_snapshotted_before_runtime_write(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, global_value: output logic<8>) {
            function read_global () -> logic<8> {
                return global_value;
            }

            function update (
                value: input logic<8>,
                written: output logic<8>
            ) -> logic {
                written = value;
                return 1'b0;
            }

            function outer () {
                var captured: logic<8>;
                var ignored: logic;
                captured = read_global();
                ignored = update(8'h5a, global_value);
                $display("captured=%0d", captured);
            }

            always_ff (clk) {
                outer();
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 0x5au8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "captured=0".to_string(),
        }],
    );
}

fn test_ff_dynamic_nonlocal_store_follows_pending_whole_write(sim) {
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let global_value = sim.signal("global_value");

    sim.modify(|io| io.set(index, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 0xfdu8.into());
}

fn test_ff_guarded_system_task_merges_definition_state(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            gate: input logic,
            index: input logic<3>,
            global_value: output logic<8>
        ) {
            function update (written: output logic) -> logic {
                written = 1'b1;
                return 1'b0;
            }

            function outer (gate: input logic, index: input logic<3>) {
                if gate {
                    $display("update=%0d", update(global_value[index]));
                }
                $display("global=%0d", global_value);
            }

            always_ff (clk) {
                outer(gate, index);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let gate = sim.signal("gate");
    let index = sim.signal("index");
    let global_value = sim.signal("global_value");

    sim.modify(|io| {
        io.set(gate, 1u8);
        io.set(index, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 2u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "update=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "global=0".to_string(),
            },
        ],
    );

    sim.modify(|io| io.set(gate, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 2u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "global=2".to_string(),
        }],
    );
}

fn test_ff_nonlocal_source_ternary_preserves_unknown_merge(sim) {
    @omit_veryl;
    @ignore_on(wasm);
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
    @build Simulator::builder(code, "Top").four_state(true);
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

fn test_ff_statement_helper_dynamic_nonlocal_copyout_is_observable(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            index: input logic<3>,
            global_value: output logic<8>
        ) {
            function set (written: output logic) {
                written = 1'b1;
            }

            function outer (index: input logic<3>) {
                $display("set");
                set(global_value[index]);
            }

            always_ff (clk) {
                outer(index);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let global_value = sim.signal("global_value");

    sim.modify(|io| io.set(index, 2u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 4u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "set".to_string(),
        }],
    );
}

fn test_ff_nested_dynamic_nonlocal_store_flushes_pending_outer_state(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            index: input logic<3>,
            global_value: output logic<8>
        ) {
            function set_bit (index: input logic<3>) -> logic {
                global_value[index] = 1'b1;
                return 1'b0;
            }

            function outer (index: input logic<3>) {
                global_value = 8'h00;
                $display("result=%0d global=%0d", set_bit(index), global_value);
            }

            always_ff (clk) {
                outer(index);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let global_value = sim.signal("global_value");

    sim.modify(|io| io.set(index, 2u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 4u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "result=0 global=4".to_string(),
        }],
    );
}

fn test_ff_retained_dynamic_copyout_does_not_repeat_nonlocal_body_write(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            index: input logic<3>,
            global_value: output logic<8>
        ) {
            function update (written: output logic) -> logic<8> {
                global_value = global_value + 1;
                written = 1'b1;
                return global_value;
            }

            function outer (index: input logic<3>) {
                $display("result=%0d global=%0d", update(global_value[index]), global_value);
            }

            always_ff (clk) {
                outer(index);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let global_value = sim.signal("global_value");

    sim.modify(|io| io.set(index, 2u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 5u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "result=1 global=1".to_string(),
        }],
    );
}

fn test_ff_function_output_index_uses_final_nonlocal_state(sim) {
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let entries = sim.signal("entries");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(index), 1u8.into());
    assert_eq!(sim.get(entries), 0xa500u16.into());
}

fn test_ff_guarded_runtime_expression_merges_definition_state(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            gate: input logic,
            index: input logic<3>,
            global_value: output logic<8>
        ) {
            function update (written: output logic) -> logic {
                written = 1'b1;
                return 1'b0;
            }

            function outer (gate: input logic, index: input logic<3>) {
                var ignored: logic;
                if gate {
                    ignored = update(global_value[index]);
                }
                $display("global=%0d", global_value);
            }

            always_ff (clk) {
                outer(gate, index);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let gate = sim.signal("gate");
    let index = sim.signal("index");
    let global_value = sim.signal("global_value");

    sim.modify(|io| {
        io.set(gate, 1u8);
        io.set(index, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 2u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "global=0".to_string(),
        }],
    );

    sim.modify(|io| io.set(gate, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 2u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "global=2".to_string(),
        }],
    );
}

fn test_ff_short_circuit_nested_output_updates_only_when_rhs_runs(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            gate: input logic,
            d: input logic<8>,
            and_effect: output logic<8>,
            or_effect: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic {
                written = x + 8'd1;
                return 1'b1;
            }

            function observe_and (
                gate: input logic,
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x;
                $display("and=%0d", gate && update(x, written));
                return written;
            }

            function observe_or (
                gate: input logic,
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x;
                $display("or=%0d", gate || update(x, written));
                return written;
            }

            always_ff (clk) {
                observe_and(gate, d, and_effect);
                observe_or(gate, d, or_effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top").four_state(true);
    let clk = sim.event("clk");
    let gate = sim.signal("gate");
    let d = sim.signal("d");
    let and_effect = sim.signal("and_effect");
    let or_effect = sim.signal("or_effect");

    sim.modify(|io| {
        io.set(gate, 0u8);
        io.set(d, 10u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(and_effect), 10u8.into());
    assert_eq!(sim.get(or_effect), 11u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "and=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "or=1".to_string(),
            },
        ],
    );

    sim.modify(|io| {
        io.set(gate, 1u8);
        io.set(d, 20u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(and_effect), 21u8.into());
    assert_eq!(sim.get(or_effect), 20u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "and=1".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "or=1".to_string(),
            },
        ],
    );

    sim.modify(|io| {
        io.set_four_state(gate, BigUint::from(0u8), BigUint::from(1u8));
        io.set(d, 30u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(and_effect), 31u8.into());
    assert_eq!(sim.get(or_effect), 31u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "and=x".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "or=1".to_string(),
            },
        ],
    );
}

fn test_ff_short_circuit_runtime_write_preserves_later_state_source(sim) {
    @omit_veryl;
    @ignore_on(wasm);
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
    @build Simulator::builder(code, "Top");
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

fn test_ff_pure_predicate_output_updates_outer_function_state(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic {
                written = x + 8'd1;
                return 1'b1;
            }

            function outer (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x;
                if update(x, written) {}
                $display("predicate=%0d", written);
                return written;
            }

            always_ff (clk) {
                q = outer(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 12u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 13u8.into());
    assert_eq!(sim.get(q), 13u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "predicate=13".to_string(),
        }],
    );
}

fn test_ff_nested_wrapper_predicate_output_updates_caller_state(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic {
                written = x + 8'd1;
                return 1'b1;
            }

            function wrapper (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = 8'd0;
                if update(x, written) {}
                return written;
            }

            function observed (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                $display("wrapper=%0d", wrapper(x, written));
                return written;
            }

            always_ff (clk) {
                q = observed(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 12u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 13u8.into());
    assert_eq!(sim.get(q), 13u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "wrapper=13".to_string(),
        }],
    );
}

fn test_ff_nested_call_predicate_uses_pre_copyout_input(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x;
            }

            function wrapper (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x;
                if update(written, written) == x {
                    written = 8'd42;
                }
                return written;
            }

            function observed (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                $display("wrapper=%0d", wrapper(x, written));
                return written;
            }

            always_ff (clk) {
                q = observed(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 42u8.into());
    assert_eq!(sim.get(q), 42u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "wrapper=42".to_string(),
        }],
    );
}

fn test_ff_bits_and_size_do_not_evaluate_output_writing_operand(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x;
            }

            function outer (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x;
                $display(
                    "bits=%0d size=%0d",
                    $bits(update(x, written)),
                    $size(update(x, written))
                );
                return written;
            }

            always_ff (clk) {
                q = outer(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 14u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 14u8.into());
    assert_eq!(sim.get(q), 14u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "bits=8 size=8".to_string(),
        }],
    );
}

fn test_ff_bits_and_size_operands_do_not_alias_earlier_array_argument(sim) {
    @omit_veryl;
    @ignore_on(wasm);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let q = sim.signal("q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x12u8.into());
}

fn test_ff_bits_and_size_array_dependencies_do_not_alias_later_write(sim) {
    @omit_veryl;
    @ignore_on(wasm);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let q = sim.signal("q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 16u32.into());
}

fn test_ff_statement_call_materializes_output_only_input_effect(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x;
            }

            function consume (x: input logic<8>) {}

            function outer (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x;
                consume(update(x, written));
                $display("statement=%0d", written);
                return written;
            }

            always_ff (clk) {
                q = outer(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 16u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 17u8.into());
    assert_eq!(sim.get(q), 17u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "statement=17".to_string(),
        }],
    );
}

fn test_ff_nested_statement_call_copies_outputs_in_declaration_order(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function split (
                first: output logic<8>,
                second: output logic<8>
            ) {
                first = 8'd1;
                second = 8'd2;
            }

            function outer (written: output logic<8>) -> logic<8> {
                split(written, written);
                $display("written=%0d", written);
                return written;
            }

            always_ff (clk) {
                q = outer(effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 2u8.into());
    assert_eq!(sim.get(q), 2u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "written=2".to_string(),
        }],
    );
}

fn test_ff_nested_statement_call_coerces_output_to_actual_width(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function write_wide (written: output logic<16>) {
                written = 16'h0101;
            }

            function outer (written: output logic<8>) -> logic<8> {
                write_wide(written);
                $display("written=%0d", written);
                return written;
            }

            always_ff (clk) {
                q = outer(effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 1u8.into());
    assert_eq!(sim.get(q), 1u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "written=1".to_string(),
        }],
    );
}

fn test_ff_state_only_statement_call_materializes_nested_input_output(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x;
            }

            function consume (x: input logic<8>) {}

            function wrapper (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x;
                consume(update(x, written));
                return written;
            }

            function outer (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                $display("wrapped=%0d", wrapper(x, written));
                return written;
            }

            always_ff (clk) {
                q = outer(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 16u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 17u8.into());
    assert_eq!(sim.get(q), 17u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "wrapped=17".to_string(),
        }],
    );
}

fn test_ff_statement_call_copies_outputs_in_declaration_order(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            first_effect: output logic,
            second_effect: output logic,
            a: output logic<2>,
            b: output logic<2>
        ) {
            function observed_index (
                tag: input logic,
                written: output logic
            ) -> logic {
                $display("index=%0d", tag);
                written = tag;
                return tag;
            }

            function split (
                first: output logic,
                second: output logic
            ) {
                first = 1'b1;
                second = 1'b1;
            }

            always_ff (clk) {
                split(
                    a[observed_index(1'b0, first_effect)],
                    b[observed_index(1'b1, second_effect)]
                );
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");

    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "index=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "index=1".to_string(),
            },
        ],
    );
}

fn test_ff_composite_runtime_arg_preserves_left_to_right_snapshot(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x + 8'd2;
            }

            function outer (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x;
                $display("sum=%0d", written + update(x, written));
                return written;
            }

            always_ff (clk) {
                q = outer(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 10u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 11u8.into());
    assert_eq!(sim.get(q), 11u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "sum=22".to_string(),
        }],
    );
}

fn test_ff_nested_call_inputs_capture_outputs_in_declaration_order(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function first (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x + 8'd1;
            }

            function second (
                seen: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = seen + 8'd1;
                return seen;
            }

            function combine (
                a: input logic<8>,
                b: input logic<8>
            ) -> logic<8> {
                return a + b;
            }

            function outer (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x;
                $display(
                    "ordered=%0d",
                    combine(first(x, written), second(written, written))
                );
                return written;
            }

            always_ff (clk) {
                q = outer(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 10u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 12u8.into());
    assert_eq!(sim.get(q), 12u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "ordered=22".to_string(),
        }],
    );
}

fn test_ff_nested_call_freezes_all_outputs_before_copy_out(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            first: output logic<8>,
            second: output logic<8>
        ) {
            function swap_copy (
                old_first: input logic<8>,
                old_second: input logic<8>,
                new_first: output logic<8>,
                new_second: output logic<8>
            ) -> logic<8> {
                new_first = old_second;
                new_second = old_first;
                return new_first + new_second;
            }

            always_ff (clk) {
                $display(
                    "sum=%0d",
                    swap_copy(first, second, first, second)
                );
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let first = sim.signal("first");
    let second = sim.signal("second");

    sim.modify(|io| {
        io.set(first, 7u8);
        io.set(second, 9u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(first), 9u8.into());
    assert_eq!(sim.get(second), 7u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "sum=16".to_string(),
        }],
    );
}

fn test_ff_nested_call_output_preserves_conditional_early_return_path(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            flag: input logic,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function inner (
                flag: input logic,
                written: output logic<8>
            ) -> logic<8> {
                written = 8'd1;
                if flag {
                    return 8'd11;
                }
                written = 8'd2;
                return 8'd22;
            }

            function outer (
                flag: input logic,
                written: output logic<8>
            ) -> logic<8> {
                $display("inner=%0d", inner(flag, written));
                $display("written=%0d", written);
                return written;
            }

            always_ff (clk) {
                q = outer(flag, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let flag = sim.signal("flag");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(flag, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 1u8.into());
    assert_eq!(sim.get(q), 1u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "inner=11".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "written=1".to_string(),
            },
        ],
    );

    sim.modify(|io| io.set(flag, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 2u8.into());
    assert_eq!(sim.get(q), 2u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "inner=22".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "written=2".to_string(),
            },
        ],
    );
}

fn test_ff_short_circuit_state_reuses_evaluated_lhs(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            gate: input logic,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function lhs (
                gate: input logic,
                x: input logic<8>,
                written: output logic<8>
            ) -> logic {
                $display("lhs=%0d", x);
                written = x + 8'd1;
                return gate;
            }

            function rhs (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic {
                written = x + 8'd2;
                return 1'b1;
            }

            function outer (
                gate: input logic,
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x;
                $display("logic=%0d", lhs(gate, x, written) && rhs(x, written));
                $display("result=%0d", written);
                return written;
            }

            always_ff (clk) {
                q = outer(gate, d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let gate = sim.signal("gate");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(gate, 0u8);
        io.set(d, 20u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 21u8.into());
    assert_eq!(sim.get(q), 21u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "lhs=20".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "logic=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "result=21".to_string(),
            },
        ],
    );
}

fn test_ff_concatenation_effects_follow_source_order(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function first (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                $display("first=%0d", x);
                written = x + 8'd1;
                return x + 8'd1;
            }

            function second (
                seen: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                $display("second=%0d", seen);
                written = seen + 8'd1;
                return seen;
            }

            function outer (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x;
                $display("concat=%0d", {first(x, written), second(written, written)});
                return written;
            }

            always_ff (clk) {
                q = outer(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 10u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 12u8.into());
    assert_eq!(sim.get(q), 12u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "first=10".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "second=11".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "concat=2827".to_string(),
            },
        ],
    );
}

fn test_ff_materialized_formal_slice_uses_expression_context(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<16>) {
            function inner (x: input logic<8>) -> logic<8> {
                $display("inner=%0d", x);
                return x;
            }

            function outer (x: input logic<8>) -> logic<16> {
                return x[3:0] + 16'd1;
            }

            always_ff (clk) {
                q = outer(inner(d));
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0xafu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x0010u16.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "inner=175".to_string(),
        }],
    );
}

fn test_ff_runtime_event_reads_updated_formal_slice(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function observed (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                $display("slice=%0d", written[3:0]);
                return x;
            }

            always_ff (clk) {
                q = observed(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0x1eu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 0x1fu8.into());
    assert_eq!(sim.get(q), 0x1eu8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "slice=15".to_string(),
        }],
    );
}

fn test_ff_case_assignment_is_visible_to_later_runtime_event(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            choose: input logic,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function observed (
                choose: input logic,
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                case choose {
                    1'b1: written = x + 8'd1;
                    default: written = x + 8'd2;
                }
                $display("case=%0d", written);
                return x;
            }

            always_ff (clk) {
                q = observed(choose, d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let choose = sim.signal("choose");
    let d = sim.signal("d");
    let effect = sim.signal("effect");

    sim.modify(|io| {
        io.set(choose, 1u8);
        io.set(d, 10u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 11u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "case=11".to_string(),
        }],
    );

    sim.modify(|io| {
        io.set(choose, 0u8);
        io.set(d, 20u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 22u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "case=22".to_string(),
        }],
    );
}

fn test_ff_statement_function_with_output_emits_runtime_effect(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, d: input logic<8>, effect: output logic<8>) {
            function observed (
                x: input logic<8>,
                written: output logic<8>
            ) {
                $display("statement=%0d", x);
                written = x + 8'd1;
            }

            always_ff (clk) {
                observed(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");

    sim.modify(|io| io.set(d, 12u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 13u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "statement=12".to_string(),
        }],
    );
}

fn test_ff_effectful_if_predicate_is_evaluated_once(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function observed_predicate (x: input logic<8>) -> logic {
                $display("predicate=%0d", x);
                return x[0];
            }

            function outer (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                if observed_predicate(x) {
                }
                written = x;
                return x;
            }

            always_ff (clk) {
                q = outer(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 7u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 7u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "predicate=7".to_string(),
        }],
    );
}

fn test_ff_nested_output_is_captured_through_signed_cast(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x + 8'd2;
            }

            function observed (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                $display("cast=%0d", $signed(update(x, written)));
                return x;
            }

            always_ff (clk) {
                q = observed(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 7u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 8u8.into());
    assert_eq!(sim.get(q), 7u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "cast=9".to_string(),
        }],
    );
}

fn test_ff_effectful_function_inputs_follow_declaration_order(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            function observed (
                tag: input logic<8>,
                x: input logic<8>
            ) -> logic<8> {
                $display("arg=%0d", tag);
                return x;
            }

            function outer (
                arg1: input logic<8>,
                arg2: input logic<8>,
                arg3: input logic<8>,
                arg4: input logic<8>,
                arg5: input logic<8>,
                arg6: input logic<8>,
                arg7: input logic<8>,
                arg8: input logic<8>
            ) -> logic<8> {
                return arg1 + arg2 + arg3 + arg4 + arg5 + arg6 + arg7 + arg8;
            }

            always_ff (clk) {
                q = outer(
                    observed(8'd1, d),
                    observed(8'd2, d),
                    observed(8'd3, d),
                    observed(8'd4, d),
                    observed(8'd5, d),
                    observed(8'd6, d),
                    observed(8'd7, d),
                    observed(8'd8, d)
                );
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 3u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 24u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "arg=1".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "arg=2".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "arg=3".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "arg=4".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "arg=5".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "arg=6".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "arg=7".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "arg=8".to_string(),
            },
        ],
    );
}

fn test_ff_pure_input_is_snapshotted_before_later_effectful_input(sim) {
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(effect, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 6u8.into());
    assert_eq!(sim.get(q), 5u8.into());
}

fn test_ff_runtime_event_arguments_use_per_argument_state(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            effect: output logic<8>,
            q: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x + 8'd2;
            }

            function observed (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x;
                $display("args=%0d %0d", written[3:0], update(x, written));
                return written;
            }

            always_ff (clk) {
                q = observed(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 10u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 11u8.into());
    assert_eq!(sim.get(q), 11u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "args=10 12".to_string(),
        }],
    );
}

fn test_ff_runtime_event_formal_uses_declared_type(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<16>,
            q: output signed logic<8>
        ) {
            function observed (
                x: input signed logic<8>
            ) -> signed logic<8> {
                $display("formal=%0d", x);
                return x;
            }

            always_ff (clk) {
                q = observed(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0x01ffu16)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xffu8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "formal=-1".to_string(),
        }],
    );
}

fn test_ff_unpacked_input_before_runtime_effect_stays_symbolically_bound(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            samples: input logic<8>[2],
            q: output logic<8>
        ) {
            function observed (x: input logic<8>) -> logic {
                $display("observed=%0d", x);
                return 1'b0;
            }

            function pick (
                values: input logic<8>[2],
                marker: input logic
            ) -> logic<8> {
                return if marker ? values[0] : values[1];
            }

            always_ff (clk) {
                q = pick(samples, observed(samples[0]));
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let samples = sim.signal("samples");
    let q = sim.signal("q");

    sim.modify(|io| io.set_wide(samples, BigUint::from(0xab11u32)))
        .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xabu8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "observed=17".to_string(),
        }],
    );
}

fn test_ff_effectful_array_item_output_is_not_a_read_alias(sim) {
    @omit_veryl;
    @ignore_on(wasm);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let effect = sim.signal("effect");
    let q = sim.signal("q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(effect), 0xa5u8.into());
    assert_eq!(sim.get(q), 0x11u8.into());
}

fn test_ff_symbolic_runtime_input_uses_declared_formal_type(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<16>,
            q: output logic<8>
        ) {
            function observed (x: input logic<8>) -> logic<8> {
                if x != 8'd0 {
                    $display("nonzero");
                }
                return x;
            }

            always_ff (clk) {
                q = observed(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0x0100u16)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
    assert!(sim.drain_runtime_events().is_empty());
}

fn test_ff_runtime_effectful_return_uses_declared_signed_type(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<16>,
            q: output signed logic<16>
        ) {
            function observed (x: input logic<16>) -> signed bit<8> {
                $display("return");
                return x;
            }

            always_ff (clk) {
                q = observed(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top").four_state(true);
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set_four_state(
            d,
            BigUint::from(0x12abu32),
            BigUint::from(0x000fu32),
        )
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get_four_state(q),
        (BigUint::from(0xffa0u32), BigUint::from(0u32))
    );
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "return".to_string(),
        }],
    );
}

fn test_ff_runtime_effectful_output_uses_declared_formal_type(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<16>,
            effect: output logic<16>
        ) {
            function observed (
                x: input logic<16>,
                written: output bit<8>
            ) {
                written = x;
                $display("output");
            }

            always_ff (clk) {
                observed(d, effect);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top").four_state(true);
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let effect = sim.signal("effect");

    sim.modify(|io| {
        io.set_four_state(
            d,
            BigUint::from(0x12abu32),
            BigUint::from(0x000fu32),
        )
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get_four_state(effect),
        (BigUint::from(0xa0u32), BigUint::from(0u32))
    );
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "output".to_string(),
        }],
    );
}

fn test_ff_runtime_effectful_merge_preserves_signed_return_type(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            choose: input logic,
            q: output signed logic<16>
        ) {
            function observed (choose: input logic) -> signed logic<16> {
                $display("choose=%0d", choose);
                if choose {
                    return 8'sh80;
                } else {
                    return 8'sh81;
                }
            }

            always_ff (clk) {
                q = observed(choose);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let choose = sim.signal("choose");
    let q = sim.signal("q");

    sim.modify(|io| io.set(choose, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xff80u16.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "choose=1".to_string(),
        }],
    );

    sim.modify(|io| io.set(choose, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xff81u16.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "choose=0".to_string(),
        }],
    );
}

fn test_ff_runtime_effectful_local_assignment_uses_declared_type(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic,
            q: output logic
        ) {
            function observed (x: input logic) -> logic {
                var temporary: signed logic<8>;
                temporary = 16'h00ff + x;
                $display("temporary=%0d", temporary);
                if temporary <: 8'sd0 {
                    return 1'b1;
                } else {
                    return 1'b0;
                }
            }

            always_ff (clk) {
                q = observed(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "temporary=-1".to_string(),
        }],
    );
}

fn test_ff_rewritten_runtime_event_argument_preserves_signedness(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input signed logic<8>,
            q: output signed logic<8>
        ) {
            function observed (x: input signed logic<8>) -> signed logic<8> {
                $display("signed=%0d", -x);
                return x;
            }

            always_ff (clk) {
                q = observed(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "signed=-1".to_string(),
        }],
    );
}

fn test_ff_runtime_events_format_verilog_radices(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, a: input logic<8>) {
            always_ff (clk) {
                $display("bin=%b hex=%h HEX=%H", a, a, a);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let a = sim.signal("a");

    sim.modify(|io| io.set(a, 0x2au8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "bin=00101010 hex=2a HEX=2A".to_string(),
        }],
    );
}

fn test_ff_runtime_events_preserve_four_state_args(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, a: input logic<4>) {
            always_ff (clk) {
                $display("a=%b hex=%x dec=%0d", a, a, a);
                $assert_continue(1'b0, "bad=%b", a);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top").four_state(true);
    let clk = sim.event("clk");
    let a = sim.signal("a");

    sim.modify(|io| {
        io.set_four_state(a, BigUint::from(0b1010u32), BigUint::from(0b0100u32))
    })
    .unwrap();
    sim.tick(clk).unwrap();
    let events = sim.drain_runtime_events();
    assert_eq!(
        events,
        vec![
            celox::RuntimeEvent::Display {
                message: "a=1x10 hex=x dec=x".to_string(),
            },
            celox::RuntimeEvent::AssertContinue {
                message: "bad=1x10".to_string(),
            },
        ],
    );
}

fn test_ff_runtime_events_support_design_sized_arg_count(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            a: input logic<8>,
            b: input logic<8>,
            c: input logic<8>,
            d: input logic<8>,
            e: input logic<8>
        ) {
            always_ff (clk) {
                $display("%0d %0d %0d %0d %0d", a, b, c, d, e);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let c = sim.signal("c");
    let d = sim.signal("d");
    let e = sim.signal("e");

    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(b, 2u8);
        io.set(c, 3u8);
        io.set(d, 4u8);
        io.set(e, 5u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "1 2 3 4 5".to_string(),
        }],
    );
}

fn test_ff_runtime_events_support_wide_four_state_args(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, a: input logic<80>) {
            always_ff (clk) {
                $display("a=%x dec=%0d", a, a);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top").four_state(true);
    let clk = sim.event("clk");
    let a = sim.signal("a");

    sim.modify(|io| {
        io.set_four_state(
            a,
            BigUint::parse_bytes(b"123456789abcdef01234", 16).unwrap(),
            BigUint::from(0x0fu32),
        )
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "a=123456789abcdef0123x dec=x".to_string(),
        }],
    );
}

fn test_ff_runtime_event_drain_handle_can_run_during_simulation(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, a: input logic<16>) {
            always_ff (clk) {
                $display("a=%0d", a);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let mut drain = sim.runtime_event_drain().expect("runtime event drain handle");
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let drained = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let drain_thread = {
        let done = std::sync::Arc::clone(&done);
        let drained = std::sync::Arc::clone(&drained);
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let events = drain.drain();
                if !events.is_empty() {
                    drained.lock().unwrap().extend(events);
                }
                std::thread::yield_now();
            }
            drained.lock().unwrap().extend(drain.drain());
        })
    };

    for value in 0..128u16 {
        sim.modify(|io| io.set(a, value)).unwrap();
        sim.tick(clk).unwrap();
    }
    done.store(true, std::sync::atomic::Ordering::Release);
    drain_thread.join().unwrap();

    let events = std::sync::Arc::try_unwrap(drained)
        .unwrap()
        .into_inner()
        .unwrap();
    let messages = events
        .into_iter()
        .map(|event| match event {
            celox::RuntimeEvent::Display { message } => message,
            other => panic!("unexpected runtime event: {other:?}"),
        })
        .collect::<Vec<_>>();
    let expected = (0..128u16)
        .map(|value| format!("a={value}"))
        .collect::<Vec<_>>();
    assert_eq!(messages, expected);
}

fn test_runtime_event_drain_handle_is_exclusive(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, a: input logic<8>) {
            always_ff (clk) {
                $display("a=%0d", a);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");

    let clk = sim.event("clk");
    let a = sim.signal("a");
    let mut drain = sim.runtime_event_drain().expect("runtime event drain handle");
    assert!(sim.runtime_event_drain().is_none());

    sim.modify(|io| io.set(a, 9u8)).unwrap();
    sim.tick(clk).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sim.drain_runtime_events();
    }));
    assert!(result.is_err());
    assert_eq!(
        drain.drain(),
        vec![celox::RuntimeEvent::Display {
            message: "a=9".to_string(),
        }],
    );

    drop(drain);
    sim.modify(|io| io.set(a, 10u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "a=10".to_string(),
        }],
    );
}

fn test_ff_runtime_fatal_assert_records_event(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock, a: input logic<8>) {
            always_ff (clk) {
                $assert(a != 8'd7, "fatal a=%0d", a);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let a = sim.signal("a");

    sim.modify(|io| io.set(a, 7u8)).unwrap();
    assert!(sim.tick(clk).is_err());
    let events = sim.drain_runtime_events();
    assert_eq!(
        events,
        vec![celox::RuntimeEvent::AssertFatal {
            message: "fatal a=7".to_string(),
        }],
    );
}

fn test_ff_message_less_runtime_fatal_assert_uses_default_message(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (clk: input clock) {
            always_ff (clk) {
                $assert(1'b0);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");

    let err = sim.tick(clk).unwrap_err();
    assert_eq!(err.to_string(), "assertion failed");
    let events = sim.drain_runtime_events();
    assert_eq!(
        events,
        vec![celox::RuntimeEvent::AssertFatal {
            message: "assertion failed".to_string(),
        }],
    );
}

fn test_ff_runtime_for_bounds(sim) {
    @setup { let code = r#"
        module Top (
            clk: input clock,
            count: input logic<8>,
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
                for i in 1..(count + 4) step *= 2 {
                    q_step = i as 8;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let count = sim.signal("count");
    let q_fwd = sim.signal("q_fwd");
    let q_rev = sim.signal("q_rev");
    let q_inc = sim.signal("q_inc");
    let q_step = sim.signal("q_step");

    sim.modify(|io| io.set(count, 4u8)).unwrap();
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            or_end: input signed logic<128>,
            xor_end: input signed logic<128>,
            q_or: output signed logic<32>,
            q_xor: output signed logic<32>
        ) {
            always_ff (clk) {
                q_or = 0;
                for i in 3..=or_end step |= 4294967302 {
                    q_or = i;
                    if i == 7 {
                        break;
                    }
                }

                q_xor = 0;
                for i in 3..=xor_end step ^= 4294967302 {
                    q_xor = i;
                    if i == 5 {
                        break;
                    }
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let or_end = sim.signal("or_end");
    let xor_end = sim.signal("xor_end");
    let q_or = sim.signal("q_or");
    let q_xor = sim.signal("q_xor");

    sim.modify(|io| {
        io.set_wide(or_end, BigUint::from(7u8));
        io.set_wide(xor_end, BigUint::from(5u8));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_or), 7u32.into());
    assert_eq!(sim.get(q_xor), 5u32.into());
}

fn test_ff_i32_xor_step_with_only_high_bits_reports_true_loop(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            end_bound: input logic<32>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in 3..end_bound step ^= 4294967296 {
                    q = i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let end_bound = sim.signal("end_bound");

    sim.modify(|io| io.set(end_bound, 4u32)).unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "Non-progressing for loop in always_ff (loop variable `i`): i"
    );
}

fn test_ff_i32_or_step_with_only_existing_low_bits_reports_true_loop(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            end_bound: input logic<32>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in 3..end_bound step |= 4294967299 {
                    q = i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let end_bound = sim.signal("end_bound");

    sim.modify(|io| io.set(end_bound, 4u32)).unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "Non-progressing for loop in always_ff (loop variable `i`): i"
    );
}

fn test_ff_i32_mul_step_overflow_reports_true_loop(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            end_bound: input signed logic<64>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in 1500000000..end_bound step *= 2 {
                    q += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let end_bound = sim.signal("end_bound");

    // The first update overflows i32 even though its widened value would
    // already exceed this still-representable bound.
    sim.set(end_bound, 1_600_000_000u64);
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "Non-progressing for loop in always_ff (loop variable `i`): i"
    );
}

fn test_ff_i32_shl_step_overflow_reports_true_loop(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            end_bound: input signed logic<64>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in 1073741824..end_bound step <<= 1 {
                    q += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let end_bound = sim.signal("end_bound");

    // The first update overflows i32 even though its widened value would
    // already exceed this still-representable bound.
    sim.set(end_bound, 1_500_000_000u64);
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "Non-progressing for loop in always_ff (loop variable `i`): i"
    );
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let count = sim.signal("count");
    let q = sim.signal("q");

    sim.modify(|io| io.set(count, 8u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 2u8.into());
}

#[ignore]
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let q_fwd = sim.signal("q_fwd");
    let q_rev_last = sim.signal("q_rev_last");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_fwd), 0u32.into());
    assert_eq!(sim.get(q_rev_last), 0u32.into());
}

fn test_ff_runtime_for_dynamic_zero_start_mul_reports_true_loop(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input logic<8>,
            count: input logic<8>,
            q: output logic<8>
        ) {
            always_ff (clk) {
                q = 0;
                for i in start..count step *= 2 {
                    q = i as 8;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let count = sim.signal("count");

    sim.modify(|io| {
        io.set(start, 0u8);
        io.set(count, 4u8);
    })
    .unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "Non-progressing for loop in always_ff (loop variable `i`): i"
    );
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let q = sim.signal("q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xaau32.into());
}

fn test_ff_runtime_for_terminal_inclusive_mul_loop_reports_true_loop(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            count: input logic<8>,
            q: output logic<8>
        ) {
            always_ff (clk) {
                q = 8'haa;
                for i in 0..=count step *= 2 {
                    q = (i + 1) as 8;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let count = sim.signal("count");

    sim.modify(|io| io.set(count, 0u8)).unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "Non-progressing for loop in always_ff (loop variable `i`): i"
    );
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
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

fn test_ff_runtime_reverse_i32_step_truncation_reports_true_loop(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input signed logic<64>,
            end_bound: input signed logic<64>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in rev start..=end_bound step += 4294967296 {
                    q += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");

    sim.modify(|io| {
        io.set(start, 0u64);
        io.set(end_bound, 3u64);
    })
    .unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "Non-progressing for loop in always_ff (loop variable `i`): i"
    );
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input logic<32>,
            count: input logic<32>,
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let q_last = sim.signal("q_last");

    sim.modify(|io| io.set(start, 0xffffu16)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_last), 255u32.into());
}

fn test_ff_if_reset_basic(sim) {
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0xFEu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xFEu32.into());
}

fn test_multiple_async_resets(sim) {
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let in_neg = sim.signal("in_neg");
    let out_pad = sim.signal("out_pad");

    sim.modify(|io| io.set(in_neg, 0xFFu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_pad), 0xFFFFu32.into());
}

fn test_ff_array_literal_expression_order(sim) {
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o0), 0x12u32.into());
    assert_eq!(sim.get(o1), 0x34u32.into());
}

fn test_ff_array_literal_default_expression(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (clk: input clock, in_data: input logic<8>, out_data: output logic<8>[4]) {
            var r: logic<8>[4];
            always_ff (clk) {
                r = '{default: in_data};
            }
            assign out_data = r;
        }
    "#; }
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 10u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 11u32.into());
}

fn test_ff_function_call_statement_with_output_argument(sim) {
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 3u8)).unwrap();
    sim.tick(clk).unwrap();
    // (3+1)*2 = 8
    assert_eq!(sim.get(out_q), 8u32.into());
}

fn test_ff_function_call_indexed_argument_access(sim) {
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 0b1101_0110u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0b0101u32.into());
}

fn test_ff_function_call_step_access_on_nonvariable_argument(sim) {
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 0b1010_0100u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0b1010u32.into());
}

fn test_ff_function_call_nonvariable_argument_uses_formal_width_before_slice(sim) {
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 1u32.into());
}

fn test_ff_function_call_nonvariable_argument_preserves_self_sized_overflow_before_coercion(sim) {
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0u32.into());
}

fn test_ff_function_call_part_select_of_signed_formal_is_unsigned(sim) {
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0xFFu32.into());
}

fn test_ff_function_call_preserves_unsigned_actual_when_widening_to_signed_formal(sim) {
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0u32.into());
}

fn test_ff_function_call_preserves_unsigned_formal_signedness_for_nonvariable_actual(sim) {
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 0xFEu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x7Fu32.into());
}

fn test_ff_function_call_nonvariable_argument_uses_formal_shape_for_indexing(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");
    let side = sim.signal("side");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x33u32.into());
    assert_eq!(sim.get(side), 0x11u32.into());
}

fn test_ff_function_call_array_literal_snapshots_scalar_before_later_write(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let changing = sim.signal("changing");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(changing, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(changing), 6u8.into());
    assert_eq!(sim.get(out_q), 5u8.into());
}

fn test_ff_function_call_array_literal_snapshots_scalar_before_callee_write(sim) {
    @omit_veryl;
    @ignore_on(wasm);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let global_value = sim.signal("global_value");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(global_value, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(global_value), 9u8.into());
    assert_eq!(sim.get(out_q), 5u8.into());
}

fn test_ff_case_range_skips_effectful_upper_bound_when_lower_is_false(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            d: input logic<8>,
            q: output logic
        ) {
            function observed_upper () -> logic<8> {
                $display("upper");
                return 8'd10;
            }

            function select (target: input logic<8>) -> logic {
                case target {
                    8'd5 .. observed_upper(): return 1'b1;
                    default: return 1'b0;
                }
            }

            always_ff (clk) {
                q = select(d);
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
    assert!(sim.drain_runtime_events().is_empty());

    sim.modify(|io| io.set(d, 6u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "upper".to_string(),
        }],
    );
}

fn test_ff_function_call_array_literal_branch_view_is_reused_after_merge(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x11u32.into());
}

fn test_ff_function_call_restores_array_literal_view_after_reentrant_call(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x11u32.into());
}

fn test_ff_function_call_restores_nearest_array_view_after_deep_reentrant_call(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x11u32.into());
}

fn test_ff_function_call_bits_and_size_evaluate_effectful_array_argument(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let index = sim.signal("index");
    let side = sim.signal("side");

    sim.modify(|io| io.set(index, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(side), 0x22u32.into());
}

fn test_ff_function_call_snapshots_pure_array_items_before_later_effect(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let q = sim.signal("q");
    let changing = sim.signal("changing");

    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u32.into());
    assert_eq!(sim.get(changing), 2u32.into());
}

fn test_ff_function_call_converts_array_literal_view_for_wider_nested_formal(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x0au32.into());
}

fn test_ff_function_call_array_literal_element_uses_element_width(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0xFu32.into());
}

fn test_ff_function_array_element_assignment_preserves_signedness(sim) {
    @omit_veryl;
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0xffu32.into());
}

fn test_ff_function_call_array_literal_default_fill_matches_formal_shape(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x55u32.into());
}

fn test_ff_function_call_multidim_array_literal_default_fill_matches_formal_shape(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x55u32.into());
}

fn test_ff_function_call_multidim_array_literal_indexing_preserves_element_order(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let out_q = sim.signal("out_q");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0x11u32.into());
}

fn test_ff_function_call_dynamic_multidim_indexing_accepts_array_valued_items(sim) {
    @ignore_on(veryl); // https://github.com/veryl-lang/veryl/pull/3131
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
    @build Simulator::builder(code, "Top");
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
    @ignore_on(veryl);
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
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 1u32.into());
}

}

// Tests that use setup_and_trace/snapshot/Simulation::builder stay as regular #[test]

#[test]
fn test_ff_effectful_output_destination_snapshots_input_first() {
    let code = r#"
        module Top (
            clk: input clock,
            index_state: output logic<2>,
            entries: output logic<8>[4],
            q: output logic<2>
        ) {
            function advance (written: output logic<2>) -> logic<2> {
                $display("advance");
                written = 2'd1;
                return 2'd0;
            }

            function write_at (
                original: input logic<2>,
                written: output logic<8>
            ) -> logic<2> {
                written = 8'ha5;
                return original;
            }

            always_ff (clk) {
                q = write_at(index_state, entries[advance(index_state)]);
            }
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .optimize(false)
        .trace_pre_optimized_sir()
        .build_with_trace();
    let sir = result.trace.format_pre_optimized_sir().unwrap();
    let input_snapshot = sir
        .find("Load(addr=index_state (region=0)")
        .unwrap_or_else(|| panic!("call-time input snapshot:\n{sir}"));
    let destination_effect = sir
        .find("RuntimeEvent(")
        .unwrap_or_else(|| panic!("effectful output destination:\n{sir}"));
    assert!(
        input_snapshot < destination_effect,
        "the input must be snapshotted before output-destination effects:\n{sir}",
    );
}

#[test]
fn test_ff_case_target_is_snapshotted_before_effectful_pattern() {
    let code = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            function observed_pattern (x: input logic<8>) -> logic<8> {
                $display("pattern=%0d", x);
                return x;
            }

            function select (x: input logic<8>) -> logic<8> {
                case x * 8'd13 {
                    observed_pattern(8'd130): return 8'd1;
                    default: return 8'd0;
                }
            }

            always_ff (clk) {
                q = select(d);
            }
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .optimize(false)
        .trace_pre_optimized_sir()
        .build_with_trace();
    let sir = result.trace.format_pre_optimized_sir().unwrap();
    let target = sir.find(" Mul ").expect("case target multiplication");
    let pattern = sir.find("RuntimeEvent(").expect("effectful case pattern");

    assert!(
        target < pattern,
        "the case target must be evaluated before an effectful pattern:\n{sir}",
    );
}

#[test]
fn test_ff_case_range_snapshots_pure_lower_bound() {
    let code = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            function observed_bound (x: input logic<8>) -> logic<8> {
                $display("bound=%0d", x);
                return x;
            }

            function select (x: input logic<8>) -> logic<8> {
                case x {
                    (8'd10 * 8'd13) ..= observed_bound(8'd132): return 8'd1;
                    default: return 8'd0;
                }
            }

            always_ff (clk) {
                q = select(d);
            }
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .optimize(false)
        .trace_pre_optimized_sir()
        .build_with_trace();
    let sir = result.trace.format_pre_optimized_sir().unwrap();
    let lower = sir
        .find("SIRValue(0x82)")
        .unwrap_or_else(|| panic!("pure lower-bound value:\n{sir}"));
    let upper = sir.find("RuntimeEvent(").expect("effectful upper bound");

    assert!(
        lower < upper,
        "the pure lower bound must be evaluated before an effectful upper bound:\n{sir}",
    );
}

#[test]
fn test_ff_assert_pure_message_argument_stays_in_failure_block() {
    let code = r#"
        module Top (clk: input clock, ok: input logic, d: input logic<8>) {
            always_ff (clk) {
                $assert_continue(ok, "value=%0d", d * 8'd13);
            }
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .optimize(false)
        .trace_pre_optimized_sir()
        .build_with_trace();
    let sir = result.trace.format_pre_optimized_sir().unwrap();
    let branch = sir
        .find("Branch(")
        .unwrap_or_else(|| panic!("assertion branch in FF SIR:\n{sir}"));
    let multiply = sir.find(" Mul ").expect("pure message argument in FF SIR");
    let event = sir
        .find("RuntimeEvent(")
        .expect("assertion runtime event in FF SIR");

    assert!(
        branch < multiply && multiply < event,
        "pure message argument should be evaluated only after entering the failure block:\n{sir}",
    );
}

#[test]
fn test_ff_assert_effectful_args_snapshot_earlier_pure_values_before_branch() {
    let code = r#"
        module Top (clk: input clock, effect: output logic<8>) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x + 8'd1;
            }

            always_ff (clk) {
                $assert_continue(1'b0, "%0d %0d", effect, update(effect, effect));
            }
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .optimize(false)
        .trace_pre_optimized_sir()
        .build_with_trace();
    let program = result.trace.pre_optimized_sir.unwrap();
    let unit = program
        .sir
        .eval_apply_ffs
        .values()
        .flatten()
        .next()
        .expect("FF execution unit");
    let first_event_arg = unit
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            celox_sir::SIRInstruction::RuntimeEvent { args, .. } => args.first().copied(),
            _ => None,
        })
        .expect("assertion event argument");
    let defining_block = unit
        .blocks
        .iter()
        .find_map(|(block_id, block)| {
            block
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(
                        instruction,
                        celox_sir::SIRInstruction::Load(dst, ..) if *dst == first_event_arg
                    )
                })
                .then_some(*block_id)
        })
        .expect("first assertion argument definition");

    assert_eq!(
        defining_block, unit.entry_block_id,
        "an earlier pure argument must be snapshotted before branching when a later argument is effectful",
    );
}

#[test]
fn test_ff_assert_trailing_pure_arg_stays_in_failure_block() {
    let code = r#"
        module Top (
            clk: input clock,
            ok: input logic,
            d: input logic<8>,
            effect: output logic<8>
        ) {
            function update (
                x: input logic<8>,
                written: output logic<8>
            ) -> logic<8> {
                written = x + 8'd1;
                return x;
            }

            always_ff (clk) {
                $assert_continue(ok, "%0d %0d", update(d, effect), d * 8'd13);
            }
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .optimize(false)
        .trace_pre_optimized_sir()
        .build_with_trace();
    let sir = result.trace.format_pre_optimized_sir().unwrap();
    let branch = sir
        .find("Branch(")
        .unwrap_or_else(|| panic!("assertion branch in FF SIR:\n{sir}"));
    let multiply = sir
        .find(" Mul ")
        .unwrap_or_else(|| panic!("trailing pure assertion argument in FF SIR:\n{sir}"));

    assert!(
        branch < multiply,
        "the trailing pure argument should remain in the failure block:\n{sir}",
    );
}

#[test]
fn test_ff_runtime_for_wide_dynamic_bound_is_still_allowed() {
    let code = r#"
        module Top (
            clk: input clock,
            bound: input logic<128>,
            q_hits: output logic<8>,
            q_last: output logic<32>
        ) {
            always_ff (clk) {
                q_hits = 0;
                q_last = 32'hffff_ffff;
                for i in (bound - 1) .. bound {
                    q_hits += 1;
                    q_last = i as 32;
                }
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let bound = sim.signal("bound");
    let q_last = sim.signal("q_last");

    sim.modify(|io| io.set_wide(bound, BigUint::from(2u32)))
        .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_last), 1u32.into());
}

#[test]
fn test_single_clock_optimization() {
    let code = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            always_ff (clk) { q = d; }
        }
    "#;
    let trace = setup_and_trace(code, "Top");
    let program = trace.post_optimized_sir.unwrap();
    assert!(program.sir.eval_only_ffs.is_empty());
    assert!(program.sir.apply_ffs.is_empty());
}

#[test]
fn test_multi_clock_no_optimization() {
    let code = r#"
        module Top (clk1: input clock, clk2: input clock, d1: input logic<8>, q1: output logic<8>) {
            always_ff (clk1) { q1 = d1; }
            always_ff (clk2) { }
        }
    "#;
    let trace = setup_and_trace(code, "Top");
    let program = trace.post_optimized_sir.unwrap();
    assert!(!program.sir.eval_only_ffs.is_empty());
    assert!(!program.sir.apply_ffs.is_empty());
}

#[test]
fn test_ff_dynamic_exclusive_end_preserves_sentinel_width_in_sir() {
    let code = r#"
        module Top (
            clk: input clock,
            count: input logic<128>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in 0..count {
                    q = i as 32;
                }
            }
        }
    "#;

    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert!(
        output.contains("bit<128>"),
        "dynamic exclusive end should keep the dynamic bound width in the compare path:\n{output}"
    );
}

#[test]
fn test_ff_runtime_for_wide_dynamic_bound_out_of_i32_range_errors() {
    let code = r#"
        module Top (
            clk: input clock,
            bound: input logic<128>,
            q_hits: output logic<8>,
            q_last: output logic<32>
        ) {
            always_ff (clk) {
                q_hits = 0;
                q_last = 0;
                for i in (bound - 1) .. bound {
                    q_hits += 1;
                    q_last = i as 32;
                }
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let bound = sim.signal("bound");

    sim.modify(|io| io.set_wide(bound, (BigUint::from(1u32) << 31) + BigUint::from(1u32)))
        .unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "For loop value exceeds loop variable range in always_ff (loop variable `i`): i"
    );
}

#[test]
fn test_ff_runtime_for_wide_dynamic_end_errors_before_iteration() {
    let code = r#"
        module Top (
            clk: input clock,
            count: input logic<128>,
            q_hits: output logic<8>
        ) {
            always_ff (clk) {
                q_hits = 0;
                for i in 0..count {
                    q_hits += 1;
                }
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let count = sim.signal("count");

    sim.modify(|io| io.set_wide(count, BigUint::from(1u64) << 40))
        .unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "For loop value exceeds loop variable range in always_ff (loop variable `i`): i"
    );
}

#[test]
fn test_ff_runtime_for_wide_dynamic_reverse_end_errors_before_iteration() {
    let code = r#"
        module Top (
            clk: input clock,
            start: input signed logic<64>,
            end_bound: input signed logic<128>,
            q_hits: output logic<8>
        ) {
            always_ff (clk) {
                q_hits = 0;
                for i in rev start..end_bound {
                    q_hits += 1;
                }
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");

    sim.modify(|io| {
        io.set(start, 0u64);
        io.set_wide(end_bound, BigUint::from(1u64) << 40);
    })
    .unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "For loop value exceeds loop variable range in always_ff (loop variable `i`): i"
    );
}

#[test]
fn test_ff_runtime_for_wide_dynamic_start_errors_before_empty_exit() {
    let code = r#"
        module Top (
            clk: input clock,
            start: input logic<128>,
            q_hits: output logic<8>
        ) {
            always_ff (clk) {
                q_hits = 0;
                for i in start..0 {
                    q_hits += 1;
                }
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let start = sim.signal("start");

    sim.modify(|io| io.set_wide(start, BigUint::from(1u64) << 40))
        .unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "For loop value exceeds loop variable range in always_ff (loop variable `i`): i"
    );
}

#[test]
fn test_ff_runtime_for_wide_dynamic_reverse_start_errors_before_empty_exit() {
    let code = r#"
        module Top (
            clk: input clock,
            start: input logic<128>,
            q_hits: output logic<8>
        ) {
            always_ff (clk) {
                q_hits = 0;
                for i in rev start..0 {
                    q_hits += 1;
                }
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let start = sim.signal("start");

    sim.modify(|io| io.set_wide(start, BigUint::from(1u64) << 40))
        .unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "For loop value exceeds loop variable range in always_ff (loop variable `i`): i"
    );
}

#[test]
fn test_ff_dynamic_inclusive_end_preserves_bound_width_in_sir() {
    let code = r#"
        module Top (
            clk: input clock,
            count: input logic<128>,
            q: output logic<8>
        ) {
            always_ff (clk) {
                q = 0;
                for i in 0..=count {
                    q += 1;
                }
            }
        }
    "#;

    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert!(
        output.contains("bit<128>"),
        "dynamic inclusive end should keep the dynamic bound width in the compare path:\n{output}"
    );
}

#[test]
fn test_internal_generated_clock() {
    // Test: half-rate clock drives a downstream FF.
    // clk_div is provided externally as a clock input (half rate of clk).
    let code = r#"
        module Top (
            clk: input '_ clock,
            clk_div: input '_ clock,
            d:   input logic<8>,
            q:   output logic<8>
        ) {
            // Downstream FF driven by the half-rate clock
            always_ff (clk_div) {
                q = d;
            }
        }
    "#;
    let mut simulation = Simulation::builder(code, "Top").build().unwrap();

    let d = simulation.signal("d");
    let q = simulation.signal("q");

    // Set input data
    simulation.modify(|io| io.set(d, 0xAAu8)).unwrap();

    // clk at 10-tick period, clk_div at 20-tick period (half rate)
    simulation.add_clock("clk", 10, 0);
    simulation.add_clock("clk_div", 20, 0);

    // Run until t=5, which includes the first rising edge of clk_div at t=0.
    // The downstream FF should capture 'd' (0xAA).
    simulation.run_until(5).unwrap();

    assert_eq!(
        simulation.get(q),
        0xAAu32.into(),
        "Downstream FF should have captured 0xAA when clk_div rose"
    );
}

#[test]
fn test_store_coalescing_sir() {
    let trace = setup_and_trace(
        r#"
        module ModuleA (clk: input clock,a: input logic<8>,b: input logic<8>,c: input logic<8>,d: input logic<8>){
            var mem: logic<8> [4];

            always_ff {
                mem[0] = a;
                mem[1] = b;
                mem[2] = c;
                mem[3] = d;
            }
        }
"#,
        "ModuleA",
    );
    let output = trace.format_program().unwrap();
    assert_snapshot!("store_coalescing_sir", output);
}

#[test]
fn test_rle_sir() {
    let trace = setup_and_trace(
        r#"
module ModuleA (
    clk: input clock,
    x: input logic<32>
) {
    var a: logic<32>;
    var b: logic<32>;
    var c: logic<32>;
    var d: logic<32>;

    always_ff (clk) {
        // Simple RLE
        a = x;
        b = x;

        // Nonblocking semantics in always_ff:
        // d = c reads OLD stable c (not the just-assigned c = x),
        // so this should remain a load from stable c.
        c = x;
        d = c;
    }
}
"#,
        "ModuleA",
    );
    let output = trace.format_program().unwrap();
    assert_snapshot!("rle_sir", output);
}

#[test]
fn test_ff_dynamic_store_sir() {
    let code = r#"
    module Top (
        clk: input clock,
        i: input logic<2>,
        val: input logic<8>
    ) {
        var a: logic<8> [4];
        always_ff (clk) {
            // Dynamic write in FF should generate Store with SIROffset::Dynamic (offset=rX)
            a[i] = val;
        }
    }
"#;
    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("ff_dynamic_store_sir", output);
}

#[test]
fn test_ff_function_array_literal_view_sir() {
    let code = r#"
    module Top (
        clk: input clock,
        index: input logic<2>,
        in0: input logic<8>,
        in1: input logic<8>,
        in2: input logic<8>,
        in3: input logic<8>,
        out_q: output logic<8>
    ) {
        function select (x: input logic<8>[4], index: input logic<2>) -> logic<8> {
            return x[index];
        }
        always_ff (clk) {
            out_q = select('{in0, in1, in2, in3}, index);
        }
    }
"#;
    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("ff_function_array_literal_view_sir", output);
}

#[test]
fn test_ff_function_static_array_literal_access_is_lazy() {
    let code = r#"
    module Top (
        clk: input clock,
        in0: input logic<8>,
        out_q: output logic<8>
    ) {
        function first (x: input logic<8>[1024]) -> logic<8> {
            return x[0];
        }
        always_ff (clk) {
            out_q = first('{default: in0});
        }
    }
"#;
    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();

    assert!(!output.contains("Store(addr=first.x"), "{output}");
    assert!(output.lines().count() < 100, "{output}");
}

#[test]
fn test_ff_nested_function_static_array_literal_access_is_lazy() {
    let code = r#"
    module Top (
        clk: input clock,
        in0: input logic<8>,
        out_q: output logic<8>
    ) {
        function first (x: input logic<8>[1024]) -> logic<8> {
            return x[0];
        }
        function forward (x: input logic<8>[1024]) -> logic<8> {
            return first(x);
        }
        always_ff (clk) {
            out_q = forward('{default: in0});
        }
    }
"#;
    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();

    assert!(!output.contains("Store(addr=forward.x"), "{output}");
    assert!(!output.contains("Store(addr=first.x"), "{output}");
    assert!(output.lines().count() < 100, "{output}");
}

#[test]
fn test_ff_static_branch_array_literal_access_is_lazy() {
    let code = r#"
    module Top (
        clk: input clock,
        guard: input logic,
        in0: input logic<8>,
        out_q: output logic<8>
    ) {
        function first_if (
            x: input logic<8>[1024],
            guard: input logic
        ) -> logic<8> {
            return if guard ? x[0] : 8'h00;
        }
        always_ff (clk) {
            out_q = first_if('{default: in0}, guard);
        }
    }
"#;
    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();

    assert!(!output.contains("Store(addr=first_if.x"), "{output}");
    assert!(output.lines().count() < 150, "{output}");
}

#[test]
fn test_ff_array_literal_argument_is_not_reevaluated_for_array_output() {
    let code = r#"
    module Top (
        clk: input clock,
        in0: input logic<8>,
        out_q: output logic<8>
    ) {
        function observe (
            x: input logic<8>,
            side: output logic<8>
        ) -> logic<8> {
            side = x;
            return x;
        }
        function copy (
            x: input logic<8>[2],
            y: output logic<8>[2]
        ) -> logic<8> {
            y = x;
            return y[0];
        }
        var copied: logic<8>[2];
        var side: logic<8>;
        always_ff (clk) {
            out_q = copy('{observe(in0, side), 8'h22}, copied);
        }
    }
"#;
    let result = SimulatorBuilder::new(code, "Top")
        .optimize(false)
        .trace_sim_modules()
        .trace_pre_optimized_sir()
        .build_with_trace();
    assert!(result.res.is_ok(), "{:?}", result.res.err());
    let output = result.trace.format_pre_optimized_sir().unwrap();

    assert_eq!(output.matches("Store(addr=side").count(), 1, "{output}");
}

#[test]
fn test_ff_array_literal_static_then_dynamic_access_evaluates_each_item_once() {
    let code = r#"
    module Top (
        clk: input clock,
        in0: input logic<8>,
        index: input logic,
        out_q: output logic<8>
    ) {
        function observe (
            x: input logic<8>,
            side: output logic<8>
        ) -> logic<8> {
            side = x;
            return x;
        }
        function mixed (
            x: input logic<8>[2],
            index: input logic,
            first: output logic<8>
        ) -> logic<8> {
            first = x[0];
            return x[index];
        }
        var first: logic<8>;
        var side: logic<8>;
        always_ff (clk) {
            out_q = mixed('{observe(in0, side), 8'h00}, index, first);
        }
    }
"#;
    let result = SimulatorBuilder::new(code, "Top")
        .optimize(false)
        .trace_sim_modules()
        .trace_pre_optimized_sir()
        .build_with_trace();
    assert!(result.res.is_ok(), "{:?}", result.res.err());
    let output = result.trace.format_pre_optimized_sir().unwrap();

    assert_eq!(output.matches("Store(addr=side").count(), 1, "{output}");
}

#[test]
#[cfg(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", feature = "experimental-arm64-backend")
))]
fn test_ff_packed_bit_select_writes_regression() {
    let code = r#"
    module Top (
        clk: input clock,
        rst: input reset,
        o: output logic<4>
    ) {
        always_ff (clk, rst) {
            if_reset {
                o = 0;
            } else {
                o = 0;
                o[0] = 1;
                o[1] = 1;
                o[2] = 1;
            }
        }
    }
"#;

    let mut sim = Simulator::builder(code, "Top")
        .optimize(true)
        .build_native()
        .unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let o = sim.signal("o");

    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), 7u8.into());
}

#[test]
fn test_commit_sinking_multi_store_sir() {
    let code = r#"
    module Top (
        clk: input clock,
        rst: input reset,
        a: output logic<8>,
        b: output logic<8>
    ) {
        always_ff (clk, rst) {
            if_reset {
                a = 0;
                b = 0;
            } else {
                a = 1;
                b = 2;
            }
        }
    }
"#;

    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();

    assert_snapshot!("commit_sinking_multi_store_sir", output);
}

#[test]
fn test_ff_common_load_hoisting_sir() {
    let code = r#"
    module Top (
        clk: input clock,
        rst: input reset,
        d: input logic<8>,
        a: output logic<8>,
        b: output logic<8>
    ) {
        always_ff (clk, rst) {
            if_reset {
                a = d;
            } else {
                b = d;
            }
        }
    }
"#;

    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();

    assert_snapshot!("ff_common_load_hoisting_sir", output);
}

#[test]
fn test_ff_function_call_multistatement_hoisting_compile() {
    let code = r#"
    module Top (
        clk: input clock,
        d  : input logic<8>,
        q  : output logic<8>,
    ) {
        function f (
            x: input logic<8>,
        ) -> logic<8> {
            if x == 8'd0 {
                return x + 8'd1;
            }
            return x + 8'd2;
        }

        always_ff {
            q = f(d);
        }
    }
"#;

    let trace = setup_and_trace(code, "Top");

    let output = trace.format_program().unwrap();
    assert_snapshot!("ff_function_call_multistatement_hoisting_sir", output);
}

#[test]
fn test_async_reset_sir_snapshot() {
    let code = r#"
module Top (
    clk: input clock,
    rst: input reset_async_high,
    d: input logic<8>,
    q: output logic<8>,
) {
    always_ff (clk, rst) {
        if_reset {
            q = 0;
        } else {
            q = d;
        }
    }
}
"#;

    let trace = setup_and_trace(code, "Top");
    let sir_output = trace.format_program().unwrap();
    insta::assert_snapshot!("async_reset_sir", sir_output);
}

#[test]
fn test_benchmark_loop_sir() {
    let code = r#"
    module Top #(
        param N: u32 = 10,
    )(
        clk: input clock,
        rst: input reset,
        cnt: output logic<32>[N],
    ) {
        for i in 0..N: g {
            always_ff (clk, rst) {
                if_reset {
                    cnt[i] = 0;
                } else {
                    cnt[i] += 1;
                }
            }
        }
    }
    "#;
    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("benchmark_loop_sir", output);
}
