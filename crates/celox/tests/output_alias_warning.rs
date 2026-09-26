use celox::{CompilationWarning, FrontendDiagnostic, Simulator, render_diagnostic};

fn alias_warnings(code: &str) -> Vec<String> {
    let simulator = Simulator::builder(code, "Top").build().unwrap();
    simulator
        .warnings()
        .iter()
        .filter(|warning| {
            matches!(
                warning,
                CompilationWarning::Frontend(FrontendDiagnostic::UnspecifiedOutputCopyOrder { .. })
            )
        })
        .map(|warning| render_diagnostic(warning))
        .collect()
}

#[test]
fn warns_on_named_outputs_inside_an_expression_without_rejecting_the_call() {
    let warnings = alias_warnings(
        r#"
module Top (tmp: output logic<8>, out: output logic) {
    function write_outputs(first: output logic<8>, second: output logic<8>) -> logic {
        first = 8'd1;
        second = 8'd2;
        return 1'b1;
    }
    always_comb {
        tmp = 0;
        out = write_outputs(second: tmp, first: tmp) & 1'b1;
    }
}
"#,
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let warning = &warnings[0];
    assert!(warning.contains("unspecified_output_copy_order"));
    assert!(warning.contains("first") && warning.contains("second"));
    assert!(warning.contains("tmp[7:0]"));
    assert!(warning.contains("write_outputs(second: tmp, first: tmp)"));
    assert!(warning.contains("use distinct output destinations"));
}

#[test]
fn warns_on_overlapping_slices_and_concatenations_but_not_disjoint_slices() {
    for (actuals, expected) in [
        ("word[7:0], word[11:4]", 1),
        ("{word[15:12], word[3:0]}, word[7:0]", 1),
        ("word[7:0], word[15:8]", 0),
    ] {
        let code = format!(
            r#"
module Top (word: output logic<16>) {{
    function outputs(first: output logic<8>, second: output logic<8>) {{
        first = 8'h12;
        second = 8'h34;
    }}
    always_comb {{
        word = 0;
        outputs({actuals});
    }}
}}
"#
        );
        assert_eq!(alias_warnings(&code).len(), expected, "{actuals}");
    }
}

#[test]
fn static_array_aliases_warn_and_dynamic_indices_are_not_assumed_to_alias() {
    for (actuals, expected) in [
        ("words[0], words[0]", 1),
        ("words[0], words[1]", 0),
        ("words[index], words[0]", 0),
    ] {
        let code = format!(
            r#"
module Top (index: input logic, words: output logic<8>[2]) {{
    function outputs(first: output logic<8>, second: output logic<8>) {{
        first = 8'h12;
        second = 8'h34;
    }}
    always_comb {{
        words = '{{0, 0}};
        outputs({actuals});
    }}
}}
"#
        );
        assert_eq!(alias_warnings(&code).len(), expected, "{actuals}");
    }
}

#[test]
fn input_output_aliasing_does_not_warn_about_copyout_order() {
    assert!(
        alias_warnings(
            r#"
module Top (d: input logic<8>, q: output logic<8>) {
    function update(value: input logic<8>, result: output logic<8>) {
        result = value + 8'd1;
    }
    always_comb {
        q = d;
        update(q, q);
    }
}
"#,
        )
        .is_empty()
    );
}

#[test]
fn repeated_instantiations_report_each_source_call_once() {
    let warnings = alias_warnings(
        r#"
module Child (d: input logic<8>, q: output logic<8>) {
    function outputs(first: output logic<8>, second: output logic<8>) {
        first = 1;
        second = 2;
    }
    function outer(x: input logic<8>, result: output logic<8>) {
        result = x;
        outputs(result, result);
    }
    always_comb { outer(d, q); }
}
module Top (d: input logic<8>, q: output logic<8>, r: output logic<8>) {
    inst a: Child (d, q);
    inst b: Child (d, q: r);
}
"#,
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}
