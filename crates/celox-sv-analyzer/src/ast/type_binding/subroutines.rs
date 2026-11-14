//! Semantic formal types owned by native subroutine declarations.
use super::*;
use crate::procedural::ParamDirection;

pub(in crate::ast) fn subroutine_formals(
    native: &celox_sv_syntax::Subroutine,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) -> Converted<
    Vec<(
        String,
        ParamDirection,
        Type,
        Option<celox_sv_syntax::ExprId>,
    )>,
> {
    let what = if native.kind == celox_sv_syntax::SubroutineKind::Function {
        "function"
    } else {
        "task"
    };
    port_formals(
        native.ports.iter().flatten().chain(
            native
                .port_declarations
                .iter()
                .flat_map(|group| &group.ports),
        ),
        what,
        tree,
        session,
        context,
    )
}

/// The formals `ports` of a `what` (a function, task or randsequence
/// production): `(name, direction, type, default)`. A formal without a
/// direction takes the previous one's, and one without a type the previous
/// one's type (IEEE 1800-2023 13.3).
pub(in crate::ast) fn port_formals<'a>(
    ports: impl IntoIterator<Item = &'a celox_sv_syntax::SubroutinePort>,
    what: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
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
    for port in ports {
        if let Some(declared) = port.direction {
            if port.static_reference {
                return Err(unsupported("ref static subroutine argument"));
            }
            direction = ParamDirection::declared(declared, port.constant);
        }
        // A prototype's lone identifier that names no type is the formal's
        // name, its type omitted.
        let renamed = classes::prototype_formal_name(port, tree, session, context);
        let name = match (&port.name, renamed.as_ref()) {
            (Some(name), _) => instances::normalize_identifier(&name.spelling),
            (None, Some(name)) => name.clone(),
            (None, None) => continue,
        };
        let data = tree.declarations().data_type(port.data_type);
        let omitted = renamed.is_some()
            || matches!(data.kind, celox_sv_syntax::DataTypeKind::Implicit)
                && data.signed.is_none()
                && data.packed_dimensions.is_empty();
        let mut ty = if port.direction.is_none() && omitted {
            previous_type.clone().unwrap_or(Type::new(TypeKind::Logic))
        } else if renamed.is_some() {
            Type::new(TypeKind::Logic)
        } else {
            bind(port.data_type, tree, session, context)?
                .ok_or_else(|| unsupported(format!("unsupported {what} formal data type")))?
        };
        if ty.kind == TypeKind::Implicit {
            ty.kind = TypeKind::Logic;
        }
        previous_type = Some(ty.clone());
        // A container formal that is not a reference holds a copy of the
        // container (IEEE 1800-2023 13.5.1), as an assignment makes one.
        ty = containers::with_dimensions(ty, &port.dimensions, tree, session, context)?;
        semantic_types::bind_declared(
            &mut session.types,
            &tree.functions,
            &mut ty,
            &context.const_env,
        );
        result.push((name, direction, ty, port.default));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn subroutine_interfaces_keep_the_types_of_their_declarations() {
        // IEEE 1800-2023 6.18, 23.9: the names in a type resolve in the scope
        // that declares it. A formal declared where `W` is 4, an implicit
        // result type and a class handle are bound as they are built, so a
        // reader that does not see `W` gets the formal's declared type, also
        // once the body's constants are substituted into it.
        let session = &mut Session::default();
        let tree = crate::source_tree::parse_source(
            "module M; class C; endclass
             function automatic logic [W-1:0] f(input logic [W-1:0] a [2]); endfunction
             endmodule",
            Path::new("subroutine-interface.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let native = &tree.declarations().subroutines[0];
        let declaring = ConstEnv::from_iter([("W".into(), 4)]);
        let context =
            PackedDimensions::new(HashMap::default(), &declaring, &TypeAliases::default());
        let formals = subroutine_formals(native, &tree, session, &context).unwrap();
        let formal = &formals[0].2;
        let declared =
            semantic_types::of_type(&mut session.types, &tree.functions, formal, &declaring)
                .unwrap();
        let scoped = procedural::scoped_type(formal.clone(), &declaring);
        for ty in [formal, &scoped] {
            assert_eq!(
                semantic_types::of_type(
                    &mut session.types,
                    &tree.functions,
                    ty,
                    &ConstEnv::default()
                ),
                Some(declared)
            );
        }
        let result = implicit_subroutine_type(
            native.return_type.unwrap(),
            &tree,
            session,
            &declaring,
            &TypeAliases::default(),
        )
        .unwrap();
        assert!(result.binding.get(&session.types).is_some());
        let handle = classes::handle_type(&tree, session, "C").unwrap();
        assert!(handle.binding.get(&session.types).is_some());
    }
}
