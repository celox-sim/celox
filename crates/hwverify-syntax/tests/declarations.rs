use hwverify_syntax::{parse_document, ParsedDocument};
use serde_json::json;

const MODERN: &str = r#"specification "named views"
input rst: bool;
input amount: bv<4>;
observation count: bv<4>;
operation add {}
component Counter {
  state value: bv<4>;
  init s.value == 0u4;
  invariant o.count == s.value;
  steps { add = n.value == s.value + i.amount; }
  example twice {
    expect positive;
    initial { count = 0u4; }
    trace {
      add { inputs { amount = 1u4; } observe { count = 1u4; } }
      add { inputs { amount = 1u4; } observe { count = 2u4; } }
    }
  }
}
composition Wrapped { members compose(Counter); }
implementation {
  composition Wrapped;
  reset_input rst;
  state count: bv<4>;
  reset { count = 0u4; }
  next { count = s.count + i.amount; }
  operations { add = true; }
  binding {
    bind Counter { value = s.count; }
    observation count = s.count;
  }
}
"#;

const LEGACY: &str = r#"specification "named views" {
  inputs { rst: bool; amount: bv<4>; }
  observations { count: bv<4>; }
  operations { add {} }
  components { Counter {
    state { value: bv<4>; }
    init s.value == 0u4;
    invariant o.count == s.value;
    steps { add = n.value == s.value + i.amount; }
    examples { twice {
      expect positive;
      initial { count = 0u4; }
      trace {
        add { inputs { amount = 1u4; } observe { count = 1u4; } }
        add { inputs { amount = 1u4; } observe { count = 2u4; } }
      }
    } }
  } }
  compositions { Wrapped { members compose(Counter); examples {} } }
  implementation {
    composition Wrapped;
    reset_input rst;
    state { count: bv<4>; }
    reset { count = 0u4; }
    next { count = s.count + i.amount; }
    operations { add = true; }
    binding {
      states { Counter { value = s.count; } }
      observations { count = s.count; }
    }
  }
}"#;

fn parse(source: &str) -> ParsedDocument {
    parse_document(source, "declarations.hwv").unwrap()
}

#[test]
fn new_and_legacy_surfaces_have_identical_validated_json() {
    let old = parse(LEGACY);
    old.validate_specification().unwrap();
    let new = parse(MODERN);
    new.validate_specification().unwrap();
    assert_eq!(old.canonical, new.canonical);
    let braced = MODERN.replacen("\n", " {\n", 1) + "}";
    let unbraced = LEGACY.replacen(" {", "", 1);
    let unbraced = unbraced.strip_suffix('}').unwrap();
    for source in [&braced, unbraced] {
        let parsed = parse(source);
        parsed.validate_specification().unwrap();
        assert_eq!(old.canonical, parsed.canonical);
    }
}

#[test]
fn mixed_declarations_merge_without_reordering_trace_steps() {
    let source = MODERN
        .replace("input rst: bool;", "inputs { rst: bool; }")
        .replace("state value: bv<4>;", "state { value: bv<4>; }")
        .replace("operation add {}", "operations { add {} }")
        .replace(
            "bind Counter { value = s.count; }",
            "states { Counter { value = s.count; } }",
        )
        .replace(
            "observation count = s.count;",
            "observations { count = s.count; }",
        );
    assert_eq!(parse(&source).canonical, parse(MODERN).canonical);
    let trace = &parse(&source).canonical["components"]["Counter"]["examples"]["twice"]["trace"];
    assert_eq!(trace.as_array().unwrap().len(), 2);
    assert_eq!(trace[0]["observe"]["count"], json!(["bv", 4, 1]));
    assert_eq!(trace[1]["observe"]["count"], json!(["bv", 4, 2]));

    for source in [
        "specification \"x\" input x: bool; inputs { y: bool; } input z: bool;",
        "specification \"x\" inputs { y: bool; } input x: bool; input z: bool;",
    ] {
        assert_eq!(
            parse(source).canonical["inputs"],
            json!({"x":"bool", "y":"bool", "z":"bool"})
        );
    }
    for source in [
        "specification \"x\" component A {} components { B {} } component C {}",
        "specification \"x\" components { B {} } component A {} component C {}",
    ] {
        assert_eq!(
            parse(source).canonical["components"]
                .as_object()
                .unwrap()
                .len(),
            3
        );
    }
}

