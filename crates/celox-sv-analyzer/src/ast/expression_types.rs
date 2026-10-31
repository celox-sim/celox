//! The self-determined types of native expressions, judged before they are
//! bound, as semantic types.
//!
//! Each kind of operand is resolved from what declares it: a variable from
//! its shape, a property from its class, a call from the type its function
//! returns, an element from the array it selects from. An operand that this
//! does not resolve yet has no type, and checks that need one skip it.

use super::*;
use celox_sv_syntax::{Declarations, DimensionKind, ExprId, ExprKind};
use expression_binding::{objects, ungroup};
use semantic_types::{Decl, TyData, TyId};

/// The type of native expression `id`.
pub(super) fn type_of(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
) -> Option<TyId> {
    let of_type = |session: &mut Session, ty: &Type| {
        semantic_types::of_type(&mut session.types, &tree.functions, ty, &dims.const_env)
    };
    let of_shape = |session: &mut Session, shape: &VariableDimensions| {
        semantic_types::of_shape(&mut session.types, &tree.functions, shape, &dims.const_env)
    };
    let class = |session: &mut Session, name: &str| {
        session
            .types
            .intern(TyData::Class(Decl::Class(name.into())))
    };
    let id = ungroup(id, native);
    if let ExprKind::Call { expression, .. } = &native.expression(id).kind {
        // `process::self()` (9.7).
        if let ExprKind::Member {
            expression: base,
            separator,
            name,
        } = &native.expression(*expression).kind
            && separator == "::"
            && name.spelling == "self"
            && processes::scope(tree, session, *base, native, dims)
        {
            return Some(class(session, processes::CLASS));
        }
        // What the function or method returns.
        let name = match objects::method(tree, session, *expression, native, dims) {
            Ok(Some((name, ..))) => name,
            Ok(None) => expression_binding::callable_name(tree, *expression, native)?,
            Err(_) => return None,
        };
        let returns = dims.function_return_types.get(&name)?;
        return match &returns.class {
            Some(name) => Some(class(session, name)),
            None if returns.string => Some(session.types.intern(TyData::String)),
            None => None,
        };
    }
    // A member of an unpacked structure variable.
    if let Some(name) = unpacked_structs::native_member(id, native, dims) {
        return of_shape(session, dims.get(&name)?);
    }
    // A member of a container element of unpacked structure type.
    if let Ok(Some((ty, _))) = containers::element_member(id, native, tree, session, dims) {
        return of_type(session, &ty);
    }
    match &native.expression(id).kind {
        ExprKind::Select {
            expression,
            dimension,
        } if matches!(dimension.kind, DimensionKind::Index(_)) => {
            if let Some(container) =
                containers::native_type(*expression, native, tree, session, dims)
                    .ok()
                    .flatten()
            {
                return of_type(session, &container.element);
            }
            let ExprKind::Name(name) = &native.expression(ungroup(*expression, native)).kind else {
                return None;
            };
            let shape = dims.get(&instances::normalize_identifier(&name.spelling))?;
            if let Some(container) = &shape.container {
                return of_type(session, &container.element);
            }
            let array = of_shape(session, shape)?;
            match session.types.data(array) {
                TyData::Array { element, .. } => Some(*element),
                _ => None,
            }
        }
        ExprKind::Name(name) => {
            let name = instances::normalize_identifier(&name.spelling);
            if let Some(shape) = dims.get(&name) {
                return of_shape(session, shape);
            }
            if name == "super" {
                let base = objects::super_class(tree, dims, session)?;
                return Some(class(session, &base));
            }
            property_type(tree, session, id, native, dims)
        }
        // The receiver of `h.p` is resolved once for both kinds of property,
        // so a chain `a.b.c` takes one walk.
        ExprKind::Member {
            expression: receiver,
            separator,
            name,
        } if separator == "."
            // Otherwise a member of a property of unpacked structure type,
            // found as a property below.
            && let Some(owner) = type_of(tree, session, *receiver, native, dims)
                .and_then(|receiver| class_name(&session.types, receiver)) =>
        {
            let member = instances::normalize_identifier(&name.spelling);
            let receiver = (!objects::is_super(*receiver, native)).then_some(*receiver);
            let ty = objects::member_type(tree, dims, &owner, &member, receiver, session)?;
            of_type(session, &ty)
        }
        _ => property_type(tree, session, id, native, dims),
    }
}

/// The type of the property that native expression `id` names: a static
/// property `h.p` or `C::p`, or an instance property.
fn property_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
) -> Option<TyId> {
    let ty = match objects::static_property_type(tree, id, native, dims, session).ok()? {
        Some(ty) => ty,
        None => {
            objects::property(tree, session, id, native, dims)
                .ok()??
                .0
                .r#type
        }
    };
    semantic_types::of_type(&mut session.types, &tree.functions, &ty, &dims.const_env)
}

/// The class of a handle of type `id`.
pub(super) fn class_name(table: &semantic_types::TyTable, id: TyId) -> Option<String> {
    match table.data(id) {
        TyData::Class(Decl::Class(name)) => Some(name.to_string()),
        _ => None,
    }
}
