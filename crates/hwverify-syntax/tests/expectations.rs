use hwverify_syntax::{parse_document, ParsedDocument, SyntaxError};
use serde_json::json;

fn source(body: &str) -> String {
    format!(
        r#"specification "expectations"
spec Counter(input amount: bv<4>, output count: bv<4>) {{
  state value: bv<4>;
  init value == 0u4;
  invariant count == value;
  {body}
}}"#
    )
}

fn valid(body: &str) -> ParsedDocument {
    let parsed = parse_document(&source(body), "expectations.hwv").unwrap();
    parsed.validate_scoped_specification().unwrap();
    parsed
}

fn failure(text: &str) -> SyntaxError {
    match parse_document(text, "bad.hwv") {
        Err(error) => error,
        Ok(parsed) => parsed.validate_scoped_specification().unwrap_err(),
    }
}

#[test]
fn local_state_and_output_primes_lower_without_extra_schema() {
    let p = valid("operation add { expect value' == value + amount; expect count' == value'; }");
    assert_eq!(p.canonical["version"], 4);
    let spec = &p.canonical["specs"]["Counter"];
    assert_eq!(spec["init"], json!(["eq", "s.value", ["bv", 4, 0]]));
    assert_eq!(spec["invariant"], json!(["eq", "count", "s.value"]));
    assert_eq!(
        spec["operations"]["add"],
        json!([
            "and",
            ["eq", "n.value", ["add", "s.value", "amount"]],
            ["eq", "no.count", "n.value"]
        ])
    );
    assert!(spec.get("expectations").is_none());
}

#[test]
fn singleton_blocks_and_legacy_prefixes_preserve_exact_trees() {
    let a = valid("operation add = n.value == s.value + i.amount;");
    let b = valid("operation add { expect value' == value + i.amount; }");
    assert_eq!(a.canonical, b.canonical);
    let c = valid("operation add { all { any { expect o.count == s.value; } } }");
    assert_eq!(
        c.canonical["specs"]["Counter"]["operations"]["add"],
        json!(["eq", "o.count", "s.value"])
    );
}

