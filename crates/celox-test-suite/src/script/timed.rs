use crate::{Design, Result};

/// Raw scheduler units are portable only with the suite's injected scale.
/// Comments, strings and escaped identifiers are not declarations.
pub fn validate_timed_design(design: &Design) -> Result<()> {
    validate_timed_design_with_defines(design, &[])
}

/// `names` are the macros the simulator predefines. A timed source must
/// not depend on them: the testbench is generated from the source as
/// preprocessed without them.
pub(crate) fn validate_timed_design_with_defines(design: &Design, names: &[&str]) -> Result<()> {
    if !design.timed {
        return Ok(());
    }
    let sources = active_sources(design, names)?;
    // Preserve the existing conservative contract: tool predefines must not
    // change any active text, including comments and whitespace.
    if !names.is_empty()
        && sources
            .iter()
            .map(|source| source.inspection_text())
            .collect::<Vec<_>>()
            != active_sources(design, &[])?
                .iter()
                .map(|source| source.inspection_text())
                .collect::<Vec<_>>()
    {
        return Err(
            "timed shared sources cannot depend on the simulator's predefined macros".into(),
        );
    }
    for source in &sources {
        let mut previous = "";
        for token in source
            .observations
            .iter()
            .filter(|token| !token.is_trivia())
        {
            if token.kind == celox_sv_syntax::TokenKind::Identifier {
                if matches!(token.text.as_str(), "timeunit" | "timeprecision")
                    || (previous == "`" && matches!(token.text.as_str(), "timescale" | "resetall"))
                {
                    return Err(
                        "explicit time scales and resetall are not supported in timed shared cases"
                            .into(),
                    );
                }
                // Final procedures run in an unspecified order relative to the
                // checks after run_to_finish (IEEE 1800-2023 9.2.3).
                if token.text == "final" && !matches!(previous, "assert" | "assume" | "cover") {
                    return Err("final procedures are not supported in timed shared cases".into());
                }
            }
            previous = &token.text;
        }
    }
    Ok(())
}

