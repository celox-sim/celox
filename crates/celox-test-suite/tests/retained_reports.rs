#![cfg(any(feature = "verilator", feature = "icarus"))]
use std::collections::BTreeSet;

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
    for contents in [
        include_str!("../verification/verilator.json"),
        include_str!("../verification/icarus.json"),
    ] {
        let report: serde_json::Value = serde_json::from_str(contents).unwrap();
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
fn systemverilog_retained_reports_explain_every_recorded_exclusion() {
    let catalogue: BTreeSet<_> = celox_test_suite::sv::cases()
        .map(|case| case.name)
        .collect();
    for (tool, contents) in [
        (
            "verilator",
            include_str!("../verification/sv/verilator.json"),
        ),
        ("icarus", include_str!("../verification/sv/icarus.json")),
    ] {
        let report: serde_json::Value = serde_json::from_str(contents).unwrap();
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
            // Every case passes, is rejected as expected, needs four-state
            // simulation the tool lacks, or has a reviewed exclusion.
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
                _ => panic!("{tool} {name}: {status}"),
            }
        }
    }
}

#[test]
fn partial_word_sar_discrepancy_remains_an_executed_failure() {
    let report: serde_json::Value =
        serde_json::from_str(include_str!("../verification/verilator.json")).unwrap();
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