#[test]
fn named_forward_dependencies_and_nested_any_all_keep_boolean_structure() {
    let p = valid(
        r#"
        expectation choice {
          any {
            all { expect grows; expect count' == value'; }
            all { expect holds; expect count' == count; }
          }
        }
        expectation grows { expect value' == value + amount; }
        expectation holds { expect value' == value; }
        operation adjust { expect choice; expect amount <= 3u4; }
    "#,
    );
    let relation = &p.canonical["specs"]["Counter"]["operations"]["adjust"];
    assert_eq!(
        relation,
        &json!([
            "and",
            [
                "or",
                [
                    "and",
                    ["eq", "n.value", ["add", "s.value", "amount"]],
                    ["eq", "no.count", "n.value"]
                ],
                [
                    "and",
                    ["eq", "n.value", "s.value"],
                    ["eq", "no.count", "count"]
                ]
            ],
            ["ule", "amount", ["bv", 4, 3]]
        ])
    );
    assert!(!p.canonical.to_string().contains("choice"));
}

#[test]
fn expectations_are_boolean_relations_and_all_declarations_are_checked() {
    for (body, message) in [
        (
            "expectation unused { expect 1u4; } operation add { expect true; }",
            "Bool",
        ),
        (
            "expectation word { expect 1u4; } operation add { expect word == 1u4; }",
            "Bool",
        ),
        (
            "expectation unused { expect unknown; } operation add { expect true; }",
            "unknown",
        ),
        (
            "expectation unused { expect value + true == value; } operation add { expect true; }",
            "word",
        ),
    ] {
        let error = failure(&source(body));
        assert!(
            error
                .message
                .to_lowercase()
                .contains(&message.to_lowercase()),
            "{error}"
        );
        assert!(error.span.is_some());
    }
}

#[test]
fn unused_cycles_self_cycles_shadowing_and_duplicates_are_rejected() {
    for (body, message) in [
        ("expectation a { expect b; } expectation b { expect a; } operation add { expect true; }", "cyclic"),
        ("expectation a { expect a; } operation add { expect true; }", "cyclic"),
        ("expectation value { expect true; } operation add { expect true; }", "shadows"),
        ("expectation amount { expect true; } operation add { expect true; }", "shadows"),
        ("expectation count { expect true; } operation add { expect true; }", "shadows"),
        ("expectation a { expect true; } expectation a { expect false; } operation add { expect true; }", "duplicate"),
        ("operation add { expect true; } operation add = true;", "duplicate"),
    ] {
        let error = failure(&source(body));
        assert!(error.message.contains(message), "{error}");
        assert!(error.span.is_some());
    }
}

#[test]
fn empty_blocks_cannot_silently_allow_or_forbid_everything() {
    for body in [
        "operation add {}",
        "operation add { all {} }",
        "operation add { any {} }",
        "expectation unused {} operation add { expect true; }",
    ] {
        assert!(failure(&source(body)).message.contains("empty"));
    }
    valid("operation add { expect true; }");
    valid("operation add { expect false; }");
}

#[test]
fn prime_restrictions_and_exact_illegal_token_spans() {
    for (expression, message, token) in [
        ("amount' == amount", "input", "amount'"),
        ("value'' == value", "one prime", "value''"),
        ("s.value' == value", "unqualified", "s.value'"),
        ("absent' == value", "unknown primed", "absent'"),
    ] {
        let text = source(&format!("operation add {{ expect {expression}; }}"));
        let error = failure(&text);
        assert!(error.message.contains(message), "{error}");
        let span = error.span.unwrap();
        assert_eq!(&text[span.start..span.end], token);
    }
    for expression in [
        "value’ == value",
        "(value)' == value",
        "1u4' == value",
        "value'() == value",
    ] {
        assert!(parse_document(
            &source(&format!("operation add {{ expect {expression}; }}")),
            "bad.hwv"
        )
        .is_err());
    }
}

#[test]
fn current_only_clauses_reject_direct_and_indirect_future_and_input_references() {
    for field in ["init", "invariant"] {
        for (expression, token) in [
            ("value' == value", "value'"),
            ("count' == count", "count'"),
            ("n.value == value", "n.value"),
            ("no.count == count", "no.count"),
            ("amount == value", "amount"),
            ("i.amount == value", "i.amount"),
            ("alias", "alias"),
        ] {
            let old = if field == "init" {
                "init value == 0u4;"
            } else {
                "invariant count == value;"
            };
            let text = source("expectation later { expect value' == value; } expectation alias { expect later; } operation add { expect true; }")
                .replace(old, &format!("{field} {expression};"));
            let error = failure(&text);
            assert!(error.message.contains("init/invariant"), "{error}");
            let span = error.span.unwrap();
            assert_eq!(&text[span.start..span.end], token);
        }
    }
    let text =
        source("expectation current { expect count == value; } operation add { expect true; }")
            .replace("invariant count == value;", "invariant current;");
    parse_document(&text, "current.hwv")
        .unwrap()
        .validate_scoped_specification()
        .unwrap();
}

#[test]
fn state_port_collision_requires_explicit_references_only_where_ambiguous() {
    let explicit = r#"specification "shared"
      spec S(input x: bool, output y: bool) {
        state x: bool; state y: bool;
        init s.x; invariant o.y == s.y;
        operation a { expect n.x == i.x; expect n.y == no.y; }
      }"#;
    parse_document(explicit, "shared.hwv")
        .unwrap()
        .validate_scoped_specification()
        .unwrap();
    for (old, new) in [("i.x", "x"), ("n.x", "x'"), ("s.y", "y"), ("no.y", "y'")] {
        let error = failure(&explicit.replace(old, new));
        assert!(error.message.contains("ambiguous"), "{error}");
    }
}

#[test]
fn scope_is_lexical_and_repeated_instances_do_not_capture_names() {
    let text = r#"specification "lexical"
      spec A(output y: bool) {
        state x: bool; init !x; invariant y == x;
        expectation flip { expect x' == !x; }
        operation a { expect flip; }
      }
      spec B(output y: bool) {
        state x: bool; init x; invariant y == x;
        expectation flip { expect x' == x; }
        operation a { expect flip; }
      }
      composition Pair(output left: bool, output right: bool) {
        use left: A(y: left); use right: A(y: right);
      }"#;
    let p = parse_document(text, "lexical.hwv").unwrap();
    p.validate_scoped_specification().unwrap();
    assert_eq!(
        p.canonical["specs"]["A"]["operations"]["a"],
        json!(["eq", "n.x", ["not", "s.x"]])
    );
    assert_eq!(
        p.canonical["specs"]["B"]["operations"]["a"],
        json!(["eq", "n.x", "s.x"])
    );
    let bad = text.replace("expectation flip { expect x' == x; }", "");
    assert!(failure(&bad).message.contains("unknown"));
}

#[test]
fn prime_on_a_memory_selects_the_next_memory_before_indexing() {
    let text = r#"specification "memory"
      spec M(input address: bv<2>, output data: bv<4>) {
        state memory: mem<2, 4>; init true; invariant true;
        operation read { expect memory'[address] == data'; }
      }"#;
    let p = parse_document(text, "memory.hwv").unwrap();
    p.validate_scoped_specification().unwrap();
    assert_eq!(
        p.canonical["specs"]["M"]["operations"]["read"],
        json!(["eq", ["read", "n.memory", "address"], "no.data"])
    );
}

#[test]
fn malformed_nested_blocks_are_not_accepted_as_constraints() {
    for body in [
        "operation add { value' == value; }",
        "operation add { expect value = value; }",
        "operation add { all { expect true; } mystery { expect false; } }",
        "operation add { expectation nested { expect true; } }",
    ] {
        assert!(parse_document(&source(body), "bad.hwv").is_err());
    }
}

#[test]
fn expansion_is_bounded_before_exponential_cloning() {
    let mut body = "expectation e0 { expect true; }\n".to_owned();
    for index in 1..24 {
        body.push_str(&format!(
            "expectation e{index} {{ expect e{}; expect e{}; }}\n",
            index - 1,
            index - 1
        ));
    }
    body.push_str("operation add { expect e23; }");
    let error = failure(&source(&body));
    assert!(error.message.contains("limit"), "{error}");
}

#[test]
fn multi_clause_blocks_lower_to_binary_relations() {
    let p = valid("operation add { expect true; expect false; expect true; }");
    assert_eq!(
        p.canonical["specs"]["Counter"]["operations"]["add"],
        json!(["and", ["and", true, false], true])
    );
    let p = valid("operation add { any { expect true; expect false; expect true; } }");
    assert_eq!(
        p.canonical["specs"]["Counter"]["operations"]["add"],
        json!(["or", ["or", true, false], true])
    );
}

#[test]
fn dependency_depth_bound_is_independent_of_declaration_sort_order() {
    for reversed in [false, true] {
        let mut body = String::new();
        for index in 0..66 {
            let name = if reversed { 65 - index } else { index };
            if index == 0 {
                body.push_str(&format!("expectation e{name:02} {{ expect true; }}\n"));
            } else {
                let dependency = if reversed { name + 1 } else { name - 1 };
                body.push_str(&format!(
                    "expectation e{name:02} {{ expect e{dependency:02}; }}\n"
                ));
            }
        }
        body.push_str("operation add { expect true; }");
        assert!(failure(&source(&body)).message.contains("depth"));
    }
}

#[test]
fn modern_example_sources_match_canonical_fixtures() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in [
        "expectation_counter",
        "quantified_counter",
        "quantified_counter_bad_order",
        "quantified_counter_impossible",
    ] {
        let source = std::fs::read_to_string(root.join(format!("examples/{name}.hwv"))).unwrap();
        let parsed = parse_document(&source, &format!("{name}.hwv")).unwrap();
        let canonical = hwverify_syntax::parse_json(
            &std::fs::read(root.join(format!("examples/{name}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(parsed.canonical, canonical, "{name}");
        parsed.validate_scoped_specification().unwrap();
    }
}
