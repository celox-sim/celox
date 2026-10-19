//! Explicit native scope identities and selected body membership.
use super::*;
use fxhash::FxHashSet as HashSet;
type Converted<T> = Result<T, AnalyzerError>;
use celox_sv_syntax::{
    BodyItemId, BodyItemKind, Declarations, SourceItemKind, SourceUnitId, Unit, UnitId, UnitKind,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ScopeHandle {
    Compilation,
    SourceUnit(SourceUnitId),
    Unit(UnitId),
}
#[derive(Clone, Debug)]
pub(super) struct View {
    pub handle: ScopeHandle,
    pub selected: Option<Vec<BodyItemId>>,
    pub ports: Option<Vec<usize>>,
}
impl Default for View {
    fn default() -> Self {
        Self {
            handle: ScopeHandle::Compilation,
            selected: None,
            ports: None,
        }
    }
}
fn mismatch() -> AnalyzerError {
    AnalyzerError::Parse("invalid native scope membership".into())
}
impl ScopeHandle {
    pub(crate) fn unit(self, native: &Declarations) -> Option<&Unit> {
        match self {
            Self::Compilation => None,
            Self::SourceUnit(id) => Some(native.source_unit(id)),
            Self::Unit(id) => Some(native.unit(id)),
        }
    }
}
fn ignored(native: &Declarations, id: BodyItemId) -> bool {
    matches!(
        native.body_item(id).kind,
        BodyItemKind::Time(_) | BodyItemKind::Modport(_)
    )
}
pub(super) fn handles(native: &Declarations) -> Converted<Vec<ScopeHandle>> {
    let mut pending = native
        .source_items
        .iter()
        .rev()
        .filter_map(|item| match item.kind {
            SourceItemKind::Unit(id) => Some(ScopeHandle::SourceUnit(id)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut seen = HashSet::default();
    let mut result = Vec::new();
    while let Some(handle) = pending.pop() {
        if !seen.insert(handle) {
            return Err(mismatch());
        }
        let unit = handle.unit(native).ok_or_else(mismatch)?;
        pending.extend(unit.children.iter().rev().map(|id| ScopeHandle::Unit(*id)));
        if !unit.external
            && matches!(
                unit.kind,
                UnitKind::Module | UnitKind::Interface | UnitKind::Package
            )
        {
            result.push(handle);
        }
    }
    Ok(result)
}
fn authored(handle: ScopeHandle, native: &Declarations) -> Converted<Vec<BodyItemId>> {
    let ids = match handle.unit(native) {
        Some(unit) => unit.items.clone(),
        None => native
            .source_items
            .iter()
            .filter_map(|item| match item.kind {
                SourceItemKind::Declaration(id) => Some(id),
                _ => None,
            })
            .collect(),
    };
    let mut seen = HashSet::default();
    if ids.iter().any(|id| !seen.insert(*id)) {
        return Err(mismatch());
    }
    if let Some(unit) = handle.unit(native) {
        if ids.iter().any(|id| {
            let tokens = native.body_item(*id).tokens;
            tokens.start < unit.body.start || tokens.end > unit.body.end
        }) {
            return Err(mismatch());
        }
    }
    Ok(ids.into_iter().filter(|id| !ignored(native, *id)).collect())
}
impl View {
    pub(super) fn ids(&self, native: &Declarations) -> Converted<Vec<BodyItemId>> {
        let authored = authored(self.handle, native)?;
        let Some(selected) = &self.selected else {
            return Ok(authored);
        };
        let mut membership = HashSet::default();
        if selected
            .iter()
            .any(|id| !membership.insert(*id) || !authored.contains(id))
        {
            return Err(mismatch());
        }
        Ok(authored
            .into_iter()
            .filter(|id| membership.contains(id))
            .collect())
    }
}
pub(super) fn validate_source(tree: &ValidatedSyntax) -> Converted<()> {
    let native = tree.declarations();
    let mut kinds = HashSet::default();
    for item in &native.source_items {
        if !kinds.insert(item.kind) {
            return Err(mismatch());
        }
        let tokens = match item.kind {
            SourceItemKind::Unit(id) => native.source_unit(id).tokens,
            SourceItemKind::Declaration(id) => native.body_item(id).tokens,
        };
        if tokens.start < item.tokens.start || tokens.end > item.tokens.end {
            return Err(mismatch());
        }
    }
    for handle in handles(native)? {
        authored(handle, native)?;
    }
    authored(ScopeHandle::Compilation, native)?;
    Ok(())
}
