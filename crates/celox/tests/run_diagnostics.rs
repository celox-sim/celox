use std::{fs, process::Command};

#[test]
fn diagnostics_reach_stderr_only_when_logging_is_enabled() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("Veryl.toml"),
        "[project]\nname = \"runner_test\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        project.path().join("test.veryl"),
        r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            initial {
                clk.next(4);
                $finish();
            }
        }
        "#,
    )
    .unwrap();

    for logging in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_celox"));
        command
            .args(["run", "--report"])
            .arg("--project")
            .arg(project.path())
            .args(["--top", "t", "--backend", "interpreter"])
            .env("CELOX_PHASE_TIMING", "1")
            .env("CELOX_TESTBENCH_PROGRESS", "2")
            .env_remove("RUST_LOG");
        if logging {
            command.env("RUST_LOG", "celox::=debug");
        }
        let output = command.output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(stdout.contains("CELOX_TEST_RESULT test=t status=pass"));
        assert_eq!(stderr.contains("[phase-timing]"), logging, "{stderr}");
        assert_eq!(
            stderr.contains("[testbench-progress] tick=4"),
            logging,
            "{stderr}"
        );
        assert!(!stdout.contains("[testbench-progress]"));
    }
}

#[test]
fn compile_only_preserves_warnings_with_and_without_cache() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("Veryl.toml"),
        "[project]\nname = 'warnings'\nversion = '0.1.0'\n",
    )
    .unwrap();
    fs::write(
        project.path().join("top.veryl"),
        r#"
module Top (tmp: output logic<8>, out: output logic) {
    function write_outputs(first: output logic<8>, second: output logic<8>) -> logic {
        first = 8'd1;
        second = 8'd2;
        return 1'b1;
    }
    always_comb {
        tmp = 0;
        out = write_outputs(second: tmp, first: tmp) & 1'b1;
    }
}
"#,
    )
    .unwrap();
    for cached in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_celox"));
        command
            .args(["run", "--top", "Top", "--compile-only", "--project"])
            .arg(project.path());
        if cached {
            command
                .arg("--build-cache-dir")
                .arg(project.path().join("cache"));
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("unspecified_output_copy_order"),
            "{output:?}"
        );
    }
}
