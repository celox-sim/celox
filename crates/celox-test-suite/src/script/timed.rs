use crate::{Design, Result};

/// Raw scheduler units are portable only with the suite's injected scale.
/// Comments, strings and escaped identifiers are not declarations.
pub fn validate_timed_design(design: &Design) -> Result<()> {
    validate_timed_design_with_defines(design, &[])
}

pub(crate) fn validate_timed_design_with_defines(design: &Design, names: &[&str]) -> Result<()> {
    if !design.timed {
        return Ok(());
    }
    let mut defines: sv_parser::Defines = Default::default();
    for name in names {
        defines.insert(
            (*name).to_owned(),
            Some(sv_parser::Define::new((*name).to_owned(), Vec::new(), None)),
        );
    }
    let includes: Vec<&std::path::Path> = Vec::new();
    for source in &design.sources {
        let (active, _) = sv_parser::preprocess_str(
            &source.text,
            &source.path,
            &defines,
            &includes,
            true,
            false,
            0,
            0,
        )
        .map_err(|error| format!("timed source preprocessing: {error}"))?;
        let text = active.text();
        if sv_words(text).iter().any(|(start, _, word)| {
            matches!(*word, "timeunit" | "timeprecision")
                || (matches!(*word, "timescale" | "resetall") && text[..*start].ends_with('`'))
        }) {
            return Err(
                "explicit time scales and resetall are not supported in timed shared cases".into(),
            );
        }
    }
    Ok(())
}

pub(crate) fn sv_words(source: &str) -> Vec<(usize, usize, &str)> {
    let bytes = source.as_bytes();
    let mut words = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                at += 2;
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += 2;
                while at + 1 < bytes.len() && &bytes[at..at + 2] != b"*/" {
                    at += 1;
                }
                at = (at + 2).min(bytes.len());
            }
            b'"' => {
                at += 1;
                while at < bytes.len() {
                    match bytes[at] {
                        b'\\' => at = (at + 2).min(bytes.len()),
                        b'"' => {
                            at += 1;
                            break;
                        }
                        _ => at += 1,
                    }
                }
            }
            b'\\' => {
                let start = at;
                at += 1;
                while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
                    at += 1;
                }
                words.push((start, at, &source[start..at]));
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => {
                let start = at;
                at += 1;
                while at < bytes.len()
                    && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'_' | b'$'))
                {
                    at += 1;
                }
                words.push((start, at, &source[start..at]));
            }
            _ => at += 1,
        }
    }
    words
}

#[cfg(feature = "external")]
pub(crate) fn after_trivia(mut source: &str) -> &str {
    loop {
        source = source.trim_start();
        if source.starts_with("//") {
            source = source.find('\n').map_or("", |at| &source[at + 1..]);
        } else if source.starts_with("/*") {
            source = source.find("*/").map_or("", |at| &source[at + 2..]);
        } else {
            return source;
        }
    }
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
}
