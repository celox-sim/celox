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

macro_rules! reports {
    ($module:ident, $backend:ident) => {
        mod $module {
            use super::*;

            /// The reports of `body` in `module Top` until time 1.
            fn reports(body: &str) -> Vec<Report> {
                let source = format!("`timescale 1ns/1ns\nmodule Top;\n{body}\nendmodule");
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source.as_str(), Path::new("reports.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap();
                sim.run_until(1).unwrap();
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
                    vec![
                        report(Severity::Info, "i 1", 5, None),
                        report(Severity::Warning, "w", 6, None),
                        report(Severity::Error, "e", 7, None),
                    ]
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
                    vec![
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
                    ]
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
                    vec![report(
                        Severity::Error,
                        "assert failed",
                        7,
                        Some((Some("inner"), Assert, Fail))
                    )]
                );
            }

            #[test]
            fn always_comb_reports_keep_their_location() {
                assert_eq!(
                    reports("  bit a;\n  always_comb if (!a) $warning(\"low\");"),
                    vec![report(Severity::Warning, "low", 4, None)]
                );
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
reports!(cranelift, build);
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
