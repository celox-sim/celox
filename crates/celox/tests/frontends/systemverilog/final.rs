use celox::{RuntimeEvent, SimBackend, Simulation};
use std::path::Path;

// The shared timed-suite emitter uses its own final checker and therefore
// rejects final procedures whose order relative to that checker is unspecified.
// These portable fixtures are also suitable for direct external execution;
// API checks here inspect state only after finalization has completed.
// Every source uses 1ns/1ns so API ticks and HDL delay units coincide.
const STATE: &str = include_str!("../../fixtures/final/state.sv");
const HIERARCHY: &str = include_str!("../../fixtures/final/hierarchy.sv");
const FINISH: &str = include_str!("../../fixtures/final/finish.sv");

#[test]
fn final_procedures_require_the_simulation_api() {
    let source = "module Top(input logic done); final assert (done); endmodule";
    let error = celox::Simulator::from_sv_sources(vec![(source, Path::new("final.sv"))], "Top")
        .build_interpreter()
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            celox::SimulatorErrorKind::ProcessRequiresSimulation(_)
        ),
        "{error}"
    );
}

fn displays(events: Vec<RuntimeEvent>) -> Vec<Vec<u8>> {
    events
        .into_iter()
        .filter_map(|event| match event {
            RuntimeEvent::Display { message } => Some(message),
            _ => None,
        })
        .collect()
}

fn check_final_state<B: SimBackend>(mut sim: Simulation<B>) {
    sim.run_until(3).unwrap();
    assert_eq!(sim.get(sim.signal("value")), 17u8.into());
    assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
    assert_eq!(
        displays(sim.drain_runtime_events()),
        vec![
            b"initial calls=0".to_vec(),
            b"final value=17 calls=1".to_vec(),
        ]
    );
    sim.finish().unwrap();
    assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
    assert!(displays(sim.drain_runtime_events()).is_empty());
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn final_procedures_survive_native_image_roundtrip() {
    let image = Simulation::from_sv_sources(vec![(STATE, Path::new("final_state.sv"))], "Top")
        .compile_native()
        .unwrap()
        .into_program_image();
    let image =
        celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    check_final_state(
        Simulation::from_sv_sources(Vec::new(), "Top")
            .build_native_from_image(image)
            .unwrap(),
    );
}

macro_rules! check_backends {
    ($source:expr, $check:ident) => {
        for four_state in [false, true] {
            let build = || {
                Simulation::from_sv_sources(vec![($source, Path::new("final.sv"))], "Top")
                    .four_state(four_state)
            };
            $check(build().build_interpreter().unwrap());
            $check(build().build_cranelift().unwrap());
            $check(build().build_wasm().unwrap());
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            {
                $check(build().build_native().unwrap());
                $check(build().build_tiered().unwrap());
                $check(
                    build()
                        .scheduler_mode(celox::SchedulerMode::Differential)
                        .build_native()
                        .unwrap(),
                );
            }
        }
    };
}

#[test]
fn final_nonblocking_updates_do_not_run_after_termination() {
    let source = r#"`timescale 1ns/1ns
    module Top(output int value, observed);
        initial begin value=3; #1; $finish; end
        final begin value <= 99; observed = value; end
    endmodule"#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sim.signal("value")), 3u8.into());
        assert_eq!(sim.get(sim.signal("observed")), 3u8.into());
    }
    check_backends!(source, check);
}

#[test]
fn explicit_finish_runs_final_with_last_state() {
    check_backends!(STATE, check_final_state);
}

#[test]
fn final_procedures_run_in_modules_and_generate_blocks() {
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(2).unwrap();
        for name in ["first", "second"] {
            assert_eq!(sim.get(sim.signal(name)), 1u8.into(), "{name}");
        }
        // Output variable ports connect through implicit continuous
        // assignments (4.9.6, 23.2.2.3). Finalization does not run another
        // scheduled settle (9.2.3), so inspect the child's own final write.
        let child_done = sim.child_signal(&[("child", 0)], "done");
        assert_eq!(sim.get(child_done), 1u8.into());
        assert_eq!(sim.get(sim.signal("generated")), 3u8.into());
        let mut labels = displays(sim.drain_runtime_events());
        labels.sort();
        assert_eq!(
            labels,
            [
                "child",
                "generate 0",
                "generate 1",
                "top first",
                "top second"
            ]
            .map(|text| text.as_bytes().to_vec())
        );
    }
    check_backends!(HIERARCHY, check);
}

