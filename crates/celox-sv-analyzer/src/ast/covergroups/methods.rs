//! The uses of a covergroup type: `new` and the predefined methods (IEEE
//! 1800-2023 19.3, 19.8, 19.9).

use super::compile::{Level, Member, OptionType, procedural_member};
use super::model::count_formals;
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
        expression_binding::check_reference_actual(
            name,
            position + leading,
            value,
            native,
            tree,
            session,
            dimensions,
        )?;
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

/// `error` of an actual of a `ref` formal of `new` of `model`, which names
/// the subroutine that constructs an instance, as an error of the
/// covergroup: what 13.5.2 does not allow to pass by reference is an
/// invalid covergroup, and what the analysis does not support a rejection
/// of a covergroup construct.
fn reference_actual(error: AnalyzerError, model: &Covergroup) -> AnalyzerError {
    let name = model.new_subroutine();
    match error {
        AnalyzerError::InvalidRefArgument {
            subroutine,
            argument,
            detail,
        } if subroutine == name => invalid(format!(
            "the actual of ref argument `{argument}` of covergroup `{}`: {detail}",
            model.display
        )),
        AnalyzerError::Unsupported(text)
            if text.starts_with(&format!("ref argument of `{name}` ")) =>
        {
            let what = &text[format!("ref argument of `{name}` ").len()..];
            unsupported(format!(
                "covergroup ref argument {what} (IEEE 1800-2023 13.5.2, 19.3)"
            ))
        }
        AnalyzerError::Unsupported(text) if text.ends_with("as a ref or const ref argument") => {
            unsupported(format!(
                "covergroup ref argument bound to a {} (IEEE 1800-2023 13.5.2, 19.3)",
                text.trim_end_matches(" as a ref or const ref argument")
            ))
        }
        error => error,
    }
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
    construction(
        tree, session, arguments, native, dimensions, class_name, false,
    )
}

/// `new`, `new()` or `new(arguments)` of covergroup type `class_name` as
/// the initializer of a declaration of a module or a program, which may be
/// of a type with a clocking event (see `procedural::covergroup_sites`).
pub(in crate::ast) fn construct_at_site(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    arguments: &[Argument],
    native: &Declarations,
    dimensions: &PackedDimensions,
    class_name: &str,
) -> Converted<Expr> {
    construction(
        tree, session, arguments, native, dimensions, class_name, true,
    )
}

