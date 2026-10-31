//! Celox's native TB adapter must resolve reset_gen through port aliases.
use celox::{SimBackend, Simulator, TestResult};
use veryl_metadata::ResetType;

fn source(reset: &str, nested_tb: bool) -> String {
    let tb = r#"
        inst clk: $tb::clock_gen;
        inst rst: $tb::reset_gen(clk);
        var count: logic<8>;
        inst dut: Wrapper(clk, rst, count);
        initial {
            rst.assert(4);
            clk.next(7);
            $assert(count == 7);
            rst.assert(2);
            clk.next(3);
            $assert(count == 3);
            $finish();
        }
    "#;
    format!(
        "module Counter(clk: input clock, rst: input {reset}, count: output logic<8>) {{
            always_ff {{ if_reset {{ count = 0; }} else {{ count += 1; }} }}
        }}
        module Wrapper(clk: input clock, rst: input reset, count: output logic<8>) {{
            inst counter: Counter(clk, rst, count);
        }}
        {}",
        if nested_tb {
            format!(
                "#[test(Harness)] module Harness {{ {tb} }} module Top {{ inst harness: Harness; }}"
            )
        } else {
            format!("#[test(Top)] module Top {{ {tb} }}")
        }
    )
}

fn run<B: SimBackend>(mut sim: Simulator<B>) {
    let tb = celox::testbench::compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        celox::testbench::run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
}

#[test]
fn generator_follows_explicit_reset_through_abstract_ports() {
    for reset in [
        "reset_sync_high",
        "reset_async_high",
        "reset_sync_low",
        "reset_async_low",
    ] {
        for nested in [false, true] {
            let source = source(reset, nested);
            let fallback = if reset.ends_with("high") {
                ResetType::AsyncLow
            } else {
                ResetType::AsyncHigh
            };
            for four_state in [false, true] {
                let builder = || {
                    Simulator::builder(&source, "Top")
                        .reset_type(fallback)
                        .four_state(four_state)
                };
                run(builder().build_native().unwrap());
                run(builder().build_interpreter().unwrap());
                #[cfg(feature = "cranelift")]
                run(builder().build_cranelift().unwrap());
                #[cfg(feature = "wasm-runtime")]
                run(builder().build_wasm().unwrap());
            }
        }
    }
}

#[test]
fn abstract_reset_uses_project_default() {
    let source = source("reset", false);
    for fallback in [
        ResetType::AsyncHigh,
        ResetType::AsyncLow,
        ResetType::SyncHigh,
        ResetType::SyncLow,
    ] {
        run(Simulator::builder(&source, "Top")
            .reset_type(fallback)
            .build_native()
            .unwrap());
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn generator_follows_declared_indices_of_sv_instance_arrays() {
    use std::path::Path;
    let veryl = r#"
        module Counter(clk: input clock, rst: input reset_sync_high, count: output logic<8>) {
            always_ff { if_reset { count = 0; } else { count += 1; } }
        }
        #[test(Top)] module Top {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var count: logic<16>;
            var reset_level: logic;
            assign reset_level = rst;
            inst bridge: $sv::Bridge(clk, rst: reset_level, count);
            initial {
                rst.assert(4);
                clk.next(7);
                $assert(count == 16'h0707);
                rst.assert(2);
                clk.next(3);
                $assert(count == 16'h0303);
                $finish();
            }
        }
    "#;
    for bounds in ["3:2", "2:3", "2:1"] {
        let sv = format!(
            "module Bridge(input logic clk, rst, output wire [15:0] count);
                prj_Counter counters[{bounds}] (.clk(clk), .rst(rst), .count(count));
             endmodule"
        );
        for four_state in [false, true] {
            let builder = || {
                Simulator::from_mixed_sources(
                    vec![(veryl, Path::new("top.veryl"))],
                    vec![(&sv, Path::new("bridge.sv"))],
                    "Top",
                )
                .reset_type(ResetType::AsyncLow)
                .four_state(four_state)
            };
            run(builder().build_native().unwrap());
            run(builder().build_interpreter().unwrap());
        }
    }
}

/// A DUT clock port is a second address of the TB clock's event. Its copy can
/// be unsettled when the compiled scheduler saves edge baselines, so the
/// four-state interpreter (host edge path) must take the level from the
/// event's own signal or it misses every later rising edge.
#[test]
fn clock_port_copy_does_not_mask_rising_edges_after_reset() {
    let source = r#"
        module Counter(clk: input clock, rst: input reset_sync_high, count: output logic<8>) {
            always_ff { if_reset { count = 0; } else { count += 1; } }
        }
        #[test(Top)] module Top {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var count: logic<8>;
            inst dut: Counter(clk, rst, count);
            initial {
                rst.assert(4);
                clk.next(1);
                $assert(count == 1, "count=%d", count);
                clk.next(6);
                $assert(count == 7, "count=%d", count);
                $finish();
            }
        }
    "#;
    for four_state in [false, true] {
        run(Simulator::builder(source, "Top")
            .four_state(four_state)
            .build_interpreter()
            .unwrap());
    }
}