#[test]
fn finish_in_final_stops_remaining_execution() {
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
        assert_eq!(sim.get(sim.signal("after_finish")), 0u8.into());
        assert_eq!(
            displays(sim.drain_runtime_events()),
            vec![b"final calls=1".to_vec()]
        );
    }
    check_backends!(FINISH, check);
}

#[test]
fn host_finish_is_explicit_and_runs_final_only_once() {
    let source = r#"`timescale 1ns/1ns
    module Top(output int value, calls);
        initial begin value = 3; calls = 0; #2 value = 7; end
        final begin calls = calls + 1; value = value + 10; $display("final"); end
    endmodule"#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let value = sim.signal("value");
        let calls = sim.signal("calls");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(calls), 0u8.into());
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(value), 3u8.into());
        assert_eq!(sim.get(calls), 0u8.into());
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(value), 7u8.into());
        assert_eq!(sim.step().unwrap(), None);
        assert_eq!(sim.get(calls), 0u8.into());
        assert!(sim.drain_runtime_events().is_empty());
        sim.finish().unwrap();
        assert_eq!(sim.get(value), 17u8.into());
        assert_eq!(sim.get(calls), 1u8.into());
        assert_eq!(
            displays(sim.drain_runtime_events()),
            vec![b"final".to_vec()]
        );
        sim.finish().unwrap();
        assert_eq!(sim.step().unwrap(), None);
        assert_eq!(sim.get(calls), 1u8.into());
        assert!(sim.drain_runtime_events().is_empty());
    }
    check_backends!(source, check);
}

#[test]
fn stop_in_final_resumes_at_the_cursor_without_running_scheduled_events() {
    let source = r#"`timescale 1ns/1ns
    module Top(output int calls, other_calls, late_event);
        initial begin calls = 0; other_calls = 0; late_event = 0; #10 late_event = 1; end
        final begin calls = calls + 1; $stop; calls = calls + 1; end
        final other_calls = other_calls + 1;
    endmodule"#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        sim.finish().unwrap();
        assert!(sim.is_stopped());
        assert!(sim.checkpoint().is_err());
        assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
        assert_eq!(sim.get(sim.signal("late_event")), 0u8.into());
        // The other final may precede or follow the suspended one. After
        // resumption both must be complete, with no restart of either body.
        sim.step().unwrap();
        assert!(!sim.is_stopped());
        assert_eq!(sim.get(sim.signal("calls")), 2u8.into());
        assert_eq!(sim.get(sim.signal("other_calls")), 1u8.into());
        let finished = sim.checkpoint().unwrap();
        sim.restore(&finished).unwrap();
        sim.finish().unwrap();
        assert_eq!(sim.get(sim.signal("calls")), 2u8.into());
        sim.run_until(100).unwrap();
        sim.finish().unwrap();
        assert_eq!(sim.step().unwrap(), None);
        assert_eq!(sim.get(sim.signal("late_event")), 0u8.into());
        assert_eq!(sim.get(sim.signal("calls")), 2u8.into());
        assert_eq!(sim.get(sim.signal("other_calls")), 1u8.into());
    }
    check_backends!(source, check);
}

#[test]
fn stop_suspends_without_running_final() {
    let source = r#"`timescale 1ns/1ns
    module Top(output int value, calls);
        initial begin value = 3; calls = 0; #1 $stop; value = 7; $finish; end
        final begin calls = calls + 1; value = value + 10; end
    endmodule"#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(10).unwrap();
        assert!(sim.is_stopped());
        assert_eq!(sim.get(sim.signal("calls")), 0u8.into());
        assert_eq!(sim.get(sim.signal("value")), 3u8.into());
        sim.run_until(10).unwrap();
        assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
        assert_eq!(sim.get(sim.signal("value")), 17u8.into());
        sim.finish().unwrap();
        assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
    }
    check_backends!(source, check);
}

