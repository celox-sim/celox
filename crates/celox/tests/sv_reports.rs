#![cfg(feature = "systemverilog")]
//! Severity tasks report their severity, the file and line of the call and
//! the scope that issued them (IEEE 1800-2023 20.10). A report in an action
//! block of an assertion names that assertion (16.3).
use celox::{
    AssertionDeferral, AssertionDirective, AssertionOutcome, AssertionSite, Report, ReportLocation,
    Severity,
};
use std::path::Path;

/// A report of `reports.sv` without a time.
fn report(
    severity: Severity,
    message: &str,
    line: usize,
    assertion: Option<(Option<&str>, AssertionDirective, AssertionOutcome)>,
) -> Report {
    Report {
        severity,
        message: message.as_bytes().to_vec(),
        time: None,
        scope: Some("Top".to_owned()),
        location: Some(ReportLocation {
            file: "reports.sv".to_owned(),
            line,
        }),
        assertion: assertion.map(|(label, directive, outcome)| AssertionSite {
            label: label.map(str::to_owned),
            directive,
            deferral: AssertionDeferral::None,
            outcome,
        }),
    }
}

/// `reports`, issued at `time`.
fn at(time: u64, reports: Vec<Report>) -> Vec<Report> {
    reports
        .into_iter()
        .map(|report| Report {
            time: Some(time),
            ..report
        })
        .collect()
}