#[test]
fn duplicate_declarations_and_legacy_aliases_never_overwrite() {
    for (prefix, modern, legacy) in [
        (
            "specification \"x\"",
            "input x: bool;",
            "inputs { x: bool; }",
        ),
        (
            "specification \"x\"",
            "observation x: bool;",
            "observations { x: bool; }",
        ),
        (
            "specification \"x\"",
            "operation x {}",
            "operations { x {} }",
        ),
        (
            "specification \"x\"",
            "component x {}",
            "components { x {} }",
        ),
        (
            "specification \"x\"",
            "composition x {}",
            "compositions { x {} }",
        ),
        (
            "specification \"x\" component C {",
            "state x: bool;",
            "state { x: bool; }",
        ),
        (
            "specification \"x\" component C {",
            "example x {}",
            "examples { x {} }",
        ),
        (
            "specification \"x\" implementation { binding {",
            "bind x {}",
            "states { x {} }",
        ),
        (
            "specification \"x\" implementation { binding {",
            "observation x = true;",
            "observations { x = true; }",
        ),
        (
            "design \"x\" contract {",
            "parameter x: bool;",
            "parameters { x: bool; }",
        ),
    ] {
        let suffix = "}".repeat(prefix.matches('{').count());
        for (first, second) in [
            (modern, modern),
            (modern, legacy),
            (legacy, modern),
            (legacy, legacy),
        ] {
            let source = format!("{prefix}\n{first}\n{second}\n{suffix}");
            let error = parse_document(&source, "duplicate.hwv").unwrap_err();
            assert!(error.message.contains("duplicate"), "{source}: {error}");
            assert_eq!(error.span.unwrap().line, 3, "{source}");
        }
    }
    for source in [
        "specification \"x\" components {} components {}",
        "specification \"x\" inputs {} input x: bool; inputs {}",
        "specification \"x\" component C { state {} state {} }",
        "design \"x\" contract { pre true; precondition true; }",
    ] {
        assert!(parse_document(source, "duplicate.hwv")
            .unwrap_err()
            .message
            .contains("duplicate"));
    }
}

#[test]
fn declarations_are_context_checked() {
    for source in [
        "design \"x\" component C {}",
        "design \"x\" observation x: bool;",
        "design \"x\" state x: bool;",
        "specification \"x\" example x {}",
        "specification \"x\" input x = true;",
        "specification \"x\" observation x = true;",
        "specification \"x\" operation x: bool;",
        "specification \"x\" component C { input x: bool; }",
        "specification \"x\" component C { component D {} }",
        "specification \"x\" components { component C {} }",
        "specification \"x\" implementation { bind C {} }",
        "specification \"x\" implementation { binding { state x: bool; } }",
        "specification \"x\" implementation { binding { observation x: bool; } }",
        "specification \"x\" component C { example x { trace { operation add {} } } }",
        "specification \"x\" mystery C {}",
        "specification \"x\" input dotted.name: bool;",
        "specification \"x\" component dotted.name {}",
    ] {
        let error = parse_document(source, "scope.hwv").unwrap_err();
        assert!(error.span.is_some(), "{source}: {error}");
    }
}

#[test]
fn malformed_headers_and_declarations_fail_closed() {
    for source in [
        "specification \"x\"; operation add {}",
        "specification \"x\" { operation add {}",
        "specification \"x\" operation add {} }",
        "specification \"x\" {} operation add {}",
        "specification \"x\" component C {",
        "specification \"x\" input x: bool",
        "specification \"x\" input : bool;",
        "specification \"x\" component C",
        "specification \"x\" operation add {};",
        "specification \"x\" input x: bool; garbage",
        "specification \"x\" design \"second\"",
    ] {
        assert!(parse_document(source, "bad.hwv").is_err(), "{source}");
    }
}

#[test]
fn contextual_words_remain_usable_as_identifiers() {
    for name in [
        "input",
        "state",
        "observation",
        "operation",
        "component",
        "composition",
        "example",
        "parameter",
        "bind",
        "design",
        "specification",
        "bv",
        "mem",
        "bool",
    ] {
        let source = format!(
            r#"specification "identifiers"
            input {name}: bool;
            observation {name}: bool;
            operation {name} {{}}
            component {name} {{
              state {name}: bool;
              init s.{name} == true;
              invariant o.{name} == s.{name};
              steps {{ {name} = n.{name} == s.{name}; }}
              example {name} {{ expect positive; initial {{}} trace {{ {name} {{}} }} }}
            }}"#
        );
        let parsed = parse(&source);
        parsed.validate_specification().unwrap();
        assert_eq!(parsed.canonical["inputs"][name], "bool");
        assert_eq!(parsed.canonical["components"][name]["state"][name], "bool");
    }
}

