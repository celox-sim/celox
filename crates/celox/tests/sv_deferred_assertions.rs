#![cfg(feature = "systemverilog")]
//! The actions of deferred immediate assertions wait in the deferred
//! assertion report queue of their process: `#0` ones mature in the
//! Observed region, `final` ones in the Postponed region, and a flush point
//! of the process discards those still waiting (IEEE 1800-2023 16.4).
use celox::{AssertionDeferral, Report, RuntimeEvent, Severity};
use std::path::Path;

/// The reports and displays of `events`, as (label or message, deferral).
fn summary(events: Vec<RuntimeEvent>) -> Vec<(String, Option<AssertionDeferral>)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            RuntimeEvent::Report(Report {
                message, assertion, ..
            }) => Some((
                String::from_utf8(message).unwrap(),
                assertion.map(|assertion| assertion.deferral),
            )),
            RuntimeEvent::Display { message } => Some((String::from_utf8(message).unwrap(), None)),
            _ => None,
        })
        .collect()
}

macro_rules! deferred {
    ($module:ident, $backend:ident) => {
        mod $module {
            use super::*;

            fn simulation(body: &str) -> celox::Simulation<impl celox::SimBackend> {
                let source = format!(
                    "`timescale 1ns/1ns\nmodule Top;\n  bit a, b, c, x;\n{body}\nendmodule"
                );
                celox::Simulation::from_sv_sources(
                    vec![(source.as_str(), Path::new("deferred.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap()
            }

            fn run(body: &str, time: u64) -> Vec<(String, Option<AssertionDeferral>)> {
                let mut sim = simulation(body);
                sim.run_until(time).unwrap();
                summary(sim.drain_runtime_events())
            }

            /// The first execution fails while `b` lags `a`; the process
            /// resumes from its event control when `b` follows, which
            /// flushes the failure. The immediate assertion reports it.
            #[test]
            fn an_event_control_resume_flushes_waiting_actions() {
                assert_eq!(
                    run(
                        "  initial forever begin\n    \
                         @(a or b);\n    \
                         i: assert (a == b) else $error(\"i\");\n    \
                         d: assert #0 (a == b) else $error(\"d\");\n  \
                         end\n  \
                         initial begin #1 a = 1; #0 b = 1; end",
                        1
                    ),
                    vec![("i".to_owned(), Some(AssertionDeferral::None))]
                );
            }

            /// IEEE 1800-2023 16.4.2: an Observed region is reached after
            /// `a3` and before the next flush point, so `a3` reports; `a4`
            /// is flushed when `b` changes after it ran.
            #[test]
            fn a_delay_resume_is_no_flush_point() {
                assert_eq!(
                    run(
                        "  always @(a or b) begin\n    \
                         a3: assert #0 (a == b) else $error(\"a3\");\n    \
                         #1;\n    \
                         a4: assert #0 (a == b) else $error(\"a4\");\n  \
                         end\n  \
                         initial begin #1 a = 1; #1; #0 b = 1; end",
                        3
                    ),
                    vec![("a3".to_owned(), Some(AssertionDeferral::Observed))]
                );
            }

            /// In one time slot, `$display` output comes first, then the
            /// `#0` actions, then the `final` ones.
            #[test]
            fn observed_actions_mature_before_postponed_ones() {
                assert_eq!(
                    run(
                        "  initial begin\n    \
                         #1;\n    \
                         f: assert final (0) else $error(\"f\");\n    \
                         o: assert #0 (0) else $error(\"o\");\n    \
                         p: assert #0 (1) $display(\"p\");\n    \
                         $display(\"d\");\n  \
                         end",
                        1
                    ),
                    vec![
                        ("d".to_owned(), None),
                        ("o".to_owned(), Some(AssertionDeferral::Observed)),
                        ("p".to_owned(), None),
                        ("f".to_owned(), Some(AssertionDeferral::Final)),
                    ]
                );
            }

            /// Each evaluation of an edge-triggered process resumes it from
            /// its event control: the second edge of a time slot flushes the
            /// failure of the first.
            #[test]
            fn a_second_edge_flushes_a_flip_flop_process() {
                let body = "  always @(posedge c) o: assert #0 (x) else $error(\"o\");\n  ";
                assert_eq!(
                    run(&format!("{body}initial #1 c = 1;"), 1),
                    vec![("o".to_owned(), Some(AssertionDeferral::Observed))]
                );
                assert_eq!(
                    run(&format!("{body}initial begin #1 c = 1; #0 c = 0; x = 1; #0 c = 1; end"), 1),
                    vec![]
                );
            }

            /// Combinational processes run again when a source changes; the
            /// second run flushes the failure of the first (IEEE 1800-2023
            /// 16.4.2). A module item is the same assertion in an
            /// `always_comb` (16.4.3).
            #[test]
            fn rerunning_a_combinational_process_flushes_waiting_actions() {
                let glitch = "initial begin #1 a = 1; #0 b = 1; end";
                for assertion in [
                    "always_comb d: assert #0 (a == b) else $error(\"d\");",
                    "always_comb d: assert final (a == b) else $error(\"d\");",
                    "always @(a or b) d: assert #0 (a == b) else $error(\"d\");",
                    "d: assert #0 (a == b) else $error(\"d\");",
                    "d: assume final (a == b) else $error(\"d\");",
                ] {
                    assert_eq!(run(&format!("  {assertion}\n  {glitch}"), 1), vec![], "{assertion}");
                    let mut reports = run(&format!("  {assertion}\n  initial #1 a = 1;"), 1);
                    assert_eq!(reports.len(), 1, "{assertion}: {reports:?}");
                    assert_eq!(reports.remove(0).0, "d", "{assertion}");
                }
            }

            /// A process also runs again when the evaluation itself changes
            /// one of its inputs, here `y` through the continuous assignment
            /// from `x`, and that run flushes the earlier failure too.
            #[test]
            fn reruns_from_changes_within_an_evaluation_flush_waiting_actions() {
                let body = "  bit y;\n  assign y = ~x;\n  \
                            always_comb d: assert #0 (a == y) else $error(\"d\");\n  \
                            initial begin x = 1; #1 a = 1; #0 x = 0; end";
                assert_eq!(run(body, 1), vec![]);
            }

            /// A module item in a generate region or a generate block is a
            /// deferred assertion as well.
            #[test]
            fn module_assertions_in_generate_scopes_are_deferred() {
                for item in [
                    "generate d: assert #0 (a == b) else $error(\"d\"); endgenerate",
                    "if (1) begin : g d: assert #0 (a == b) else $error(\"d\"); end",
                    "for (genvar i = 0; i < 1; i++) begin : g\n    d: assert #0 (a == b) else $error(\"d\");\n  end",
                    "case (1) 1: begin : g d: assert #0 (a == b) else $error(\"d\"); end endcase",
                ] {
                    assert_eq!(
                        run(&format!("  {item}\n  initial begin #1 a = 1; #0 b = 1; end"), 1),
                        vec![],
                        "{item}"
                    );
                    let reports = run(&format!("  {item}\n  initial #1 a = 1;"), 1);
                    assert_eq!(reports.len(), 1, "{item}: {reports:?}");
                }
            }

            /// A run watching reports matures the actions of a settle that
            /// no step ran before it checks them, and stops there.
            #[test]
            fn fail_on_sees_actions_of_settles_without_steps() {
                let mut sim = simulation("  always_comb d: assert #0 (a) else $error(\"d\");");
                sim.set_fail_on(Some(Severity::Error));
                sim.run_until(10).unwrap();
                assert_eq!(sim.time(), 0);
                assert_eq!(sim.failure().map(|report| report.message.clone()), Some(b"d".to_vec()));
            }

            /// A run leaving a slot with nothing due ends it, so a deferred
            /// `$fatal` of its settle ends the run there, also without
            /// `set_fail_on`, by `run_until` or by `step`.
            #[test]
            fn a_deferred_fatal_of_an_idle_slot_ends_the_run() {
                for (body, step) in [
                    ("  always_comb assert #0 (a) else $fatal(1, \"failed\");", false),
                    ("  always_comb assert #0 (a) else $fatal(1, \"failed\");\n  initial #5 $display(\"later\");", true),
                ] {
                    let mut sim = simulation(body);
                    let result = if step { sim.step().map(|_| ()) } else { sim.run_until(10) };
                    assert!(result.is_err(), "{body}");
                    assert_eq!(sim.time(), 0, "{body}");
                }
                // Also when a settle at that time already ran, by
                // `eval_comb` or by a run to the current time.
                let body = "  always_comb assert #0 (a) else $fatal(1, \"failed\");";
                for settle in [false, true] {
                    let mut sim = simulation(body);
                    if settle {
                        sim.eval_comb().unwrap();
                    } else {
                        sim.run_until(0).unwrap();
                    }
                    assert!(sim.run_until(10).is_err(), "eval_comb: {settle}");
                    assert_eq!(sim.time(), 0, "eval_comb: {settle}");
                }
                // `advance_to` leaves the slot too.
                let mut sim = simulation(body);
                assert!(sim.advance_to(1).is_err());
                assert_eq!(sim.time(), 0);
                // A step with nothing scheduled, after a settle or not.
                for settle in [false, true] {
                    let mut sim = simulation(body);
                    if settle {
                        sim.eval_comb().unwrap();
                    }
                    assert!(sim.step().is_err(), "step after eval_comb: {settle}");
                }
            }

            /// A run watching reports also checks the actions of a settle
            /// that already ran at the time it leaves.
            #[test]
            fn fail_on_sees_actions_of_earlier_settles() {
                let mut sim = simulation("  always_comb d: assert #0 (a) else $error(\"d\");");
                sim.eval_comb().unwrap();
                sim.set_fail_on(Some(Severity::Error));
                sim.run_until(10).unwrap();
                assert_eq!(sim.time(), 0);
                assert_eq!(sim.failure().map(|report| report.message.clone()), Some(b"d".to_vec()));
            }

            /// A design without events may never step; the initial settle
            /// of combinational logic belongs to time zero, whose slot is
            /// over once nothing more is due.
            #[test]
            fn actions_of_the_initial_settle_mature_without_a_step() {
                let mut sim = simulation("  d: assume #0 (a) else $error(\"d\");");
                sim.run_until(0).unwrap();
                assert_eq!(
                    summary(sim.drain_runtime_events()),
                    vec![("d".to_owned(), Some(AssertionDeferral::Observed))]
                );
            }

            /// A deferred cover reports its pass action when it matures.
            #[test]
            fn deferred_cover_reports_when_it_matures() {
                let cover = "  c: cover #0 (a != b) $info(\"c\");\n  ";
                assert_eq!(run(&format!("{cover}initial begin #1 a = 1; #0 b = 1; end"), 1), vec![]);
                assert_eq!(
                    run(&format!("{cover}initial #1 a = 1;"), 1),
                    vec![("c".to_owned(), Some(AssertionDeferral::Observed))]
                );
            }

            /// A deferred `$fatal` of a combinational process ends the
            /// simulation when it matures, not while combinational logic
            /// settles.
            #[test]
            fn a_combinational_deferred_fatal_ends_the_simulation_when_it_matures() {
                let mut sim = simulation(
                    "  always_comb f: assert final (a == b) else $fatal(1, \"boom\");\n  \
                     initial begin #1 a = 1; #0 b = 1; #1 a = 0; end",
                );
                sim.run_until(1).unwrap();
                assert!(sim.run_until(5).is_err());
                assert_eq!(sim.time(), 2);
            }

            /// A deferred `$fatal` ends the simulation when it matures, at
            /// the end of its time slot.
            #[test]
            fn a_deferred_fatal_ends_the_simulation_when_it_matures() {
                let mut sim = simulation(
                    "  initial begin\n    \
                     #1;\n    \
                     f: assert final (0) else $fatal(1, \"boom\");\n    \
                     $display(\"after\");\n  \
                     end",
                );
                assert!(sim.run_until(5).is_err());
                assert_eq!(sim.time(), 1);
                let events = sim.drain_runtime_events();
                assert!(events.iter().any(|event| matches!(
                    event,
                    RuntimeEvent::Report(Report { severity: Severity::Fatal, message, .. })
                        if message == b"boom"
                )), "{events:?}");
            }

            /// A deferred assertion that only a `wait` condition reaches
            /// still waits for the end of the slot.
            #[test]
            fn assertions_in_wait_conditions_are_deferred() {
                assert_eq!(
                    run(
                        "  function automatic bit check();\n    \
                         d: assert #0 (0) else $error(\"d\");\n    \
                         return 1;\n  \
                         endfunction\n  \
                         initial begin\n    \
                         #1;\n    \
                         wait (check());\n    \
                         $display(\"after\");\n  \
                         end",
                        5
                    ),
                    vec![
                        ("after".to_owned(), None),
                        ("d".to_owned(), Some(AssertionDeferral::Observed)),
                    ]
                );
            }

            /// The simulation ends at a deferred `$fatal`: the actions that
            /// would mature after it, here in the Postponed region, do not
            /// run.
            #[test]
            fn actions_after_a_deferred_fatal_do_not_mature() {
                let mut sim = simulation(
                    "  initial begin\n    \
                     #1;\n    \
                     assert #0 (0) else $fatal(1, \"boom\");\n    \
                     assert final (1) $display(\"after fatal\");\n  \
                     end",
                );
                assert!(sim.run_until(5).is_err());
                assert_eq!(
                    summary(sim.drain_runtime_events()),
                    vec![("boom".to_owned(), Some(AssertionDeferral::Observed))]
                );
            }

            /// A deferred assertion that only the default of an omitted
            /// argument reaches still waits for the end of the slot.
            #[test]
            fn assertions_in_argument_defaults_are_deferred() {
                assert_eq!(
                    run(
                        "  function automatic bit check();\n    \
                         d: assert #0 (0) else $error(\"d\");\n    \
                         return 1;\n  \
                         endfunction\n  \
                         function automatic bit f(bit v = check());\n    \
                         return v;\n  \
                         endfunction\n  \
                         initial begin\n    \
                         #1;\n    \
                         x = f();\n    \
                         $display(\"after\");\n  \
                         end",
                        5
                    ),
                    vec![
                        ("after".to_owned(), None),
                        ("d".to_owned(), Some(AssertionDeferral::Observed)),
                    ]
                );
            }

            /// Events lost to a full event buffer may have held a deferred
            /// `$fatal`, which then still ends the simulation.
            #[test]
            fn a_deferred_fatal_lost_to_the_event_buffer_ends_the_simulation() {
                let mut sim = simulation(
                    "  initial begin\n    \
                     #1;\n    \
                     f: assert final (0) else $fatal(1, \"boom\");\n    \
                     repeat (2000) $display(\"x\");\n    \
                     #1 $display(\"after\");\n  \
                     end",
                );
                let error = sim.run_until(5).unwrap_err();
                assert!(error.to_string().contains("missed"), "{error}");
                assert_eq!(sim.time(), 1);
            }
        }
    };
}

deferred!(interpreter, build_interpreter);
deferred!(cranelift, build);
#[cfg(target_arch = "x86_64")]
deferred!(native, build_native);
deferred!(wasm, build_wasm);

/// A `Simulator` call is a time slot: a deferred assertion of a flip-flop
/// domain matures at the end of the tick that ran it.
#[test]
fn deferred_actions_mature_at_the_end_of_a_tick() {
    let source = "module Top(input bit clk, input bit x);\n  \
                  always_ff @(posedge clk) o: assert #0 (x) else $error(\"o\");\n\
                  endmodule";
    let mut sim =
        celox::Simulator::from_sv_sources(vec![(source, Path::new("deferred.sv"))], "Top")
            .build()
            .unwrap();
    let clk = sim.event("clk");
    sim.tick(clk).unwrap();
    assert_eq!(
        summary(sim.drain_runtime_events()),
        vec![("o".to_owned(), Some(AssertionDeferral::Observed))]
    );
}

/// So is a call of the edge APIs that run derived domains, and of
/// `edges`: a later edge does not flush the actions of an earlier one.
#[test]
fn deferred_actions_mature_at_the_end_of_edges_and_derived_ticks() {
    let source = "module Top(input bit clk, input bit x);\n  \
                  bit div;\n  \
                  always_ff @(posedge clk) div <= !div;\n  \
                  always_ff @(posedge clk) o: assert #0 (x) else $error(\"o\");\n  \
                  always_ff @(posedge div) d: assert #0 (x) else $error(\"d\");\n\
                  endmodule";
    let mut sim =
        celox::Simulator::from_sv_sources(vec![(source, Path::new("deferred.sv"))], "Top")
            .build()
            .unwrap();
    let clk = sim.event("clk");
    for _ in 0..2 {
        sim.edges(&[(clk, celox::Edge::Rising)]).unwrap();
        sim.edges(&[(clk, celox::Edge::Falling)]).unwrap();
    }
    for _ in 0..4 {
        sim.tick(clk).unwrap();
    }
    let labels: Vec<_> = summary(sim.drain_runtime_events())
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(labels, ["o", "d", "o", "o", "d", "o", "o", "d", "o"]);
}

/// Without a scheduler, a settle of combinational logic ends the host
/// call's time slot.
#[test]
fn combinational_deferred_actions_mature_when_logic_settles() {
    let source = "module Top(input bit a);\n  \
                  always_comb d: assert #0 (a) else $error(\"d\");\n\
                  endmodule";
    let mut sim =
        celox::Simulator::from_sv_sources(vec![(source, Path::new("deferred.sv"))], "Top")
            .build()
            .unwrap();
    assert_eq!(
        summary(sim.drain_runtime_events()),
        vec![("d".to_owned(), Some(AssertionDeferral::Observed))]
    );
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(summary(sim.drain_runtime_events()), vec![]);
}

/// A drain returns a deferred `$fatal` of combinational logic as a fatal
/// report.
#[test]
fn draining_a_combinational_deferred_fatal_returns_its_report() {
    let source = "module Top(input bit a);\n  \
                  always_comb assert #0 (a) else $fatal(1, \"failed\");\n\
                  endmodule";
    let mut sim =
        celox::Simulator::from_sv_sources(vec![(source, Path::new("deferred.sv"))], "Top")
            .build()
            .unwrap();
    let events = sim.drain_runtime_events();
    assert!(
        matches!(
            events.as_slice(),
            [RuntimeEvent::Report(Report { severity: Severity::Fatal, message, .. })]
                if message == b"failed"
        ),
        "{events:?}"
    );
}

/// A tick is one time slot: the settle before its flip-flops does not end
/// it, so the settle after them flushes what failed in between.
#[test]
fn a_tick_flushes_actions_of_the_settle_before_its_flip_flops() {
    let source = "module Top(input bit clk);\n  \
                  bit q;\n  \
                  always_ff @(posedge clk) q <= 1'b1;\n  \
                  always_comb assert #0 (!clk || q) else $error(\"failed\");\n\
                  endmodule";
    let mut sim =
        celox::Simulator::from_sv_sources(vec![(source, Path::new("deferred.sv"))], "Top")
            .build()
            .unwrap();
    assert_eq!(summary(sim.drain_runtime_events()), vec![]);
    // The host write leaves the logic to settle before the flip-flops.
    let q = sim.signal("q");
    sim.set(q, 0u8);
    let clk = sim.event("clk");
    sim.tick(clk).unwrap();
    assert_eq!(summary(sim.drain_runtime_events()), vec![]);
}

/// The first drain of a simulation settles its combinational logic before
/// the slot matures, and returns what failed.
#[test]
fn a_first_drain_returns_actions_of_the_initial_settle() {
    let source = "module Top;\n  bit a;\n  \
                  d: assert #0 (a) else $error(\"d\");\n\
                  endmodule";
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("deferred.sv"))], "Top")
            .build()
            .unwrap();
    assert_eq!(
        summary(sim.drain_runtime_events()),
        vec![("d".to_owned(), Some(AssertionDeferral::Observed))]
    );
}

#[test]
fn deferred_actions_are_one_display_or_severity_task_call() {
    let source = "module Top; bit a;\n  \
                  initial assert #0 (a) else begin $error(\"x\"); $error(\"y\"); end\n\
                  endmodule";
    let error = celox::Simulation::from_sv_sources(vec![(source, Path::new("deferred.sv"))], "Top")
        .build()
        .unwrap_err();
    assert!(
        error.to_string().contains("deferred assertion action"),
        "{error}"
    );
}
