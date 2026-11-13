//! Shared source syntax and explicit semantic scope projections.

use std::path::Path;

use crate::AnalyzerError;
mod scopes;
pub(crate) use scopes::ScopeHandle;
mod spans;
pub(crate) use spans::{Ranges, SourceText, Spans, Starts};

/// Shared native arena with copy-on-write support for ownership regressions.
/// The span index is built on first use and dropped when the arena changes.
#[derive(Clone, Debug)]
pub(super) struct Shared<T: Clone>(
    std::sync::Arc<T>,
    std::sync::Arc<std::sync::OnceLock<spans::Spans>>,
);
impl<T: Clone> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Self(std::sync::Arc::new(value), Default::default())
    }
}
impl<T: Clone> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T: Clone> std::ops::DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.1 = Default::default();
        std::sync::Arc::make_mut(&mut self.0)
    }
}
#[derive(Clone, Debug)]
pub(crate) struct SourceTree {
    declarations: Shared<celox_sv_syntax::Declarations>,
    scope: scopes::View,
    compilation_path: std::sync::Arc<std::path::PathBuf>,
}

impl SourceTree {
    /// The tree of an empty source, for evaluation outside any scope.
    pub(crate) fn empty() -> Self {
        parse_source("", Path::new("")).expect("an empty source parses")
    }

    pub(crate) fn compilation_path(&self) -> &Path {
        &self.compilation_path
    }
    pub(crate) fn scope_handle(&self) -> ScopeHandle {
        self.scope.handle
    }
    pub(crate) fn compilation_scope(&self) -> Self {
        Self {
            scope: scopes::View::default(),
            ..self.clone()
        }
    }
    pub(crate) fn root_scopes(&self) -> Result<Vec<ScopeHandle>, AnalyzerError> {
        scopes::handles(self.declarations())
    }
    pub(crate) fn scoped(&self, handle: ScopeHandle) -> Result<Self, AnalyzerError> {
        if handle == ScopeHandle::Compilation {
            return Ok(self.compilation_scope());
        }
        if handle.unit(self.declarations()).is_none() {
            return Err(AnalyzerError::Parse(
                "explicit native unit scope is required".into(),
            ));
        }
        Ok(Self {
            scope: scopes::View {
                handle,
                selected: None,
                ports: None,
            },
            ..self.clone()
        })
    }
    pub(crate) fn with_body_membership(
        &self,
        ids: Vec<celox_sv_syntax::BodyItemId>,
    ) -> Result<Self, AnalyzerError> {
        let scope = scopes::View {
            selected: Some(ids),
            ..self.scope.clone()
        };
        scope.ids(&self.declarations)?;
        Ok(Self {
            scope,
            ..self.clone()
        })
    }
    pub(crate) fn header_port_ids(&self) -> Result<Vec<usize>, AnalyzerError> {
        let unit = self.root_unit()?;
        let ids = self
            .scope
            .ports
            .clone()
            .unwrap_or_else(|| (0..unit.ports.len()).collect());
        let mut seen = fxhash::FxHashSet::default();
        if ids
            .iter()
            .any(|id| *id >= unit.ports.len() || !seen.insert(*id))
        {
            return Err(AnalyzerError::Parse(
                "native selected header membership is invalid".into(),
            ));
        }
        Ok(ids)
    }
    pub(crate) fn with_header_membership(&self, ids: Vec<usize>) -> Result<Self, AnalyzerError> {
        let view = Self {
            scope: scopes::View {
                ports: Some(ids),
                ..self.scope.clone()
            },
            ..self.clone()
        };
        view.header_port_ids()?;
        Ok(view)
    }
    pub(crate) fn body_membership(
        &self,
    ) -> Result<Vec<celox_sv_syntax::BodyItemId>, AnalyzerError> {
        self.scope.ids(self.declarations())
    }
    pub(crate) fn root_unit(&self) -> Result<&celox_sv_syntax::Unit, AnalyzerError> {
        self.scope
            .handle
            .unit(&self.declarations)
            .ok_or_else(|| AnalyzerError::Parse("explicit native unit scope is required".into()))
    }
    pub(crate) fn root_name(&self) -> Result<String, AnalyzerError> {
        Ok(self.root_unit()?.name.spelling.clone())
    }
    pub(crate) fn scope_start(&self) -> Result<usize, AnalyzerError> {
        Ok(self
            .scope
            .handle
            .unit(&self.declarations)
            .map_or(0, |unit| unit.tokens.start))
    }
    pub(crate) fn declarations(&self) -> &celox_sv_syntax::Declarations {
        &self.declarations
    }
    /// The arena, to rewrite, which drops its span index.
    pub(crate) fn declarations_mut(&mut self) -> &mut celox_sv_syntax::Declarations {
        &mut self.declarations
    }
    /// A handle that keeps the address of [`Self::declarations`] from being
    /// reused while it is held, so that the address identifies this arena.
    pub(crate) fn declarations_incarnation(
        &self,
    ) -> std::sync::Weak<celox_sv_syntax::Declarations> {
        std::sync::Arc::downgrade(&self.declarations.0)
    }
    /// The token-order index of this file's arenas.
    pub(crate) fn spans(&self) -> &Spans {
        self.declarations
            .1
            .get_or_init(|| Spans::new(&self.declarations))
    }
    /// The expanded token text of this file.
    pub(crate) fn source_text(&self) -> &SourceText {
        self.spans()
            .text
            .get_or_init(|| SourceText::new(&self.declarations))
    }

    pub(crate) fn into_declarations(self) -> celox_sv_syntax::Declarations {
        std::sync::Arc::try_unwrap(self.declarations.0).unwrap_or_else(|shared| (*shared).clone())
    }
}

