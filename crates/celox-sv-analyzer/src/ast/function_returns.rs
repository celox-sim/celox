//! Return metadata is constructed in one place from resolved declarations.
//! The semantic ID survives body binding; legacy metadata projections remain
//! while their consumers migrate to the semantic table.
use super::*;
use crate::ir::{DpiImport, DpiType};

#[cfg(test)]
mod tests;

impl FunctionReturnMetadata {
    pub(super) fn from_type(
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        ty: Option<&Type>,
        arguments: Arc<[FunctionArgumentMetadata]>,
        context: &PackedDimensions,
    ) -> Self {
        let functions = &tree.functions;
        let widths = ty.map(|ty| {
            ty.packed_ranges()
                .iter()
                .map(|range| {
                    usize::try_from(
                        eval_ast_const_expr(functions, range.left(), &context.const_env)?.abs_diff(
                            eval_ast_const_expr(functions, range.right(), &context.const_env)?,
                        ),
                    )
                    .ok()?
                    .checked_add(1)
                })
                .collect::<Option<Vec<_>>>()
        });
        let width = ty
            .and_then(|ty| expr_type_from_type(&tree.functions, ty, &context.const_env))
            .map(|ty| ty.width);
        Self {
            semantic_type: ty.and_then(|ty| {
                semantic_types::of_type(&mut session.types, functions, ty, &context.const_env)
            }),
            arguments: Some(arguments),
            width,
            first_packed_dimension_width: widths
                .as_ref()
                .and_then(|widths| widths.as_ref())
                .and_then(|widths| widths.first().copied())
                .or(width),
            // A chandle has no packed dimensions (IEEE 1800-2023 6.14).
            dimensions: ty.map(|ty| {
                ty.unpacked_ranges().len()
                    + if ty.chandle || matches!(ty.kind(), TypeKind::Real | TypeKind::Shortreal) {
                        0
                    } else {
                        ty.packed_ranges().len()
                    }
            }),
            signed: ty.is_some_and(Type::is_signed),
            is_2state: ty.is_some_and(|ty| ty.kind() == TypeKind::Bit),
            string: ty.is_some_and(|ty| ty.kind() == TypeKind::String),
            enum_domain: ty
                .filter(|ty| ty.unpacked_ranges.is_empty())
                .and_then(|ty| ty.enum_domain.clone()),
            virtual_interface: ty.and_then(|ty| ty.virtual_interface.clone()),
            chandle: ty.is_some_and(|ty| ty.chandle && ty.unpacked_ranges.is_empty()),
            pattern_type: ty
                .filter(|ty| {
                    !ty.members.is_empty()
                        || !ty.unpacked_ranges.is_empty()
                        || ty.packed_ranges.len() > 1
                })
                .and_then(|ty| {
                    ty.pattern_target(&const_functions::Environment {
                        constants: &*context.const_env,
                        functions,
                    })
                }),
        }
    }

    pub(super) fn from_function(function: &Function) -> Self {
        Self {
            semantic_type: function.return_semantic_type,
            arguments: Some(function_argument_metadata(&function.params)),
            width: function.return_width,
            first_packed_dimension_width: function.return_first_packed_dimension_width,
            dimensions: function.return_dimensions,
            signed: function.return_signed,
            is_2state: function.return_is_2state,
            string: function.return_string,
            chandle: function.return_chandle,
            pattern_type: function.return_type.clone(),
            enum_domain: function.return_enum_domain.clone(),
            virtual_interface: function.return_virtual_interface.clone(),
        }
    }

    pub(super) fn from_dpi(
        import: &DpiImport,
        table: &mut semantic_types::TyTable,
    ) -> Option<Self> {
        let ty = import.return_type()?;
        // DPI's supported scalar ABI types have evaluated, fixed widths.
        // chandle retains the existing representation until migration stage 7.
        let keyword = match ty {
            DpiType::Bit => Some("bit"),
            DpiType::Logic => Some("logic"),
            DpiType::Integer { width: 8, .. } => Some("byte"),
            DpiType::Integer { width: 16, .. } => Some("shortint"),
            DpiType::Integer { width: 32, .. } => Some("int"),
            DpiType::Integer { width: 64, .. } => Some("longint"),
            DpiType::String => Some("string"),
            DpiType::Chandle => Some("chandle"),
            DpiType::Integer { .. } => None,
        };
        let semantic_type = keyword
            .and_then(|name| type_binding::builtin_type(name, Some(ty.is_signed())))
            .and_then(|ty| {
                semantic_types::of_type(
                    table,
                    &const_functions::ConstantFunctions::default(),
                    &ty,
                    &HashMap::default(),
                )
            });
        Some(Self {
            semantic_type,
            arguments: Some(dpi_argument_metadata(import)),
            width: Some(ty.width()),
            first_packed_dimension_width: (ty.width() > 1).then_some(ty.width()),
            dimensions: Some(usize::from(ty.width() > 1 && ty != DpiType::Chandle)),
            signed: ty.is_signed(),
            is_2state: !ty.is_4state(),
            string: ty == DpiType::String,
            chandle: ty == DpiType::Chandle,
            pattern_type: None,
            enum_domain: None,
            virtual_interface: None,
        })
    }
}
