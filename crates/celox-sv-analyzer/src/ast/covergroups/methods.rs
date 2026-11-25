//! The uses of a covergroup type: `new` and the predefined methods (IEEE
//! 1800-2023 19.3, 19.8, 19.9).

use super::*;
use crate::system_functions::{CoverageOp, CoverageQuery, ObjectOp};
use celox_sv_syntax::{Argument, ExprId, ExprKind};

/// The predefined methods of a covergroup, coverpoint or cross (Table
/// 19-5).
const METHODS: [&str; 6] = [
    "sample",
    "get_coverage",
    "get_inst_coverage",
    "set_inst_name",
    "start",
    "stop",
];

/// The option structures of a covergroup, coverpoint or cross (19.7).
const OPTIONS: [&str; 2] = ["option", "type_option"];

fn invalid(text: impl Into<String>) -> AnalyzerError {
    AnalyzerError::InvalidCovergroup(text.into())
}

/// The actuals of a call without the one empty actual that `f()` lists.
fn listed(arguments: &[Argument]) -> &[Argument] {
    match arguments {
        [argument] if argument.name.is_none() && argument.value.is_none() => &[],
        arguments => arguments,
    }
}

/// Record that the scope being analyzed uses the subroutines of `model`:
/// those of a covergroup a module or program declares are made only then,
/// so that a type nothing uses has none.
fn request(session: &mut Session, model: &Covergroup) {
    if session.covergroups.contains_key(&model.name)
        && !session.covergroup_requests.contains(&model.name)
    {
        session.covergroup_requests.push(model.name.clone());
    }
}

/// The call of the subroutine that registers the type of `model` and
/// returns its key.
fn type_key(model: &Covergroup) -> Expr {
    Expr::Call {
        callee: Callee::Subroutine(model.type_subroutine()),
        args: Vec::new(),
        arg_order: Vec::new(),
    }
}

/// The actuals `arguments` pass to `formals` of subroutine `name`, after
/// its first `leading` formals, which the caller passes: bound, with `None`
/// for an omitted one that has a default, and the order they are evaluated
/// in (13.5).
fn actuals(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    native: &Declarations,
    dimensions: &PackedDimensions,
    name: &str,
    formals: &[Formal],
    leading: usize,
    arguments: &[Argument],
) -> Converted<(Vec<Option<Expr>>, Vec<usize>)> {
    let names = formals
        .iter()
        .map(|formal| Some(formal.name.clone()))
        .collect::<Vec<_>>();
    let (values, order) = expression_binding::formal_arguments(name, arguments, &names)?;
    let mut bound = Vec::with_capacity(values.len());
    for (position, (value, formal)) in values.into_iter().zip(formals).enumerate() {
        let Some(value) = value else {
            if formal.default.is_none() {
                return Err(AnalyzerError::InvalidCallArguments {
                    subroutine: name.to_owned(),
                    detail: format!(
                        "no actual for argument `{}`, which has no default (IEEE 1800-2023 13.5.3)",
                        formal.name
                    ),
                });
            }
            bound.push(None);
            continue;
        };
        expression_binding::check_argument(
            name,
            position + leading,
            position + 1,
            value,
            native,
            tree,
            dimensions,
            session,
        )?;
        let target = expression_binding::copied_in_target(
            tree,
            session,
            dimensions,
            name,
            position + leading,
        );
        bound.push(Some(expression_binding::bind_value(
            value, native, tree, session, dimensions, target,
        )?));
    }
    Ok((
        bound,
        (0..leading)
            .chain(order.into_iter().map(|index| index + leading))
            .collect(),
    ))
}

/// `new`, `new()` or `new(arguments)` of covergroup type `class_name`
/// (19.3): a call of the subroutine that constructs an instance. An
/// embedded covergroup is constructed in the `new` method of its class
/// alone, with the object as the first actual (19.4).
pub(in crate::ast) fn construct(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    arguments: &[Argument],
    native: &Declarations,
    dimensions: &PackedDimensions,
    class_name: &str,
) -> Converted<Expr> {
    let model = model(
        tree,
        session,
        dimensions,
        class_name,
        dimensions.class_scope.as_deref(),
    )?;
    if let Some(owner) = &model.owner
        && !(dimensions.constructor && dimensions.class_scope.as_deref() == Some(owner.as_str()))
    {
        return Err(AnalyzerError::CovergroupAssignment {
            owner: owner.clone(),
            name: model.identifier.clone(),
        });
    }
    let model = model.usable()?;
    request(session, model);
    let name = model.new_subroutine();
    let leading = usize::from(model.owner.is_some());
    let (mut args, arg_order) = actuals(
        tree,
        session,
        native,
        dimensions,
        &name,
        &model.new_formals,
        leading,
        listed(arguments),
    )?;
    if leading == 1 {
        args.insert(0, Some(expression_binding::object_this()));
    }
    Ok(Expr::Call {
        callee: Callee::Subroutine(name),
        args,
        arg_order,
    })
}

