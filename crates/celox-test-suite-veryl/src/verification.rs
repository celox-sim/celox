//! Verification of this suite against the external simulators.
mod known_issues;

use celox_test_suite_core::verification::{Suite, Tool};

/// The reviewed exclusion of `case` for `tool`, if any.
pub fn known_issue(tool: &str, case: &str) -> Option<serde_json::Value> {
    known_issues::find(tool, case)
}

/// This suite, for the shared runner.
pub fn suite() -> Suite {
    Suite {
        name: "veryl",
        cases: crate::cases().collect(),
        known_issue,
    }
}

/// The command-line runner of `verify-verilator` and `verify-icarus`.
pub fn run(tool: Tool) -> crate::Result<()> {
    celox_test_suite_core::verification::run(tool, &suite(), &crate::emit::VerylFrontend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompilationRejected, Design, TestCase};
    use celox_test_suite_core::verification::{
        Builders, EmissionError, case_metadata, is_failure, run_case,
    };
    use serde_json::Value;
    use std::path::Path;

    /// Run `case` with builders that fail with `error` before compiling.
    fn run(
        case: &TestCase,
        directory: &Path,
        tool: &str,
        include_ignored: bool,
        error: fn() -> crate::Error,
    ) -> Value {
        let design =
            move |_: &Design, _: &Path| -> crate::Result<Box<dyn crate::Backend>> { Err(error()) };
        let script = move |_: &TestCase, _: &Path| -> crate::Result<Box<dyn crate::Backend>> {
            Err(error())
        };
        let issue = known_issues::find(tool, case.name);
        let skip = if include_ignored { None } else { issue.clone() };
        let builders = Builders {
            design: &design,
            script: &script,
        };
        run_case(case, directory, true, &builders, skip, issue)
    }

    #[test]
    fn known_issues_skip_only_the_affected_tool_and_can_be_rechecked() {
        let directory = std::env::temp_dir().join(format!(
            "veryl-known-issue-classification-{}",
            std::process::id()
        ));
        let mut entries = std::collections::BTreeSet::new();
        for (tool, name) in known_issues::entries() {
            assert!(
                entries.insert((tool, name)),
                "duplicate exclusion: {tool} {name}"
            );
            let issue = known_issues::find(tool, name).unwrap();
            let case = crate::case(name).expect("excluded case must exist");
            let ignored = run(case, &directory, tool, false, || -> crate::Error {
                panic!("ignored case must not reach the compiler")
            });
            assert_eq!(ignored["status"], "ignored");
            assert_eq!(ignored["tags"], case_metadata(case)["tags"]);
            assert_eq!(
                ignored["stronger_than_sv"],
                case.has_stronger_than_sv_expectations()
            );
            assert_eq!(ignored["phase"], "skip");
            assert!(!is_failure(ignored["status"].as_str().unwrap()));
            assert!(!issue["reason"].as_str().unwrap().is_empty());
            assert!(!issue["observed_version"].as_str().unwrap().is_empty());
            let upstream = issue["upstream"].as_array().unwrap();
            let evidence = issue["evidence"].as_array().unwrap();
            assert!(!upstream.is_empty() || !evidence.is_empty());
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
            for evidence in evidence {
                let path = evidence.as_str().unwrap().split('#').next().unwrap();
                assert!(Path::new(env!("CARGO_MANIFEST_DIR")).join(path).is_file());
            }

            // Forced runs and unaffected tools must execute normally, including
            // surfacing new failures instead of treating them as expected.
            for (tool, include_ignored) in [(tool, true), ("unaffected", false)] {
                let executed = run(case, &directory, tool, include_ignored, || {
                    "compiler executable unavailable".into()
                });
                assert_eq!(executed["status"], "compile_error");
                assert_eq!(executed["tag_reasons"], case_metadata(case)["tag_reasons"]);
                assert!(is_failure(executed["status"].as_str().unwrap()));
                assert_eq!(executed.get("known_issue").is_some(), include_ignored);
            }
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unlisted_cases_still_report_failures() {
        let case = crate::case("operators::test_bitwise_operations").unwrap();
        let directory =
            std::env::temp_dir().join(format!("veryl-unlisted-case-{}", std::process::id()));
        for tool in ["verilator", "icarus"] {
            assert!(known_issues::find(tool, case.name).is_none());
            let result = run(case, &directory, tool, false, || {
                "new compiler failure".into()
            });
            assert_eq!(result["status"], "compile_error");
            assert!(is_failure(result["status"].as_str().unwrap()));
            assert!(result.get("known_issue").is_none());
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn negative_cases_do_not_pass_on_infrastructure_or_emission_failure() {
        let case = crate::case("hierarchy::test_instance_output_concat_advances_each_destination")
            .unwrap();
        let directory = std::env::temp_dir().join(format!(
            "veryl-rejection-classification-{}",
            std::process::id()
        ));
        let rejected = run(case, &directory, "test", false, || {
            Box::new(CompilationRejected("nonconstant destination".into()))
        });
        assert_eq!(rejected["status"], "rejected");
        let unavailable = run(case, &directory, "test", false, || {
            "compiler executable unavailable".into()
        });
        assert_eq!(unavailable["status"], "compile_error");
        let emission = run(case, &directory, "test", false, || {
            Box::new(EmissionError("emitter panicked".into()))
        });
        assert_eq!(emission["status"], "emission_error");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
