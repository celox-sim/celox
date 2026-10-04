use super::*;
use crate::{bv, Sort};

fn leaf() -> Value {
    json!({
        "inputs": {"amount": {"bv": 8}},
        "outputs": {"value": {"bv": 8}},
        "state": {"x": {"bv": 8}},
        "init": ["eq", "s.x", ["bv", 8, 0]],
        "invariant": ["eq", "value", "s.x"],
        "operations": {"advance": ["and", ["eq", "n.x", ["add", "s.x", "amount"]], ["eq", "no.value", "n.x"]]},
        "examples": {"own": {"expect": "positive", "initial": {"value": ["bv", 8, 0]}, "trace": []}}
    })
}

fn composition(target: &str) -> Value {
    json!({
        "inputs": {"delta": {"bv": 8}},
        "outputs": {"result": {"bv": 8}},
        "instances": {"counter": {"target": target, "connections": {"amount": "delta", "value": "result"}}},
        "examples": {}
    })
}

fn document() -> Value {
    json!({"version": 4, "kind": "specification", "specs": {"Counter": leaf()}, "compositions": {"Machine": composition("Counter")}})
}

fn implementation() -> Value {
    json!({
        "composition": "Machine", "inputs": {"delta": {"bv": 8}, "rst": "bool"}, "reset_input": "rst",
        "state": {"count": {"bv": 8}}, "reset": {"count": ["bv", 8, 0]},
        "next": {"count": ["add", "s.count", "i.delta"]}, "operations": {"advance": ["not", "i.rst"]},
        "binding": {"states": {"counter": {"x": "s.count"}}, "outputs": {"result": "s.count"}}
    })
}

#[test]
fn standalone_and_composition_have_independent_complete_v3_targets() {
    let doc = document();
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    assert_eq!(scoped.document(), &doc);
    assert_eq!(scoped.targets().len(), 2);
    let leaf = &scoped.targets()["Counter"];
    assert_eq!(leaf.kind, "spec");
    assert_eq!(
        leaf.instance_paths.values().collect::<Vec<_>>(),
        [&"self".to_string()]
    );
    assert_eq!(leaf.specification.compositions().len(), 1);
    assert_eq!(
        leaf.specification.compositions()["Counter"].examples.len(),
        1
    );
    assert!(leaf
        .specification
        .components()
        .values()
        .all(|leaf| leaf.examples.is_empty()));
    assert!(leaf.specification.inputs().contains_key("amount"));
    assert!(!leaf.specification.inputs().contains_key("delta"));
    let target = &scoped.targets()["Machine"];
    assert_eq!(target.kind, "composition");
    assert_eq!(
        target.instance_paths.values().collect::<Vec<_>>(),
        [&"counter".to_string()]
    );
    assert!(target.specification.compositions()["Machine"]
        .examples
        .is_empty());
    let component = target.specification.components().values().next().unwrap();
    assert_eq!(
        component.invariant.0.args[0],
        target.specification.observations()["result"]
    );
    assert!(format!("{:?}", component.steps.values().next().unwrap()).contains("input_delta"));
    assert!(!format!("{:?}", component.steps.values().next().unwrap()).contains("input_amount"));
}

#[test]
fn nested_repeated_instances_compose_connections_and_have_fresh_private_state() {
    let mut doc = document();
    doc["compositions"]["Outer"] = json!({
        "inputs": {"left_delta": {"bv": 8}, "right_delta": {"bv": 8}},
        "outputs": {"left_value": {"bv": 8}, "right_value": {"bv": 8}},
        "instances": {
            "left": {"target": "Machine", "connections": {"delta": "left_delta", "result": "left_value"}},
            "right": {"target": "Machine", "connections": {"delta": "right_delta", "result": "right_value"}}
        }, "examples": {}
    });
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["Outer"];
    assert_eq!(
        target
            .instance_paths
            .values()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["left.counter".into(), "right.counter".into()])
    );
    let components: Vec<_> = target.specification.components().values().collect();
    assert_ne!(components[0].state["x"], components[1].state["x"]);
    assert_ne!(components[0].next_state["x"], components[1].next_state["x"]);
    assert_ne!(components[0].state["x"], components[0].next_state["x"]);
    assert_eq!(components[0].initial.0.args[1], bv(8, 0));
    assert!(
        format!("{:?}", components[0].steps.values().next().unwrap()).contains("input_left_delta")
    );
    assert!(
        format!("{:?}", components[1].steps.values().next().unwrap()).contains("input_right_delta")
    );
}

