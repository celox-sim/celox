//! Verify actual CLI cache decisions with isolated compiler process fixtures.
#![cfg(all(unix, any(feature = "verilator", feature = "icarus")))]

use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn script(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn check_incremental(binary: &str, tool: &str, name: &str) {
    let directory = std::env::temp_dir().join(format!(
        "celox-incremental-cli-{tool}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    script(
        &directory.join("timeout"),
        "#!/bin/sh\nshift\nexec \"$@\"\n",
    );
    for name in ["verilator_bin", "make", "c++", "vvp", "iverilog-vpi"] {
        script(
            &directory.join(name),
            "#!/bin/sh\nprintf '%s\\n' 'fixture auxiliary tool'\n",
        );
    }
    let compiler = directory.join(tool);
    script(
        &compiler,
        r#"#!/bin/sh
case "$1" in --version|-V) printf '%s\n' 'fixture compiler v1'; exit 0;; esac
printf '%s\n' invocation >> "$CELOX_INCREMENTAL_INVOCATIONS"
IFS= read -r mode < "$CELOX_INCREMENTAL_MODE"
if [ "$mode" = failure ]; then exit 124; fi
for argument do
    case "$argument" in */source_0.sv) source_path="$argument";; esac
done
case "$0" in
    *verilator)
        printf '%%Error: %s:1:1: Illegal assignment: types are not assignment compatible (IEEE 1800-2023 7.6)\n' "$source_path"
        printf '%s\n' '%Error: Exiting due to 1 error(s)';;
    *)
        printf '%s:1: error: Bit select expressions must be a constant integral value.\n' "$source_path"
        printf '%s\n' '1 error(s) during elaboration.';;
esac
exit 1
"#,
    );
    let report_path = directory.join("report.json");
    let cache = directory.join("cache");
    let mode = directory.join("mode");
    let invocations = directory.join("invocations");
    let execute = |flags: &[&str], expected_success: bool| {
        let mut command = Command::new(binary);
        command
            .args([
                "--filter",
                name,
                "--jobs",
                "1",
                "--include-ignored",
                "--output",
            ])
            .arg(directory.join("output"))
            .arg("--cache")
            .arg(&cache)
            .arg("--report")
            .arg(&report_path)
            .args(flags)
            .env("PATH", &directory)
            .env("CELOX_INCREMENTAL_MODE", &mode)
            .env("CELOX_INCREMENTAL_INVOCATIONS", &invocations);
        let result = command.output().unwrap();
        assert_eq!(
            result.status.success(),
            expected_success,
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    };
    // The local baseline keeps run metadata; --report retains only results.
    let results_path = directory.join("output").join("results.json");
    let report = || -> Value { serde_json::from_slice(&fs::read(&results_path).unwrap()).unwrap() };
    let retained =
        || -> Value { serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap() };
    let calls = || fs::read_to_string(&invocations).unwrap().lines().count();

    fs::write(&mode, "failure\n").unwrap();
    for expected_calls in [1, 2] {
        execute(&[], false);
        assert_eq!(calls(), expected_calls);
        assert_eq!(report()["run_counts"], json!({"fresh": 1, "reused": 0}));
        assert_eq!(report()["cases"][0]["status"], "compile_error");
    }
    fs::write(&mode, "reject\n").unwrap();
    execute(&[], true);
    let first = report();
    assert!(first["context_fingerprint"].is_string());
    assert_eq!(calls(), 3);
    assert_eq!(first["cases"][0]["status"], "rejected");
    execute(&["--incremental"], true);
    assert_eq!(
        calls(),
        3,
        "unchanged successful rejection must not compile again"
    );
    assert_eq!(report()["run_counts"], json!({"fresh": 0, "reused": 1}));
    assert_eq!(report()["cases"][0]["reused"], true);
    assert_eq!(
        report()["cases"][0]["verified_at_unix"],
        first["cases"][0]["verified_at_unix"]
    );
    let mut expected = report();
    let fields = expected.as_object_mut().unwrap();
    for key in ["context_fingerprint", "incremental", "run_counts", "counts"] {
        fields.remove(key).unwrap();
    }
    let row = fields["cases"][0].as_object_mut().unwrap();
    for key in ["case_fingerprint", "reused", "verified_at_unix"] {
        row.remove(key).unwrap();
    }
    assert_eq!(retained(), expected, "retained reports omit run metadata");

    // A stale local fingerprint does not change the actual case. The shared
    // cache still contains matching evidence and must supply the fallback.
    let mut changed = report();
    changed["cases"][0]["case_fingerprint"] = json!("prior case contents");
    fs::write(&results_path, changed.to_string()).unwrap();
    execute(&[], true);
    assert_eq!(
        calls(),
        3,
        "stale local results must fall back to shared evidence"
    );
    assert_eq!(report()["run_counts"], json!({"fresh": 0, "reused": 1}));
    let mut reused_case = first["cases"][0].clone();
    reused_case["reused"] = json!(true);
    assert_eq!(report()["cases"][0], reused_case);

    // A new output directory can reuse the same isolated shared cache.
    fs::remove_dir_all(directory.join("output")).unwrap();
    execute(&[], true);
    assert_eq!(calls(), 3, "shared reuse must not require a local report");
    assert_eq!(report()["run_counts"], json!({"fresh": 0, "reused": 1}));

    // With no local evidence, corrupt shared evidence must trigger execution.
    let shared = cache
        .join(first["context_fingerprint"].as_str().unwrap())
        .join(format!(
            "{}.json",
            first["cases"][0]["case_fingerprint"].as_str().unwrap()
        ));
    fs::write(&shared, "corrupted shared report").unwrap();
    fs::remove_file(&results_path).unwrap();
    execute(&[], true);
    assert_eq!(calls(), 4, "corrupt shared evidence must be reverified");
    assert_eq!(report()["run_counts"], json!({"fresh": 1, "reused": 0}));

    let contents = fs::read_to_string(&compiler).unwrap();
    script(
        &compiler,
        &format!("{contents}\n# changed compiler build\n"),
    );
    execute(&[], true);
    assert_eq!(calls(), 5, "changed compiler must invalidate reuse");
    assert_ne!(
        report()["context_fingerprint"],
        first["context_fingerprint"]
    );
    assert_eq!(report()["run_counts"], json!({"fresh": 1, "reused": 0}));
    execute(&[], true);
    assert_eq!(calls(), 5, "default mode must reuse unchanged successes");
    assert_eq!(report()["run_counts"], json!({"fresh": 0, "reused": 1}));
    execute(&["--fresh"], true);
    assert_eq!(calls(), 6, "explicit fresh mode must always run afresh");
    assert_eq!(report()["run_counts"], json!({"fresh": 1, "reused": 0}));
    fs::write(&results_path, "corrupted report").unwrap();
    execute(&[], false);
    assert_eq!(calls(), 6, "corrupt baseline must fail before compilation");
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(feature = "verilator")]
#[test]
fn verilator_incremental_reuses_successes_and_invalidates_changed_inputs() {
    check_incremental(
        env!("CARGO_BIN_EXE_verify-verilator"),
        "verilator",
        "hierarchy::test_dynamic_prefix_colon_output_port_allows_zero_lsb",
    );
}

#[cfg(feature = "icarus")]
#[test]
fn icarus_incremental_reuses_successes_and_invalidates_changed_inputs() {
    check_incremental(
        env!("CARGO_BIN_EXE_verify-icarus"),
        "iverilog",
        "hierarchy::test_dynamic_output_port_rmw_preserves_unselected_bits",
    );
}
