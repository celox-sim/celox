//! Diagnostics derived from authored function declarations.

use super::*;
use crate::procedural::SourceSpan;

/// A non-fatal source diagnostic, independent of constant evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalyzerWarning {
    /// Explicit and implementation-default time scales occur in one design.
    MixedTimescale,
    /// `$size` on an interface array is a compatibility extension to 20.7.
    InterfaceArrayQuery { site: Option<SourceSpan> },
    /// An ignored `$exit` in a function outside a program (IEEE 1800-2023 24.7).
    ExitOutsideProgram { site: Option<SourceSpan> },
}

pub(crate) fn append(
    warnings: &mut Vec<AnalyzerWarning>,
    found: impl IntoIterator<Item = AnalyzerWarning>,
) {
    for warning in found {
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }
}

/// Function diagnostics belong to declarations, including functions whose
/// bodies disappear during constant evaluation. Native token ownership keeps
/// nested opaque program bodies separate from surrounding function scopes.
fn functions(tree: &SyntaxTree, range: celox_sv_syntax::TokenRange) -> Vec<AnalyzerWarning> {
    use celox_sv_syntax::{ExprKind, SubroutineKind, UnitKind};
    let native = tree.declarations();
    let programs: Vec<_> = native
        .all_units()
        .filter(|unit| unit.kind == UnitKind::Program)
        .map(|unit| unit.tokens)
        .collect();
    let functions: Vec<_> = native
        .subroutines
        .iter()
        .filter(|function| {
            function.kind == SubroutineKind::Function
                && range.start <= function.tokens.start
                && function.tokens.end <= range.end
        })
        .map(|function| function.tokens)
        .collect();
    let called_names = native
        .expressions
        .iter()
        .filter_map(|expression| {
            if let ExprKind::Call { expression, .. } = &expression.kind {
                Some(native.expression(*expression).tokens)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let mut warnings = Vec::new();
    for expression in &native.expressions {
        let tokens = expression.tokens;
        if !functions
            .iter()
            .any(|range| range.start <= tokens.start && tokens.end <= range.end)
            || programs
                .iter()
                .any(|range| range.start <= tokens.start && tokens.end <= range.end)
        {
            continue;
        }
        let name = match &expression.kind {
            ExprKind::Call {
                expression: callee, ..
            } => {
                let ExprKind::Name(name) = &native.expression(*callee).kind else {
                    continue;
                };
                name
            }
            ExprKind::Name(name) if !called_names.contains(&tokens) => name,
            _ => continue,
        };
        if name.spelling == "$exit" {
            append(
                &mut warnings,
                [AnalyzerWarning::ExitOutsideProgram {
                    site: super::procedural::source_span(tokens, tree),
                }],
            );
        }
    }
    warnings
}

pub(crate) fn components(tree: &SyntaxTree) -> HashMap<String, Vec<AnalyzerWarning>> {
    let mut warnings = HashMap::default();
    for unit in tree.declarations().all_units() {
        if matches!(
            unit.kind,
            celox_sv_syntax::UnitKind::Module
                | celox_sv_syntax::UnitKind::Program
                | celox_sv_syntax::UnitKind::Interface
        ) {
            append(
                warnings
                    .entry(super::instances::normalize_identifier(&unit.name.spelling))
                    .or_default(),
                functions(tree, unit.tokens),
            );
        }
    }
    warnings
}

pub(crate) fn packages(tree: &SyntaxTree) -> Vec<AnalyzerWarning> {
    let native = tree.declarations();
    let mut warnings = Vec::new();
    for unit in native
        .all_units()
        .filter(|unit| unit.kind == celox_sv_syntax::UnitKind::Package)
    {
        append(&mut warnings, functions(tree, unit.tokens));
    }
    // Compilation-unit functions have no enclosing design unit.
    for function in &native.subroutines {
        if !native.all_units().any(|unit| {
            unit.tokens.start <= function.tokens.start && function.tokens.end <= unit.tokens.end
        }) {
            append(&mut warnings, functions(tree, function.tokens));
        }
    }
    warnings
}

/// Declaration warnings for a complete compilation unit.
pub(crate) fn source(tree: &SyntaxTree) -> Vec<AnalyzerWarning> {
    functions(
        tree,
        celox_sv_syntax::TokenRange {
            start: 0,
            end: tree.declarations().tokens.len(),
        },
    )
}