#[test]
fn unrelated_templates_may_reuse_port_names_with_different_types_and_operations() {
    let mut doc = document();
    doc["specs"]["Switch"] = json!({
        "inputs": {"amount": "bool"}, "outputs": {"value": "bool"}, "state": {"x": "bool"},
        "init": true, "invariant": ["eq", "o.value", "s.x"],
        "operations": {"toggle": ["eq", "n.x", "i.amount"]}, "examples": {}
    });
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    assert_eq!(
        scoped.targets()["Switch"].specification.inputs()["amount"]
            .0
            .sort,
        Sort::Bool
    );
    assert_eq!(
        scoped.targets()["Counter"].specification.inputs()["amount"]
            .0
            .sort,
        Sort::Bv(8)
    );
    assert_eq!(
        scoped.targets()["Switch"]
            .action_inputs
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["toggle".into()])
    );
}

#[test]
fn connection_validation_rejects_missing_unknown_wrong_direction_and_wrong_type() {
    let cases = [
        (json!({"amount": "delta"}), "missing connection"),
        (
            json!({"amount": "delta", "value": "result", "extra": "delta"}),
            "unknown child port",
        ),
        (
            json!({"amount": "result", "value": "result"}),
            "direction mismatch",
        ),
        (
            json!({"amount": "delta", "value": "delta"}),
            "direction mismatch",
        ),
        (
            json!({"amount": "missing", "value": "result"}),
            "unknown enclosing input",
        ),
        (
            json!({"amount": ["bv", 8, 1], "value": "result"}),
            "expected string",
        ),
    ];
    for (connections, message) in cases {
        let mut doc = document();
        doc["compositions"]["Machine"]["instances"]["counter"]["connections"] = connections;
        let error = ScopedSpecification::from_json(&doc).unwrap_err();
        assert!(error.message.contains(message), "{error}");
        assert!(
            error
                .path
                .starts_with("/compositions/Machine/instances/counter/connections"),
            "{error}"
        );
    }
    let mut doc = document();
    doc["compositions"]["Machine"]["inputs"]["delta"] = json!({"bv": 4});
    let error = ScopedSpecification::from_json(&doc).unwrap_err();
    assert!(error.message.contains("type mismatch"));
    assert_eq!(
        error.path,
        "/compositions/Machine/instances/counter/connections/amount"
    );
}

#[test]
fn repeated_outputs_are_relational_constraints_not_driver_errors() {
    let mut doc = document();
    doc["compositions"]["Machine"]["instances"]["other"] =
        doc["compositions"]["Machine"]["instances"]["counter"].clone();
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    assert_eq!(
        scoped.targets()["Machine"].specification.components().len(),
        2
    );
}

#[test]
fn rejects_direct_and_indirect_cycles_before_expansion() {
    let empty_cycle = |target: &str| json!({"inputs": {}, "outputs": {}, "instances": {"child": {"target": target, "connections": {}}}, "examples": {}});
    let mut doc = document();
    doc["compositions"]["Loop"] = empty_cycle("Loop");
    assert!(ScopedSpecification::from_json(&doc)
        .unwrap_err()
        .message
        .contains("composition cycle"));
    doc["compositions"]["Loop"] = empty_cycle("Other");
    doc["compositions"]["Other"] = empty_cycle("Loop");
    assert!(ScopedSpecification::from_json(&doc)
        .unwrap_err()
        .message
        .contains("composition cycle"));
}

#[test]
fn rejects_unknown_targets_empty_compositions_and_mixed_operations() {
    let mut doc = document();
    doc["compositions"]["Machine"]["instances"]["counter"]["target"] = json!("Absent");
    assert_eq!(
        ScopedSpecification::from_json(&doc).unwrap_err().path,
        "/compositions/Machine/instances/counter/target"
    );
    let mut doc = document();
    doc["compositions"]["Machine"]["instances"] = json!({});
    assert!(ScopedSpecification::from_json(&doc)
        .unwrap_err()
        .message
        .contains("at least one instance"));
    let mut doc = document();
    doc["specs"]["Other"] = leaf();
    doc["specs"]["Other"]["operations"] = json!({"different": true});
    doc["compositions"]["Machine"]["instances"]["other"] =
        json!({"target": "Other", "connections": {"amount": "delta", "value": "result"}});
    assert!(ScopedSpecification::from_json(&doc)
        .unwrap_err()
        .message
        .contains("identical nonempty operation sets"));
}

