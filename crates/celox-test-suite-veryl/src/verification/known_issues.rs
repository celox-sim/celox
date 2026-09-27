//! External-runner exclusions, never changes to the reusable case expectations.
use serde_json::{Value, json};

pub(super) struct KnownIssue {
    pub tool: &'static str,
    pub case: &'static str,
    pub observed_version: &'static str,
    pub reason: &'static str,
    pub standard: &'static str,
    pub upstream: &'static [&'static str],
    /// Retained evidence paths relative to the crate, separate from upstream statements.
    pub evidence: &'static [&'static str],
}

impl KnownIssue {
    pub fn as_json(&self) -> Value {
        json!({
            "observed_version": self.observed_version,
            "reason": self.reason,
            "standard": self.standard,
            "upstream": self.upstream,
            "evidence": self.evidence,
        })
    }

    const fn dynamic_output(case: &'static str) -> Self {
        Self {
            tool: "verilator",
            case,
            observed_version: "5.052",
            reason: "Verilator accepts this nonconstant output-port destination instead of rejecting it. The expected compilation error is retained. No direct upstream decision was found; this exclusion is based on the recorded acceptance and the checked SV rules.",
            standard: "IEEE 1800-2023 10.2 Table 10-1 and 23.3.3.2: an output variable connection implies a continuous assignment, whose packed selects must be constant; expect compilation rejection.",
            upstream: &[],
            evidence: &[
                "verification/repros/dynamic_output_acceptance.json",
                "MISMATCH_REVIEW.md#7-dynamic-instance-output-selection-compilation-error",
            ],
        }
    }
}

pub(super) const KNOWN_ISSUES: &[KnownIssue] = &[
    KnownIssue {
        tool: "verilator",
        case: "basic::test_always_comb_read_before_write_uses_previous_value",
        observed_version: "5.052",
        reason: "Verilator reads the new value. Upstream #4052 prioritizes optimized combinational execution over exact IEEE behavior; #7608 improves the warning and documentation without correcting execution.",
        standard: "IEEE 1800-2023 9.3.1 and 9.2.2.2.1: execute in source order and exclude written variables from implicit sensitivity; expect c=0,0,1.",
        upstream: &[
            "https://github.com/verilator/verilator/issues/4052#issuecomment-1476997115",
            "https://github.com/verilator/verilator/pull/7608",
        ],
        evidence: &[],
    },
    KnownIssue {
        tool: "verilator",
        case: "signed_divrem::signed_divrem_i64",
        observed_version: "5.052",
        reason: "Verilator intentionally returns zero for MIN / -1 to avoid a host division exception (#2460). That discussion gives no SV rule permitting zero.",
        standard: "IEEE 1800-2023 6.9.1, 11.4.3 and 11.6.1: fixed-width arithmetic modulo 2^64 preserves 0x8000000000000000 for MIN / -1.",
        upstream: &["https://github.com/verilator/verilator/issues/2460#issuecomment-656970666"],
        evidence: &[],
    },
    KnownIssue {
        tool: "icarus",
        case: "hierarchy::test_instance_input_port_assignment_width_context",
        observed_version: "13.0",
        reason: "Icarus evaluates the input expression at its self-determined width before padding, losing the carry. Its source says this permits port-width warnings. No direct upstream response or decision on this discrepancy was found; this exclusion records our observed limitation.",
        standard: "IEEE 1800-2023 10.8 and 11.8.2: the 9-bit input-port context widens operands before addition; expect 0x100.",
        upstream: &[
            "https://github.com/steveicarus/iverilog/blob/3a28970ee1d9dc9717b52de58cdc19a30a5c5510/elaborate.cc#L1938-L1953",
        ],
        evidence: &[],
    },
    KnownIssue::dynamic_output("hierarchy::test_dynamic_minus_colon_output_port_rmw"),
    KnownIssue::dynamic_output(
        "hierarchy::test_dynamic_output_port_converts_four_state_child_to_two_state_parent",
    ),
    KnownIssue::dynamic_output("hierarchy::test_dynamic_output_port_rmw_preserves_unselected_bits"),
    KnownIssue::dynamic_output("hierarchy::test_dynamic_step_output_port_rmw"),
    KnownIssue::dynamic_output("hierarchy::test_instance_output_concat_advances_each_destination"),
    KnownIssue::dynamic_output(
        "hierarchy::test_instance_output_dynamic_index_composes_aliasing_writeback",
    ),
    KnownIssue::dynamic_output(
        "hierarchy::test_instance_output_dynamic_index_function_output_writeback",
    ),
];

// Explicitly reviewed case IDs, never diagnostic patterns or the latest run's
// failures. A new case or an unlisted tool still executes and can fail CI.
fn limitations() -> &'static [Value] {
    static LIMITATIONS: std::sync::OnceLock<Vec<Value>> = std::sync::OnceLock::new();
    LIMITATIONS.get_or_init(|| {
        serde_json::from_str(include_str!("limitations.json"))
            .expect("checked-in verification limitations must be valid JSON")
    })
}

pub(super) fn find(tool: &str, case: &str) -> Option<Value> {
    if let Some(issue) = KNOWN_ISSUES
        .iter()
        .find(|issue| issue.tool == tool && issue.case == case)
    {
        return Some(issue.as_json());
    }
    for group in limitations() {
        let Some(cases) = group["cases"][tool].as_array() else {
            continue;
        };
        if cases.iter().any(|name| name.as_str() == Some(case)) {
            let mut metadata = group.clone();
            metadata.as_object_mut().unwrap().remove("cases");
            return Some(metadata);
        }
    }
    None
}

#[cfg(test)]
pub(super) fn entries() -> Vec<(&'static str, &'static str)> {
    let mut entries: Vec<_> = KNOWN_ISSUES
        .iter()
        .map(|issue| (issue.tool, issue.case))
        .collect();
    for group in limitations() {
        for (tool, cases) in group["cases"].as_object().unwrap() {
            for case in cases.as_array().unwrap() {
                entries.push((tool.as_str(), case.as_str().unwrap()));
            }
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limitations_only_exclude_cases_with_retained_failure_evidence() {
        for (tool, contents) in [
            (
                "verilator",
                include_str!("../../verification/limitations/verilator.json"),
            ),
            (
                "icarus",
                include_str!("../../verification/limitations/icarus.json"),
            ),
        ] {
            let report: Value = serde_json::from_str(contents).unwrap();
            for group in limitations() {
                let Some(cases) = group["cases"][tool].as_array() else {
                    continue;
                };
                assert!(!cases.is_empty());
                for case in cases {
                    let row = report["cases"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|row| row["name"] == *case)
                        .unwrap_or_else(|| panic!("missing retained {tool} failure for {case}"));
                    assert_eq!(row["phase"], group["phase"]);
                    assert_eq!(row["expectation"], "Simulation");
                    assert!(matches!(
                        row["status"].as_str(),
                        Some("emission_error" | "compile_error" | "runtime_error")
                    ));
                    assert!(!row["detail"].as_str().unwrap().is_empty());
                }
            }
        }
    }
}
