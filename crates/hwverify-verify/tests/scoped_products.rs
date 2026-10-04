use hwverify_ir::ScopedSpecification;
use hwverify_verify::check_scoped_specification;
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hwverify")
}

fn output(name: &str) -> PathBuf {
    root().join("target/scoped-product-regression").join(name)
}

fn z3() -> String {
    std::env::var("Z3_BIN").unwrap_or_else(|_| {
        root()
            .join("../proof-binding-study/.venv/bin/z3")
            .display()
            .to_string()
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

fn increment(state: &str, enable: Value) -> Value {
    json!(["ite", enable, ["add", state, ["bv", 4, 1]], state])
}

fn counter() -> Value {
    json!({
        "inputs":{}, "outputs":{"count":{"bv":4}}, "state":{"x":{"bv":4}},
        "init":["eq","s.x",["bv",4,0]], "invariant":["eq","count","s.x"],
        "operations":{"add":["eq","n.x",["add","s.x",["bv",4,1]]]}, "examples":{}
    })
}

fn step(actions: &[&str], left: u64, right: u64) -> Value {
    json!({"actions":actions,"inputs":{},"observe":{"left":["bv",4,left],"right":["bv",4,right]}})
}

fn independent() -> Value {
    json!({
        "version":4,"kind":"specification","specs":{"Counter":counter()},
        "compositions":{"Pair":{
            "inputs":{},"outputs":{"left":{"bv":4},"right":{"bv":4}},
            "instances":{
                "a":{"target":"Counter","connections":{"count":"left"}},
                "b":{"target":"Counter","connections":{"count":"right"}}
            },
            "operations":{"A":["a.add"],"B":["b.add"]},
            "examples":{"four_cases":{
                "expect":"positive", "initial":{"left":["bv",4,0],"right":["bv",4,0]},
                "trace":[step(&["A"],1,0),step(&["B"],1,1),step(&["A","B"],2,2),step(&[],2,2)]
            }}
        }},
        "implementation":{
            "composition":"Pair", "inputs":{"rst":"bool","ena":"bool","enb":"bool"},
            "state":{"xa":{"bv":4},"xb":{"bv":4}},"reset_input":"rst",
            "reset":{"xa":["bv",4,0],"xb":["bv",4,0]},
            "next":{"xa":increment("s.xa",json!("i.ena")),"xb":increment("s.xb",json!("i.enb"))},
            "operations":{"A":"i.ena","B":"i.enb"},
            "binding":{"states":{"a":{"x":"s.xa"},"b":{"x":"s.xb"}},"outputs":{"left":"s.xa","right":"s.xb"}}
        }
    })
}

fn binding_obligation<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["implementation_binding"]["obligations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|obligation| obligation["name"] == name)
        .unwrap()
}

#[test]
fn independent_actions_cover_a_b_both_and_neither_symbolically() {
    let doc = independent();
    let report = check(&doc, "independent");
    assert_eq!(report["status"], "spec_examples_and_binding_verified");
    assert_eq!(report["examples"].as_array().unwrap().len(), 1);
    assert_eq!(report["examples"][0]["steps"], 4);
    let binding = &report["implementation_binding"];
    assert_eq!(binding["members"], json!(["a", "b"]));
    assert_eq!(binding["actions"], json!(["A", "B"]));
    // Two one-operation leaves need no exclusivity obligation and there is only
    // one preservation query, rather than one query per action subset.
    assert_eq!(binding["obligations"].as_array().unwrap().len(), 3);
    let query = binding_obligation(&report, "binding_product_preservation");
    assert_eq!(query["solver_result"], "unsat");
    let script =
        fs::read_to_string(output("independent").join(query["evidence"].as_str().unwrap()))
            .unwrap();
    let validated = ScopedSpecification::from_json(&doc).unwrap();
    let target = &validated.targets()["Pair"];
    for control in target.action_inputs.values() {
        assert!(
            !script.contains(&format!("input_{control}")),
            "unsubstituted {control}"
        );
    }
    assert!(!script.contains("component0_state"));
    let symbols = query["context_symbols"].as_object().unwrap();
    for label in [
        "i.ena",
        "i.enb",
        "i.rst",
        "action.A",
        "action.B",
        "private.a.x",
        "private_next.b.x",
    ] {
        assert!(symbols.contains_key(label), "missing {label}");
    }
    for symbol in symbols.values() {
        assert!(script.contains(symbol.as_str().unwrap()));
    }
    let trace_symbols = report["examples"][0]["evidence"]["context_symbols"]
        .as_object()
        .unwrap();
    assert!(trace_symbols.contains_key("step0.action.A"));
    assert!(trace_symbols.contains_key("step0.action.B"));
    assert!(!trace_symbols.keys().any(|key| key.contains(".i.__scoped")));
}

#[test]
fn changing_an_inactive_private_state_is_rejected() {
    let mut doc = independent();
    doc["implementation"]["next"]["xb"] = json!(["add", "s.xb", ["bv", 4, 1]]);
    let report = check(&doc, "inactive-mutant");
    assert_eq!(report["status"], "implementation_binding_failed");
    assert_eq!(
        binding_obligation(&report, "binding_product_preservation")["status"],
        "counterexample"
    );
}

#[test]
fn swapping_instance_state_mappings_is_rejected() {
    let mut doc = independent();
    doc["implementation"]["binding"]["states"] = json!({"a":{"x":"s.xb"},"b":{"x":"s.xa"}});
    let report = check(&doc, "swapped-state-mutant");
    assert_eq!(report["status"], "implementation_binding_failed");
    assert_eq!(
        binding_obligation(&report, "binding_product_preservation")["status"],
        "counterexample"
    );
}

fn overlapping_groups(distinct_operations: bool) -> Value {
    let mut contract = counter();
    if distinct_operations {
        // Identical relations ensure rejection is about operation compatibility,
        // rather than contradictory arithmetic in the activated relations.
        contract["operations"]["other"] = contract["operations"]["add"].clone();
    }
    json!({
        "version":4,"kind":"specification","specs":{"Counter":contract},
        "compositions":{"Single":{
            "inputs":{},"outputs":{"count":{"bv":4}},
            "instances":{"a":{"target":"Counter","connections":{"count":"count"}}},
            "operations":{"A":["a.add"],"B":[if distinct_operations {"a.other"} else {"a.add"}]},
            "examples":{}
        }},
        "implementation":{
            "composition":"Single","inputs":{"rst":"bool","ena":"bool","enb":"bool"},
            "state":{"x":{"bv":4}},"reset_input":"rst","reset":{"x":["bv",4,0]},
            "next":{"x":increment("s.x",json!(["or","i.ena","i.enb"]))},
            "operations":{"A":"i.ena","B":"i.enb"},
            "binding":{"states":{"a":{"x":"s.x"}},"outputs":{"count":"s.x"}}
        }
    })
}

#[test]
fn distinct_local_operations_must_be_exclusive_even_when_relations_agree() {
    let report = check(&overlapping_groups(true), "local-collision");
    assert_eq!(report["status"], "implementation_binding_failed");
    let collision = binding_obligation(&report, "binding_leaf_exclusive_0");
    assert_eq!(collision["instance"], "a");
    assert_eq!(collision["status"], "counterexample");
    assert_eq!(
        collision["local_operations"],
        json!({"add":["A"],"other":["B"]})
    );
}

#[test]
fn overlapping_exported_groups_activate_the_same_local_operation_once() {
    let mut doc = overlapping_groups(false);
    doc["compositions"]["Single"]["examples"] = json!({
        "once":{"expect":"positive","initial":{"count":["bv",4,0]},"trace":[
            {"actions":["A","B"],"inputs":{},"observe":{"count":["bv",4,1]}},
            {"actions":[],"inputs":{},"observe":{"count":["bv",4,1]}}
        ]},
        "not_twice":{"expect":"negative","initial":{"count":["bv",4,0]},"trace":[
            {"actions":["A","B"],"inputs":{},"observe":{"count":["bv",4,2]}}
        ]}
    });
    let report = check(&doc, "same-local-overlap");
    assert_eq!(report["status"], "spec_examples_and_binding_verified");
    assert_eq!(
        report["implementation_binding"]["obligations"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn shared_output_relations_conjoin_and_can_be_contradictory() {
    let output_contract = |value| {
        json!({
            "inputs":{},"outputs":{"value":{"bv":4}},"state":{},"init":true,"invariant":true,
            "operations":{"write":["eq","no.value",["bv",4,value]]},"examples":{}
        })
    };
    let doc = json!({
        "version":4,"kind":"specification","specs":{"One":output_contract(1),"Two":output_contract(2)},
        "compositions":{"Shared":{
            "inputs":{},"outputs":{"value":{"bv":4}},
            "instances":{
                "a":{"target":"One","connections":{"value":"value"}},
                "b":{"target":"Two","connections":{"value":"value"}}
            },
            "operations":{"A":["a.write"],"B":["b.write"],"Both":["a.write","b.write"]},
            "examples":{
                "a_only":{"expect":"positive","initial":{},"trace":[{"operation":"A","inputs":{},"observe":{"value":["bv",4,1]}}]},
                "b_only":{"expect":"positive","initial":{},"trace":[{"operation":"B","inputs":{},"observe":{"value":["bv",4,2]}}]},
                "simultaneous":{"expect":"negative","initial":{},"trace":[{"actions":["A","B"],"inputs":{},"observe":{}}]},
                "intentional_same_event":{"expect":"negative","initial":{},"trace":[{"operation":"Both","inputs":{},"observe":{}}]}
            }
        }},
        "implementation":{
            "composition":"Shared","inputs":{"rst":"bool"},"state":{"x":{"bv":4}},
            "reset_input":"rst","reset":{"x":["bv",4,0]},"next":{"x":["bv",4,1]},
            "operations":{"A":true,"B":true,"Both":false},
            "binding":{"states":{"a":{},"b":{}},"outputs":{"value":"s.x"}}
        }
    });
    let report = check(&doc, "contradictory-shared-output");
    assert!(
        report["examples"]
            .as_array()
            .unwrap()
            .iter()
            .all(|example| example["status"] == "passed")
    );
    assert_eq!(report["status"], "implementation_binding_failed");
    assert_eq!(
        binding_obligation(&report, "binding_product_preservation")["status"],
        "counterexample"
    );
}

#[test]
fn output_only_contract_allows_unconstrained_idle_output_changes() {
    let doc = json!({
        "version":4,"kind":"specification","specs":{"Output":{
            "inputs":{},"outputs":{"value":{"bv":4}},"state":{},"init":true,"invariant":true,
            "operations":{"sample":true},"examples":{"idle_change":{
                "expect":"positive","initial":{"value":["bv",4,0]},
                "trace":[{"actions":[],"inputs":{},"observe":{"value":["bv",4,1]}}]
            }}
        }},"compositions":{"Shell":{
            "inputs":{},"outputs":{"value":{"bv":4}},
            "instances":{"a":{"target":"Output","connections":{"value":"value"}}},"examples":{}
        }},
        "implementation":{
            "composition":"Shell","inputs":{"rst":"bool"},"state":{"x":{"bv":4}},
            "reset_input":"rst","reset":{"x":["bv",4,0]},"next":{"x":["add","s.x",["bv",4,1]]},
            "operations":{"sample":false},"binding":{"states":{"a":{}},"outputs":{"value":"s.x"}}
        }
    });
    let report = check(&doc, "output-only-idle");
    assert_eq!(report["status"], "spec_examples_and_binding_verified");
    assert!(
        report["implementation_binding"]["limitations"]
            .to_string()
            .contains("no general observable-stutter claim")
    );
}

#[test]
fn nested_exported_groups_expand_as_sets_in_traces_and_binding() {
    let mut doc = independent();
    doc["compositions"]["Wrapped"] = json!({
        "inputs":{},"outputs":{"left":{"bv":4},"right":{"bv":4}},
        "instances":{"inner":{"target":"Pair","connections":{"left":"left","right":"right"}}},
        "operations":{"left":["inner.A"],"right":["inner.B"],"both":["inner.A","inner.B"]},
        "examples":{"nested":{
            "expect":"positive","initial":{"left":["bv",4,0],"right":["bv",4,0]},
            "trace":[step(&["left"],1,0),step(&["right"],1,1),step(&["both","left","right"],2,2),step(&[],2,2)]
        }}
    });
    doc["implementation"]["composition"] = json!("Wrapped");
    doc["implementation"]["inputs"]["all"] = json!("bool");
    doc["implementation"]["next"] = json!({
        "xa":increment("s.xa",json!(["or","i.ena","i.all"])),
        "xb":increment("s.xb",json!(["or","i.enb","i.all"]))
    });
    doc["implementation"]["operations"] = json!({"left":"i.ena","right":"i.enb","both":"i.all"});
    doc["implementation"]["binding"]["states"] =
        json!({"inner.a":{"x":"s.xa"},"inner.b":{"x":"s.xb"}});
    let report = check(&doc, "nested-groups");
    assert_eq!(report["status"], "spec_examples_and_binding_verified");
    assert_eq!(
        report["implementation_binding"]["members"],
        json!(["inner.a", "inner.b"])
    );
    assert_eq!(
        report["implementation_binding"]["obligations"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn reset_must_establish_nonempty_product_invariants() {
    let mut doc = independent();
    doc["implementation"]["reset"]["xa"] = json!(["bv", 4, 1]);
    let report = check(&doc, "reset-mutant");
    assert_eq!(report["status"], "implementation_binding_failed");
    assert_eq!(
        binding_obligation(&report, "binding_reset_nonempty")["status"],
        "failed_nonvacuity"
    );
    assert_eq!(
        binding_obligation(&report, "binding_reset_establishes_product")["status"],
        "counterexample"
    );
}

#[test]
fn real_inputs_with_action_like_names_keep_their_witness_symbols() {
    let mut doc = independent();
    let ports = json!({"A":"bool","__scoped_action_0":"bool"});
    doc["specs"]["Counter"]["inputs"] = ports.clone();
    doc["compositions"]["Pair"]["inputs"] = ports;
    for instance in ["a", "b"] {
        for input in ["A", "__scoped_action_0"] {
            doc["compositions"]["Pair"]["instances"][instance]["connections"][input] = json!(input);
        }
    }
    for input in ["A", "__scoped_action_0"] {
        doc["implementation"]["inputs"][input] = json!("bool");
    }
    for step in doc["compositions"]["Pair"]["examples"]["four_cases"]["trace"]
        .as_array_mut()
        .unwrap()
    {
        step["inputs"] = json!({"A":true,"__scoped_action_0":false});
    }
    let report = check(&doc, "input-hygiene");
    assert_eq!(report["status"], "spec_examples_and_binding_verified");
    let trace = &report["examples"][0]["evidence"];
    let symbols = trace["context_symbols"].as_object().unwrap();
    for label in [
        "step0.i.A",
        "step0.i.__scoped_action_0",
        "step0.action.A",
        "step0.action.B",
    ] {
        assert!(symbols.contains_key(label), "missing {label}");
    }
    let script =
        fs::read_to_string(output("input-hygiene").join(trace["evidence"].as_str().unwrap()))
            .unwrap();
    for symbol in symbols.values() {
        assert!(script.contains(symbol.as_str().unwrap()));
    }
    let query = binding_obligation(&report, "binding_product_preservation");
    for input in ["i.A", "i.__scoped_action_0", "i.ena", "i.enb", "i.rst"] {
        assert!(
            query["context_symbols"].get(input).is_some(),
            "missing {input}"
        );
    }
}

#[test]
fn private_aliases_cannot_overwrite_output_implementation_or_next_contexts() {
    let mut doc = independent();
    let aliases = ["o", "impl", "a", "a_next"];
    let mut outputs = serde_json::Map::new();
    let mut instances = serde_json::Map::new();
    let mut states = serde_json::Map::new();
    let mut observed = serde_json::Map::new();
    for alias in aliases {
        let output = format!("out_{alias}");
        outputs.insert(output.clone(), json!({"bv":4}));
        instances.insert(
            alias.into(),
            json!({"target":"Counter","connections":{"count":output}}),
        );
        states.insert(alias.into(), json!({"x":"s.xa"}));
        observed.insert(output, json!("s.xa"));
    }
    doc["compositions"]["Pair"] =
        json!({"inputs":{},"outputs":outputs,"instances":instances,"examples":{}});
    doc["implementation"]["operations"] = json!({"add":"i.ena"});
    doc["implementation"]["binding"] = json!({"states":states,"outputs":observed});
    let report = check(&doc, "alias-hygiene");
    assert_eq!(report["status"], "binding_verified_no_examples");
    let query = binding_obligation(&report, "binding_product_preservation");
    let symbols = query["context_symbols"].as_object().unwrap();
    for alias in aliases {
        for label in [
            format!("private.{alias}.x"),
            format!("private_next.{alias}.x"),
            format!("o.out_{alias}"),
            format!("no.out_{alias}"),
        ] {
            assert!(symbols.contains_key(&label), "missing {label}");
        }
    }
    assert!(symbols.contains_key("impl.xa"));
    assert!(symbols.contains_key("impl_next.xa"));
}