#[test]
fn rejects_global_capture_even_when_parent_or_implementation_has_matching_name() {
    for reference in [
        "delta",
        "i.delta",
        "result",
        "o.result",
        "no.result",
        "w.amount",
        "x",
    ] {
        let mut doc = document();
        doc["specs"]["Counter"]["operations"]["advance"] = json!(["eq", reference, "s.x"]);
        let error = ScopedSpecification::from_json(&doc).unwrap_err();
        assert_eq!(
            error.path, "/specs/Counter/operations/advance/1",
            "{reference}: {error}"
        );
        assert!(error.message.contains("unknown"));
    }
    let mut doc = document();
    doc["implementation"] = implementation();
    doc["specs"]["Counter"]["operations"]["advance"] = json!("i.rst");
    assert!(ScopedSpecification::from_json(&doc)
        .unwrap_err()
        .message
        .contains("unknown local reference i.rst"));
}

#[test]
fn exact_source_paths_survive_legacy_type_and_scope_validation() {
    let cases = [
        (
            "init",
            json!(["eq", "amount", ["bv", 8, 0]]),
            "/specs/Counter/init/1",
        ),
        (
            "invariant",
            json!(["eq", "no.value", "s.x"]),
            "/specs/Counter/invariant/1",
        ),
        (
            "invariant",
            json!(["eq", "value", "s.missing"]),
            "/specs/Counter/invariant/2",
        ),
        (
            "invariant",
            json!(["add", "value", true]),
            "/specs/Counter/invariant",
        ),
    ];
    for (field, expression, path) in cases {
        let mut doc = document();
        doc["specs"]["Counter"][field] = expression;
        assert_eq!(ScopedSpecification::from_json(&doc).unwrap_err().path, path);
    }
}

#[test]
fn invalid_unused_templates_and_local_examples_are_checked() {
    let mut doc = document();
    doc["specs"]["Unused"] = leaf();
    doc["specs"]["Unused"]["operations"]["advance"] = json!(["eq", "n.missing", "s.x"]);
    assert_eq!(
        ScopedSpecification::from_json(&doc).unwrap_err().path,
        "/specs/Unused/operations/advance/1"
    );
    let mut doc = document();
    doc["implementation"] = implementation();
    doc["compositions"]["Machine"]["examples"] = json!({"control_capture": {"expect": "positive", "initial": {}, "trace": [{"operation": "advance", "inputs": {"rst": false}, "observe": {}}]}});
    assert_eq!(
        ScopedSpecification::from_json(&doc).unwrap_err().path,
        "/compositions/Machine/examples/control_capture/trace/0/inputs/rst"
    );
}

#[test]
fn implementation_controls_only_extend_the_selected_target() {
    let mut doc = document();
    doc["implementation"] = implementation();
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["Machine"];
    assert!(!target.specification.inputs().contains_key("rst"));
    assert!(target.implementation_inputs.contains_key("rst"));
    assert!(target.specification.implementation().is_none());
    assert_eq!(
        target.implementation.as_ref().unwrap().composition,
        "Machine"
    );
    assert!(!scoped.targets()["Counter"]
        .specification
        .inputs()
        .contains_key("rst"));
    assert!(scoped.targets()["Counter"]
        .specification
        .implementation()
        .is_none());
    let id = target.instance_paths.keys().next().unwrap();
    assert!(target
        .implementation
        .as_ref()
        .unwrap()
        .states
        .contains_key(id));
}

#[test]
fn implementation_requires_exact_target_inputs_leaf_paths_and_output_bindings() {
    let mut doc = document();
    doc["implementation"] = implementation();
    doc["implementation"]["inputs"]
        .as_object_mut()
        .unwrap()
        .remove("delta");
    assert!(ScopedSpecification::from_json(&doc)
        .unwrap_err()
        .message
        .contains("missing target input delta"));
    doc["implementation"]["inputs"]["delta"] = json!("bool");
    assert_eq!(
        ScopedSpecification::from_json(&doc).unwrap_err().path,
        "/implementation/inputs/delta"
    );
    for states in [
        json!({}),
        json!({"Counter": {"x": "s.count"}}),
        json!({"counter": {"x": "s.count"}, "extra": {"x": "s.count"}}),
    ] {
        let mut doc = document();
        doc["implementation"] = implementation();
        doc["implementation"]["binding"]["states"] = states;
        assert_eq!(
            ScopedSpecification::from_json(&doc).unwrap_err().path,
            "/implementation/binding/states"
        );
    }
    for outputs in [json!({}), json!({"result": "s.count", "extra": "s.count"})] {
        let mut doc = document();
        doc["implementation"] = implementation();
        doc["implementation"]["binding"]["outputs"] = outputs;
        assert_eq!(
            ScopedSpecification::from_json(&doc).unwrap_err().path,
            "/implementation/binding/outputs"
        );
    }
}

