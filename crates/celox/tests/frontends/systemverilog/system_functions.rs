use super::*;

#[test]
fn veryl_assert_unknown_conditions_are_eager_on_interpreter() {
    for process in ["always_comb", "always_ff @(posedge clk)"] {
        let source = format!(
            r#"
            module Top(input logic clk, enabled, input logic [1:0] condition);
              function automatic int message_value();
                $display("inside");
                return 7;
              endfunction
              {process} if (enabled)
                $assert_continue(condition, "value=%0d", message_value());
            endmodule
        "#
        );
        let mut sim = Simulator::from_sv_sources(
            vec![(&source, Path::new("veryl_assert_unknown.sv"))],
            "Top",
        )
        .four_state(true)
        .build_interpreter()
        .unwrap();
        sim.drain_runtime_events();
        let enabled = sim.signal("enabled");
        let condition = sim.signal("condition");
        sim.modify(|io| {
            io.set(enabled, 1u8);
            io.set_four_state(condition, BigUint::from(2u8), BigUint::from(1u8));
        })
        .unwrap();
        if process.starts_with("always_ff") {
            sim.tick(sim.event("clk")).unwrap();
        }
        assert_eq!(
            sim.drain_runtime_events(),
            vec![celox::RuntimeEvent::Display {
                message: b"inside".to_vec()
            },]
        );
        sim.modify(|io| {
            io.set_four_state(condition, BigUint::from(0u8), BigUint::from(1u8));
        })
        .unwrap();
        if process.starts_with("always_ff") {
            sim.tick(sim.event("clk")).unwrap();
        }
        assert_eq!(
            sim.drain_runtime_events(),
            vec![
                celox::RuntimeEvent::Display {
                    message: b"inside".to_vec()
                },
                celox::RuntimeEvent::AssertContinue {
                    message: b"value=7".to_vec()
                },
            ]
        );
    }
}

const URANDOM_SKIPPED_INITIAL_SOURCE: &str = r#"
    module Top(input int seed, output int unsigned value, reference);
      initial $display("skipped initial");
      always_comb value = $urandom(seed);
      always_comb reference = $urandom(17);
    endmodule
"#;

#[test]
fn urandom_comb_skipped_initials_work_on_interpreter_and_tiered() {
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let value = sim.signal("value");
        assert_ne!(sim.get(value), 0u8.into());
        let seed = sim.signal("seed");
        sim.modify(|io| io.set(seed, 17u8)).unwrap();
        assert_eq!(sim.get(value), sim.get(sim.signal("reference")));
        assert!(sim.drain_runtime_events().is_empty());
    }
    let builder = || {
        Simulator::from_sv_sources(
            vec![(
                URANDOM_SKIPPED_INITIAL_SOURCE,
                Path::new("random_skipped_initial.sv"),
            )],
            "Top",
        )
        .four_state(true)
    };
    check(builder().build_interpreter().unwrap());
    check(builder().build_tiered().unwrap());
}

#[test]
fn urandom_initial_extends_signed_seed_values() {
    let mut sim = initial_simulation(
        r#"
        module Top(output logic ok);
          byte narrow;
          int wide;
          int unsigned a, b;
          initial begin
            ok = 1;
            for (int i = -128; i < 128; i++) begin
              narrow = i; wide = i;
              a = $urandom(narrow); b = $urandom(wide);
              ok &= a == b;
            end
          end
        endmodule
    "#,
    );
    sim.step().unwrap();
    assert_eq!(sim.get(sim.signal("ok")), 1u8.into());
}

