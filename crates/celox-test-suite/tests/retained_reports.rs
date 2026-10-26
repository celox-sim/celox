#![cfg(feature = "external")]
use std::collections::{BTreeMap, BTreeSet};

const STATUSES: [&str; 9] = [
    "passed",
    "rejected",
    "unexpected_accept",
    "mismatch",
    "emission_error",
    "compile_error",
    "runtime_error",
    "unsupported",
    "ignored",
];

/// Metadata that changes on every run stays out of retained reports, so that
/// branches adding cases only conflict where they change the same results.
fn assert_no_run_metadata(report: &serde_json::Value) {
    for key in ["context_fingerprint", "incremental", "run_counts", "counts"] {
        assert!(report.get(key).is_none(), "retained report has {key}");
    }
    for row in report["cases"].as_array().unwrap() {
        for key in ["case_fingerprint", "reused", "verified_at_unix"] {
            assert!(row.get(key).is_none(), "{} has {key}", row["name"]);
        }
    }
}

#[test]
fn retained_reports_are_valid_historical_evidence_for_the_live_catalogue() {
    let catalogue: BTreeSet<_> = celox_test_suite::veryl::cases()
        .map(|case| case.name)
        .collect();
    for file in [
        "verification/verilator.json",
        "verification/icarus.json",
        "verification/xezim.json",
    ] {
        let report = celox_test_suite::verification::read_report(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file),
        )
        .unwrap();
        assert_eq!(report["schema_version"], 3);
        assert_no_run_metadata(&report);
        assert!(report["include_ignored"].is_boolean());
        let rows = report["cases"].as_array().unwrap();
        let names: BTreeSet<_> = rows
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), rows.len(), "duplicate results");
        // Passing new cases need no historical baseline row. The daily gate
        // separately requires a fresh report of the complete live catalogue
        // and rejects every new or changed failure without matching evidence.
        assert!(
            names.is_subset(&catalogue),
            "retained evidence names an unknown case"
        );
        for name in &catalogue {
            if celox_test_suite::veryl::verification::known_issue(
                report["tool"].as_str().unwrap(),
                name,
            )
            .is_some()
            {
                assert!(
                    names.contains(name),
                    "missing reviewed exclusion for {name}"
                );
            }
        }
        for row in rows {
            let case = celox_test_suite::veryl::case(row["name"].as_str().unwrap()).unwrap();
            assert_eq!(row["expectation"], format!("{:?}", case.expectation));
            if row["status"] == "rejected" || row["status"] == "unexpected_accept" {
                assert_eq!(
                    case.expectation,
                    celox_test_suite::Expectation::CompilationError
                );
            }
            if row["status"] == "passed" {
                assert_eq!(case.expectation, celox_test_suite::Expectation::Simulation);
            }
            let status = row["status"].as_str().unwrap();
            assert!(STATUSES.contains(&status), "unknown result status");
            if status != "passed" {
                assert!(!row["detail"].as_str().unwrap().is_empty());
            }
            if status == "mismatch" {
                assert_eq!(row["phase"], "execute");
            }
            if status == "ignored" {
                assert_eq!(report["include_ignored"], false);
                assert_eq!(row["phase"], "skip");
                let issue = &row["known_issue"];
                assert_eq!(row["detail"], issue["reason"]);
                assert!(!issue["observed_version"].as_str().unwrap().is_empty());
                if issue.get("category").is_some() {
                    assert!(!issue["category"].as_str().unwrap().is_empty());
                    assert!(matches!(
                        issue["phase"].as_str(),
                        Some("emission" | "compile" | "execute")
                    ));
                } else {
                    assert!(
                        issue["standard"]
                            .as_str()
                            .unwrap()
                            .contains("IEEE 1800-2023")
                    );
                }
                let upstream = issue["upstream"].as_array().unwrap();
                let evidence = issue.get("evidence").and_then(|v| v.as_array());
                assert!(!upstream.is_empty() || evidence.is_some_and(|e| !e.is_empty()));
                for path in evidence.into_iter().flatten() {
                    let file = path.as_str().unwrap().split('#').next().unwrap();
                    assert!(
                        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join(file)
                            .is_file()
                    );
                }
            }
        }
    }
}

