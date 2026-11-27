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
    assert!(String::from_utf8_lossy(&output.stderr).contains("Veryl project sources"));
    let output = run(&[("top.veryl", "module Top {}")], &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no native testbench"),
        "{output:?}"
    );
}

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
    for extension in ["sv", "veryl"] {
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

#[test]
fn veryl_backends_share_cli_execution_and_opt_in_reports() {
    let source =
        "#[test(Top)] module Top { inst clk: $tb::clock_gen; initial { clk.next(4); $finish(); } }";
    for backend in [
        "native",
        "tiered",
        "interpreter",
        #[cfg(feature = "cranelift")]
        "cranelift",
        #[cfg(feature = "wasm-runtime")]
        "wasm",
    ] {
        for four_state in [false, true] {
            let mut args = vec!["--backend", backend, "--threads", "2", "--report"];
            if four_state {
                args.push("--four-state");
            }
            let output = run(&[("top.veryl", source)], &args);
            assert!(output.status.success(), "{backend}: {output:?}");
            let report = String::from_utf8(output.stdout).unwrap();
            assert!(
                report.contains(&format!(
                    "backend={backend} opt_level=O2 four_state={four_state}"
                )),
                "{report}"
            );
            assert!(
                report.contains("CELOX_TEST_THREADS test=Top threads=2"),
                "{report}"
            );
            assert!(
                report.contains("CELOX_TEST_RESULT test=Top status=pass"),
                "{report}"
            );
        }
    }
    let output = run(&[("top.veryl", source)], &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}

#[test]
fn tiered_report_records_when_each_tier_ran() {
    let source = "module Counter (clk: input clock, q: output bit<32>) { always_ff { q += 1; } }
        #[test(Top)] module Top {
            inst clk: $tb::clock_gen;
            var q: bit<32>;
            inst counter: Counter (clk, q);
            initial { clk.next(40); $assert(q == 40); $finish(); }
        }";
    let output = run(
        &[("top.veryl", source)],
        &["--report", "--backend", "tiered", "--jit", "whole-image"],
    );
    assert!(output.status.success(), "{output:?}");
    let report = String::from_utf8(output.stdout).unwrap();
    let line = report
        .lines()
        .find(|line| line.starts_with("CELOX_TIERED_TIMELINE "))
        .unwrap_or_else(|| panic!("no timeline: {report}"));
    let field = |name: &str| {
        line.split(' ')
            .find_map(|part| part.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("no {name}: {line}"))
    };
    // Compilation may finish after this short run; the record stays complete.
    let finish: u64 = field("finish_tick").parse().unwrap();
    assert!(finish >= 40, "{line}");
    for stage in field("compiles")
        .split(',')
        .filter(|stage| *stage != "none")
    {
        let parts: Vec<_> = stage.split(':').collect();
        assert_eq!(parts.len(), 3, "{line}");
        parts[1].parse::<u128>().unwrap();
        assert!(matches!(parts[2], "completed" | "failed"), "{line}");
    }
    if let Ok(promoted) = field("promoted_tick").parse::<u64>() {
        assert!(promoted <= finish, "{line}");
    }
}

// Unknown bits print as `x` in testbench messages (IEEE 1800-2023
// 21.2.1.3), not as their payload (#2629).
#[test]
fn veryl_testbench_displays_print_unknown_bits() {
    let source = r#"
        #[test(Top)]
        module Top {
            inst clk: $tb::clock_gen;
            var mem: logic<64> [4];
            var v  : logic<64>;
            initial {
                clk.next(1);
                v = mem[1];
                $display("v=%h", v);
                $display("m=%h", mem[2]);
                $finish();
            }
        }
    "#;
    for backend in [
        "native",
        "tiered",
        "interpreter",
        #[cfg(feature = "cranelift")]
        "cranelift",
    ] {
        for (state, digit) in [("--four-state", 'x'), ("--two-state", '0')] {
            let output = run(&[("top.veryl", source)], &["--backend", backend, state]);
            assert!(output.status.success(), "{backend} {state}: {output:?}");
            let printed = String::from_utf8_lossy(&output.stderr);
            let value = digit.to_string().repeat(16);
            for line in [format!("v={value}"), format!("m={value}")] {
                assert!(
                    printed.lines().any(|printed| printed == line),
                    "{backend} {state}: missing {line} in {printed}"
                );
            }
        }
    }
}

#[cfg(not(any(
    all(target_arch = "x86_64", feature = "arm64-codegen"),
    all(target_arch = "aarch64", feature = "x86_64-codegen")
)))]
#[test]
fn backend_defaults_to_tiered_unless_native_code_is_requested() {
    let source =
        "#[test(Top)] module Top { inst clk: $tb::clock_gen; initial { clk.next(4); $finish(); } }";
    let output = run(&[("top.veryl", source)], &["--report"]);
    assert!(output.status.success(), "{output:?}");
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("backend=tiered "), "{report}");

    let output = run(&[("top.veryl", source)], &["--report", "--compile-only"]);
    assert!(output.status.success(), "{output:?}");
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("backend=native "), "{report}");
}

#[test]
fn sv_defaults_to_two_state_and_four_state_is_explicit() {
    for (args, expected) in [(vec![], "0"), (vec!["--four-state"], "x")] {
        let source = format!(
            "module Top; logic value; initial begin #1; assert (value === 1'b{expected}) else $fatal(1, \"state mode\"); $finish; end endmodule"
        );
        let output = run(&[("top.sv", &source)], &args);
        assert!(output.status.success(), "{args:?}: {output:?}");
    }
}