sv_backends! {
    fn urandom_sibling_default_streams_have_independent_state(sim) {
        @setup { let source = r#"
            module Child(input logic clk, output int unsigned value);
              always_ff @(posedge clk) value <= $urandom;
            endmodule
            module Top(input logic clk_a, clk_b, output int unsigned a, b);
              Child left(clk_a, a);
              Child right(clk_b, b);
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("random_siblings.sv"))], "Top").four_state(false);
        let a = sim.signal("a");
        let b = sim.signal("b");
        sim.tick(sim.event("clk_a")).unwrap();
        let first = sim.get(a);
        assert_eq!(sim.get(b), 0u8.into());
        sim.tick(sim.event("clk_b")).unwrap();
        assert_eq!(sim.get(b), first);
        sim.tick(sim.event("clk_a")).unwrap();
        let second = sim.get(a);
        assert_ne!(first, second);
        assert_eq!(sim.get(b), first);
        sim.tick(sim.event("clk_b")).unwrap();
        assert_eq!(sim.get(b), second);
    }

    fn urandom_comb_runs_when_untimed_initials_are_skipped(sim) {
        @setup { let source = URANDOM_SKIPPED_INITIAL_SOURCE; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("random_skipped_initial.sv"))], "Top").four_state(true);
        let value = sim.signal("value");
        let seed = sim.signal("seed");
        assert_ne!(sim.get(value), 0u8.into());
        sim.modify(|io| io.set(seed, 17u8)).unwrap();
        assert_eq!(sim.get(value), sim.get(sim.signal("reference")));
        let saved = sim.checkpoint().unwrap();
        let first = sim.get(value);
        sim.modify(|io| io.set(seed, 23u8)).unwrap();
        assert_ne!(sim.get(value), first);
        sim.restore(&saved).unwrap();
        sim.eval_comb().unwrap();
        assert_eq!(sim.get(value), first);
        assert!(sim.drain_runtime_events().is_empty());
    }

    fn urandom_extends_signed_seed_values(sim) {
        @case "system_functions::urandom_extends_signed_seed_values";
    }

    fn urandom_comb_executes_in_statement_order(sim) {
        @case "system_functions::urandom_comb_executes_in_statement_order";
    }

    fn urandom_comb_uses_only_process_activations(sim) {
        @setup { let source = r#"
            module Top(input logic trigger, unrelated, reseed, output int unsigned value, next_value, replay, expected);
              function automatic int unsigned draw(); return $urandom; endfunction
              always_comb begin
                if (trigger) begin
                  if (reseed) value = $urandom(17);
                  else value = draw();
                  next_value = draw();
                end else begin value = 0; next_value = 0; end
              end
              always_comb begin
                replay = $urandom(17);
                replay = draw();
                expected = draw();
              end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("random_comb.sv"))], "Top").four_state(true);
        let value = sim.signal("value");
        let next_value = sim.signal("next_value");
        let trigger = sim.signal("trigger");
        let reseed = sim.signal("reseed");
        sim.modify(|io| { io.set(trigger, 1u8); io.set(reseed, 1u8); }).unwrap();
        let seeded = sim.get(value);
        let second = sim.get(next_value);
        assert_eq!(second, sim.get(sim.signal("replay")));
        for bit in [1u8, 0, 1] {
            sim.set(sim.signal("unrelated"), bit);
            sim.eval_comb().unwrap();
            assert_eq!(sim.get(value), seeded);
            assert_eq!(sim.get(next_value), second);
        }
        let checkpoint = sim.checkpoint().unwrap();
        let file = sim.save_state().unwrap();
        assert!(file.objects.iter().filter(|object| object.path.contains("urandom_state")).all(|object| object.role == celox_runtime::StateRole::State));
        sim.modify(|io| io.set(reseed, 0u8)).unwrap();
        let third = sim.get(value);
        assert_eq!(third, sim.get(sim.signal("expected")));
        let fourth = sim.get(next_value);
        sim.restore(&checkpoint).unwrap();
        assert_eq!(sim.get(value), seeded);
        assert_eq!(sim.get(next_value), second);
        sim.modify(|io| io.set(reseed, 0u8)).unwrap();
        assert_eq!(sim.get(value), third);
        assert_eq!(sim.get(next_value), fourth);
        sim.load_state(&file).unwrap();
        assert_eq!(sim.get(value), seeded);
        assert_eq!(sim.get(next_value), second);
        sim.modify(|io| io.set(reseed, 0u8)).unwrap();
        assert_eq!(sim.get(value), third);
        assert_eq!(sim.get(next_value), fourth);
        let mut incomplete = file.clone();
        incomplete.objects.retain(|object| !object.path.contains("urandom_state"));
        assert!(sim.load_state(&incomplete).is_err());
    }

    fn urandom_comb_distinguishes_time_zero_and_function_sensitivity(sim) {
        @setup { let source = r#"
            module Top(input logic trigger, global_input, output int unsigned a, b, initial_draw, star_draw);
              function automatic int unsigned draw(input logic unused);
                if (global_input) return $urandom;
                return $urandom;
              endfunction
              always_comb a = draw(trigger);
              always @* b = draw(trigger);
              always_comb initial_draw = $urandom;
              always @* star_draw = $urandom;
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("random_sensitivity.sv"))], "Top").four_state(false);
        let a = sim.signal("a");
        let b = sim.signal("b");
        let first = sim.get(a);
        assert_ne!(first, 0u8.into());
        assert_eq!(sim.get(b), 0u8.into());
        let initial_draw = sim.get(sim.signal("initial_draw"));
        assert_ne!(initial_draw, 0u8.into());
        assert_eq!(sim.get(sim.signal("star_draw")), 0u8.into());
        let global_input = sim.signal("global_input");
        sim.modify(|io| io.set(global_input, 1u8)).unwrap();
        assert_ne!(sim.get(a), first);
        assert_eq!(sim.get(b), 0u8.into());
        let trigger = sim.signal("trigger");
        sim.modify(|io| io.set(trigger, 1u8)).unwrap();
        assert_ne!(sim.get(b), 0u8.into());
        assert_eq!(sim.get(sim.signal("initial_draw")), initial_draw);
        assert_eq!(sim.get(sim.signal("star_draw")), 0u8.into());
    }

    fn urandom_select_keeps_distinct_source_calls(sim) {
        @case "system_functions::urandom_select_keeps_distinct_source_calls";
    }

    fn urandom_comb_tracks_static_bits_dead_branches_and_unknown_changes(sim) {
        @setup { let source = r#"
            module Top(input logic [1:0] trigger, input logic dead_input, output logic echo, output int unsigned value);
              always_comb begin
                echo = trigger[0];
                value = $urandom;
                if (0) $display("%b", dead_input);
              end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("random_bits.sv"))], "Top").four_state(true);
        let trigger = sim.signal("trigger");
        let value = sim.signal("value");
        let dead_input = sim.signal("dead_input");
        sim.modify(|io| { io.set(trigger, 0u8); io.set(dead_input, 0u8); }).unwrap();
        let first = sim.get(value);
        sim.modify(|io| io.set(trigger, 2u8)).unwrap();
        assert_eq!(sim.get(value), first);
        sim.modify(|io| io.set_four_state(trigger, 2u8.into(), 1u8.into())).unwrap();
        let second = sim.get(value);
        assert_ne!(second, first);
        sim.eval_comb().unwrap();
        assert_eq!(sim.get(value), second);
        sim.modify(|io| io.set(dead_input, 1u8)).unwrap();
        assert_ne!(sim.get(value), second);
        assert!(sim.drain_runtime_events().is_empty());
    }

    fn urandom_comb_propagates_all_outputs_and_hierarchical_events(sim) {
        @setup { let source = r#"
            module Child(input logic enabled, input int seed, output int unsigned a, b);
              function automatic int unsigned draw(); return $urandom; endfunction
              always_comb begin
                if (enabled) begin
                  a = $urandom(seed); b = draw(); b[0] = 0;
                  $display("inside=%0d", seed);
                end else begin a = 0; b = 0; end
              end
            endmodule
            module Top(input logic clk, enabled, unrelated, input int data, output int unsigned q, b);
              int unsigned a;
              wire [31:0] adjusted = data + 1;
              Child child(enabled, adjusted, a, b);
              assign q = a ^ b;
              always_comb if (enabled) $display("observed=%0h", b);
              always_ff @(posedge clk) $display("clock");
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("random_events.sv"))], "Top").four_state(true);
        sim.drain_runtime_events();
        let enabled = sim.signal("enabled");
        let data = sim.signal("data");
        sim.modify(|io| { io.set(enabled, 1u8); io.set(data, 7u8); }).unwrap();
        let first = sim.get(sim.signal("q"));
        let messages = sim.drain_runtime_events();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages.contains(&celox::RuntimeEvent::Display { message: b"inside=8".to_vec() }));
        sim.modify(|io| io.set(data, 8u8)).unwrap();
        assert_ne!(sim.get(sim.signal("q")), first);
        let messages = sim.drain_runtime_events();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages.contains(&celox::RuntimeEvent::Display { message: b"inside=9".to_vec() }));
        sim.set(sim.signal("unrelated"), 1u8);
        sim.eval_comb().unwrap();
        assert!(sim.drain_runtime_events().is_empty());
        sim.tick(sim.event("clk")).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![celox::RuntimeEvent::Display { message: b"clock".to_vec() }]);
    }

    fn urandom_comb_notifies_only_observed_bits(sim) {
        @setup { let source = r#"
            module Static(input logic low, input logic [6:0] high, output logic [7:0] q);
              always_comb begin q[0] = low; q[7:1] = high; void'($urandom); end
              always_comb $display("static=%b", q[0]);
            endmodule
            module Dynamic(input logic low, high, input logic [2:0] index, output logic [7:0] q);
              always_comb begin
                q[0] = low; q[7:1] = 0; q[index] = high; void'($urandom);
              end
              always_comb $display("dynamic=%b", q[0]);
            endmodule
            module Array(input logic low, input logic [6:0] high, input logic [2:0] index, output logic [7:0] q);
              logic [7:0] words[0:1];
              always_comb begin
                words[0] = {high, low}; words[1] = 0;
                words[(index == 0) ? 0 : 1] = {7'b0, high[0]}; void'($urandom);
              end
              assign q = words[0];
              always_comb $display("array=%b", words[0][0]);
            endmodule
            module Top(input logic low, input logic [6:0] high, input logic [2:0] index,
                       output logic [7:0] q_static, q_dynamic, q_array);
              Static s(low, high, q_static);
              Dynamic d(low, high[0], index, q_dynamic);
              Array a(low, high, index, q_array);
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("random_observed_bits.sv"))], "Top").four_state(true);
        let low = sim.signal("low");
        let high = sim.signal("high");
        let index = sim.signal("index");
        sim.modify(|io| { io.set(low, 0u8); io.set(high, 0u8); io.set(index, 4u8); }).unwrap();
        sim.drain_runtime_events();
        sim.modify(|io| io.set(high, 1u8)).unwrap();
        let events = sim.drain_runtime_events();
        let array_value = sim.get(sim.signal("q_array"));
        assert!(events.is_empty(), "{events:?}; array={array_value:?}");
        assert_eq!(array_value, 2u8.into());
        sim.modify(|io| io.set(low, 1u8)).unwrap();
        let mut messages: Vec<_> = sim.drain_runtime_events().into_iter().map(|event| match event {
            celox::RuntimeEvent::Display { message } => message,
            event => panic!("unexpected event: {event:?}"),
        }).collect();
        messages.sort();
        assert_eq!(messages, vec![b"array=1".to_vec(), b"dynamic=1".to_vec(), b"static=1".to_vec()]);
        sim.modify(|io| io.set_four_state(high, 1u8.into(), 64u8.into())).unwrap();
        let events = sim.drain_runtime_events();
        assert!(events.is_empty(), "{events:?}");
        sim.modify(|io| { io.set(high, 0u8); io.set(index, 5u8); }).unwrap();
        let events = sim.drain_runtime_events();
        assert!(events.is_empty(), "{events:?}");
        sim.modify(|io| io.set(index, 0u8)).unwrap();
        let mut messages: Vec<_> = sim.drain_runtime_events().into_iter().map(|event| match event {
            celox::RuntimeEvent::Display { message } => message,
            event => panic!("unexpected event: {event:?}"),
        }).collect();
        messages.sort();
        assert_eq!(messages, vec![b"array=0".to_vec(), b"dynamic=0".to_vec()]);
    }

    fn urandom_has_an_unsigned_self_determined_result(sim) {
        @case "system_functions::urandom_has_an_unsigned_self_determined_result";
    }

    fn urandom_respects_short_circuits_and_evaluates_indices_once(sim) {
        @case "system_functions::urandom_respects_short_circuits_and_evaluates_indices_once";
    }

    fn urandom_checkpoint_restores_the_stream(sim) {
        @setup { let source = r#"
            module Top(input logic clk, reseed, output int unsigned value);
                always_ff @(posedge clk) begin
                    if (reseed) value <= $urandom(17);
                    else value <= $urandom;
                end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("random_state.sv"))], "Top").four_state(true);
        let clk = sim.event("clk");
        let reseed = sim.signal("reseed");
        let value = sim.signal("value");
        sim.set(reseed, 1u8);
        sim.tick(clk).unwrap();
        sim.set(reseed, 0u8);
        let checkpoint = sim.checkpoint().unwrap();
        let mut expected = Vec::new();
        for _ in 0..4 {
            sim.tick(clk).unwrap();
            let (value, mask) = sim.get_four_state(value);
            assert_eq!(mask, 0u8.into());
            expected.push(value);
        }
        sim.restore(&checkpoint).unwrap();
        for value_expected in expected {
            sim.tick(clk).unwrap();
            assert_eq!(sim.get(value), value_expected);
        }
    }

    fn urandom_reseeds_and_shares_the_calling_process_stream(sim) {
        @case "system_functions::urandom_reseeds_and_shares_the_calling_process_stream";
    }
    fn urandom_keeps_process_and_instance_streams_independent(sim) {
        @case "system_functions::urandom_keeps_process_and_instance_streams_independent";
    }

    fn display_expressions_propagate_operand_widths(sim) { @case "system_functions::display_expressions_propagate_operand_widths"; }

    fn character_formats_preserve_non_utf8_bytes(sim) { @case "system_functions::character_formats_preserve_non_utf8_bytes"; }
    fn time_format_consumes_its_expression_argument(sim) { @case "system_functions::time_format_consumes_its_expression_argument"; }
    fn string_literals_follow_numeric_format_specifiers(sim) { @case "system_functions::string_literals_follow_numeric_format_specifiers"; }
    // Veryl extension calls emitted unchanged into SV, not SV assert statements.
    fn veryl_assert_continue_ff_captures_arguments_before_reporting(sim) {
        @setup { let source = r#"
            module Top(input logic clk, enabled, ok, output logic effect, output logic reached);
              function automatic int message_value(input logic value, output logic written);
                $display("inside=%0d", value);
                written = !value;
                return -2;
              endfunction
              always_ff @(posedge clk) begin
                reached = 0;
                effect = ok;
                if (enabled) begin
                  $assert_continue(effect, "before=%0d value=%0d after=%0d",
                                   effect, message_value(effect, effect), effect);
                  reached = 1;
                end
              end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("veryl_assert.sv"))], "Top")
            .four_state(true);
        let enabled = sim.signal("enabled");
        let ok = sim.signal("ok");
        let clk = sim.event("clk");
        sim.set(enabled, 0u8);
        sim.set(ok, 0u8);
        sim.tick(clk).unwrap();
        assert!(sim.drain_runtime_events().is_empty());
        assert_eq!(sim.get(sim.signal("reached")), 0u8.into());
        sim.set(enabled, 1u8);
        sim.set(ok, 1u8);
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sim.signal("effect")), 0u8.into());
        assert_eq!(sim.get(sim.signal("reached")), 1u8.into());
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"inside=1".to_vec() },
        ]);
        sim.set(ok, 0u8);
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sim.signal("effect")), 1u8.into());
        assert_eq!(sim.get(sim.signal("reached")), 1u8.into());
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"inside=0".to_vec() },
            celox::RuntimeEvent::AssertContinue { message: b"before=0 value=-2 after=1".to_vec() },
        ]);
    }

    fn veryl_assert_comb_evaluates_success_message_effects(sim) {
        @setup { let source = r#"
            module Top(input logic enabled, ok, output logic effect);
              function automatic int message_value(input logic value, output logic written);
                $display("inside=%0d", value);
                written = !value;
                return -2;
              endfunction
              always_comb begin
                effect = ok;
                if (enabled)
                  $assert_continue(effect, "before=%0d value=%0d after=%0d",
                                   effect, message_value(effect, effect), effect);
              end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("veryl_assert_comb.sv"))], "Top")
            .four_state(true);
        sim.drain_runtime_events();
        let enabled = sim.signal("enabled");
        let ok = sim.signal("ok");
        sim.modify(|io| { io.set(enabled, 1u8); io.set(ok, 1u8); }).unwrap();
        assert_eq!(sim.get(sim.signal("effect")), 0u8.into());
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"inside=1".to_vec() },
        ]);
        sim.modify(|io| io.set(ok, 0u8)).unwrap();
        assert_eq!(sim.get(sim.signal("effect")), 1u8.into());
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"inside=0".to_vec() },
            celox::RuntimeEvent::AssertContinue { message: b"before=0 value=-2 after=1".to_vec() },
        ]);
        sim.modify(|io| io.set(enabled, 0u8)).unwrap();
        assert_eq!(sim.get(sim.signal("effect")), 0u8.into());
        assert!(sim.drain_runtime_events().is_empty());
    }

    fn veryl_assert_fatal_ff_evaluates_messages_and_stops_only_on_failure(sim) {
        @setup { let source = r#"
            module Top(input logic clk, ok, output logic reached);
              function automatic int message_value();
                $display("inside");
                return -2;
              endfunction
              always_ff @(posedge clk) begin
                reached = 0;
                $assert(ok, "value=%0d", message_value());
                reached = 1;
              end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("veryl_assert_fatal.sv"))], "Top");
        let ok = sim.signal("ok");
        let clk = sim.event("clk");
        sim.set(ok, 1u8);
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sim.signal("reached")), 1u8.into());
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"inside".to_vec() },
        ]);
        sim.set(ok, 0u8);
        assert!(sim.tick(clk).is_err());
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"inside".to_vec() },
            celox::RuntimeEvent::AssertFatal { message: b"value=-2".to_vec() },
        ]);
    }

    fn veryl_assert_comb_runtime_loop_keeps_eager_argument_effects(sim) {
        @setup { let source = r#"
            module Top(input logic ok, input logic [2:0] count, output int q);
              function automatic int message_value(input int index);
                $display("inside=%0d", index);
                return index + 1;
              endfunction
              always_comb begin
                q = 0;
                for (int i = 0; i < count; ++i) begin
                  $assert_continue(ok, "value=%0d", message_value(i));
                  q += 1;
                end
              end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("veryl_assert_loop.sv"))], "Top");
        sim.drain_runtime_events();
        let ok = sim.signal("ok");
        let count = sim.signal("count");
        sim.modify(|io| { io.set(ok, 1u8); io.set(count, 2u8); }).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 2u32.into());
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"inside=0".to_vec() },
            celox::RuntimeEvent::Display { message: b"inside=1".to_vec() },
        ]);
        sim.modify(|io| io.set(ok, 0u8)).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"inside=0".to_vec() },
            celox::RuntimeEvent::AssertContinue { message: b"value=1".to_vec() },
            celox::RuntimeEvent::Display { message: b"inside=1".to_vec() },
            celox::RuntimeEvent::AssertContinue { message: b"value=2".to_vec() },
        ]);
    }

    fn veryl_assert_constant_conditions_preserve_message_effects(sim) {
        @setup { let source = r#"
            module Top(input logic clk);
              function automatic int message_value();
                $display("inside");
                return -2;
              endfunction
              always_ff @(posedge clk) begin
                $assert(1'b1, "value=%0d", message_value());
                $assert_continue(1'b0, message_value());
                $assert_continue(1'b0);
                assert (1'b1) else $error("value=%0d", message_value());
              end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("veryl_assert_constant.sv"))], "Top");
        sim.tick(sim.event("clk")).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"inside".to_vec() },
            celox::RuntimeEvent::Display { message: b"inside".to_vec() },
            celox::RuntimeEvent::AssertContinue { message: b"assertion failed".to_vec() },
            celox::RuntimeEvent::AssertContinue { message: b"assertion failed".to_vec() },
        ]);
    }

    fn unpacked_dimensions_publishes_dependent_array_parameters(sim) { @case "system_functions::unpacked_dimensions_publishes_dependent_array_parameters"; }
    fn unpacked_dimensions_accepts_function_result_arguments(sim) { @case "system_functions::unpacked_dimensions_accepts_function_result_arguments"; }
    fn unpacked_dimensions_resolves_sibling_declarators(sim) { @case "system_functions::unpacked_dimensions_resolves_sibling_declarators"; }
    fn unpacked_dimensions_selects_integral_atoms(sim) { @case "system_functions::unpacked_dimensions_selects_integral_atoms"; }
    fn unpacked_dimensions_publishes_array_parameters_before_declarations(sim) { @case "system_functions::unpacked_dimensions_publishes_array_parameters_before_declarations"; }
    fn unpacked_dimensions_resolves_preceding_module_declarations(sim) { @case "system_functions::unpacked_dimensions_resolves_preceding_module_declarations"; }
    fn unpacked_dimensions_prefers_values_over_imported_types(sim) { @case "system_functions::unpacked_dimensions_prefers_values_over_imported_types"; }
    fn unpacked_dimensions_checks_unevaluated_indices(sim) { @case "system_functions::unpacked_dimensions_checks_unevaluated_indices"; }

    fn unpacked_dimensions_restores_shadowed_array_rank(sim) { @case "system_functions::unpacked_dimensions_restores_shadowed_array_rank"; }
    fn unpacked_dimensions_accepts_explicit_default_omissions(sim) { @case "system_functions::unpacked_dimensions_accepts_explicit_default_omissions"; }
    fn unpacked_dimensions_validates_writable_function_arguments(sim) { @case "system_functions::unpacked_dimensions_validates_writable_function_arguments"; }

    fn unpacked_dimensions_accepts_defaulted_function_arguments(sim) { @case "system_functions::unpacked_dimensions_accepts_defaulted_function_arguments"; }

    fn unpacked_dimensions_types_function_pattern_arguments(sim) { @case "system_functions::unpacked_dimensions_types_function_pattern_arguments"; }
    fn unpacked_dimensions_resolves_preceding_block_declarations(sim) { @case "system_functions::unpacked_dimensions_resolves_preceding_block_declarations"; }

    fn unpacked_dimensions_resolves_procedural_scopes(sim) {
        @case "system_functions::unpacked_dimensions_resolves_procedural_scopes";
    }
    fn unpacked_dimensions_resolves_package_array_constants(sim) {
        @case "system_functions::unpacked_dimensions_resolves_package_array_constants";
    }

    fn unpacked_dimensions_counts_declared_arrays(sim) {
        @case "system_functions::unpacked_dimensions_counts_declared_arrays";
    }

    fn unpacked_dimensions_preserves_selection_and_return_types(sim) {
        @case "system_functions::unpacked_dimensions_preserves_selection_and_return_types";
    }

    fn unpacked_dimensions_resolves_types_and_constant_scopes(sim) {
        @case "system_functions::unpacked_dimensions_resolves_types_and_constant_scopes";
    }

    fn unpacked_dimensions_is_independent_of_unknown_values(sim) {
        @case "system_functions::unpacked_dimensions_is_independent_of_unknown_values";
    }

    fn printtimescale_displays_unit_and_global_precision(sim) { @case "system_functions::printtimescale_displays_unit_and_global_precision"; }
    fn printtimescale_preserves_package_subroutine_scope(sim) { @case "system_functions::printtimescale_preserves_package_subroutine_scope"; }

    fn printtimescale_qualifies_current_and_relative_module_instances(sim) {
        @setup { let source = r#"
            module Child(input logic clk, input logic enabled);
              timeunit 100ns / 10ps;
              always_ff @(posedge clk) if (enabled) $printtimescale;
            endmodule
            module Top(input logic clk, input logic enabled);
              timeunit 1ns / 1ps;
              Child first(clk, enabled);
              Child second(clk, enabled);
              always_ff @(posedge clk) if (enabled) begin
                $printtimescale();
                $printtimescale(first);
                $printtimescale($root.Top.second);
              end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("printtimescale.sv"))], "Top");
        sim.set(sim.signal("enabled"), 1u8);
        sim.tick(sim.event("clk")).unwrap();
        let mut messages = sim.drain_runtime_events().into_iter().map(|event| match event {
            celox::RuntimeEvent::Display { message } => String::from_utf8(message).unwrap(),
            event => panic!("unexpected event: {event:?}"),
        }).collect::<Vec<_>>();
        messages.sort();
        assert_eq!(messages, [
            "Time scale of (Top) is 1ns / 1ps",
            "Time scale of (Top.first) is 100ns / 10ps",
            "Time scale of (Top.first) is 100ns / 10ps",
            "Time scale of (Top.second) is 100ns / 10ps",
            "Time scale of (Top.second) is 100ns / 10ps",
        ]);
    }

    fn printtimescale_resolves_interfaces_before_flattening(sim) {
        @setup { let source = r#"
            interface I;
              timeunit 100ns / 10ps;
              function automatic int show(input int data); $printtimescale; return data; endfunction
            endinterface
            module Child; timeunit 1us / 1ns; I nested(); endmodule
            module Top(input logic clk, input int data, output int q);
              timeunit 1ns / 1ps;
              I info(); Child child();
              always_ff @(posedge clk) begin
                $printtimescale(info);
                $printtimescale(child.nested);
                q <= info.show(data);
              end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("printtimescale.sv"))], "Top");
        sim.set(sim.signal("data"), 7u32);
        sim.tick(sim.event("clk")).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 7u32.into());
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"Time scale of (Top.info) is 100ns / 10ps".to_vec() },
            celox::RuntimeEvent::Display { message: b"Time scale of (Top.child.nested) is 100ns / 10ps".to_vec() },
            celox::RuntimeEvent::Display { message: b"Time scale of (Top.info) is 100ns / 10ps".to_vec() },
        ]);
    }

    fn printtimescale_keeps_generic_interface_bindings_separate(sim) {
        @setup { let source = r#"
            interface I;
              timeunit 1ns / 1ps;
              function automatic int show(input int v); $printtimescale; return v; endfunction
            endinterface
            interface J;
              timeunit 100ns / 10ps;
              function automatic int show(input int v); $printtimescale; return v; endfunction
            endinterface
            module Reader(interface info, input logic clk, input int data, output int q);
              timeunit 1us / 1ns;
              always_ff @(posedge clk) begin $printtimescale(info); q <= info.show(data); end
            endmodule
            module Top(input logic clk, input int data, output int i_q, j_q);
              timeunit 1us / 1ns;
              I i(); J j(); Reader r_i(i, clk, data, i_q); Reader r_j(j, clk, data, j_q);
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("interface_prints.sv"))], "Top");
        sim.set(sim.signal("data"), 7u32);
        sim.tick(sim.event("clk")).unwrap();
        assert_eq!(sim.get(sim.signal("i_q")), 7u32.into());
        assert_eq!(sim.get(sim.signal("j_q")), 7u32.into());
        let mut messages = sim.drain_runtime_events().into_iter().map(|event| match event {
            celox::RuntimeEvent::Display { message } => String::from_utf8(message).unwrap(),
            event => panic!("unexpected event: {event:?}"),
        }).collect::<Vec<_>>();
        messages.sort();
        assert_eq!(messages, [
            "Time scale of (Top.r_i.info) is 1ns / 1ps",
            "Time scale of (Top.r_i.info) is 1ns / 1ps",
            "Time scale of (Top.r_j.info) is 100ns / 10ps",
            "Time scale of (Top.r_j.info) is 100ns / 10ps",
        ]);
    }
    fn timescale_queries_resolve_hierarchical_interface_instances(sim) { @case "system_functions::timescale_queries_resolve_hierarchical_interface_instances"; }
    fn timescale_queries_resolve_interface_instances_and_ports(sim) { @case "system_functions::timescale_queries_resolve_interface_instances_and_ports"; }
    fn timescale_queries_accept_leading_zero_magnitudes(sim) { @case "system_functions::timescale_queries_accept_leading_zero_magnitudes"; }
    fn timescale_queries_survive_type_parameter_specialization(sim) { @case "system_functions::timescale_queries_survive_type_parameter_specialization"; }
    fn timescale_queries_resolve_packages_with_instantiated_namesakes(sim) { @case "system_functions::timescale_queries_resolve_packages_with_instantiated_namesakes"; }
    fn timescale_queries_preserve_interface_scopes_when_flattened(sim) { @case "system_functions::timescale_queries_preserve_interface_scopes_when_flattened"; }
    fn timescale_queries_accept_directives_before_unit_declarations(sim) { @case "system_functions::timescale_queries_accept_directives_before_unit_declarations"; }

    fn timescale_queries_accept_separate_initial_declarations(sim) { @case "system_functions::timescale_queries_accept_separate_initial_declarations"; }
    fn timescale_queries_keep_module_and_package_names_independent(sim) { @case "system_functions::timescale_queries_keep_module_and_package_names_independent"; }

    fn timescale_queries_keep_selected_root_despite_unreachable_instantiations(sim) {
        @setup {
            let source = r#"
                package P;
                    timeunit 100ps / 10fs;
                    function automatic int root_precision(); return $timeprecision($root.Top); endfunction
                endpackage
                interface I;
                    timeunit 100ns / 10ps;
                    function automatic int root_unit(); return $timeunit($root.Top); endfunction
                endinterface
                module Child(output logic ok);
                    timeunit 1us / 1ns;
                    assign ok = ($timeunit($root.Top) == -9) && ($timeprecision($root.Top) == -12)
                        && ($timeunit(Top) == -9) && ($timeprecision(Top) == -12)
                        && (P::root_precision() == -12);
                endmodule
                module Top(output logic ok);
                    timeunit 1ns / 1ps;
                    logic child_ok;
                    I info();
                    Child child(.ok(child_ok));
                    assign ok = child_ok && (info.root_unit() == -9);
                endmodule
                module Harness(); timeunit 1ms / 1us; Top unused_top(); endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("selected_root.sv"))], "Top");
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), 1u32.into());
    }

    fn timescale_queries_resolve_cross_file_and_root_instance_paths(sim) {
        @setup {
            let source = r#"
                timeunit 10ns / 1ps;
                module Top(output logic ok);
                    timeunit 1ns / 1ps;
                    Child child();
                    assign ok = ($timeunit() == -9) && ($timeprecision() == -12)
                        && ($timeunit(child) == -6) && ($timeprecision(child) == -7)
                        && ($timeunit(Top.child) == -6) && ($timeprecision($root.Top.child) == -7)
                        && ($timeunit($unit) == -8) && ($timeprecision($root) == -15);
                endmodule
            "#;
            let child = "module Child(); timeunit 1us / 100ns; endmodule";
            let unused = "module Unused(); timeunit 1fs / 1fs; endmodule";
        }
        @build Simulator::from_sv_sources(vec![
            (source, Path::new("top.sv")),
            (child, Path::new("child.sv")),
            (unused, Path::new("unused.sv")),
        ], "Top");
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), 1u32.into());
    }

    fn timescale_queries_resolve_declarations_and_return_types(sim) { @case "system_functions::timescale_queries_resolve_declarations_and_return_types"; }
    fn timescale_queries_resolve_directives_and_hierarchy(sim) { @case "system_functions::timescale_queries_resolve_directives_and_hierarchy"; }
    fn timescale_queries_distinguish_unit_and_global_precision(sim) { @case "system_functions::timescale_queries_distinguish_unit_and_global_precision"; }
    fn timescale_queries_follow_partial_declarations_and_resetall(sim) { @case "system_functions::timescale_queries_follow_partial_declarations_and_resetall"; }
    fn timescale_queries_preserve_procedural_and_package_scopes(sim) { @case "system_functions::timescale_queries_preserve_procedural_and_package_scopes"; }
    fn timescale_queries_are_constant_in_unqualified_calls(sim) { @case "system_functions::timescale_queries_are_constant_in_unqualified_calls"; }

    fn isunbounded_generate_guards_preserve_short_circuit_bounds(sim) { @case "system_functions::isunbounded_generate_guards_preserve_short_circuit_bounds"; }
    fn isunbounded_restores_shadowed_function_arguments(sim) { @case "system_functions::isunbounded_restores_shadowed_function_arguments"; }
    fn isunbounded_binds_procedural_aliases_in_declaration_order(sim) { @case "system_functions::isunbounded_binds_procedural_aliases_in_declaration_order"; }
    fn isunbounded_restores_procedural_parameter_bindings(sim) { @case "system_functions::isunbounded_restores_procedural_parameter_bindings"; }
    fn isunbounded_restores_procedural_parameter_type_metadata(sim) { @case "system_functions::isunbounded_restores_procedural_parameter_type_metadata"; }
    fn isunbounded_preserves_lexical_scopes_and_short_circuits(sim) { @case "system_functions::isunbounded_preserves_lexical_scopes_and_short_circuits"; }
    fn isunbounded_reports_bounded_parameters(sim) { @case "system_functions::isunbounded_reports_bounded_parameters"; }
    fn isunbounded_reports_bounded_array_parameters(sim) { @case "system_functions::isunbounded_reports_bounded_array_parameters"; }
    fn isunbounded_preserves_parameter_values_and_result_type(sim) { @case "system_functions::isunbounded_preserves_parameter_values_and_result_type"; }
    fn isunbounded_handles_overrides_and_forwarding(sim) { @case "system_functions::isunbounded_handles_overrides_and_forwarding"; }
    fn isunbounded_resolves_package_and_local_parameters(sim) { @case "system_functions::isunbounded_resolves_package_and_local_parameters"; }
    fn dimensions_resolves_preceding_block_declarations(sim) { @case "system_functions::dimensions_resolves_preceding_block_declarations"; }
    fn dimensions_types_function_pattern_arguments(sim) { @case "system_functions::dimensions_types_function_pattern_arguments"; }
    fn dimensions_signing_conversions_are_vectors(sim) { @case "system_functions::dimensions_signing_conversions_are_vectors"; }
    fn dimensions_resolves_procedural_parameters_and_indices(sim) { @case "system_functions::dimensions_resolves_procedural_parameters_and_indices"; }

    fn dimensions_resolves_procedural_scopes(sim) {
        @case "system_functions::dimensions_resolves_procedural_scopes";
    }
    fn dimensions_resolves_package_array_constants(sim) {
        @case "system_functions::dimensions_resolves_package_array_constants";
    }
    fn dimensions_preserves_one_bit_vectors(sim) {
        @case "system_functions::dimensions_preserves_one_bit_vectors";
    }
    fn dimensions_resolves_imported_function_returns(sim) {
        @case "system_functions::dimensions_resolves_imported_function_returns";
    }

    fn dimensions_of_nonarray_handle_types(sim) {
        @case "system_functions::dimensions_of_nonarray_handle_types";
    }

    fn dimensions_is_independent_of_unknown_values(sim) {
        @case "system_functions::dimensions_is_independent_of_unknown_values";
    }

    fn dimensions_counts_declared_arrays(sim) {
        @case "system_functions::dimensions_counts_declared_arrays";
    }

    fn dimensions_preserves_selected_and_expression_types(sim) {
        @case "system_functions::dimensions_preserves_selected_and_expression_types";
    }

    fn dimensions_resolves_types_and_constant_scopes(sim) {
        @case "system_functions::dimensions_resolves_types_and_constant_scopes";
    }

    fn array_query_evaluates_dimension_function_once(sim) {
        @setup {
            let source = r#"
                module Top(input int d, output int y);
                    logic [7:4] mem [2:5];
                    function automatic int dimension();
                        $display("dimension");
                        return d;
                    endfunction
                    assign y = $left(mem, dimension());
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("array_query.sv"))], "Top");
        let d = sim.signal("d");
        let y = sim.signal("y");
        for (dimension, expected) in [(1u32, 2u32), (2, 7), (3, 0), (0x0800_0001, 0)] {
            sim.set(d, dimension);
            assert_eq!(sim.get(y), expected.into());
            assert_eq!(sim.drain_runtime_events(), vec![celox::RuntimeEvent::Display {
                message: b"dimension".to_vec(),
            }]);
        }
    }
    fn readmem_descending_ranges_and_absolute_addresses(sim) {
        @setup {
            let dir = tempfile::tempdir().unwrap();
            let hex = dir.path().join("data.hex");
            let bin = dir.path().join("data.bin");
            std::fs::write(&hex, "aa @2 bb cc").unwrap();
            std::fs::write(&bin, "1010 1011 1100").unwrap();
            let source = format!(r#"
                module Top(input logic clk, output logic [31:0] initial_words, registered);
                    logic [7:0] a [3:0];
                    logic [7:0] b [1:4];
                    initial $readmemh("{}", a, 3, 1);
                    assign initial_words = {{a[3], a[2], a[1], 8'h00}};
                    always_ff @(posedge clk) begin
                        $readmemb("{}", b, 4, 2);
                        registered <= {{b[4], b[3], b[2], 8'h00}};
                    end
                endmodule
            "#, hex.display(), bin.display());
        }
        @build Simulator::from_sv_sources(vec![(&source, Path::new("readmem.sv"))], "Top");
        let initial_words = sim.signal("initial_words");
        let registered = sim.signal("registered");
        let clk = sim.event("clk");
        assert_eq!(sim.get(initial_words), 0xaabbcc00u32.into());
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(registered), 0x0a0b0c00u32.into());
    }

    fn readmem_count_mismatch_preserves_unwritten_words(sim) {
        @setup {
            let dir = tempfile::tempdir().unwrap();
            let short = dir.path().join("short.hex");
            let long = dir.path().join("long.hex");
            std::fs::write(&short, "aa").unwrap();
            std::fs::write(&long, "bb cc dd").unwrap();
            let source = format!(r#"
                module Top(output logic [31:0] y, short_y);
                    logic [7:0] mem [-1:2];
                    logic [7:0] short_mem [0:3];
                    always_comb begin
                        mem[-1] = 8'h11; mem[0] = 8'h22;
                        mem[1] = 8'h33; mem[2] = 8'h44;
                        short_mem[0] = 8'h11; short_mem[1] = 8'h22;
                        short_mem[2] = 8'h33; short_mem[3] = 8'h44;
                        $readmemh("{}", short_mem, 3, 2);
                        $readmemh("{}", mem, -1, 0);
                        y = {{mem[-1], mem[0], mem[1], mem[2]}};
                        short_y = {{short_mem[0], short_mem[1], short_mem[2], short_mem[3]}};
                    end
                endmodule
            "#, short.display(), long.display());
        }
        @build Simulator::from_sv_sources(vec![(&source, Path::new("readmem.sv"))], "Top");
        let y = sim.signal("y");
        let short_y = sim.signal("short_y");
        assert_eq!(sim.get(y), 0xbbcc3344u32.into());
        assert_eq!(sim.get(short_y), 0x112233aau32.into());
    }
    fn signed_casts_in_parameters(sim) { @case "system_functions::signed_casts_in_parameters"; }
    fn array_queries_preserve_bounds_and_dimensions(sim) { @case "system_functions::array_queries_preserve_bounds_and_dimensions"; }
    fn array_queries_handle_runtime_dimensions(sim) { @case "system_functions::array_queries_handle_runtime_dimensions"; }
    fn immediate_cover_handles_unknown_conditions(sim) { @case "system_functions::immediate_cover_handles_unknown_conditions"; }
    fn immediate_cover_executes_pass_statement(sim) { @case "system_functions::immediate_cover_executes_pass_statement"; }

    fn drain_runtime_events_returns_always_comb_fatal(sim) {
        @setup {
            let source = r#"
                module Top(input logic fail, output logic y);
                    always_comb begin
                        y = fail;
                        $display("before");
                        if (fail) $fatal(1, "boom %0d", fail);
                        $display("after");
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("fatal.sv"))], "Top");

        let fail = sim.signal("fail");
        sim.drain_runtime_events();
        sim.set(fail, 1u8);
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"before".to_vec() },
            celox::RuntimeEvent::AssertFatal { message: b"boom 1".to_vec() },
        ]);
        assert!(sim.drain_runtime_events().is_empty());
    }

    fn display_field_flags(sim) { @case "system_functions::display_field_flags"; }

    fn display_arguments_are_sized_as_ieee_specifies(sim) {
        @case "system_functions::display_arguments_are_sized_as_ieee_specifies";
    }

    fn display_field_widths_expand_to_the_value(sim) {
        @case "system_functions::display_field_widths_expand_to_the_value";
    }

    fn display_unknown_bits_as_ieee_specifies(sim) {
        @case "system_functions::display_unknown_bits_as_ieee_specifies";
    }

    fn display_tasks_default_to_their_radix(sim) {
        @case "system_functions::display_tasks_default_to_their_radix";
    }

    fn countbits_in_parameter_specializations_and_generate_scopes(sim) {
        @case "system_functions::countbits_in_parameter_specializations_and_generate_scopes";
    }

    fn countbits_preserves_argument_and_return_types(sim) {
        @case "system_functions::countbits_preserves_argument_and_return_types";
    }
    fn countbits_in_constant_expressions(sim) {
        @case "system_functions::countbits_in_constant_expressions";
    }
    fn countbits_matches_four_states_and_variable_controls(sim) {
        @case "system_functions::countbits_matches_four_states_and_variable_controls";
    }

    fn countones_preserves_argument_and_return_types(sim) {
        @case "system_functions::countones_preserves_argument_and_return_types";
    }

    fn countones_ignores_unknown_bits_in_comb_and_ff(sim) {
        @case "system_functions::countones_ignores_unknown_bits_in_comb_and_ff";
    }

    fn countones_in_constant_expressions(sim) {
        @case "system_functions::countones_in_constant_expressions";
    }

    fn bit_vector_predicates_preserve_argument_and_return_types(sim) {
        @case "system_functions::bit_vector_predicates_preserve_argument_and_return_types";
    }

    fn bit_vector_predicates_handle_unknown_bits_in_comb_ff_and_ports(sim) {
        @case "system_functions::bit_vector_predicates_handle_unknown_bits_in_comb_ff_and_ports";
    }

    fn bit_vector_predicates_in_constant_expressions(sim) {
        @case "system_functions::bit_vector_predicates_in_constant_expressions";
    }
}