#[test]
fn fatal_runs_final_and_preserves_the_original_error() {
    // IEEE 1800-2023 20.10: $fatal implicitly calls $finish.
    let source = r#"`timescale 1ns/1ns
    module Top(output int calls);
        initial begin calls = 0; #1 $fatal(1, "initial fatal"); end
        final begin calls = calls + 1; $display("final after fatal"); end
    endmodule"#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let error = sim.run_until(2).unwrap_err();
        assert!(error.to_string().contains("initial fatal"), "{error}");
        assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
        let events = sim.drain_runtime_events();
        assert!(events.iter().any(|event| matches!(event,
            RuntimeEvent::Report(report)
                if report.severity == celox::Severity::Fatal && report.message == b"initial fatal"
        )), "{events:?}");
        assert!(
            events.iter().any(|event| matches!(event,
                RuntimeEvent::Display { message } if message == b"final after fatal"
            )),
            "{events:?}"
        );
    }
    check_backends!(source, check);
}

#[test]
fn final_order_is_sequential_and_repeatable() {
    let source = r#"`timescale 1ns/1ns
    module Top;
        initial begin #1; $finish; end
        final begin $write("A"); $display("a"); end
        final begin $write("B"); $display("b"); end
        final begin $write("C"); $display("c"); end
    endmodule"#;
    fn transcript<B: SimBackend>(mut sim: Simulation<B>) -> Vec<RuntimeEvent> {
        sim.run_until(2).unwrap();
        sim.drain_runtime_events()
            .into_iter()
            .filter(|event| {
                matches!(
                    event,
                    RuntimeEvent::Write { .. } | RuntimeEvent::Display { .. }
                )
            })
            .collect()
    }
    // Order is implementation-defined; require complete procedure pairs and
    // stability between fresh instances, not a particular source order.
    for four_state in [false, true] {
        let build = || {
            Simulation::from_sv_sources(vec![(source, Path::new("final_order.sv"))], "Top")
                .four_state(four_state)
        };
        let first = transcript(build().build_interpreter().unwrap());
        assert_eq!(first, transcript(build().build_interpreter().unwrap()));
        assert_eq!(first.len(), 6);
        let mut labels = Vec::new();
        for pair in first.as_chunks::<2>().0 {
            let [
                RuntimeEvent::Write { message: upper },
                RuntimeEvent::Display { message: lower },
            ] = pair
            else {
                panic!("interleaved final procedures: {first:?}");
            };
            assert_eq!(upper.len(), 1);
            assert_eq!(lower, &upper.to_ascii_lowercase());
            labels.push(upper[0]);
        }
        labels.sort_unstable();
        assert_eq!(labels, b"ABC");
    }
}

#[test]
fn watched_idle_deferred_fatal_runs_final_and_preserves_the_error() {
    let source = r#"`timescale 1ns/1ns
    module Top(input bit fail, output int calls);
        initial calls = 0;
        always_comb assert final (!fail) else $fatal(1, "deferred fatal");
        final calls = calls + 1;
    endmodule"#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        sim.set_fail_on(Some(celox::Severity::Error));
        let fail = sim.signal("fail");
        sim.modify(|io| io.set(fail, 1u8)).unwrap();
        // No timed process is scheduled: run_until must finalize a fatal
        // deferred report matured while settling watched host writes.
        let error = sim.run_until(1).unwrap_err();
        assert!(error.to_string().contains("deferred fatal"), "{error}");
        assert!(sim.is_finished());
        assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
    }
    check_backends!(source, check);
}

#[test]
fn pending_deferred_output_does_not_run_after_final() {
    let source = r#"`timescale 1ns/1ns
    module Top;
        initial begin assert final (0) else $display("late"); $finish; end
        final $display("final");
    endmodule"#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.step().unwrap();
        assert!(sim.is_finished());
        assert_eq!(
            displays(sim.drain_runtime_events()),
            vec![b"final".to_vec()]
        );
        assert!(displays(sim.drain_runtime_events()).is_empty());
        assert_eq!(sim.step().unwrap(), None);
        assert!(displays(sim.drain_runtime_events()).is_empty());
    }
    check_backends!(source, check);
}

