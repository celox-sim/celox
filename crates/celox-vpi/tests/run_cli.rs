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
    assert!(String::from_utf8_lossy(&output.stderr).contains("Veryl project sources"));
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
        &["--report", "--backend", "tiered"],
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

#[cfg(feature = "systemverilog")]
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

#[cfg(feature = "systemverilog")]
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

#[cfg(feature = "systemverilog")]
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
        (None, None, false, false),
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

#[cfg(feature = "systemverilog")]
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

#[cfg(feature = "systemverilog")]
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

#[cfg(all(unix, feature = "systemverilog"))]
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
    for backend in [
        "interpreter",
        "native",
        "tiered",
        #[cfg(feature = "cranelift")]
        "cranelift",
        #[cfg(feature = "wasm-runtime")]
        "wasm",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("top.sv");
        let output_path = dir.path().join("output.log");
        let waveform_path = dir.path().join("trace.vcd");
        std::fs::write(
            &source,
            format!(
                r#"
module Top(output longint unsigned count);
 timeunit 1ns; timeprecision 1ns;
 int fd = $fopen("{}", "w");
 initial begin $dumpfile("{}"); $dumpvars(0, Top); end
 always #1 begin count++; $display("%0d", count); end
endmodule
"#,
                output_path.display(),
                waveform_path.display()
            ),
        )
        .unwrap();
        let mut process = Running(
            Command::new(env!("CARGO_BIN_EXE_celox"))
                .arg("run")
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
        let resumed = Command::new(env!("CARGO_BIN_EXE_celox"))
            .arg("run")
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
            1,
            "{backend}: {}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let waveform = std::fs::read_to_string(segments[0].path()).unwrap();
        assert!(waveform.contains("$enddefinitions $end"), "{waveform}");
        assert!(
            waveform.contains(&format!("#{}\n", time + 3)),
            "{backend}: {waveform}"
        );
        let output = String::from_utf8(resumed.stdout).unwrap();
        let counts: Vec<u64> = output.lines().map(|line| line.parse().unwrap()).collect();
        // A checkpoint before the initial time-zero slot needs one step to
        // start the always process; otherwise all three steps advance time.
        assert!((2..=3).contains(&counts.len()), "{output}");
        assert_eq!(
            counts,
            (time + 1..=time + counts.len() as u64).collect::<Vec<_>>()
        );
        assert!(String::from_utf8_lossy(&resumed.stderr).contains("exceeded --max-steps 3"));
    }
}
