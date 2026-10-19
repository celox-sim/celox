//! End-to-end coverage of the direct HDL testbench command-line boundary.

use std::process::Command;

fn run(sources: &[(&str, &str)], extra: &[&str]) -> std::process::Output {
    let temporary = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_celox"));
    command.arg("run");
    for (name, source) in sources {
        let path = temporary.path().join(name);
        std::fs::write(&path, source).unwrap();
        command.arg(path);
    }
    command.args(["--top", "Top"]).args(extra).output().unwrap()
}

#[cfg(feature = "systemverilog")]
#[test]
fn sv_optimization_preserves_testbench_reads_of_internal_combinational_signals() {
    let source = r#"
        module Dut(input logic [7:0] value, output logic [7:0] result);
            logic [7:0] observed, unused;
            always_comb begin
                observed = value ^ 8'h55;
                unused = value + 8'd1;
                result = observed;
            end
        endmodule
        module Top;
            logic [7:0] value, result;
            Dut dut(value, result);
            initial begin
                value = 8'h12;
                #1;
                assert (result === 8'h47) else $fatal(1, "first result");
                $display("result=%h", result);
                value = 8'h34;
                #1;
                assert (result === 8'h61) else $fatal(1, "second result");
                $display("result=%h", result);
                $finish;
            end
        endmodule
    "#;
    for extra in [vec![], vec!["-O", "O0"], vec!["-O", "O1"], vec!["-O", "O2"]] {
        let output = run(&[("top.sv", source)], &extra);
        assert!(output.status.success(), "{extra:?}: {output:?}");
        assert_eq!(output.stdout, b"result=47\nresult=61\n", "{extra:?}");
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn runs_multiple_sources_and_preserves_display_and_write_output() {
    let output = run(
        &[
            (
                "types.sv",
                "package types; parameter int VALUE = 42; endpackage",
            ),
            (
                "top.sv",
                "module Top; initial begin $write(\"value=\"); $display(\"%0d\", types::VALUE); $finish; end endmodule",
            ),
        ],
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"value=42\n");
}

#[cfg(feature = "systemverilog")]
#[test]
fn no_remaining_events_is_success() {
    let output = run(
        &[(
            "top.sv",
            "module Top; initial $display(\"done\"); endmodule",
        )],
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"done\n");
}

#[cfg(feature = "systemverilog")]
#[test]
fn fatal_and_nonfatal_failures_return_failure_with_output() {
    for action in [
        "$fatal(1, \"failed\")",
        "$error(\"failed\")",
        "assert (0) else $error(\"failed\")",
    ] {
        let source = format!("module Top; initial begin {action}; $finish; end endmodule");
        let output = run(&[("top.sv", &source)], &[]);
        assert_eq!(output.status.code(), Some(1), "{action}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("failed"),
            "{action}: {output:?}"
        );
    }
}

#[test]
fn invalid_source_and_missing_source_return_failure() {
    let output = run(&[("bad.veryl", "module Top { invalid syntax }")], &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("compilation failed"));

    let temporary = tempfile::tempdir().unwrap();
    let missing = temporary.path().join("missing.veryl");
    let output = Command::new(env!("CARGO_BIN_EXE_celox"))
        .arg("run")
        .arg(&missing)
        .args(["--top", "Top"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to read"));
}

#[test]
fn requires_sources_top_and_positive_step_limit() {
    for args in [
        vec!["run", "--top", "Top"],
        vec!["run", "top.sv"],
        vec!["run", "top.sv", "--top", "Top", "--max-steps", "0"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_celox"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
}

#[test]
fn veryl_optimization_preserves_native_testbench_reads_of_internal_signals() {
    let source = r#"
        #[test(Top)]
        module Top {
            var source: logic<8>;
            var observed: logic<8>;
            var unused: logic<8>;
            assign observed = source ^ 8'h55;
            assign unused = source + 8'd1;
            initial {
                source = 8'h12;
                $assert(observed == 8'h47, "first result");
                $display("result=%h", observed);
                source = 8'h34;
                $assert(observed == 8'h61, "second result");
                $display("result=%h", observed);
                $finish();
            }
        }
    "#;
    for extra in [vec![], vec!["-O", "O0"], vec!["-O", "O1"], vec!["-O", "O2"]] {
        let output = run(&[("top.veryl", source)], &extra);
        assert!(output.status.success(), "{extra:?}: {output:?}");
        let messages = String::from_utf8_lossy(&output.stderr);
        assert!(messages.contains("result=47"), "{extra:?}: {messages}");
        assert!(messages.contains("result=61"), "{extra:?}: {messages}");
    }
}

#[test]
fn runs_veryl_native_testbenches_and_reports_assertions() {
    for (assertion, success) in [("1'd1", true), ("1'd0", false)] {
        let source = format!(
            "#[test(Top)] module Top {{ initial {{ $display(\"native tb\"); $assert({assertion}); $finish(); }} }}"
        );
        let output = run(&[("top.veryl", &source)], &[]);
        assert_eq!(output.status.success(), success, "{output:?}");
        // The native testbench API forwards HDL output to stderr.
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("native tb"),
            "{output:?}"
        );
    }
}

#[test]
fn veryl_tick_limit_is_a_failure() {
    let output = run(
        &[(
            "top.veryl",
            "#[test(Top)] module Top { inst clk: $tb::clock_gen; initial { clk.next(10); $finish(); } }",
        )],
        &["--max-steps", "2"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("testbench ticks before completion"),
        "{output:?}"
    );
}

#[test]
fn veryl_project_discovers_sibling_sources() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::write(
        temporary.path().join("Veryl.toml"),
        "[project]\nname = \"cli_project\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(temporary.path().join("dut.veryl"), "module Dut {} ").unwrap();
    let source = temporary.path().join("top.veryl");
    std::fs::write(
        &source,
        "#[test(Top)] module Top { inst dut: Dut; initial { $assert(1'd1); $finish(); } }",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_celox"))
        .arg("run")
        .arg(source)
        .args(["--top", "Top"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn rejects_mixed_sources_and_veryl_without_a_testbench() {
    let output = run(&[("top.sv", ""), ("top.veryl", "")], &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("one .veryl"));
    let output = run(&[("top.veryl", "module Top {}")], &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no native testbench"),
        "{output:?}"
    );
}

#[cfg(feature = "systemverilog")]
#[test]
fn sv_delays_complete_and_execution_limit_fails() {
    let output = run(
        &[(
            "top.sv",
            "module Top; initial begin #1; $display(\"later\"); $finish; end endmodule",
        )],
        &[],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"later\n");

    let output = run(
        &[(
            "clock.sv",
            "module Top; bit clk = 0; initial forever #1 clk = ~clk; endmodule",
        )],
        &["--max-steps", "3"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("exceeded --max-steps 3"),
        "{output:?}"
    );
}

#[test]
fn accepts_supported_optimization_levels() {
    for extension in ["sv", "veryl"]
        .into_iter()
        .filter(|extension| *extension != "sv" || cfg!(feature = "systemverilog"))
    {
        let name = format!("top.{extension}");
        let source = if extension == "sv" {
            "module Top; initial $finish; endmodule"
        } else {
            "#[test(Top)] module Top { initial { $finish(); } }"
        };
        for level in ["O0", "O1", "O2"] {
            let output = run(&[(&name, source)], &["-O", level]);
            assert!(output.status.success(), "{extension}, {level}: {output:?}");
        }
    }
    let output = run(&[("top.sv", "")], &["-O", "O3"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn accepts_relative_standalone_veryl_source() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::write(
        temporary.path().join("top.veryl"),
        "#[test(Top)] module Top { initial { $finish(); } }",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_celox"))
        .current_dir(temporary.path())
        .args(["run", "top.veryl", "--top", "Top"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[cfg(not(feature = "systemverilog"))]
#[test]
fn sv_without_feature_explains_how_to_enable_it() {
    let output = run(&[("top.sv", "module Top; endmodule")], &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--features systemverilog"));
}

#[cfg(feature = "systemverilog")]
#[test]
fn sv_stop_is_not_reported_as_successful_completion() {
    let output = run(
        &[(
            "top.sv",
            "module Top; initial begin $display(\"before stop\"); $stop; $display(\"after stop\"); end endmodule",
        )],
        &[],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("requested $stop"),
        "{output:?}"
    );
    assert_eq!(output.stdout, b"before stop\n");
}
