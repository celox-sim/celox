//! Declaration-scoped parameter bindings.

use super::*;

/// Bind native declarations in this module's parameter scope by stable ID.
/// A nested local type named T must not inherit the module parameter's binding.
pub(super) fn bind(
    tree: &SyntaxTree,
    overrides: &[(String, Type)],
) -> Result<HashMap<celox_sv_syntax::ParameterId, Type>, AnalyzerError> {
    let mut declarations = HashMap::default();
    let unit = tree.root_unit()?;
    let has_port_list = unit.parameters.is_some();
    for parameter in unit.parameters.iter().flatten() {
        if matches!(parameter.kind, celox_sv_syntax::ParameterKind::Type { .. }) {
            declarations.insert(
                instances::normalize_identifier(&parameter.name.spelling),
                (parameter.id, parameter.local),
            );
        }
    }
    let mut pending = tree
        .body_membership()?
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        match &tree.declarations().body_item(id).kind {
            celox_sv_syntax::BodyItemKind::Region(items) => pending.extend(items.iter().rev()),
            celox_sv_syntax::BodyItemKind::Parameter(id) => {
                for parameter in tree
                    .declarations()
                    .parameter_declaration(*id)
                    .parameters
                    .iter()
                {
                    if matches!(parameter.kind, celox_sv_syntax::ParameterKind::Type { .. }) {
                        declarations.insert(
                            instances::normalize_identifier(&parameter.name.spelling),
                            (parameter.id, parameter.local || has_port_list),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    let mut bindings = HashMap::default();
    for (name, ty) in overrides {
        let &(offset, local) = declarations
            .get(name)
            .ok_or_else(|| AnalyzerError::UnknownParameterOverride { name: name.clone() })?;
        if local {
            return Err(AnalyzerError::Unsupported(format!(
                "localparam override `{name}`"
            )));
        }
        if bindings.insert(offset, ty.clone()).is_some() {
            return Err(AnalyzerError::Unsupported(format!(
                "duplicate parameter override `{name}`"
            )));
        }
    }
    Ok(bindings)
}

/// Freeze every dimension in the caller's environment before crossing the
/// instance boundary. A child's parameters must not reinterpret caller names.
pub(super) fn resolve(
    state: &AnalysisState,
    mut ty: Type,
    env: &HashMap<String, i128>,
) -> Option<Type> {
    fn bound(
        state: &AnalysisState,
        expr: &ConstExpr,
        env: &HashMap<String, i128>,
    ) -> Option<ConstExpr> {
        let value = eval_ast_const_expr(state, expr, env)?;
        Some(if value >= 0 {
            ConstExpr::Literal(value.to_string())
        } else {
            ConstExpr::Literal(format_typed_parameter_literal(value, 128, true))
        })
    }
    for range in &mut ty.packed_ranges {
        range.left = bound(state, &range.left, env)?;
        range.right = bound(state, &range.right, env)?;
    }
    for range in &mut ty.unpacked_ranges {
        range.left = bound(state, &range.left, env)?;
        range.right = bound(state, &range.right, env)?;
        if let Some(size) = &range.size {
            range.size = Some(bound(state, size, env)?);
        }
    }
    let mut members = Vec::new();
    for member in ty.members {
        let resolved = resolve(state, member.r#type().clone(), env)?;
        members.push(member.with_type(|_| resolved));
    }
    ty.members = members;
    Some(ty)
}

/// Locate value-pattern bindings by declaration, so nested names stay local.
pub(super) fn bind_patterns(
    tree: &SyntaxTree,
    overrides: &[(String, BoundPattern)],
) -> Converted<HashMap<celox_sv_syntax::ParameterId, BoundPattern>> {
    let unit = tree.root_unit()?;
    let mut declarations = HashMap::default();
    for parameter in unit.parameters.iter().flatten() {
        if matches!(parameter.kind, celox_sv_syntax::ParameterKind::Value { .. }) {
            declarations.insert(
                instances::normalize_identifier(&parameter.name.spelling),
                (parameter.id, parameter.local),
            );
        }
    }
    let mut pending = tree.body_membership()?;
    while let Some(id) = pending.pop() {
        match &tree.declarations().body_item(id).kind {
            celox_sv_syntax::BodyItemKind::Region(items) => pending.extend(items),
            celox_sv_syntax::BodyItemKind::Parameter(id) => {
                for parameter in &tree.declarations().parameter_declaration(*id).parameters {
                    if matches!(parameter.kind, celox_sv_syntax::ParameterKind::Value { .. }) {
                        declarations.insert(
                            instances::normalize_identifier(&parameter.name.spelling),
                            (parameter.id, parameter.local || unit.parameters.is_some()),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    let mut bindings = HashMap::default();
    for (name, value) in overrides {
        let &(key, local) = declarations.get(name).ok_or_else(|| {
            unsupported(format!(
                "pattern override of non-overridable parameter `{name}`"
            ))
        })?;
        if local {
            return Err(unsupported(format!(
                "pattern override of non-overridable parameter `{name}`"
            )));
        }
        if bindings.insert(key, value.clone()).is_some() {
            return Err(unsupported(format!(
                "duplicate parameter override `{name}`"
            )));
        }
    }
    Ok(bindings)
}