pub(crate) fn parse_source(code: &str, path: &Path) -> Result<SourceTree, AnalyzerError> {
    source_tree(celox_sv_syntax::Declarations::parse(code, path), path)
}

/// The validated tree of a parsed source.
fn parse_error(error: celox_sv_syntax::ParseError) -> AnalyzerError {
    match error.kind {
        celox_sv_syntax::ParseErrorKind::MalformedHeader => AnalyzerError::Parse(error.to_string()),
        celox_sv_syntax::ParseErrorKind::UnsupportedSyntax => {
            AnalyzerError::Unsupported(error.to_string())
        }
    }
}

fn source_tree(
    declarations: Result<celox_sv_syntax::Declarations, celox_sv_syntax::ParseError>,
    path: &Path,
) -> Result<SourceTree, AnalyzerError> {
    let declarations = declarations.map_err(parse_error)?;
    let mut tree = SourceTree {
        declarations: declarations.into(),
        scope: scopes::View::default(),
        compilation_path: std::sync::Arc::new(path.to_owned()),
    };
    scopes::validate_source(&tree)?;
    crate::ast::lets::expand(&mut tree)?;
    Ok(tree)
}

/// [`parse_source`] with only the first of several declarations of a module
/// of one name, and the names of the modules whose later declarations it
/// left out. `declared` holds the modules of the sources before this one and
/// receives the modules of this one. The tokens of the declarations left out
/// are removed before the source is parsed again, so no consumer of the tree
/// sees them.
pub(crate) fn parse_source_with_first_modules(
    code: &str,
    path: &Path,
    declared: &mut fxhash::FxHashSet<String>,
) -> Result<(SourceTree, Vec<String>), AnalyzerError> {
    let mut preprocessor = celox_sv_syntax::Preprocessor::new(celox_sv_syntax::Options {
        includes: celox_sv_syntax::IncludePolicy::Ignore,
        ..celox_sv_syntax::Options::default()
    });
    let source = preprocessor.preprocess(code, path).map_err(|diagnostic| {
        parse_error(celox_sv_syntax::ParseError {
            kind: celox_sv_syntax::ParseErrorKind::MalformedHeader,
            diagnostic,
        })
    })?;
    // The declarations are found on the tokens, before anything is parsed,
    // so a declaration left out is not checked either, such as for the
    // implicit nets of its `default_nettype`.
    let (dropped, mut names) = later_module_declarations(&source.tokens, declared);
    names.sort();
    names.dedup();
    let tokens = if dropped.is_empty() {
        source.tokens
    } else {
        let mut tokens = Vec::with_capacity(source.tokens.len());
        let mut kept = 0;
        for range in dropped {
            tokens.extend_from_slice(&source.tokens[kept..range.start]);
            kept = range.end;
        }
        tokens.extend_from_slice(&source.tokens[kept..]);
        tokens
    };
    let declarations = celox_sv_syntax::Declarations::from_tokens(preprocessor.sources, tokens);
    Ok((source_tree(declarations, path)?, names))
}

/// The token ranges of the declarations of modules that `declared` or an
/// earlier declaration in `tokens` already declares, in order, and their
/// names; `declared` receives the names of the others. A declaration runs
/// from `module` to its `endmodule` and label; module prototypes
/// (`extern module`) declare nothing.
fn later_module_declarations(
    tokens: &[celox_sv_syntax::Token],
    declared: &mut fxhash::FxHashSet<String>,
) -> (Vec<std::ops::Range<usize>>, Vec<String>) {
    let significant: Vec<usize> = tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| !token.is_trivia())
        .map(|(index, _)| index)
        .collect();
    let text = |position: usize| {
        significant
            .get(position)
            .map(|&index| tokens[index].text.as_str())
    };
    let mut dropped = Vec::new();
    let mut names = Vec::new();
    let mut position = 0;
    while position < significant.len() {
        let keyword = matches!(text(position), Some("module" | "macromodule"));
        if !keyword || (position > 0 && text(position - 1) == Some("extern")) {
            position += 1;
            continue;
        }
        let mut name_at = position + 1;
        if matches!(text(name_at), Some("static" | "automatic")) {
            name_at += 1;
        }
        let Some(name) = text(name_at).map(str::to_owned) else {
            break;
        };
        // The matching `endmodule`, past nested module declarations.
        let mut depth = 0usize;
        let mut end = position;
        while end < significant.len() {
            match text(end) {
                Some("module" | "macromodule") => depth += 1,
                Some("endmodule") => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            end += 1;
        }
        if end >= significant.len() {
            break;
        }
        // An end label, `endmodule : name`.
        if text(end + 1) == Some(":") {
            end += 2;
        }
        // `\M ` and `M` name the same module (IEEE 1800-2023 5.6.1).
        if !declared.insert(crate::ast::instances::normalize_identifier(&name)) {
            // The attribute instances in front of the declaration belong to
            // it (A.1.2, 5.12) and go with it.
            let mut start = position;
            while start > 0 && text(start - 1) == Some("*)") {
                let Some(open) = (0..start - 1).rev().find(|&at| text(at) == Some("(*")) else {
                    break;
                };
                start = open;
            }
            let last = significant[end.min(significant.len() - 1)];
            dropped.push(significant[start]..last + 1);
            names.push(name);
        }
        position = end + 1;
    }
    (dropped, names)
}

/// Test fixtures calling private scope kernels must issue a native handle too.
#[cfg(test)]
pub(crate) fn parse_scope_fixture(code: &str, path: &Path) -> Result<SourceTree, AnalyzerError> {
    let tree = parse_source(code, path)?;
    let handle = tree
        .root_scopes()?
        .first()
        .copied()
        .ok_or_else(|| AnalyzerError::Parse("fixture needs a root scope".into()))?;
    tree.scoped(handle)
}

#[cfg(test)]
mod tests;