#[test]
fn rejects_bit_vector_functions_with_missing_or_extra_arguments() {
    for name in [
        "$countones",
        "$onehot",
        "$onehot0",
        "$isunknown",
        "$dimensions",
        "$unpacked_dimensions",
    ] {
        for args in ["", "a, a", ", a", "a,"] {
            let call = format!("{name}({args})");
            let source =
                format!("module Top(input logic a, output int y); assign y = {call}; endmodule");
            let result = Simulator::from_sv_sources(
                vec![(&source, Path::new("invalid_countones.sv"))],
                "Top",
            )
            .build_cranelift();
            assert!(result.is_err(), "invalid call should be rejected: {call}");
        }
    }
}

fn build_error(source: &str) -> String {
    match Simulator::from_sv_sources(vec![(source, Path::new("system_functions.sv"))], "Top")
        .build_cranelift()
    {
        Ok(_) => panic!("design unexpectedly compiled:\n{source}"),
        // The rendered diagnostic wraps long messages behind `│` gutters.
        Err(error) => error
            .to_string()
            .split_whitespace()
            .filter(|word| *word != "│")
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[test]
fn printtimescale_rejects_invalid_operands_and_value_contexts() {
    for item in [
        "$printtimescale(missing);",
        "$printtimescale($root.P);",
        "$printtimescale(1);",
        "$printtimescale(,);",
        "$printtimescale(Top, Top);",
        "void'($printtimescale());",
        "y = $bits($printtimescale());",
        "y = $printtimescale();",
    ] {
        let source = format!(
            "package P; timeunit 1ns / 1ps; endpackage module Top(output int y); timeunit 1ns / 1ps; always_comb begin y = 0; {item} end endmodule"
        );
        build_error(&source);
    }
    build_error(
        "module Top(output int y); localparam UNUSED=$bits($printtimescale()); assign y=0; endmodule",
    );
    let error = build_error(
        "interface I; function automatic int show(); $printtimescale; return 0; endfunction initial $display(\"%0d\", show()); endinterface module Top(output logic y); I info[2](); assign y=0; endmodule",
    );
    assert!(
        error.contains("uses its instance scope in `$printtimescale`"),
        "{error}"
    );
}

#[test]
fn printtimescale_runs_in_initial_blocks_with_all_time_magnitudes() {
    for (declaration, expected) in [
        ("timeunit 100s / 10s;", "100s / 10s"),
        ("timeunit 1ms / 100us;", "1ms / 100us"),
        ("timeunit 10ns / 1ps;", "10ns / 1ps"),
        ("timeunit 100ps / 10fs;", "100ps / 10fs"),
        ("", "1ns / 1ps"),
    ] {
        let source = format!(
            "module Top(output logic y); {declaration} initial $printtimescale; assign y=0; endmodule"
        );
        let mut sim = initial_simulation(&source);
        sim.step().unwrap();
        assert_eq!(
            sim.drain_runtime_events(),
            vec![celox::RuntimeEvent::Display {
                message: format!("Time scale of (Top) is {expected}").into_bytes()
            }]
        );
    }
}

#[test]
fn printtimescale_initial_instances_and_interface_arrays_keep_their_names() {
    let source = r#"
        interface I;
          timeunit 100ns / 10ps;
          initial begin $printtimescale; $printtimescale($root); end
        endinterface
        module Child; timeunit 1us / 1ns; initial $printtimescale(); endmodule
        module Top(output logic y);
          timeunit 1ns / 1ps;
          I info[2:1](); Child children[2:1]();
          assign y=0;
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    sim.step().unwrap();
    let mut messages = sim
        .drain_runtime_events()
        .into_iter()
        .map(|event| match event {
            celox::RuntimeEvent::Display { message } => String::from_utf8(message).unwrap(),
            event => panic!("unexpected event: {event:?}"),
        })
        .collect::<Vec<_>>();
    messages.sort();
    assert_eq!(
        messages,
        [
            "Time scale of ($root) is 1ps / 1ps",
            "Time scale of ($root) is 1ps / 1ps",
            "Time scale of (Top.children[1]) is 1us / 1ns",
            "Time scale of (Top.children[2]) is 1us / 1ns",
            "Time scale of (Top.info[1]) is 100ns / 10ps",
            "Time scale of (Top.info[2]) is 100ns / 10ps",
        ]
    );
}

#[test]
fn printtimescale_interface_arrays_preserve_integer_boundary_indices() {
    for (range, indices) in [
        ("-2147483647:-2147483648", vec![i32::MIN, i32::MIN + 1]),
        ("2147483646:2147483647", vec![i32::MAX - 1, i32::MAX]),
        ("-2147483648:-2147483648", vec![i32::MIN]),
        ("2147483647:2147483647", vec![i32::MAX]),
    ] {
        let source = format!(
            "interface I; timeunit 100ns / 10ps; initial $printtimescale; endinterface module Top(output logic y); timeunit 1ns / 1ps; I items[{range}](); assign y=0; endmodule"
        );
        let mut sim = initial_simulation(&source);
        sim.step().unwrap();
        let mut messages = sim
            .drain_runtime_events()
            .into_iter()
            .map(|event| match event {
                celox::RuntimeEvent::Display { message } => String::from_utf8(message).unwrap(),
                event => panic!("unexpected event: {event:?}"),
            })
            .collect::<Vec<_>>();
        messages.sort();
        let mut expected = indices
            .into_iter()
            .map(|index| format!("Time scale of (Top.items[{index}]) is 100ns / 10ps"))
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(messages, expected, "range [{range}]");
    }
}

#[test]
fn readmem_diagnostics_identify_task_and_requested_range() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("invalid.mem");
    for name in ["$readmemh", "$readmemb"] {
        for (content, expected) in [
            ("@4", "requested range [3:2]"),
            ("g", "invalid data token g"),
        ] {
            std::fs::write(&file, content).unwrap();
            let source = format!(
                r#"module Top(output logic [7:0] y);
                logic [7:0] mem [0:3]; initial {name}("{}", mem, 3, 2);
                assign y = mem[2]; endmodule"#,
                file.display()
            );
            let error = build_error(&source);
            assert!(error.contains(name), "{error}");
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("destination depth"), "{error}");
        }
    }
}