#[test]
fn nested_binding_uses_full_path_and_state_only_expressions() {
    let mut doc = document();
    doc["compositions"]["Outer"] = json!({"inputs": {"delta": {"bv": 8}}, "outputs": {"result": {"bv": 8}}, "instances": {"nested": {"target": "Machine", "connections": {"delta": "delta", "result": "result"}}}, "examples": {}});
    doc["implementation"] = implementation();
    doc["implementation"]["composition"] = json!("Outer");
    doc["implementation"]["binding"]["states"] = json!({"nested.counter": {"x": "s.count"}});
    assert!(ScopedSpecification::from_json(&doc).is_ok());
    doc["implementation"]["binding"]["states"]["nested.counter"]["x"] = json!("i.delta");
    let error = ScopedSpecification::from_json(&doc).unwrap_err();
    assert_eq!(
        error.path,
        "/implementation/binding/states/nested.counter/x"
    );
    assert!(error.message.contains("unknown reference i.delta"));
    doc["implementation"]["binding"]["states"] = json!({"nested": {"x": "s.count"}});
    assert!(ScopedSpecification::from_json(&doc)
        .unwrap_err()
        .message
        .contains("every instantiated leaf path"));
}

#[test]
fn strict_shapes_and_namespace_collisions_are_rejected() {
    let mut doc = document();
    doc["specs"]["Counter"]["inputs"]["value"] = json!({"bv": 8});
    assert!(ScopedSpecification::from_json(&doc)
        .unwrap_err()
        .message
        .contains("input and output names must be distinct"));
    let mut doc = document();
    doc["compositions"]["Counter"] = composition("Counter");
    assert!(ScopedSpecification::from_json(&doc)
        .unwrap_err()
        .message
        .contains("spec and composition names must be distinct"));
    for (field, extra) in [("specs", "steps"), ("compositions", "members")] {
        let mut doc = document();
        let name = if field == "specs" {
            "Counter"
        } else {
            "Machine"
        };
        doc[field][name][extra] = json!({});
        assert!(ScopedSpecification::from_json(&doc)
            .unwrap_err()
            .message
            .contains("unsupported field"));
    }
    let mut doc = document();
    doc["compositions"]["Machine"]["instances"]["a.b"] =
        doc["compositions"]["Machine"]["instances"]["counter"].clone();
    assert!(ScopedSpecification::from_json(&doc)
        .unwrap_err()
        .message
        .contains("invalid field name a.b"));
}

#[test]
fn generated_component_identity_never_collides_with_target_name() {
    let mut doc = document();
    doc["specs"]["__scoped_leaf_0"] = leaf();
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["__scoped_leaf_0"];
    assert!(!target
        .specification
        .components()
        .contains_key("__scoped_leaf_0"));
    assert!(target
        .specification
        .compositions()
        .contains_key("__scoped_leaf_0"));
}

#[test]
fn complete_expression_asts_keep_literal_and_metadata_arguments() {
    let mut doc = document();
    let expression = json!([
        "eq",
        [
            "extract",
            7,
            0,
            [
                "zext",
                8,
                ["read", ["const_mem", 4, "amount"], ["bv", 4, -1]]
            ]
        ],
        "n.x"
    ]);
    doc["specs"]["Counter"]["operations"]["advance"] = expression.clone();
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["Machine"];
    let id = target.instance_paths.keys().next().unwrap();
    let mut expected = expression;
    expected[1][3][2][1][2] = json!("i.delta");
    let tick = target.specification.operations().iter().next().unwrap();
    fn contains(value: &Value, expected: &Value) -> bool {
        value == expected
            || value
                .as_array()
                .is_some_and(|items| items.iter().any(|item| contains(item, expected)))
    }
    assert!(contains(
        &target.specification.document()["components"][id]["steps"][tick],
        &expected
    ));
}

#[test]
fn expansion_limit_rejects_exponential_products_before_allocating_them() {
    let mut doc = json!({"version": 4, "kind": "specification", "specs": {"Unit": {"inputs": {}, "outputs": {}, "state": {}, "init": true, "invariant": true, "operations": {"step": true}, "examples": {}}}, "compositions": {}});
    let mut previous = "Unit".to_string();
    for index in 0..13 {
        let name = format!("Level{index:02}");
        doc["compositions"][&name] = json!({"inputs": {}, "outputs": {}, "instances": {"a": {"target": previous, "connections": {}}, "b": {"target": previous, "connections": {}}}, "examples": {}});
        previous = name;
    }
    assert!(ScopedSpecification::from_json(&doc)
        .unwrap_err()
        .message
        .contains("4096-leaf expansion limit"));
}

