//! Thin wrapper around `sv-parser`.

use std::{collections::HashMap, path::Path};

use sv_parser::{Define, Defines, SyntaxTree, parse_sv_str};

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
