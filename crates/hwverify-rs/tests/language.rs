use hwverify_ir::Design;
use hwverify_syntax::{parse_document, parse_json};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hwverify")
}
fn out(name: &str) -> PathBuf {
    let dir = root().join("target/language-regression").join(name);
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
const SIMPLE: &str = r#"design "small refinement" {
  inputs { rst: bool; }
  reset_input rst;
  spec {
    state { x: bv<8>; }
    reset { x = 0u8; }
    next { x = s.x + 1u8; }
    outputs { ready = true; }
  }
  impl {
    state { x: bv<8>; }
    reset { x = 0u8; }
    next { x = s.x + 1u8; }
    outputs { commit = true; }
  }
  binding spec.x == impl.x;
  commit commit;
  can_step ready;
  progress { enabled true; rank 0u1; }
}"#;

#[test]
fn language_and_json_generate_identical_obligations_and_results() {
    for (name, dsl_file, json_file) in [
        ("array", "examples/array_sum.hwv", "examples/array_sum.json"),
        (
            "auto_array",
            "examples/auto_array_sum.hwv",
            "examples/auto_array_sum.json",
        ),
        (
            "memory",
            "examples/memory_increment.hwv",
            "audit/structural_solver/memory_increment_good.json",
        ),
        (
            "memory_readable",
            "examples/memory_increment_readable.hwv",
            "audit/structural_solver/memory_increment_good.json",
        ),
    ] {
        let source = fs::read_to_string(root().join(dsl_file)).unwrap();
        let parsed = parse_document(&source, dsl_file).unwrap();
        let doc = parse_json(&fs::read(root().join(json_file)).unwrap()).unwrap();
        assert_eq!(parsed.canonical, doc);
        let dsl = parsed.validate().unwrap();
        let json = Design::from_json(&doc).unwrap();
        let dsl_dir = out(&format!("{name}_dsl"));
        let json_dir = out(&format!("{name}_json"));
        let a = hwverify_verify::check_design(&dsl, z3(), dsl_dir.clone()).unwrap();
        let b = hwverify_verify::check_design(&json, z3(), json_dir.clone()).unwrap();
        assert_eq!(a["status"], "program_and_refinement_verified");
        assert_eq!(a["status"], b["status"]);
        assert_eq!(a["normalization"], b["normalization"]);
        let ao = a["obligations"].as_array().unwrap();
        let bo = b["obligations"].as_array().unwrap();
        assert_eq!(ao.len(), bo.len());
        for (x, y) in ao.iter().zip(bo) {
            for key in ["name", "status", "solver_result", "backend", "evidence"] {
                assert_eq!(x[key], y[key], "{name} {} key {key}", x["name"]);
            }
            let evidence = x["evidence"].as_str().unwrap();
            assert_eq!(
                fs::read(dsl_dir.join(evidence)).unwrap(),
                fs::read(json_dir.join(evidence)).unwrap(),
                "{name}: {evidence}"
            );
        }
        let mut pa = a["partition_plan"].clone();
        let mut pb = b["partition_plan"].clone();
        for p in [&mut pa, &mut pb] {
            if let Some(p) = p.as_object_mut() {
                p.remove("scoring_seconds");
            }
        }
        assert_eq!(pa, pb);
    }
}

#[test]
fn invalid_names_types_assignments_and_cycles_have_source_locations() {
    for (old, new, message) in [
        (
            "binding spec.x == impl.x;",
            "binding spec.unknown == impl.x;",
            "unknown reference",
        ),
        (
            "next { x = s.x + 1u8; }",
            "next { x = s.x + 1u4; }",
            "type error",
        ),
        ("reset { x = 0u8; }", "reset { }", "assign every state"),
        ("next { x = s.x + 1u8; }", "next { }", "assign every state"),
        (
            "next { x = s.x + 1u8; }",
            "wires { a = w.b; b = w.a; } next { x = s.x; }",
            "wire cycle",
        ),
        (
            "next { x = s.x + 1u8; }",
            "wires { unused = unknown; } next { x = s.x; }",
            "unknown reference",
        ),
        (
            "binding spec.x == impl.x;",
            "binding i.rst;",
            "unknown reference",
        ),
        ("rank 0u1;", "rank true;", "rank must be unsigned word"),
        ("x: bv<8>;", "x: bv<0>;", "width must be 1..64"),
        (
            "reset { x = 0u8; }",
            "reset { x = true; }",
            "state type mismatch",
        ),
    ] {
        let source = SIMPLE.replacen(old, new, 1);
        let parsed = parse_document(&source, "negative.hwv").unwrap();
        let error = parsed.validate().unwrap_err();
        assert!(error.message.contains(message), "{new}: {error}");
        assert!(
            error
                .span
                .as_ref()
                .is_some_and(|s| s.line > 0 && s.column > 0 && s.end > s.start)
        );
        assert!(error.to_string().starts_with("negative.hwv:"));
    }
    for (old, new) in [
        ("x: bv<8>;", "x: bv<8>; x: bv<8>;"),
        ("reset { x = 0u8; }", "reset { x = 0u8; x = 1u8; }"),
        ("rank 0u1;", "rank 0u1"),
        (
            "binding spec.x == impl.x;",
            "binding spec.x < impl.x < 1u8;",
        ),
    ] {
        assert!(parse_document(&SIMPLE.replacen(old, new, 1), "syntax-error.hwv").is_err());
    }
}

