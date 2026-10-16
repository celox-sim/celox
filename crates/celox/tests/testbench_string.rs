#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {
    fn tb_string_array_elements(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "tb_string::array_elements";
    }
    fn tb_string_function_argument(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "tb_string::function_argument";
    }

    fn tb_string_assignment(sim) {
        @omit_veryl;
        // Veryl does not emit #[test] modules into SystemVerilog.
        @ignore_on(sv);
        @case "tb_string::assignment";
    }
    fn tb_string_comparison(sim) {
        @omit_veryl;
        // Veryl does not emit #[test] modules into SystemVerilog.
        @ignore_on(sv);
        @case "tb_string::comparison";
    }
    fn tb_string_control_flow(sim) {
        @omit_veryl;
        // Veryl does not emit #[test] modules into SystemVerilog.
        @ignore_on(sv);
        @case "tb_string::control_flow";
    }
}

#[test]
fn strings_survive_clock_steps_and_preserve_message_lengths() {
    let result = celox::Simulator::builder(
        r#"
        #[test(Top)] module Top {
            var text: string;
            inst clk: $tb::clock_gen;
            initial {
                text = "long string across a clock step";
                clk.next();
                $assert(text == "long string across a clock step", "[%s]", text);
                text = "";
                $assert(text == "", "[%s]", text);
                $finish();
            }
        }
    "#,
        "Top",
    )
    .run_test_detailed()
    .unwrap();
    assert!(result.passed);
    assert_eq!(
        result.assertions[0].message.as_deref(),
        Some(b"[long string across a clock step]".as_slice())
    );
    assert_eq!(
        result.assertions[1].message.as_deref(),
        Some(b"[]".as_slice())
    );
}

#[test]
fn checkpoint_restores_string_values() {
    use celox::testbench::{compile_initial_testbench, run_compiled_testbench};
    let mut sim = celox::Simulator::builder(
        r#"
        #[test(Top)] module Top {
            var text: string;
            var mode: u32;
            initial {
                if mode == 0 { text = "saved string"; }
                else if mode == 1 { text = "replacement"; }
                else { $assert(text == "saved string"); }
                $finish();
            }
        }
    "#,
        "Top",
    )
    .build_interpreter()
    .unwrap();
    let program = compile_initial_testbench(&sim).unwrap();
    let mode = sim.signal("mode");
    sim.set(mode, 0u32);
    let empty_size = sim.checkpoint().unwrap().state_size();
    assert_eq!(
        run_compiled_testbench(&mut sim, &program),
        celox::TestResult::Pass
    );
    let saved = sim.checkpoint().unwrap();
    assert_eq!(saved.state_size(), empty_size + b"saved string".len());
    assert!(
        sim.save_state()
            .unwrap_err()
            .to_string()
            .contains("dynamic testbench strings")
    );
    sim.set(mode, 1u32);
    assert_eq!(
        run_compiled_testbench(&mut sim, &program),
        celox::TestResult::Pass
    );
    sim.restore(&saved).unwrap();
    sim.set(mode, 2u32);
    assert_eq!(
        run_compiled_testbench(&mut sim, &program),
        celox::TestResult::Pass
    );
}
