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
    fs::write(project.path().join("top.sv"), "module Top(output logic y); timeunit 1ns / 1ps; Plain child(y); initial #1 $finish; endmodule").unwrap();
    fs::write(&config, "sources = ['plain.sv', 'top.sv']\ntop = 'Top'\n[diagnostics]\nmixed_timescale = 'error'\n[targets.compat.diagnostics]\nmixed_timescale = 'warn'\n").unwrap();
    for build in [false, true] {
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
