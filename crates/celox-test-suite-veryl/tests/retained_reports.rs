#![cfg(any(feature = "verilator", feature = "icarus"))]
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn retained_reports_cover_the_catalogue_without_losing_failures() {
    let catalogue: BTreeSet<_> = celox_test_suite_veryl::cases()
        .map(|case| case.name)
        .collect();
    for contents in [
        include_str!("../verification/verilator.json"),
        include_str!("../verification/icarus.json"),
    ] {
        let report: serde_json::Value = serde_json::from_str(contents).unwrap();
        assert_eq!(report["schema_version"], 2);
        assert!(report["include_ignored"].is_boolean());
        let rows = report["cases"].as_array().unwrap();
        let names: BTreeSet<_> = rows
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), rows.len(), "duplicate results");
        assert_eq!(
            names, catalogue,
            "refresh both reports after catalogue changes"
        );
        let mut counts = BTreeMap::new();
        for row in rows {
            let case = celox_test_suite_veryl::case(row["name"].as_str().unwrap()).unwrap();
            assert_eq!(row["expectation"], format!("{:?}", case.expectation));
            if row["status"] == "rejected" || row["status"] == "unexpected_accept" {
                assert_eq!(
                    case.expectation,
                    celox_test_suite_veryl::Expectation::CompilationError
                );
            }
            if row["status"] == "passed" {
                assert_eq!(
                    case.expectation,
                    celox_test_suite_veryl::Expectation::Simulation
                );
            }
            let status = row["status"].as_str().unwrap();
            assert!(
                report["counts"].get(status).is_some(),
                "unknown result status"
            );
            *counts.entry(status).or_insert(0u64) += 1;
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
        for (status, count) in report["counts"].as_object().unwrap() {
            assert_eq!(
                count.as_u64().unwrap(),
                counts.get(status.as_str()).copied().unwrap_or(0)
            );
        }
    }
}
