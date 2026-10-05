//! Machine-readable catalogue selection must work without external HDL tools.
#![cfg(any(feature = "icarus", feature = "verilator"))]
use celox_test_suite::veryl::cases;
use serde_json::Value;
use std::process::Command;

fn runner() -> Command {
    #[cfg(feature = "icarus")]
    let binary = env!("CARGO_BIN_EXE_verify-icarus");
    #[cfg(all(feature = "verilator", not(feature = "icarus")))]
    let binary = env!("CARGO_BIN_EXE_verify-verilator");
    let mut command = Command::new(binary);
    command.env("PATH", "");
    command
}

#[test]
fn json_catalogue_and_selection_need_no_simulator() {
    let output = runner().arg("--list").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let catalogue: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(catalogue["schema_version"], 3);
    let rows = catalogue["cases"].as_array().unwrap();
    assert_eq!(rows.len(), cases().count());
    for (row, case) in rows.iter().zip(cases()) {
        assert_eq!(row["name"], case.name);
        assert_eq!(
            row["stronger_than_sv"],
            case.has_stronger_than_sv_expectations()
        );
        assert_eq!(
            row["tags"],
            serde_json::json!(case.tags.iter().map(|tag| tag.as_str()).collect::<Vec<_>>())
        );
        for tag in case.tags {
            assert_eq!(row["tag_reasons"][tag.as_str()], tag.reason());
        }
    }

    let output = runner()
        .args([
            "--list",
            "--filter",
            "function_arguments::",
            "--exclude-stronger-than-sv",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let selected: Value = serde_json::from_slice(&output.stdout).unwrap();
    let expected: Vec<_> = rows
        .iter()
        .filter(|row| {
            row["name"]
                .as_str()
                .unwrap()
                .starts_with("function_arguments::")
                && row["stronger_than_sv"] == false
        })
        .cloned()
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(selected["cases"], serde_json::json!(expected));
    assert_eq!(selected["exclude_stronger_than_sv"], true);

    let output = runner()
        .args([
            "--list",
            "--filter",
            "test_named_function_inputs_evaluate_in_source_order",
            "--exclude-stronger-than-sv",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no cases matched the selection"));
}
