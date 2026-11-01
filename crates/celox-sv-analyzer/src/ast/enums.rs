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

/// A scalar enumerator's declaration type, fixed in its declaration scope.
/// The frozen Type serves legacy enum-method queries while they migrate; the
/// retained node validates that its semantic ID belongs to the current table.
#[derive(Debug, Clone)]
pub(super) struct ConstantType {
    declared: std::sync::Arc<super::Type>,
    semantic: super::semantic_types::RetainedType,
}
impl ConstantType {
    pub(super) fn new(
        mut base: super::Type,
        declaration: &super::Type,
        functions: &super::const_functions::ConstantFunctions,
        table: &mut super::semantic_types::TyTable,
        env: &fxhash::FxHashMap<String, i128>,
    ) -> Option<Self> {
        let mut declaration = declaration;
        while let Some(container) = &declaration.container {
            declaration = &container.element;
        }
        base.is_enum = true;
        base.spelling = declaration.spelling.clone();
        base.enum_domain = declaration.enum_domain.clone().and_then(|domain| {
            let mut domain = (*domain).clone();
            domain.width = super::expr_type_from_type(functions, &base, env)?.width;
            domain.signed = base.is_signed;
            domain.four_state = base.kind != super::TypeKind::Bit;
            Some(std::sync::Arc::new(domain))
        });
        super::types::freeze_bounds(&mut base, functions, env);
        let id = super::semantic_types::of_type(table, functions, &base, env)?;
        Some(Self {
            declared: std::sync::Arc::new(base),
            semantic: table.retain(id),
        })
    }

    pub(super) fn type_id(
        &self,
        table: &mut super::semantic_types::TyTable,
    ) -> Option<super::semantic_types::TyId> {
        self.semantic.in_table(table).or_else(|| {
            // Imported metadata may enter another independent analysis. Bounds
            // are already frozen; never reinterpret them in the caller's env.
            super::semantic_types::of_type(
                table,
                &Default::default(),
                &self.declared,
                &Default::default(),
            )
        })
    }
    pub(super) fn domain(&self) -> Option<std::sync::Arc<EnumDomain>> {
        self.declared.enum_domain.clone()
    }
}
impl PartialEq for ConstantType {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.declared, &other.declared)
            || (self.declared.declaration_name() == other.declared.declaration_name()
                && self.declared == other.declared)
    }
}
impl Eq for ConstantType {}

/// Resolve an enumerator from declaration bindings. Typedef member contents
/// and numeric values never participate in this lookup.
pub(super) fn constant_binding(
    tree: &super::AnalysisContext<'_>,
    session: &mut super::Session,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dims: &super::PackedDimensions,
) -> super::Converted<Option<ConstantType>> {
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
        if let Some(binding) = dims.enum_constants.get(&name) {
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
