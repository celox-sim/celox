//! Parameter values are selected by native declaration and body owners.
use super::*;
use celox_sv_syntax::{BodyItemId, BodyItemKind, ParameterKind};
pub(in crate::ast) type Values<'a> = Vec<&'a celox_sv_syntax::Parameter>;

pub(in crate::ast) fn default(
    parameter: &celox_sv_syntax::Parameter,
) -> Option<celox_sv_syntax::ExprId> {
    match parameter.kind {
        ParameterKind::Value { default, .. } => default,
        _ => None,
    }
}

pub(in crate::ast) fn body(id: BodyItemId, tree: &SyntaxTree) -> Converted<Values<'_>> {
    let BodyItemKind::Parameter(id) = tree.declarations().body_item(id).kind else {
        return Err(AnalyzerError::Parse(
            "native parameter body owner has a different declaration class".into(),
        ));
    };
    Ok(tree
        .declarations()
        .parameter_declaration(id)
        .parameters
        .iter()
        .filter(|parameter| matches!(parameter.kind, ParameterKind::Value { .. }))
        .collect())
}
