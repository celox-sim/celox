//! Recoverable source diagnostics.

use super::*;
use crate::procedural::SourceSpan;

/// A non-fatal source diagnostic, independent of constant evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalyzerWarning {
    /// Design element `element` takes the default time unit (`unit`) or
    /// precision (`precision`) while others declare theirs (IEEE 1800-2023
    /// 3.14.2.3).
    MixedTimescale {
        element: String,
        unit: bool,
        precision: bool,
    },
    /// A later declaration of module `name` was ignored for the first one.
    DuplicateModule { name: String },
    /// `$size` on an interface array is a compatibility extension to 20.7.
    InterfaceArrayQuery { site: Option<SourceSpan> },
    /// Omitted interface array selections are an extension to 23.6.
    InterfaceArrayParameter {
        path: String,
        element: String,
        site: Option<SourceSpan>,
    },
    /// An ignored `$exit` in a function outside a program (IEEE 1800-2023 24.7).
    ExitOutsideProgram { site: Option<SourceSpan> },
    /// A static variable whose declaration specifies an initialization value
    /// without the `static` keyword (IEEE 1800-2023 6.21).
    ImplicitStaticInitializer {
        name: String,
        site: Option<SourceSpan>,
    },
    /// A packed integral value or unbased fill assigned to a fixed unpacked array (7.6).
    PackedToUnpackedAssignment { site: Option<SourceSpan> },
    /// A force or release on a variable selection or unpacked array element (10.6.2).
    NonstandardForceTarget { site: Option<SourceSpan> },
    /// The vendor alias of the standard formatting function (21.3.3).
    PsprintfAlias { site: Option<SourceSpan> },
    /// A call of `$set_coverage_db_name` or `$load_coverage_db` (`task`),
    /// which saves or loads nothing: there is no coverage database (IEEE
    /// 1800-2023 19.9).
    NoCoverageDatabase {
        task: &'static str,
        site: Option<SourceSpan>,
    },
    /// A value or a range of bin `bin` of coverpoint `item` of covergroup
    /// `covergroup` that takes no part in the bin, or only a part of which
    /// does, as `detail` says: the type of the coverpoint has not all of
    /// its values (IEEE 1800-2023 19.5.7).
    CovergroupBinValue {
        covergroup: String,
        item: String,
        bin: String,
        detail: String,
        site: Option<SourceSpan>,
    },
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
    let spans = tree.spans();
    let functions: Vec<_> = spans
        .subroutines
        .within(&[range])
        .into_iter()
        .map(|index| &native.subroutines[index])
        .filter(|function| {
            function.kind == SubroutineKind::Function
                && range.start <= function.tokens.start
                && function.tokens.end <= range.end
        })
        .map(|function| function.tokens)
        .collect();
    let called_names = &validation::Facts::of(tree).callee_spans;
    let mut warnings = Vec::new();
    for id in spans.expressions.within(&functions) {
        let expression = native.expression(id);
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
            ExprKind::Name(name) if !called_names.contains(&(tokens.start, tokens.end)) => name,
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

/// The names of the coverage database tasks (IEEE 1800-2023 19.9).
pub(crate) const COVERAGE_DATABASE_TASKS: [&str; 2] =
    ["$set_coverage_db_name", "$load_coverage_db"];

/// The calls of the coverage database tasks in `range` (IEEE 1800-2023
/// 19.9), which have no database to name or load.
fn coverage_databases(
    tree: &SyntaxTree,
    range: celox_sv_syntax::TokenRange,
) -> Vec<AnalyzerWarning> {
    let mut warnings = Vec::new();
    for (tokens, task) in &tree.spans().coverage_database_tasks {
        if range.start <= tokens.start && tokens.end <= range.end {
            append(
                &mut warnings,
                [AnalyzerWarning::NoCoverageDatabase {
                    task: COVERAGE_DATABASE_TASKS[*task],
                    site: super::procedural::source_span(*tokens, tree),
                }],
            );
        }
    }
    warnings
}

/// The parser canonicalizes the alias even in expressions that are folded
/// away, so use the original spelling from the preprocessed tokens.
fn psprintf_aliases(
    tree: &SyntaxTree,
    range: celox_sv_syntax::TokenRange,
) -> impl Iterator<Item = AnalyzerWarning> + '_ {
    use celox_sv_syntax::{TokenKind, TokenRange};
    tree.declarations().tokens[range.start..range.end]
        .iter()
        .enumerate()
        .filter(|(_, token)| token.kind == TokenKind::SystemIdentifier && token.text == "$psprintf")
        .map(move |(offset, _)| {
            let start = range.start + offset;
            AnalyzerWarning::PsprintfAlias {
                site: procedural::source_span(
                    TokenRange {
                        start,
                        end: start + 1,
                    },
                    tree,
                ),
            }
        })
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
            let warnings = warnings
                .entry(super::instances::normalize_identifier(&unit.name.spelling))
                .or_default();
            append(warnings, functions(tree, unit.tokens));
            append(warnings, coverage_databases(tree, unit.tokens));
            append(warnings, psprintf_aliases(tree, unit.tokens));
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
        append(&mut warnings, coverage_databases(tree, unit.tokens));
        append(&mut warnings, psprintf_aliases(tree, unit.tokens));
    }
    for item in &native.source_items {
        if matches!(item.kind, celox_sv_syntax::SourceItemKind::Declaration(_)) {
            append(&mut warnings, psprintf_aliases(tree, item.tokens));
        }
    }
    // Compilation-unit functions have no enclosing design unit.
    for function in &native.subroutines {
        if !native.all_units().any(|unit| {
            unit.tokens.start <= function.tokens.start && function.tokens.end <= unit.tokens.end
        }) {
            append(&mut warnings, functions(tree, function.tokens));
            append(&mut warnings, coverage_databases(tree, function.tokens));
        }
    }
    warnings
}

/// Declaration warnings for a complete compilation unit.
pub(crate) fn source(tree: &SyntaxTree) -> Vec<AnalyzerWarning> {
    let range = celox_sv_syntax::TokenRange {
        start: 0,
        end: tree.declarations().tokens.len(),
    };
    let mut warnings = functions(tree, range);
    append(&mut warnings, coverage_databases(tree, range));
    append(&mut warnings, psprintf_aliases(tree, range));
    warnings
}
