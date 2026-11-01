//! The self-determined types of native expressions, judged before they are
//! bound, as semantic types.
//!
//! Each kind of operand is resolved from what declares it: a variable from
//! its shape, a property from its class, a call from the type its function
//! returns, an element from the array it selects from. An operand that this
//! does not resolve yet reports an unsupported query. Legacy classification
//! consumers explicitly retain their existing fallback until they migrate.

use super::*;
use celox_sv_syntax::{Declarations, DimensionKind, ExprId, ExprKind};
use expression_binding::{objects, ungroup};
use semantic_types::{ArrayKind, Decl, PackedElement, Range, TyData, TyId};

mod builtins;
mod memo;
pub(super) use memo::Memo;
#[cfg(test)]
mod tests;

/// The type of native expression `id`.
pub(super) fn type_of(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
) -> Converted<TyId> {
    if let Some(ty) = session
        .expression_types
        .get(tree, session, id, native, dims)
    {
        return Ok(ty);
    }
    let ty = resolve(tree, session, id, native, dims)
        .ok_or_else(|| unsupported("semantic type of native expression"))?;
    let version = (
        session.foreign.revision(),
        session.class_specializations.len(),
    );
    session
        .expression_types
        .insert(tree, id, native, dims, version, ty);
    Ok(ty)
}

fn resolve(
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
    // Index chains are walked iteratively, so deeply nested arrays do not
    // consume the host stack. Each select removes one type-tree layer (7.4.5).
    if matches!(native.expression(id).kind, ExprKind::Select { .. }) {
        let mut base = id;
        let mut selections = Vec::new();
        while let ExprKind::Select {
            expression,
            dimension,
        } = &native.expression(base).kind
        {
            selections.push(&dimension.kind);
            base = ungroup(*expression, native);
        }
        let mut ty = type_of(tree, session, base, native, dims).ok()?;
        for selection in selections.into_iter().rev() {
            ty = select(tree, session, ty, selection, native, dims)?;
        }
        return Some(ty);
    }
    if let Some(ty) = builtins::result(tree, session, id, native, dims) {
        return Some(ty);
    }
    if let ExprKind::Call { expression, .. } = &native.expression(id).kind {
        let expression = *expression;
        // `process::self()` (9.7).
        if let ExprKind::Member {
            expression: base,
            separator,
            name,
        } = &native.expression(expression).kind
            && separator == "::"
            && name.spelling == "self"
            && processes::scope(tree, session, *base, native, dims)
        {
            return Some(class(session, processes::CLASS));
        }
        return function_result(tree, session, expression, native, dims);
    }
    // A member of an unpacked structure variable.
    if let Some(name) = unpacked_structs::native_member(id, native, dims) {
        return of_shape(session, dims.get(&name)?);
    }
    // A member of a container element of unpacked structure type.
    if let Ok(Some((ty, _))) = containers::element_member(id, native, tree, session, dims) {
        return of_type(session, &ty);
    }
    if let Some(name) = expression_binding::callable_name(tree, id, native)
        && let Some(shape) = dims.get(&name)
    {
        return of_shape(session, shape);
    }
    match &native.expression(id).kind {
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
                .ok().and_then(|receiver| class_name(&session.types, receiver)) =>
        {
            let member = instances::normalize_identifier(&name.spelling);
            let through_super = objects::is_super(*receiver, native);
            let handle = (!through_super).then_some(*receiver);
            if let Some(ty) = objects::member_type(tree, dims, &owner, &member, handle, session) {
                return of_type(session, &ty);
            }
            // A method called without parentheses (13.5.5).
            let receiver = match handle {
                Some(handle) => objects::Receiver::Handle(handle),
                None => objects::Receiver::This,
            };
            let (name, ..) = objects::method_of(
                tree,
                session,
                dims,
                &owner,
                &member,
                receiver,
                through_super,
            )
            .ok()??;
            named_function_result(tree, session, &name, dims)
        }
        // A static property `C::p`, or a static method called without
        // parentheses (13.5.5).
        ExprKind::Member { separator, .. } if separator == "::" => {
            property_type(tree, session, id, native, dims).or_else(|| {
                let (name, ..) = objects::method(tree, session, id, native, dims).ok()??;
                named_function_result(tree, session, &name, dims)
            })
        }
        _ => property_type(tree, session, id, native, dims),
    }
}

/// The declared result of an ordinary function or class method, given its
/// callee rather than its argument expressions. Legacy consumers of function
/// metadata use this same query while they migrate to full expression types.
pub(super) fn function_result(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    callee: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
) -> Option<TyId> {
    let name = match objects::method(tree, session, callee, native, dims) {
        Ok(Some((name, ..))) => name,
        Ok(None) => expression_binding::callable_name(tree, callee, native)?,
        Err(_) => return None,
    };
    named_function_result(tree, session, &name, dims)
}

