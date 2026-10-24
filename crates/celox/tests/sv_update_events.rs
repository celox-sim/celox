#![cfg(feature = "systemverilog")]
//! Every change of a variable is an update event that schedules the
//! processes sensitive to it (IEEE 1800-2023 4.3, 4.5), also a change that
//! a later write of the same time slot undoes. Nonblocking updates are
//! performed one by one, in the order they were scheduled (10.4.2).
use std::path::Path;

/// The scheduler mode: `CELOX_SCHEDULER` set to `reference`, `compiled`,
/// `differential` or `differential-runs`, by default the library's.
#[allow(clippy::disallowed_methods)] // Opt-in test configuration is environment-driven.
fn scheduler_mode() -> celox::SchedulerMode {
    match std::env::var("CELOX_SCHEDULER").as_deref() {
        Ok("reference") => celox::SchedulerMode::Reference,
        Ok("compiled") => celox::SchedulerMode::Compiled,
        Ok("differential") => celox::SchedulerMode::Differential,
        Ok("differential-runs") => celox::SchedulerMode::DifferentialRuns,
        Ok(other) => panic!("unknown CELOX_SCHEDULER mode `{other}`"),
        Err(_) => celox::SchedulerMode::default(),
    }
}

macro_rules! update_events {
    ($module:ident, $backend:ident) => {
        mod $module {
            use super::*;

            fn simulation(body: &str) -> celox::Simulation<impl celox::SimBackend> {
                let source = format!(
                    "`timescale 1ns/1ns\nmodule Top;\n  bit a, c;\n  int count;\n{body}\nendmodule"
                );
                celox::Simulation::from_sv_sources(
                    vec![(source.as_str(), Path::new("update_events.sv"))],
                    "Top",
                )
                .scheduler_mode(scheduler_mode())
                .$backend()
                .unwrap()
            }

            /// The `$display`s of `sim` until `time`.
            fn displays(sim: &mut celox::Simulation<impl celox::SimBackend>, time: u64) -> usize {
                sim.run_until(time).unwrap();
                sim.drain_runtime_events()
                    .into_iter()
                    .filter(|event| matches!(event, celox::RuntimeEvent::Display { .. }))
                    .count()
            }

            /// The assertion failures of `sim` until `time`.
            fn failures(sim: &mut celox::Simulation<impl celox::SimBackend>, time: u64) -> usize {
                sim.run_until(time).unwrap();
                sim.drain_runtime_events()
                    .into_iter()
                    .filter(|event| matches!(event, celox::RuntimeEvent::AssertContinue { .. }))
                    .count()
            }

            fn value(sim: &mut celox::Simulation<impl celox::SimBackend>, name: &str) -> u64 {
                let signal = sim.signal(name);
                sim.get(signal).try_into().unwrap()
            }

            #[test]
            fn blocking_writes_that_restore_a_source_reexecute_always_comb() {
                let mut sim = simulation(
                    "  always_comb if (!a) $display(\"low\");\n  \
                     initial begin #1; a = 1; a = 0; end",
                );
                assert_eq!(displays(&mut sim, 0), 1);
                assert_eq!(displays(&mut sim, 1), 1);
            }

            #[test]
            fn kernel_nonblocking_updates_that_restore_a_source_reexecute_always_comb() {
                let mut sim = simulation(
                    "  always_comb if (!a) $display(\"low\");\n  \
                     initial begin #1; a <= 1; a <= 0; end",
                );
                assert_eq!(displays(&mut sim, 0), 1);
                assert_eq!(displays(&mut sim, 1), 1);
            }

            #[test]
            fn flip_flop_nonblocking_updates_that_restore_a_source_reexecute_always_comb() {
                let mut sim = simulation(
                    "  always_ff @(posedge c) begin a <= 1; a <= 0; end\n  \
                     always_comb if (!a) $display(\"low\");\n  \
                     initial #1 c = 1;",
                );
                assert_eq!(displays(&mut sim, 0), 1);
                assert_eq!(displays(&mut sim, 1), 1);
            }

            #[test]
            fn immediate_assertions_in_always_comb_report_each_reexecution() {
                let mut sim = simulation(
                    "  always_comb assert (a) else $error(\"a low\");\n  \
                     initial begin #1; a = 1; a = 0; end",
                );
                assert_eq!(failures(&mut sim, 0), 1);
                assert_eq!(failures(&mut sim, 1), 1);
            }

            #[test]
            fn flip_flop_nonblocking_updates_that_restore_a_signal_are_events() {
                let mut sim = simulation(
                    "  always @(posedge c) begin a <= 1; a <= 0; end\n  \
                     initial begin @(a) count = count + 1; end\n  \
                     always @(posedge a) count = count + 10;\n  \
                     initial #1 c = 1;",
                );
                sim.run_until(1).unwrap();
                assert_eq!(value(&mut sim, "count"), 11);
                assert_eq!(value(&mut sim, "a"), 0);
            }

            #[test]
            fn flip_flop_blocking_writes_that_restore_a_signal_are_events() {
                let mut sim = simulation(
                    "  always @(posedge c) begin a = 1; a = 0; end\n  \
                     initial begin @(a) count = count + 1; end\n  \
                     initial #1 c = 1;",
                );
                sim.run_until(1).unwrap();
                assert_eq!(value(&mut sim, "count"), 1);
            }

            #[test]
            fn updates_that_keep_the_value_are_no_events() {
                let mut sim = simulation(
                    "  always_ff @(posedge c) begin a <= 0; a <= 0; end\n  \
                     always_comb if (!a) $display(\"low\");\n  \
                     initial begin @(a) count = count + 1; end\n  \
                     initial begin #1; c = 1; #1; a = 0; a = 0; end",
                );
                assert_eq!(displays(&mut sim, 0), 1);
                assert_eq!(displays(&mut sim, 2), 0);
                assert_eq!(value(&mut sim, "count"), 0);
            }

            #[test]
            fn updates_in_exclusive_branches_are_one_event() {
                let mut sim = simulation(
                    "  always_ff @(posedge c) if (count == 0) a <= 1; else a <= 0;\n  \
                     always_comb if (a) $display(\"high\");\n  \
                     initial #1 c = 1;",
                );
                assert_eq!(displays(&mut sim, 0), 0);
                assert_eq!(displays(&mut sim, 1), 1);
            }

            #[test]
            fn flip_flop_pulses_trigger_domains_without_processes() {
                // No process: the batch round checks the counted signal.
                let source = "module Top(input bit c);\n  \
                     bit d;\n  int count;\n  \
                     always_ff @(posedge c) begin d <= 1; d <= 0; end\n  \
                     always_ff @(posedge d) count <= count + 1;\n\
                     endmodule";
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("pulse.sv"))],
                    "Top",
                )
                .scheduler_mode(scheduler_mode())
                .$backend()
                .unwrap();
                sim.schedule("c", 1, 1).unwrap();
                sim.schedule("c", 2, 0).unwrap();
                sim.schedule("c", 3, 1).unwrap();
                sim.run_until(3).unwrap();
                assert_eq!(value(&mut sim, "count"), 2);
                assert_eq!(value(&mut sim, "d"), 0);
            }
        }
    };
}

update_events!(interpreter, build_interpreter);
update_events!(cranelift, build);
#[cfg(target_arch = "x86_64")]
update_events!(native, build_native);
update_events!(wasm, build_wasm);