#[test]
fn cli_compiles_images_and_loads_without_sources() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("top.veryl");
    let image = temporary.path().join("top.image");
    std::fs::write(
        &source,
        "#[test(Top)] module Top { initial { $finish(); } }",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_celox"))
        .args([
            "run",
            "--top",
            "Top",
            "--compile-only",
            "--native-image-output",
        ])
        .arg(&image)
        .arg(&source)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    std::fs::remove_file(source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_celox"))
        .current_dir(temporary.path())
        .args(["run", "--top", "Top", "--native-image-input"])
        .arg(image)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn execution_option_conflicts_are_rejected() {
    for args in [
        vec!["--threads", "0"],
        vec!["--threads", "65"],
        vec!["--max-steps", "1", "--unlimited"],
        vec!["--require-compiled"],
        vec!["--report", "--require-compiled", "--backend", "native"],
        vec!["--compile-only", "--backend", "tiered"],
        vec![
            "--compile-only",
            "--dump-ir-dir",
            "unused",
            "--dump-ir-and-run",
        ],
        // A settle profile needs a run to count.
        vec!["--compile-only", "--settle-profile", "unused.tsv"],
        vec!["--dump-ir-dir", "unused", "--settle-profile", "unused.tsv"],
    ] {
        let output = run(
            &[(
                "top.veryl",
                "#[test(Top)] module Top { initial { $finish(); } }",
            )],
            &args,
        );
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
    }
}

/// `--sir-pass` selects the passes of a SystemVerilog design as it does
/// those of a Veryl one: with net inlining (the two-state O2 default)
/// disabled, the run prints the same.
#[test]
fn sv_accepts_pass_overrides() {
    let source = r#"
        module Dut(input logic [7:0] value, output logic [7:0] result);
            logic [7:0] bumped, mixed;
            assign bumped = value + 8'd3;
            assign mixed = bumped ^ {value[3:0], value[7:4]};
            assign result = mixed & 8'h7e;
        endmodule
        module Top;
            logic [7:0] value, result;
            Dut dut(value, result);
            initial begin
                value = 8'h12;
                #1;
                $display("result=%h", result);
                value = 8'h34;
                #1;
                $display("result=%h", result);
                $finish;
            end
        endmodule
    "#;
    for extra in [
        vec![],
        vec!["--sir-pass=-net_inlining"],
        vec!["--sir-pass=-net_inlining", "--sir-pass=-gvn"],
        vec!["--backend", "native", "--sir-pass=-net_inlining"],
    ] {
        let output = run(&[("top.sv", source)], &extra);
        assert!(output.status.success(), "{extra:?}: {output:?}");
        assert_eq!(output.stdout, b"result=34\nresult=74\n", "{extra:?}");
    }
    let output = run(&[("top.sv", source)], &["--sir-pass=-no_such_pass"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

#[test]
fn sv_backends_preserve_timed_nba_execution_and_failures() {
    let source = r#"
        module Top;
            logic clk = 0;
            logic [7:0] count = 0;
            always #5 clk = ~clk;
            always @(posedge clk) count <= count + 1;
            initial begin
                #16;
                assert (count == 2) else $fatal(1, "NBA count");
                $display("count=%0d", count);
                $finish;
            end
        endmodule
    "#;
    for backend in [
        "native",
        #[cfg(feature = "cranelift")]
        "cranelift",
        "interpreter",
        #[cfg(feature = "wasm-runtime")]
        "wasm",
        "tiered",
    ] {
        for four_state in [false, true] {
            let mut args = vec!["--backend", backend, "--threads", "2"];
            if four_state {
                args.push("--four-state");
            }
            let output = run(&[("top.sv", source)], &args);
            assert!(output.status.success(), "{args:?}: {output:?}");
            assert_eq!(output.stdout, b"count=2\n", "{args:?}");
            let limited = run(
                &[("top.sv", source)],
                &[args.clone(), vec!["--max-steps", "1"]].concat(),
            );
            assert!(!limited.status.success(), "{args:?}: {limited:?}");
            assert!(String::from_utf8_lossy(&limited.stderr).contains("exceeded --max-steps"));
            let failing = source.replace("count == 2", "count == 3");
            let output = run(&[("top.sv", &failing)], &args);
            assert!(!output.status.success(), "{args:?}: {output:?}");
            assert!(String::from_utf8_lossy(&output.stdout).contains("NBA count"));
        }
    }
}

#[test]
fn image_reports_its_embedded_state_mode() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("top.veryl");
    let image = temporary.path().join("top.image");
    std::fs::write(
        &source,
        "#[test(Top)] module Top { initial { $finish(); } }",
    )
    .unwrap();
    for four_state in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_celox"));
        command
            .args([
                "run",
                "--top",
                "Top",
                "--compile-only",
                "--native-image-output",
            ])
            .arg(&image)
            .arg(&source);
        if four_state {
            command.arg("--four-state");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let mut command = Command::new(env!("CARGO_BIN_EXE_celox"));
        command
            .args(["run", "--top", "Top", "--report", "--native-image-input"])
            .arg(&image);
        if !four_state {
            command.arg("--four-state");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("four_state={four_state}")),
            "{output:?}"
        );
    }
}

#[test]
fn source_file_is_relative_to_celox_manifest_from_another_directory() {
    let project = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    std::fs::write(
        project.path().join("src/top.veryl"),
        "#[test(Top)] module Top { initial { $finish(); } }",
    )
    .unwrap();
    std::fs::write(project.path().join("celox.toml"), "top = 'Top'\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_celox"))
        .current_dir(elsewhere.path())
        .args(["run", "--project"])
        .arg(project.path().join("celox.toml"))
        .args(["--source-file", "src/top.veryl"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn interface_array_query_policy_flows_through_manifest_and_cli_overrides() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("top.sv"),
        r#"
        interface Lane; logic v; endinterface
        module Top;
            Lane lanes[3:2]();
            localparam N=$size(lanes);
            initial begin
                assert(N==2) else $fatal(1,"size");
                $display("size=%0d",N);
                $finish;
            end
        endmodule
    "#,
    )
    .unwrap();
    let manifest = project.path().join("celox.toml");
    for (configured, overridden, success, warning) in [
        // The default is warn; strict or an explicit error rejects.
        (None, None, true, true),
        (Some("warn"), None, true, true),
        (Some("allow"), None, true, false),
        (Some("warn"), Some("error"), false, false),
        (Some("error"), Some("allow"), true, false),
    ] {
        let mut config = "sources=['top.sv']\ntop='Top'\n".to_string();
        if let Some(level) = configured {
            config.push_str(&format!("[diagnostics]\ninterface_array_query='{level}'\n"));
        }
        std::fs::write(&manifest, config).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_celox"));
        cmd.args(["run", "--project"])
            .arg(&manifest)
            .args(["--backend", "interpreter"])
            .env("RUST_LOG", "warn");
        if let Some(level) = overridden {
            cmd.args(["--diagnostic", &format!("interface_array_query={level}")]);
        }
        let output = cmd.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.success(),
            success,
            "{configured:?}/{overridden:?}: {output:?}"
        );
        if success {
            assert_eq!(output.stdout, b"size=2\n", "{output:?}");
            assert_eq!(
                stderr.contains("interface_array_query"),
                warning,
                "{stderr}"
            );
        } else {
            assert!(stderr.contains("interface_array_query"), "{stderr}");
        }
    }
}

#[test]
fn sv_saves_state_files_at_cycles_and_times() {
    let source = "`timescale 1ns/1ns
module Top;
  logic clk = 0;
  logic [7:0] count = 0;
  always #5 clk = ~clk;
  always_ff @(posedge clk) count <= count + 8'd1;
  initial #100 $finish;
endmodule
";
    let states = tempfile::tempdir().unwrap();
    let directory = states.path().to_str().unwrap();
    for backend in ["tiered", "interpreter", "native"] {
        let output = run(
            &[("top.sv", source)],
            &[
                "--backend",
                backend,
                "--save-state-on",
                "Top.clk",
                "--save-state-at",
                "2,5",
                "--save-state-at-time",
                "32",
                "--save-state-dir",
                directory,
            ],
        );
        assert!(output.status.success(), "{backend}: {output:?}");
        // Cycle N is the N-th rising edge of clk, at 10N - 5; the file holds
        // the state after that time step, its nonblocking updates included.
        for (file, time, count) in [
            ("Top.clk-2.state", 15, 2),
            ("Top.clk-5.state", 45, 5),
            // Time 32 has no activity; the state is that after time 30.
            ("time-32.state", 30, 3),
        ] {
            let dump = std::process::Command::new(env!("CARGO_BIN_EXE_celox"))
                .args(["state", "dump"])
                .arg(states.path().join(file))
                .output()
                .unwrap();
            assert!(dump.status.success(), "{backend} {file}: {dump:?}");
            let text = String::from_utf8_lossy(&dump.stdout);
            assert!(
                text.contains(&format!("count[8] = {count:02x}\n")),
                "{backend} {file}:\n{text}"
            );
            assert!(
                text.contains(&format!("time: {time}\n")),
                "{backend} {file}:\n{text}"
            );
        }
    }
    let output = run(
        &[("top.sv", source)],
        &[
            "--save-state-on",
            "clk",
            "--save-state-at",
            "50",
            "--save-state-dir",
            directory,
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("before cycle 50"),
        "{output:?}"
    );
}

#[test]
fn sv_plusargs_reach_the_testbench_in_order() {
    let source = r#"module Top;
  int iterations;
  logic [8*8-1:0] mode;
  int verbose;
  initial begin
    if (!$value$plusargs("iterations=%d", iterations)) $fatal(1, "no iterations");
    if (!$value$plusargs("mode=%s", mode)) $fatal(1, "no mode");
    verbose = $test$plusargs("verbose");
    $display("iterations=%0d mode=%0s verbose=%0d", iterations, mode, verbose);
  end
endmodule
"#;
    for backend in ["tiered", "interpreter", "native"] {
        // The first match wins, so the order of the two kinds matters.
        let output = run(
            &[("top.sv", source)],
            &[
                "+iterations=9",
                "--plusarg",
                "+mode=fast",
                "+iterations=7000",
                "--plusarg=+verbose",
                "--backend",
                backend,
            ],
        );
        assert!(output.status.success(), "{backend}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "iterations=9 mode=fast verbose=1\n",
            "{backend}"
        );
    }
    let output = run(
        &[("top.sv", source)],
        &["--plusarg", "+mode=slow", "+iterations=7000"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "iterations=7000 mode=slow verbose=0\n"
    );
}

#[cfg(unix)]
#[test]
fn sigusr1_checkpoints_by_default_and_resumes_in_a_new_process() {
    use std::{
        io::BufRead,
        process::{Child, Stdio},
        sync::mpsc,
        time::Duration,
    };
    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for (backend, source_waveform) in [
        "interpreter",
        "native",
        "tiered",
        #[cfg(feature = "cranelift")]
        "cranelift",
        #[cfg(feature = "wasm-runtime")]
        "wasm",
    ]
    .into_iter()
    .flat_map(|backend| [true, false].map(|source_waveform| (backend, source_waveform)))
    {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("top.sv");
        let output_path = dir.path().join("output.log");
        let waveform_path = dir.path().join("trace.vcd");
        let dump_tasks = if source_waveform {
            format!(
                "initial begin $dumpfile(\"{}\"); $dumpvars(0, Top); end",
                waveform_path.display()
            )
        } else {
            String::new()
        };
        std::fs::write(
            &source,
            format!(
                r#"
module Top(output longint unsigned count);
 timeunit 1ns; timeprecision 1ns;
 int fd = $fopen("{}", "w");
 {dump_tasks}
 always #1 begin count++; $display("%0d", count); end
endmodule
"#,
                output_path.display()
            ),
        )
        .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_celox"));
        command.arg("run");
        if !source_waveform {
            command.arg("--vcd").arg(&waveform_path);
        }
        let mut process = Running(
            command
                .arg(&source)
                .args([
                    "--top",
                    "Top",
                    "--backend",
                    backend,
                    "--unlimited",
                    "--save-state-dir",
                ])
                .arg(dir.path())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let stderr = process.0.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let wait = |message: &str| {
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            let mut logs = String::new();
            loop {
                let line = rx
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .unwrap_or_else(|error| panic!("{backend}: {message}: {error}; {logs}"));
                logs.push_str(&line);
                logs.push('\n');
                if line.contains(message) {
                    break;
                }
            }
        };
        wait("SIGUSR1 checkpoint ready");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !waveform_path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "waveform did not start: {backend}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid = process.0.id();
        // SAFETY: signal a live child owned by this test.
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGUSR1) }, 0);
        wait("saved SIGUSR1 checkpoint");
        assert!(process.0.try_wait().unwrap().is_none());
        let checkpoint = dir.path().join(format!("signal-{pid}-1.checkpoint"));
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&checkpoint).unwrap()).unwrap();
        let time = state["time"].as_u64().unwrap();
        // A failed request must leave the old file and running simulation intact.
        let collision = dir.path().join(format!("signal-{pid}-2.checkpoint"));
        std::fs::write(&collision, b"keep").unwrap();
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGUSR1) }, 0);
        wait("cannot save SIGUSR1 checkpoint");
        assert_eq!(std::fs::read(&collision).unwrap(), b"keep");
        assert!(process.0.try_wait().unwrap().is_none());
        drop(process);
        reader.join().unwrap();
        std::fs::write(&output_path, b"delivered after checkpoint").unwrap();
        let original_waveform = std::fs::read(&waveform_path).unwrap();
        let resumed_path = dir.path().join("resumed.vcd");
        let mut command = Command::new(env!("CARGO_BIN_EXE_celox"));
        command.arg("run");
        if !source_waveform {
            command.arg("--vcd").arg(&resumed_path);
        }
        let resumed = command
            .arg(&source)
            .args([
                "--top",
                "Top",
                "--backend",
                backend,
                "--max-steps",
                "3",
                "--no-checkpoint-on-signal",
                "--restore-checkpoint",
            ])
            .arg(&checkpoint)
            .output()
            .unwrap();
        assert_eq!(
            std::fs::read(&output_path).unwrap(),
            b"delivered after checkpoint"
        );
        assert_eq!(std::fs::read(&waveform_path).unwrap(), original_waveform);
        let segments = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("trace.vcd.resume-")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            segments.len(),
            usize::from(source_waveform),
            "{backend}: {}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let path = if source_waveform {
            segments[0].path()
        } else {
            resumed_path
        };
        let waveform = std::fs::read_to_string(path).unwrap();
        assert!(waveform.contains("$enddefinitions $end"), "{waveform}");
        let output = String::from_utf8(resumed.stdout).unwrap();
        let counts: Vec<u64> = output.lines().map(|line| line.parse().unwrap()).collect();
        // A checkpoint before the initial time-zero slot needs one step to
        // start the always process; otherwise all three steps advance time.
        assert!((2..=3).contains(&counts.len()), "{output}");
        assert!(
            waveform.contains(&format!("#{}\n", time + counts.len() as u64)),
            "{backend} source_waveform={source_waveform}: {waveform}"
        );
        assert_eq!(
            counts,
            (time + 1..=time + counts.len() as u64).collect::<Vec<_>>()
        );
        assert!(String::from_utf8_lossy(&resumed.stderr).contains("exceeded --max-steps 3"));
    }
}

