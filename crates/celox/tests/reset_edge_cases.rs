#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn test_reset_literals_polarity(sim) {
        @case "reset_edge_cases::test_reset_literals_polarity";
    }
    fn test_reset_literals_context(sim) {
        @case "reset_edge_cases::test_reset_literals_context";
    }
    fn test_reset_literals_ff(sim) {
        @case "reset_edge_cases::test_reset_literals_ff";
    }


    // Test `reset_async_high`: the FF resets when the reset signal is HIGH.
    fn test_reset_async_high(sim) {
        @case "reset_edge_cases::test_reset_async_high";
    }

    // Test `reset_sync_high`: synchronous reset, active HIGH.
    fn test_reset_sync_high(sim) {
        @case "reset_edge_cases::test_reset_sync_high";
    }

    // Test `reset_sync_low`: synchronous reset, active LOW.
    fn test_reset_sync_low(sim) {
        @case "reset_edge_cases::test_reset_sync_low";
    }

    // Test multiple FF blocks sharing the same reset signal.
    fn test_shared_reset(sim) {
        @case "reset_edge_cases::test_shared_reset";
    }

    // Reset value that is non-zero: verifies the reset assignment uses
    // the specified value, not just zero.
    fn test_nonzero_reset_value(sim) {
        @case "reset_edge_cases::test_nonzero_reset_value";
    }
}

#[test]
fn reset_literals_follow_the_builder_reset_type_in_rtl_and_testbenches() {
    use celox::testbench::{compile_initial_testbench, run_compiled_testbench_to_finish};
    use celox::{ResetType, SimBackend, Simulator, TestResult};

    fn check<B: SimBackend>(mut sim: Simulator<B>, active: u8) {
        let clock = sim.event("clk");
        sim.tick(clock).unwrap();
        for (name, expected) in [
            ("held", active),
            ("released", 1 - active),
            ("registered", 1),
            ("explicit_high", 1),
            ("explicit_low", 0),
        ] {
            assert_eq!(sim.get(sim.signal(name)), expected.into(), "{name}");
        }
        let program = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &program),
            TestResult::Pass
        );
    }

    for reset_type in [
        ResetType::AsyncHigh,
        ResetType::AsyncLow,
        ResetType::SyncHigh,
        ResetType::SyncLow,
    ] {
        let active = u8::from(matches!(
            reset_type,
            ResetType::AsyncHigh | ResetType::SyncHigh
        ));
        let inactive = 1 - active;
        let source = format!(
            r#"module Top (
                clk: input clock,
                held: output reset,
                released: output reset,
                registered: output logic,
                explicit_high: output reset_async_high,
                explicit_low: output reset_sync_low,
            ) {{
                assign held = reset_active;
                assign released = reset_inactive;
                assign explicit_high = reset_active;
                assign explicit_low = reset_active;
                always_ff (clk) {{ registered = held == reset_active; }}
                #[allow(initial_assign)]
                var test_reset: reset;
                initial {{
                    test_reset = reset_active;
                    $assert(test_reset == reset_active);
                    $assert((test_reset as u8) == 8'd{active});
                    test_reset = reset_inactive;
                    $assert(reset_inactive == test_reset);
                    $assert((test_reset as u8) == 8'd{inactive});
                    $finish();
                }}
            }}"#
        );
        for four_state in [false, true] {
            macro_rules! check_backend {
                ($build:ident) => {
                    check(
                        Simulator::builder(&source, "Top")
                            .reset_type(reset_type)
                            .four_state(four_state)
                            .$build()
                            .unwrap(),
                        active,
                    );
                };
            }
            check_backend!(build_interpreter);
            check_backend!(build_cranelift);
            check_backend!(build_wasm);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check_backend!(build_native);
        }
    }
}
