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
        let mut command = Command::new(env!("CARGO_BIN_EXE_celox-heliodor"));
        command
            .arg("--project")
            .arg(project.path())
            .args(["--test", "t", "--backend", "interpreter"])
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
