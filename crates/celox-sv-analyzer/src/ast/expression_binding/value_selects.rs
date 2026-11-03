//! Member, bit, part and indexed selects of a value of a known type, such as
//! a class property read through its handle or a container element.
//!
//! A select of a variable takes the bits that its layout places at the
//! selected positions (IEEE 1800-2023 7.2.1, 11.5.1). A value that is no
//! variable has the layout of its type: the selection of a value of type
//! `T` is the selection of a variable of type `T`, applied to the value.
//! The positions are computed from that layout alone, and the indices are
//! bound in the scope of the expression, so a select inside an index, of
//! another value or of the same one, is bound on its own.
use super::*;

/// One step of a select chain on a value: a member, or a select.
pub(in crate::ast) enum Step<'a> {
    Member(String),
    Select(&'a DimensionKind),
}

/// The members and selects that native expression `root` applies to its
/// subexpression `base`, innermost first, or `None` when another kind of
/// expression comes between them.
pub(in crate::ast) fn steps<'a>(
    root: ExprId,
    base: ExprId,
    native: &'a Declarations,
) -> Option<Vec<Step<'a>>> {
    let base = ungroup(base, native);
    let mut steps = Vec::new();
    let mut id = ungroup(root, native);
    while id != base {
        match &native.expression(id).kind {
            ExprKind::Select {
                expression,
                dimension,
            } => {
                steps.push(Step::Select(&dimension.kind));
                id = ungroup(*expression, native);
            }
            ExprKind::Member {
                expression,
                separator,
                name,
            } if separator == "." => {
                steps.push(Step::Member(instances::normalize_identifier(
                    &name.spelling,
                )));
                id = ungroup(*expression, native);
            }
            _ => return None,
        }
    }
    steps.reverse();
    Some(steps)
}

/// A selection of a value: the selection of a variable that holds it, the
/// checks of the tags its members cross, and the scope with its layout.
pub(in crate::ast) struct Selection {
    pub(in crate::ast) lvalue: LValue,
    pub(in crate::ast) guards: Vec<Expr>,
    pub(in crate::ast) scope: PackedDimensions,
}

/// The selection that `steps` make of a value of type `ty`, as a selection
/// of variable `key`, with `dimensions` extended by the layout of `key`.
/// A write assigns the selection of a variable `key` that holds the value.
/// Each member is accessed as a member of a variable is, so a member of a
/// tagged union, at any depth, is checked against the tag that `value`, the
/// value selected from, holds (IEEE 1800-2023 7.3.2).
#[allow(clippy::too_many_arguments)]
pub(in crate::ast) fn selection(
    key: &str,
    value: Expr,
    ty: &Type,
    steps: &[Step<'_>],
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Selection> {
    let mut scope = dimensions.clone();
    scope.insert(key.to_owned(), procedural::dimensions_from_type(ty));
    scope
        .expression_signedness
        .insert(key.to_owned(), ty.is_signed());
    let mut parts = vec![key.to_owned()];
    parts.extend(steps.iter().map_while(|step| match step {
        Step::Member(name) => Some(name.clone()),
        Step::Select(_) => None,
    }));
    let suffix = steps[parts.len() - 1..]
        .iter()
        .map(|step| match step {
            Step::Select(dimension) => Ok(*dimension),
            Step::Member(name) => Err(unsupported(format!(
                "member `{name}` of a select of a value"
            ))),
        })
        .collect::<Converted<Vec<_>>>()?;
    let guards = if parts.len() > 1 {
        packed_structs::member_guards(&parts, value, &scope)?
            .map(|(guards, _)| guards)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let lvalue = match (parts.len(), suffix.is_empty()) {
        (1, true) => LValue::Ident(key.to_owned()),
        (_, true) => packed_structs::whole_member(tree, parts, None, &scope)?,
        (1, false) => direct::select_suffix(
            key.to_owned(),
            &suffix,
            native,
            tree,
            session,
            &scope,
            false,
            false,
        )?,
        (_, false) => {
            packed_structs::select_member(tree, parts, None, &scope, |name, context, strict| {
                direct::select_suffix(name, &suffix, native, tree, session, context, strict, false)
            })?
        }
    };
    Ok(Selection {
        lvalue,
        guards,
        scope,
    })
}

/// The value that `steps` select of `value`, of type `ty`.
pub(in crate::ast) fn read(
    value: Expr,
    ty: &Type,
    steps: &[Step<'_>],
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    // `$` starts no identifier, so the key names no declaration.
    const KEY: &str = "$value";
    let selection = selection(
        KEY,
        value.clone(),
        ty,
        steps,
        native,
        tree,
        session,
        dimensions,
    )?;
    // A check reads the value once more.
    if !selection.guards.is_empty() && inlining::effectful(&value) {
        return Err(unsupported(
            "tagged union member of a value read with a side effect",
        ));
    }
    let read = statements::expr_from_selection(tree, value, &selection.lvalue, &selection.scope);
    packed_structs::checked_value(selection.guards, read, tree, &selection.scope)
}
