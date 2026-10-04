use hwverify_ir::Specification;
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command};
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hwverify")
}
fn out(name: &str) -> PathBuf {
    let dir = root().join("target/specification-regression").join(name);
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
fn base() -> Value {
    serde_json::from_slice(&fs::read(root().join("examples/budgeted_counter.json")).unwrap())
        .unwrap()
}
fn check(doc: &Value, name: &str) -> Value {
    hwverify_verify::check_specification(&Specification::from_json(doc).unwrap(), z3(), out(name))
        .unwrap()
}
fn case<'a>(report: &'a Value, target: &str, name: &str) -> &'a Value {
    report["examples"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["target"] == target && e["example"] == name)
        .unwrap()
}
fn set_step(doc: &mut Value, c: &str, value: Value) {
    doc["components"][c]["steps"]["add"] = value;
}
fn bare() -> Value {
    json!({"version":3,"kind":"specification","inputs":{},"observations":{"visible":"bool"},"operations":{"tick":{}},
      "components":{"A":{"state":{"hidden":"bool"},"init":true,"invariant":["eq","s.hidden","o.visible"],"steps":{"tick":["eq","n.hidden","s.hidden"]},"examples":{}}},
      "compositions":{"P":{"members":["A"],"examples":{}}}})
}
fn example(positive: bool, initial: Value, trace: Value) -> Value {
    json!({"expect":if positive {"positive"}else{"negative"},"initial":initial,"trace":trace})
}
#[test]
fn composed_examples_and_state_only_bridge_pass_with_positive_sat_witnesses() {
    let report = check(&base(), "good");
    assert_eq!(report["status"], "spec_examples_and_binding_verified");
    assert_eq!(report["implementation_binding"]["status"], "verified");
    assert_eq!(report["examples"].as_array().unwrap().len(), 7);
    let positive = case(&report, "Budgeted", "spend_all");
    let witness = fs::read_to_string(
        out("good").join(positive["evidence"]["solver_output"].as_str().unwrap()),
    )
    .unwrap();
    assert!(witness.starts_with("sat\n"));
    assert!(witness.contains("define-fun"));
    let symbols = positive["evidence"]["context_symbols"].as_object().unwrap();
    for key in [
        "frame0.Budget.credits",
        "frame2.Counter.value",
        "frame2.o.count",
        "step1.i.amount",
    ] {
        assert!(symbols.contains_key(key), "{key}");
    }
}
#[test]
fn dsl_round_trip_and_cli_example_failures() {
    for (file, status, exit) in [
        ("budgeted_counter", "spec_examples_and_binding_verified", 0),
        ("contradictory_composition", "spec_examples_failed", 1),
        ("weakened_budget", "spec_examples_failed", 1),
    ] {
        let path = root().join(format!("examples/{file}.hwv"));
        let parsed = hwverify_syntax::parse_document(
            &fs::read_to_string(&path).unwrap(),
            &path.display().to_string(),
        )
        .unwrap();
        parsed.validate_specification().unwrap();
        if file == "budgeted_counter" {
            assert_eq!(parsed.canonical, base());
        }
        let result = Command::new(env!("CARGO_BIN_EXE_hwverify-rs"))
            .arg(&path)
            .arg("--out")
            .arg(out(file))
            .arg("--z3")
            .arg(z3())
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(exit));
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["status"], status);
    }
}
#[test]
fn individually_satisfiable_components_can_disagree_only_after_an_operation() {
    let source = fs::read_to_string(root().join("examples/contradictory_composition.hwv")).unwrap();
    let parsed = hwverify_syntax::parse_document(&source, "contradiction.hwv").unwrap();
    let report = check(&parsed.canonical, "contradiction");
    for (target, name) in [
        ("One", "one_step"),
        ("Two", "one_step"),
        ("Contradiction", "common_initial_state"),
        ("Contradiction", "no_joint_extension"),
    ] {
        assert_eq!(case(&report, target, name)["status"], "passed");
    }
    let rejected = case(&report, "Contradiction", "intended_operation");
    assert_eq!(rejected["status"], "failed");
    assert_eq!(rejected["admitted"], false);
}
#[test]
fn weakening_specification_is_caught_by_negative_examples_even_if_implementation_is_good() {
    let mut doc = base();
    set_step(
        &mut doc,
        "Budget",
        json!(["eq", "n.credits", ["sub", "s.credits", "i.amount"]]),
    );
    let report = check(&doc, "weak");
    assert_eq!(report["status"], "spec_examples_failed");
    assert_eq!(
        case(&report, "Budgeted", "exclude_overspend")["admitted"],
        true
    );
    assert_eq!(report["implementation_binding"]["status"], "verified");
}
#[test]
fn omitted_observations_and_hidden_initial_state_are_existential_not_defaults() {
    let mut doc = bare();
    doc["components"]["A"]["examples"] = json!({
        "true_possible":example(true,json!({"visible":true}),json!([])),
        "false_possible":example(true,json!({"visible":false}),json!([])),
        "unspecified_is_possible":example(false,json!({}),json!([])),
        "hidden_cannot_switch":example(false,json!({"visible":true}),json!([{"operation":"tick","inputs":{},"observe":{"visible":false}}]))});
    let r = check(&doc, "existential");
    for name in ["true_possible", "false_possible", "hidden_cannot_switch"] {
        assert_eq!(case(&r, "A", name)["status"], "passed");
    }
    assert_eq!(case(&r, "A", "unspecified_is_possible")["status"], "failed");
}
#[test]
fn private_state_names_do_not_alias_across_components() {
    let mut doc = bare();
    doc["components"]["A"]["init"] = json!(["not", "s.hidden"]);
    doc["components"]["A"]["invariant"] = json!(true);
    doc["components"]["B"] = doc["components"]["A"].clone();
    doc["components"]["B"]["init"] = json!("s.hidden");
    doc["compositions"]["P"]["members"] = json!(["A", "B"]);
    doc["compositions"]["P"]["examples"]["independent"] = example(true, json!({}), json!([]));
    assert_eq!(check(&doc, "private")["status"], "spec_examples_passed");
}
#[test]
fn shared_inputs_really_are_synchronized_and_partial_inputs_are_existential() {
    let mut doc = bare();
    doc["inputs"] = json!({"bit":"bool"});
    doc["components"]["A"]["steps"]["tick"] = json!(["eq", "n.hidden", "i.bit"]);
    doc["components"]["B"] = doc["components"]["A"].clone();
    doc["components"]["B"]["steps"]["tick"] = json!(["eq", "n.hidden", ["not", "i.bit"]]);
    doc["compositions"]["P"]["members"] = json!(["A", "B"]);
    let trace = json!([{"operation":"tick","inputs":{},"observe":{}}]);
    for component in ["A", "B"] {
        doc["components"][component]["examples"]["alone"] = example(true, json!({}), trace.clone());
    }
    doc["compositions"]["P"]["examples"]["no_joint_input"] = example(false, json!({}), trace);
    assert_eq!(
        check(&doc, "shared-inputs")["status"],
        "spec_examples_passed"
    );
}
#[test]
fn operation_names_select_distinct_relations() {
    let mut doc = bare();
    doc["operations"]["flip"] = json!({});
    doc["components"]["A"]["steps"]["flip"] = json!(["eq", "n.hidden", ["not", "s.hidden"]]);
    doc["components"]["A"]["examples"]["flip"] = example(
        true,
        json!({"visible":false}),
        json!([{"operation":"flip","inputs":{},"observe":{"visible":true}},{"operation":"tick","inputs":{},"observe":{"visible":true}}]),
    );
    assert_eq!(
        check(&doc, "operation-choice")["status"],
        "spec_examples_passed"
    );
}
#[test]
fn all_specification_fields_validate_before_solver_including_unused_components() {
    for (index, mut doc) in vec![base(); 13].into_iter().enumerate() {
        match index {
            0 => {
                doc["components"]["Counter"]["steps"]["add"] = json!("n.missing");
            }
            1 => {
                doc["components"]["Counter"]["init"] = json!("i.go");
            }
            2 => {
                doc["components"]["Counter"]["invariant"] = json!("n.value");
            }
            3 => {
                doc["components"]["Counter"]["examples"]["add_two"]["initial"]["count"] =
                    json!("s.value");
            }
            4 => {
                doc["components"]["Counter"]["examples"]["add_two"]["initial"]["count"] =
                    json!(true);
            }
            5 => {
                doc["components"]["Counter"]["examples"]["add_two"]["trace"][0]["operation"] =
                    json!("missing");
            }
            6 => {
                doc["components"]["Counter"]["steps"] = json!({});
            }
            7 => {
                doc["compositions"]["Arithmetic"]["members"] = json!(["unknown"]);
            }
            8 => {
                doc["compositions"]["Arithmetic"]["members"] = json!(["Budgeted"]);
            }
            9 => {
                doc["compositions"]["Budgeted"]["members"] = json!(["Arithmetic", "Counter"]);
            }
            10 => {
                doc["compositions"]["Budgeted"]["members"] = json!([]);
            }
            11 => {
                doc["components"]["Unused"] = doc["components"]["Counter"].clone();
                doc["components"]["Unused"]["steps"]["add"] = json!("unknown");
            }
            12 => {
                doc["components"]["Counter"]["extra"] = json!(false);
            }
            _ => unreachable!(),
        }
        assert!(Specification::from_json(&doc).is_err(), "mutation {index}");
    }
}
#[test]
fn bridge_rejects_bad_reset_bad_step_and_unannounced_state_change() {
    for (index, mut doc) in vec![base(); 3].into_iter().enumerate() {
        match index {
            0 => doc["implementation"]["reset"]["count"] = json!(["bv", 4, 1]),
            1 => doc["implementation"]["next"]["count"] = json!(["add", "s.count", ["bv", 4, 1]]),
            2 => doc["implementation"]["operations"]["add"] = json!(false),
            _ => unreachable!(),
        }
        let report = check(&doc, &format!("bad-bridge-{index}"));
        assert_eq!(
            report["status"], "implementation_binding_failed",
            "{report}"
        );
    }
}
#[test]
fn forever_stuttering_bridge_proves_only_safety_not_positive_example_realization() {
    let mut doc = base();
    doc["implementation"]["operations"]["add"] = json!(false);
    doc["implementation"]["next"] = json!({"count":"s.count","remaining":"s.remaining"});
    let report = check(&doc, "stutter");
    assert_eq!(report["status"], "spec_examples_and_binding_verified");
    assert_eq!(case(&report, "Budgeted", "spend_all")["status"], "passed");
    assert!(
        report["implementation_binding"]["limitations"]
            .to_string()
            .contains("No liveness")
    );
}
#[test]
fn bridge_bindings_are_total_state_only_and_checked_before_solver() {
    for (index, mut doc) in vec![base(); 7].into_iter().enumerate() {
        match index {
            0 => doc["implementation"]["binding"]["states"]["Counter"]["value"] = json!("i.amount"),
            1 => doc["implementation"]["binding"]["states"] = json!({}),
            2 => doc["implementation"]["binding"]["observations"] = json!({}),
            3 => doc["implementation"]["binding"]["observations"]["count"] = json!(true),
            4 => doc["implementation"]["operations"] = json!({}),
            5 => doc["implementation"]["wires"]["unused"] = json!("w.missing"),
            6 => doc["implementation"]["reset_input"] = json!("amount"),
            _ => unreachable!(),
        }
        assert!(Specification::from_json(&doc).is_err(), "mutation {index}");
    }
}
#[test]
fn malformed_example_cli_never_starts_solver() {
    let mut doc = base();
    doc["components"]["Budget"]["examples"]["spend_two"]["trace"][0]["observe"]["remaining"] =
        json!("i.amount");
    let path = out("malformed").join("bad.json");
    fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_hwverify-rs"))
        .arg(path)
        .arg("--out")
        .arg(out("malformed-report"))
        .arg("--z3")
        .arg("/definitely/not/a/solver")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    let r: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(r["error"].as_str().unwrap().contains("unknown reference"));
    assert!(!r["error"].as_str().unwrap().contains("start Z3"));
}
#[cfg(unix)]
#[test]
fn unknown_and_changed_sat_witness_recheck_fail_closed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = out("solver-shims");
    let mut doc = bare();
    doc["components"]["A"]["examples"]["possible"] = example(true, json!({}), json!([]));
    let path = dir.join("spec.json");
    fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    for (name, script, status, exit) in [
        (
            "unknown",
            "#!/bin/sh\ncat >/dev/null\nprintf 'unknown\\n'\n",
            "unknown",
            3,
        ),
        (
            "changed",
            "#!/bin/sh\ns=$(cat)\ncase \"$s\" in *get-model*) printf 'unknown\\n';; *) printf 'sat\\n';; esac\n",
            "invalid_or_tool_error",
            2,
        ),
    ] {
        let shim = dir.join(name);
        fs::write(&shim, script).unwrap();
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_hwverify-rs"))
            .arg(&path)
            .arg("--out")
            .arg(dir.join(name.to_string() + "-result"))
            .arg("--z3")
            .arg(shim)
            .env("HWVERIFY_KERNEL", "off")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(exit));
        let r: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(r["status"], status);
    }
}
#[test]
fn no_examples_does_not_claim_examples_passed() {
    assert_eq!(check(&bare(), "empty-suite")["status"], "no_examples");
}
#[test]
fn bridge_requires_exclusive_operation_selection_and_reset_has_priority() {
    let mut doc = base();
    doc["operations"]["other"] = json!({});
    for component in ["Counter", "Budget"] {
        doc["components"][component]["steps"]["other"] =
            doc["components"][component]["steps"]["add"].clone();
    }
    doc["implementation"]["operations"]["other"] =
        doc["implementation"]["operations"]["add"].clone();
    let r = check(&doc, "overlap");
    assert_eq!(r["implementation_binding"]["status"], "failed");
    assert!(
        r["implementation_binding"]["obligations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["name"] == "binding_operation_exclusive" && o["status"] == "counterexample")
    );
    doc["implementation"]["operations"]["other"] = json!("i.rst");
    assert_eq!(
        check(&doc, "reset-priority")["implementation_binding"]["status"],
        "verified"
    );
}
#[test]
fn memory_observations_are_projected_as_arrays_with_hidden_memory_witnesses() {
    let mut doc = bare();
    doc["observations"] = json!({"memory":{"mem":[2,4]}});
    doc["components"]["A"]["state"] = json!({"memory":{"mem":[2,4]}});
    let zero = json!(["const_mem", 2, ["bv", 4, 0]]);
    let written = json!(["write", zero.clone(), ["bv", 2, 1], ["bv", 4, 7]]);
    doc["components"]["A"]["init"] = json!(["eq", "s.memory", zero]);
    doc["components"]["A"]["invariant"] = json!(["eq", "s.memory", "o.memory"]);
    doc["components"]["A"]["steps"]["tick"] = json!([
        "eq",
        "n.memory",
        ["write", "s.memory", ["bv", 2, 1], ["bv", 4, 7]]
    ]);
    doc["components"]["A"]["examples"]["write"] = example(
        true,
        json!({}),
        json!([{"operation":"tick","inputs":{},"observe":{"memory":written}}]),
    );
    doc["components"]["A"]["examples"]["not_zero"] = example(
        false,
        json!({}),
        json!([{"operation":"tick","inputs":{},"observe":{"memory":zero}}]),
    );
    assert_eq!(check(&doc, "memory")["status"], "spec_examples_passed");
}
#[test]
fn negative_only_suite_reports_missing_positive_coverage_without_claiming_consistency() {
    let mut doc = bare();
    doc["components"]["A"]["init"] = json!(false);
    doc["components"]["A"]["examples"]["excluded"] = example(false, json!({}), json!([]));
    let r = check(&doc, "negative-only");
    assert_eq!(r["status"], "spec_examples_passed");
    let coverage = r["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["target"] == "A")
        .unwrap();
    assert_eq!(coverage["positive_examples"], 0);
    assert_eq!(coverage["negative_examples"], 1);
    assert!(
        coverage["warnings"]
            .to_string()
            .contains("does not establish")
    );
}
#[test]
fn new_document_keyword_preserves_legacy_identifier_names() {
    let source = r#"design "keyword backwards compatibility" {
        inputs {rst:bool;} reset_input rst;
        spec {state{specification:bv<1>;}reset{specification=0u1;}next{specification=s.specification;}outputs{ready=true;}}
        impl {state{x:bv<1>;}reset{x=0u1;}next{x=s.x;}outputs{commit=true;}}
        binding spec.specification==impl.x; commit commit; can_step ready; progress{enabled true;rank 0u1;}
    }"#;
    hwverify_syntax::parse_document(source, "legacy-name.hwv")
        .unwrap()
        .validate()
        .unwrap();
}