/// The result of an already resolved subroutine, including a method called
/// without parentheses. Both call forms consult the same signature metadata.
fn named_function_result(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
    dims: &PackedDimensions,
) -> Option<TyId> {
    if let Some(returns) = dims.function_return_types.get(name) {
        if let Some(ty) = returns.semantic_type {
            return Some(ty);
        }
        if let Some(ty) = returns.pattern_type.as_ref() {
            return semantic_types::of_type(
                &mut session.types,
                &tree.functions,
                ty,
                &dims.const_env,
            );
        }
        if let Some(name) = &returns.class {
            return Some(
                session
                    .types
                    .intern(TyData::Class(Decl::Class(name.as_str().into()))),
            );
        }
        if returns.string {
            return Some(session.types.intern(TyData::String));
        }
    }
    // Older bound-function contexts have no signature entry yet. Preserve
    // their string-result query until every registry carries semantic types.
    dims.functions
        .get(name)
        .is_some_and(|function| function.return_string)
        .then(|| session.types.intern(TyData::String))
}

/// Self-determined selection types, independent of an assignment context
/// (IEEE 1800-2023 7.4.5, 7.4.6 and 11.5.1).
fn select(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    base: TyId,
    selection: &DimensionKind,
    native: &Declarations,
    dims: &PackedDimensions,
) -> Option<TyId> {
    let data = session.types.data(base).clone();
    // The parser represents a bare `$` index as a queue dimension. It
    // selects one element just like an ordinary index (7.10, 7.10.1),
    // without needing the queue's runtime length, even when it is empty.
    if matches!(selection, DimensionKind::Queue(None)) {
        return match data {
            TyData::Array {
                kind: ArrayKind::Queue { .. },
                element,
            } => Some(element),
            _ => None,
        };
    }
    if matches!(selection, DimensionKind::Index(_)) {
        return match data {
            TyData::Array { element, .. } => Some(element),
            // A string character has type byte, as does getc (6.16,
            // Table 6-9). The index and runtime string length do not
            // affect its self-determined type.
            TyData::String => semantic_types::of_type(
                &mut session.types,
                &tree.functions,
                &type_binding::builtin_type("byte", None)?,
                &dims.const_env,
            ),
            TyData::Packed(mut packed) if !packed.dims.is_empty() => {
                // The legacy packed representation has no named-element
                // boundary. Do not guess where an enum/aggregate element ends.
                if !matches!(packed.element, PackedElement::Bits) {
                    return None;
                }
                if packed.dims.len() == 1 {
                    packed.element = PackedElement::Bits;
                    packed.dims = Box::new([]);
                    packed.signed = false;
                    packed.signed_element_depth = None;
                } else {
                    packed.dims = packed.dims[1..].into();
                    packed.signed = packed.signed_element_depth == Some(1);
                    packed.signed_element_depth = packed
                        .signed_element_depth
                        .and_then(|depth| depth.checked_sub(1))
                        .filter(|depth| *depth != 0);
                }
                packed.keyword = None;
                Some(session.types.intern(TyData::Packed(packed)))
            }
            _ => None,
        };
    }
    // Queue range bounds may be runtime expressions, including `$` (7.10.1).
    // The result is a queue of the same elements regardless of its length;
    // querying its type must not bind or evaluate those bounds.
    if let TyData::Array {
        kind: ArrayKind::Queue { .. },
        element,
    } = &data
        // A leading `$` is represented by the parser's queue dimension.
        && matches!(
            selection,
            DimensionKind::Range { .. } | DimensionKind::Queue(Some(_))
        )
    {
        return Some(session.types.intern(TyData::Array {
            kind: ArrayKind::Queue { bound: None },
            element: *element,
        }));
    }
    // Constant binding still creates temporary runtime call identities. Keep
    // those and any discovery caches in a disposable session: only the value
    // of the bound expression may escape this query. Resolved declarations
    // and the semantic-table prefix retain the caller's identities.
    let mut constants = Session {
        foreign: session.foreign.clone(),
        classes: session.classes.clone(),
        class_declaration_scopes: session.class_declaration_scopes.clone(),
        class_declarations_repeated: session.class_declarations_repeated.clone(),
        class_specializations: session.class_specializations.clone(),
        class_specializations_frozen: true,
        virtual_interfaces: session.virtual_interfaces.clone(),
        types: session.types.clone(),
        ..Session::default()
    };
    let mut constant = |id| {
        let value = constant_binding::bind_in(id, native, tree, &mut constants, dims).ok()??;
        eval_ast_const_expr(&tree.functions, &value, &dims.const_env)
    };
    let count = match *selection {
        DimensionKind::Range { left, right } => {
            constant(left)?.abs_diff(constant(right)?).checked_add(1)?
        }
        DimensionKind::Indexed { width, .. } => u128::try_from(constant(width)?).ok()?,
        _ => return None,
    };
    let last = i128::try_from(count.checked_sub(1)?).ok()?;
    match data {
        TyData::Array {
            kind: ArrayKind::Associative { .. },
            ..
        } => None,
        TyData::Array {
            kind: ArrayKind::Queue { .. },
            element,
        } => Some(session.types.intern(TyData::Array {
            kind: ArrayKind::Queue { bound: None },
            element,
        })),
        TyData::Array { element, .. } => Some(session.types.intern(TyData::Array {
            kind: ArrayKind::Fixed(Range {
                left: 0,
                right: last,
            }),
            element,
        })),
        TyData::Packed(mut packed) if !packed.dims.is_empty() => {
            if packed.dims.len() == 1 {
                packed.element = PackedElement::Bits;
            }
            packed.dims[0] = Range {
                left: last,
                right: 0,
            };
            packed.signed = false;
            packed.keyword = None;
            Some(session.types.intern(TyData::Packed(packed)))
        }
        _ => None,
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
