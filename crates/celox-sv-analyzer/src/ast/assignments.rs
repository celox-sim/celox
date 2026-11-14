//! The one check of a value given to a typed place (IEEE 1800-2023 6.22.3,
//! 10.8): an assignment, an argument, a return value, a default or an item
//! of an assignment pattern. The value's own type and the place's type
//! decide, by [`semantic_types::TyTable::assignable`].

use super::*;
use celox_sv_syntax::{Declarations, ExprId, ExprKind};

/// Check that native expression `id` may be assigned to a place of type
/// `target`. A conditional whose arms have no common type (11.4.11) is
/// judged by its arms, either of which may be the value; a value an effect
/// bound in its place, such as the handle a `$cast` to a property writes, is
/// checked where it was made; and a value whose type is not resolved before
/// binding is not judged here.
pub(super) fn check_assign(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
    target: semantic_types::TyId,
    context: impl Fn() -> String,
) -> Converted<()> {
    let mut pending = vec![id];
    while let Some(id) = pending.pop() {
        if dims.effect_values.contains_key(&id) {
            continue;
        }
        let Ok(source) = expression_types::type_of(tree, session, id, native, dims) else {
            if let ExprKind::Conditional {
                then_expression,
                else_expression,
                ..
            } = &native
                .expression(expression_binding::ungroup(id, native))
                .kind
            {
                pending.extend([*else_expression, *then_expression]);
            }
            continue;
        };
        check_types(tree, session, dims, source, target, &context)?;
    }
    Ok(())
}

/// Check that a value of type `source` may be assigned to a place of type
/// `target` (see [`check_assign`]).
pub(super) fn check_types(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
    source: semantic_types::TyId,
    target: semantic_types::TyId,
    context: &dyn Fn() -> String,
) -> Converted<()> {
    // The supertypes of a class are recorded once it is first assigned
    // from; a built-in class has none.
    if let Some(class) = expression_types::class_name(&session.types, source)
        && !classes::builtin(&class)
    {
        expression_binding::object_class(tree, session, dims, &class)?.record_lineage(
            tree,
            session,
            &dims.const_env,
            &dims.type_aliases,
        );
    }
    if session.types.assignable(source, target).is_ok() {
        return Ok(());
    }
    Err(diagnostic(session, source, target, context))
}

/// The diagnostic of a value of type `source` that is not assignment
/// compatible with type `target`.
fn diagnostic(
    session: &Session,
    source: semantic_types::TyId,
    target: semantic_types::TyId,
    context: &dyn Fn() -> String,
) -> AnalyzerError {
    use semantic_types::TyData;
    let table = &session.types;
    match (table.data(source), table.data(target)) {
        (TyData::Class(_), TyData::Class(_)) => AnalyzerError::IncompatibleClassHandle {
            context: context(),
            actual: expression_types::class_name(table, source)
                .unwrap_or_else(|| table.spell(source)),
            target: expression_types::class_name(table, target)
                .unwrap_or_else(|| table.spell(target)),
        },
        (_, TyData::VirtualInterface { .. }) => AnalyzerError::IncompatibleVirtualInterface {
            context: context(),
            actual: format!("a `{}`", table.spell(source)),
            target: table.spell(target),
        },
        (_, TyData::Array { .. }) => AnalyzerError::IncompatibleUnpackedArray {
            context: context(),
            actual: table.spell(source),
            target: table.spell(target),
        },
        (_, TyData::Class(_)) => AnalyzerError::NotClassHandle {
            context: context(),
            actual: table.spell(source),
            target: expression_types::class_name(table, target)
                .unwrap_or_else(|| table.spell(target)),
        },
        _ => AnalyzerError::IncompatibleAssignment {
            context: context(),
            actual: table.spell(source),
            target: table.spell(target),
        },
    }
}

/// Reject operator `op`, a compound assignment or an increment or
/// decrement, updating native expression `target` of an enumerated type:
/// its result is of the enumeration's base type, which the variable takes
/// only by a cast (IEEE 1800-2023 6.19.3, 6.19.4).
pub(super) fn check_enum_update(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    target: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
    op: &str,
) -> Converted<()> {
    let Some(ty) = place_type(tree, session, target, native, dims)? else {
        return Ok(());
    };
    if !session.types.is_enum(ty) {
        return Ok(());
    }
    let base = enum_base(session, ty);
    Err(AnalyzerError::IncompatibleAssignment {
        context: format!("operator `{op}`"),
        actual: session.types.spell(base),
        target: session.types.spell(ty),
    })
}

/// Check value `right` assigned to native expression `left` when `left` is
/// of an enumerated type: a variable, an element, a member or a property
/// of one takes only a value of that type, unless it is a member of a
/// union (IEEE 1800-2023 6.19.3).
pub(super) fn check_enum_target(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    left: ExprId,
    right: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
) -> Converted<()> {
    let Some(target) = place_type(tree, session, left, native, dims)? else {
        return Ok(());
    };
    if !session.types.is_enum(target) {
        return Ok(());
    }
    check_assign(tree, session, right, native, dims, target, || {
        "assignment".into()
    })
}

/// The type of the place native expression `id` writes: its own type, but
/// for an enumerated member of a union the bits of it, which take any value
/// of them (IEEE 1800-2023 6.19.3). Every write to a place, an assignment,
/// a compound update or an argument copied out, is judged against it.
pub(super) fn place_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
) -> Converted<Option<semantic_types::TyId>> {
    let Ok(ty) = expression_types::type_of(tree, session, id, native, dims) else {
        return Ok(None);
    };
    if session.types.is_enum(ty) && union_member(tree, session, id, native, dims)? {
        return Ok(Some(enum_base(session, ty)));
    }
    Ok(Some(ty))
}

/// The packed type of the bits of enumerated type `ty`.
fn enum_base(session: &mut Session, ty: semantic_types::TyId) -> semantic_types::TyId {
    let semantic_types::TyData::Packed(packed) = session.types.data(ty).clone() else {
        return ty;
    };
    session
        .types
        .intern(semantic_types::TyData::Packed(semantic_types::Packed {
            element: semantic_types::PackedElement::Bits,
            keyword: None,
            ..packed
        }))
}

/// Whether native expression `id` is a member of a packed union: its
/// members all start at bit 0, or are tagged (7.3).
pub(super) fn union_member(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
) -> Converted<bool> {
    let ExprKind::Member {
        expression: receiver,
        ..
    } = &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    else {
        return Ok(false);
    };
    // A container element of unpacked structure type, such as `q[i][j]` of
    // a queue of queues, whose shape the operand's does not describe.
    if containers::native_struct_element(*receiver, native, tree, session, dims)?.is_some() {
        return Ok(false);
    }
    let Some(shape) = dimensions::operand_shape(*receiver, tree, session, dims)? else {
        return Ok(false);
    };
    let members = &shape.members;
    Ok(members.iter().any(packed_structs::PackedMember::is_tagged)
        || members.len() > 1 && members.iter().all(|member| member.offset() == 0))
}
