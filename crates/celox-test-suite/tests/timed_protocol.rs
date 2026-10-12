#![cfg(all(feature = "icarus", feature = "verilator"))]

use celox_test_suite::{Backend, load_group, no_std_library};

#[test]
#[ignore = "requires Icarus, Verilator and a C++ compiler"]
fn early_finish_and_event_exhaustion_do_not_skip_script_checks() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/timed-protocol")
        .join(std::process::id().to_string());
    for (name, source, script) in [
        (
            "early",
            "module Top(output bit done); initial #5 $finish; endmodule",
            "(run_until 10) (assert_eq done 1) (run_to_finish)",
        ),
        (
            "finish_on_eval",
            "module Top(input bit go); initial begin wait(go); $finish; end endmodule",
            "(run_until 0) (set go 1) (eval) (run_to_finish)",
        ),
        (
            "idle",
            "module Top(output bit done); initial done = 1; endmodule",
            "(run_to_finish) (assert_eq done 1)",
        ),
    ] {
        let text = format!(
            "(group protocol (category sequential)) (case {name} (source \"test.sv\" #\"{source}\"#) (top Top) {script})"
        );
        let case = load_group("protocol.vtest", &text, no_std_library)[0];
        let frontend = celox_test_suite::sv::verification::SvFrontend;
        let mut icarus = celox_test_suite::icarus::Icarus::build_script(
            &frontend,
            case,
            &root.join(name).join("icarus"),
        )
        .unwrap();
        assert!(icarus.run_testbench().is_err(), "Icarus accepted {name}");
        let mut verilator = celox_test_suite::verilator::Verilator::build_script(
            &frontend,
            case,
            &root.join(name).join("verilator"),
        )
        .unwrap();
        let result = verilator.run_testbench();
        let log = std::fs::read_to_string(root.join(name).join("verilator/protocol.log")).unwrap();
        assert!(
            result.is_err() || celox_test_suite::script::sv::check_output(&log, &[]).is_err(),
            "Verilator accepted {name}"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires Verilator and a C++ compiler; Icarus rejects unique-if syntax"]
fn qualified_ff_if_actions_are_not_truncated_during_staging() {
    let text = r##"(group qualified (category sequential))
        (case startup (source "qualified.sv" #"module Top(input bit clk, en, output bit q, y);
            always_ff @(posedge clk) unique if(en) q<=1; else q<=0;
            initial y=q;
        endmodule"#) (top Top) (set en 1) (set clk 1) (run_until 0) (assert_eq y 0) (assert_eq q 1))"##;
    let case = load_group("qualified.vtest", text, no_std_library)[0];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/qualified-ff-protocol")
        .join(std::process::id().to_string());
    let frontend = celox_test_suite::sv::verification::SvFrontend;
    let mut backend =
        celox_test_suite::verilator::Verilator::build_script(&frontend, case, &root).unwrap();
    backend.run_testbench().unwrap();
    let log = std::fs::read_to_string(root.join("protocol.log")).unwrap();
    celox_test_suite::script::sv::check_output(&log, &[]).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
