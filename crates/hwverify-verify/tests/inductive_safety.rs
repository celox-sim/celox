use hwverify_ir::Specification;
use hwverify_verify::induction::check_inductive_safety;
use serde_json::{json, Value};
use std::{fs, process::Command};
fn document() -> Value {
    json!({"version":3,"kind":"specification","name":"Two-register strengthening",
        "inputs":{"rst":"bool","input":"bool"},"observations":{},"operations":{"tick":{}},
        "components":{"Check":{"state":{"a":"bool","b":"bool"},"init":true,"invariant":true,
            "steps":{"tick":["not","n.b"]},"examples":{}}},
        "compositions":{"System":{"members":["Check"],"examples":{}}},
        "implementation":{"composition":"System","reset_input":"rst","state":{"a":"bool","b":"bool"},
            "reset":{"a":false,"b":false},"next":{"a":false,"b":"s.a"},"wires":{},"operations":{"tick":true},
            "binding":{"states":{"Check":{"a":"s.a","b":"s.b"}},"observations":{}}}})
}
fn candidate(name: &str, predicate: Value, deps: Vec<&str>) -> Value {
    json!({"name":name,"predicate":predicate,"depends_on":deps})
}
#[test]
fn live_induction_rejects_unchecked_strengthening_and_keeps_original_targets() {
    if std::env::var("HWVERIFY_INDUCTION_TEST_CHILD").is_err() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "live_induction_rejects_unchecked_strengthening_and_keeps_original_targets",
                "--nocapture",
            ])
            .env("HWVERIFY_INDUCTION_TEST_CHILD", "1")
            .env("HWVERIFY_SOLVER", "finite")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let out = std::env::temp_dir().join(format!("hwverify-induction-tests-{}", std::process::id()));
    let run = |name: &str, doc: &Value, proposals: &Value| {
        check_inductive_safety(
            &Specification::from_json(doc).unwrap(),
            proposals,
            out.join(name),
        )
    };
    let d = document();
    let a = candidate("a_false", json!(["not", "s.a"]), vec![]);
    let b = candidate("b_false", json!(["not", "s.b"]), vec!["a_false"]);
    let good = run("good", &d, &json!([a, b])).unwrap();
    assert_eq!(good["status"], "inductive_safety_verified");
    assert_eq!(good["candidates"].as_array().unwrap().len(), 2);
    assert!(good["proofs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|q| q["backend"] == "checked_proof_bundle")
        .all(|q| q["root"].is_number() && q["coverage"]["fresh_handles_only"] == true));
    let bare = run("bare", &d, &json!([])).unwrap();
    assert_eq!(bare["status"], "induction_counterexample");
    let weak = run("weak", &d, &json!([candidate("weak", json!(true), vec![])])).unwrap();
    assert_eq!(weak["candidates"][0]["status"], "established");
    assert_eq!(weak["status"], "induction_counterexample");
    let wrong_init = run(
        "wrong-init",
        &d,
        &json!([candidate("uninitialized", json!("s.a"), vec![])]),
    )
    .unwrap();
    assert_eq!(wrong_init["status"], "induction_counterexample");
    assert_eq!(wrong_init["candidates"][0]["initialization_checked"], false);
    assert_eq!(wrong_init["target_uses"], json!([]));
    let no_dependency = run(
        "missing-dependency",
        &d,
        &json!([candidate("b", json!(["not", "s.b"]), vec![])]),
    )
    .unwrap();
    assert_eq!(
        no_dependency["candidates"][0]["initialization_checked"],
        true
    );
    assert_eq!(
        no_dependency["candidates"][0]["preservation_checked"],
        false
    );
    assert_eq!(no_dependency["target_uses"], json!([]));
    // A target invariant must not silently prove a candidate's preservation.
    let mut target_assumption = d.clone();
    target_assumption["components"]["Check"]["invariant"] = json!(["not", "s.a"]);
    let circular = run(
        "target-circular",
        &target_assumption,
        &json!([candidate("b", json!(["not", "s.b"]), vec![])]),
    )
    .unwrap();
    assert_eq!(circular["status"], "induction_counterexample");
    for (name, p) in [
        ("self", json!([candidate("a", json!(true), vec!["a"])])),
        (
            "cycle",
            json!([
                candidate("a", json!(true), vec!["b"]),
                candidate("b", json!(true), vec!["a"])
            ]),
        ),
        ("input", json!([candidate("a", json!("i.input"), vec![])])),
        ("future", json!([candidate("a", json!("n.a"), vec![])])),
        (
            "wrong-sort",
            json!([candidate("a", json!(["bv", 8, 0]), vec![])]),
        ),
        (
            "forged",
            json!([{"name":"a","predicate":true,"depends_on":[],"status":"established"}]),
        ),
        (
            "guard",
            json!([{"name":"a","predicate":false,"depends_on":[],"guard":false}]),
        ),
        (
            "duplicate",
            json!([
                candidate("a", json!(true), vec![]),
                candidate("a", json!(true), vec![])
            ]),
        ),
    ] {
        assert!(run(name, &d, &p).is_err(), "{name}");
    }
    // Old reports are descriptive, and cannot serve as new proof proposals.
    assert!(run("saved-report", &d, &good["candidates"]).is_err());
    let mut changed = d.clone();
    changed["implementation"]["next"]["a"] = json!("i.input");
    let stale = run("changed-source", &changed, &json!([a])).unwrap();
    assert_eq!(stale["status"], "induction_counterexample");
    assert_eq!(stale["candidates"][0]["preservation_checked"], false);
    // No candidate can hide a false original reset or original transition.
    changed = d.clone();
    changed["components"]["Check"]["init"] = json!("s.a");
    let bad_reset = run("target-reset", &changed, &json!([a])).unwrap();
    assert_eq!(bad_reset["status"], "induction_counterexample");
    assert_eq!(bad_reset["candidates"], json!([]));
    changed = d.clone();
    changed["implementation"]["next"]["b"] = json!(true);
    let bad_use = run("target-step", &changed, &json!([a])).unwrap();
    assert_eq!(bad_use["status"], "induction_counterexample");
    fs::remove_dir_all(out).unwrap();
}

#[test]
fn induction_requires_finite_mode_without_external_fallback() {
    if std::env::var("HWVERIFY_INDUCTION_NONFINITE_CHILD").is_err() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "induction_requires_finite_mode_without_external_fallback",
                "--nocapture",
            ])
            .env("HWVERIFY_INDUCTION_NONFINITE_CHILD", "1")
            .env("HWVERIFY_SOLVER", "external-forbidden")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let out = std::env::temp_dir().join(format!(
        "hwverify-induction-nonfinite-{}",
        std::process::id()
    ));
    let error = check_inductive_safety(
        &Specification::from_json(&document()).unwrap(),
        &json!([]),
        out.clone(),
    )
    .unwrap_err();
    assert!(error.contains("finite-only"));
    fs::remove_dir_all(out).unwrap();
}