/// [`construct`], at a declaration that samples the instance at the
/// clocking event of its type if `site`.
fn construction(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    arguments: &[Argument],
    native: &Declarations,
    dimensions: &PackedDimensions,
    class_name: &str,
    site: bool,
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
    // An instance reads the variables of its `ref` actuals at each sample,
    // which only the declaration that constructs it names (see
    // `procedural::covergroup_sites`).
    if model.has_references() && !site {
        return Err(outside_a_site());
    }
    request(session, model);
    name_subroutines(session, model);
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
    )
    .map_err(|error| {
        in_covergroup_terms(reference_actual(error, model), &session.covergroup_names)
    })?;
    for (index, formal) in model.references() {
        if args[leading + index].is_none() {
            return Err(unsupported(format!(
                "covergroup ref argument `{}` that takes its default (IEEE 1800-2023 13.5.3, 19.3)",
                formal.name
            )));
        }
    }
    if leading == 1 {
        args.insert(0, Some(expression_binding::object_this()));
    }
    let construction = Expr::Call {
        callee: Callee::Subroutine(name),
        args,
        arg_order,
    };
    // An instance of a type with a clocking event is sampled at the event
    // (19.3): a declaration of a module or a program that constructs it has
    // a sampler of the scope (see `procedural::covergroup_sites`), and any
    // other `new` hands the instance to `N::$spawn`, which starts one.
    if model.event.is_none() || site {
        return Ok(construction);
    }
    if session.covergroups.contains_key(&model.name)
        && !session.covergroup_spawn_requests.contains(&model.name)
    {
        session.covergroup_spawn_requests.push(model.name.clone());
    }
    let mut args = vec![Some(construction)];
    if leading == 1 {
        args.insert(0, Some(expression_binding::object_this()));
    }
    Ok(Expr::Call {
        callee: Callee::Subroutine(model.spawn_subroutine()),
        arg_order: (0..args.len()).collect(),
        args,
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
    /// The coverpoint or cross between the covergroup and the member, `cp`
    /// of `R.cp.m`.
    item: Option<String>,
    /// Whether the member follows `type_option`, as in `R.type_option.m`,
    /// or `option`; `None` for a method.
    structure: Option<bool>,
    member: String,
    /// The actuals of a call, `None` for a member that is not called.
    arguments: Option<&'n [Argument]>,
}

impl Use<'_> {
    /// The item the use names, its number among those of the heap (0 for
    /// the covergroup) and its level. One the covergroup does not have is
    /// an error (19.5, 19.6).
    fn item(&self) -> Converted<(u32, Level)> {
        let Some(name) = &self.item else {
            return Ok((0, Level::Covergroup));
        };
        match self.model.item(name) {
            Some((item, cross)) => Ok((
                item,
                if cross {
                    Level::Cross
                } else {
                    Level::Coverpoint
                },
            )),
            None => Err(invalid(format!(
                "covergroup `{}` has no coverpoint or cross named `{name}` (IEEE 1800-2023 19.5, 19.6)",
                self.model.display
            ))),
        }
    }

    /// The structure the member follows, as source code names it.
    fn structure(&self) -> &'static str {
        match self.structure {
            Some(true) => "type_option",
            _ => "option",
        }
    }
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
    let member = spelled(name);
    let mut separator = separator.as_str();
    let mut structure = None;
    // `R.option.m` and `T::type_option.m`: the member is one of the option
    // structure, which the separator before it names the covergroup with.
    if let ExprKind::Member {
        expression,
        separator: inner,
        name,
    } = &native.expression(receiver).kind
        && OPTIONS.contains(&name.spelling.as_str())
    {
        structure = Some(name.spelling == "type_option");
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
    let mut item = None;
    let mut found = class_of(session, receiver)?.filter(|(class, _)| is_class(class));
    if found.is_none()
        && let ExprKind::Member {
            expression,
            separator: inner,
            name,
        } = &native
            .expression(expression_binding::ungroup(receiver, native))
            .kind
        && inner == separator
    {
        // `R.item.m`.
        found = class_of(session, *expression)?.filter(|(class, _)| is_class(class));
        if found.is_some() {
            item = Some(spelled(name));
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
        structure,
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

/// `error` of the actuals of subroutine `subroutine`, which predefined
/// method `method` of a covergroup calls, as an error of the method: the
/// subroutine is no name of the source.
fn as_method(error: AnalyzerError, subroutine: &str, method: &str) -> AnalyzerError {
    match error {
        AnalyzerError::InvalidCallArguments {
            subroutine: called,
            detail,
        } if called == subroutine => AnalyzerError::InvalidCallArguments {
            subroutine: method.to_owned(),
            detail,
        },
        AnalyzerError::InvalidRefArgument {
            subroutine: called,
            argument,
            detail,
        } if called == subroutine => AnalyzerError::InvalidRefArgument {
            subroutine: method.to_owned(),
            argument,
            detail,
        },
        error => error,
    }
}

/// What a use of a predefined member that is not supported, or that the
/// standard excludes, is rejected with; `Ok` for one that binds.
fn check(found: &Use<'_>) -> Converted<()> {
    let model = found.model.usable()?;
    let member = found.member.as_str();
    if found.structure.is_none() && OPTIONS.contains(&member) {
        return Err(unsupported(format!(
            "covergroup option structure used as a whole (`{member}` of `{}`; IEEE 1800-2023 19.10)",
            model.display
        )));
    }
    let scoped = matches!(found.receiver, Receiver::Type);
    if let Some(type_option) = found.structure {
        // `type_option` is "a built-in static member", `option` one of each
        // instance (19.7, 19.7.1).
        if scoped && !type_option {
            return Err(invalid(format!(
                "`option.{member}` of covergroup `{}` named with `::`: `option` is a member of each instance and only `type_option` is a static member (IEEE 1800-2023 19.7, 19.7.1)",
                model.display
            )));
        }
        if found.arguments.is_some() {
            return Err(invalid(format!(
                "`{}.{member}` of covergroup `{}` is called: it is a member of a structure, not a method (IEEE 1800-2023 19.10)",
                found.structure(),
                model.display
            )));
        }
        found.item()?;
        return Ok(());
    }
    // Only `get_coverage` is a static method (19.8).
    if scoped && member != "get_coverage" {
        return Err(invalid(format!(
            "`{member}` of covergroup `{}` called with `::`: it is not a static method (IEEE 1800-2023 19.8)",
            model.display
        )));
    }
    if found.item.is_some() {
        // A coverpoint or cross has neither (Table 19-5).
        if matches!(member, "sample" | "set_inst_name") {
            return Err(invalid(format!(
                "`{member}` called on a coverpoint or cross of covergroup `{}` (IEEE 1800-2023 19.8)",
                model.display
            )));
        }
        found.item()?;
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
    if found.member != "sample" || found.structure.is_some() {
        return Ok(None);
    }
    check(&found)?;
    let Receiver::Handle(handle) = found.receiver else {
        return Ok(None);
    };
    let model = &*found.model;
    // The `ref` actuals are those of the declaration that constructs the
    // instance, which only the variable it declares names: the call takes
    // none here, and `procedural::covergroup_sites::pass_references` gives
    // it those of the declaration its handle names, once the declarations
    // of the scope are bound.
    let references = model.references().count();
    if references != 0
        && !matches!(
            native
                .expression(expression_binding::ungroup(handle, native))
                .kind,
            ExprKind::Name(_)
        )
    {
        return Err(sampled_elsewhere());
    }
    request(session, model);
    name_subroutines(session, model);
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
        1 + references,
        listed(found.arguments.unwrap_or_default()),
    )
    .map_err(|error| {
        as_method(
            error,
            &name,
            &format!("sample` of covergroup `{}", model.display),
        )
    })?;
    args.splice(
        0..0,
        std::iter::once(Some(first)).chain((0..references).map(|_| None)),
    );
    Ok(Some((name, args, arg_order)))
}

/// The rejection of `sample()` of a covergroup with a `ref` formal through
/// anything but the variable of the declaration that constructs the
/// instance (19.3).
pub(in crate::ast) fn sampled_elsewhere() -> AnalyzerError {
    unsupported(
        "covergroup ref argument of an instance sampled through a variable other than its declaration (IEEE 1800-2023 19.3, 19.8)",
    )
}

/// The type a member of an option structure of `kind` has (19.10).
fn option_type(kind: OptionType) -> Type {
    match kind {
        OptionType::Int => procedural::int_type(),
        OptionType::Bit => classes::unsigned_type(1),
        OptionType::Text => procedural::randomization::string_type(),
    }
}

/// Native expression `value` as the value assigned to a member of an option
/// structure of `kind`, which `context` names: checked as an assignment to
/// a variable of its type (19.10), with the assignment checks of an `int`
/// or `bit` place, and bound as one.
fn assigned(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    native: &Declarations,
    dimensions: &PackedDimensions,
    value: ExprId,
    kind: OptionType,
    context: impl Fn() -> String,
) -> Converted<Expr> {
    let ty = option_type(kind);
    // A `string` takes what a `string` variable is assigned (6.16), as the
    // value is converted below, as an assignment converts it.
    if kind != OptionType::Text
        && let Some(target) = semantic_types::of_type(
            &mut session.types,
            &tree.functions,
            &ty,
            &dimensions.const_env,
        )
    {
        assignments::check_assign(tree, session, value, native, dimensions, target, context)?;
    }
    let shape = procedural::bound_dimensions_from_type(
        &ty,
        &tree.functions,
        &mut session.types,
        &dimensions.const_env,
    );
    let bound =
        expression_binding::bind_value(value, native, tree, session, dimensions, Some(&shape))?;
    Ok(if kind == OptionType::Text {
        expression_binding::string_operand(bound, dimensions)
    } else {
        bound
    })
}

/// The value of a use of a predefined member of a covergroup (19.7, 19.8):
/// a coverage query of the covergroup or of one of its items, with or
/// without the two `ref int` arguments, `start()`, `stop()` and
/// `set_inst_name()`, which have no value and are used as statements,
/// which `statement` tells, and a read of a member of `option` or
/// `type_option`. A use the analysis does not support, or one the standard
/// excludes, is an error. None for an expression that is no such use.
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
    let model = found.model.clone();
    let model = &*model;
    let (item, level) = found.item()?;
    let item_number = Expr::Literal(format!("32'd{item}"));
    let handle = |session: &mut Session| match found.receiver {
        Receiver::Handle(handle) => {
            expression_binding::bind_value(handle, native, tree, session, dimensions, None)
        }
        // `check` allows `::` for the members of the type alone.
        Receiver::Type => Err(invalid(format!(
            "`{}` of covergroup `{}` called with `::` (IEEE 1800-2023 19.8)",
            found.member, model.display
        ))),
    };
    if let Some(type_option) = found.structure {
        let (member, _) =
            procedural_member(type_option, &found.member, level, &model.display, false)??;
        request(session, model);
        return Ok(Some(match member {
            Member::Option(option) => classes::object_call(
                ObjectOp::Coverage(CoverageOp::GetOption { item, option }),
                vec![handle(session)?],
            ),
            // A static member, which a handle only names: it is not read, so
            // `null` is no error.
            Member::Type(option) => classes::object_call(
                ObjectOp::Coverage(CoverageOp::GetTypeOption { item, option }),
                vec![type_key(model)],
            ),
            Member::Strobe => unreachable!("`procedural_member` rejects a read of strobe"),
        }));
    }
    let member = found.member.as_str();
    let arguments = listed(found.arguments.unwrap_or_default());
    let no_value = || {
        invalid(format!(
            "the {member} method of covergroup `{}` returns no value (IEEE 1800-2023 19.8)",
            model.display
        ))
    };
    let arity = |expected: &str| AnalyzerError::InvalidCallArguments {
        subroutine: member.to_owned(),
        detail: format!(
            "{} actuals for `{member}` of covergroup `{}`, which takes {expected} (IEEE 1800-2023 19.8)",
            arguments.len(),
            model.display
        ),
    };
    // Table 19-5 gives the formals of these methods no names.
    if member != "sample"
        && let Some(named) = arguments.iter().find_map(|argument| argument.name.as_ref())
    {
        return Err(AnalyzerError::InvalidCallArguments {
            subroutine: member.to_owned(),
            detail: format!(
                "`{member}` of covergroup `{}` is called with the named argument `.{}`, but its formals have no names (IEEE 1800-2023 19.8, Table 19-5)",
                model.display,
                instances::normalize_identifier(&named.spelling)
            ),
        });
    }
    match member {
        "sample" => {
            if statement {
                // `sample_statement` binds it.
                return Ok(None);
            }
            return Err(no_value());
        }
        "start" | "stop" => {
            if !statement {
                return Err(no_value());
            }
            if !arguments.is_empty() {
                return Err(arity("no argument"));
            }
            request(session, model);
            return Ok(Some(classes::object_call(
                ObjectOp::Coverage(CoverageOp::Collect {
                    item,
                    start: member == "start",
                }),
                vec![handle(session)?],
            )));
        }
        "set_inst_name" => {
            if !statement {
                return Err(no_value());
            }
            let [
                Argument {
                    value: Some(value), ..
                },
            ] = arguments
            else {
                return Err(arity("one string"));
            };
            let name = assigned(
                tree,
                session,
                native,
                dimensions,
                *value,
                OptionType::Text,
                || {
                    format!(
                        "the argument of `set_inst_name` of covergroup `{}`",
                        model.display
                    )
                },
            )?;
            request(session, model);
            // `set_inst_name(s)` "sets the instance name", which is
            // `option.name` (Table 19-1).
            return Ok(Some(classes::object_call(
                ObjectOp::Coverage(CoverageOp::SetOption {
                    item: 0,
                    option: celox_heap::coverage::OptionId::Name,
                }),
                vec![handle(session)?, name],
            )));
        }
        _ => {}
    }
    // `get_coverage` and `get_inst_coverage`, of the instance only through
    // a handle (`check`).
    let instance = member == "get_inst_coverage";
    request(session, model);
    match arguments.len() {
        0 => Ok(Some(if instance {
            classes::object_call(
                ObjectOp::Coverage(CoverageOp::InstCoverage {
                    what: CoverageQuery::Real,
                }),
                vec![handle(session)?, item_number],
            )
        } else {
            // The coverage of the type, which a handle only names: it is
            // not read, so `null` is no error.
            classes::object_call(
                ObjectOp::Coverage(CoverageOp::TypeCoverage {
                    what: CoverageQuery::Real,
                }),
                vec![type_key(model), item_number],
            )
        })),
        // "a pair of int values passed by reference" (19.8).
        2 => {
            if !session.covergroup_count_requests.contains(&model.name) {
                session.covergroup_count_requests.push(model.name.clone());
            }
            let name = if instance {
                model.counts_subroutine()
            } else {
                model.type_counts_subroutine()
            };
            let formals = count_formals();
            let leading = if instance { 2 } else { 1 };
            let (mut args, arg_order) = actuals(
                tree,
                session,
                native,
                dimensions,
                &name,
                &formals[1..],
                leading,
                arguments,
            )
            .map_err(|error| as_method(error, &name, member))?;
            args.insert(0, Some(item_number));
            if instance {
                args.insert(0, Some(handle(session)?));
            }
            Ok(Some(Expr::Call {
                callee: Callee::Subroutine(name),
                args,
                arg_order,
            }))
        }
        _ => Err(arity(
            "no argument or two `int` variables passed by reference",
        )),
    }
}

/// The assignment of `right` to native expression `left` with operator `op`
/// when `left` is a member of `option` or `type_option` of a covergroup or
/// of one of its items (19.7, 19.7.1): the coverage operation that assigns
/// it, whose second operand is the value, and the type of the member
/// (19.10) where it is an `int` or a `bit`. The value is checked as one
/// assigned to a variable of that type, and the caller converts it as such
/// an assignment does, by assigning it to one. None when `left` is no such
/// member.
#[allow(clippy::too_many_arguments)]
pub(in crate::ast) fn option_assignment(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    left: ExprId,
    right: ExprId,
    op: &str,
    nonblocking: bool,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<(Expr, Option<Type>)>> {
    let Some(found) = predefined(tree, session, left, native, dimensions)? else {
        return Ok(None);
    };
    let Some(type_option) = found.structure else {
        return Ok(None);
    };
    check(&found)?;
    let model = found.model.clone();
    let model = &*model;
    let (item, level) = found.item()?;
    let (member, kind) =
        procedural_member(type_option, &found.member, level, &model.display, true)??;
    let place = format!(
        "`{}.{}` of covergroup `{}`",
        found.structure(),
        found.member,
        model.display
    );
    if op != "=" || nonblocking {
        return Err(unsupported(format!(
            "covergroup option member assigned with `{}` ({place}; IEEE 1800-2023 19.7)",
            if nonblocking { "<=" } else { op }
        )));
    }
    let value = assigned(tree, session, native, dimensions, right, kind, || {
        place.clone()
    })?;
    request(session, model);
    let converted = (kind != OptionType::Text).then(|| option_type(kind));
    Ok(Some((
        match member {
            Member::Option(option) => {
                let Receiver::Handle(handle) = found.receiver else {
                    unreachable!("`check` rejects `option` named with `::`");
                };
                classes::object_call(
                    ObjectOp::Coverage(CoverageOp::SetOption { item, option }),
                    vec![
                        expression_binding::bind_value(
                            handle, native, tree, session, dimensions, None,
                        )?,
                        value,
                    ],
                )
            }
            // A static member: a handle only names the type.
            Member::Type(option) => classes::object_call(
                ObjectOp::Coverage(CoverageOp::SetTypeOption { item, option }),
                vec![type_key(model), value],
            ),
            Member::Strobe => unreachable!("`procedural_member` rejects a write of strobe"),
        },
        converted,
    )))
}

/// The rejection of native expression `id` as a place that something other
/// than an assignment writes, an output argument or a destination of a
/// system task, when it is a member of `option` or `type_option` of a
/// covergroup: only `=` assigns one (see [`option_assignment`]).
pub(in crate::ast) fn check_place(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    let Some(found) = predefined(tree, session, id, native, dimensions)? else {
        return Ok(());
    };
    if found.structure.is_none() && !OPTIONS.contains(&found.member.as_str()) {
        return Ok(());
    }
    check(&found)?;
    Err(unsupported(format!(
        "covergroup option member written other than by an assignment with `=` (`{}.{}` of covergroup `{}`; IEEE 1800-2023 19.7)",
        found.structure(),
        found.member,
        found.model.display
    )))
}

/// The type of the value of native expression `id` when it is a coverage
/// query of a covergroup, which is a `real` (19.8), or a member of `option`
/// or `type_option`, of the type 19.10 gives it.
pub(in crate::ast) fn result_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Option<semantic_types::TyId> {
    let ty = result(tree, session, id, native, dimensions)?;
    semantic_types::of_type(
        &mut session.types,
        &tree.functions,
        &ty,
        &dimensions.const_env,
    )
}

/// The shape of the value of native expression `id` as a query (`$bits`,
/// a place an update reads) takes it, when it is a coverage query of a
/// covergroup or a member of `option` or `type_option` (see
/// [`result_type`]).
pub(in crate::ast) fn result_shape(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Option<VariableDimensions> {
    let ty = result(tree, session, id, native, dimensions)?;
    Some(procedural::bound_dimensions_from_type(
        &ty,
        &tree.functions,
        &mut session.types,
        &dimensions.const_env,
    ))
}

/// The type of [`result_type`].
fn result(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Option<Type> {
    let callee = match &native.expression(id).kind {
        ExprKind::Call { expression, .. } => expression_binding::ungroup(*expression, native),
        ExprKind::Member { .. } => id,
        _ => return None,
    };
    let ExprKind::Member {
        expression, name, ..
    } = &native.expression(callee).kind
    else {
        return None;
    };
    let option = matches!(
        &native.expression(*expression).kind,
        ExprKind::Member { name, .. } if OPTIONS.contains(&name.spelling.as_str())
    );
    if !option && !matches!(name.spelling.as_str(), "get_coverage" | "get_inst_coverage") {
        return None;
    }
    let found = predefined(tree, session, id, native, dimensions)
        .ok()
        .flatten()?;
    let Some(type_option) = found.structure else {
        return type_binding::builtin_type("real", None);
    };
    let (_, level) = found.item().ok()?;
    let (_, kind) = procedural_member(
        type_option,
        &found.member,
        level,
        &found.model.display,
        false,
    )
    .ok()?
    .ok()?;
    Some(option_type(kind))
}

/// `$get_coverage()` (19.9): the coverage of every covergroup type of the
/// design.
pub(in crate::ast) fn overall() -> Expr {
    classes::object_call(ObjectOp::Coverage(CoverageOp::Overall), Vec::new())
}
