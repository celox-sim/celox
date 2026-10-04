use hwverify_ir::ScopedSpecification;
use hwverify_verify::check_scoped_specification;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hwverify")
}

fn output(name: &str) -> PathBuf {
    root().join("target/scoped-verifier-regression").join(name)
}

fn z3() -> String {
    std::env::var("Z3_BIN").unwrap_or_else(|_| {
        root()
            .join("../proof-binding-study/.venv/bin/z3")
            .display()
            .to_string()
    })
}

fn trace(input: &str, outputs: &[&str], positive: bool, count: u64) -> Value {
    let inputs = json!({input: ["bv", 4, 1]});
    let observations = outputs
        .iter()
        .map(|name| ((*name).to_string(), json!(["bv", 4, count])))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "expect": if positive { "positive" } else { "negative" },
        "initial": {},
        "trace": [{"operation":"add", "inputs":inputs, "observe":observations}]
    })
}

fn examples(input: &str, outputs: &[&str]) -> Value {
    json!({"accept":trace(input, outputs, true, 1), "reject":trace(input, outputs, false, 2)})
}

fn document() -> Value {
    json!({
        "version":4,
        "kind":"specification",
        "name":"Scoped report regression",
        "specs":{
            "Counter":{
                "inputs":{"amount":{"bv":4}},
                "outputs":{"count":{"bv":4}},
                "state":{"x":{"bv":4}},
                "init":["eq","s.x",["bv",4,0]],
                "invariant":["eq","count","s.x"],
                "operations":{"add":["eq","n.x",["add","s.x","amount"]]},
                "examples":examples("amount", &["count"])
            },
            "Empty":{
                "inputs":{}, "outputs":{}, "state":{},
                "init":true, "invariant":true,
                "operations":{"tick":true}, "examples":{}
            }
        },
        "compositions":{
            "Pair":{
                "inputs":{"delta":{"bv":4}},
                "outputs":{"left":{"bv":4},"right":{"bv":4}},
                "instances":{
                    "a":{"target":"Counter","connections":{"amount":"delta","count":"left"}},
                    "b":{"target":"Counter","connections":{"amount":"delta","count":"right"}}
                },
                "examples":examples("delta", &["left","right"])
            },
            "Wrapped":{
                "inputs":{"step":{"bv":4}},
                "outputs":{"first":{"bv":4},"second":{"bv":4}},
                "instances":{
                    "inner":{"target":"Pair","connections":{"delta":"step","left":"first","right":"second"}}
                },
                "examples":examples("step", &["first","second"])
            }
        }
    })
}

fn check(doc: &Value, name: &str) -> Value {
    check_scoped_specification(
        &ScopedSpecification::from_json(doc).unwrap(),
        z3(),
        output(name),
    )
    .unwrap()
}

fn entry<'a>(report: &'a Value, field: &str, target: &str) -> &'a Value {
    report[field]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["target"] == target)
        .unwrap()
}

fn evidence_paths(value: &Value, paths: &mut Vec<String>) {
    match value {
        Value::Array(values) => {
            for value in values {
                evidence_paths(value, paths);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(key.as_str(), "evidence" | "solver_output" | "residual") {
                    if let Some(path) = value.as_str() {
                        paths.push(path.to_string());
                        continue;
                    }
                }
                evidence_paths(value, paths);
            }
        }
        _ => {}
    }
}