#[test]
fn pending_deferred_output_does_not_run_while_final_is_stopped() {
    let source = r#"`timescale 1ns/1ns
    module Top;
        initial begin assert final (0) else $display("late"); $finish; end
        final begin $display("before"); $stop; $display("after"); end
    endmodule"#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.step().unwrap();
        assert!(sim.is_stopped());
        assert!(!sim.is_finished());
        assert_eq!(
            displays(sim.drain_runtime_events()),
            vec![b"before".to_vec()]
        );
        assert!(displays(sim.drain_runtime_events()).is_empty());
        sim.step().unwrap();
        assert!(!sim.is_stopped());
        assert!(sim.is_finished());
        assert_eq!(
            displays(sim.drain_runtime_events()),
            vec![b"after".to_vec()]
        );
        assert!(displays(sim.drain_runtime_events()).is_empty());
    }
    check_backends!(source, check);
}

#[test]
fn event_drain_finalizes_immediate_fatal_before_deferred_actions() {
    for deferred in [
        "",
        "always_comb assert final (!fail) else $display(\"deferred\");",
    ] {
        let source = format!(
            r#"`timescale 1ns/1ns
            module Top(input bit fail, output int calls);
                initial calls = 0;
                always_comb if (fail) $fatal(1, "drain fatal");
                {deferred}
                final begin calls += 1; $display("final"); end
            endmodule"#
        );
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            sim.drain_runtime_events();
            let fail = sim.signal("fail");
            sim.modify(|io| io.set(fail, 1u8)).unwrap();
            let events = sim.drain_runtime_events();
            assert!(sim.is_finished(), "{events:?}");
            assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event,
                        RuntimeEvent::Report(report) if report.severity == celox::Severity::Fatal
                            && report.message == b"drain fatal"
                    ))
                    .count(),
                1,
                "{events:?}"
            );
            assert_eq!(displays(events), vec![b"final".to_vec()]);
            assert!(sim.drain_runtime_events().is_empty());
            sim.finish().unwrap();
            assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
        }
        check_backends!(source.as_str(), check);
    }
}

#[test]
fn saved_state_contains_final_updates_after_lazy_finish() {
    let source = r#"`timescale 1ns/1ns
        module Top(input bit go, output int result, calls);
            initial begin result = 7; calls = 0; end
            always_comb if (go) $finish;
            final begin result = 42; calls += 1; end
        endmodule"#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        let before = sim.save_state().unwrap();
        let go = sim.signal("go");
        sim.modify(|io| io.set(go, 1u8)).unwrap();
        let finished = sim.save_state().unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.get(sim.signal("result")), 42u8.into());
        sim.load_state(&before).unwrap();
        assert!(!sim.is_finished());
        sim.load_state(&finished).unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.get(sim.signal("result")), 42u8.into());
        assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
        sim.finish().unwrap();
        assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
    }
    check_backends!(source, check);
}

#[test]
fn saving_state_rejects_a_stop_reached_inside_final() {
    let source = r#"`timescale 1ns/1ns
        module Top(input bit go, output int result);
            initial result = 0;
            always_comb if (go) $finish;
            final begin result = 7; $stop; result = 42; end
        endmodule"#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        let go = sim.signal("go");
        sim.modify(|io| io.set(go, 1u8)).unwrap();
        assert!(matches!(
            sim.save_state(),
            Err(celox::StateError::Checkpoint(
                celox::CheckpointError::Stopped
            ))
        ));
        assert!(sim.is_stopped());
        assert!(!sim.is_finished());
        assert_eq!(sim.get(sim.signal("result")), 7u8.into());
        sim.step().unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.get(sim.signal("result")), 42u8.into());
        let finished = sim.save_state().unwrap();
        sim.load_state(&finished).unwrap();
        assert_eq!(sim.get(sim.signal("result")), 42u8.into());
    }
    check_backends!(source, check);
}

#[test]
fn saving_state_preserves_fatal_and_runs_final() {
    let source = r#"`timescale 1ns/1ns
        module Top(input bit fail, output int calls);
            initial calls = 0;
            always_comb if (fail) $fatal(1, "save fatal");
            final calls += 1;
        endmodule"#;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        let fail = sim.signal("fail");
        sim.modify(|io| io.set(fail, 1u8)).unwrap();
        assert!(matches!(sim.save_state(),
            Err(celox::StateError::Runtime(celox::RuntimeErrorCode::Fatal { message, .. }))
                if message == "save fatal"
        ));
        assert!(sim.is_finished());
        assert_eq!(sim.get(sim.signal("calls")), 1u8.into());
    }
    check_backends!(source, check);
}
