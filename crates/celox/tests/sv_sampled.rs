#![cfg(feature = "systemverilog")]
//! `$sampled(e)` is the value of `e` in the Preponed region of the time
//! slot, the value at the end of the previous slot, and before the first
//! slot the declared or default value (IEEE 1800-2023 4.4.2.1, 16.5.1,
//! 16.9.3).
use std::path::Path;

macro_rules! sampled {
    ($module:ident, $backend:ident) => {
        mod $module {
            use super::*;

            fn simulation(body: &str) -> celox::Simulation<impl celox::SimBackend> {
                let source = format!(
                    "`timescale 1ns/1ns\nmodule Top;\n  bit clk, a, b, r, s, x, y, z;\n  int v;\n{body}\nendmodule"
                );
                celox::Simulation::from_sv_sources(
                    vec![(source.as_str(), Path::new("sampled.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap()
            }

            fn value(sim: &mut celox::Simulation<impl celox::SimBackend>, name: &str) -> u64 {
                let signal = sim.signal(name);
                sim.get(signal).try_into().unwrap()
            }

            /// A clock edge and a blocking write in one slot: the register
            /// sees the new value, `$sampled` the value from before the slot.
            #[test]
            fn a_flip_flop_samples_the_value_before_the_slot() {
                let mut sim = simulation(
                    "  always @(posedge clk) begin r <= $sampled(a); s <= a; end\n  \
                     initial begin #1 a = 1; clk = 1; #1 clk = 0; #1 clk = 1; end",
                );
                sim.run_until(1).unwrap();
                assert_eq!((value(&mut sim, "r"), value(&mut sim, "s")), (0, 1));
                sim.run_until(3).unwrap();
                assert_eq!((value(&mut sim, "r"), value(&mut sim, "s")), (1, 1));
            }

            /// Returning to the host does not advance the Preponed baseline:
            /// the host may schedule more work at the same timestamp.
            #[test]
            fn host_resumptions_keep_the_current_timestamp_sample() {
                for mode in [celox::SchedulerMode::Reference, celox::SchedulerMode::Compiled] {
                    for watch in [false, true] {
                        for entry in 0..4 {
                            let source = r#"`timescale 1ns/1ns
module Top(input bit clk, input bit [7:0] host);
  bit [7:0] a, r;
  always @(posedge clk) r <= $sampled(a + host);
  initial #1 a = 1;
endmodule"#;
                            let mut sim = celox::Simulation::from_sv_sources(
                                vec![(source, Path::new("sampled.sv"))], "Top",
                            ).scheduler_mode(mode).$backend().unwrap();
                            if watch { sim.set_fail_on(Some(celox::Severity::Error)); }
                            sim.run_until(0).unwrap();
                            match entry {
                                0 => sim.run_until(1).unwrap(),
                                1 => { sim.run_until_bounded(1, 10).unwrap(); }
                                2 => { assert_eq!(sim.step().unwrap(), Some(1)); }
                                _ => { sim.advance_to(1).unwrap(); sim.run_until(1).unwrap(); }
                            }
                            assert_eq!(value(&mut sim, "a"), 1);
                            sim.schedule("clk", 1, 1).unwrap();
                            sim.run_until(1).unwrap();
                            assert_eq!(value(&mut sim, "r"), 0, "{mode:?}, watch={watch}, entry={entry}");

                            // Host writes also belong to time 1, not a new slot.
                            let host = sim.signal("host");
                            sim.modify(|io| io.set(host, 2u8)).unwrap();
                            sim.schedule("clk", 1, 0).unwrap();
                            sim.run_until(1).unwrap();
                            sim.schedule("clk", 1, 1).unwrap();
                            sim.run_until(1).unwrap();
                            assert_eq!(value(&mut sim, "r"), 0);

                            // Once time advances, the updated values are sampled.
                            sim.schedule("clk", 2, 0).unwrap();
                            sim.schedule("clk", 3, 1).unwrap();
                            sim.run_until(3).unwrap();
                            assert_eq!(value(&mut sim, "r"), 3);
                            if matches!(mode, celox::SchedulerMode::Compiled)
                                && !watch && entry == 0
                                && matches!(stringify!($backend), "build" | "build_cranelift") {
                                assert!(sim.scheduler_stats().compiled_slots > 0);
                            }
                        }
                    }
                }
            }

            /// In the first slot the sampled value is the declared value,
            /// in a later slot the value at the end of the previous one.
            #[test]
            fn time_zero_samples_the_declared_value() {
                let mut sim = simulation(
                    "  bit d = 1;\n  \
                     initial begin\n    \
                     x = $sampled(b) ^ $sampled(d); b = 1; d = 0; #0 y = $sampled(b) ^ $sampled(d);\n    \
                     #1 z = $sampled(b) ^ $sampled(d);\n  \
                     end",
                );
                sim.run_until(1).unwrap();
                assert_eq!(
                    (value(&mut sim, "x"), value(&mut sim, "y"), value(&mut sim, "z")),
                    (1, 1, 1)
                );
            }

            /// A real operand keeps its type.
            #[test]
            fn real_operands_keep_their_type() {
                let mut sim = simulation(
                    "  real d = 1.25;\n  real rr;\n  initial begin rr = $sampled(d); $display(\"%0.2f\", rr); end",
                );
                sim.run_until(0).unwrap();
                assert!(matches!(
                    sim.drain_runtime_events().as_slice(),
                    [celox::RuntimeEvent::Display { message }] if message == b"1.25"
                ));
            }

            /// A sampled expression selects, as `$sampled(i + 1)`.
            #[test]
            fn sampled_expressions_select() {
                let mut sim = simulation(
                    "  bit [7:0] data [0:3] = '{10, 11, 12, 13};\n  bit [1:0] i = 1;\n  bit [7:0] q;\n  \
                     always @(posedge clk) q <= data[$sampled(i + 1)];\n  \
                     initial begin #1 clk = 1; i = 0; end",
                );
                sim.run_until(1).unwrap();
                assert_eq!(value(&mut sim, "q"), 12);
            }

            /// A call in a sampled select index reads the sampled values too.
            #[test]
            fn sampled_calls_select() {
                let mut sim = simulation(
                    "  int calls; function automatic bit [1:0] f(bit [1:0] k); calls++; return k; endfunction\n  \
                     bit [7:0] data [0:3] = '{10, 11, 12, 13};\n  bit [1:0] i = 2;\n  bit [7:0] q;\n  bit [3:0] w;\n  \
                     always @(posedge clk) begin q <= data[$sampled(f(i))]; w[$sampled(f(i))] <= 1; end\n  \
                     initial begin #1 clk = 1; i = 0; end",
                );
                sim.run_until(1).unwrap();
                assert_eq!((value(&mut sim, "q"), value(&mut sim, "w")), (12, 4));
            }

            /// The copy of an array holds all of it.
            #[test]
            fn arrays_are_sampled_whole() {
                let mut sim = simulation(
                    "  bit [2:0] mem [0:3];\n  bit [1:0] i;\n  \
                     int u;\n  \
                     always_ff @(posedge clk) mem[i] <= 3'd5;\n  \
                     always_ff @(posedge clk) u <= $sampled(mem[i]);\n  \
                     initial begin i = 3; #1 clk = 1; #1 clk = 0; #1 clk = 1; end",
                );
                sim.run_until(1).unwrap();
                assert_eq!(value(&mut sim, "u"), 0);
                sim.run_until(3).unwrap();
                assert_eq!(value(&mut sim, "u"), 5);
            }

            /// Automatic variables keep their current value; the operand may
            /// be any expression.
            #[test]
            fn automatic_variables_and_expressions() {
                let mut sim = simulation(
                    "  initial begin\n    \
                     automatic int k = 5;\n    \
                     #1 a = 1; b = 1;\n    \
                     v = $sampled(k + a + {31'b0, b});\n  \
                     end",
                );
                sim.run_until(1).unwrap();
                assert_eq!(value(&mut sim, "v"), 5);
            }

            #[test]
            fn combinational_logic_cannot_read_sampled_values() {
                for item in [
                    "always_comb y = $sampled(a);",
                    "always @(a) y = $sampled(a);",
                    "assign y = $sampled(a);",
                ] {
                    let source = format!("module Top; bit a, y; {item} endmodule");
                    let error = celox::Simulation::from_sv_sources(
                        vec![(source.as_str(), Path::new("sampled.sv"))],
                        "Top",
                    )
                    .$backend()
                    .err()
                    .expect("rejected");
                    assert!(
                        error.to_string().contains("$sampled in combinational logic"),
                        "{item}: {error}"
                    );
                }
            }
        }
    };
}

sampled!(interpreter, build_interpreter);
#[cfg(feature = "cranelift")]
sampled!(cranelift, build_cranelift);
#[cfg(target_arch = "x86_64")]
sampled!(native, build_native);
#[cfg(feature = "wasm-runtime")]
sampled!(wasm, build_wasm);

/// A `Simulator` call is a time slot: a domain reads the values its
/// inputs had when the previous call ended.
#[test]
fn a_tick_samples_the_value_of_the_previous_call() {
    let source = "module Top(input bit clk, input bit a, output bit r, output bit s);\n  \
                  always_ff @(posedge clk) begin r <= $sampled(a); s <= a; end\n\
                  endmodule";
    let mut sim = celox::Simulator::from_sv_sources(vec![(source, Path::new("sampled.sv"))], "Top")
        .build()
        .unwrap();
    let (a, r, s, clk) = (
        sim.signal("a"),
        sim.signal("r"),
        sim.signal("s"),
        sim.event("clk"),
    );
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!((sim.get_as::<u8>(r), sim.get_as::<u8>(s)), (1, 1));
}

/// A domain of a derived clock samples the values from before the call, not
/// those the source domain committed in it.
#[test]
fn derived_domains_sample_the_values_from_before_the_call() {
    let source = "module Top(input bit clk, output bit [7:0] r);\n  \
                  bit div;\n  bit [7:0] a;\n  \
                  always_ff @(posedge clk) begin div <= !div; a <= a + 1; end\n  \
                  always_ff @(posedge div) r <= $sampled(a);\n\
                  endmodule";
    // With the edges in generated units and through the plan walk.
    for units in [true, false] {
        let mut builder =
            celox::Simulator::from_sv_sources(vec![(source, Path::new("sampled.sv"))], "Top");
        if !units {
            builder = builder.disable_pass(celox::SirPass::DerivedEdgeUnits);
        }
        let mut sim = builder.build().unwrap();
        let (r, clk) = (sim.signal("r"), sim.event("clk"));
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u8>(r), 0, "units: {units}");
        sim.tick(clk).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u8>(r), 2, "units: {units}");
    }
}

/// A host write in a slot with nothing to run is what the next slot
/// samples.
#[test]
fn host_writes_of_idle_slots_are_sampled() {
    let source = "`timescale 1ns/1ns\nmodule Top(input bit clk, input bit a, output bit r);\n  \
                  always_ff @(posedge clk) r <= $sampled(a);\nendmodule";
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("sampled.sv"))], "Top")
            .build()
            .unwrap();
    sim.add_clock("clk", 2, 1);
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    sim.run_until(1).unwrap();
    let r = sim.signal("r");
    assert_eq!(sim.get(r), 1u8.into());
}