#[test]
fn rejects_stop_and_deferred_cover_instead_of_discarding_their_semantics() {
    for body in [
        "initial $stop;",
        "always_comb begin $stop(0); y = 0; end",
        "always_ff @(posedge clk) begin $stop; y <= 0; end",
        "function automatic int f(); $stop; return 1; endfunction initial y = f();",
    ] {
        let source = format!("module Top(input logic clk, output logic y); {body} endmodule");
        assert!(build_error(&source).contains("system task `$stop`"));
    }
    for timing in ["#0", "final"] {
        let source =
            format!("module Top(output logic y); initial cover {timing} (1) y = 1; endmodule");
        assert!(build_error(&source).contains("deferred immediate cover"));
    }
}

#[test]
fn rejects_unknown_system_functions_in_every_context() {
    let cases = [
        (
            "always_comb statement",
            "module Top(input logic a, output logic y);
                always_comb begin $foo_bar(a); y = a; end
            endmodule",
        ),
        (
            "always_ff statement",
            "module Top(input logic clk, a, output logic q);
                always_ff @(posedge clk) begin $foo_bar(a); q <= a; end
            endmodule",
        ),
        (
            "initial statement",
            "module Top(output logic y);
                initial $foo_bar(1);
                assign y = 1'b0;
            endmodule",
        ),
        (
            "continuous assignment",
            "module Top(input logic a, output logic y); assign y = $foo_bar(a); endmodule",
        ),
        (
            "unused localparam",
            "module Top(output logic y);
                localparam int P = $foo_bar(5);
                assign y = 1'b0;
            endmodule",
        ),
        (
            "constant function",
            "module Top(output int y);
                function automatic int f(input int x);
                    $foo_bar(x);
                    return x + 1;
                endfunction
                localparam int P = f(5);
                assign y = P;
            endmodule",
        ),
        (
            "generate condition",
            "module Top(output logic y);
                if ($foo_bar(1)) begin : g assign y = 1'b1; end
                else begin : h assign y = 1'b0; end
            endmodule",
        ),
        (
            "packed range",
            "module Top(output logic [$foo_bar(3):0] y); assign y = '0; endmodule",
        ),
    ];
    for (context, source) in cases {
        let error = build_error(source);
        assert!(
            error.contains("unknown system task or function `$foo_bar`"),
            "{context}: {error}"
        );
    }
}