#[test]
fn nesting_limit_accounts_for_memoized_subtrees() {
    let mut doc = json!({"version": 4, "kind": "specification", "specs": {"Unit": {"inputs": {}, "outputs": {}, "state": {}, "init": true, "invariant": true, "operations": {"step": true}, "examples": {}}}, "compositions": {}});
    let mut previous = "Unit".to_string();
    for index in 0..65 {
        let name = format!("Level{index:02}");
        doc["compositions"][&name] = json!({"inputs": {}, "outputs": {}, "instances": {"child": {"target": previous, "connections": {}}}, "examples": {}});
        previous = name;
    }
    assert!(ScopedSpecification::from_json(&doc)
        .unwrap_err()
        .message
        .contains("64-level nesting limit"));
}

#[test]
fn scoped_examples_validate_and_keep_each_declared_example_exactly_once() {
    for source in [
        include_str!("../../../../examples/scoped_budgeted_counter.json"),
        include_str!("../../../../examples/scoped_independent_counters.json"),
        include_str!("../../../../examples/scoped_contradictory_outputs.json"),
    ] {
        let document: Value = serde_json::from_str(source).unwrap();
        let scoped = ScopedSpecification::from_json(&document).unwrap();
        let declared_examples: usize = ["specs", "compositions"]
            .into_iter()
            .flat_map(|field| document[field].as_object().unwrap().values())
            .map(|declaration| declaration["examples"].as_object().unwrap().len())
            .sum();
        let elaborated_examples: usize = scoped
            .targets()
            .values()
            .map(|target| {
                assert!(target
                    .specification
                    .components()
                    .values()
                    .all(|component| component.examples.is_empty()));
                target
                    .specification
                    .compositions()
                    .values()
                    .map(|composition| composition.examples.len())
                    .sum::<usize>()
            })
            .sum();
        assert_eq!(declared_examples, elaborated_examples);
    }
}

#[test]
fn document_expansion_limit_bounds_many_individually_small_targets() {
    let unit = json!({"inputs": {}, "outputs": {}, "state": {}, "init": true, "invariant": true, "operations": {"step": true}, "examples": {}});
    let mut document =
        json!({"version": 4, "kind": "specification", "specs": {"Unit": unit}, "compositions": {}});
    let mut previous = "Unit".to_string();
    for index in 0..12 {
        let name = format!("Level{index:02}");
        document["compositions"][&name] = json!({"inputs": {}, "outputs": {}, "instances": {"a": {"target": previous, "connections": {}}, "b": {"target": previous, "connections": {}}}, "examples": {}});
        previous = name;
    }
    for index in 0..3 {
        let name = format!("Wrapper{index}");
        document["compositions"][&name] = json!({"inputs": {}, "outputs": {}, "instances": {"leaf": {"target": previous, "connections": {}}}, "examples": {}});
    }
    assert!(ScopedSpecification::from_json(&document)
        .unwrap_err()
        .message
        .contains("16384 elaborated-leaf limit"));
}

fn independent_actions_document() -> Value {
    json!({
        "version": 4, "kind": "specification",
        "specs": {
            "Bit": {"inputs": {}, "outputs": {"value": "bool"}, "state": {"bit": "bool"},
                "init": ["not", "s.bit"], "invariant": ["eq", "value", "s.bit"],
                "operations": {"set": ["eq", "n.bit", true], "clear": ["eq", "n.bit", false]}, "examples": {}},
            "Other": {"inputs": {}, "outputs": {"value": "bool"}, "state": {"bit": "bool"},
                "init": true, "invariant": ["eq", "value", "s.bit"],
                "operations": {"flip": ["eq", "n.bit", ["not", "s.bit"]]}, "examples": {}}
        },
        "compositions": {"Pair": {"inputs": {}, "outputs": {"left": "bool", "right": "bool"},
            "instances": {
                "left": {"target": "Bit", "connections": {"value": "left"}},
                "right": {"target": "Other", "connections": {"value": "right"}}
            },
            "operations": {"left_set": ["left.set"], "left_clear": ["left.clear"], "right_flip": ["right.flip"]},
            "examples": {}
        }}
    })
}

fn leaf_id<'a>(target: &'a ScopedTarget, path: &str) -> &'a str {
    target
        .instance_paths
        .iter()
        .find(|(_, candidate)| *candidate == path)
        .unwrap()
        .0
}

// Evaluate the finite Boolean fragment used by these symbolic-activation tests.
// Exhausting state assignments checks contradictory activation without solver I/O.
fn eval_boolean(term: &crate::Term, variables: &BTreeMap<crate::Term, bool>) -> bool {
    if let Some(value) = variables.get(term) {
        return *value;
    }
    let arg = |index| eval_boolean(&term.0.args[index], variables);
    match term.0.op.as_str() {
        "true" => true,
        "false" => false,
        "not" => !arg(0),
        "and" => arg(0) && arg(1),
        "or" => arg(0) || arg(1),
        "=>" => !arg(0) || arg(1),
        "=" => arg(0) == arg(1),
        other => panic!("unassigned Boolean term {other}"),
    }
}

