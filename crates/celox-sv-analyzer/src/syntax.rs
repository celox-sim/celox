//! Parsing entry point and source directive inspection.
//!
//! Owned native declaration arenas are the public parse result. Unsupported
//! body classes retain explicit native boundaries for semantic diagnostics.

use std::path::Path;

use crate::AnalyzerError;

/// Expand macros and includes using the native preprocessor.
/// Each call is one compilation unit; source-defined macros do not escape it.
/// Command-line macros without a value expand to `1`. Compiler directives such
/// as `timescale`, `line` and `default_nettype` remain in the returned source.
pub fn preprocess_source(
    code: &str,
    path: &Path,
    macros: &[(String, Option<String>)],
    includes: &[&Path],
) -> Result<String, AnalyzerError> {
    let mut preprocessor = celox_sv_syntax::Preprocessor::new(celox_sv_syntax::Options {
        include_paths: includes.iter().map(|path| path.to_path_buf()).collect(),
        ..Default::default()
    });
    for (name, value) in macros {
        let definition = format!("`define {name} {}\n", value.as_deref().unwrap_or("1"));
        preprocessor
            .preprocess(&definition, Path::new("<command-line>"))
            .map_err(|error| AnalyzerError::Parse(error.to_string()))?;
    }
    preprocessor
        .preprocess(code, path)
        .map(|source| source.text())
        .map_err(|error| AnalyzerError::Parse(error.to_string()))
}

/// Parse native source syntax with authored tokens and original source origins.
///
/// The native parser validates supported syntax and structural boundaries.
/// Syntax classes that remain unsupported are retained explicitly; semantic
/// analysis reports their unsupported behavior.
///
/// ```
/// use std::path::Path;
/// let syntax: celox_sv_syntax::Declarations =
///     celox_sv_analyzer::syntax::parse_source("module M; endmodule", Path::new("m.sv"))?;
/// assert_eq!(syntax.units[0].name.spelling, "M");
/// # Ok::<(), celox_sv_analyzer::AnalyzerError>(())
/// ```
///
/// Syntax arenas do not expose an implicit source-tree iterator:
///
/// ```compile_fail
/// let syntax = celox_sv_analyzer::syntax::parse_source(
///     "module M; endmodule", std::path::Path::new("m.sv")).unwrap();
/// syntax.into_iter();
/// ```
pub fn parse_source(
    code: &str,
    path: &Path,
) -> Result<celox_sv_syntax::Declarations, AnalyzerError> {
    Ok(crate::source_tree::parse_source(code, path)?.into_declarations())
}

