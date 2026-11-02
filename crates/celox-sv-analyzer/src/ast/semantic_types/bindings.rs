//! Declaration-bound constant types shared by parameters and enumerators.
use super::*;
use crate::ast::{
    Type, TypeKind, const_functions::ConstantFunctions, enums::EnumDomain, expr_type_from_type,
    types,
};

/// A frozen declaration type plus an owner-checked interned semantic node.
#[derive(Debug, Clone)]
pub(in crate::ast) struct ConstantType {
    declared: Arc<Type>,
    semantic: RetainedType,
    parameter: Option<(std::path::PathBuf, celox_sv_syntax::ParameterId)>,
}
impl ConstantType {
    pub(in crate::ast) fn of_type(
        mut declared: Type,
        functions: &ConstantFunctions,
        table: &mut TyTable,
        env: &HashMap<String, i128>,
    ) -> Option<Self> {
        types::freeze_bounds(&mut declared, functions, env);
        let id = super::of_type(table, functions, &declared, env)?;
        Some(Self {
            declared: Arc::new(declared),
            semantic: table.retain(id),
            parameter: None,
        })
    }
    /// Keep binding provenance separate from nominal type identity. A query
    /// must not retain the type of an imported parameter hidden by this one.
    pub(in crate::ast) fn for_parameter(
        mut self,
        tree: &crate::ast::AnalysisContext<'_>,
        parameter: &celox_sv_syntax::Parameter,
    ) -> Self {
        self.parameter = Some((tree.compilation_path().to_owned(), parameter.id));
        self
    }

    pub(in crate::ast) fn declares_parameter(
        &self,
        tree: &crate::ast::AnalysisContext<'_>,
        parameter: &celox_sv_syntax::Parameter,
    ) -> bool {
        self.parameter
            .as_ref()
            .is_some_and(|(path, id)| path == tree.compilation_path() && *id == parameter.id)
    }

    pub(in crate::ast) fn enum_member(
        mut base: Type,
        declaration: &Type,
        functions: &ConstantFunctions,
        table: &mut TyTable,
        env: &HashMap<String, i128>,
    ) -> Option<Self> {
        let mut declaration = declaration;
        while let Some(container) = &declaration.container {
            declaration = &container.element;
        }
        base.is_enum = true;
        base.spelling = declaration.spelling.clone();
        base.enum_domain = declaration.enum_domain.clone().and_then(|domain| {
            let mut domain = (*domain).clone();
            domain.width = expr_type_from_type(functions, &base, env)?.width;
            domain.signed = base.is_signed;
            domain.four_state = base.kind != TypeKind::Bit;
            Some(Arc::new(domain))
        });
        Self::of_type(base, functions, table, env)
    }

    pub(in crate::ast) fn type_id(&self, table: &mut TyTable) -> Option<TyId> {
        self.semantic.in_table(table).or_else(|| {
            // Imported metadata may enter another independent analysis. Bounds
            // are already frozen; never reinterpret them in the caller's env.
            super::of_type(
                table,
                &Default::default(),
                &self.declared,
                &Default::default(),
            )
        })
    }
    pub(in crate::ast) fn domain(&self) -> Option<Arc<EnumDomain>> {
        self.declared.enum_domain.clone()
    }
}
impl PartialEq for ConstantType {
    fn eq(&self, other: &Self) -> bool {
        self.parameter == other.parameter
            && (Arc::ptr_eq(&self.declared, &other.declared)
                || (self.semantic.same_node(&other.semantic) && self.declared == other.declared))
    }
}
impl Eq for ConstantType {}
