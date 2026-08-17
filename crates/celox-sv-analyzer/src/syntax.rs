//! Thin wrapper around `sv-parser`.

use std::{collections::HashMap, path::Path};

use sv_parser::{Define, Defines, SyntaxTree, parse_sv_str, preprocess_str};

use crate::AnalyzerError;

/// Parse a SystemVerilog source string into an `sv-parser` syntax tree.
pub fn parse_source(code: &str, path: &Path) -> Result<SyntaxTree, AnalyzerError> {
    let defines: Defines<std::collections::hash_map::RandomState> =
        HashMap::<String, Option<Define>>::default();
    let includes: Vec<&Path> = Vec::new();
    let (syntax_tree, _) = parse_sv_str(code, path, &defines, &includes, true, false)
        .map_err(|error| AnalyzerError::Parse(error.to_string()))?;

    Ok(syntax_tree)
}

pub fn source_module_implicit_net_permissions(
    code: &str,
    path: &Path,
) -> Result<Vec<(String, bool)>, AnalyzerError> {
    let defines: Defines<std::collections::hash_map::RandomState> = HashMap::default();
    let includes: Vec<&Path> = Vec::new();
    let (source, _) = preprocess_str(code, path, &defines, &includes, true, true, 0, 0)
        .map_err(|error| AnalyzerError::Parse(error.to_string()))?;
    let tokens = systemverilog_tokens(source.text());
    let mut modules = Vec::new();
    let mut implicit_nets_allowed = true;
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "`default_nettype" => {
                if let Some(value) = tokens.get(index + 1) {
                    implicit_nets_allowed = value != "none";
                    index += 1;
                }
            }
            "`resetall" => implicit_nets_allowed = true,
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
                }
            }
            _ => {}
        }
        index += 1;
    }
    Ok(modules)
}

fn systemverilog_tokens(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                } else if bytes[index] == b'"' {
                    index += 1;
                    break;
                } else {
                    index += 1;
                }
            }
            continue;
        }
        if bytes[index] == b'\\' {
            let start = index;
            index += 1;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            let token = std::str::from_utf8(&bytes[start..index]).unwrap_or_default();
            tokens.push(token.to_string());
            continue;
        }
        let directive = bytes[index] == b'`';
        if directive {
            index += 1;
        }
        if index < bytes.len()
            && (bytes[index].is_ascii_alphabetic() || matches!(bytes[index], b'_' | b'$'))
        {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'$'))
            {
                index += 1;
            }
            let token = std::str::from_utf8(&bytes[start..index]).unwrap_or_default();
            tokens.push(if directive {
                format!("`{token}")
            } else {
                token.to_string()
            });
            continue;
        }
        index += 1;
    }
    tokens
}
