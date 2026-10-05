use crate::*;
use serde_json::{Value, json};

fn document() -> Value {
    json!({
        "version":2,
        "inputs":{"rst":"bool"},"reset_input":"rst",
        "spec":{"state":{"x":{"bv":8}},"reset":{"x":["bv",8,0]},"next":{"x":["add","s.x",["bv",8,1]]},"outputs":{"ready":true}},
        "impl":{"state":{"x":{"bv":8}},"reset":{"x":["bv",8,0]},"next":{"x":["add","s.x",["bv",8,1]]},"outputs":{"commit":true}},
        "binding":["eq","spec.x","impl.x"],"commit":"commit","can_step":"ready",
        "progress":{"enabled":true,"rank":["bv",1,0]},
        "program_contract":{"parameters":{},"precondition":true,"invariant":true,"terminal":false,"postcondition":true,"rank":["bv",8,0],"partitioning":"none"}
    })
}
#[test]
fn semantic_representation_contains_typed_transitions_and_contracts() {
    let doc = document();
    let design = Design::from_json(&doc).unwrap();
    assert_eq!(design.spec().next["x"].0.sort, Sort::Bv(8));
    assert_eq!(design.contract().unwrap().invariant.0.sort, Sort::Bool);
    assert_eq!(design.document(), &doc);
}
#[test]
fn nested_reference_diagnostic_points_to_exact_expression_path() {
    let mut doc = document();
    doc["program_contract"]["postcondition"] =
        json!(["eq", ["add", "s.missing", ["bv", 8, 1]], ["bv", 8, 0]]);
    let error = Design::from_json(&doc).unwrap_err();
    assert_eq!(error.path, "/program_contract/postcondition/1/1");
    assert!(error.message.contains("unknown reference s.missing"));
}
#[test]
fn unused_wires_and_cycles_are_still_validated() {
    let mut doc = document();
    doc["impl"]["wires"] = json!({"unused":["add", true, true]});
    assert_eq!(
        Design::from_json(&doc).unwrap_err().path,
        "/impl/wires/unused"
    );
    doc["impl"]["wires"] = json!({"a":"w.b", "b":"w.a"});
    let e = Design::from_json(&doc).unwrap_err();
    assert!(e.message.contains("wire cycle: a -> b -> a"));
}
#[test]
fn missing_assignments_and_late_contract_errors_are_rejected() {
    let mut doc = document();
    doc["spec"]["reset"] = json!({});
    assert!(
        Design::from_json(&doc)
            .unwrap_err()
            .message
            .contains("assign every state")
    );
    let mut doc = document();
    doc["program_contract"]["rank"] = json!(true);
    assert!(
        Design::from_json(&doc)
            .unwrap_err()
            .message
            .contains("program rank must be unsigned word")
    );
    doc["program_contract"]["rank"] = json!(["bv", 8, 0]);
    doc["program_contract"]["split"] = json!({"x":{"expr":"s.x","min":0,"max":256}});
    doc["program_contract"]
        .as_object_mut()
        .unwrap()
        .remove("partitioning");
    assert!(
        Design::from_json(&doc)
            .unwrap_err()
            .message
            .contains("split bounds invalid")
    );
}
#[test]
fn typed_design_keeps_a_private_snapshot() {
    let mut source = document();
    let design = Design::from_json(&source).unwrap();
    source["binding"] = json!(true);
    assert_ne!(design.document()["binding"], source["binding"]);
}
