#[cfg(feature = "systemverilog")]
use std::fs;
use std::process::Command;

#[test]
fn cli_rejects_invalid_diagnostic_levels() {
    let output = Command::new(env!("CARGO_BIN_EXE_celox"))
        .args([
            "vpi",
            "build",
            "top.veryl",
            "--top",
            "Top",
            "--diagnostic",
            "unused_variable=quiet",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid diagnostic level"));
}

#[cfg(feature = "systemverilog")]
#[test]
fn run_and_build_share_project_target_and_cli_diagnostic_precedence() {
    let project = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let nested = project.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let config = project.path().join("celox.toml");
    let output = project.path().join("top-sim");
    fs::write(
        project.path().join("plain.sv"),
        "module Plain(output logic y); assign y = 1; endmodule",
    )
    .unwrap();
    fs::write(&config, "sources = ['plain.sv', 'top.sv']\ntop = 'Top'\n[diagnostics]\nmixed_timescale = 'error'\n[targets.compat.diagnostics]\nmixed_timescale = 'warn'\n").unwrap();
    for build in [false, true] {
        // A VPI image runs no process.
        let finish = if build { "" } else { "initial #1 $finish;" };
        fs::write(
            project.path().join("top.sv"),
            format!(
                "module Top(output logic y); timeunit 1ns / 1ps; Plain child(y); {finish} endmodule"
            ),
        )
        .unwrap();
        let compile = |target: bool, overrides: &[&str], discover: bool| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_celox"));
            if build {
                command.args(["vpi", "build", "--output"]).arg(&output);
            } else {
                command.args(["run", "-O", "O2"]);
            }
            if discover {
                command.current_dir(&nested);
            } else {
                command
                    .current_dir(elsewhere.path())
                    .arg("--project")
                    .arg(&config);
            }
            if target {
                command.args(["--target", "compat"]);
            }
            for value in overrides {
                command.args(["--diagnostic", value]);
            }
            command.output().unwrap()
        };
        let strict = compile(false, &[], false);
        assert!(!strict.status.success());
        assert!(String::from_utf8_lossy(&strict.stderr).contains("mixed explicit and default"));
        for discover in [false, true] {
            let warned = compile(true, &[], discover);
            assert!(
                warned.status.success(),
                "{}",
                String::from_utf8_lossy(&warned.stderr)
            );
            assert!(String::from_utf8_lossy(&warned.stderr).contains("mixed_timescale"));
        }
        let allowed = compile(
            true,
            &["mixed_timescale=error", "mixed_timescale=allow"],
            false,
        );
        assert!(
            allowed.status.success(),
            "{}",
            String::from_utf8_lossy(&allowed.stderr)
        );
        assert!(!String::from_utf8_lossy(&allowed.stderr).contains("mixed_timescale"));
        let denied = compile(true, &["mixed_timescale=error"], false);
        assert!(!denied.status.success());
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn aggregate_input_defaults_can_be_enabled_with_diagnostic_policy() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("top.sv"),
        r#"
        module Child(input logic [7:0] a[2]='{8'ha5,8'h5a}, output logic [15:0] y);
            assign y={a[0],a[1]};
        endmodule
        module Top(output logic [15:0] omitted, connected);
            logic [7:0] a[2];
            assign a[0]=8'h12;
            assign a[1]=8'h34;
            Child c0(.y(omitted));
            Child c1(.a(a),.y(connected));
            initial begin #1;
                assert(omitted===16'ha55a && connected===16'h1234) else $fatal(1,"defaults");
                $display("ok"); $finish;
            end
        endmodule
    "#,
    )
    .unwrap();
    fs::write(
        project.path().join("celox.toml"),
        "sources=['top.sv']\ntop='Top'\n[diagnostics]\nunpacked_port_default='warn'\n",
    )
    .unwrap();
    let run = |overrides: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_celox"))
            .current_dir(project.path())
            .arg("run")
            .args(overrides)
            .output()
            .unwrap()
    };
    let warned = run(&[]);
    assert!(
        warned.status.success(),
        "{}",
        String::from_utf8_lossy(&warned.stderr)
    );
    assert_eq!(warned.stdout, b"ok\n");
    assert!(String::from_utf8_lossy(&warned.stderr).contains("unpacked_port_default"));
    let allowed = run(&["--diagnostic", "unpacked_port_default=allow"]);
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert_eq!(allowed.stdout, b"ok\n");
    assert!(!String::from_utf8_lossy(&allowed.stderr).contains("unpacked_port_default"));
    let rejected = run(&["--diagnostic", "unpacked_port_default=error"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("unpacked_port_default"));
}

#[cfg(feature = "systemverilog")]
#[test]
fn inferred_latch_uses_common_diagnostics_for_run_and_build() {
    let project = tempfile::tempdir().unwrap();
    let config = project.path().join("celox.toml");
    fs::write(&config, "sources = ['top.sv']\ntop = 'Top'\n[diagnostics]\ninferred_latch = 'error'\n[targets.compat.diagnostics]\ninferred_latch = 'warn'\n").unwrap();
    for build in [false, true] {
        // A VPI image runs no process.
        let source = if build {
            r#"module Top(input logic en, input logic [7:0] d, output logic [7:0] q);
        timeunit 1ns / 1ps;
        always_comb if (en) q = d;
    endmodule"#
        } else {
            r#"module Top;
        timeunit 1ns / 1ps;
        logic en; logic [7:0] d, q;
        always_comb if (en) q = d;
        initial begin
            en = 1; d = 7; #1;
            assert (q == 7) else $fatal(1, "open latch");
            en = 0; d = 99; #1;
            assert (q == 7) else $fatal(1, "closed latch");
            $finish;
        end
    endmodule"#
        };
        fs::write(project.path().join("top.sv"), source).unwrap();
        let compile = |target: bool, overrides: &[&str]| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_celox"));
            if build {
                command
                    .args(["vpi", "build", "--output"])
                    .arg(project.path().join("top-sim"));
            } else {
                command.args(["run", "-O", "O2"]);
            }
            command.arg("--project").arg(&config);
            if target {
                command.args(["--target", "compat"]);
            }
            for value in overrides {
                command.args(["--diagnostic", value]);
            }
            command.output().unwrap()
        };
        for (target, overrides, success, diagnostic) in [
            (false, vec![], false, true),
            (true, vec![], true, true),
            (false, vec!["inferred_latch=allow"], true, false),
            (
                true,
                vec!["inferred_latch=error", "inferred_latch=allow"],
                true,
                false,
            ),
            (true, vec!["inferred_latch=error"], false, true),
        ] {
            let result = compile(target, &overrides);
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert_eq!(result.status.success(), success, "{stderr}");
            assert_eq!(stderr.contains("inferred_latch"), diagnostic, "{stderr}");
        }
    }
}
