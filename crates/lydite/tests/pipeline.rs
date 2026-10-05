use lydite_ir::Design;
use lydite_syntax::parse_document;
use serde_json::Value;
use std::{fs, path::PathBuf, process::Command};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lydite")
}
fn fixture(name: &str) -> Value {
    serde_json::from_slice(&fs::read(root().join(format!("examples/{name}.json"))).unwrap())
        .unwrap()
}

#[test]
fn pipeline_language_roundtrip_and_impl_only_mutations() {
    let doc = fixture("pipeline");
    Design::from_json(&doc).unwrap();
    let source = fs::read_to_string(root().join("examples/pipeline.lyd")).unwrap();
    let parsed = parse_document(&source, "pipeline.lyd").unwrap();
    assert_eq!(parsed.canonical, doc);
    parsed.validate().unwrap();
    for name in ["forward", "priority", "interlock", "retire", "stall"] {
        let mut mutant = fixture(&format!("pipeline_bad_{name}"));
        Design::from_json(&mutant).unwrap();
        assert_ne!(mutant["impl"], doc["impl"]);
        mutant["impl"] = doc["impl"].clone();
        mutant["name"] = doc["name"].clone();
        assert_eq!(mutant, doc, "{name} must modify only implementation");
    }
}

#[test]
fn finite_pipeline_proves_all_obligations_without_z3_and_rejects_faults() {
    for name in [
        "pipeline",
        "pipeline_bad_forward",
        "pipeline_bad_priority",
        "pipeline_bad_interlock",
        "pipeline_bad_retire",
        "pipeline_bad_stall",
    ] {
        let out = root().join("../target/pipeline-regression").join(name);
        fs::create_dir_all(&out).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_lydite"))
            .arg(root().join(format!("examples/{name}.json")))
            .args([
                "--out",
                out.to_str().unwrap(),
                "--z3",
                "/definitely/absent/z3",
            ])
            .env("LYDITE_SOLVER", "finite")
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        let good = name == "pipeline";
        assert_eq!(
            report["status"],
            if good {
                "stuttering_refinement_verified"
            } else {
                "counterexample"
            },
            "{name}: {report}"
        );
        assert_eq!(result.status.code(), Some(if good { 0 } else { 1 }));
        assert_eq!(report["engine_summary"]["z3_queries"], 0);
        assert!(report["engine_summary"]["finite_queries"].as_u64().unwrap() > 0);
        let obligations = report["obligations"].as_array().unwrap();
        assert_eq!(obligations.len(), 8);
        assert!(obligations.iter().all(|q| q["backend"] != "z3"));
        if good {
            assert!(obligations.iter().all(|q| q["status"] == "passed"));
        } else {
            assert!(obligations
                .iter()
                .any(|q| q["name"] == "microstep_refinement" && q["status"] == "counterexample"));
        }
    }
}
