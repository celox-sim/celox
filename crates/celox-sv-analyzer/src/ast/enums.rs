//! Enumerated types (IEEE 1800-2023 6.19) and their methods (6.19.5).

/// One named member of an enumerated type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct EnumMember {
    pub name: String,
    pub value: i128,
}

/// The members of an enumerated type in declaration order, with the shape
/// of its base type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct EnumDomain {
    pub members: Vec<EnumMember>,
    pub width: usize,
    pub signed: bool,
    /// Whether the base type is 4-state, whose default initial value is
    /// `'x` rather than `'0` (Table 6-7).
    pub four_state: bool,
}

impl EnumDomain {
    /// The values of the members, in declaration order.
    pub fn values(&self) -> Vec<i128> {
        self.members.iter().map(|member| member.value).collect()
    }
}

/// Resolve an enumerator from declaration bindings. Typedef member contents
/// and numeric values never participate in this lookup.
pub(super) fn constant_binding(
    tree: &super::AnalysisContext<'_>,
    session: &mut super::Session,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dims: &super::PackedDimensions,
) -> super::Converted<Option<super::semantic_types::ConstantType>> {
    use super::*;
    use celox_sv_syntax::ExprKind;
    let id = expression_binding::ungroup(id, native);
    if let Some(name) = expression_binding::callable_name(tree, id, native) {
        if dims.get(&name).is_some()
            || dims
                .const_env
                .contains_key(&parameters::unbounded_parameter_marker(&name))
        {
            return Ok(None);
        }
        if let Some(binding) = dims.constant_types.get(&name) {
            return Ok(Some(binding.clone()));
        }
        // A local enumerator may hide an outer typedef. If no visible
        // constant binding owns the name, a visible typedef hides imports.
        if dims.type_aliases.contains_key(&name) {
            return Ok(None);
        }
        if let Some(binding) = scope::imported(tree).enum_constants.declarations.get(&name) {
            return Ok(Some(binding.clone()));
        }
        if !name.contains("::")
            && let Some(class) = &dims.class_scope
        {
            return Ok(
                classes::specializations::enum_constants(tree, session, class)
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
        && separator == "::"
        && let Some(class) =
            expression_binding::objects::named_class(tree, session, *expression, native, dims)?
    {
        return Ok(
            classes::specializations::enum_constants(tree, session, &class)
                .get(&instances::normalize_identifier(&name.spelling))
                .cloned(),
        );
    }
    Ok(None)
}