#[test]
fn reports_where_a_known_system_function_is_not_supported() {
    let cases = [
        (
            "system function `$time` in an expression",
            "module Top(output logic [63:0] y); assign y = $time; endmodule",
        ),
        (
            "system function `$sqrt` in an expression",
            "module Top(output logic y);
                localparam int P = $sqrt(4);
                assign y = 1'b0;
            endmodule",
        ),
        (
            "system function `$fopen`",
            r#"module Top(input logic a, output logic y);
                always_comb begin $fopen("x.txt"); y = a; end
            endmodule"#,
        ),
        (
            "system task `$assert` inside an initial block",
            "module Top(output logic y);
                initial $assert(1'b1);
                assign y = 1'b0;
            endmodule",
        ),
        (
            "system task `$stop`",
            "module Top(output logic y); initial $stop; assign y = 0; endmodule",
        ),
    ];
    for (expected, source) in cases {
        let error = build_error(source);
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn timescale_queries_reject_packages_beneath_root() {
    for function in ["$timeunit", "$timeprecision"] {
        for item in [
            format!("assign y = {function}($root.P);"),
            format!("always_comb begin {function}($root.P); y = 0; end"),
            format!("always_comb begin void'({function}($root.P)); y = 0; end"),
            format!("assign y = $bits({function}($root.P));"),
            format!("localparam UNUSED = {function}($root.P); assign y = 0;"),
        ] {
            let source = format!(
                "package P; timeunit 100ns / 10ps; endpackage module Top(output int y); timeunit 1ns / 1ps; {item} endmodule"
            );
            let error = build_error(&source);
            assert!(
                error.contains("unknown timescale query design element `P`"),
                "{item}: {error}"
            );
        }
    }
}

#[test]
fn rejects_system_function_calls_with_invalid_arguments() {
    let cases = [
        (
            "invalid call of `$countones`: expected 1 argument, found 2",
            "module Top(input logic a, output logic y);
                always_comb begin $countones(a, a); y = a; end
            endmodule",
        ),
        (
            "invalid call of `$clog2`: expected 1 argument, found 0",
            "module Top(output int y); assign y = $clog2(); endmodule",
        ),
        (
            "invalid call of `$readmemh`: expected 2 to 4 arguments, found 5",
            r#"module Top(output logic [7:0] y);
                logic [7:0] mem [0:3];
                initial $readmemh("m.hex", mem, 0, 3, 99);
                assign y = mem[0];
            endmodule"#,
        ),
        (
            "invalid call of `$display`: a system task has no value",
            r#"module Top(output logic y); assign y = $display("x"); endmodule"#,
        ),
    ];
    for (expected, source) in cases {
        let error = build_error(source);
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn value_functions_called_as_statements_check_their_operands() {
    for source in [
        "module Top(input logic a, output logic y);
            always_comb begin $countones(zzz); y = a; end
        endmodule",
        "module Top(input logic a, output logic y);
            always_comb begin $bits(no_such_signal); y = a; end
        endmodule",
        "module Top(input logic a, output logic y);
            always_comb begin $dimensions(no_such_signal); y = a; end
        endmodule",
    ] {
        build_error(source);
    }
}

#[test]
fn value_functions_called_as_statements_evaluate_their_operands_once() {
    let source = r#"
        module Top(input logic [3:0] a, output logic y);
            function automatic logic [3:0] g(input logic [3:0] v);
                $display("g %0d", v);
                return v;
            endfunction
            always_comb begin
                $countones(g(a));
                y = a[0];
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("eval.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    sim.drain_runtime_events();
    sim.modify(|io| io.set(a, 5u8)).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: b"g 5".to_vec(),
        }],
    );
}

fn initial_simulation(source: &str) -> celox::Simulation {
    celox::Simulation::from_sv_sources(vec![(source, Path::new("initial.sv"))], "Top")
        .build()
        .unwrap()
}

#[test]
fn initial_blocks_run_their_system_tasks_at_time_zero() {
    let source = r#"
        module Top(output logic [7:0] y);
            logic [15:0] a = 16'h1234;
            logic [7:0] b;
            initial begin
                b = a[0+:8];
                $display("b=%h", b);
                for (int i = 0; i < 2; i++) $write("%0d,", i);
                if (b == 8'h35) $display("unreachable");
                $error("e%0d", b - 8'h30);
            end
            assign y = b;
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    let y = sim.signal("y");
    // The block runs as a process at time zero, not before it.
    assert_eq!(sim.get(y), 0u8.into());
    assert_eq!(sim.drain_runtime_events(), Vec::new());
    assert_eq!(sim.step().unwrap(), Some(0));
    assert_eq!(sim.get(y), 0x34u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: b"b=34".to_vec(),
            },
            celox::RuntimeEvent::Write {
                message: b"0,".to_vec(),
            },
            celox::RuntimeEvent::Write {
                message: b"1,".to_vec(),
            },
            celox::RuntimeEvent::AssertContinue {
                message: b"e4".to_vec(),
            },
        ],
    );
    assert!(!sim.is_finished());
    assert_eq!(sim.step().unwrap(), None);
}

#[test]
fn finish_in_an_initial_block_ends_the_simulation() {
    let source = r#"
        module Top(output logic y);
            initial begin
                $display("before");
                $finish;
                $display("after");
            end
            assign y = 1'b0;
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    sim.step().unwrap();
    assert!(sim.is_finished());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: b"before".to_vec(),
        }],
    );
}

