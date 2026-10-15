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
        let log = std::fs::read(root.join(name).join("verilator/protocol.log")).unwrap();
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
    let log = std::fs::read(root.join("protocol.log")).unwrap();
    celox_test_suite::script::sv::check_output(&log, &[]).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires Icarus, Verilator and a C++ compiler"]
fn bare_continuous_function_calls_observe_staged_inputs() {
    // Exercise the adapter directly: Celox currently rejects bare function
    // values in continuous assignments, while the adapter normalizes them
    // to explicit calls for the external simulators.
    let text = r##"(group protocol (category sequential))
      (case bare (source "bare.sv" #"
        package bare_shadow_pkg; function automatic bit constant_ready; return 0; endfunction endpackage
        module Top(input bit[7:0] go,output wire[7:0] y,z,output wire ready);
          import bare_shadow_pkg::*;
          function automatic bit[7:0] checked;
            if(!go)$fatal(1,"bare function ran before setup");
            return go;
          endfunction
          assign y=checked;
          wire[7:0] other=checked;
          assign z=other;
          wire constant_ready=1;
          assign ready=constant_ready;
          initial begin #2;$finish;end
        endmodule
      "#) (top Top)(set go 1)(run_until 0)(assert_eq y 1)(assert_eq z 1)(assert_eq ready 1)
      (set go 2)(run_until 1)(assert_eq y 2)(assert_eq z 2))"##;
    let case = load_group("bare.vtest", text, no_std_library)[0];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/bare-continuous-protocol")
        .join(std::process::id().to_string());
    let frontend = celox_test_suite::sv::verification::SvFrontend;
    let mut icarus =
        celox_test_suite::icarus::Icarus::build_script(&frontend, case, &root.join("icarus"))
            .unwrap();
    icarus.run_testbench().unwrap();
    let mut verilator = celox_test_suite::verilator::Verilator::build_script(
        &frontend,
        case,
        &root.join("verilator"),
    )
    .unwrap();
    verilator.run_testbench().unwrap();
    let log = std::fs::read(root.join("verilator/protocol.log")).unwrap();
    celox_test_suite::script::sv::check_output(&log, &[]).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires Icarus, Verilator and a C++ compiler"]
fn callable_instance_connections_wait_for_staged_inputs() {
    let text = r##"(group protocol (category sequential))

(case callable_instance_inputs_wait_for_staged_values
 (source "instance-connection-effects.sv" #"
 module Child #(parameter W=8)(input bit[W-1:0]value,output wire[W-1:0]seen);assign seen=value;endmodule
 module Top(input bit[7:0]go,output wire[7:0]a,b);
 function automatic bit[1:0]checked(input bit[7:0]value);if(!value)$fatal(1,"early instance connection");return value;endfunction
 Child #(.W(8)) named(.value(checked(go)+2'd3),.seen(a));
 Child #(8) ordered(checked(go)+2'd3,b);
 initial begin #2;$finish;end
 endmodule
 "#)
 (top Top)(set go 1)(run_until 0)(assert_eq a 4)(assert_eq b 4)
 (set go 2)(run_until 1)(assert_eq a 5)(assert_eq b 5))

"##;
    let case = load_group("connections.vtest", text, no_std_library)[0];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/callable-connection-protocol")
        .join(std::process::id().to_string());
    let frontend = celox_test_suite::sv::verification::SvFrontend;
    let mut icarus =
        celox_test_suite::icarus::Icarus::build_script(&frontend, case, &root.join("icarus"))
            .unwrap();
    let icarus_result = icarus.run_testbench();
    let mut verilator = celox_test_suite::verilator::Verilator::build_script(
        &frontend,
        case,
        &root.join("verilator"),
    )
    .unwrap();
    let verilator_result = verilator.run_testbench();
    assert!(
        icarus_result.is_ok() && verilator_result.is_ok(),
        "Icarus: {icarus_result:?}; Verilator: {verilator_result:?}"
    );
    let log = std::fs::read(root.join("verilator/protocol.log")).unwrap();
    celox_test_suite::script::sv::check_output(&log, &[]).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn dynamic_const_initializers_are_rejected_before_tool_invocation() {
    let text = r##"(group protocol (category sequential))
      (case dynamic_const (source "const.sv" #"module Top(input bit a,output wire seen);const bit saved=a;assign seen=saved;initial #2 $finish;endmodule"#)
        (top Top)(set a 1)(run_until 0)(assert_eq seen 1))"##;
    let case = load_group("const.vtest", text, no_std_library)[0];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/const-initializer-protocol")
        .join(std::process::id().to_string());
    let frontend = celox_test_suite::sv::verification::SvFrontend;
    let icarus =
        celox_test_suite::icarus::Icarus::build_script(&frontend, case, &root.join("icarus"))
            .err()
            .expect("dynamic const reached Icarus compilation");
    assert!(
        icarus.is::<celox_test_suite::script::sv::Unsupported>(),
        "{icarus}"
    );
    let verilator = celox_test_suite::verilator::Verilator::build_script(
        &frontend,
        case,
        &root.join("verilator"),
    )
    .err()
    .expect("dynamic const reached Verilator compilation");
    assert!(
        verilator.is::<celox_test_suite::script::sv::Unsupported>(),
        "{verilator}"
    );
    std::fs::remove_dir_all(root).unwrap();
}