#[test]
fn sv_report_exposes_actual_compiled_scheduler_execution() {
    let source = r#"module Top;
        bit clk = 0;
        bit [7:0] q = 0;
        always #1 clk = !clk;
        always_ff @(posedge clk) q <= q + 1;
        initial begin #20; if (q != 10) $fatal(1, "bad count"); $finish; end
    endmodule"#;
    let output = run(&[("top.sv", source)], &["--backend", "native", "--report"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let line = stdout
        .lines()
        .find(|line| line.starts_with("CELOX_SCHEDULER phase=finished "))
        .unwrap();
    let slots: u64 = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("compiled_slots="))
        .unwrap()
        .parse()
        .unwrap();
    assert!(slots > 0, "{stdout}");
    let field = |name| -> u64 {
        line.split_whitespace()
            .find_map(|field| field.strip_prefix(name))
            .unwrap()
            .parse()
            .unwrap()
    };
    assert!(field("compiled_runs=") > 0, "{stdout}");
    assert!(field("reference_steps=") > 0, "{stdout}");
    assert_eq!(field("native_service_calls="), 0, "{stdout}");
}

fn waveform_body(text: &str) -> &str {
    text.split_once("$enddefinitions").unwrap().1
}

#[test]
fn veryl_vcd_records_clock_phases_across_backends() {
    let source = r#"
        module Counter(clk: input clock, q: output logic<8>) {
            always_ff (clk) { q += 1; }
        }
        #[test(Top)] module Top {
            inst clk: $tb::clock_gen #(period: 2);
            var q: logic<8>;
            inst dut: Counter(clk, q);
            initial { clk.next(4); $assert(q == 4); $finish(); }
            initial { clk.next(100); }
        }
    "#;
    let dir = tempfile::tempdir().unwrap();
    let mut reference = None;
    for backend in [
        "interpreter",
        "native",
        "tiered",
        #[cfg(feature = "cranelift")]
        "cranelift",
        #[cfg(feature = "wasm-runtime")]
        "wasm",
    ] {
        let path = dir.path().join(format!("{backend}.vcd"));
        let output = run(
            &[("top.veryl", source)],
            &["--backend", backend, "--vcd", path.to_str().unwrap()],
        );
        assert!(output.status.success(), "{backend}: {output:?}");
        let text = std::fs::read_to_string(path).unwrap();
        let body = waveform_body(&text);
        for time in 0..=7 {
            assert!(
                body.lines().any(|line| line == format!("#{time}")),
                "{backend}: {text}"
            );
        }
        if let Some(reference) = &reference {
            assert_eq!(body, reference, "{backend}");
        } else {
            reference = Some(body.to_string());
        }
    }
}