#[test]
fn fatal_in_an_initial_block_fails_the_simulation() {
    let source = r#"
        module Top(output logic y);
            initial begin
                $display("before");
                $fatal(1, "boom %0d", 7);
                $display("after");
            end
            assign y = 1'b0;
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    let error = sim.step().unwrap_err();
    assert!(error.to_string().contains("boom"), "{error}");
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: b"before".to_vec(),
            },
            celox::RuntimeEvent::AssertFatal {
                message: b"boom 7".to_vec(),
            },
        ],
    );
}

#[test]
fn initial_blocks_skip_the_operands_short_circuits_skip() {
    let source = r#"
        module Top(input logic a, output logic y);
            logic d;
            function automatic logic f(input logic v);
                $display("called");
                return v;
            endfunction
            initial begin
                d = 1'b1 || f(a);
                $display("%0d", d);
            end
            assign y = d;
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    sim.step().unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: b"1".to_vec(),
        }],
    );
}

#[test]
fn initial_blocks_that_read_design_state_run_at_time_zero() {
    let source = r#"
        module Top(input logic [3:0] a, output logic [3:0] y);
            logic [3:0] value = a + 4'd1;
            initial begin
                $display("value=%0d", value);
                if (a == 4'd0) $display("zero");
            end
            assign y = value;
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    let y = sim.signal("y");
    sim.step().unwrap();
    // The initializer runs before the initial block.
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: b"value=1".to_vec(),
            },
            celox::RuntimeEvent::Display {
                message: b"zero".to_vec(),
            },
        ],
    );
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn rejects_nonblocking_assignments_in_initial_processes() {
    let error = build_error(
        r#"module Top(output logic y);
            logic v;
            initial begin v <= 1'b1; $display("%0d", v); end
            assign y = v;
        endmodule"#,
    );
    assert!(
        error.contains("nonblocking assignment in an initial block that runs as a process"),
        "{error}"
    );
}

