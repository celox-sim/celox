use std::{
    fs,
    process::{Command, Output},
};

fn run_fixture(source: &str, args: &[&str]) -> Output {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("Veryl.toml"),
        "[project]\nname = \"runner_test\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(project.path().join("test.veryl"), source).unwrap();

    Command::new(env!("CARGO_BIN_EXE_veryl-heliodor"))
        .arg("--project")
        .arg(project.path())
        .args(["--test", "t"])
        .args(args)
        .env("VERYL_AOT_CACHE_DIR", project.path().join("aot-cache"))
        .env("VERYL_SETTLE_FILTER_DIAG", "1")
        .output()
        .unwrap()
}

fn metric(output: &str, key: &str) -> u128 {
    output
        .split_whitespace()
        .find_map(|field| field.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("missing {key}: {output}"))
        .parse()
        .unwrap()
}

#[test]
fn private_testbench_writes_do_not_add_design_evaluations() {
    let source = r#"
        module Counter (
            clk: input clock,
            rst: input reset,
            q: output logic<32>,
        ) {
            var count: logic<32>;
            always_ff {
                if_reset { count = 0; }
                else { count += 1; }
            }
            assign q = count;
        }
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen (clk);
            var q: logic<32>;
            var sample: logic<32>;
            inst dut: Counter (clk, rst, q);
            initial {
                rst.assert(2);
                for _i in 0..8 {
                    // SAMPLE
                    clk.next(1);
                }
                $assert(q == 8);
                $finish();
            }
        }
    "#;

    for args in [&[][..], &["--aot-c-async"][..]] {
        let mut evaluations = Vec::new();
        for sampling in ["", "sample = q;"] {
            let output = run_fixture(&source.replace("// SAMPLE", sampling), args);
            let stdout = String::from_utf8(output.stdout).unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(output.status.success(), "{stdout}\n{stderr}");
            assert!(stdout.contains("VERYL_TEST_RESULT test=t status=pass"));
            let compile = metric(&stdout, "compile_ns");
            let execute = metric(&stdout, "execute_ns");
            assert!(compile > 0 && execute > 0, "{stdout}");
            assert!(
                compile + execute <= metric(&stdout, "elapsed_ns"),
                "{stdout}"
            );
            evaluations.push(metric(&stderr, "settles_run"));
        }
        assert_eq!(
            evaluations[0], evaluations[1],
            "sampling into a testbench-private variable must not re-evaluate the design ({args:?})"
        );
    }
}

#[test]
fn compile_only_does_not_run_and_execution_reports_failure() {
    let source = r#"
        #[test(t)]
        module t {
            initial {
                $display("EXECUTED");
                $assert(1 == 0);
            }
        }
    "#;
    let compiled = run_fixture(source, &["--compile-only"]);
    let stdout = String::from_utf8(compiled.stdout).unwrap();
    assert!(compiled.status.success(), "{stdout}\n{:?}", compiled.stderr);
    assert!(!stdout.contains("EXECUTED"), "{stdout}");
    assert!(stdout.contains("status=compile-only"), "{stdout}");
    assert_eq!(metric(&stdout, "execute_ns"), 0);

    for args in [&[][..], &["--aot-c-async"][..]] {
        let output = run_fixture(source, args);
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!output.status.success(), "{stdout}");
        assert!(stdout.contains("EXECUTED"), "{stdout}");
        assert!(stdout.contains("status=fail"), "{stdout}");
    }
}