fn activation_values(target: &ScopedTarget, actions: &[&str]) -> BTreeMap<crate::Term, bool> {
    target
        .action_inputs
        .iter()
        .map(|(action, input)| {
            (
                target.specification.inputs()[input].clone(),
                actions.contains(&action.as_str()),
            )
        })
        .collect()
}

#[test]
fn explicit_groups_allow_independent_children_with_different_operations() {
    let doc = independent_actions_document();
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["Pair"];
    assert_eq!(scoped.document(), &doc);
    assert_eq!(target.specification.operations().len(), 1);
    assert_eq!(
        target.leaf_operations[leaf_id(target, "left")]["set"],
        BTreeSet::from(["left_set".into()])
    );
    assert_eq!(
        target.leaf_operations[leaf_id(target, "right")]["flip"],
        BTreeSet::from(["right_flip".into()])
    );
    for (path, before, after) in [("left", false, true), ("right", false, true)] {
        let leaf = &target.specification.components()[leaf_id(target, path)];
        let mut values = activation_values(target, &["left_set", "right_flip"]);
        values.insert(leaf.state["bit"].clone(), before);
        values.insert(leaf.next_state["bit"].clone(), after);
        assert!(eval_boolean(leaf.steps.values().next().unwrap(), &values));
    }
}

#[test]
fn nested_exports_expand_and_repeated_reach_deduplicates_by_leaf_operation() {
    let mut doc = independent_actions_document();
    doc["compositions"]["Pair"]["operations"]["again"] = json!(["left.set", "left.set"]);
    doc["compositions"]["Outer"] = json!({
        "inputs": {}, "outputs": {"a": "bool", "b": "bool"},
        "instances": {"pair": {"target": "Pair", "connections": {"left": "a", "right": "b"}}},
        "operations": {"both": ["pair.left_set", "pair.again", "pair.right_flip"], "again": ["pair.again"]},
        "examples": {}
    });
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["Outer"];
    let left = leaf_id(target, "pair.left");
    assert_eq!(
        target.leaf_operations[left]["set"],
        BTreeSet::from(["both".into(), "again".into()])
    );
    assert!(target.leaf_operations[left]["clear"].is_empty());
    assert_eq!(
        target.leaf_operations[leaf_id(target, "pair.right")]["flip"],
        BTreeSet::from(["both".into()])
    );
    let leaf = &target.specification.components()[left];
    let mut values = activation_values(target, &["both", "again"]);
    values.insert(leaf.state["bit"].clone(), false);
    values.insert(leaf.next_state["bit"].clone(), true);
    assert!(eval_boolean(leaf.steps.values().next().unwrap(), &values));
}

#[test]
fn group_references_are_nonempty_direct_child_exports_with_precise_errors() {
    let cases = [
        (json!([]), "/compositions/Pair/operations/bad", "nonempty"),
        (json!(true), "/compositions/Pair/operations/bad", "array"),
        (
            json!(["absent.set"]),
            "/compositions/Pair/operations/bad/0",
            "unknown direct child",
        ),
        (
            json!(["left.absent"]),
            "/compositions/Pair/operations/bad/0",
            "unknown exported operation",
        ),
        (
            json!(["left.inner.set"]),
            "/compositions/Pair/operations/bad/0",
            "direct_child.operation",
        ),
        (
            json!(["left"]),
            "/compositions/Pair/operations/bad/0",
            "direct_child.operation",
        ),
        (
            json!([".set"]),
            "/compositions/Pair/operations/bad/0",
            "direct_child.operation",
        ),
        (
            json!([7]),
            "/compositions/Pair/operations/bad/0",
            "expected string",
        ),
    ];
    for (group, path, message) in cases {
        let mut doc = independent_actions_document();
        doc["compositions"]["Pair"]["operations"]["bad"] = group;
        let error = ScopedSpecification::from_json(&doc).unwrap_err();
        assert_eq!(error.path, path);
        assert!(error.message.contains(message), "{error}");
    }
    let mut doc = independent_actions_document();
    doc["compositions"]["Pair"]["operations"] = json!({});
    assert_eq!(
        ScopedSpecification::from_json(&doc).unwrap_err().path,
        "/compositions/Pair/operations"
    );
}