#[test]
fn coverage_and_examples_include_each_original_target_only() {
    let report = check(&document(), "targets");
    assert_eq!(report["status"], "spec_examples_passed");
    assert_eq!(report["schema_version"], 4);
    assert_eq!(report["coverage"].as_array().unwrap().len(), 4);
    assert_eq!(report["examples"].as_array().unwrap().len(), 6);
    for (target, kind, members, count) in [
        ("Counter", "spec", json!(["self"]), 1),
        ("Empty", "spec", json!(["self"]), 0),
        ("Pair", "composition", json!(["a", "b"]), 1),
        ("Wrapped", "composition", json!(["inner.a", "inner.b"]), 1),
    ] {
        let coverage = entry(&report, "coverage", target);
        assert_eq!(coverage["target_kind"], kind);
        assert_eq!(coverage["members"], members);
        assert_eq!(coverage["positive_examples"], count);
        assert_eq!(coverage["negative_examples"], count);
    }
    assert_eq!(
        entry(&report, "coverage", "Empty")["warnings"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let example = entry(&report, "examples", "Wrapped");
    let symbols = example["evidence"]["context_symbols"].as_object().unwrap();
    assert!(symbols.contains_key("frame0.private.inner.a.x"));
    assert!(symbols.contains_key("frame1.private.inner.b.x"));
    assert!(symbols.contains_key("frame1.o.first"));
    assert!(symbols.contains_key("step0.i.step"));
    assert!(report["implementation_binding"].is_null());
}

#[test]
fn target_queries_have_isolated_names_and_all_nested_evidence_paths_exist() {
    let report = check(&document(), "evidence");
    let mut paths = vec![];
    evidence_paths(&report, &mut paths);
    assert!(!paths.is_empty());
    assert_eq!(paths.len(), paths.iter().collect::<BTreeSet<_>>().len());
    for path in &paths {
        assert!(output("evidence").join(path).is_file(), "missing {path}");
        assert!(path.starts_with("target_"));
    }
    for (target, directory) in [
        ("Counter", "target_0000"),
        ("Pair", "target_0002"),
        ("Wrapped", "target_0003"),
    ] {
        let example = entry(&report, "examples", target);
        assert_eq!(example["evidence"]["name"], "example_0");
        assert_eq!(
            example["evidence"]["evidence"],
            format!("{directory}/example_0.smt2")
        );
        let script = fs::read_to_string(
            output("evidence").join(example["evidence"]["evidence"].as_str().unwrap()),
        )
        .unwrap();
        for symbol in example["evidence"]["context_symbols"]
            .as_object()
            .unwrap()
            .values()
        {
            assert!(script.contains(symbol.as_str().unwrap()));
        }
    }
    assert!(!output("evidence").join("example_0.smt2").exists());
}

#[test]
fn empty_targets_report_no_examples_without_starting_a_solver() {
    let mut doc = document();
    for declaration in doc["specs"].as_object_mut().unwrap().values_mut() {
        declaration["examples"] = json!({});
    }
    for declaration in doc["compositions"].as_object_mut().unwrap().values_mut() {
        declaration["examples"] = json!({});
    }
    let report = check_scoped_specification(
        &ScopedSpecification::from_json(&doc).unwrap(),
        "/does/not/exist".into(),
        output("no-examples"),
    )
    .unwrap();
    assert_eq!(report["status"], "no_examples");
    assert_eq!(report["coverage"].as_array().unwrap().len(), 4);
    assert_eq!(report["examples"], json!([]));
}

#[test]
fn binding_is_a_separate_safety_claim_for_only_its_selected_target() {
    let mut doc = document();
    // A forever-stuttering machine has a safety witness even though it never
    // realizes the positive add traces. The report must keep those claims apart.
    doc["implementation"] = json!({
        "composition":"Wrapped",
        "inputs":{"step":{"bv":4}, "rst":"bool"},
        "state":{"x":{"bv":4}},
        "reset_input":"rst", "reset":{"x":["bv",4,0]},
        "wires":{}, "next":{"x":"s.x"}, "operations":{"add":false},
        "binding":{
            "states":{"inner.a":{"x":"s.x"},"inner.b":{"x":"s.x"}},
            "outputs":{"first":"s.x","second":"s.x"}
        }
    });
    let report = check(&doc, "binding");
    assert_eq!(report["status"], "spec_examples_and_binding_verified");
    let binding = &report["implementation_binding"];
    assert_eq!(binding["status"], "verified");
    assert_eq!(binding["composition"], "Wrapped");
    assert_eq!(binding["target"], "Wrapped");
    assert_eq!(binding["target_kind"], "composition");
    assert_eq!(binding["members"], json!(["inner.a", "inner.b"]));
    assert_eq!(report["examples"].as_array().unwrap().len(), 6);
    assert!(report["claim"].as_str().unwrap().contains("finite"));
    assert!(binding["limitations"].to_string().contains("No liveness"));
    let labels = binding["obligations"][0]["context_symbols"]
        .as_object()
        .unwrap();
    for label in [
        "private.inner.a.x",
        "private_next.inner.a.x",
        "private.inner.b.x",
        "private_next.inner.b.x",
    ] {
        assert!(labels.contains_key(label), "missing {label}");
    }
    let mut paths = vec![];
    evidence_paths(binding, &mut paths);
    assert!(!paths.is_empty());
    for path in paths {
        assert!(path.starts_with("target_0003/"), "{path}");
        assert!(output("binding").join(path).is_file());
    }
    // Good implementation safety must not hide a rejected intended trace.
    doc["compositions"]["Wrapped"]["examples"]["reject"]["expect"] = json!("positive");
    let report = check(&doc, "binding-with-failed-example");
    assert_eq!(report["status"], "spec_examples_failed");
    assert_eq!(report["implementation_binding"]["status"], "verified");
}
