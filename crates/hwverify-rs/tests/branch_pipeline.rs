use hwverify_ir::Design;
use hwverify_syntax::parse_document;
use serde_json::Value;
use std::{fs, path::PathBuf, process::Command};

const FAULTS: [&str; 7] = [
    "no_flush",
    "wrong_kill",
    "wrong_target",
    "stall_branch",
    "branch_forward",
    "branch_interlock",
    "branch_write",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hwverify")
}
fn fixture(name: &str) -> Value {
    serde_json::from_slice(&fs::read(root().join(format!("examples/{name}.json"))).unwrap())
        .unwrap()
}

#[test]
fn branch_pipeline_widths_roundtrip_and_faults_change_only_implementation() {
    for width in [4, 8, 16, 32] {
        let name = format!("branch_pipeline_w{width}");
        let doc = fixture(&name);
        Design::from_json(&doc).unwrap();
        let source = fs::read_to_string(root().join(format!("examples/{name}.hwv"))).unwrap();
        let parsed = parse_document(&source, &format!("{name}.hwv")).unwrap();
        assert_eq!(parsed.canonical, doc);
        parsed.validate().unwrap();
        assert_eq!(doc["impl"]["state"]["r0"]["bv"], width);
        assert_eq!(doc["impl"]["state"]["x_ir"]["bv"], width + 5);
        assert_eq!(doc["impl"]["state"]["pc"]["bv"], 2);
        for fault in FAULTS {
            let mut mutant = fixture(&format!("{name}_bad_{fault}"));
            Design::from_json(&mutant).unwrap();
            assert_ne!(mutant["impl"], doc["impl"]);
            mutant["impl"] = doc["impl"].clone();
            mutant["name"] = doc["name"].clone();
            assert_eq!(
                mutant, doc,
                "{name}/{fault} must only change implementation"
            );
        }
    }
}

#[test]
fn finite_branch_pipeline_proves_and_rejects_seven_faults_without_z3() {
    let mut cases = vec!["branch_pipeline_w4".to_owned()];
    cases.extend(FAULTS.map(|fault| format!("branch_pipeline_w4_bad_{fault}")));
    for name in cases {
        let out = root().join("target/branch-pipeline-regression").join(&name);
        fs::create_dir_all(&out).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_hwverify-rs"))
            .arg(root().join(format!("examples/{name}.json")))
            .args([
                "--out",
                out.to_str().unwrap(),
                "--z3",
                "/definitely/absent/z3",
            ])
            .env("HWVERIFY_SOLVER", "finite")
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        let good = !name.contains("_bad_");
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
        if name.ends_with("stall_branch") {
            assert!(
                obligations
                    .iter()
                    .any(|q| q["name"] == "hold_contract" && q["status"] == "counterexample")
            );
        }
    }
}
