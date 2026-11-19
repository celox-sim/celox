//! Reset literal tests that are not celox-test-suite cases (the suite
//! cases themselves run from their `.vtest` files).

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
            #[cfg(feature = "cranelift")]
            check_backend!(build_cranelift);
            #[cfg(feature = "wasm-runtime")]
            check_backend!(build_wasm);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check_backend!(build_native);
        }
    }
}
