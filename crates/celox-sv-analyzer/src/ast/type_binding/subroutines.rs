//! Semantic formal types owned by native subroutine declarations.
use super::*;
use crate::procedural::ParamDirection;

pub(in crate::ast) fn subroutine_formals(
    native: &celox_sv_syntax::Subroutine,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<
    Vec<(
        String,
        ParamDirection,
        Type,
        Option<celox_sv_syntax::ExprId>,
    )>,
> {
    let mut result = Vec::new();
    let mut previous_type: Option<Type> = None;
    let mut direction = ParamDirection::Input;
    for port in native.ports.iter().flatten().chain(
        native
            .port_declarations
            .iter()
            .flat_map(|group| &group.ports),
    ) {
        if let Some(declared) = port.direction {
            if port.static_reference {
                return Err(unsupported("ref static subroutine argument"));
            }
            direction = ParamDirection::declared(declared, port.constant);
        }
        let Some(name) = &port.name else { continue };
        let data = tree.declarations().data_type(port.data_type);
        let omitted = matches!(data.kind, celox_sv_syntax::DataTypeKind::Implicit)
            && data.signed.is_none()
            && data.packed_dimensions.is_empty();
        let mut ty = if port.direction.is_none() && omitted {
            previous_type.clone().unwrap_or(Type::new(TypeKind::Logic))
        } else {
            bind(port.data_type, tree, context)?.ok_or_else(|| {
                unsupported(format!(
                    "unsupported {} formal data type",
                    if native.kind == celox_sv_syntax::SubroutineKind::Function {
                        "function"
                    } else {
                        "task"
                    }
                ))
            })?
        };
        if ty.kind == TypeKind::Implicit {
            ty.kind = TypeKind::Logic;
        }
        previous_type = Some(ty.clone());
        // A container formal that is not a reference holds a copy of the
        // container (IEEE 1800-2023 13.5.1), as an assignment makes one.
        ty = containers::with_dimensions(ty, &port.dimensions, tree, context)?;
        result.push((
            instances::normalize_identifier(&name.spelling),
            direction,
            ty,
            port.default,
        ));
    }
    Ok(result)
}