#[test]
fn sv_vcd_preserves_compiled_slots_and_matches_interpreter() {
    let source = r#"`timescale 1ns/1ns
    module Top;
        bit clk = 0;
        bit [7:0] q = 0;
        always #1 clk = !clk;
        always_ff @(posedge clk) q <= q + 1;
        initial begin #20; if (q != 10) $fatal(1, "bad count"); $finish; end
    endmodule"#;
    let dir = tempfile::tempdir().unwrap();
    let mut reference = None;
    for backend in ["interpreter", "native", "tiered"] {
        let path = dir.path().join(format!("{backend}.vcd"));
        let output = run(
            &[("top.sv", source)],
            &[
                "--backend",
                backend,
                "--report",
                "--vcd",
                path.to_str().unwrap(),
            ],
        );
        assert!(output.status.success(), "{backend}: {output:?}");
        if backend == "native" {
            let stdout = String::from_utf8(output.stdout).unwrap();
            let line = stdout
                .lines()
                .find(|line| line.starts_with("CELOX_SCHEDULER phase=finished "))
                .unwrap();
            let slots: u64 = line
                .split_whitespace()
                .find_map(|field| field.strip_prefix("compiled_slots="))
                .unwrap()
                .parse()
                .unwrap();
            assert!(slots > 0, "{stdout}");
        }
        let text = std::fs::read_to_string(path).unwrap();
        let body = waveform_body(&text);
        for time in 0..=20 {
            assert!(
                body.lines().any(|line| line == format!("#{time}")),
                "{backend}: {text}"
            );
        }
        if let Some(reference) = &reference {
            assert_eq!(body, reference, "{backend}");
        } else {
            reference = Some(body.to_string());
        }
    }
}

