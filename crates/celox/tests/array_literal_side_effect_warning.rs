use celox::{CompilationWarning, FrontendDiagnostic, Simulator, render_diagnostic};

fn warnings(code: &str) -> Vec<String> {
    Simulator::builder(code, "Top")
        .allow_always_ff_function_effects(true)
        .build()
        .unwrap()
        .warnings()
        .iter()
        .filter(|warning| {
            matches!(
                warning,
                CompilationWarning::Frontend(
                    FrontendDiagnostic::UndefinedArrayLiteralEvaluationCount { .. }
                )
            )
        })
        .map(|warning| render_diagnostic(warning))
        .collect()
}

fn source(literal: &str, sequential: bool) -> String {
    let process = if sequential {
        format!(
            "always_ff {{ if_reset {{ state = 0; values = '{{default: 0}}; }} else {{ values[0] = pick({literal}); values[1] = 0; }} }}"
        )
    } else {
        format!("always_comb {{ state = 0; values = {literal}; }}")
    };
    format!(
        r#"
module Top (clk: input clock, rst: input reset, d: input logic<8>,
            state: output logic<8>, values: output logic<8>[2]) {{
    function pick(x: input logic<8>[2]) -> logic<8> {{
        return x[0];
    }}
    function update(x: input logic<8>, result: output logic<8>) -> logic<8> {{
        result = x + 8'd1;
        return x;
    }}
    function calculate(x: input logic<8>) -> logic<8> {{
        var local_value: logic<8>;
        local_value = x + 8'd1;
        return local_value;
    }}
    {process}
}}
"#
    )
}

#[test]
fn warns_on_default_and_repeat_in_comb_and_ff_with_source_and_help() {
    for sequential in [false, true] {
        for (literal, kind) in [
            ("'{default: update(d, state)}", "default"),
            ("'{update(d, state) repeat 2}", "repeat"),
            ("'{default: calculate(update(d, state))}", "default"),
        ] {
            let warnings = warnings(&source(literal, sequential));
            assert_eq!(warnings.len(), 1, "{literal}: {warnings:?}");
            let warning = &warnings[0];
            assert!(warning.contains("undefined_array_literal_evaluation_count"));
            assert!(warning.contains(&format!("`{kind}` item")));
            assert!(warning.contains("10.9.1"));
            assert!(warning.contains("update(d, state)"));
            assert!(warning.contains("temporary"));
        }
    }
}

#[test]
fn pure_default_repeat_and_explicit_effectful_items_do_not_warn() {
    for literal in [
        "'{default: calculate(d)}",
        "'{calculate(d) repeat 2}",
        "'{update(d, state), d}",
        "'{default: {d repeat 1}}",
        "'{default: d}",
    ] {
        assert!(warnings(&source(literal, false)).is_empty(), "{literal}");
    }
}

#[test]
fn ordinary_concatenation_repeat_is_not_an_array_pattern_repeat() {
    let code = source("'{ {update(d, state) repeat 1}, d}", false);
    assert!(warnings(&code).is_empty());
}

#[test]
fn warns_on_observable_effects_in_input_only_function_bodies() {
    let code = source("'{default: calculate(d)}", true).replace(
        "local_value = x + 8'd1;",
        "local_value = x + 8'd1; $display(\"value %d\", x);",
    );
    assert_eq!(warnings(&code).len(), 1);
}

#[test]
fn repeated_instantiations_deduplicate_the_source_item() {
    let child = source("'{default: update(d, state)}", false).replace("module Top", "module Child");
    let code = format!(
        r#"{child}
module Top (clk: input clock, rst: input reset, d: input logic<8>,
            a: output logic<8>[2], b: output logic<8>[2]) {{
    inst first: Child (clk, rst, d, state: _, values: a);
    inst second: Child (clk, rst, d, state: _, values: b);
}}
"#
    );
    assert_eq!(warnings(&code).len(), 1);
}

#[test]
fn nested_patterns_and_dynamic_output_destinations_warn() {
    let nested = source("'{default: '{update(d, state) repeat 2}}", false).replace(
        "values: output logic<8>[2]",
        "values: output logic<8>[2, 2]",
    );
    assert_eq!(warnings(&nested).len(), 2);
    let dynamic = source("'{default: update(d, state[d[0]])}", false)
        .replace("state: output logic<8>", "state: output logic<8>[2]")
        .replace("state = 0;", "state = '{default: 0};");
    assert_eq!(warnings(&dynamic).len(), 1);
}

#[test]
fn unevaluated_shape_operands_do_not_warn() {
    for query in ["$bits", "$size"] {
        for literal in [
            format!("'{{default: {query}(update(d, state))}}"),
            format!("'{{{query}(update(d, state)) repeat 2}}"),
        ] {
            assert!(warnings(&source(&literal, false)).is_empty(), "{literal}");
        }
        let code = source("'{default: calculate(d)}", false).replace(
            "local_value = x + 8'd1;",
            &format!("local_value = {query}(update(x, local_value));"),
        );
        assert!(warnings(&code).is_empty(), "{query} in function body");
    }
}

#[test]
fn dynamic_nonlocal_writes_warn_but_local_writes_and_reads_do_not() {
    for literal in ["'{default: calculate(d)}", "'{calculate(d) repeat 2}"] {
        let base = source(literal, false);
        let nonlocal = base.replace(
            "local_value = x + 8'd1;",
            "state[x[0]] = 1'b1; local_value = x;",
        );
        assert_eq!(warnings(&nonlocal).len(), 1, "nonlocal: {literal}");
        let array = nonlocal
            .replace("state: output logic<8>", "state: output logic<8>[2]")
            .replace("state = 0;", "state = '{default: 0};")
            .replace("state[x[0]] = 1'b1;", "state[x[0]] = 8'd1;");
        assert_eq!(warnings(&array).len(), 1, "nonlocal array: {literal}");
        let local = base.replace(
            "local_value = x + 8'd1;",
            "local_value = x; local_value[x[0]] = 1'b1;",
        );
        assert!(warnings(&local).is_empty(), "local: {literal}");
        let read = base.replace("local_value = x + 8'd1;", "local_value = x[x[0]];");
        assert!(warnings(&read).is_empty(), "read: {literal}");
    }
}

#[test]
fn warns_on_effectful_defaults_eliminated_by_expansion() {
    for default in ["update(d, state)", "calculate(update(d, state))"] {
        let code = source(&format!("'{{d, d, default: {default}}}"), false);
        assert_eq!(warnings(&code).len(), 1, "{default}");
    }
    for default in [
        "calculate(d)",
        "$bits(update(d, state))",
        "$size(update(d, state))",
    ] {
        let code = source(&format!("'{{d, d, default: {default}}}"), false);
        assert!(warnings(&code).is_empty(), "{default}");
    }
}

#[test]
fn eliminated_defaults_preserve_body_effects_and_unevaluated_contexts() {
    let base = source("'{d, d, default: calculate(d)}", false);
    let observable = base.replace(
        "local_value = x + 8'd1;",
        "local_value = x; $display(\"value %d\", x);",
    );
    assert_eq!(warnings(&observable).len(), 1);
    let local_output = base.replace(
        "local_value = x + 8'd1;",
        "local_value = update(x, local_value);",
    );
    assert!(warnings(&local_output).is_empty());
    for query in ["$bits", "$size"] {
        let code = source(
            &format!("'{{default: {query}(pick('{{d, d, default: update(d, state)}}))}}"),
            false,
        );
        assert!(warnings(&code).is_empty(), "{query}");
    }
}
