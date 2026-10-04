//! The rejection classifiers against this suite's retained diagnostics.
#![cfg(any(feature = "verilator", feature = "icarus"))]
use std::path::PathBuf;

#[cfg(feature = "icarus")]
mod icarus {
    use super::*;
    use celox_test_suite_core::icarus::is_source_rejection;

    #[test]
    fn retained_negative_diagnostics_are_source_rejections() {
        let report: serde_json::Value =
            serde_json::from_str(include_str!("../verification/icarus.json")).unwrap();
        let mut checked = 0;
        for case in report["cases"].as_array().unwrap() {
            if case["status"] != "rejected" {
                continue;
            }
            let (status, log) = case["detail"].as_str().unwrap().split_once('\n').unwrap();
            let code = status
                .strip_prefix("Icarus build exit status: ")
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert!(
                is_source_rejection(Some(code), log, &["<case>/source_0.sv".into()]),
                "{}",
                case["name"]
            );
            checked += 1;
        }
        assert_eq!(checked, 8);
    }

    #[test]
    fn incomplete_or_unattributed_diagnostics_do_not_prove_rejection() {
        let sources = [PathBuf::from("design.sv")];
        for log in [
            "",
            "Elaboration failed",
            "1 error(s) during elaboration.",
            "design.sv:1: error: Function f port q is not an input port.\ndesign.sv:1:      : Function arguments must be input ports.\n1 error(s) during elaboration.",
            "unknown.sv:1: error: Bit select expressions must be a constant integral value.",
            "design.sv:1: error: internal compiler error",
            "design.sv:1: error: failed to read input",
        ] {
            assert!(!is_source_rejection(Some(1), log, &sources), "{log}");
        }
        let source_error =
            "design.sv:1: error: Bit select expressions must be a constant integral value.\n";
        for code in [
            None,
            Some(0),
            Some(124),
            Some(125),
            Some(126),
            Some(127),
            Some(137),
        ] {
            assert!(!is_source_rejection(code, source_error, &sources));
        }
        for failure in [
            "internal compiler error",
            "design.sv: No such file or directory",
            "ivl: Assertion failed",
        ] {
            assert!(!is_source_rejection(
                Some(1),
                &format!("{source_error}{failure}"),
                &sources
            ));
        }
    }
}

#[cfg(feature = "verilator")]
mod verilator {
    use super::*;
    use celox_test_suite_core::verilator::is_source_rejection;

    #[test]
    fn retained_negative_diagnostic_is_a_source_rejection() {
        let report: serde_json::Value =
            serde_json::from_str(include_str!("../verification/verilator.json")).unwrap();
        let mut checked = 0;
        for case in report["cases"].as_array().unwrap() {
            if case["status"] != "rejected" {
                continue;
            }
            let (_, log) = case["detail"].as_str().unwrap().split_once('\n').unwrap();
            let sources = [PathBuf::from("<case>/source_0.sv")];
            assert!(is_source_rejection(Some(1), log, &sources));
            assert!(!is_source_rejection(Some(1), log, &["other.sv".into()]));
            assert!(!is_source_rejection(None, log, &sources));
            checked += 1;
        }
        assert_eq!(checked, 1);
    }
}