#[test]
fn modified_parser_output_is_not_a_validated_design() {
    let mut parsed = parse_document(SIMPLE, "generated.hwv").unwrap();
    parsed.canonical["impl"]["next"]["x"] = json!(["add", true, true]);
    assert!(
        parsed
            .validate()
            .unwrap_err()
            .message
            .contains("type error")
    );
}

#[test]
fn unicode_diagnostic_columns_count_characters_and_spans_use_bytes() {
    let source = SIMPLE.replace("binding spec.x == impl.x;", "/* 日本語 */ binding absent;");
    let error = parse_document(&source, "unicode.hwv")
        .unwrap()
        .validate()
        .unwrap_err();
    let span = error.span.unwrap();
    assert_eq!(&source[span.start..span.end], "absent");
    let prefix = source[..span.start].rsplit('\n').next().unwrap();
    assert_eq!(span.column, prefix.chars().count() + 1);
    assert_ne!(span.column, prefix.len() + 1);
}

#[cfg(unix)]
fn sentinel(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let marker = dir.join("solver-started");
    if marker.exists() {
        fs::remove_file(&marker).unwrap();
    }
    let script = dir.join("sentinel.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\ntouch '{}'\ncat >/dev/null\necho unknown\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    (script, marker)
}
#[cfg(unix)]
#[test]
fn cli_rejects_invalid_dsl_before_solver_and_does_not_emit_invalid_json() {
    let dir = out("invalid_cli");
    let (solver, marker) = sentinel(&dir);
    let source = dir.join("invalid.hwv");
    let emit = dir.join("must-not-exist.json");
    if emit.exists() {
        fs::remove_file(&emit).unwrap();
    }
    for value in [
        SIMPLE.replace("rank 0u1;", "rank true;"),
        SIMPLE.replace("binding spec.x == impl.x;", "binding spec.x == impl.x"),
    ] {
        fs::write(&source, value).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_hwverify-rs"))
            .arg(&source)
            .args([
                "--out",
                dir.to_str().unwrap(),
                "--z3",
                solver.to_str().unwrap(),
                "--emit-json",
                emit.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["status"], "invalid_or_tool_error");
        assert!(!marker.exists());
        assert!(!emit.exists());
    }
}
#[cfg(unix)]
#[test]
fn cli_emit_json_and_check_are_validation_only() {
    let dir = out("validate_cli");
    let (solver, marker) = sentinel(&dir);
    let source = dir.join("model.txt");
    let emit = dir.join("canonical.json");
    fs::write(&source, SIMPLE).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_hwverify-rs"))
        .arg(&source)
        .args([
            "--format",
            "hwv",
            "--check",
            "--out",
            dir.to_str().unwrap(),
            "--z3",
            solver.to_str().unwrap(),
            "--emit-json",
            emit.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["status"], "validated");
    assert!(!marker.exists());
    assert_eq!(
        parse_json(&fs::read(emit).unwrap()).unwrap(),
        parse_document(SIMPLE, "expected.hwv").unwrap().canonical
    );
}

#[test]
fn nested_name_errors_point_at_the_unknown_token() {
    let source = SIMPLE.replace(
        "binding spec.x == impl.x;",
        "binding eq(\n    add(spec.missing, 1u8),\n    impl.x\n  );",
    );
    let error = parse_document(&source, "nested.hwv")
        .unwrap()
        .validate()
        .unwrap_err();
    assert!(error.message.starts_with("/binding/1/1:"));
    let span = error.span.unwrap();
    assert_eq!(&source[span.start..span.end], "spec.missing");
}

#[test]
fn language_preserves_modular_and_signed_literal_semantics() {
    for (text, expected) in [
        ("bv(3, 9)", hwverify_ir::bv(3, 1)),
        ("bv(3, -1)", hwverify_ir::bv(3, 7)),
        ("18446744073709551615u64", hwverify_ir::bv(64, u64::MAX)),
        (
            "bv(64, -9223372036854775808)",
            hwverify_ir::bv(64, 1u64 << 63),
        ),
    ] {
        let parsed = parse_document(
            &format!("design \"literal\" {{ binding {text}; }}"),
            "literal.hwv",
        )
        .unwrap();
        let term = hwverify_ir::Lower::default()
            .expr(&parsed.canonical["binding"], &hwverify_ir::Env::new())
            .unwrap();
        assert_eq!(term, expected);
    }
    for text in ["18446744073709551616u64", "bv(64, -9223372036854775809)"] {
        assert!(
            parse_document(
                &format!("design \"literal\" {{ binding {text}; }}"),
                "literal.hwv"
            )
            .is_err()
        );
    }
    for text in ["0u0", "0u65", "1 + 2"] {
        let source = SIMPLE.replace("binding spec.x == impl.x;", &format!("binding {text};"));
        assert!(
            parse_document(&source, "literal.hwv")
                .unwrap()
                .validate()
                .is_err()
        );
    }
}
