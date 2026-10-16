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
            // A design that finishes at the target time of run_until.
            "finish_at_target",
            "module Top(output bit done); initial #5 $finish; endmodule",
            "(run_until 5) (assert_eq done 0)",
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
        let result = icarus.run_testbench();
        assert!(
            result.is_err()
                || celox_test_suite::script::sv::check_run(&root.join(name).join("icarus"))
                    .is_err(),
            "Icarus accepted {name}"
        );
        let mut verilator = celox_test_suite::verilator::Verilator::build_script(
            &frontend,
            case,
            &root.join(name).join("verilator"),
        )
        .unwrap();
        let result = verilator.run_testbench();
        assert!(
            result.is_err()
                || celox_test_suite::script::sv::check_run(&root.join(name).join("verilator"))
                    .is_err(),
            "Verilator accepted {name}"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
