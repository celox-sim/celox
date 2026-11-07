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