#[test]
fn vcd_rejects_compile_only_and_reports_invalid_output_paths() {
    let source = "#[test(Top)] module Top { initial { $finish(); } }";
    let output = run(
        &[("top.veryl", source)],
        &["--compile-only", "--vcd", "unused.vcd"],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        &[("top.veryl", source)],
        &["--vcd", dir.path().to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
}

#[test]
fn fst_records_each_time_step_and_rejects_conflicting_options() {
    let source = r#"
        module Counter(clk: input clock, q: output logic<8>) {
            always_ff (clk) { q += 1; }
        }
        #[test(Top)] module Top {
            inst clk: $tb::clock_gen #(period: 2);
            var q: logic<8>;
            inst dut: Counter(clk, q);
            initial { clk.next(4); $assert(q == 4); $finish(); }
            initial { clk.next(100); }
        }
    "#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("counter.fst");
    let output = run(
        &[("top.veryl", source)],
        &["--backend", "native", "--fst", path.to_str().unwrap()],
    );
    assert!(output.status.success(), "{output:?}");

    let mut wave = wellen::simple::read(&path).unwrap();
    for time in 0..=7u64 {
        assert!(
            wave.time_table().contains(&time),
            "missing time {time}: {:?}",
            wave.time_table()
        );
    }
    let hierarchy = wave.hierarchy();
    let q = hierarchy
        .all_vars()
        .find(|var_ref| hierarchy[*var_ref].full_name(hierarchy).ends_with(".q"))
        .expect("counter output recorded");
    let id = hierarchy[q].signal_ref();
    wave.load_signals(&[id]);
    let times: Vec<u64> = wave.time_table().to_vec();
    let values: Vec<String> = wave
        .get_signal(id)
        .unwrap()
        .iter_changes()
        .map(|(_, value)| value.to_bit_string().unwrap())
        .collect();
    assert!(
        values.iter().any(|value| value == "00000100"),
        "final count recorded, times {times:?}: {values:?}"
    );

    let output = run(
        &[("top.veryl", source)],
        &["--compile-only", "--fst", "unused.fst"],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let output = run(
        &[("top.veryl", source)],
        &["--vcd", "unused.vcd", "--fst", "unused.fst"],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

#[test]
fn vcd_is_flushed_when_a_veryl_run_hits_the_tick_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("limited.vcd");
    let source = "#[test(Top)] module Top { inst clk: $tb::clock_gen #(period: 2); initial { clk.next(100); $finish(); } }";
    let output = run(
        &[("top.veryl", source)],
        &[
            "--backend",
            "native",
            "--max-steps",
            "3",
            "--vcd",
            path.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let text = std::fs::read_to_string(path).unwrap();
    assert!(
        waveform_body(&text)
            .lines()
            .any(|line| line.starts_with('#')),
        "{text}"
    );
}

#[test]
fn vcd_is_flushed_when_an_sv_run_fails_or_hits_the_step_limit() {
    let dir = tempfile::tempdir().unwrap();
    for ending in ["#3 $fatal(1, \"failure\");", "#100 $finish;"] {
        let path = dir.path().join("failed.vcd");
        let source = format!(
            "`timescale 1ns/1ns\nmodule Top; bit clk; always #1 clk = !clk; initial begin {ending} end endmodule"
        );
        let output = run(
            &[("top.sv", &source)],
            &[
                "--backend",
                "native",
                "--max-steps",
                "5",
                "--vcd",
                path.to_str().unwrap(),
            ],
        );
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            waveform_body(&text).lines().any(|line| line == "#2"),
            "{text}"
        );
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn jit_strategy_defaults_and_overrides_reach_both_frontends() {
    for (name, source) in [
        (
            "top.veryl",
            "#[test(Top)] module Top { initial { $finish(); } }",
        ),
        ("top.sv", "module Top; initial $finish; endmodule"),
    ] {
        for (args, expected) in [
            (vec![], "hotspot"),
            (vec!["--jit", "auto"], "hotspot"),
            (vec!["--jit", "hotspot"], "hotspot"),
            (vec!["--hotspot-jit"], "hotspot"),
            (vec!["--jit", "whole-image"], "whole-image"),
            (vec!["--threads", "2"], "whole-image"),
            (vec!["--backend", "native"], "none"),
            (vec!["--backend", "interpreter"], "none"),
        ] {
            let mut args = args;
            args.push("--report");
            let output = run(&[(name, source)], &args);
            assert!(output.status.success(), "{name} {args:?}: {output:?}");
            let report = String::from_utf8(output.stdout).unwrap();
            assert!(
                report.contains(&format!("CELOX_JIT_CONFIG mode={expected} ")),
                "{report}"
            );
            assert_eq!(
                report.contains("CELOX_HOTSPOT_STATS "),
                expected == "hotspot",
                "{report}"
            );
        }
        for args in [
            vec!["--jit", "hotspot", "--threads", "2"],
            vec!["--jit", "hotspot", "--backend", "native"],
            vec!["--hotspot-jit", "--jit", "whole-image"],
            vec!["--jit", "whole-image", "--backend", "interpreter"],
        ] {
            let output = run(&[(name, source)], &args);
            assert_eq!(output.status.code(), Some(2), "{name} {args:?}: {output:?}");
        }
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn require_compiled_accepts_hotspot_execution_and_rejects_cold_runs() {
    let args = [
        "--report",
        "--require-compiled",
        "--jit",
        "hotspot",
        "--unlimited",
    ];
    let cold = run(
        &[(
            "top.veryl",
            "#[test(Top)] module Top { initial { $finish(); } }",
        )],
        &args,
    );
    assert!(!cold.status.success(), "{cold:?}");
    assert!(
        String::from_utf8_lossy(&cold.stderr).contains("did not exercise generated code"),
        "{cold:?}"
    );
    let source = "module Counter(clk: input clock, q: output logic<32>) { always_ff { q += 1; } }
        #[test(Top)] module Top {
            inst clk: $tb::clock_gen;
            var q: logic<32>;
            inst counter: Counter(clk, q);
            initial { clk.next(200000); $assert(q == 200000); $finish(); }
        }";
    let hot = run(&[("top.veryl", source)], &args);
    assert!(hot.status.success(), "{hot:?}");
    assert!(
        String::from_utf8_lossy(&hot.stdout).contains("CELOX_HOTSPOT_STATS "),
        "{hot:?}"
    );
}

#[test]
fn repeated_tops_run_together() {
    // `--top` repeated names several top-level modules (IEEE 1800-2023
    // 23.3.1), each an instance of its own name.
    let source = r#"
        package shared; int value; endpackage
        module Top; initial begin shared::value = 7; $display("%m set"); end endmodule
        module Other; initial #1 begin $display("%m read %0d", shared::value); $finish; end endmodule
    "#;
    let output = run(&[("tops.sv", source)], &["--top", "Other"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"Top set\nOther read 7\n");
}

#[test]
fn sv_cdsp_exposes_live_waveforms_and_controls_simulation() {
    use std::{
        fmt::Write as _,
        io::{BufRead, BufReader, Write},
        net::{SocketAddr, TcpStream},
        process::{Child, Stdio},
        time::Duration,
    };

    use serde_json::{Value, json};

    struct Server(Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn send(stream: &mut impl Write, message: Value) {
        serde_json::to_writer(&mut *stream, &message).unwrap();
        stream.write_all(&[0]).unwrap();
    }

    fn receive(stream: &mut impl BufRead) -> Value {
        let mut frame = Vec::new();
        assert!(stream.read_until(0, &mut frame).unwrap() > 0);
        assert_eq!(frame.pop(), Some(0));
        serde_json::from_slice(&frame).unwrap()
    }

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("top.sv");
    std::fs::write(
        &source,
        r#"
        `timescale 1ns/1ps
        module Dut(input logic [64:0] data);
        endmodule
        module Top;
            logic [64:0] counter;
            Dut dut(counter);
            initial begin
                counter = 65'h1_00000000_12345678;
                #2 counter = 65'h0_00000001_00000002;
                #3 begin
                    $display("reached breakpoint");
                    $stop;
                    counter = 65'h1_00000002_00000003;
                end
                #2 $finish;
            end
        endmodule
        "#,
    )
    .unwrap();

    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_celox"))
            .arg("run")
            .arg(&source)
            .args(["--top", "Top", "--cdsp", "0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let (ready, address) = std::sync::mpsc::channel();
    let stderr = server.0.stderr.take().unwrap();
    let log = std::thread::spawn(move || {
        let mut transcript = String::new();
        for line in BufReader::new(stderr).lines() {
            let line = line.unwrap();
            if let Some(addr) = line.strip_prefix("CDSP listening at cxxrtl+tcp://") {
                let _ = ready.send(addr.parse::<SocketAddr>().unwrap());
            }
            writeln!(&mut transcript, "{line}").unwrap();
        }
        transcript
    });
    let timeout = Duration::from_secs(15);
    let address = address
        .recv_timeout(timeout)
        .expect("Celox did not start CDSP");
    let mut write = TcpStream::connect_timeout(&address, timeout).unwrap();
    write.set_read_timeout(Some(timeout)).unwrap();
    write.set_write_timeout(Some(timeout)).unwrap();
    let mut read = BufReader::new(write.try_clone().unwrap());
    send(&mut write, json!({ "type": "greeting", "version": 0 }));
    assert_eq!(receive(&mut read)["type"], "greeting");

    let command = |write: &mut TcpStream, read: &mut BufReader<TcpStream>, request: Value| {
        let mut request = request;
        request["type"] = json!("command");
        send(write, request.clone());
        let response = receive(read);
        assert_eq!(response["type"], "response", "{response}");
        assert_eq!(response["command"], request["command"]);
        response
    };

    let status = command(
        &mut write,
        &mut read,
        json!({ "command": "get_simulation_status" }),
    );
    assert_eq!(status["status"], "paused");
    assert_eq!(status["latest_time"], "0.000000000000000");
    assert_eq!(status["next_sample_time"], "0.000000002000000");
    let scopes = command(
        &mut write,
        &mut read,
        json!({ "command": "list_scopes", "scope": null }),
    );
    assert_eq!(scopes["scopes"]["dut"]["definition"]["name"], "Dut");
    let items = command(
        &mut write,
        &mut read,
        json!({ "command": "list_items", "scope": null }),
    );
    assert_eq!(items["items"]["dut data"]["width"], 65);
    command(
        &mut write,
        &mut read,
        json!({
            "command": "reference_items", "reference": "signals", "items": [["counter"], ["dut data"]],
        }),
    );
    command(
        &mut write,
        &mut read,
        json!({
            "command": "run_simulation", "until_time": "0.000000003000000",
            "until_diagnostics": [], "sample_item_values": true,
        }),
    );
    let event = receive(&mut read);
    assert_eq!(event["event"], "simulation_paused");
    assert_eq!(event["cause"], "until_time");
    assert_eq!(event["time"], "0.000000002000000");

    // Pipeline run and pause in one write. Both commands are accepted before
    // the simulator advances beyond this slot.
    let run = json!({
        "type": "command", "command": "run_simulation", "until_time": null,
        "until_diagnostics": [], "sample_item_values": true,
    });
    let pause = json!({ "type": "command", "command": "pause_simulation" });
    write
        .write_all(format!("{run}\0{pause}\0").as_bytes())
        .unwrap();
    assert_eq!(receive(&mut read)["command"], "run_simulation");
    let paused = receive(&mut read);
    assert_eq!(paused["command"], "pause_simulation");
    assert_eq!(paused["time"], "0.000000002000000");

    send(&mut write, run.clone());
    assert_eq!(receive(&mut read)["command"], "run_simulation");
    let event = receive(&mut read);
    assert_eq!(event["event"], "simulation_paused");
    assert_eq!(event["cause"], "until_diagnostics");
    assert_eq!(event["time"], "0.000000005000000");
    send(&mut write, run);
    assert_eq!(receive(&mut read)["command"], "run_simulation");
    let event = receive(&mut read);
    assert_eq!(event["event"], "simulation_finished");
    assert_eq!(event["time"], "0.000000007000000");
    send(
        &mut write,
        json!({
            "type": "command", "command": "query_interval",
            "interval": ["0.0", "0.000000007000000"], "collapse": true,
            "items": "signals", "item_values_encoding": "base64(u32)", "diagnostics": true,
        }),
    );
    let wave = receive(&mut read);
    let samples = wave["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 4);
    assert_eq!(
        samples[0]["item_values"],
        "eFY0EgAAAAABAAAAeFY0EgAAAAABAAAA"
    );
    assert_eq!(
        samples[1]["item_values"],
        "AgAAAAEAAAAAAAAAAgAAAAEAAAAAAAAA"
    );
    assert_eq!(samples[2]["diagnostics"][0]["text"], "reached breakpoint\n");
    assert_eq!(samples[2]["diagnostics"][1]["type"], "break");
    assert_eq!(
        samples[3]["item_values"],
        "AwAAAAIAAAABAAAAAwAAAAIAAAABAAAA"
    );

    drop((read, write));
    let result = server.0.wait().unwrap();
    assert!(result.success(), "{}", log.join().unwrap());
}

#[test]
fn include_trailing_semicolons_warn_by_default_and_error_in_strict_mode() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("value.svh"), "`define VALUE 7\n").unwrap();
    let top = directory.path().join("top.sv");
    std::fs::write(
        &top,
        "`include \"value.svh\";\nmodule Top; initial begin $display(\"%0d\", `VALUE); $finish; end endmodule\n",
    )
    .unwrap();
    for (extra, success, warning) in [
        (&[][..], true, true),
        (
            &["--diagnostic", "include_trailing_semicolon=allow"][..],
            true,
            false,
        ),
        (&["--strict"][..], false, false),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_celox"))
            .arg("run")
            .arg(&top)
            .arg("-I")
            .arg(directory.path())
            .args(["--top", "Top", "--backend", "interpreter"])
            .args(extra)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.success(), success, "{extra:?}: {output:?}");
        assert_eq!(
            stderr.contains("include_trailing_semicolon"),
            warning || !success,
            "{extra:?}: {stderr}"
        );
        if success {
            assert_eq!(output.stdout, b"7\n", "{extra:?}: {output:?}");
        }
    }
}

/// `--coverage-report` writes the covergroup coverage as pretty JSON when
/// the run ends: the format is the interface, pinned here exactly. The
/// design's output stays alone on stdout.
#[test]
fn sv_coverage_report_is_written_after_the_run() {
    let source = r#"module Top;
    bit [1:0] v;
    bit w;
    covergroup cg;
        cp: coverpoint v {
            bins lo = {0};
            bins hi = {1};
            illegal_bins bad = {3};
        }
        cw: coverpoint w;
        x: cross cp, cw;
    endgroup
    cg c = new;
    initial begin
        v = 0; w = 1; c.sample();
        $display("sampled");
    end
endmodule
"#;
    let temporary = tempfile::tempdir().unwrap();
    let report = temporary.path().join("coverage.json");
    let output = run(
        &[("top.sv", source)],
        &["--coverage-report", report.to_str().unwrap()],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"sampled\n");
    // The JSON of `coverage_json()` in `celox-napi`, pretty-printed and
    // ended by a newline.
    assert_eq!(
        std::fs::read_to_string(&report).unwrap(),
        concat!(
            r#"{
  "coverage": 41.666666666666664,
  "covergroups": [
    {
      "name": "Top.cg",
      "comment": "",
      "weight": 1,
      "goal": 100,
      "mergeInstances": false,
      "coverage": 41.666666666666664,
      "covered": 3,
      "total": 8,
      "items": [
        {
          "name": "cp",
          "kind": "coverpoint",
          "coverpoints": [],
          "comment": "",
          "weight": 1,
          "goal": 100,
          "atLeast": 1,
          "coverage": 50.0,
          "covered": 1,
          "total": 2,
          "bins": [
            {
              "name": "lo",
              "kind": "bins",
              "hits": 1,
              "covered": true
            },
            {
              "name": "hi",
              "kind": "bins",
              "hits": 0,
              "covered": false
            },
            {
              "name": "bad",
              "kind": "illegal",
              "hits": 0,
              "covered": false
            }
          ],
          "omittedBins": 0
        },
        {
          "name": "cw",
          "kind": "coverpoint",
          "coverpoints": [],
          "comment": "",
          "weight": 1,
          "goal": 100,
          "atLeast": 1,
          "coverage": 50.0,
          "covered": 1,
          "total": 2,
          "bins": [
            {
              "name": "auto[0]",
              "kind": "bins",
              "hits": 0,
              "covered": false
            },
            {
              "name": "auto[1]",
              "kind": "bins",
              "hits": 1,
              "covered": true
            }
          ],
          "omittedBins": 0
        },
        {
          "name": "x",
          "kind": "cross",
          "coverpoints": [
            "cp",
            "cw"
          ],
          "comment": "",
          "weight": 1,
          "goal": 100,
          "atLeast": 1,
          "coverage": 25.0,
          "covered": 1,
          "total": 4,
          "bins": [
            {
              "name": "<lo,auto[1]>",
              "kind": "bins",
              "hits": 1,
              "covered": true
            }
          ],
          "omittedBins": 3
        }
      ],
      "instances": [
        {
          "name": "Top.cg#0",
          "comment": "",
          "weight": 1,
          "goal": 100,
          "atLeast": 1,
          "perInstance": false,
          "collecting": true,
          "live": true,
          "samples": 1,
          "coverage": 41.666666666666664,
          "covered": 3,
          "total": 8,
          "items": [
            {
              "name": "cp",
              "kind": "coverpoint",
              "coverpoints": [],
              "comment": "",
              "weight": 1,
              "goal": 100,
              "atLeast": 1,
              "coverage": 50.0,
              "covered": 1,
              "total": 2,
              "bins": [
                {
                  "name": "lo",
                  "kind": "bins",
                  "hits": 1,
                  "covered": true
                },
                {
                  "name": "hi",
                  "kind": "bins",
                  "hits": 0,
                  "covered": false
                },
                {
                  "name": "bad",
                  "kind": "illegal",
                  "hits": 0,
                  "covered": false
                }
              ],
              "omittedBins": 0
            },
            {
              "name": "cw",
              "kind": "coverpoint",
              "coverpoints": [],
              "comment": "",
              "weight": 1,
              "goal": 100,
              "atLeast": 1,
              "coverage": 50.0,
              "covered": 1,
              "total": 2,
              "bins": [
                {
                  "name": "auto[0]",
                  "kind": "bins",
                  "hits": 0,
                  "covered": false
                },
                {
                  "name": "auto[1]",
                  "kind": "bins",
                  "hits": 1,
                  "covered": true
                }
              ],
              "omittedBins": 0
            },
            {
              "name": "x",
              "kind": "cross",
              "coverpoints": [
                "cp",
                "cw"
              ],
              "comment": "",
              "weight": 1,
              "goal": 100,
              "atLeast": 1,
              "coverage": 25.0,
              "covered": 1,
              "total": 4,
              "bins": [
                {
                  "name": "<lo,auto[1]>",
                  "kind": "bins",
                  "hits": 1,
                  "covered": true
                }
              ],
              "omittedBins": 3
            }
          ]
        }
      ]
    }
  ]
}"#,
            "\n"
        )
    );
}

/// The bins of the first item of the first instance in a coverage report,
/// with their hits.
fn coverage_hits(report: &serde_json::Value) -> Vec<(String, u64)> {
    report["covergroups"][0]["instances"][0]["items"][0]["bins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|bin| {
            (
                bin["name"].as_str().unwrap().to_owned(),
                bin["hits"].as_u64().unwrap(),
            )
        })
        .collect()
}

/// Samples `v` at times 0, 1 and 2, one value each.
const COVERAGE_SAMPLES_SOURCE: &str = "`timescale 1ns/1ns
module Top;
  bit [1:0] v;
  covergroup cg; cp: coverpoint v; endgroup
  cg c = new;
  initial repeat (3) begin c.sample(); v++; #1; end
endmodule
";

/// A run that fails on an illegal bin hit still writes its report, which
/// counts the hit; a design without covergroups writes an empty one.
#[test]
fn sv_coverage_report_is_written_when_the_run_fails() {
    let source = r#"module Top;
    bit [1:0] v;
    covergroup cg;
        cp: coverpoint v {
            bins lo = {0};
            illegal_bins bad = {3};
        }
    endgroup
    cg c = new;
    initial begin
        v = 3; c.sample();
        v = 0; c.sample();
    end
endmodule
"#;
    let temporary = tempfile::tempdir().unwrap();
    let report = temporary.path().join("coverage.json");
    let output = run(
        &[("top.sv", source)],
        &["--coverage-report", report.to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("illegal_bins `bad`"),
        "{output:?}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    let instance = &json["covergroups"][0]["instances"][0];
    assert_eq!(instance["samples"], 2);
    assert_eq!(
        coverage_hits(&json),
        [("lo".to_owned(), 1), ("bad".to_owned(), 1)]
    );

    // `$fatal` and the step limit end the run with its samples so far,
    // which replace the report of the run before.
    for (ending, extra, hits) in [
        ("$fatal(1, \"stop\");", &[][..], [1, 1, 0, 0]),
        (
            "forever #1 begin v = 2; c.sample(); end",
            &["--max-steps", "4"][..],
            [1, 1, 3, 0],
        ),
    ] {
        let source = format!(
            "`timescale 1ns/1ns\nmodule Top; bit [1:0] v; \
             covergroup cg; cp: coverpoint v; endgroup cg c = new; \
             initial begin c.sample(); v = 1; c.sample(); {ending} end endmodule"
        );
        let mut arguments = vec!["--coverage-report", report.to_str().unwrap()];
        arguments.extend_from_slice(extra);
        let output = run(&[("top.sv", &source)], &arguments);
        assert_eq!(output.status.code(), Some(1), "{ending}: {output:?}");
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
        let expected: Vec<_> = hits
            .iter()
            .enumerate()
            .map(|(value, &hits)| (format!("auto[{value}]"), hits))
            .collect();
        assert_eq!(coverage_hits(&json), expected, "{ending}");
        assert_eq!(
            json["covergroups"][0]["instances"][0]["samples"],
            hits.iter().sum::<u64>(),
            "{ending}"
        );
    }

    let empty = temporary.path().join("empty.json");
    let output = run(
        &[(
            "top.sv",
            "module Top; initial $display(\"done\"); endmodule",
        )],
        &["--coverage-report", empty.to_str().unwrap()],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(&empty).unwrap(),
        "{\n  \"coverage\": 100.0,\n  \"covergroups\": []\n}\n"
    );
}

/// A report that cannot be written fails the run after it ran; a Veryl
/// run has no covergroups and rejects the option.
#[test]
fn sv_coverage_report_errors() {
    let temporary = tempfile::tempdir().unwrap();
    let unwritable = temporary.path().join("missing").join("coverage.json");
    let output = run(
        &[(
            "top.sv",
            "module Top; initial $display(\"done\"); endmodule",
        )],
        &["--coverage-report", unwritable.to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(output.stdout, b"done\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot write the coverage report"),
        "{stderr}"
    );
    // A failed run that cannot write its report names both.
    let output = run(
        &[(
            "top.sv",
            "module Top; initial $error(\"failed\"); endmodule",
        )],
        &["--coverage-report", unwritable.to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "reported an assertion failure or $error\ncelox: cannot write the coverage report"
        ),
        "{stderr}"
    );

    let output = run(
        &[("top.veryl", "module Top {}")],
        &["--coverage-report", "coverage.json"],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("--coverage-report requires a SystemVerilog testbench"),
        "{output:?}"
    );
}

/// A run that does not start writes no report: a file at the path is left
/// byte for byte as it was, and none is created where there was none.
/// That is a compilation error, a checkpoint that cannot be restored, a
/// `--save-state-on` signal that does not exist, a CDSP port that cannot
/// be opened, and invalid arguments (status 2).
#[test]
fn sv_coverage_report_of_a_run_that_does_not_start_is_untouched() {
    let temporary = tempfile::tempdir().unwrap();
    let report = temporary.path().join("coverage.json");
    let junk = temporary.path().join("junk.checkpoint");
    std::fs::write(&junk, "garbage").unwrap();
    let busy = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = busy.local_addr().unwrap().port().to_string();
    let bad = "module Top; initial undeclared = 1; endmodule";
    let cases: [(&str, &str, Vec<&str>, i32); 5] = [
        ("compilation", bad, vec![], 1),
        (
            "restore",
            COVERAGE_SAMPLES_SOURCE,
            vec!["--restore-checkpoint", junk.to_str().unwrap()],
            1,
        ),
        (
            "save-state-on",
            COVERAGE_SAMPLES_SOURCE,
            vec!["--save-state-on", "Top.nosuch", "--save-state-at", "1"],
            1,
        ),
        ("cdsp", COVERAGE_SAMPLES_SOURCE, vec!["--cdsp", &port], 1),
        (
            "arguments",
            COVERAGE_SAMPLES_SOURCE,
            vec!["--jit", "hotspot", "--backend", "native"],
            2,
        ),
    ];
    let earlier = b"{\"earlier\": true}\n";
    for (case, source, extra, status) in cases {
        for existing in [true, false] {
            if existing {
                std::fs::write(&report, earlier).unwrap();
            } else if report.exists() {
                std::fs::remove_file(&report).unwrap();
            }
            let mut arguments = vec!["--coverage-report", report.to_str().unwrap()];
            arguments.extend(extra.iter().copied());
            let output = run(&[("top.sv", source)], &arguments);
            assert_eq!(output.status.code(), Some(status), "{case}: {output:?}");
            if existing {
                assert_eq!(std::fs::read(&report).unwrap(), earlier, "{case}");
            } else {
                assert!(!report.exists(), "{case}: {output:?}");
            }
        }
    }
    drop(busy);
}

/// A report path that is a file the project resolution read, the manifest
/// found without `--project` or a file list named directly or by another
/// file list, is rejected and the file is kept.
#[test]
fn sv_coverage_report_must_not_name_a_resolved_project_file() {
    let temporary = tempfile::tempdir().unwrap();
    let dir = temporary.path();
    std::fs::write(dir.join("top.sv"), COVERAGE_SAMPLES_SOURCE).unwrap();
    std::fs::write(dir.join("inner.f"), "top.sv\n").unwrap();
    std::fs::write(dir.join("outer.f"), "-f inner.f\n").unwrap();
    let manifest = "[targets.sim]\ntop = \"Top\"\nfilelists = [\"outer.f\"]\n";
    std::fs::write(dir.join("celox.toml"), manifest).unwrap();
    for (arguments, file, what) in [
        (
            &["--target", "sim", "--coverage-report", "celox.toml"][..],
            "celox.toml",
            "the project or a file list",
        ),
        (
            &["--target", "sim", "--coverage-report", "inner.f"][..],
            "inner.f",
            "the project or a file list",
        ),
        (
            &[
                "-f",
                "outer.f",
                "--top",
                "Top",
                "--coverage-report",
                "outer.f",
            ][..],
            "outer.f",
            "the project or a file list",
        ),
        (
            &[
                "-f",
                "outer.f",
                "--top",
                "Top",
                "--coverage-report",
                "inner.f",
            ][..],
            "inner.f",
            "the project or a file list",
        ),
        (
            &["--target", "sim", "--coverage-report", "top.sv"][..],
            "top.sv",
            "a source",
        ),
    ] {
        let before = std::fs::read(dir.join(file)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_celox"))
            .current_dir(dir)
            .arg("run")
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{arguments:?}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.starts_with(&format!(
                "celox: --coverage-report {file} names the same file as {what} "
            )),
            "{arguments:?}: {stderr}"
        );
        assert_eq!(std::fs::read(dir.join(file)).unwrap(), before, "{file}");
    }
}

/// A report path that another argument names as a file of the run is
/// rejected before anything runs, and that file is kept.
#[test]
fn sv_coverage_report_must_not_name_another_file_of_the_run() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("top.sv");
    std::fs::write(&source, COVERAGE_SAMPLES_SOURCE).unwrap();
    let checkpoint = temporary.path().join("copy.checkpoint");
    std::fs::write(&checkpoint, "keep").unwrap();
    let same = temporary.path().join("same.json");
    std::fs::write(&same, "keep").unwrap();
    let relative = temporary.path().join(".").join("same.json");
    for (option, path, report) in [
        ("--vcd", &same, &same),
        ("--vcd", &same, &relative),
        ("--restore-checkpoint", &checkpoint, &checkpoint),
        ("--settle-profile", &same, &same),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_celox"))
            .arg("run")
            .arg(&source)
            .args(["--top", "Top", option])
            .arg(path)
            .arg("--coverage-report")
            .arg(report)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{option}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.starts_with("celox: --coverage-report ")
                && stderr.contains(&format!("names the same file as {option} ")),
            "{option}: {stderr}"
        );
        assert_eq!(std::fs::read(path).unwrap(), b"keep", "{option}");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_celox"))
        .arg("run")
        .arg(&source)
        .args(["--top", "Top", "--coverage-report"])
        .arg(&source)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("names the same file as a source"),
        "{output:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        COVERAGE_SAMPLES_SOURCE
    );
}

/// A run resumed from a checkpoint reports the samples taken before the
/// checkpoint and those after it.
#[cfg(unix)]
#[test]
fn sv_coverage_report_of_a_restored_run_keeps_earlier_samples() {
    use std::{
        io::BufRead,
        process::{Child, Stdio},
        time::Duration,
    };
    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("top.sv");
    std::fs::write(
        &source,
        "`timescale 1ns/1ns
module Top;
  bit [1:0] v;
  covergroup cg; cp: coverpoint v; endgroup
  cg c = new;
  always #1 begin c.sample(); v++; end
endmodule
",
    )
    .unwrap();
    let mut process = Running(
        Command::new(env!("CARGO_BIN_EXE_celox"))
            .arg("run")
            .arg(&source)
            .args(["--top", "Top", "--unlimited", "--save-state-dir"])
            .arg(dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let stderr = process.0.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let wait = |message: &str| loop {
        let line = rx
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|error| panic!("{message}: {error}"));
        if line.contains(message) {
            break;
        }
    };
    wait("SIGUSR1 checkpoint ready");
    // Let the run take samples before the checkpoint.
    std::thread::sleep(Duration::from_millis(500));
    let pid = process.0.id();
    // SAFETY: signal a live child owned by this test.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGUSR1) }, 0);
    wait("saved SIGUSR1 checkpoint");
    drop(process);
    reader.join().unwrap();
    let checkpoint = dir.path().join(format!("signal-{pid}-1.checkpoint"));
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&checkpoint).unwrap()).unwrap();
    let time = state["time"].as_u64().unwrap();
    let report = dir.path().join("coverage.json");
    let output = Command::new(env!("CARGO_BIN_EXE_celox"))
        .arg("run")
        .arg(&source)
        .args([
            "--top",
            "Top",
            "--max-steps",
            "3",
            "--no-checkpoint-on-signal",
            "--restore-checkpoint",
        ])
        .arg(&checkpoint)
        .arg("--coverage-report")
        .arg(&report)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("exceeded --max-steps 3"),
        "{output:?}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    // One sample at each of the times 1 to `time` before the checkpoint,
    // then at most one in each of the three steps after it.
    let samples = json["covergroups"][0]["instances"][0]["samples"]
        .as_u64()
        .unwrap();
    assert!(
        time > 3 && (time..=time + 3).contains(&samples),
        "{time} {samples}"
    );
    let hits: Vec<_> = (0..4u64)
        .map(|value| (format!("auto[{value}]"), (samples + 3 - value) / 4))
        .collect();
    assert_eq!(coverage_hits(&json), hits);
}

/// A CDSP session writes the report when its client leaves.
#[test]
fn sv_coverage_report_of_a_cdsp_session() {
    use std::{
        io::{BufRead, BufReader, Write},
        net::{SocketAddr, TcpStream},
        process::Stdio,
        time::Duration,
    };
    fn send(stream: &mut impl Write, message: serde_json::Value) {
        serde_json::to_writer(&mut *stream, &message).unwrap();
        stream.write_all(&[0]).unwrap();
    }
    fn receive(stream: &mut impl BufRead) -> serde_json::Value {
        let mut frame = Vec::new();
        assert!(stream.read_until(0, &mut frame).unwrap() > 0);
        assert_eq!(frame.pop(), Some(0));
        serde_json::from_slice(&frame).unwrap()
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("top.sv");
    std::fs::write(&source, COVERAGE_SAMPLES_SOURCE).unwrap();
    let report = dir.path().join("coverage.json");
    let mut server = Command::new(env!("CARGO_BIN_EXE_celox"))
        .arg("run")
        .arg(&source)
        .args(["--top", "Top", "--cdsp", "0", "--coverage-report"])
        .arg(&report)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(server.stderr.take().unwrap());
    let address = loop {
        let mut line = String::new();
        assert!(stderr.read_line(&mut line).unwrap() > 0, "no CDSP listener");
        if let Some(address) = line.trim().strip_prefix("CDSP listening at cxxrtl+tcp://") {
            break address.parse::<SocketAddr>().unwrap();
        }
    };
    let timeout = Duration::from_secs(15);
    let mut write = TcpStream::connect_timeout(&address, timeout).unwrap();
    write.set_read_timeout(Some(timeout)).unwrap();
    let mut read = BufReader::new(write.try_clone().unwrap());
    send(
        &mut write,
        serde_json::json!({ "type": "greeting", "version": 0 }),
    );
    assert_eq!(receive(&mut read)["type"], "greeting");
    send(
        &mut write,
        serde_json::json!({
            "type": "command", "command": "run_simulation", "until_time": null,
            "until_diagnostics": [], "sample_item_values": false,
        }),
    );
    assert_eq!(receive(&mut read)["command"], "run_simulation");
    assert_eq!(receive(&mut read)["event"], "simulation_finished");
    drop((read, write));
    assert!(server.wait().unwrap().success());
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert_eq!(
        coverage_hits(&json),
        [
            ("auto[0]".to_owned(), 1),
            ("auto[1]".to_owned(), 1),
            ("auto[2]".to_owned(), 1),
            ("auto[3]".to_owned(), 0),
        ]
    );
}