#[test]
fn rejects_countbits_with_missing_or_omitted_arguments() {
    for args in ["", "a", "a,", ", a", "a, '1,", "a,, '1"] {
        let source = format!(
            "module Top(input logic a, output int y); assign y = $countbits({args}); endmodule"
        );
        let error = build_error(&source);
        assert!(error.contains("$countbits"), "{args}: {error}");
    }
}

#[test]
fn countbits_called_as_a_statement_evaluates_all_arguments_once() {
    let source = r#"
        module Top(input logic [3:0] a, output logic y);
            function automatic logic [3:0] value(input logic [3:0] v);
                $display("value %0d", v);
                return v;
            endfunction
            function automatic logic control(input logic v);
                $display("control %0d", v);
                return v;
            endfunction
            always_comb begin
                $countbits(value(a), control(a[0]), control(a[1]));
                y = a[0];
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("countbits_statement.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let a = sim.signal("a");
    sim.drain_runtime_events();
    sim.modify(|io| io.set(a, 5u8)).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: b"value 5".to_vec()
            },
            celox::RuntimeEvent::Display {
                message: b"control 1".to_vec()
            },
            celox::RuntimeEvent::Display {
                message: b"control 0".to_vec()
            },
        ]
    );
}

#[test]
fn dimensions_rejects_undefined_constant_operands() {
    for source in [
        "module Top(output int y); localparam N = $dimensions(missing); assign y = 0; endmodule",
        "module Top(output int y); if ($dimensions(missing) == 1) assign y = 1; else assign y = 0; endmodule",
    ] {
        build_error(source);
    }
}

#[test]
fn dimensions_rejects_zero_size_casts() {
    for statement in [
        "assign y = $dimensions(logic)'(1'b1);",
        "localparam P = $dimensions(logic)'(1'b1); assign y = P;",
        "localparam N = $dimensions(logic); assign y = N'(1'b1);",
        "localparam N = $dimensions(real); assign y = N'(1'b1);",
    ] {
        build_error(&format!("module Top(output int y); {statement} endmodule"));
    }
}

#[test]
fn dimensions_rejects_malformed_pattern_arguments() {
    for statement in [
        "assign y = $dimensions(f('{8'h1, 8'h2, 8'h3}));",
        "localparam N = $dimensions(f('{8'h1, 8'h2, 8'h3})); assign y = 0;",
    ] {
        build_error(&format!(
            "module Top(output int y); function automatic logic [7:0] f(input logic [7:0] a [0:1]); return a[0]; endfunction {statement} endmodule"
        ));
    }
}

#[test]
fn unpacked_dimensions_rejects_undefined_operands() {
    for source in [
        "module Top(output int y); assign y = $unpacked_dimensions(missing); endmodule",
        "module Top(output int y); localparam N = $unpacked_dimensions(missing); assign y = 0; endmodule",
        "module Top(output int y); if ($unpacked_dimensions(missing) == 1) assign y = 1; else assign y = 0; endmodule",
        "module Top(input logic a, output logic y); always_comb begin $unpacked_dimensions(missing); y = a; end endmodule",
    ] {
        build_error(source);
    }
}

#[test]
fn unpacked_dimensions_rejects_excess_indices() {
    for operand in ["a[0][3][0]", "a[0][3][0:0]", "a[0][3][0 +: 1]"] {
        for use_site in [
            format!("assign y = $unpacked_dimensions({operand});"),
            format!("localparam N = $unpacked_dimensions({operand}); assign y = 0;"),
            format!("always_comb begin $unpacked_dimensions({operand}); y = 0; end"),
        ] {
            build_error(&format!(
                "module Top(output int y); logic [7:0] a [0:1]; {use_site} endmodule"
            ));
        }
    }
}

#[test]
fn unpacked_dimensions_rejects_zero_size_casts() {
    for statement in [
        "assign y = $unpacked_dimensions(logic)'(1'b1);",
        "localparam P = $unpacked_dimensions(logic)'(1'b1); assign y = P;",
        "localparam N = $unpacked_dimensions(logic); assign y = N'(1'b1);",
    ] {
        build_error(&format!("module Top(output int y); {statement} endmodule"));
    }
}

#[test]
fn unpacked_dimensions_rejects_malformed_pattern_arguments() {
    for statement in [
        "assign y = $unpacked_dimensions(f('{8'h1, 8'h2, 8'h3}));",
        "localparam N = $unpacked_dimensions(f('{8'h1, 8'h2, 8'h3})); assign y = 0;",
    ] {
        build_error(&format!(
            "module Top(output int y); function automatic logic [7:0] f(input logic [7:0] a [0:1]); return a[0]; endfunction {statement} endmodule"
        ));
    }
}

#[test]
fn unpacked_dimensions_rejects_invalid_range_bounds() {
    for operand in [
        "a[0][MISSING:0]",
        "a[0][0:MISSING]",
        "a[0][MISSING +: 1]",
        "a[0][(MISSING == 0) +: 1]",
        "a[0][(0 << MISSING) +: 1]",
        "a[0][0 +: MISSING]",
        "a[0][0 +: 0]",
    ] {
        for use_site in [
            format!("assign y = $unpacked_dimensions({operand});"),
            format!("localparam N = $unpacked_dimensions({operand}); assign y = 0;"),
        ] {
            build_error(&format!(
                "module Top(output int y); logic [7:0] a [0:1][0:2]; {use_site} endmodule"
            ));
        }
    }
}