/// What a use of a covergroup names it with.
enum Receiver {
    /// `R.m`: an expression of the handle.
    Handle(ExprId),
    /// `T::m`: the type.
    Type,
}

/// A use of a predefined member of a covergroup.
struct Use<'n> {
    model: Arc<Covergroup>,
    receiver: Receiver,
    /// A coverpoint or cross between the covergroup and the member, as in
    /// `R.cp.m`.
    item: bool,
    member: String,
    /// The actuals of a call, `None` for a member that is not called.
    arguments: Option<&'n [Argument]>,
}

/// The use of a predefined member of a covergroup that native expression
/// `id` is: `R.m`, `R.item.m`, `T::m` or `T::item::m`, called or not, where
/// `m` is a predefined method or follows `option` or `type_option`. None
/// for any other expression, whose receiver is then not looked at.
fn predefined<'n>(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &'n Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<Use<'n>>> {
    let id = expression_binding::ungroup(id, native);
    let (callee, arguments) = match &native.expression(id).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => (
            expression_binding::ungroup(*expression, native),
            Some(arguments.as_slice()),
        ),
        _ => (id, None),
    };
    let ExprKind::Member {
        expression: receiver,
        separator,
        name,
    } = &native.expression(callee).kind
    else {
        return Ok(None);
    };
    let mut receiver = *receiver;
    let spelled = |name: &celox_sv_syntax::Name| instances::normalize_identifier(&name.spelling);
    let mut member = spelled(name);
    let mut separator = separator.as_str();
    // `R.option.m` and `T::type_option.m`: the member is the option
    // structure, which the separator before it names the covergroup with.
    if let ExprKind::Member {
        expression,
        separator: inner,
        name,
    } = &native.expression(receiver).kind
        && OPTIONS.contains(&name.spelling.as_str())
    {
        member = spelled(name);
        receiver = *expression;
        separator = inner.as_str();
    } else if !METHODS.contains(&member.as_str()) && !OPTIONS.contains(&member.as_str()) {
        return Ok(None);
    }
    let class_of =
        |session: &mut Session, id: ExprId| -> Converted<Option<(String, Option<String>)>> {
            if separator == "::" {
                return Ok(
                    expression_binding::named_class(tree, session, id, native, dimensions)
                        .ok()
                        .flatten()
                        .map(|class| (class, None)),
                );
            }
            let Some(class) =
                expression_binding::object_native_class(tree, session, id, native, dimensions)
            else {
                return Ok(None);
            };
            // The class that holds an embedded covergroup: that of the object
            // `e` of `e.cg`, or the one whose method names `cg`.
            let owner = match &native
                .expression(expression_binding::ungroup(id, native))
                .kind
            {
                ExprKind::Member {
                    expression,
                    separator,
                    ..
                } if separator == "." => expression_binding::object_native_class(
                    tree,
                    session,
                    *expression,
                    native,
                    dimensions,
                ),
                _ => dimensions.class_scope.clone(),
            };
            Ok(Some((class, owner)))
        };
    let mut item = false;
    let mut found = class_of(session, receiver)?.filter(|(class, _)| is_class(class));
    if found.is_none()
        && let ExprKind::Member {
            expression,
            separator: inner,
            ..
        } = &native
            .expression(expression_binding::ungroup(receiver, native))
            .kind
        && inner == separator
    {
        // `R.item.m`.
        found = class_of(session, *expression)?.filter(|(class, _)| is_class(class));
        item = found.is_some();
        if item {
            receiver = *expression;
        }
    }
    let Some((class, owner)) = found else {
        return Ok(None);
    };
    let model = model(tree, session, dimensions, &class, owner.as_deref())?;
    Ok(Some(Use {
        model,
        receiver: if separator == "::" {
            Receiver::Type
        } else {
            Receiver::Handle(receiver)
        },
        item,
        member,
        arguments,
    }))
}

/// The object that holds the embedded covergroup that handle expression
/// `handle` names: `this` for `cg` in a method of its class, `e` for `e.cg`.
fn owner_object(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    handle: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    match &native
        .expression(expression_binding::ungroup(handle, native))
        .kind
    {
        ExprKind::Name(_) => Ok(expression_binding::object_this()),
        ExprKind::Member {
            expression,
            separator,
            ..
        } if separator == "." => {
            expression_binding::bind_value(*expression, native, tree, session, dimensions, None)
        }
        _ => Err(through_expression()),
    }
}

