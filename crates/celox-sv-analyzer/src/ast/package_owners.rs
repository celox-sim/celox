//! Native import/export owners; original ranges only retain visibility kernels.
//! No range is used as an identity key or stored in ValidatedSyntax.
use super::*;
use celox_sv_syntax::{
    BodyItemId, BodyItemKind, Declarations, LocalDeclaration, PackageBindingId,
    PackageBindingItemKind, StatementKind, Unit,
};

fn mismatch() -> AnalyzerError {
    AnalyzerError::Parse("native package binding disagrees with owned declaration".into())
}
#[derive(Clone)]
pub(super) struct Binding {
    pub id: PackageBindingId,
    /// Original source range for the interface dependency reader.
    pub original: Option<(usize, usize)>,
}
impl Binding {
    pub(super) fn names(&self, tree: &SyntaxTree) -> Vec<(String, Option<String>)> {
        tree.declarations()
            .package_binding(self.id)
            .items
            .iter()
            .filter_map(|item| match &item.kind {
                PackageBindingItemKind::Named { package, name } => Some((
                    instances::normalize_identifier(&package.spelling),
                    Some(instances::normalize_identifier(&name.spelling)),
                )),
                PackageBindingItemKind::Wildcard(package) => {
                    Some((instances::normalize_identifier(&package.spelling), None))
                }
                _ => None,
            })
            .collect()
    }
}
fn header(unit: &Unit, native: &Declarations) -> Vec<PackageBindingId> {
    unit.package_bindings
        .iter()
        .copied()
        .filter(|id| {
            let range = native.package_binding(*id).tokens;
            unit.header.start <= range.start && range.end <= unit.header.end
        })
        .collect()
}
fn body_ids(ids: &[BodyItemId], tree: &Declarations) -> Converted<Vec<PackageBindingId>> {
    let mut pending = ids.iter().rev().copied().collect::<Vec<_>>();
    let mut statements = Vec::new();
    let mut result = Vec::new();
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(mismatch());
        }
        match &tree.body_item(id).kind {
            BodyItemKind::PackageBinding(id) => result.push(*id),
            BodyItemKind::Subroutine(id) => {
                statements.extend(&tree.subroutine(*id).authored_statements)
            }
            BodyItemKind::Procedure(id) => {
                statements.extend(&tree.procedure(*id).authored_statements)
            }
            BodyItemKind::Unit(id) => {
                let unit = tree.unit(*id);
                result.extend(header(unit, tree));
                pending.extend(unit.items.iter().rev().copied());
            }
            BodyItemKind::Region(items)
            | BodyItemKind::Block { items, .. }
            | BodyItemKind::Unconverted { items, .. } => {
                pending.extend(items.iter().rev().copied())
            }
            BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } => {
                pending.extend(otherwise);
                pending.push(*then_body);
            }
            BodyItemKind::Case { arms, .. } => {
                pending.extend(arms.iter().rev().map(|arm| arm.body))
            }
            BodyItemKind::Loop { body, .. } => pending.push(*body),
            _ => {}
        }
    }
    let mut seen = HashSet::default();
    while let Some(id) = statements.pop() {
        if !seen.insert(id) {
            continue;
        }
        let statement = tree.statement(id);
        if let StatementKind::Declaration(LocalDeclaration::PackageBinding(id)) = statement.kind {
            result.push(id);
        }
        statements.extend(statement.kind.children());
    }
    Ok(result)
}
fn bindings(ids: Vec<PackageBindingId>, tree: &SyntaxTree) -> Converted<Vec<Binding>> {
    let native = tree.declarations();
    let mut seen = HashSet::default();
    ids.into_iter()
        .map(|id| {
            if !seen.insert(id) {
                return Err(mismatch());
            }
            let binding = native.package_binding(id);
            let original = native.tokens[binding.tokens.start..binding.tokens.end]
                .iter()
                .filter(|token| !token.is_trivia())
                .map(|token| native.sources.invocation(token.origin))
                .fold(None, |range: Option<(usize, usize)>, span| {
                    Some(match range {
                        None => (span.start, span.end),
                        Some((start, end)) => (start.min(span.start), end.max(span.end)),
                    })
                });
            Ok(Binding { id, original })
        })
        .collect()
}
pub(super) fn item(id: BodyItemId, tree: &SyntaxTree) -> Converted<Vec<Binding>> {
    bindings(body_ids(&[id], tree.declarations())?, tree)
}
pub(super) fn scope(tree: &SyntaxTree) -> Converted<Vec<Binding>> {
    let mut ids = if let Ok(unit) = tree.root_unit() {
        header(unit, tree.declarations())
    } else {
        Vec::new()
    };
    ids.extend(body_ids(
        &tree.native_body_membership()?,
        tree.declarations(),
    )?);
    bindings(ids, tree)
}
pub(super) fn compilation(tree: &SyntaxTree) -> Converted<Vec<Binding>> {
    let tree = tree.compilation_scope();
    let ids = tree
        .native_body_membership()?
        .into_iter()
        .filter_map(|id| match tree.declarations().body_item(id).kind {
            BodyItemKind::PackageBinding(id) => Some(id),
            _ => None,
        })
        .collect();
    bindings(ids, &tree)
}
