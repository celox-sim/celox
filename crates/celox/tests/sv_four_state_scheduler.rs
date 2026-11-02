#![cfg(feature = "systemverilog")]
//! The compiled scheduler of a four-state design: event levels are 0, 1, x
//! or z, and a change between them is an edge as IEEE 1800-2023 Table 9-2
//! has it (9.4.2), for clocks, asynchronous resets, clocks derived from
//! them and waits. Every step is checked against the reference loop.
use std::path::Path;

use num_bigint::BigUint;

/// `compiled` when the backend runs the compiled scheduler; the
/// interpreter runs every slot in the reference loop.
macro_rules! four_state_scheduler {
    ($module:ident, $backend:ident, $compiled:expr) => {
        four_state_scheduler!($module, $backend, $compiled, false);
    };
    ($module:ident, $backend:ident, $compiled:expr, $partitioned:expr) => {
        mod $module {
            use super::*;

            fn simulation(body: &str) -> celox::Simulation<impl celox::SimBackend> {
                let source = format!("`timescale 1ns/1ns\nmodule Top{body}\nendmodule");
                let builder = celox::Simulation::from_sv_sources(
                    vec![(source.as_str(), Path::new("four_state.sv"))],
                    "Top",
                )
                .four_state(true)
                .scheduler_mode(celox::SchedulerMode::Differential);
                let builder = if $partitioned {
                    builder
                        .settle_partition(celox::SettlePartition::Always)
                        .settle_check(true)
                } else {
                    builder
                };
                builder.$backend().unwrap()
            }

            /// Run `sim` until `time`, every step checked, with every kernel
            /// but the declaration initializers, which run at time zero, in
            /// the compiled scheduler's slots.
            fn run<B: celox::SimBackend>(sim: &mut celox::Simulation<B>, time: u64) {
                sim.run_until(time).unwrap();
                let stats = sim.scheduler_stats();
                assert_eq!(stats.unchecked, None);
                assert!(stats.checked_steps > 0);
                assert_eq!(stats.compiled_slots > 0, $compiled, "{stats:?}");
                assert!(
                    stats.kernels.iter().all(|kernel| match kernel {
                        celox::KernelSchedule::Compiled => true,
                        celox::KernelSchedule::Reference(reason) => {
                            reason == "a declaration initializer"
                        }
                    }),
                    "kernels: {:?}",
                    stats.kernels
                );
            }

            fn value<B: celox::SimBackend>(
                sim: &mut celox::Simulation<B>,
                name: &str,
            ) -> (u64, u64) {
                let signal = sim.signal(name);
                let (value, mask) = sim.get_four_state(signal);
                let digit = |number: BigUint| number.iter_u64_digits().next().unwrap_or(0);
                (digit(value), digit(mask))
            }

            /// A clock that moves through 0, 1, x and z: rises leave 0 or
            /// reach 1, falls leave 1 or reach 0, and x to z is a change but
            /// neither (Table 9-2).
            #[test]
            fn clock_levels_follow_table_9_2() {
                let mut sim = simulation(
                    r#";
  logic clk;
  logic [7:0] rises = 0, falls = 0, changes = 0, waited = 0;
  initial begin
    #5 clk = 1'bx; #5 clk = 1; #5 clk = 1'bz; #5 clk = 0; #5 clk = 1'bx;
    #5 clk = 0; #5 clk = 1; #5 clk = 1'bz; #5 clk = 1'bx; #5 clk = 1;
  end
  always_ff @(posedge clk) rises <= rises + 1;
  always_ff @(negedge clk) falls <= falls + 1;
  always @(clk) changes = changes + 1;
  always @(posedge clk) waited <= waited + 1;"#,
                );
                run(&mut sim, 60);
                assert_eq!(value(&mut sim, "rises"), (4, 0));
                assert_eq!(value(&mut sim, "falls"), (4, 0));
                assert_eq!(value(&mut sim, "changes"), (9, 0));
                assert_eq!(value(&mut sim, "waited"), (4, 0));
            }

            /// Asynchronous resets that become x or z: a move to x is an
            /// edge of either polarity, and the flip-flop's condition is
            /// then unknown.
            #[test]
            fn asynchronous_resets_with_unknown_levels() {
                let mut sim = simulation(
                    r#";
  logic clk = 0;
  logic rst_n, rst;
  logic [7:0] low = 5, high = 5;
  always #5 clk = ~clk;
  initial begin
    #12 rst_n = 0; rst = 1; #10 rst_n = 1; rst = 0; #20 rst_n = 1'bx; rst = 1'bx;
    #10 rst_n = 1; rst = 0; #10 rst_n = 1'bz; rst = 1'bz; #10 rst_n = 0; rst = 1;
    #10 rst_n = 1; rst = 0;
  end
  always_ff @(posedge clk or negedge rst_n) if (!rst_n) low <= 0; else low <= low + 1;
  always_ff @(posedge clk or posedge rst) if (rst) high <= 0; else high <= high + 1;"#,
                );
                run(&mut sim, 120);
            }

            /// IEEE 1800-2023 9.4.2.1: any changed OR term wakes the
            /// process. Changes while its body is delayed do not carry into
            /// its next wait. Include late terms and mask-only transitions.
            #[test]
            fn large_change_wait_recaptures_after_delay() {
                let mut sim = simulation(
                    r#";
  logic [7:0] a = 0, b = 0, c = 0, d = 0, e = 0, f = 0, g = 0, h = 0;
  integer hits = 0;
  always @(a or b or c or d or e or f or g or h) begin
    hits = hits + 1;
    #3;
  end
  initial begin
    #2 a = 8'h80;
    #1 h = 'x;
    #1 h = 'z;
    #2 b = 1;
    #1 a = 0;
    #1 h = 0;
    #2 h = 'x;
    #4 h = 'z;
    #4 h = 0;
    #4 h = 0;
    #2 c = 1;
    #4 g = 1;
    #4 d = 1;
    #4 e = 1;
    #4 f = 1;
  end"#,
                );
                run(&mut sim, 45);
                assert_eq!(value(&mut sim, "hits"), (10, 0));
            }

            /// Clocks computed from a clock and an enable that is x or z
            /// for a while, and a clock divided by a flip-flop that starts
            /// at x.
            #[test]
            fn derived_clocks_with_unknown_levels() {
                let mut sim = simulation(
                    r#";
  logic clk = 0;
  logic en, rst_n;
  logic div;
  logic [7:0] gated = 0, gated_falls = 0, inverted = 0, divided = 0;
  always #5 clk = ~clk;
  initial begin
    #12 en = 1; #20 en = 1'bx; #20 en = 0; #20 en = 1'bz; #20 en = 1;
  end
  initial begin #47 rst_n = 0; #10 rst_n = 1; end
  wire gclk = clk & en;
  wire nclk = ~clk;
  always_ff @(posedge gclk) gated <= gated + 1;
  always_ff @(negedge gclk) gated_falls <= gated_falls + 1;
  always_ff @(posedge nclk) inverted <= inverted + 1;
  always_ff @(posedge clk or negedge rst_n) if (!rst_n) div <= 0; else div <= ~div;
  always_ff @(posedge div) divided <= divided + 1;"#,
                );
                run(&mut sim, 140);
            }

            /// Waits on a signal a delay process moves through x and z.
            #[test]
            fn waits_on_unknown_levels() {
                let mut sim = simulation(
                    r#";
  logic s;
  logic [7:0] rises = 0, falls = 0, changes = 0;
  initial begin
    #3 s = 1'bz; #3 s = 1'bx; #3 s = 0; #3 s = 1'bz; #3 s = 1; #3 s = 1'bx;
    #3 s = 1; #3 s = 0; #3 s = 1'bx; #3 s = 1'bx; #3 s = 0;
  end
  always @(posedge s) rises = rises + 1;
  always @(negedge s) falls = falls + 1;
  always @(s) changes = changes + 1;"#,
                );
                run(&mut sim, 40);
                // z, x, 0, z, 1, x, 1, 0, x, x, 0 from x.
                assert_eq!(value(&mut sim, "rises"), (4, 0));
                assert_eq!(value(&mut sim, "falls"), (4, 0));
                assert_eq!(value(&mut sim, "changes"), (10, 0));
            }

            /// A host that writes x or z between runs, to a reset and to a
            /// clock enable, and a host clock that starts at x.
            #[test]
            fn host_writes_and_clocks_with_unknown_levels() {
                let mut sim = simulation(
                    r#"(input logic clk, input logic rst_n, input logic en);
  logic [7:0] count = 0, gated = 0;
  logic [7:0] ticks = 0;
  logic tick = 0;
  always #3 tick = ~tick;
  always_ff @(posedge tick) ticks <= ticks + 1;
  wire gclk = clk & en;
  always_ff @(posedge clk or negedge rst_n) if (!rst_n) count <= 0; else count <= count + 1;
  always_ff @(posedge gclk) gated <= gated + 1;"#,
                );
                sim.add_clock("clk", 10, 5);
                let rst_n = sim.signal("rst_n");
                let en = sim.signal("en");
                let writes: [(u64, u8, u8, u8, u8); 6] = [
                    (17, 0, 0, 1, 1),
                    (33, 1, 0, 1, 0),
                    (48, 1, 1, 0, 1),
                    (62, 1, 0, 1, 0),
                    (80, 0, 1, 0, 1),
                    (95, 1, 0, 1, 0),
                ];
                for (time, rst_value, rst_mask, en_value, en_mask) in writes {
                    run(&mut sim, time);
                    sim.modify(|io| {
                        io.set_four_state(rst_n, rst_value.into(), rst_mask.into());
                        io.set_four_state(en, en_value.into(), en_mask.into());
                    })
                    .unwrap();
                }
                run(&mut sim, 120);
            }
        }
    };
}

four_state_scheduler!(interpreter, build_interpreter, false);
#[cfg(feature = "cranelift")]
four_state_scheduler!(cranelift, build, true);
#[cfg(target_arch = "x86_64")]
four_state_scheduler!(native, build_native, true);
#[cfg(feature = "wasm-runtime")]
four_state_scheduler!(wasm, build_wasm, true);

// Exercise the same event-order and X/Z contracts with selective settling.
four_state_scheduler!(partitioned_interpreter, build_interpreter, false, true);
#[cfg(feature = "cranelift")]
four_state_scheduler!(partitioned_cranelift, build, true, true);
#[cfg(target_arch = "x86_64")]
four_state_scheduler!(partitioned_native, build_native, true, true);
#[cfg(feature = "wasm-runtime")]
four_state_scheduler!(partitioned_wasm, build_wasm, true, true);