/// The active text of each source, preprocessed in order with `names`
/// defined.
fn active_sources(design: &Design, names: &[&str]) -> Result<Vec<celox_sv_syntax::Preprocessed>> {
    let mut preprocessor = celox_sv_syntax::Preprocessor::new(celox_sv_syntax::Options {
        includes: celox_sv_syntax::IncludePolicy::Ignore,
        ..Default::default()
    });
    for name in names {
        preprocessor.define(*name);
    }
    let mut texts = Vec::new();
    for source in &design.sources {
        let active = preprocessor
            .preprocess(&source.text, &source.path)
            .map_err(|error| format!("timed source preprocessing: {error}"))?;
        texts.push(active);
    }
    Ok(texts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn design(source: &str) -> Design {
        let text = format!(
            "(group timing (category sequential)) (case scale (source \"test.sv\" #\"{source}\"#) (top Top) (run_until 0))"
        );
        crate::load_group("timing.vtest", &text, crate::no_std_library)[0].design()
    }

    #[test]
    fn explicit_scales_are_rejected_without_rejecting_literal_text() {
        for source in [
            "`timescale 10ns/1ps\nmodule Top; endmodule",
            "`resetall\nmodule Top; endmodule",
            "module Top; timeunit 10ns; endmodule",
            "module Top; timeprecision 1fs; endmodule",
        ] {
            assert!(validate_timed_design(&design(source)).is_err());
        }
        let source = "// `timescale 10ns/1ps\nmodule Top; bit timescale; initial $display(\"timeunit 10ns\"); endmodule";
        assert!(validate_timed_design(&design(source)).is_ok());
    }

    #[test]
    fn final_procedures_are_rejected() {
        let source = "module Top; bit done; final done = 1; endmodule";
        assert!(validate_timed_design(&design(source)).is_err());
        let source = "module Top; initial $display(\"final\"); endmodule";
        assert!(validate_timed_design(&design(source)).is_ok());
    }
    #[test]
    fn final_deferred_assertions_are_not_final_procedures() {
        for assertion in ["assert", "assume", "cover"] {
            let source = format!(
                "module Top; initial {assertion} /* trivia */ final (1) $display(\"ok\"); endmodule"
            );
            assert!(validate_timed_design(&design(&source)).is_ok(), "{source}");
            let source = format!(
                "module Top; initial {assertion} final (1); final $display(\"done\"); endmodule"
            );
            assert!(validate_timed_design(&design(&source)).is_err(), "{source}");
        }
    }

    #[test]
    fn source_order_controls_macro_hidden_scale_validation() {
        for directive in [
            "`timescale 10ns/1ps",
            "`resetall",
            "timeunit 10ns;",
            "timeprecision 1fs;",
        ] {
            for undef in [false, true] {
                let text = format!(
                    r##"(group timing (category sequential))
                  (case scale (source "header.sv" #"`define SCALE_ON
module Header;endmodule"#)
                  (source "top.sv" #"{}
`ifdef SCALE_ON
{}
`endif
module Top;endmodule"#) (top Top)(run_until 0))"##,
                    if undef { "`undef SCALE_ON" } else { "" },
                    directive
                );
                let case = crate::load_group("timing.vtest", &text, crate::no_std_library)[0];
                assert_eq!(
                    validate_timed_design(&case.design()).is_ok(),
                    undef,
                    "{directive}"
                );
            }
        }
    }

    #[test]
    fn scales_follow_active_preprocessor_defines() {
        for directive in [
            "`timescale 10ns/1ps",
            "`resetall",
            "timeunit 10ns;",
            "timeprecision 1fs;",
        ] {
            let source = format!("`ifdef TOOL_ACTIVE\n{directive}\n`endif\nmodule Top;endmodule");
            assert!(validate_timed_design(&design(&source)).is_ok());
            assert!(
                validate_timed_design_with_defines(&design(&source), &["TOOL_ACTIVE"]).is_err()
            );
        }
    }

    #[test]
    fn sources_cannot_depend_on_predefined_macros() {
        let source =
            "module Top(\n`ifdef VERILATOR\ninput bit a,\n`endif\noutput bit y);\nendmodule";
        let error =
            validate_timed_design_with_defines(&design(source), &["VERILATOR"]).unwrap_err();
        assert!(error.to_string().contains("predefined macros"), "{error}");
        assert!(validate_timed_design(&design(source)).is_ok());
        let source = "module Top; endmodule";
        assert!(validate_timed_design_with_defines(&design(source), &["VERILATOR"]).is_ok());
    }

    #[test]
    fn macro_generated_scales_and_escaped_identifiers_use_native_tokens() {
        let source =
            "`define SCALE(x) timeunit x;\n`define ALIAS `SCALE\nmodule Top; `ALIAS(1ns) endmodule";
        assert!(validate_timed_design(&design(source)).is_err());
        let source = "module Top; bit \\timeunit ; bit \\final ; bit \\resetall ; endmodule";
        assert!(validate_timed_design(&design(source)).is_ok());
        let source = "`include \"missing.svh\"\nmodule Top; endmodule";
        assert!(validate_timed_design(&design(source)).is_ok());
    }

    #[test]
    fn unused_active_definitions_keep_conservative_timed_source_policy() {
        for body in [
            "timeunit 1ns;",
            "timeprecision 1ps;",
            "`timescale 1ns/1ps",
            "`resetall",
            "final done = 1;",
        ] {
            let source = format!("`define UNUSED {body}\nmodule Top; endmodule");
            assert!(validate_timed_design(&design(&source)).is_err(), "{body}");
        }
        let source =
            "`ifdef TOOL\n`define UNUSED 1\n`else\n`define UNUSED 2\n`endif\nmodule Top; endmodule";
        assert!(
            validate_timed_design_with_defines(&design(source), &["TOOL"])
                .unwrap_err()
                .to_string()
                .contains("predefined macros")
        );
        assert!(validate_timed_design(&design(source)).is_ok());
    }

    #[test]
    fn shared_simulation_sources_preprocess_with_the_native_frontend() {
        for case in
            crate::sv::cases().filter(|case| case.expectation == crate::Expectation::Simulation)
        {
            let mut pp = celox_sv_syntax::Preprocessor::new(celox_sv_syntax::Options {
                includes: celox_sv_syntax::IncludePolicy::Ignore,
                ..Default::default()
            });
            for source in case.design().sources {
                pp.preprocess(&source.text, &source.path)
                    .unwrap_or_else(|error| panic!("{}: {error}", case.name));
            }
        }
    }

    #[test]
    fn words_inside_macro_quoted_strings_are_not_time_declarations() {
        let source = "`define Q(x) `\"timeunit timeprecision final x`\"\nmodule Top; initial $display(`Q(world)); endmodule";
        assert!(validate_timed_design(&design(source)).is_ok());
        let source = "`define UNUSED `\"unclosed\nmodule Top; timeunit 1ns; endmodule";
        assert!(validate_timed_design(&design(source)).is_err());
    }
}
