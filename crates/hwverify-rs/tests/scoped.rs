use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hwverify")
}
fn output(name: &str) -> PathBuf {
    let dir = root().join("target/scoped-cli-regression").join(name);
    fs::create_dir_all(&dir).unwrap();
    dir
}
fn z3() -> String {
    std::env::var("Z3_BIN").unwrap_or_else(|_| {
        root()
            .join("../proof-binding-study/.venv/bin/z3")
            .display()
            .to_string()
    })
}
fn smt_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, result);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "smt2")
            {
                result.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn scoped_cli_preserves_v4_json_and_check_never_needs_a_solver() {
    let dir = output("validate");
    let emitted = dir.join("canonical.json");
    let result = Command::new(env!("CARGO_BIN_EXE_hwverify-rs"))
        .arg(root().join("examples/scoped_dual_operator.hwv"))
        .arg("--out")
        .arg(&dir)
        .arg("--check")
        .arg("--z3")
        .arg(dir.join("nonexistent-solver"))
        .arg("--emit-json")
        .arg(&emitted)
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["status"], "validated");
    let actual: Value = serde_json::from_slice(&fs::read(emitted).unwrap()).unwrap();
    let expected: Value = serde_json::from_slice(
        &fs::read(root().join("examples/scoped_dual_operator.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(actual, expected);
    assert!(smt_files(&dir).is_empty());
}

#[test]
fn scoped_cli_surfaces_emit_identical_product_obligations() {
    for (name, status, exit) in [
        (
            "scoped_dual_operator",
            "spec_examples_and_binding_verified",
            0,
        ),
        ("scoped_contradictory_outputs", "spec_examples_failed", 1),
    ] {
        let mut reports = Vec::new();
        let mut artifacts = Vec::new();
        for format in ["json", "hwv"] {
            let dir = output(&format!("{name}_{format}"));
            let result = Command::new(env!("CARGO_BIN_EXE_hwverify-rs"))
                .arg(root().join(format!("examples/{name}.{format}")))
                .arg("--out")
                .arg(&dir)
                .arg("--z3")
                .arg(z3())
                .output()
                .unwrap();
            assert_eq!(
                result.status.code(),
                Some(exit),
                "{}",
                String::from_utf8_lossy(&result.stdout)
            );
            let report: Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(report["status"], status);
            assert_eq!(report["schema_version"], 4);
            reports.push(report);
            artifacts.push(smt_files(&dir));
        }
        assert!(!artifacts[0].is_empty());
        assert_eq!(artifacts[0], artifacts[1]);
        for field in ["coverage", "claim", "semantics", "limitations"] {
            assert_eq!(reports[0][field], reports[1][field], "{field}");
        }
        for (left, right) in reports[0]["examples"]
            .as_array()
            .unwrap()
            .iter()
            .zip(reports[1]["examples"].as_array().unwrap())
        {
            for field in [
                "target",
                "target_kind",
                "members",
                "example",
                "expect",
                "steps",
                "admitted",
                "status",
            ] {
                assert_eq!(left[field], right[field], "{field}");
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn scoped_invalid_unused_definitions_fail_before_solver_or_json_emission() {
    use std::os::unix::fs::PermissionsExt;
    let dir = output("fail_closed");
    let marker = dir.join("solver-started");
    if marker.exists() {
        fs::remove_file(&marker).unwrap();
    }
    let solver = dir.join("sentinel.sh");
    fs::write(
        &solver,
        format!(
            "#!/bin/sh\ntouch '{}'\ncat >/dev/null\necho unknown\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&solver, fs::Permissions::from_mode(0o700)).unwrap();
    let mut document: Value = serde_json::from_slice(
        &fs::read(root().join("examples/scoped_dual_operator.json")).unwrap(),
    )
    .unwrap();
    document["specs"]["Unused"] = json!({"inputs":{},"outputs":{},"state":{},"init":true,"invariant":true,"operations":{"bad":"missing"},"examples":{}});
    let source = fs::read_to_string(root().join("examples/scoped_dual_operator.hwv")).unwrap()
        + "\nspec Unused() { init true; invariant true; operation bad = missing; }\n";
    for (format, bytes) in [
        ("json", serde_json::to_vec(&document).unwrap()),
        ("hwv", source.into_bytes()),
    ] {
        let input = dir.join(format!("invalid.{format}"));
        let emitted = dir.join(format!("must-not-emit-{format}.json"));
        if emitted.exists() {
            fs::remove_file(&emitted).unwrap();
        }
        fs::write(&input, bytes).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_hwverify-rs"))
            .arg(input)
            .arg("--out")
            .arg(&dir)
            .arg("--z3")
            .arg(&solver)
            .arg("--emit-json")
            .arg(&emitted)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["status"], "invalid_or_tool_error");
        assert!(
            report["error"]
                .as_str()
                .unwrap()
                .contains("/specs/Unused/operations/bad")
        );
        assert!(!emitted.exists());
        assert!(!marker.exists());
    }
}
