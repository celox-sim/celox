use celox::{BigUint, Simulation, Simulator, SimulatorBuilder};
use insta::assert_snapshot;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

fn setup_and_trace(code: &str, top: &str) -> celox::CompilationTrace {
    let result = SimulatorBuilder::new(code, top)
        .allow_always_ff_function_effects(true)
        .optimize(true)
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .build_with_trace();

    result.trace
}

all_backends! {

fn test_ff_nonblocking(sim) {
    @case "flip_flop::test_ff_nonblocking";
}

fn test_ff_statement_after_if_reset_keeps_source_order(sim) {
    @case "flip_flop::test_ff_statement_after_if_reset_keeps_source_order";
}

fn test_ff_static_and_dynamic_writes_share_sparse_state(sim) {
    @case "flip_flop::test_ff_static_and_dynamic_writes_share_sparse_state";
}

fn test_ff_runtime_display_and_assert_continue(sim) {
    @omit_veryl;
    @ignore_on(sv);
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
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_assert_message_output_argument_is_eager";
}

fn test_ff_assert_message_runtime_effect_is_eager(sim) {
    @omit_veryl;
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
    @case "flip_flop::test_ff_runtime_function_snapshots_input_before_callee_nonlocal_write";
}

fn test_ff_runtime_function_snapshots_helper_input_before_callee_nonlocal_write(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_runtime_function_snapshots_helper_input_before_callee_nonlocal_write";
}

fn test_ff_outputless_nested_nonlocal_write_updates_later_event_argument(sim) {
    @omit_veryl;
    @ignore_on(sv);
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
    @ignore_on(sv);
    @case "flip_flop::test_ff_statement_function_direct_nonlocal_assignment_is_observable";
}

fn test_ff_skipped_conditional_nonlocal_write_preserves_prior_ff_assignment(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_skipped_conditional_nonlocal_write_preserves_prior_ff_assignment";
}

fn test_ff_nonlocal_write_precedes_aliased_formal_output_copyout(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_nonlocal_write_precedes_aliased_formal_output_copyout";
}

fn test_ff_outputless_wrapper_nested_copyout_to_nonlocal_is_observable(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_outputless_wrapper_nested_copyout_to_nonlocal_is_observable";
}

fn test_ff_outputless_wrapper_expression_copyout_to_nonlocal_is_observable(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_outputless_wrapper_expression_copyout_to_nonlocal_is_observable";
}

fn test_ff_outputless_wrapper_indexed_copyout_to_nonlocal_is_observable(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_outputless_wrapper_indexed_copyout_to_nonlocal_is_observable";
}

fn test_ff_outputless_wrapper_dynamic_indexed_copyout_to_nonlocal_is_observable(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_outputless_wrapper_dynamic_indexed_copyout_to_nonlocal_is_observable";
}

fn test_ff_outputless_wrapper_direct_dynamic_nonlocal_assignment_is_observable(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_outputless_wrapper_direct_dynamic_nonlocal_assignment_is_observable";
}

fn test_ff_pure_helper_nonlocal_read_is_snapshotted_before_runtime_write(sim) {
    @omit_veryl;
    @ignore_on(sv);
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
    @ignore_on(sv);
    @case "flip_flop::test_ff_dynamic_nonlocal_store_follows_pending_whole_write";
}

fn test_ff_guarded_system_task_merges_definition_state(sim) {
    @omit_veryl;
    @ignore_on(sv);
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
    @ignore_on(sv);
    @case "flip_flop::test_ff_nonlocal_source_ternary_preserves_unknown_merge";
}

fn test_ff_statement_helper_dynamic_nonlocal_copyout_is_observable(sim) {
    @omit_veryl;
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_output_index_uses_final_nonlocal_state";
}

fn test_ff_guarded_runtime_expression_merges_definition_state(sim) {
    @omit_veryl;
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
    @case "flip_flop::test_ff_short_circuit_runtime_write_preserves_later_state_source";
}

fn test_ff_pure_predicate_output_updates_outer_function_state(sim) {
    @omit_veryl;
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
    @case "flip_flop::test_ff_bits_and_size_operands_do_not_alias_earlier_array_argument";
}

fn test_ff_bits_and_size_array_dependencies_do_not_alias_later_write(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_bits_and_size_array_dependencies_do_not_alias_later_write";
}

fn test_ff_statement_call_materializes_output_only_input_effect(sim) {
    @omit_veryl;
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
    @case "flip_flop::test_ff_pure_input_is_snapshotted_before_later_effectful_input";
}

fn test_ff_runtime_event_arguments_use_per_argument_state(sim) {
    @omit_veryl;
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
    @case "flip_flop::test_ff_effectful_array_item_output_is_not_a_read_alias";
}

fn test_ff_symbolic_runtime_input_uses_declared_formal_type(sim) {
    @omit_veryl;
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(wasm, sv);
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
    @ignore_on(wasm, sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
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
    @ignore_on(sv);
    @case "flip_flop::test_ff_runtime_for_bounds";
}

fn test_ff_runtime_for_bitwise_steps(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_runtime_for_bitwise_steps";
}

fn test_ff_signed_xor_step_uses_loop_counter_width(sim) {
    @ignore_on(veryl, sv);
    @case "flip_flop::test_ff_signed_xor_step_uses_loop_counter_width";
}

fn test_ff_i32_bitwise_steps_discard_bits_above_the_counter_width(sim) {
    @ignore_on(veryl, sv);
    @case "flip_flop::test_ff_i32_bitwise_steps_discard_bits_above_the_counter_width";
}

fn test_ff_i32_xor_step_with_only_high_bits_reports_true_loop(sim) {
    @ignore_on(veryl, sv);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input logic<32>,
            end_bound: input logic<32>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in start..end_bound step ^= 4294967296 {
                    q = i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");

    sim.modify(|io| {
        io.set(start, 3u32);
        io.set(end_bound, 4u32);
    })
    .unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "Non-progressing for loop in always_ff (loop variable `i`): i"
    );
}

fn test_ff_i32_or_step_with_only_existing_low_bits_reports_true_loop(sim) {
    @ignore_on(veryl, sv);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input logic<32>,
            end_bound: input logic<32>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in start..end_bound step |= 4294967299 {
                    q = i;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");

    sim.modify(|io| {
        io.set(start, 3u32);
        io.set(end_bound, 4u32);
    })
    .unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "Non-progressing for loop in always_ff (loop variable `i`): i"
    );
}

fn test_ff_i32_mul_step_overflow_reports_true_loop(sim) {
    @ignore_on(veryl, sv);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input signed logic<32>,
            end_bound: input signed logic<64>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in start..end_bound step *= 2 {
                    q += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");

    // The first update overflows i32 even though its widened value would
    // already exceed this still-representable bound.
    sim.set(start, 1_500_000_000u32);
    sim.set(end_bound, 1_600_000_000u64);
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "Non-progressing for loop in always_ff (loop variable `i`): i"
    );
}

fn test_ff_i32_shl_step_overflow_reports_true_loop(sim) {
    @ignore_on(veryl, sv);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            start: input signed logic<32>,
            end_bound: input signed logic<64>,
            q: output logic<32>
        ) {
            always_ff (clk) {
                q = 0;
                for i in start..end_bound step <<= 1 {
                    q += 1;
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let start = sim.signal("start");
    let end_bound = sim.signal("end_bound");

    // The first update overflows i32 even though its widened value would
    // already exceed this still-representable bound.
    sim.set(start, 1_073_741_824u32);
    sim.set(end_bound, 1_500_000_000u64);
    assert_eq!(
        sim.tick(clk).unwrap_err().to_string(),
        "Non-progressing for loop in always_ff (loop variable `i`): i"
    );
}

fn test_ff_runtime_for_break(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_runtime_for_break";
}

#[ignore]
fn test_ff_constant_signed_bounds_in_unrolled_loops(sim) {
    @case "flip_flop::test_ff_constant_signed_bounds_in_unrolled_loops";
}

fn test_ff_runtime_for_dynamic_zero_start_mul_reports_true_loop(sim) {
    @ignore_on(sv);
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
    let error = sim.tick(clk).unwrap_err();
    // Only the Veryl reference adapter lacks Celox's detailed loop diagnostic.
    let expected = if (&sim as &dyn std::any::Any)
        .is::<test_utils::veryl_sim::VerylSimAdapter>()
    {
        celox::RuntimeErrorCode::DetectedTrueLoop
    } else {
        celox::RuntimeErrorCode::Runtime {
            message: "Non-progressing for loop in always_ff (loop variable `i`)".into(),
            signals: vec!["i".into()],
        }
    };
    assert_eq!(error, expected);
}

fn test_ff_runtime_for_zero_iteration_mul_loop_is_allowed(sim) {
    @case "flip_flop::test_ff_runtime_for_zero_iteration_mul_loop_is_allowed";
}

fn test_ff_runtime_for_terminal_inclusive_mul_loop_reports_true_loop(sim) {
    @ignore_on(sv);
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
    let error = sim.tick(clk).unwrap_err();
    // Only the Veryl reference adapter lacks Celox's detailed loop diagnostic.
    let expected = if (&sim as &dyn std::any::Any)
        .is::<test_utils::veryl_sim::VerylSimAdapter>()
    {
        celox::RuntimeErrorCode::DetectedTrueLoop
    } else {
        celox::RuntimeErrorCode::Runtime {
            message: "Non-progressing for loop in always_ff (loop variable `i`)".into(),
            signals: vec!["i".into()],
        }
    };
    assert_eq!(error, expected);
}

fn test_ff_runtime_reverse_step_matches_emitted_sv_order(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_runtime_reverse_step_matches_emitted_sv_order";
}

fn test_ff_runtime_reverse_exclusive_i32_upper_sentinel(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_runtime_reverse_exclusive_i32_upper_sentinel";
}

fn test_ff_runtime_reverse_min_i32_end_wraps_before_range_check(sim) {
    @ignore_on(veryl, sv);
    @case "flip_flop::test_ff_runtime_reverse_min_i32_end_wraps_before_range_check";
}

fn test_ff_runtime_reverse_i32_step_truncation_reports_true_loop(sim) {
    @ignore_on(veryl, sv);
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
    @ignore_on(sv);
    @case "flip_flop::test_ff_runtime_for_reverse_singleton_exits_cleanly";
}

fn test_ff_runtime_for_signed_inclusive_range_preserves_negative_bounds(sim) {
    @ignore_on(veryl, sv);
    @case "flip_flop::test_ff_runtime_for_signed_inclusive_range_preserves_negative_bounds";
}

fn test_ff_runtime_for_forward_overshoot_exits_without_wraparound(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_runtime_for_forward_overshoot_exits_without_wraparound";
}

fn test_ff_runtime_for_unsigned_slice_bound_zero_extends_signed_source(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_runtime_for_unsigned_slice_bound_zero_extends_signed_source";
}

fn test_ff_if_reset_basic(sim) {
    @case "flip_flop::test_ff_if_reset_basic";
}

fn test_async_reset(sim) {
    @ignore_on(veryl);
    @case "flip_flop::test_async_reset";
}

fn test_ff_swap_correctness(sim) {
    @case "flip_flop::test_ff_swap_correctness";
}

fn test_multiple_clocks(sim) {
    @case "flip_flop::test_multiple_clocks";
}

fn test_hierarchical_clocks(sim) {
    @case "flip_flop::test_hierarchical_clocks";
}

fn test_multiple_async_resets(sim) {
    @ignore_on(veryl);
    @case "flip_flop::test_multiple_async_resets";
}

fn test_ff_if_reset_multi_cycle(sim) {
    @case "flip_flop::test_ff_if_reset_multi_cycle";
}

fn test_ff_if_reset_with_nested_if(sim) {
    @case "flip_flop::test_ff_if_reset_with_nested_if";
}

fn test_ff_struct_constructor_expression(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_struct_constructor_expression";
}

fn test_ff_struct_constructor_expression_literal_order(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_struct_constructor_expression_literal_order";
}

fn test_ff_struct_constructor_signed_member_extension(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_struct_constructor_signed_member_extension";
}

fn test_ff_array_literal_expression_order(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_array_literal_expression_order";
}

fn test_ff_array_literal_default_expression(sim) {
    @ignore_on(veryl, sv);
    @case "flip_flop::test_ff_array_literal_default_expression";
}

fn test_ff_array_literal_nested_default_multidim_expression(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_array_literal_nested_default_multidim_expression";
}

fn test_ff_function_call_expression(sim) {
    @case "flip_flop::test_ff_function_call_expression";
}

fn test_ff_function_call_statement_with_output_argument(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_statement_with_output_argument";
}

fn test_ff_function_call_statement_with_output_argument_and_return_value(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_statement_with_output_argument_and_return_value";
}

fn test_ff_function_call_expression_with_output_argument_and_return_value(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_expression_with_output_argument_and_return_value";
}

fn test_ff_function_call_expression_with_if(sim) {
    @case "flip_flop::test_ff_function_call_expression_with_if";
}

fn test_ff_nested_function_call_expression(sim) {
    @case "flip_flop::test_ff_nested_function_call_expression";
}

fn test_ff_function_call_multistatement_body(sim) {
    @case "flip_flop::test_ff_function_call_multistatement_body";
}

fn test_ff_function_call_indexed_argument_access(sim) {
    @ignore_on(veryl, sv);
    @case "flip_flop::test_ff_function_call_indexed_argument_access";
}

fn test_ff_function_call_nested_output_statement_in_function_body(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_nested_output_statement_in_function_body";
}

fn test_ff_function_call_indexed_nonvariable_argument_expression(sim) {
    @case "flip_flop::test_ff_function_call_indexed_nonvariable_argument_expression";
}

fn test_ff_function_call_chained_range_access_on_argument(sim) {
    @case "flip_flop::test_ff_function_call_chained_range_access_on_argument";
}

fn test_ff_function_call_step_access_on_nonvariable_argument(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_step_access_on_nonvariable_argument";
}

fn test_ff_function_call_nonvariable_argument_uses_formal_width_before_slice(sim) {
    @case "flip_flop::test_ff_function_call_nonvariable_argument_uses_formal_width_before_slice";
}

fn test_ff_function_call_nonvariable_argument_preserves_self_sized_overflow_before_coercion(sim) {
    // Deferred Veryl/Celox fix: input-formal width must reach the addition
    // before evaluation (IEEE 1800-2023 10.8, 11.8.2). These backends return 0
    // instead of 1. Keep the passing SV frontend enabled; see the suite's
    // MISMATCH_REVIEW.md section 2 for the retained failure observations.
    @ignore_on(native, cranelift, wasm, interp, veryl);
    @case "flip_flop::test_ff_function_call_nonvariable_argument_preserves_self_sized_overflow_before_coercion";
}

fn test_ff_function_call_part_select_of_signed_formal_is_unsigned(sim) {
    @ignore_on(veryl);
    @case "flip_flop::test_ff_function_call_part_select_of_signed_formal_is_unsigned";
}

fn test_ff_function_call_sign_extends_narrow_signed_actual_before_slice(sim) {
    @case "flip_flop::test_ff_function_call_sign_extends_narrow_signed_actual_before_slice";
}

fn test_ff_function_call_preserves_unsigned_actual_when_widening_to_signed_formal(sim) {
    @case "flip_flop::test_ff_function_call_preserves_unsigned_actual_when_widening_to_signed_formal";
}

fn test_ff_function_call_preserves_unsigned_formal_signedness_for_nonvariable_actual(sim) {
    @case "flip_flop::test_ff_function_call_preserves_unsigned_formal_signedness_for_nonvariable_actual";
}

fn test_ff_function_call_nonvariable_argument_uses_formal_shape_for_indexing(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_nonvariable_argument_uses_formal_shape_for_indexing";
}

fn test_ff_function_call_array_literal_element_uses_formal_context_width(sim) {
    @ignore_on(veryl, sv);
    @case "flip_flop::test_ff_function_call_array_literal_element_uses_formal_context_width";
}

fn test_ff_function_call_array_literal_supports_dynamic_multidim_indexing(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_array_literal_supports_dynamic_multidim_indexing";
}

fn test_ff_function_call_array_literal_view_dominates_conditional_access(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_array_literal_view_dominates_conditional_access";
}

fn test_ff_function_call_array_literal_effect_is_eager_in_ternary_arm(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_array_literal_effect_is_eager_in_ternary_arm";
}

fn test_ff_function_call_array_literal_effect_is_eager_in_short_circuit_rhs(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_array_literal_effect_is_eager_in_short_circuit_rhs";
}

fn test_ff_function_call_array_literal_view_preserves_expression_order(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_array_literal_view_preserves_expression_order";
}

fn test_ff_function_call_array_literal_snapshots_scalar_before_later_write(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_array_literal_snapshots_scalar_before_later_write";
}

fn test_ff_function_call_array_literal_snapshots_scalar_before_callee_write(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_array_literal_snapshots_scalar_before_callee_write";
}

fn test_ff_case_range_skips_effectful_upper_bound_when_lower_is_false(sim) {
    @omit_veryl;
    @ignore_on(sv);
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
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_array_literal_branch_view_is_reused_after_merge";
}

fn test_ff_function_call_effectful_array_items_are_eager_before_conditional_access(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_effectful_array_items_are_eager_before_conditional_access";
}

fn test_ff_function_call_carries_branch_local_static_array_item_cache(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_carries_branch_local_static_array_item_cache";
}

fn test_ff_function_call_tracks_nested_static_array_read_through_branch(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_tracks_nested_static_array_read_through_branch";
}

fn test_ff_function_call_tracks_array_view_hidden_in_bound_literal(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_tracks_array_view_hidden_in_bound_literal";
}

fn test_ff_function_call_merges_nested_array_state_at_cache_completion(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_merges_nested_array_state_at_cache_completion";
}

fn test_ff_function_call_merges_nested_array_state_at_static_cache_completion(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_merges_nested_array_state_at_static_cache_completion";
}

fn test_ff_function_call_merges_directly_forwarded_array_cache(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_merges_directly_forwarded_array_cache";
}

fn test_ff_function_call_tracks_array_reads_in_output_indices(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_tracks_array_reads_in_output_indices";
}

fn test_ff_function_call_tracks_nested_array_reads_in_output_indices(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_tracks_nested_array_reads_in_output_indices";
}

fn test_ff_function_call_restores_initialized_forwarded_alias_view(sim) {
    @ignore_on(veryl, sv);
    @case "flip_flop::test_ff_function_call_restores_initialized_forwarded_alias_view";
}

fn test_ff_function_call_merges_outer_array_view_across_nested_short_circuit(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_merges_outer_array_view_across_nested_short_circuit";
}

fn test_ff_function_call_forwards_array_literal_view_to_nested_call(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_forwards_array_literal_view_to_nested_call";
}

fn test_ff_function_call_keeps_array_view_active_for_output_index(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_keeps_array_view_active_for_output_index";
}

fn test_ff_function_call_restores_array_literal_view_after_reentrant_call(sim) {
    @ignore_on(veryl, sv);
    @case "flip_flop::test_ff_function_call_restores_array_literal_view_after_reentrant_call";
}

fn test_ff_function_call_restores_nearest_array_view_after_deep_reentrant_call(sim) {
    @ignore_on(veryl, sv);
    @case "flip_flop::test_ff_function_call_restores_nearest_array_view_after_deep_reentrant_call";
}

fn test_ff_function_call_bits_and_size_evaluate_effectful_array_argument(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_bits_and_size_evaluate_effectful_array_argument";
}

fn test_ff_function_call_nested_bits_evaluates_effectful_array_argument(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_nested_bits_evaluates_effectful_array_argument";
}

fn test_ff_function_call_array_literal_view_preserves_source_order(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_array_literal_view_preserves_source_order";
}

fn test_ff_function_call_snapshots_pure_array_items_before_later_effect(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_snapshots_pure_array_items_before_later_effect";
}

fn test_ff_function_call_converts_array_literal_view_for_wider_nested_formal(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_converts_array_literal_view_for_wider_nested_formal";
}

fn test_ff_function_call_converts_forwarded_static_array_element(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_converts_forwarded_static_array_element";
}

fn test_ff_function_call_array_literal_element_uses_element_width(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_array_literal_element_uses_element_width";
}

fn test_ff_function_array_element_assignment_preserves_signedness(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_array_element_assignment_preserves_signedness";
}

fn test_ff_function_call_array_literal_default_fill_matches_formal_shape(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_array_literal_default_fill_matches_formal_shape";
}

fn test_ff_function_call_multidim_array_literal_default_fill_matches_formal_shape(sim) {
    @ignore_on(veryl, sv);
    @case "flip_flop::test_ff_function_call_multidim_array_literal_default_fill_matches_formal_shape";
}

fn test_ff_function_call_multidim_array_literal_indexing_preserves_element_order(sim) {
    @ignore_on(sv);
    @case "flip_flop::test_ff_function_call_multidim_array_literal_indexing_preserves_element_order";
}

fn test_ff_function_call_dynamic_multidim_indexing_accepts_array_valued_items(sim) {
    @ignore_on(veryl, sv);
    @case "flip_flop::test_ff_function_call_dynamic_multidim_indexing_accepts_array_valued_items";
}

fn test_ff_function_call_bit_select_on_nonvariable_one_bit_formal(sim) {
    @case "flip_flop::test_ff_function_call_bit_select_on_nonvariable_one_bit_formal";
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
        .allow_always_ff_function_effects(true)
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
        .allow_always_ff_function_effects(true)
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
        .allow_always_ff_function_effects(true)
        .optimize(false)
        .trace_sim_modules()
        .trace_pre_optimized_sir()
        .build_with_trace();
    assert!(result.res.is_ok(), "{:?}", result.res.err());
    let output = result.trace.format_pre_optimized_sir().unwrap();

    assert_eq!(output.matches("Store(addr=side").count(), 1, "{output}");
}

#[test]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
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