#[test]
fn traces_encode_action_sets_singletons_and_idle_with_every_control_explicit() {
    let mut doc = independent_actions_document();
    doc["compositions"]["Pair"]["examples"] = json!({"trace": {"expect": "positive", "initial": {}, "trace": [
        {"actions": [], "inputs": {}, "observe": {}},
        {"operation": "left_set", "inputs": {}, "observe": {"left": true}},
        {"actions": ["right_flip", "left_clear"], "inputs": {}, "observe": {}}
    ]}});
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["Pair"];
    let trace = &target.specification.compositions()["Pair"].examples["trace"].trace;
    for (index, selected) in [vec![], vec!["left_set"], vec!["right_flip", "left_clear"]]
        .iter()
        .enumerate()
    {
        assert_eq!(
            trace[index].operation,
            *target.specification.operations().iter().next().unwrap()
        );
        assert_eq!(trace[index].inputs.len(), target.action_inputs.len());
        for (action, input) in &target.action_inputs {
            assert_eq!(
                trace[index].inputs[input],
                crate::boolv(selected.contains(&action.as_str()))
            );
        }
    }
    assert_eq!(trace[1].observe["left"], crate::boolv(true));
    assert_eq!(scoped.document(), &doc);
}

#[test]
fn traces_reject_duplicate_unknown_and_ambiguous_action_fields() {
    let cases = [
        (
            json!({"actions": ["left_set", "left_set"]}),
            "/actions/1",
            "duplicate action",
        ),
        (
            json!({"actions": ["absent"]}),
            "/actions/0",
            "unknown action",
        ),
        (json!({"actions": [0]}), "/actions/0", "expected string"),
        (json!({"actions": "left_set"}), "/actions", "array"),
        (
            json!({"operation": "absent"}),
            "/operation",
            "unknown operation",
        ),
        (
            json!({"operation": "left_set", "actions": []}),
            "",
            "exactly one",
        ),
        (json!({}), "", "exactly one"),
    ];
    for (mut frame, suffix, message) in cases {
        frame["inputs"] = json!({});
        frame["observe"] = json!({});
        let mut doc = independent_actions_document();
        doc["compositions"]["Pair"]["examples"] =
            json!({"bad": {"expect": "positive", "initial": {}, "trace": [frame]}});
        let error = ScopedSpecification::from_json(&doc).unwrap_err();
        assert_eq!(
            error.path,
            format!("/compositions/Pair/examples/bad/trace/0{suffix}")
        );
        assert!(error.message.contains(message), "{error}");
    }
}

#[test]
fn simultaneous_distinct_ops_on_same_leaf_remain_unsatisfiable_relations() {
    let mut doc = independent_actions_document();
    // Both local relations are deliberately satisfiable together. Exclusivity
    // itself must rule out simultaneously activating distinct local operations.
    doc["specs"]["Bit"]["operations"] = json!({"set": true, "clear": true});
    doc["compositions"]["Pair"]["operations"]["collision"] = json!(["left.set", "left.clear"]);
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["Pair"];
    let leaf = &target.specification.components()[leaf_id(target, "left")];
    for actions in [vec!["left_set", "left_clear"], vec!["collision"]] {
        for before in [false, true] {
            for after in [false, true] {
                let mut values = activation_values(target, &actions);
                values.insert(leaf.state["bit"].clone(), before);
                values.insert(leaf.next_state["bit"].clone(), after);
                assert!(!eval_boolean(leaf.steps.values().next().unwrap(), &values));
            }
        }
    }
}

#[test]
fn inactive_leaves_hold_private_state_without_implicitly_holding_outputs() {
    let mut doc = independent_actions_document();
    doc["specs"]["Bit"]["invariant"] = json!(true);
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["Pair"];
    let left = &target.specification.components()[leaf_id(target, "left")];
    for selected in [vec![], vec!["right_flip"]] {
        for before in [false, true] {
            for after in [false, true] {
                let mut values = activation_values(target, &selected);
                values.insert(left.state["bit"].clone(), before);
                values.insert(left.next_state["bit"].clone(), after);
                // No public output assignment is needed to evaluate this relation.
                assert_eq!(
                    eval_boolean(left.steps.values().next().unwrap(), &values),
                    before == after
                );
            }
        }
    }
    assert_eq!(left.invariant, crate::boolv(true));
    let right = &target.specification.components()[leaf_id(target, "right")];
    assert_eq!(
        right.invariant.0.args[0],
        target.specification.observations()["right"]
    );
}

