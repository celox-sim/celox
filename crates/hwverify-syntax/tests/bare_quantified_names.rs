use hwverify_syntax::parse_document;
use serde_json::json;

fn source(body: &str) -> String {
    format!(
        r#"specification "ordinary quantified names"
spec Counter(input amount: bv<4>, output count: bv<4>) {{
  state value: bv<4>;
  init value == 0u4;
  invariant count == value;
  operation add {{ expect value' == value + amount; }}
  example property {{ {body} }}
}}"#
    )
}

#[test]
fn bare_names_preserve_canonical_json_in_every_example_expression_scope() {
    let bare = source(
        r#"
      forall a: bv<4>;
      exists b: bv<4>;
      execution forall;
      initial { count = a; }
      trace {
        add(amount: a) => count == a;
        add { inputs { amount = b; } observe { count = a + b; } ensure count == a + b; }
        actions(add) { inputs { amount = a + b; } ensure count == a + b; }
        actions() { ensure count == a; }
      }
    "#,
    );
    let qualified = bare
        .replace("count = a", "count = q.a")
        .replace("amount: a", "amount: q.a")
        .replace("amount = a", "amount = q.a")
        .replace("amount = b", "amount = q.b")
        .replace("count == a", "count == q.a")
        .replace(" + b", " + q.b");
    let parsed = parse_document(&bare, "bare.hwv").unwrap();
    let old = parse_document(&qualified, "qualified.hwv").unwrap();
    parsed.validate_scoped_specification().unwrap();
    old.validate_scoped_specification().unwrap();
    assert_eq!(parsed.canonical, old.canonical);
    assert_eq!(
        parsed.canonical["specs"]["Counter"]["examples"]["property"]["trace"][0]["inputs"]
            ["amount"],
        "q.a"
    );
    let span = parsed
        .span_for("/specs/Counter/examples/property/trace/1/ensure/2/2")
        .unwrap();
    assert_eq!(&bare[span.start..span.end], "b");
}

#[test]
fn output_collision_is_explicit_without_changing_qualified_compatibility() {
    let bare = source("forall count: bv<4>; execution forall; initial {} trace { add(amount: count) => count == 0u4; }");
    let error = parse_document(&bare, "ambiguous.hwv").unwrap_err();
    assert!(error.message.contains("ambiguous name count"));
    assert!(error.message.contains("rename the quantified variable"));
    assert!(error.message.contains("o.count"));
    let span = error.span.unwrap();
    assert_eq!(&bare[span.start..span.end], "count");
    assert!(bare[..span.start].ends_with("=> "));

    let explicit = source("forall count: bv<4>; execution forall; initial { count = count; } trace { add(amount: count) => o.count == q.count; }");
    let parsed = parse_document(&explicit, "explicit.hwv").unwrap();
    parsed.validate_scoped_specification().unwrap();
    let example = &parsed.canonical["specs"]["Counter"]["examples"]["property"];
    assert_eq!(example["initial"]["count"], "q.count");
    assert_eq!(example["trace"][0]["inputs"]["amount"], "q.count");
    assert_eq!(
        example["trace"][0]["ensure"],
        json!(["eq", "o.count", "q.count"])
    );
}

#[test]
fn input_and_private_state_names_do_not_capture_bound_aliases() {
    let text = source("forall amount: bv<4>; exists value: bv<4>; execution exists; initial { count = value; } trace { add(amount: amount) => count == value + amount; }");
    let parsed = parse_document(&text, "local.hwv").unwrap();
    parsed.validate_scoped_specification().unwrap();
    let spec = &parsed.canonical["specs"]["Counter"];
    assert_eq!(spec["init"], json!(["eq", "s.value", ["bv", 4, 0]]));
    assert_eq!(
        spec["operations"]["add"],
        json!(["eq", "n.value", ["add", "s.value", "amount"]])
    );
    assert_eq!(
        spec["examples"]["property"]["trace"][0]["ensure"],
        json!(["eq", "count", ["add", "q.value", "q.amount"]])
    );
}

#[test]
fn v3_output_prefix_keeps_bound_output_names_unambiguous() {
    let text = r#"specification "v3 aliases"
      input amount: bv<4>; observation count: bv<4>; operation add {}
      component Counter {
        state value: bv<4>; init s.value == 0u4; invariant o.count == s.value;
        steps { add = n.value == s.value + i.amount; }
        example property {
          forall count: bv<4>; execution forall; initial { count = 0u4; }
          trace { add(amount: count) => o.count == count; }
        }
      }"#;
    let parsed = parse_document(text, "legacy.hwv").unwrap();
    parsed.validate_specification().unwrap();
    assert_eq!(
        parsed.canonical["components"]["Counter"]["examples"]["property"]["trace"][0]["ensure"],
        json!(["eq", "o.count", "q.count"])
    );
}

#[test]
fn bindings_are_example_local_and_unbound_names_still_fail() {
    for body in [
        "forall a: bv<4>; execution forall; initial {} trace { add(amount: missing) => true; }",
        "forall a: bv<4>; execution forall; initial {} trace { add(amount: a) => count == missing; }",
        "execution forall; initial {} trace { add(amount: a) => true; }",
        "forall a: bv<4>; execution forall; initial {} trace { add(amount: q.missing) => true; }",
        "forall a: bv<4>; execution forall; initial {} trace { add(amount: a) => count == value; }",
    ] {
        let parsed = parse_document(&source(body), "unbound.hwv").unwrap();
        assert!(parsed.validate_scoped_specification().is_err(), "{body}");
    }
    let text = source(
        "forall a: bv<4>; execution forall; initial {} trace { add(amount: a) => true; }",
    )
    .replace("example property", "example first")
    .replace(
        "\n}",
        "\n  example second { execution forall; initial {} trace { add(amount: a) => true; } }\n}",
    );
    let error = parse_document(&text, "separate.hwv")
        .unwrap()
        .validate_scoped_specification()
        .unwrap_err();
    assert!(error
        .message
        .contains("/examples/second/trace/0/inputs/amount"));
}

#[test]
fn quantified_values_cannot_be_primed() {
    for expression in ["a'", "a''", "q.a'", "a’"] {
        let text = source(&format!("forall a: bv<4>; execution forall; initial {{}} trace {{ add(amount: {expression}) => true; }}"));
        assert!(parse_document(&text, "prime.hwv").is_err(), "{expression}");
    }
    let text = source(
        "forall a: bv<4>; execution forall; initial {} trace { add(amount: a) => a' == a; }",
    );
    let error = parse_document(&text, "prime.hwv").unwrap_err();
    assert!(error
        .message
        .contains("quantified variables have no next-state value"));
    let span = error.span.unwrap();
    assert_eq!(&text[span.start..span.end], "a'");
}

#[test]
fn operators_and_literal_metadata_are_not_resolved_as_binders() {
    let text = source(
        r#"
      forall { bv: bv<4>; add: bv<4>; read: bv<4>; m: mem<2,4>; index: bv<2>; flag: bool; }
      execution exists; initial {}
      trace { add(amount: if flag { add(bv, read(m, index)) } else { bv(4, 0) }) => count == add + read; }
    "#,
    );
    let parsed = parse_document(&text, "operators.hwv").unwrap();
    parsed.validate_scoped_specification().unwrap();
    let frame = &parsed.canonical["specs"]["Counter"]["examples"]["property"]["trace"][0];
    assert_eq!(frame["operation"], "add");
    assert_eq!(
        frame["inputs"]["amount"],
        json!([
            "ite",
            "q.flag",
            ["add", "q.bv", ["read", "q.m", "q.index"]],
            ["bv", 4, 0]
        ])
    );
    assert_eq!(
        frame["ensure"],
        json!(["eq", "count", ["add", "q.add", "q.read"]])
    );
}

#[test]
fn composition_examples_resolve_against_their_own_interface() {
    let text = r#"specification "composition aliases"
      spec Unit(input amount: bv<4>, output count: bv<4>) {
        init true; invariant true; operation add { expect count' == amount; }
      }
      composition Wrapped(input value: bv<4>, output result: bv<4>) {
        use unit: Unit(amount: value, count: result);
        example checked {
          forall count: bv<4>; execution forall; initial {}
          trace { add(value: count) => result == count; }
        }
      }"#;
    let parsed = parse_document(text, "composition.hwv").unwrap();
    parsed.validate_scoped_specification().unwrap();
    assert_eq!(
        parsed.canonical["compositions"]["Wrapped"]["examples"]["checked"]["trace"][0]["ensure"],
        json!(["eq", "result", "q.count"])
    );
}
