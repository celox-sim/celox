//! Shared source syntax and explicit semantic scope projections.

use std::path::Path;

use crate::AnalyzerError;
mod scopes;
pub(crate) use scopes::ScopeHandle;

/// Shared native arena with copy-on-write support for ownership regressions.
#[derive(Clone, Debug)]
pub(super) struct Shared<T: Clone>(std::sync::Arc<T>);
impl<T: Clone> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Self(std::sync::Arc::new(value))
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
        std::sync::Arc::make_mut(&mut self.0)
    }
}
#[derive(Clone, Debug)]
pub(crate) struct SourceTree {
    declarations: Shared<celox_sv_syntax::Declarations>,
    scope: scopes::View,
}

impl SourceTree {
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

    pub(crate) fn into_declarations(self) -> celox_sv_syntax::Declarations {
        std::sync::Arc::try_unwrap(self.declarations.0).unwrap_or_else(|shared| (*shared).clone())
    }
}

pub(crate) fn parse_source(code: &str, path: &Path) -> Result<SourceTree, AnalyzerError> {
    let declarations =
        celox_sv_syntax::Declarations::parse(code, path).map_err(|error| match error.kind {
            celox_sv_syntax::ParseErrorKind::MalformedHeader => {
                AnalyzerError::Parse(error.to_string())
            }
            celox_sv_syntax::ParseErrorKind::UnsupportedSyntax => {
                AnalyzerError::Unsupported(error.to_string())
            }
        })?;
    let tree = SourceTree {
        declarations: declarations.into(),
        scope: scopes::View::default(),
    };
    scopes::validate_source(&tree)?;
    Ok(tree)
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