#[test]
fn unpacked_dimensions_rejects_function_argument_count() {
    for declaration in [
        "function int f(input int a); return a; endfunction",
        "function int f; input int a; begin f = a; end endfunction",
        "function int f(input int a, input int b = 2); return a + b; endfunction",
    ] {
        for arguments in ["", "1, 2, 3"] {
            for use_site in [
                format!("assign y = $unpacked_dimensions(f({arguments}));"),
                format!("localparam N = $unpacked_dimensions(f({arguments})); assign y = 0;"),
            ] {
                build_error(&format!(
                    "module Top(output int y); {declaration} {use_site} endmodule"
                ));
            }
        }
    }
    build_error(
        "module Top(output int y); function int f(input int a); return a; endfunction assign y = $unpacked_dimensions(f(1, 2)); endmodule",
    );
}

#[test]
fn unpacked_dimensions_rejects_nonwritable_function_arguments() {
    for direction in ["output", "inout"] {
        for operand in ["1 + 2", "1", "int'(a)", "{a, 1'b0}", "P", "P[0]"] {
            for use_site in [
                format!("assign y = $unpacked_dimensions(f({operand}));"),
                format!("localparam N = $unpacked_dimensions(f({operand})); assign y = 0;"),
            ] {
                build_error(&format!(
                    "module Top(output int y); int a; localparam int P = 1; function int f({direction} int x); x = 4; return x; endfunction {use_site} endmodule"
                ));
            }
        }
    }
}

#[test]
fn dimension_queries_reject_four_state_parameter_output_actuals() {
    for query in ["$unpacked_dimensions", "$dimensions"] {
        for direction in ["output", "inout"] {
            for value in ["4'bxxxx", "4'bzzzz", "4'b10xz"] {
                for operand in ["P", "P[0]", "{a, P}", "{P, a}"] {
                    for use_site in [
                        format!("assign y = {query}(f({operand}));"),
                        format!("localparam N = {query}(f({operand})); assign y = N;"),
                    ] {
                        let diagnostic = build_error(&format!(
                            "module Top(output int y); logic [3:0] a; localparam logic [3:0] P = {value}; function int f({direction} logic [7:0] x); x = 0; f = 1; endfunction {use_site} endmodule"
                        ));
                        assert!(
                            diagnostic.contains("non-writable argument")
                                || diagnostic.contains("operand of"),
                            "{query}({operand}), {direction}, {value}, {use_site}: {diagnostic}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn unpacked_dimensions_rejects_omitted_required_arguments() {
    for arguments in [",", "1,", ", 2, ,", ", , 3"] {
        for use_site in [
            format!("assign y = $unpacked_dimensions(f({arguments}));"),
            format!("localparam N = $unpacked_dimensions(f({arguments})); assign y = 0;"),
        ] {
            build_error(&format!(
                "module Top(output int y); function int f(input int a = 1, input int b, input int c = 3); return a + b + c; endfunction {use_site} endmodule"
            ));
        }
    }
}

#[test]
fn dimension_queries_reject_invalid_indices() {
    for query in ["$unpacked_dimensions", "$dimensions"] {
        for index in [
            "MISSING",
            "(MISSING == 0)",
            "(0 << MISSING)",
            "f()",
            "f(1, 2)",
        ] {
            for statement in [
                format!("assign y = {query}(a[{index}]);"),
                format!("localparam N = {query}(a[{index}]); assign y = N;"),
            ] {
                build_error(&format!(
                    "module Top(output int y); logic [7:0] a [0:1][0:2]; function int f(input int x); return x; endfunction {statement} endmodule"
                ));
            }
        }
    }
}

#[test]
fn urandom_initial_processes_have_separate_default_streams() {
    let source = r#"
        module Top(output int unsigned a, b, replay_a, replay_b);
            function automatic int unsigned draw(); return $urandom(); endfunction
            initial begin a = draw(); a = draw(); end
            initial begin b = draw(); b = draw(); end
            initial begin
                replay_a = $urandom(123); replay_a = draw();
                replay_b = $urandom(123); replay_b = draw();
            end
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    sim.step().unwrap();
    assert_ne!(sim.get(sim.signal("a")), sim.get(sim.signal("b")));
    assert_eq!(
        sim.get(sim.signal("replay_a")),
        sim.get(sim.signal("replay_b"))
    );
}

#[test]
fn urandom_rejects_invalid_seeds_and_constant_value_contexts() {
    for item in [
        "always_ff @(posedge clk) y <= $urandom(1, 2);",
        "always_ff @(posedge clk) y <= $urandom(,);",
        "always_ff @(posedge clk) y <= $urandom(.seed(1));",
        "always_ff @(posedge clk) y <= $urandom(int);",
        "always_ff @(posedge clk) y <= $urandom(1.5);",
        "int seeds[2]; always_ff @(posedge clk) y <= $urandom(seeds);",
        "localparam UNUSED = $urandom; assign y=0;",
        "localparam UNUSED = 0 && $urandom; assign y=0;",
    ] {
        let source = format!("module Top(input logic clk, output int y); {item} endmodule");
        let error = build_error(&source);
        assert!(error.contains("urandom"), "{item}: {error}");
    }
    // A query inspects the return type without executing the RNG; an
    // element of an unpacked array is still a valid integral seed.
    let source = "module Top(input logic clk, output int width, value); int seeds[2]; localparam W=$bits($urandom()); assign width=W; always_ff @(posedge clk) value <= $urandom(seeds[0]); endmodule";
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("random_query.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    assert_eq!(sim.get(sim.signal("width")), 32u32.into());
}

#[test]
fn urandom_default_seeds_follow_mixed_static_process_order() {
    fn draws(extra: &str) -> Vec<BigUint> {
        let source = format!(
            r#"
            module Top(input logic clk, output int unsigned early, comb, clocked, late, extra);
              initial early = $urandom;
              always_comb comb = $urandom;
              always_ff @(posedge clk) clocked <= $urandom;
              initial late = $urandom;
              {extra}
            endmodule
        "#
        );
        let mut sim = initial_simulation(&source);
        sim.add_clock("clk", 2, 1);
        sim.step().unwrap();
        sim.step().unwrap();
        ["early", "comb", "clocked", "late"]
            .into_iter()
            .map(|name| sim.get(sim.signal(name)))
            .collect()
    }
    let baseline = draws("");
    for extra in [
        "always_ff @(posedge clk) extra <= $urandom;",
        "initial extra = $urandom;",
        "always_comb extra = $urandom;",
        "always @* extra = $urandom;",
    ] {
        assert_eq!(draws(extra), baseline, "appended process: {extra}");
    }
    for (i, value) in baseline.iter().enumerate() {
        for other in &baseline[i + 1..] {
            assert_ne!(value, other);
        }
    }
}

#[test]
fn urandom_comb_starts_after_initial_procedures() {
    let source = r#"
        module Child(input int seed, output int unsigned value);
          always_comb begin value = $urandom(seed); $display("comb=%0d", seed); end
        endmodule
        module Top(output int unsigned value);
          int seed;
          Child child(seed, value);
          initial begin seed = 17; $display("initial"); end
        endmodule
    "#;
    let mut sim = initial_simulation(source);
    assert!(sim.drain_runtime_events().is_empty());
    sim.step().unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: b"initial".to_vec()
            },
            celox::RuntimeEvent::Display {
                message: b"comb=17".to_vec()
            },
        ]
    );
    let value = sim.get(sim.signal("value"));
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(sim.signal("value")), value);
    assert!(sim.drain_runtime_events().is_empty());
}

#[test]
fn urandom_comb_interpreter_preserves_activation_and_unknown_masks() {
    let source = r#"
        module Top(input logic trigger, output logic echo, output int unsigned value);
          function automatic int unsigned draw(); return $urandom; endfunction
          always_comb begin echo = trigger; value = draw(); end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("random_interp.sv"))], "Top")
        .four_state(true)
        .build_interpreter()
        .unwrap();
    let trigger = sim.signal("trigger");
    let value = sim.signal("value");
    sim.modify(|io| io.set(trigger, 0u8)).unwrap();
    let first = sim.get(value);
    sim.modify(|io| io.set_four_state(trigger, 0u8.into(), 1u8.into()))
        .unwrap();
    let second = sim.get(value);
    assert_ne!(first, second);
    assert_eq!(sim.get_four_state(value).1, 0u8.into());
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(value), second);
}

#[test]
fn urandom_declaration_initializers_have_distinct_stable_streams() {
    fn run(extra: &str) -> Vec<BigUint> {
        let source = format!(
            r#"
            module Top(output int unsigned first, second, observed_first, observed_second);
              int unsigned a = $urandom;
              int unsigned b = $urandom;
              assign first = a;
              assign second = b;
              initial begin observed_first = a; observed_second = b; end
              {extra}
            endmodule
        "#
        );
        let mut sim = initial_simulation(&source);
        sim.step().unwrap();
        ["first", "second", "observed_first", "observed_second"]
            .map(|name| sim.get(sim.signal(name)))
            .to_vec()
    }
    let baseline = run("");
    assert_ne!(baseline[0], 0u8.into());
    assert_ne!(baseline[1], 0u8.into());
    assert_ne!(baseline[0], baseline[1]);
    assert_eq!(baseline[0], baseline[2]);
    assert_eq!(baseline[1], baseline[3]);
    assert_eq!(baseline, run("int unsigned later = $urandom;"));
}

#[test]
fn urandom_parameter_storage_does_not_consume_default_seeds() {
    fn run(parameter: &str) -> BigUint {
        let source = format!(
            r#"
            module Top(output int unsigned value);
              {parameter}
              initial value = $urandom;
            endmodule
        "#
        );
        let mut sim = initial_simulation(&source);
        sim.step().unwrap();
        sim.get(sim.signal("value"))
    }
    let baseline = run("");
    for parameter in [
        "parameter logic [7:0] A[0:0] = '{0};",
        "localparam logic [7:0] A[0:0] = '{0};",
    ] {
        assert_eq!(run(parameter), baseline, "{parameter}");
    }
}
