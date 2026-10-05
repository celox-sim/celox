use lydite_syntax::{parse_document, parse_json};
use serde_json::json;
use std::{fs, path::PathBuf};

const SOURCE: &str = r#"specification "independent views"
spec Counter(input amount: bv<2>, output count: bv<2>) {
  state x: bv<2>;
  init s.x == 0u2;
  invariant count == s.x;
  operation add = n.x == s.x + amount;
}
composition Pair(input amount: bv<2>, output left_count: bv<2>, output right_count: bv<2>) {
  use left: Counter(amount: amount, count: left_count);
  use right: Counter(amount: amount, count: right_count);
  operation left = actions(left.add);
  operation right = actions(right.add);
  example parallel {
    expect positive;
    initial { left_count = 0u2; right_count = 0u2; }
    trace {
      actions(left, right) { inputs { amount = 1u2; } observe { left_count = 1u2; right_count = 1u2; } }
      actions() { observe { left_count = 1u2; right_count = 1u2; } }
      left { inputs { amount = 1u2; } observe { left_count = 2u2; right_count = 1u2; } }
    }
  }
}
implementation {
  composition Pair;
  input rst: bool;
  input go_left: bool;
  input go_right: bool;
  input amount: bv<2>;
  reset_input rst;
  state a: bv<2>;
  state b: bv<2>;
  reset { a = 0u2; b = 0u2; }
  next { a = if i.go_left { s.a + i.amount } else { s.a }; b = if i.go_right { s.b + i.amount } else { s.b }; }
  operations { left = i.go_left; right = i.go_right; }
  binding {
    bind left { x = s.a; }
    bind right { x = s.b; }
    output left_count = s.a;
    output right_count = s.b;
  }
}"#;

#[test]
fn signatures_named_connections_and_action_sets_lower_exactly() {
    let parsed = parse_document(SOURCE, "scoped.lyd").unwrap();
    assert_eq!(parsed.canonical["version"], 4);
    assert_eq!(
        parsed.canonical["specs"]["Counter"]["inputs"],
        json!({"amount":{"bv":2}})
    );
    assert_eq!(
        parsed.canonical["compositions"]["Pair"]["instances"]["left"],
        json!({"target":"Counter","connections":{"amount":"amount","count":"left_count"}})
    );
    assert_eq!(
        parsed.canonical["compositions"]["Pair"]["operations"],
        json!({"left":["left.add"],"right":["right.add"]})
    );
    let trace = &parsed.canonical["compositions"]["Pair"]["examples"]["parallel"]["trace"];
    assert_eq!(trace[0]["actions"], json!(["left", "right"]));
    assert_eq!(trace[1]["actions"], json!([]));
    assert_eq!(trace[1]["inputs"], json!({}));
    assert_eq!(trace[2]["operation"], "left");
    parsed.validate_scoped_specification().unwrap();
}

#[test]
fn scoped_fixture_sources_preserve_their_public_v4_json() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lydite");
    for name in [
        "scoped_budgeted_counter",
        "scoped_independent_counters",
        "scoped_contradictory_outputs",
    ] {
        let path = root.join(format!("examples/{name}.lyd"));
        let parsed =
            parse_document(&fs::read_to_string(&path).unwrap(), path.to_str().unwrap()).unwrap();
        let json =
            parse_json(&fs::read(root.join(format!("examples/{name}.json"))).unwrap()).unwrap();
        assert_eq!(parsed.canonical, json, "{name}");
        parsed.validate_scoped_specification().unwrap();
    }
}

#[test]
fn scoped_duplicate_ports_instances_connections_and_actions_fail() {
    for (old, new) in [
        (
            "input amount: bv<2>, output count: bv<2>",
            "input amount: bv<2>, output amount: bv<2>",
        ),
        ("use right: Counter", "use left: Counter"),
        ("count: left_count", "count: left_count, count: right_count"),
        ("actions(left, right)", "actions(left, left)"),
        (
            "operation left = actions(left.add);",
            "operation left = actions(left.add); operation left = actions(right.add);",
        ),
        ("bind right {", "bind left {"),
        ("output right_count = s.b;", "output left_count = s.b;"),
    ] {
        let source = SOURCE.replacen(old, new, 1);
        let error = parse_document(&source, "duplicate.lyd").unwrap_err();
        assert!(error.message.contains("duplicate"), "{new}: {error}");
        assert!(error.span.is_some());
    }
}