#[test]
fn systemverilog_retained_reports_explain_exclusions_and_executed_failures() {
    let catalogue: BTreeSet<_> = celox_test_suite::sv::cases()
        .map(|case| case.name)
        .collect();
    for tool in ["verilator", "icarus", "xezim"] {
        let report = celox_test_suite::verification::read_report(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("verification/sv/{tool}.json")),
        )
        .unwrap();
        assert_eq!(report["schema_version"], 3);
        assert_no_run_metadata(&report);
        let rows = report["cases"].as_array().unwrap();
        let names: BTreeSet<_> = rows
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), rows.len(), "duplicate results");
        // Passing new cases need no historical baseline row. The daily gate
        // separately requires a fresh report of the complete live catalogue
        // and rejects every new or changed failure without matching evidence.
        assert!(
            names.is_subset(&catalogue),
            "retained evidence names an unknown case"
        );
        for name in &catalogue {
            if celox_test_suite::sv::verification::known_issue(tool, name).is_some() {
                assert!(
                    names.contains(name),
                    "missing reviewed exclusion for {name}"
                );
            }
        }
        for row in rows {
            let name = row["name"].as_str().unwrap();
            let case = celox_test_suite::sv::case(name).unwrap();
            let status = row["status"].as_str().unwrap();
            // Reviewed failures stay executable and retain their diagnostics;
            // they are distinct from exclusions and passing cases.
            match status {
                "passed" => {
                    assert_eq!(case.expectation, celox_test_suite::Expectation::Simulation)
                }
                "rejected" => assert_eq!(
                    case.expectation,
                    celox_test_suite::Expectation::CompilationError
                ),
                "unsupported" => assert!(case.script().four_state, "{name}"),
                "ignored" => {
                    let issue = &row["known_issue"];
                    assert_eq!(row["detail"], issue["reason"]);
                    assert_eq!(
                        celox_test_suite::sv::verification::known_issue(tool, name).as_ref(),
                        Some(issue)
                    );
                    for path in issue["evidence"].as_array().unwrap() {
                        let file = path.as_str().unwrap().split('#').next().unwrap();
                        assert!(
                            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                                .join(file)
                                .is_file()
                        );
                    }
                }
                // xezim_retained_failures_match_their_review accounts for
                // each one.
                "mismatch" | "compile_error" | "runtime_error" | "unexpected_accept"
                    if tool == "xezim" =>
                {
                    assert!(!row["detail"].as_str().unwrap().is_empty(), "{name}");
                    assert!(row.get("known_issue").is_none(), "{name}");
                    let (phase, expectation) = match status {
                        "compile_error" => ("compile", celox_test_suite::Expectation::Simulation),
                        "unexpected_accept" => {
                            ("execute", celox_test_suite::Expectation::CompilationError)
                        }
                        _ => ("execute", celox_test_suite::Expectation::Simulation),
                    };
                    assert_eq!(row["phase"], phase, "{name}");
                    assert_eq!(case.expectation, expectation, "{name}");
                }
                "compile_error" | "mismatch" | "runtime_error" => {
                    let (expected_status, phase, diagnostic) = match (tool, name) {
                        ("verilator", "fork::compound_event_activations_outlive_parent") => (
                            "runtime_error",
                            "execute",
                            "native testbench failed: exit status: 1",
                        ),
                        (
                            "icarus",
                            "fork::compound_event_activations_outlive_parent"
                            | "fork::compound_event_function_scratch_isolated",
                        ) => (
                            "compile_error",
                            "compile",
                            "complex event expressions are not yet supported in automatic tasks",
                        ),
                        (
                            "icarus",
                            "fork::compound_event_tracks_result_not_operands"
                            | "fork::compound_event_four_state_lsb_edges",
                        ) => (
                            "compile_error",
                            "compile",
                            "Overriding the default variable lifetime is not yet supported",
                        ),
                        (
                            "verilator",
                            "tagged_unions::conditional_struct_payload"
                            | "tagged_unions::constructors_and_checked_members"
                            | "tagged_unions::writable_system_arguments"
                            | "tagged_unions::writable_task_arguments"
                            | "tagged_unions::default_arguments"
                            | "tagged_unions::factory_and_union_arguments"
                            | "tagged_unions::nested_and_void_constructors",
                        ) => ("compile_error", "compile", "Unsupported: tagged union"),
                        (
                            "icarus",
                            "tagged_unions::casts_ports_and_registers"
                            | "tagged_unions::conditional_struct_payload"
                            | "tagged_unions::constructors_and_checked_members"
                            | "tagged_unions::writable_system_arguments"
                            | "tagged_unions::writable_task_arguments"
                            | "tagged_unions::default_arguments"
                            | "tagged_unions::factory_and_union_arguments"
                            | "tagged_unions::nested_and_void_constructors"
                            | "tagged_unions::wide_four_state_payload",
                        ) => ("compile_error", "compile", "syntax error"),
                        (
                            "icarus",
                            "fork_control::function_join_none_automatic_values_outlive_returns"
                            | "fork_control::function_join_none_calls_in_defaults_need_process_frames"
                            | "fork_control::function_join_none_children_can_call_timed_tasks"
                            | "fork_control::function_join_none_event_driven_caller"
                            | "fork_control::function_join_none_preserves_expression_temporaries"
                            | "fork_control::function_join_none_retains_child_only_class_handle",
                        ) => (
                            "compile_error",
                            "compile",
                            "functions cannot have delay statements.",
                        ),
                        ("icarus", "unions::packed_mixed_state") => (
                            "mismatch",
                            "execute",
                            "assert_eq two: expected 0xa0, got 0xaX",
                        ),
                        (
                            "icarus",
                            "unions::soft_overlay"
                            | "unions::soft_signed_ports"
                            | "unions::soft_wide_four_state",
                        ) => ("compile_error", "compile", "syntax error"),
                        (
                            "icarus",
                            "hierarchy::packed_pattern_parameter_arguments_keep_types_and_unknown_bits"
                            | "hierarchy::packed_pattern_parameter_arguments_preserve_wide_values"
                            | "hierarchy::packed_pattern_parameters_forward_whole_values",
                        ) => (
                            "compile_error",
                            "compile",
                            "packed array parameters are not supported yet",
                        ),
                        _ => panic!("unreviewed failure: {tool} {name}: {status}"),
                    };
                    assert_eq!(status, expected_status, "{name}");
                    assert_eq!(row["phase"], phase, "{name}");
                    assert!(
                        row["detail"].as_str().unwrap().contains(diagnostic),
                        "{name}"
                    );
                    assert_eq!(case.expectation, celox_test_suite::Expectation::Simulation);
                    assert!(row.get("known_issue").is_none(), "{name}");
                    assert!(celox_test_suite::sv::verification::known_issue(tool, name).is_none());
                }
                _ => panic!("{tool} {name}: {status}"),
            }
        }
    }
}