/// What a use of a predefined member that is not supported, or that the
/// standard excludes, is rejected with; `Ok` for one that binds.
fn check(found: &Use<'_>) -> Converted<()> {
    let model = found.model.usable()?;
    let member = found.member.as_str();
    if OPTIONS.contains(&member) {
        return Err(unsupported(format!(
            "covergroup option (`{member}` of `{}`; IEEE 1800-2023 19.7)",
            model.display
        )));
    }
    let scoped = matches!(found.receiver, Receiver::Type);
    // Only `get_coverage` is a static method (19.8).
    if scoped && member != "get_coverage" {
        return Err(invalid(format!(
            "`{member}` of covergroup `{}` called with `::`: it is not a static method (IEEE 1800-2023 19.8)",
            model.display
        )));
    }
    if found.item {
        // A coverpoint or cross has neither (Table 19-5).
        if matches!(member, "sample" | "set_inst_name") {
            return Err(invalid(format!(
                "`{member}` called on a coverpoint or cross of covergroup `{}` (IEEE 1800-2023 19.8)",
                model.display
            )));
        }
        return Err(unsupported(
            "covergroup coverpoint or cross method (IEEE 1800-2023 19.8)",
        ));
    }
    if matches!(member, "start" | "stop" | "set_inst_name") {
        return Err(unsupported(format!(
            "covergroup method `{member}` (IEEE 1800-2023 19.8)"
        )));
    }
    Ok(())
}

/// A call of `sample` on a covergroup as a statement (19.8, 19.8.1): the
/// subroutine that samples its type with its actuals and their evaluation
/// order. Its first actual is the handle, or for an embedded covergroup the
/// object that holds it (19.4). None for another statement.
pub(in crate::ast) fn sample_statement(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<(String, Vec<Option<Expr>>, Vec<usize>)>> {
    let Some(found) = predefined(tree, session, id, native, dimensions)? else {
        return Ok(None);
    };
    if found.member != "sample" {
        return Ok(None);
    }
    check(&found)?;
    let Receiver::Handle(handle) = found.receiver else {
        return Ok(None);
    };
    let model = &*found.model;
    request(session, model);
    let name = model.sample_subroutine();
    let first = match model.owner {
        Some(_) => owner_object(tree, session, handle, native, dimensions)?,
        None => expression_binding::bind_value(handle, native, tree, session, dimensions, None)?,
    };
    let (mut args, arg_order) = actuals(
        tree,
        session,
        native,
        dimensions,
        &name,
        &model.sample_formals,
        1,
        listed(found.arguments.unwrap_or_default()),
    )?;
    args.insert(0, Some(first));
    Ok(Some((name, args, arg_order)))
}

/// The value of a use of a predefined member of a covergroup (19.8):
/// `R.get_inst_coverage()`, `R.get_coverage()` or `T::get_coverage()`. A
/// use the analysis does not support, or one the standard excludes, is an
/// error; `statement` tells a use whose value is discarded. None for an
/// expression that is no such use.
pub(in crate::ast) fn value(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    statement: bool,
) -> Converted<Option<Expr>> {
    let Some(found) = predefined(tree, session, id, native, dimensions)? else {
        return Ok(None);
    };
    check(&found)?;
    let model = &*found.model;
    let member = found.member.as_str();
    if member == "sample" {
        if statement {
            // `sample_statement` binds it.
            return Ok(None);
        }
        return Err(invalid(format!(
            "the sample method of covergroup `{}` returns no value (IEEE 1800-2023 19.8)",
            model.display
        )));
    }
    if !listed(found.arguments.unwrap_or_default()).is_empty() {
        return Err(unsupported(format!(
            "covergroup method `{member}` with arguments (IEEE 1800-2023 19.8)"
        )));
    }
    request(session, model);
    let covergroup = Expr::Literal("32'd0".into());
    Ok(Some(match (member, found.receiver) {
        ("get_inst_coverage", Receiver::Handle(handle)) => classes::object_call(
            ObjectOp::Coverage(CoverageOp::InstCoverage {
                what: CoverageQuery::Real,
            }),
            vec![
                expression_binding::bind_value(handle, native, tree, session, dimensions, None)?,
                covergroup,
            ],
        ),
        // The coverage of the type, which a handle only names: it is not
        // read, so `null` is no error.
        _ => classes::object_call(
            ObjectOp::Coverage(CoverageOp::TypeCoverage {
                what: CoverageQuery::Real,
            }),
            vec![type_key(model), covergroup],
        ),
    }))
}

/// The type of the value of native expression `id` when it is a coverage
/// query of a covergroup, which is a `real` (19.8).
pub(in crate::ast) fn result_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Option<semantic_types::TyId> {
    let callee = match &native.expression(id).kind {
        ExprKind::Call { expression, .. } => expression_binding::ungroup(*expression, native),
        ExprKind::Member { .. } => id,
        _ => return None,
    };
    let ExprKind::Member { name, .. } = &native.expression(callee).kind else {
        return None;
    };
    if !matches!(name.spelling.as_str(), "get_coverage" | "get_inst_coverage") {
        return None;
    }
    predefined(tree, session, id, native, dimensions)
        .ok()
        .flatten()
        .map(|_| session.types.intern(semantic_types::TyData::Real))
}

/// `$get_coverage()` (19.9): the coverage of every covergroup type of the
/// design.
pub(in crate::ast) fn overall() -> Expr {
    classes::object_call(ObjectOp::Coverage(CoverageOp::Overall), Vec::new())
}