#[test]
fn scoped_declaration_and_action_shapes_are_context_checked() {
    for (old, new) in [
        (
            "spec Counter(input amount: bv<2>, output count: bv<2>)",
            "spec Counter(observation amount: bv<2>, output count: bv<2>)",
        ),
        (
            "spec Counter(input amount: bv<2>, output count: bv<2>)",
            "component Counter(input amount: bv<2>, output count: bv<2>)",
        ),
        (
            "use left: Counter(amount: amount, count: left_count);",
            "use left: Counter(amount: i.amount, count: left_count);",
        ),
        (
            "operation left = actions(left.add);",
            "operation left = left.add;",
        ),
        (
            "operation left = actions(left.add);",
            "operation left = actions(true);",
        ),
        ("actions(left, right)", "actions(left.add, right.add)"),
        ("actions(left, right)", "actions(true)"),
        ("actions(left, right)", "parallel(left, right)"),
        (
            "invariant count == s.x;",
            "input undeclared: bool; invariant count == s.x;",
        ),
    ] {
        assert!(
            parse_document(&SOURCE.replacen(old, new, 1), "bad.lyd").is_err(),
            "{new}"
        );
    }
    for source in [
        "specification \"x\" component Old {} spec New() { init true; invariant true; operation tick = true; }",
        "specification \"x\" input global: bool; spec New() { init true; invariant true; operation tick = true; }",
        "design \"x\" spec New() { init true; invariant true; operation tick = true; }",
        "specification \"x\" use New();",
        "specification \"x\" actions() {}",
    ] {
        assert!(parse_document(source, "mixed.lyd").is_err(), "{source}");
    }
}

#[test]
fn scoped_semantic_failures_keep_source_locations() {
    for (old, new, path, token) in [
        (
            "count: left_count",
            "count: missing",
            "/compositions/Pair/instances/left/connections/count",
            "missing",
        ),
        (
            "left.add",
            "left.missing",
            "/compositions/Pair/operations/left/0",
            "left.missing",
        ),
        (
            "n.x == s.x + amount",
            "n.x == s.x + absent",
            "/specs/Counter/operations/add/2/2",
            "absent",
        ),
        (
            "output left_count = s.a;",
            "output left_count = i.amount;",
            "/implementation/binding/outputs/left_count",
            "i.amount",
        ),
    ] {
        let source = SOURCE.replacen(old, new, 1);
        let parsed = parse_document(&source, "location.lyd").unwrap();
        let error = parsed.validate_scoped_specification().unwrap_err();
        assert!(error.message.contains(path), "{new}: {error}");
        let span = error.span.unwrap();
        assert_eq!(
            &source[span.start..span.end],
            token,
            "{new}: {}",
            error.message
        );
    }
}

#[test]
fn signature_and_instance_words_remain_identifiers() {
    for name in [
        "spec",
        "use",
        "input",
        "output",
        "operation",
        "expectation",
        "expect",
        "all",
        "any",
        "actions",
        "composition",
        "bv",
        "bool",
    ] {
        let source = format!(
            r#"specification "names"
          spec {name}(input {name}: bool, output visible: bool) {{
            state {name}: bool; init s.{name}; invariant visible == s.{name};
            operation {name} = n.{name} == i.{name};
          }}
          composition Top(input {name}: bool, output visible: bool) {{
            use {name}({name}: {name}, visible: visible);
            operation run = actions({name}.{name});
            example x {{ expect positive; initial {{}} trace {{ actions(run) {{}} }} }}
          }}"#
        );
        parse_document(&source, "names.lyd")
            .unwrap()
            .validate_scoped_specification()
            .unwrap();
    }
}

#[test]
fn edited_scoped_canonical_output_is_revalidated() {
    let mut parsed = parse_document(SOURCE, "mutable.lyd").unwrap();
    parsed.canonical["specs"]["Counter"]["operations"]["add"] = json!(["add", true, true]);
    assert!(parsed.validate_scoped_specification().is_err());
}