#[test]
fn empty_declaration_collections_are_explicit_json_not_semantic_defaults() {
    let source = "specification \"empty maps\" operation tick {} component C { init true; invariant true; steps { tick = true; } }";
    let parsed = parse(source);
    parsed.validate_specification().unwrap();
    for path in [
        "/inputs",
        "/observations",
        "/compositions",
        "/components/C/state",
        "/components/C/examples",
    ] {
        assert_eq!(parsed.canonical.pointer(path), Some(&json!({})), "{path}");
    }
    for source in [
        "specification \"empty\"",
        "specification \"empty\" operation tick {}",
        "specification \"x\" operation tick {} component C { init true; invariant true; }",
        "specification \"x\" operation tick {} component C { init true; invariant true; steps { tick = true; } example e { expect positive; trace {} } }",
    ] {
        assert!(parse(source).validate_specification().is_err(), "{source}");
    }
    let binding = parse("specification \"x\" implementation { binding {} }");
    assert_eq!(
        binding.canonical["implementation"]["binding"],
        json!({"states":{}, "observations":{}})
    );
}

#[test]
fn canonical_paths_retain_precise_source_spans() {
    let parsed = parse(MODERN);
    for (path, expected) in [
        ("/inputs/amount", "amount"),
        ("/observations/count", "count"),
        ("/components/Counter", "Counter"),
        ("/components/Counter/state/value", "value"),
        ("/components/Counter/examples/twice", "twice"),
        (
            "/components/Counter/examples/twice/trace/1/observe/count",
            "2u4",
        ),
        ("/implementation/binding/states/Counter/value", "s.count"),
        ("/implementation/binding/observations/count", "s.count"),
    ] {
        let span = parsed.span_for(path).unwrap();
        assert_eq!(&MODERN[span.start..span.end], expected, "{path}");
    }
    let source = MODERN.replace(
        "observation count = s.count;",
        "/* 日本語 */ observation count = s.missing + 1u4;",
    );
    let error = parse(&source).validate_specification().unwrap_err();
    assert!(error
        .message
        .contains("/implementation/binding/observations/count/1"));
    let span = error.span.unwrap();
    assert_eq!(&source[span.start..span.end], "s.missing");
    assert_eq!(
        span.column,
        source[..span.start]
            .rsplit('\n')
            .next()
            .unwrap()
            .chars()
            .count()
            + 1
    );
}

#[test]
fn design_state_input_and_contract_parameter_aliases_are_exact() {
    let source = r#"design "design declarations"
      input rst: bool;
      reset_input rst;
      spec {
        state x: bv<8>;
        reset { x = 0u8; } next { x = s.x + 1u8; } outputs { ready = true; }
      }
      impl {
        state x: bv<8>;
        reset { x = 0u8; } next { x = s.x + 1u8; } outputs { commit = true; }
      }
      binding spec.x == impl.x; commit commit; can_step ready;
      progress { enabled true; rank 0u1; }
      contract {
        parameter bound: bv<8>;
        pre true; invariant true; terminal false; post s.x == p.bound;
        rank 0u8; partition none;
      }"#;
    let legacy = source
        .replace("input rst: bool;", "inputs { rst: bool; }")
        .replace("state x: bv<8>;", "state { x: bv<8>; }")
        .replace("parameter bound: bv<8>;", "parameters { bound: bv<8>; }");
    let modern = parse(source);
    modern.validate().unwrap();
    let old = parse(&legacy);
    old.validate().unwrap();
    assert_eq!(modern.canonical, old.canonical);
    let invalid = source.replacen("state x: bv<8>;", "state x: bv<0>;", 1);
    let error = parse(&invalid).validate().unwrap_err();
    assert!(error.message.contains("/spec/state/x"));
    let span = error.span.unwrap();
    assert_eq!(&invalid[span.start..span.end], "x");
}