#[test]
fn generated_controls_and_tick_avoid_user_and_implementation_names() {
    let mut doc = document();
    doc["compositions"]["Machine"]["inputs"]["__scoped_action_0"] = json!("bool");
    doc["compositions"]["Machine"]["outputs"]["__scoped_tick"] = json!("bool");
    doc["implementation"] = implementation();
    doc["implementation"]["inputs"]["__scoped_action_0"] = json!("bool");
    doc["implementation"]["inputs"]["__scoped_action_0_"] = json!("bool");
    doc["implementation"]["binding"]["outputs"]["__scoped_tick"] = json!(false);
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["Machine"];
    assert_eq!(target.action_inputs["advance"], "__scoped_action_0__");
    assert_eq!(
        target.specification.operations(),
        &BTreeSet::from(["__scoped_tick_".into()])
    );
    assert!(target
        .implementation_inputs
        .contains_key("__scoped_action_0_"));
    assert!(!target
        .implementation_inputs
        .contains_key("__scoped_action_0__"));
    assert!(target
        .specification
        .inputs()
        .contains_key("__scoped_action_0"));
    assert!(!target
        .specification
        .inputs()
        .contains_key("__scoped_action_0_"));
}

#[test]
fn witness_validation_keeps_real_term_identities_and_rejects_selector_map_errors() {
    let mut doc = document();
    doc["implementation"] = implementation();
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["Machine"];
    assert_eq!(
        target.specification.inputs()["delta"],
        target.implementation_inputs["delta"]
    );
    let witness = target.implementation.as_ref().unwrap();
    assert_eq!(
        witness.states[leaf_id(target, "counter")]["x"],
        witness.machine.state["count"]
    );
    assert_eq!(
        witness.observations["result"],
        witness.machine.state["count"]
    );
    for selectors in [
        json!({}),
        json!({"advance": true, "extra": false}),
        json!({"advance": ["bv", 8, 0]}),
    ] {
        doc["implementation"]["operations"] = selectors;
        assert!(ScopedSpecification::from_json(&doc)
            .unwrap_err()
            .path
            .starts_with("/implementation/operations"));
    }
}

#[test]
fn shorthand_export_still_synchronizes_every_child_with_same_operation() {
    let mut doc = document();
    doc["compositions"]["Machine"]["instances"]["other"] =
        doc["compositions"]["Machine"]["instances"]["counter"].clone();
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["Machine"];
    assert_eq!(
        target.action_inputs.keys().cloned().collect::<Vec<_>>(),
        ["advance"]
    );
    for operations in target.leaf_operations.values() {
        assert_eq!(operations["advance"], BTreeSet::from(["advance".into()]));
    }
}

#[test]
fn unexported_bad_relations_are_validated_at_the_original_nested_path() {
    let mut doc = independent_actions_document();
    doc["compositions"]["Pair"]["operations"] = json!({"only_right": ["right.flip"]});
    doc["specs"]["Bit"]["operations"]["set"] = json!(["eq", "n.absent", true]);
    assert_eq!(
        ScopedSpecification::from_json(&doc).unwrap_err().path,
        "/specs/Bit/operations/set/1"
    );
    doc["specs"]["Bit"]["operations"]["set"] = json!(["bv", 8, 1]);
    assert_eq!(
        ScopedSpecification::from_json(&doc).unwrap_err().path,
        "/specs/Bit/operations/set"
    );
}

#[test]
fn synthetic_activation_inputs_cannot_be_supplied_by_source_examples_or_relations() {
    let mut doc = independent_actions_document();
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let synthetic = scoped.targets()["Pair"].action_inputs["left_set"].clone();
    doc["compositions"]["Pair"]["examples"] = json!({"bad": {"expect": "positive", "initial": {},
        "trace": [{"actions": [], "inputs": {&synthetic: true}, "observe": {}}]}});
    assert_eq!(
        ScopedSpecification::from_json(&doc).unwrap_err().path,
        format!("/compositions/Pair/examples/bad/trace/0/inputs/{synthetic}")
    );
    doc["compositions"]["Pair"]["examples"] = json!({});
    doc["specs"]["Bit"]["operations"]["set"] = json!(format!("i.{synthetic}"));
    assert_eq!(
        ScopedSpecification::from_json(&doc).unwrap_err().path,
        "/specs/Bit/operations/set"
    );
}

#[test]
fn overlapping_implementation_selectors_are_not_rejected_or_assumed_exclusive() {
    let mut doc = document();
    doc["specs"]["Counter"]["operations"]["other"] = json!(true);
    doc["implementation"] = implementation();
    doc["implementation"]["operations"] = json!({"advance": true, "other": true});
    let scoped = ScopedSpecification::from_json(&doc).unwrap();
    let target = &scoped.targets()["Machine"];
    assert_eq!(
        target.implementation.as_ref().unwrap().operations["advance"],
        crate::boolv(true)
    );
    assert_eq!(
        target.implementation.as_ref().unwrap().operations["other"],
        crate::boolv(true)
    );
    assert_eq!(
        target.leaf_operations[leaf_id(target, "counter")]["other"],
        BTreeSet::from(["other".into()])
    );
    assert!(target.specification.implementation().is_none());
}