#[test]
fn partial_word_sar_discrepancy_remains_an_executed_failure() {
    let report: serde_json::Value = celox_test_suite::verification::read_report(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("verification/verilator.json"),
    )
    .unwrap();
    let name = "partial_word_shift::two_state_matrix";
    let row = report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .expect("retain the reviewed SAR failure");
    assert_eq!(row["status"], "mismatch");
    assert_eq!(row["phase"], "execute");
    assert!(row["detail"].as_str().unwrap().contains("assert_eq c63_64"));
    assert!(celox_test_suite::veryl::verification::known_issue("verilator", name).is_none());
}

/// xezim retains its failures as executed evidence instead of exclusions.
/// Each one is reviewed against IEEE 1800-2023, and a xezim defect names the
/// reported bug it reproduces.
#[test]
fn xezim_retained_failures_match_their_review() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let review: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("verification/xezim-review.json")).unwrap(),
    )
    .unwrap();
    let bugs = review["bugs"].as_object().unwrap();
    for bug in bugs.values() {
        assert!(!bug["title"].as_str().unwrap().is_empty());
        assert!(
            bug["standard"]
                .as_str()
                .unwrap()
                .starts_with("IEEE 1800-2023 ")
        );
    }
    let mut cited = BTreeSet::new();
    for (suite, file) in [
        ("veryl", "verification/xezim.json"),
        ("sv", "verification/sv/xezim.json"),
    ] {
        let report = celox_test_suite::verification::read_report(&root.join(file)).unwrap();
        let failures: BTreeMap<_, _> = report["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| {
                !matches!(
                    row["status"].as_str(),
                    // The statuses check-external-suite-report.mjs counts as
                    // successes.
                    Some("passed" | "rejected" | "unsupported" | "ignored")
                )
            })
            .map(|row| {
                (
                    row["name"].as_str().unwrap(),
                    row["status"].as_str().unwrap(),
                )
            })
            .collect();
        let mut reviewed = BTreeMap::new();
        for case in review["cases"].as_array().unwrap() {
            if case["suite"] != suite {
                continue;
            }
            let name = case["name"].as_str().unwrap();
            assert!(!case["note"].as_str().unwrap().is_empty(), "{name}");
            let classification = case["classification"].as_str().unwrap();
            assert!(
                matches!(
                    classification,
                    "xezim_bug"
                        | "suite_issue"
                        | "lrm_ambiguous"
                        | "lrm_permitted_race"
                        | "adapter_two_state_init"
                        | "adapter_two_state_semantics"
                ),
                "{name}: {classification}"
            );
            let bug = case.get("bug").and_then(|bug| bug.as_str());
            assert_eq!(classification == "xezim_bug", bug.is_some(), "{name}");
            if let Some(bug) = bug {
                assert!(bugs.contains_key(bug), "{name}: unknown bug {bug}");
                cited.insert(bug);
            }
            let status = case["status"].as_str().unwrap();
            assert!(reviewed.insert(name, status).is_none(), "duplicate {name}");
        }
        assert_eq!(
            failures, reviewed,
            "{suite}: unreviewed or stale xezim results"
        );
    }
    assert_eq!(cited.len(), bugs.len(), "every listed bug has a case");
}
