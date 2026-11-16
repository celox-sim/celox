use super::*;

/// Resolve a constant from declaration bindings. Its numeric value and the
/// contents of visible typedefs never choose the declaration type.
pub(in crate::ast) fn constant_binding(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dims: &PackedDimensions,
) -> Converted<Option<semantic_types::ConstantType>> {
    use celox_sv_syntax::ExprKind;
    let id = expression_binding::ungroup(id, native);
    if let Some(name) = expression_binding::callable_name(tree, id, native) {
        if let Some(binding) = dims.constant_types.get(&name) {
            return Ok(Some(binding.clone()));
        }
        if dims.get(&name).is_some()
            || dims
                .const_env
                .contains_key(&parameters::unbounded_parameter_marker(&name))
        {
            return Ok(None);
        }
        // A local enumerator may hide an outer typedef. If no visible
        // constant binding owns the name, a visible typedef hides imports.
        if dims.type_aliases.contains_key(&name) {
            return Ok(None);
        }
        if let Some(binding) = scope::imported(tree).constant_types.get(&name) {
            return Ok(Some(binding.clone()));
        }
        if !name.contains("::")
            && let Some(class) = &dims.class_scope
        {
            return Ok(
                classes::specializations::constant_types(tree, session, class)
                    .get(&name)
                    .cloned(),
            );
        }
    }
    if let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(id).kind
        && let Some(class) = match separator.as_str() {
            "::" => {
                expression_binding::objects::named_class(tree, session, *expression, native, dims)?
            }
            "." => type_of(tree, session, *expression, native, dims)
                .ok()
                .and_then(|ty| class_name(&session.types, ty)),
            _ => None,
        }
    {
        return Ok(
            classes::specializations::constant_types(tree, session, &class)
                .get(&instances::normalize_identifier(&name.spelling))
                .cloned(),
        );
    }
    Ok(None)
}
