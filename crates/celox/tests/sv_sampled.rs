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
sampled!(cranelift, build);
#[cfg(target_arch = "x86_64")]
sampled!(native, build_native);
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
