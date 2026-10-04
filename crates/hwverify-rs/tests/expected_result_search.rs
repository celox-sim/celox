use serde_json::Value;
use std::{fs, path::PathBuf, process::Command};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn run(
    model: &str,
    name: &str,
    flags: &[&str],
    environment: Option<&str>,
) -> (i32, Value, PathBuf) {
    let out = root()
        .join("target/expected-result-search-regression")
        .join(name);
    fs::create_dir_all(&out).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_hwverify-rs"));
    command
        .arg(root().join(format!("examples/{model}.json")))
        .arg("--out")
        .arg(&out)
        .args(["--z3", "/definitely/absent/z3"])
        .args(flags)
        .env("HWVERIFY_SOLVER", "finite")
        .env("HWVERIFY_KERNEL", "off")
        .env_remove("HWVERIFY_FINITE_SEARCH_HINT");
    if let Some(value) = environment {
        command.env("HWVERIFY_FINITE_SEARCH_HINT", value);
    }
    let result = command.output().unwrap();
    (
        result.status.code().unwrap(),
        serde_json::from_slice(&result.stdout).unwrap(),
        out,
    )
}
fn assertion(path: PathBuf) -> String {
    fs::read_to_string(path)
        .unwrap()
        .split("(check-sat)")
        .next()
        .unwrap()
        .into()
}
#[test]
fn hints_do_not_change_proof_status_or_original_queries() {
    for (model, actual, code) in [
        ("branch_pipeline_w4", "stuttering_refinement_verified", 0),
        ("branch_pipeline_w4_bad_branch_forward", "counterexample", 1),
    ] {
        let mut expected_formulas = Vec::new();
        for hint in ["query", "sat", "unsat"] {
            let (exit, report, out) = run(
                model,
                &format!("{model}-{hint}"),
                &["--finite-search-hint", hint],
                None,
            );
            assert_eq!(exit, code, "{report}");
            assert_eq!(report["status"], actual);
            let mut formulas = Vec::new();
            for q in report["obligations"].as_array().unwrap() {
                assert_eq!(q["backend"], "finite_bv");
                let logical = if q["name"] == "binding_nonempty"
                    || q["name"] == "progress_nonvacuity"
                    || q["name"] == "commit_reachable_in_relation"
                {
                    "sat"
                } else {
                    "unsat"
                };
                assert_eq!(q["logical_expectation"], logical);
                assert_eq!(
                    q["finite"]["search_hint"],
                    if hint == "query" { logical } else { hint }
                );
                if q["finite"]["search_hint"] == "unsat" {
                    assert_eq!(q["finite"]["probe_work"], 0);
                }
                if q["solver_result"] == "sat" {
                    assert_eq!(q["finite"]["original_formula_validated"], true);
                }
                formulas.push(assertion(out.join(q["evidence"].as_str().unwrap())));
            }
            if expected_formulas.is_empty() {
                expected_formulas = formulas;
            } else {
                assert_eq!(formulas, expected_formulas);
            }
        }
    }
}
#[test]
fn positive_negative_and_quantified_examples_keep_their_logical_meaning() {
    for hint in ["query", "sat", "unsat"] {
        let (_, report, _) = run(
            "budgeted_counter",
            &format!("examples-{hint}"),
            &["--finite-search-hint", hint],
            None,
        );
        for case in report["examples"].as_array().unwrap() {
            let logical = if case["expect"] == "positive" {
                "sat"
            } else {
                "unsat"
            };
            let q = &case["evidence"];
            assert_eq!(q["logical_expectation"], logical);
            assert_eq!(
                q["finite"]["search_hint"],
                if hint == "query" { logical } else { hint }
            );
            assert_eq!(case["status"], "passed");
        }
        let (_, report, _) = run(
            "quantified_counter",
            &format!("quantified-{hint}"),
            &["--finite-search-hint", hint],
            None,
        );
        for case in report["examples"].as_array().unwrap() {
            let q = &case["evidence"];
            assert_eq!(q["solver_result"], "unknown");
            assert_eq!(q["logical_expectation"], "sat");
            assert_eq!(q["finite"]["search_strategy"], "unsupported_quantified");
            assert_eq!(q["concrete_model"], false);
        }
    }
}
#[test]
fn explicit_cli_hint_overrides_environment_and_bad_options_fail() {
    let (_, report, _) = run(
        "branch_pipeline_w4",
        "override",
        &["--finite-search-hint", "query"],
        Some("sat"),
    );
    let q = &report["obligations"][2];
    assert_eq!(q["logical_expectation"], "unsat");
    assert_eq!(q["finite"]["search_hint"], "unsat");
    for (i, flags, environment) in [
        (0, vec!["--finite-search-hint", "unknown"], None),
        (
            1,
            vec![
                "--finite-search-hint",
                "sat",
                "--finite-search-hint",
                "unsat",
            ],
            None,
        ),
        (2, vec!["--finite-search-hint"], None),
        (3, vec![], Some("unknown")),
    ] {
        let (code, report, _) = run(
            "branch_pipeline_w4",
            &format!("invalid-{i}"),
            &flags,
            environment,
        );
        assert_eq!(code, 2);
        assert_eq!(report["status"], "invalid_or_tool_error");
    }
}