pub fn source_module_implicit_net_permissions(
    code: &str,
    path: &Path,
) -> Result<Vec<(String, bool)>, AnalyzerError> {
    let mut preprocessor = celox_sv_syntax::Preprocessor::new(celox_sv_syntax::Options {
        includes: celox_sv_syntax::IncludePolicy::Ignore,
        strip_comments: true,
        ..Default::default()
    });
    let source = preprocessor
        .preprocess(code, path)
        .map_err(|error| AnalyzerError::Parse(error.to_string()))?;
    let tokens: Vec<String> = source
        .tokens
        .iter()
        .filter(|token| !token.is_trivia())
        .map(|token| token.text.clone())
        .collect();
    let mut modules = Vec::new();
    let mut implicit_nets_allowed = true;
    let mut current_module = None;
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "default_nettype" if index > 0 && tokens[index - 1] == "`" => {
                if let Some(value) = tokens.get(index + 1) {
                    if !matches!(value.as_str(), "wire" | "none") {
                        return Err(AnalyzerError::Unsupported(format!(
                            "`default_nettype {value}`"
                        )));
                    }
                    let next_implicit_nets_allowed = value != "none";
                    if next_implicit_nets_allowed != implicit_nets_allowed
                        && let Some(module) = current_module.as_deref()
                    {
                        return Err(AnalyzerError::Unsupported(format!(
                            "`default_nettype change inside module `{module}`"
                        )));
                    }
                    implicit_nets_allowed = next_implicit_nets_allowed;
                    index += 1;
                }
            }
            "resetall" if index > 0 && tokens[index - 1] == "`" => {
                if !implicit_nets_allowed && let Some(module) = current_module.as_deref() {
                    return Err(AnalyzerError::Unsupported(format!(
                        "`default_nettype change inside module `{module}`"
                    )));
                }
                implicit_nets_allowed = true;
            }
            "module" => {
                let mut name_index = index + 1;
                if tokens
                    .get(name_index)
                    .is_some_and(|token| matches!(token.as_str(), "automatic" | "static"))
                {
                    name_index += 1;
                }
                if let Some(name) = tokens.get(name_index) {
                    modules.push((name.clone(), implicit_nets_allowed));
                    current_module = Some(name.clone());
                }
            }
            "endmodule" => current_module = None,
            _ => {}
        }
        index += 1;
    }
    Ok(modules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_enum_declarations_materialize_storage() {
        for items in ["a,b,c", "start=10,step[10]", "start=10,stop[11:13]"] {
            let code = format!("module M; enum {{{items}}} e; endmodule");
            let ir = crate::analyze_source(&code, Path::new("anonymous-enum.sv")).unwrap();
            assert_eq!(
                ir.modules()[0]
                    .signals()
                    .iter()
                    .map(|signal| signal.name())
                    .collect::<Vec<_>>(),
                ["e"]
            );
        }
    }

    #[test]
    fn inert_specparams_do_not_become_value_parameters() {
        let path = Path::new("specparams.sv");
        let code = "module M; specparam delay=50; endmodule";
        parse_source(code, path).unwrap();
        crate::analyze_source(code, path).unwrap();
        for code in [
            "module M; specparam delay=50; parameter p=delay+2; endmodule",
            r"module M; specparam \delay =50; parameter p=\delay +2; endmodule",
        ] {
            let error = crate::analyze_source(code, path).unwrap_err();
            assert!(
                matches!(error, AnalyzerError::Unsupported(ref reason) if reason == "specparam in value parameter"),
                "{error}"
            );
        }
    }

    #[test]
    /// A deferred assertion as a module item is the same assertion in an
    /// `always_comb` (IEEE 1800-2023 16.4.3), labeled or not.
    fn module_deferred_assertions_are_always_comb_processes() {
        for keyword in ["assert", "assume", "cover"] {
            for deferral in ["#0", "final"] {
                for label in ["", "a1: "] {
                    let code = format!(
                        "module M; logic a=1; {label}{keyword} {deferral} (a!=0); endmodule"
                    );
                    let path = Path::new("module-deferred.sv");
                    parse_source(&code, path).unwrap();
                    crate::analyze_source(&code, path).unwrap();
                }
            }
        }
    }

    #[test]
    fn public_preprocessing_and_parsing_reject_nameless_pragmas() {
        let code = "`pragma\nmodule M; endmodule";
        let path = Path::new("pragma.sv");
        assert!(preprocess_source(code, path, &[], &[]).is_err());
        assert!(parse_source(code, path).is_err());
        assert!(crate::ParsedSource::parse(code, path).is_err());
    }

    #[test]
    fn command_macros_have_values_without_leaking_source_definitions() {
        let path = Path::new("macros.sv");
        let macros = [("WIDTH".into(), Some("7".into())), ("FLAG".into(), None)];
        let expanded = preprocess_source(
            "`define LOCAL 1\nmodule M; localparam N=`WIDTH+`FLAG; endmodule",
            path,
            &macros,
            &[],
        )
        .unwrap();
        assert!(expanded.contains("N=7+1"), "{expanded}");
        let separate = preprocess_source(
            "`ifdef LOCAL\nmodule Leaked; endmodule\n`else\nmodule Separate; endmodule\n`endif\n",
            path,
            &macros,
            &[],
        )
        .unwrap();
        assert!(separate.contains("module Separate"), "{separate}");
        assert!(!separate.contains("module Leaked"), "{separate}");
    }

    #[test]
    fn public_native_parse_rejects_empty_gate_terminals() {
        let code = "module M; and g(a,,a); endmodule";
        let path = Path::new("invalid_body.sv");
        assert!(celox_sv_syntax::Declarations::parse(code, path).is_err());
        assert!(parse_source(code, path).is_err());
        assert!(crate::ParsedSource::parse(code, path).is_err());

        let syntax: celox_sv_syntax::Declarations =
            parse_source("module M; endmodule", Path::new("original.sv")).unwrap();
        assert_eq!(syntax.units[0].name.spelling, "M");
    }

    #[test]
    fn uninvoked_macro_bodies_cannot_create_modules_or_change_nettype() {
        let code = "`define PHANTOM module Hidden; endmodule\n`define DISABLE `default_nettype none\nmodule Actual; endmodule";
        assert_eq!(
            source_module_implicit_net_permissions(code, Path::new("original.sv")).unwrap(),
            [("Actual".to_owned(), true)]
        );
        let code = "`define DISABLE `default_nettype none\n`DISABLE\nmodule Actual; endmodule";
        assert_eq!(
            source_module_implicit_net_permissions(code, Path::new("original.sv")).unwrap(),
            [("Actual".to_owned(), false)]
        );
        let code =
            "`define HEADER(name) module name; endmodule\n`define ALIAS `HEADER\n`ALIAS(Actual)";
        assert_eq!(
            source_module_implicit_net_permissions(code, Path::new("original.sv")).unwrap(),
            [("Actual".to_owned(), true)]
        );
    }
}