macro_rules! reports {
    ($module:ident, $backend:ident) => {
        mod $module {
            use super::*;

            /// The reports of `body` in `module Top` until time 10, drained
            /// after the run.
            fn reports(body: &str) -> Vec<Report> {
                let source = format!("`timescale 1ns/1ns\nmodule Top;\n{body}\nendmodule");
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source.as_str(), Path::new("reports.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap();
                sim.run_until(10).unwrap();
                sim.drain_runtime_events()
                    .into_iter()
                    .filter_map(|event| match event {
                        celox::RuntimeEvent::Report(report) => Some(report),
                        _ => None,
                    })
                    .collect()
            }

            #[test]
            fn severity_tasks_report_their_severity_location_and_scope() {
                assert_eq!(
                    reports(
                        "  initial begin\n    \
                         #1;\n    \
                         $info(\"i %0d\", 1);\n    \
                         $warning(\"w\");\n    \
                         $error(\"e\");\n  \
                         end"
                    ),
                    at(
                        1,
                        vec![
                            report(Severity::Info, "i 1", 5, None),
                            report(Severity::Warning, "w", 6, None),
                            report(Severity::Error, "e", 7, None),
                        ]
                    )
                );
            }

            #[test]
            fn action_blocks_report_their_assertion() {
                use AssertionDirective::{Assert, Assume, Cover};
                use AssertionOutcome::{Fail, Pass};
                assert_eq!(
                    reports(
                        "  bit a;\n  \
                         initial begin\n    \
                         #1;\n    \
                         a1: assert (a) else $warning(\"a low\");\n    \
                         a2: assert (a);\n    \
                         c1: cover (!a) $info(\"covered\");\n    \
                         assume (!a) $info(\"assumed\");\n  \
                         end"
                    ),
                    at(1, vec![
                        report(
                            Severity::Warning,
                            "a low",
                            6,
                            Some((Some("a1"), Assert, Fail))
                        ),
                        report(
                            Severity::Error,
                            "assert failed",
                            7,
                            Some((Some("a2"), Assert, Fail))
                        ),
                        report(
                            Severity::Info,
                            "covered",
                            8,
                            Some((Some("c1"), Cover, Pass))
                        ),
                        report(Severity::Info, "assumed", 9, Some((None, Assume, Pass))),
                    ])
                );
            }

            /// A report of an assertion in the action block of another
            /// names the inner one.
            #[test]
            fn nested_assertions_keep_their_attribution() {
                use AssertionDirective::Assert;
                use AssertionOutcome::Fail;
                assert_eq!(
                    reports(
                        "  bit a;\n  \
                         initial begin\n    \
                         #1;\n    \
                         outer: assert (1) begin\n      \
                         inner: assert (a);\n    \
                         end\n  \
                         end"
                    ),
                    at(
                        1,
                        vec![report(
                            Severity::Error,
                            "assert failed",
                            7,
                            Some((Some("inner"), Assert, Fail))
                        )]
                    )
                );
            }

            #[test]
            fn always_comb_reports_keep_their_location() {
                assert_eq!(
                    reports("  bit a;\n  always_comb if (!a) $warning(\"low\");"),
                    at(0, vec![report(Severity::Warning, "low", 4, None)])
                );
            }

            /// A report keeps the time of its step however late the host
            /// drains it (IEEE 1800-2023 20.10).
            #[test]
            fn reports_keep_their_time_when_drained_late() {
                let times: Vec<_> = reports(
                    "  initial begin\n    \
                     #1 $warning(\"w\");\n    \
                     #3 $error(\"e\");\n  \
                     end",
                )
                .into_iter()
                .map(|report| report.time)
                .collect();
                assert_eq!(times, vec![Some(1), Some(4)]);
            }

            /// With `failOn`, a run stops after the time step of the first
            /// report of at least that severity, the next run continues,
            /// and every report keeps the time it was issued at.
            #[test]
            fn fail_on_stops_runs_at_the_time_of_a_report() {
                let source = "`timescale 1ns/1ns\nmodule Top;\n  initial begin\n    \
                              #1 $warning(\"w\");\n    \
                              #1 $error(\"e\");\n    \
                              #1 $display(\"after\");\n  \
                              end\nendmodule";
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("reports.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap();
                sim.set_fail_on(Some(Severity::Error));
                sim.run_until(10).unwrap();
                assert_eq!(sim.time(), 2);
                let failure = sim.failure().unwrap();
                assert_eq!(failure.message, b"e");
                assert_eq!(failure.time, Some(2));
                let times: Vec<_> = sim
                    .drain_runtime_events()
                    .into_iter()
                    .map(|event| match event {
                        celox::RuntimeEvent::Report(report) => (report.severity, report.time),
                        event => panic!("unexpected event {event:?}"),
                    })
                    .collect();
                assert_eq!(
                    times,
                    vec![(Severity::Warning, Some(1)), (Severity::Error, Some(2))]
                );
                sim.run_until(10).unwrap();
                assert!(sim.failure().is_none());
                assert_eq!(sim.time(), 10);
                assert!(matches!(
                    sim.drain_runtime_events().as_slice(),
                    [celox::RuntimeEvent::Display { message }] if message == b"after"
                ));
            }

            /// A report caused by a host write stops the run in the slot
            /// that settles the write, before later slots run.
            #[test]
            fn fail_on_stops_at_reports_of_host_writes() {
                let source = "`timescale 1ns/1ns\nmodule Top(input logic a);\n  \
                              always @(posedge a) $error(\"e\");\n  \
                              initial #5 $display(\"later\");\nendmodule";
                for step in [false, true] {
                    let mut sim = celox::Simulation::from_sv_sources(
                        vec![(source, Path::new("reports.sv"))],
                        "Top",
                    )
                    .$backend()
                    .unwrap();
                    sim.set_fail_on(Some(Severity::Error));
                    sim.run_until(2).unwrap();
                    let a = sim.signal("a");
                    sim.modify(|io| io.set(a, 1u8)).unwrap();
                    if step {
                        assert_eq!(sim.step().unwrap(), Some(2));
                    } else {
                        sim.run_until(10).unwrap();
                    }
                    assert_eq!(sim.time(), 2);
                    assert_eq!(sim.failure().unwrap().time, Some(2));
                    let events = sim.drain_runtime_events();
                    assert!(
                        matches!(
                            events.as_slice(),
                            [celox::RuntimeEvent::Report(report)] if report.message == b"e"
                        ),
                        "{events:?}"
                    );
                }
            }

            /// A report of combinational logic that a host write leaves to
            /// settle, with no step at the current time, stops the run there,
            /// by `run_until` or by `step`, also before later work.
            #[test]
            fn fail_on_stops_at_reports_of_settles_without_steps() {
                for later in ["", "\n  initial #5 $display(\"later\");"] {
                let source = format!("`timescale 1ns/1ns\nmodule Top(input logic a);\n  \
                              always_comb if (a) $error(\"e\");{later}\nendmodule");
                for step in [false, true] {
                    let mut sim = celox::Simulation::from_sv_sources(
                        vec![(source.as_str(), Path::new("reports.sv"))],
                        "Top",
                    )
                    .$backend()
                    .unwrap();
                    sim.set_fail_on(Some(Severity::Error));
                    sim.run_until(2).unwrap();
                    let a = sim.signal("a");
                    sim.modify(|io| io.set(a, 1u8)).unwrap();
                    if step {
                        sim.step().unwrap();
                    } else {
                        sim.run_until(10).unwrap();
                    }
                    assert_eq!(sim.time(), 2, "step: {step}");
                    let failure = sim.failure().expect("a failure");
                    assert_eq!(failure.message, b"e");
                    assert_eq!(failure.time, Some(2));
                    sim.run_until(10).unwrap();
                    assert!(sim.failure().is_none());
                    assert_eq!(sim.time(), 10);
                }
                }
            }

            /// Events lost to a full event buffer may have been failures.
            #[test]
            fn fail_on_stops_at_missed_events() {
                let source = "`timescale 1ns/1ns\nmodule Top;\n  initial begin\n    \
                              #1;\n    \
                              for (int i = 0; i < 2000; i++) $display(\"x\");\n    \
                              #1 $display(\"after\");\n  \
                              end\nendmodule";
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("reports.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap();
                sim.set_fail_on(Some(Severity::Error));
                sim.run_until(10).unwrap();
                assert_eq!(sim.time(), 1);
                let failure = sim.failure().unwrap();
                assert_eq!(failure.severity, Severity::Error);
                assert!(failure.message.starts_with(b"missed "), "{failure:?}");
            }

            /// The initial settle of combinational logic is at time zero,
            /// also when the first work is a clock edge at a later time,
            /// reached by `run_until` or by `step`.
            #[test]
            fn initial_reports_before_a_later_first_slot_are_at_time_zero() {
                let source = "`timescale 1ns/1ns\nmodule Top(input bit clk);\n  \
                              bit q, a;\n  \
                              always_ff @(posedge clk) q <= !q;\n  \
                              always_comb if (!a) $warning(\"low\");\nendmodule";
                for (step, watch) in [(false, false), (true, false), (false, true), (true, true)] {
                    let mut sim = celox::Simulation::from_sv_sources(
                        vec![(source, Path::new("reports.sv"))],
                        "Top",
                    )
                    .$backend()
                    .unwrap();
                    sim.add_clock("clk", 2, 5);
                    // Watching reports keeps the issued time too.
                    if watch {
                        sim.set_fail_on(Some(Severity::Warning));
                    }
                    if step {
                        sim.step().unwrap();
                    } else {
                        sim.run_until(10).unwrap();
                    }
                    let times: Vec<_> = sim
                        .drain_runtime_events()
                        .into_iter()
                        .map(|event| match event {
                            celox::RuntimeEvent::Report(report) => report.time,
                            event => panic!("unexpected event {event:?}"),
                        })
                        .collect();
                    assert_eq!(times, vec![Some(0)], "step: {step}, watch: {watch}");
                    if watch {
                        assert_eq!(sim.failure().and_then(|report| report.time), Some(0));
                    }
                }
            }

            /// A leave-time settle can terminate before any scheduled slot.
            /// Exercise the host entry points, report watching, and final blocks.
            #[test]
            fn initial_report_finish_preserves_time_and_runs_final() {
                for clock in [false, true] {
                    for watch in [false, true] {
                        for entry in 0..4 {
                            let source = r#"`timescale 1ns/1ns
module Top(input bit clk);
  bit a;
  int ticks, final_calls;
  always_ff @(posedge clk) ticks <= ticks + 1;
  always_comb if (!a) begin $warning("initial"); $finish; end
  final final_calls++;
endmodule"#;
                            let mut sim = celox::Simulation::from_sv_sources(
                                vec![(source, Path::new("reports.sv"))], "Top",
                            ).$backend().unwrap();
                            if clock { sim.add_clock("clk", 2, 5); }
                            if watch { sim.set_fail_on(Some(Severity::Error)); }
                            match entry {
                                0 => sim.run_until(10).unwrap(),
                                1 => { sim.step().unwrap(); }
                                2 => sim.advance_to(10).unwrap(),
                                _ => { sim.run_until_bounded(10, 10).unwrap(); }
                            }
                            assert_eq!(sim.time(), 0, "clock={clock}, watch={watch}, entry={entry}");
                            assert!(sim.is_finished(), "clock={clock}, watch={watch}, entry={entry}");
                            let final_calls = sim.signal("final_calls");
                            let ticks = sim.signal("ticks");
                            assert_eq!(sim.get(final_calls), 1u32.into());
                            assert_eq!(sim.get(ticks), 0u32.into());
                            let times: Vec<_> = sim.drain_runtime_events().into_iter()
                                .filter_map(|event| match event {
                                    celox::RuntimeEvent::Report(report) => Some(report.time),
                                    _ => None,
                                }).collect();
                            assert_eq!(times, vec![Some(0)]);
                        }
                    }
                }
            }

            #[test]
            fn leave_time_final_can_stop_and_resume_at_the_same_time() {
                for watch in [false, true] {
                    for entry in 0..4 {
                        let source = r#"`timescale 1ns/1ns
module Top;
  bit a;
  int final_calls;
  always_comb if (!a) begin $warning("initial"); $finish; end
  final begin final_calls++; $stop; final_calls++; end
endmodule"#;
                        let mut sim = celox::Simulation::from_sv_sources(
                            vec![(source, Path::new("reports.sv"))], "Top",
                        ).$backend().unwrap();
                        if watch { sim.set_fail_on(Some(Severity::Error)); }
                        for resumed in [false, true] {
                            match entry {
                                0 => sim.run_until(10).unwrap(),
                                1 => { sim.step().unwrap(); }
                                2 => sim.advance_to(10).unwrap(),
                                _ => { sim.run_until_bounded(10, 10).unwrap(); }
                            }
                            assert_eq!(sim.time(), 0);
                            assert_eq!(sim.is_stopped(), !resumed, "watch={watch}, entry={entry}");
                            assert_eq!(sim.is_finished(), resumed);
                            let final_calls = sim.signal("final_calls");
                            assert_eq!(sim.get(final_calls), (if resumed { 2u32 } else { 1u32 }).into());
                        }
                    }
                }
            }

            #[test]
            fn initial_fatal_runs_final_before_returning_the_error() {
                for clock in [false, true] {
                    let source = r#"`timescale 1ns/1ns
module Top(input bit clk);
  bit a, q;
  int final_calls;
  always_ff @(posedge clk) q <= !q;
  always_comb if (!a) $fatal(1, "initial");
  final final_calls++;
endmodule"#;
                    let mut sim = celox::Simulation::from_sv_sources(
                        vec![(source, Path::new("reports.sv"))], "Top",
                    ).$backend().unwrap();
                    if clock { sim.add_clock("clk", 2, 5); }
                    assert!(matches!(sim.run_until(10), Err(celox::RuntimeErrorCode::Fatal { .. })));
                    assert_eq!(sim.time(), 0);
                    assert!(sim.is_finished());
                    let final_calls = sim.signal("final_calls");
                    assert_eq!(sim.get(final_calls), 1u32.into());
                }
            }

            #[test]
            fn line_directives_set_the_reported_location() {
                let reports = reports("`line 40 \"other.sv\" 0\n  initial $info(\"moved\");");
                assert_eq!(
                    reports[0].location,
                    Some(ReportLocation {
                        file: "other.sv".to_owned(),
                        line: 40,
                    })
                );
            }
        }
    };
}

reports!(interpreter, build_interpreter);
#[cfg(feature = "cranelift")]
reports!(cranelift, build_cranelift);
#[cfg(target_arch = "x86_64")]
reports!(native, build_native);
reports!(wasm, build_wasm);

#[test]
fn reports_render_their_location_time_and_scope() {
    let mut report = report(
        Severity::Warning,
        "a low",
        6,
        Some((
            Some("a1"),
            AssertionDirective::Assert,
            AssertionOutcome::Fail,
        )),
    );
    report.time = Some(10);
    assert_eq!(
        String::from_utf8(report.render()).unwrap(),
        "Warning: reports.sv:6: a low\n  Time: 10  Scope: Top.a1"
    );
    assert_eq!(
        String::from_utf8(Report::new(Severity::Info, b"bare".to_vec()).render()).unwrap(),
        "Info: bare"
    );
}
