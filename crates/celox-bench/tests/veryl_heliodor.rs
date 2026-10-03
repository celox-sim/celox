use std::{
    fs,
    process::{Command, Output},
};

fn run_fixture(source: &str, asynchronous: bool) -> Output {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("Veryl.toml"),
        "[project]\nname = \"runner_test\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(project.path().join("test.veryl"), source).unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_veryl-heliodor"));
    command
        .arg("--project")
        .arg(project.path())
        .args(["--test", "t"]);
    if asynchronous {
        command.arg("--aot-c-async");
    }
    command.output().unwrap()
}

#[test]
fn runs_all_initial_blocks_concurrently() {
    let source = r#"
        module Helper {
            initial {
                $display("HELPER");
            }
        }

        #[test(t)]
        module t {
            inst helper: Helper;
            inst clk: $tb::clock_gen;
            initial {
                clk.next(3);
                $display("LATE");
                $finish();
            }
            initial {
                clk.next(1);
                $display("EARLY");
                clk.next(3);
                $display("UNREACHABLE");
            }
        }
    "#;

    for asynchronous in [false, true] {
        let output = run_fixture(source, asynchronous);
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stdout}\n{stderr}");
        let helper = stdout.find("HELPER").expect(&stdout);
        let early = stdout.find("EARLY").expect(&stdout);
        let late = stdout.find("LATE").expect(&stdout);
        assert!(helper < early && early < late, "{stdout}");
        assert!(!stdout.contains("UNREACHABLE"), "{stdout}");
        assert!(
            stdout.contains("VERYL_TEST_RESULT test=t status=pass"),
            "{stdout}"
        );
    }
}

#[test]
fn reports_assertion_failure_in_a_later_initial_block() {
    let source = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            initial {
                clk.next(3);
                $finish();
            }
            initial {
                clk.next(1);
                $assert(1 == 0);
            }
        }
    "#;

    for asynchronous in [false, true] {
        let output = run_fixture(source, asynchronous);
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!output.status.success(), "{stdout}\n{stderr}");
        assert!(
            stdout.contains("VERYL_TEST_RESULT test=t status=fail"),
            "{stdout}\n{stderr}"
        );
    }
}

#[test]
fn shared_concurrent_initial_fixtures_match_veryl() {
    // mixed_edges is checked against the IEEE NBA rule and an independent SV
    // reproducer: Veryl 0.22.0 disagrees there. See the shared suite's LIMITATIONS.md.
    for source in [
        include_str!(
            "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_shared_clock.veryl"
        ),
        include_str!(
            "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_control_flow.veryl"
        ),
        include_str!(
            "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_clock_periods.veryl"
        ),
        include_str!(
            "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_simultaneous_edges.veryl"
        ),
        include_str!(
            "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_reset.veryl"
        ),
        include_str!(
            "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_hierarchy.veryl"
        ),
    ] {
        for asynchronous in [false, true] {
            let output = run_fixture(&source.replace("Top", "t"), asynchronous);
            let stdout = String::from_utf8(output.stdout).unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(
                output.status.success(),
                "async={asynchronous}\n{source}\n{stdout}\n{stderr}"
            );
            assert!(
                stdout.contains("VERYL_TEST_RESULT test=t status=pass"),
                "{stdout}\n{stderr}"
            );
        }
    }
}
