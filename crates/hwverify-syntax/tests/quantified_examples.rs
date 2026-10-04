use hwverify_syntax::parse_document;
use serde_json::json;

fn source(body: &str) -> String {
    format!(
        r#"specification "bounded properties"
spec Counter(input amount: bv<4>, output count: bv<4>) {{
  state value: bv<4>; init value == 0u4; invariant count == value;
  operation add {{ expect value' == value + amount; }}
  example property {{ {body} }}
}}"#
    )
}

#[test]
fn ordered_input_binders_and_final_execution_mode_lower_exactly() {
    let text = source(
        r#"
      forall a: bv<4>;
      exists { b: bv<4>; c: bool; }
      forall d: bv<4>;
      execution forall;
      initial { count = 0u4; }
      trace {
        add(amount: q.a) => count == q.a;
        add(amount: q.b) => count == q.a + q.b;
      }
    "#,
    );
    let p = parse_document(&text, "quantifiers.hwv").unwrap();
    p.validate_scoped_specification().unwrap();
    let example = &p.canonical["specs"]["Counter"]["examples"]["property"];
    assert_eq!(
        example["quantifiers"],
        json!([
          {"kind":"forall", "variables":{"a":{"bv":4}}},
          {"kind":"exists", "variables":{"b":{"bv":4},"c":"bool"}},
          {"kind":"forall", "variables":{"d":{"bv":4}}}
        ])
    );
    assert_eq!(example["expect"], "forall");
    assert_eq!(
        example["trace"],
        json!([
          {"operation":"add","inputs":{"amount":"q.a"},"observe":{},"ensure":["eq","count","q.a"]},
          {"operation":"add","inputs":{"amount":"q.b"},"observe":{},"ensure":["eq","count",["add","q.a","q.b"]]}
        ])
    );
    let span = p
        .span_for("/specs/Counter/examples/property/trace/1/ensure/2/2")
        .unwrap();
    assert_eq!(&text[span.start..span.end], "q.b");
    let span = p
        .span_for("/specs/Counter/examples/property/quantifiers/1/variables/c")
        .unwrap();
    assert_eq!(&text[span.start..span.end], "c");
}

#[test]
fn legacy_block_ensure_and_compact_invocations_lower_identically() {
    for mode in [
        "expect positive",
        "expect negative",
        "execution exists",
        "execution not_exists",
        "execution forall",
    ] {
        let prefix = format!("{mode}; initial {{}} trace {{");
        let compact = source(&format!("{prefix} add(amount: 2u4) => count == 2u4; }}"));
        let blocks = source(&format!(
            "{prefix} add {{ inputs {{ amount = 2u4; }} ensure count == 2u4; }} }}"
        ));
        let a = parse_document(&compact, "compact.hwv").unwrap();
        let b = parse_document(&blocks, "blocks.hwv").unwrap();
        a.validate_scoped_specification().unwrap();
        b.validate_scoped_specification().unwrap();
        assert_eq!(a.canonical, b.canonical);
    }
}

#[test]
fn empty_invocation_and_explicit_empty_quantifier_prefix_are_preserved() {
    let text = source("quantifiers {} execution exists; initial {} trace { add() => true; }");
    let p = parse_document(&text, "empty.hwv").unwrap();
    p.validate_scoped_specification().unwrap();
    let example = &p.canonical["specs"]["Counter"]["examples"]["property"];
    assert_eq!(example["quantifiers"], json!([]));
    assert_eq!(
        example["trace"][0],
        json!({"operation":"add","inputs":{},"observe":{},"ensure":true})
    );
}

#[test]
fn malformed_quantifiers_modes_and_duplicate_invocation_arguments_fail() {
    for body in [
        "forall a: bool; forall a: bool; execution forall; initial {} trace {}",
        "forall { a: bool; a: bool; } execution forall; initial {} trace {}",
        "forall q.a: bool; execution forall; initial {} trace {}",
        "forall {} execution forall; initial {} trace {}",
        "forall { expect true; } execution forall; initial {} trace {}",
        "forall a = true; execution forall; initial {} trace {}",
        "execution forall; forall a: bool; initial {} trace {}",
        "execution positive; initial {} trace {}",
        "execution negative; initial {} trace {}",
        "execution true; initial {} trace {}",
        "execution exists; expect positive; initial {} trace {}",
        "expect negative; execution forall; initial {} trace {}",
        "execution exists; execution forall; initial {} trace {}",
        "quantifiers {} quantifiers {} execution exists; initial {} trace {}",
        "execution exists; initial {} trace { add(amount: 1u4, amount: 2u4) => true; }",
        "execution exists; initial {} trace { add(i.amount: 1u4) => true; }",
        "execution exists; initial {} trace { pair.add(amount: 1u4) => true; }",
        "execution exists; initial {} trace { add { ensure true; ensure false; } }",
        "execution exists; initial {} add(amount: 1u4) => true; trace {}",
    ] {
        let error = parse_document(&source(body), "bad.hwv").unwrap_err();
        assert!(error.span.is_some(), "{body}: {error}");
    }
}

#[test]
fn v3_compatibility_surface_can_express_ordered_bounded_assertions() {
    let text = r#"specification "legacy scoped inputs"
      input amount: bv<4>; observation count: bv<4>; operation add {}
      component Counter {
        state value: bv<4>; init s.value == 0u4; invariant o.count == s.value;
        steps { add = n.value == s.value + i.amount; }
        example property {
          forall a: bv<4>; execution forall; initial { count = 0u4; }
          trace { add(amount: q.a) => o.count == q.a; }
        }
      }"#;
    let p = parse_document(text, "legacy.hwv").unwrap();
    p.validate_specification().unwrap();
    assert_eq!(p.canonical["version"], 3);
    assert_eq!(
        p.canonical["components"]["Counter"]["examples"]["property"]["expect"],
        "forall"
    );
}

#[test]
fn simultaneous_and_idle_actions_keep_block_ensure_predicates() {
    let text = r#"specification "parallel"
      spec Counter(output count: bv<4>) {
        state value: bv<4>; init value == 0u4; invariant count == value;
        operation add { expect value' == value + 1u4; }
      }
      composition Pair(output left: bv<4>, output right: bv<4>) {
        use a: Counter(count: left); use b: Counter(count: right);
        operation a = actions(a.add); operation b = actions(b.add);
        example property {
          execution forall; initial { left = 0u4; right = 0u4; }
          trace {
            actions(a, b) { ensure left == 1u4 && right == 1u4; }
            actions() { ensure left == 1u4 && right == 1u4; }
          }
        }
      }"#;
    let p = parse_document(text, "parallel.hwv").unwrap();
    p.validate_scoped_specification().unwrap();
    let trace = &p.canonical["compositions"]["Pair"]["examples"]["property"]["trace"];
    assert_eq!(trace[0]["actions"], json!(["a", "b"]));
    assert_eq!(trace[1]["actions"], json!([]));
    assert_eq!(
        trace[0]["ensure"],
        json!([
            "and",
            ["eq", "left", ["bv", 4, 1]],
            ["eq", "right", ["bv", 4, 1]]
        ])
    );
}
