//! What the analysis knows of a covergroup type where it is used.

use super::*;
use crate::procedural::ParamDirection;
use celox_sv_syntax::{CoverageEvent, CovergroupKind, SubroutinePort, TimingControlId};

/// A formal of `new` or of `sample` of a covergroup (IEEE 1800-2023 19.3,
/// 19.8.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ast) struct Formal {
    pub name: String,
    pub direction: ParamDirection,
    pub r#type: Type,
    /// The default a call that omits the actual takes (13.5.3): an
    /// expression of the file that declares the covergroup, which the scope
    /// that declares it binds in the subroutine. Another scope reads only
    /// whether there is one.
    pub default: Option<celox_sv_syntax::ExprId>,
}

/// A coverpoint or cross of a covergroup type, as a use of the type names it
/// (IEEE 1800-2023 19.5, 19.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ast) struct Item {
    /// The name the language can use: the label, or the variable of an
    /// unlabelled coverpoint of one; `None` for a name only reports show.
    pub name: Option<String>,
    pub cross: bool,
}

/// A covergroup type, as the scope that declares it bound its header. It is
/// what travels to the scopes that use the type: a scope-level one with the
/// symbols of its scope, an embedded one with its class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ast) struct Covergroup {
    /// The reserved class name, which identifies the type in the design.
    pub name: String,
    /// The name reports and messages show, such as `Top.cg` or `p::cg`.
    pub display: String,
    /// The identifier of the declaration, which names the property of an
    /// embedded covergroup.
    pub identifier: String,
    /// The declaration, in the file of the scope that declares the type:
    /// that scope makes the subroutines of the type from it.
    pub declaration: CovergroupId,
    /// The class an embedded covergroup is declared in (19.4): its
    /// subroutines take an object of the class, whose property the handle
    /// is, where those of another covergroup take the handle.
    pub owner: Option<String>,
    /// The formals of `new` (19.3).
    pub new_formals: Vec<Formal>,
    /// The formals of `sample`: none unless the covergroup is declared `with
    /// function sample` (19.8.1).
    pub sample_formals: Vec<Formal>,
    /// The coverpoints, then the crosses, in the order the heap numbers
    /// them from item 1: the declared coverpoints, those of the variables
    /// the crosses cross, then the crosses (see `compile::Layout`).
    pub items: Vec<Item>,
    /// The clocking event of the type, at which each of its instances is
    /// sampled (19.3): the instance of a declaration of a module or a
    /// program with an initializer by a sampler of the scope (see
    /// `procedural::covergroup_sites`), any other by the process that
    /// [`Self::spawn_subroutine`] starts. An event in the file of
    /// [`Self::declaration`], bound in the scope that declares the type, or
    /// in the class of an embedded one.
    pub event: Option<TimingControlId>,
    /// Whether an interface declares the type (19.3, 25.3). Each instance of
    /// the interface, and each view of one through a port, holds the
    /// subroutines of the type under names of its own, as they read its
    /// members (see [`instance_subroutine`]); only the interface's own body
    /// uses the type.
    pub interface: bool,
    /// Why the type cannot be constructed or sampled: something it declares
    /// is not supported, or its header does not bind. The declaration
    /// itself is accepted, so that a covergroup nothing uses keeps a design
    /// building; a `new` or a method call of the type reports this, and the
    /// type has no subroutines.
    pub rejection: Option<AnalyzerError>,
}

impl Covergroup {
    /// The key of the type in the heap.
    pub fn key(&self) -> u64 {
        celox_heap::coverage::type_key(&self.name)
    }

    /// The class of the objects of the instances.
    pub fn class(&self) -> u32 {
        classes::class_id(&self.name)
    }

    /// The subroutine that registers the type and returns its key.
    pub fn type_subroutine(&self) -> String {
        format!("{}::$type", self.name)
    }

    /// The subroutine that constructs an instance.
    pub fn new_subroutine(&self) -> String {
        format!("{}::$new", self.name)
    }

    /// The subroutine that samples an instance.
    pub fn sample_subroutine(&self) -> String {
        format!("{}::$sample", self.name)
    }

    /// The subroutine that starts sampling an instance at the clocking
    /// event of the type and returns its handle, which a `new` of a type
    /// with one calls with the instance it constructs, unless a declaration
    /// of a module or a program constructs it (19.3; see
    /// `procedural::covergroup_subroutines`).
    pub fn spawn_subroutine(&self) -> String {
        format!("{}::$spawn", self.name)
    }

    /// The subroutine of `get_inst_coverage(ref int, ref int)` of an
    /// instance (19.8).
    pub fn counts_subroutine(&self) -> String {
        format!("{}::$counts", self.name)
    }

    /// The subroutine of `get_coverage(ref int, ref int)` of the type
    /// (19.8).
    pub fn type_counts_subroutine(&self) -> String {
        format!("{}::$type_counts", self.name)
    }

    /// The item named `name`, its number among the items of the heap from
    /// 1, and whether it is a cross.
    pub fn item(&self, name: &str) -> Option<(u32, bool)> {
        self.items
            .iter()
            .position(|item| item.name.as_deref() == Some(name))
            .map(|index| (index as u32 + 1, self.items[index].cross))
    }

    /// The `ref` and `const ref` formals of `new`, by their index among
    /// [`Self::new_formals`] (19.3): `N::$sample` takes them after the
    /// handle, before the formals of `sample`.
    pub fn references(&self) -> impl Iterator<Item = (usize, &Formal)> {
        self.new_formals
            .iter()
            .enumerate()
            .filter(|(_, formal)| formal.direction.is_reference())
    }

    /// Whether `new` of the type has a `ref` or `const ref` formal: only a
    /// declaration of a module or a program with an initializer constructs
    /// an instance then (see `procedural::covergroup_sites`).
    pub fn has_references(&self) -> bool {
        self.references().next().is_some()
    }

    /// Whether the type has [`Self::spawn_subroutine`]: a clocking event,
    /// and no `ref` formal, whose instances only a declaration constructs.
    pub fn spawns(&self) -> bool {
        self.event.is_some() && !self.has_references()
    }

    /// The type, when it can be constructed and sampled.
    pub fn usable(&self) -> Converted<&Self> {
        match &self.rejection {
            Some(rejection) => Err(rejection.clone()),
            None => Ok(self),
        }
    }
}

/// The formals of `N::$counts` after the handle of the instance, which are
/// those of `N::$type_counts`: the number of the item, then the two `int`
/// formals that `get_coverage` and `get_inst_coverage` take by reference
/// (IEEE 1800-2023 19.8).
pub(in crate::ast) fn count_formals() -> Vec<Formal> {
    let formal = |name: &str, direction, r#type| Formal {
        name: name.to_owned(),
        direction,
        r#type,
        default: None,
    };
    vec![
        formal(
            ITEM_FORMAL,
            ParamDirection::Input,
            classes::unsigned_type(32),
        ),
        formal(
            "covered",
            ParamDirection::Ref,
            crate::ast::procedural::int_type(),
        ),
        formal(
            "total",
            ParamDirection::Ref,
            crate::ast::procedural::int_type(),
        ),
    ]
}

/// The source name of the formal of `N::$counts` and `N::$type_counts` that
/// numbers the item, which no identifier of a source spells.
pub(in crate::ast) const ITEM_FORMAL: &str = "$item";

/// Whether subroutine `name` samples an instance of a covergroup.
pub(in crate::ast) fn is_sampler(name: &str) -> bool {
    is_class(name) && name.ends_with("::$sample")
}

/// Whether subroutine `name` starts the sampler of an instance of a
/// covergroup with a clocking event (see [`Covergroup::spawn_subroutine`]).
pub(in crate::ast) fn is_spawner(name: &str) -> bool {
    is_class(name) && name.ends_with("::$spawn")
}

/// Whether subroutine `name` constructs an instance of a covergroup.
pub(crate) fn is_constructor(name: &str) -> bool {
    is_class(name) && name.ends_with("::$new")
}

/// The name subroutine `name` of a covergroup type has in the instance of an
/// interface, or the view of one through a port, whose members are bound
/// with `prefix`, when `name` is a subroutine of a covergroup type of the
/// interface, one of `models`, or one of an interface instance its body
/// binds: `$covergroup@prefix::N::$sample` for `$covergroup::N::$sample`,
/// `$covergroup@prefix$inner::N::$sample` for `$covergroup@inner::N::$sample`,
/// as the members of a nested instance are named. The subroutines read the
/// members of the instance, so two instances of the interface in one scope
/// have their own; the type, its class and its key stay those of the
/// interface (IEEE 1800-2023 19.3). The name keeps the suffix of its role,
/// which tells what the subroutine does, and starts with
/// [`INSTANCE_PREFIX`], which no other name of the design starts with: every
/// other covergroup name starts with [`PREFIX`].
pub(in crate::ast) fn instance_subroutine(
    name: &str,
    models: &[Arc<Covergroup>],
    prefix: &str,
) -> Option<String> {
    if let Some(rest) = name.strip_prefix(INSTANCE_PREFIX) {
        let (inner, rest) = rest.split_once("::")?;
        return Some(format!("{INSTANCE_PREFIX}{prefix}${inner}::{rest}"));
    }
    let (base, _) = name.split_at(name.rfind("::$")?);
    let rest = name.strip_prefix(PREFIX)?;
    models
        .iter()
        .any(|model| model.interface && model.owner.is_none() && model.name == base)
        .then(|| format!("{INSTANCE_PREFIX}{prefix}::{rest}"))
}

/// Whether subroutine `name` is a subroutine of a covergroup type of an
/// interface in an instance or a port view of it (see
/// [`instance_subroutine`]).
pub(in crate::ast) fn is_instance_subroutine(name: &str) -> bool {
    name.starts_with(INSTANCE_PREFIX)
}

fn invalid(text: impl Into<String>) -> AnalyzerError {
    AnalyzerError::InvalidCovergroup(text.into())
}

/// What messages name subroutine `N::$new` or `N::$sample` of a covergroup
/// type by: its method and the type, whose `ref` formals of `new` are
/// those of `references`.
#[derive(Debug, Clone)]
pub(in crate::ast) struct Named {
    display: String,
    method: &'static str,
    references: Vec<String>,
}

impl Named {
    /// The name of the method in messages that name a subroutine.
    fn method(&self) -> String {
        // The message quotes the subroutine: "`new` of covergroup `Top.cg`".
        format!("{}` of covergroup `{}", self.method, self.display)
    }
}

/// Record what messages name the subroutines `new` and `sample` of `model`
/// by (see [`in_covergroup_terms`]).
pub(in crate::ast) fn name_subroutines(session: &mut Session, model: &Covergroup) {
    if session
        .covergroup_names
        .contains_key(&model.sample_subroutine())
    {
        return;
    }
    let references = model
        .references()
        .map(|(_, formal)| formal.name.clone())
        .collect::<Vec<_>>();
    for (subroutine, method) in [
        (model.new_subroutine(), "new"),
        (model.sample_subroutine(), "sample"),
    ] {
        session.covergroup_names.insert(
            subroutine,
            Named {
                display: model.display.clone(),
                method,
                references: references.clone(),
            },
        );
    }
}

/// `error` of a call or of the body of subroutine `N::$new` or
/// `N::$sample` of a covergroup type that `names` names, in the terms of the
/// covergroup: its method and its name instead of the subroutine, which no
/// source spells. A `ref` argument of `new` that an expression of the
/// covergroup writes is an error of the covergroup: "a covergroup cannot
/// modify any argument to the new operator, a ref argument will be treated
/// the same as a read-only const ref argument" (19.3); a `ref` argument of
/// `sample`, which the analysis treats as a `const ref` one as well, is not
/// supported written.
pub(in crate::ast) fn in_covergroup_terms(
    error: AnalyzerError,
    names: &HashMap<String, Named>,
) -> AnalyzerError {
    match error {
        AnalyzerError::InvalidCallArguments { subroutine, detail }
            if let Some(named) = names.get(&subroutine) =>
        {
            AnalyzerError::InvalidCallArguments {
                subroutine: named.method(),
                detail,
            }
        }
        AnalyzerError::InvalidRefArgument {
            subroutine,
            argument,
            detail,
        } if let Some(named) = names.get(&subroutine) => {
            if !detail.starts_with("a const ref argument cannot be written") {
                return AnalyzerError::InvalidRefArgument {
                    subroutine: named.method(),
                    argument,
                    detail,
                };
            }
            if named.references.contains(&argument) {
                invalid(format!(
                    "ref argument `{argument}` of covergroup `{}` is written by an expression of the covergroup, which cannot modify an argument of `new`: a ref argument is read-only, as a const ref argument is (IEEE 1800-2023 19.3)",
                    named.display
                ))
            } else {
                unsupported(format!(
                    "covergroup ref argument `{argument}` of the sample method of `{}` written by an expression of the covergroup (IEEE 1800-2023 19.8.1)",
                    named.display
                ))
            }
        }
        error => error,
    }
}

/// The rejection of a `new` of a covergroup with a `ref` formal that is not
/// the initializer of a declaration of a module or a program (19.3).
pub(in crate::ast) fn outside_a_site() -> AnalyzerError {
    unsupported(
        "covergroup ref argument outside a declaration with an initializer (IEEE 1800-2023 19.3)",
    )
}

/// A rejection of something the analysis does not support in a covergroup:
/// every one starts with `covergroup `, which the tracking issues key on.
fn deferred(error: AnalyzerError) -> AnalyzerError {
    match error {
        AnalyzerError::Unsupported(text) if !text.starts_with("covergroup ") => {
            unsupported(format!("covergroup argument ({text})"))
        }
        error => error,
    }
}

/// The error of making the subroutines of a covergroup type. What the
/// analysis does not support of the expressions of its items starts with
/// `covergroup `, as every rejection of a covergroup does.
pub(in crate::ast) fn rejected(error: AnalyzerError) -> AnalyzerError {
    match error {
        AnalyzerError::Unsupported(text) if !text.starts_with("covergroup ") => {
            unsupported(format!("covergroup expression ({text})"))
        }
        error => error,
    }
}

/// The formals `ports` of `what` of the covergroup, bound in `context`.
fn formals(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    ports: &[SubroutinePort],
    context: &PackedDimensions,
) -> Converted<Vec<Formal>> {
    Ok(
        type_binding::port_formals(ports, "covergroup", tree, session, context)?
            .into_iter()
            .map(|(name, direction, r#type, default)| Formal {
                name,
                direction,
                r#type,
                default,
            })
            .collect(),
    )
}

/// The model of covergroup `id` of the file of `tree`, of `names`, embedded
/// in class `owner` if it is, with its header bound in `context`, the
/// scope that declares it. What the standard makes illegal is an error; what
/// the analysis does not support, or cannot bind, is kept in the model for
/// the uses of the type (see [`Covergroup::rejection`]).
pub(in crate::ast) fn build(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: CovergroupId,
    names: Names,
    owner: Option<String>,
    context: &PackedDimensions,
) -> Converted<Covergroup> {
    let declaration = tree.declarations().covergroup(id);
    let mut model = Covergroup {
        name: names.class,
        display: names.display,
        identifier: identifier(tree.declarations(), id),
        declaration: id,
        owner,
        new_formals: Vec::new(),
        sample_formals: Vec::new(),
        items: Vec::new(),
        event: None,
        interface: classes::enclosing_unit(tree.declarations(), declaration.tokens)
            .is_some_and(|unit| unit.kind == celox_sv_syntax::UnitKind::Interface),
        rejection: None,
    };
    fn reject(model: &mut Covergroup, error: AnalyzerError) {
        model.rejection.get_or_insert(deferred(error));
    }
    // A class of an interface reads the members of the first instance of
    // the interface in every instance (a limitation of classes declared in
    // interfaces): what a covergroup embedded in one samples would be wrong.
    if model.interface && model.owner.is_some() {
        reject(
            &mut model,
            unsupported(
                "covergroup embedded in a class declared in an interface (IEEE 1800-2023 19.4, 25.3)",
            ),
        );
    }
    let CovergroupKind::Declared { ports, event } = &declaration.kind else {
        reject(
            &mut model,
            unsupported("covergroup extends (IEEE 1800-2023 19.4.1)"),
        );
        return Ok(model);
    };
    let layout = compile::Layout::of(tree.declarations(), declaration);
    model.items = layout
        .names
        .iter()
        .map(|name| Item {
            name: name.clone(),
            cross: false,
        })
        .chain(layout.crosses.iter().map(|(cross, _)| {
            Item {
                name: cross
                    .label
                    .as_ref()
                    .map(|label| instances::normalize_identifier(&label.spelling)),
                cross: true,
            }
        }))
        .collect();
    // Before the formals are bound: an unexpanded `let` in one would bind
    // as a call of a function nothing declares.
    let port_tokens = formal_ports(declaration)
        .map(|port| port.tokens)
        .collect::<Vec<_>>();
    if let Some(named) = let_name(tree, declaration, &port_tokens, &[], None) {
        model.rejection = Some(unsupported(format!(
            "covergroup argument that names `let` `{named}` (IEEE 1800-2023 11.12, 19.3)"
        )));
    }
    match formals(tree, session, ports.as_deref().unwrap_or_default(), context) {
        Ok(formals) => model.new_formals = formals,
        Err(error) => reject(&mut model, error),
    }
    let mut rejection = None;
    for formal in &mut model.new_formals {
        match formal.direction {
            ParamDirection::Input => {}
            ParamDirection::Output | ParamDirection::Inout => {
                return Err(invalid(format!(
                    "argument `{}` of covergroup `{}` is an output or inout argument (IEEE 1800-2023 19.3)",
                    formal.name, model.display
                )));
            }
            // "A ref argument will be treated the same as a read-only const
            // ref argument" (19.3). The instance reads the variable its
            // declaration names (see `procedural::covergroup_sites`); an
            // embedded covergroup is constructed by `new` of its class
            // alone, which is no such declaration.
            ParamDirection::Ref | ParamDirection::ConstRef => {
                formal.direction = ParamDirection::ConstRef;
                if model.owner.is_some() {
                    rejection.get_or_insert_with(outside_a_site);
                }
                // An integral variable or a fixed-size array of them, which
                // `N::$sample` reads as any subroutine reads a `const ref`
                // formal of its type.
                let ty = &formal.r#type;
                if !matches!(
                    ty.kind(),
                    TypeKind::Bit | TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit
                ) || ty.container().is_some()
                    || ty.is_unpacked_struct_in(&session.types)
                {
                    rejection.get_or_insert_with(|| {
                        unsupported(
                            "covergroup ref argument of a type that is neither integral nor a fixed-size array of an integral type (IEEE 1800-2023 19.3)",
                        )
                    });
                }
            }
        }
        if formal.direction == ParamDirection::Input
            && (!formal.r#type.unpacked_ranges.is_empty()
                || formal.r#type.container().is_some()
                || formal.r#type.is_unpacked_struct_in(&session.types))
        {
            rejection.get_or_insert_with(|| {
                unsupported("covergroup argument of unpacked type (IEEE 1800-2023 19.3)")
            });
        }
    }
    if let Some(rejection) = rejection {
        model.rejection.get_or_insert(rejection);
    }
    match event {
        None => {}
        // `type_option.strobe` "only applies to the scheduling of samples
        // triggered by a clocking event" (19.3): a type without one may set
        // it to no effect, and a type with one that sets it is rejected for
        // it, as a sample in the Postponed region is not supported.
        Some(CoverageEvent::Clocking(_)) if strobes(tree, session, declaration, context) => reject(
            &mut model,
            unsupported("covergroup strobe sampling (IEEE 1800-2023 19.7.1)"),
        ),
        Some(CoverageEvent::Clocking(id)) => match clocking_rejection(tree, &model, *id) {
            Some(rejection) => reject(&mut model, rejection),
            None => model.event = Some(*id),
        },
        Some(CoverageEvent::Block(_)) => reject(
            &mut model,
            unsupported("covergroup block event (IEEE 1800-2023 19.3)"),
        ),
        Some(CoverageEvent::Sample(ports)) => {
            match formals(tree, session, ports, context) {
                Ok(formals) => model.sample_formals = formals,
                Err(error) => reject(&mut model, error),
            }
            for formal in &mut model.sample_formals {
                if formal.direction == ParamDirection::Output {
                    return Err(invalid(format!(
                        "argument `{}` of the sample method of covergroup `{}` is an output argument (IEEE 1800-2023 19.8.1)",
                        formal.name, model.display
                    )));
                }
                if model
                    .new_formals
                    .iter()
                    .any(|other| other.name == formal.name)
                {
                    return Err(invalid(format!(
                        "`{}` is an argument of covergroup `{}` and of its sample method (IEEE 1800-2023 19.8.1)",
                        formal.name, model.display
                    )));
                }
                // The covergroup reads a formal of `sample` in the sample
                // alone, and writes none: a `ref` one is a `const ref` one,
                // which the call passes as any subroutine call does.
                if formal.direction == ParamDirection::Ref {
                    formal.direction = ParamDirection::ConstRef;
                }
            }
        }
    }
    // The items: what is illegal is an error here, and what is not
    // supported is rejected where the type is used.
    let sample_formals = model
        .sample_formals
        .iter()
        .map(|formal| formal.name.clone())
        .collect::<Vec<_>>();
    if let Some(error) = compile::check(
        tree.declarations(),
        declaration,
        &model.display,
        &sample_formals,
    )? {
        reject(&mut model, error);
    }
    // The expressions of the items are bound with the subroutines of the
    // type, where a `let` is not expanded either, unless an argument of the
    // covergroup or of its sample method hides it.
    if model.rejection.is_none() {
        let hidden = model
            .new_formals
            .iter()
            .map(|formal| formal.name.clone())
            .chain(sample_formals)
            .collect::<Vec<_>>();
        let items = compile::Bound::of(tree.declarations(), declaration);
        let bound = items.ranges(tree.declarations());
        if let Some(named) = let_name(tree, declaration, &bound, &hidden, Some(&items)) {
            model.rejection = Some(unsupported(format!(
                "covergroup expression that names `let` `{named}` (IEEE 1800-2023 11.12, 19.5)"
            )));
        }
    }
    Ok(model)
}

/// Why the instances of covergroup `model` cannot be sampled at its
/// clocking event `event` (IEEE 1800-2023 19.3): the event
/// may not name an argument of the covergroup.
fn clocking_rejection(
    tree: &AnalysisContext<'_>,
    model: &Covergroup,
    event: TimingControlId,
) -> Option<AnalyzerError> {
    let native = tree.declarations();
    // "A clocking event may refer to arguments of the covergroup" (19.3).
    // The names are compared as written: an argument hides a signal of
    // the scope with its name, which binding the event in the scope would
    // read instead.
    let formals = model
        .new_formals
        .iter()
        .map(|formal| formal.name.as_str())
        .collect::<Vec<_>>();
    let tokens = native.timing_control(event).tokens;
    let names_formal = !formals.is_empty()
        && native.expression_nodes().any(|(_, expression)| {
            tokens.start <= expression.tokens.start
                && expression.tokens.end <= tokens.end
                && matches!(&expression.kind, celox_sv_syntax::ExprKind::Name(name)
                    if formals.contains(&instances::normalize_identifier(&name.spelling).as_str()))
        });
    names_formal.then(|| {
        unsupported(
            "covergroup sampling event that names an argument of the covergroup (IEEE 1800-2023 19.3)",
        )
    })
}

/// Whether covergroup `declaration` assigns `type_option.strobe` a value
/// that `context`, the scope that declares it, folds to a nonzero constant
/// (IEEE 1800-2023 19.7.1).
fn strobes(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    declaration: &celox_sv_syntax::CovergroupDeclaration,
    context: &PackedDimensions,
) -> bool {
    let native = tree.declarations();
    declaration.items.iter().any(|item| {
        let celox_sv_syntax::CoverageItem::Option(option) = item else {
            return false;
        };
        if !option.type_option
            || instances::normalize_identifier(&option.member.spelling) != "strobe"
        {
            return false;
        }
        let Ok(value) =
            expression_binding::bind_value(option.value, native, tree, session, context, None)
        else {
            return false;
        };
        match fold_const_integral_expr_preserving_mask(tree, session, value, &context.const_env) {
            Expr::Literal(text) => crate::typecheck::parse_integral_literal(&text)
                .is_some_and(|literal| literal.value != num_bigint::BigUint::ZERO),
            _ => false,
        }
    })
}

/// The `let` declaration of the design element of covergroup `declaration`
/// that an expression or a type in token ranges `ports` of it names, if one
/// does and none of `hidden` has its name. The calls of a `let` are
/// expanded before the element is analyzed (see `lets`), but for what
/// follows the name of a covergroup, where a name may be a coverpoint or
/// `item`: a `let` named there is not expanded, and the covergroup is
/// rejected where it is used rather than bound with a call of a function
/// that nothing declares. A formal does not hide the `let` in the types and
/// defaults of the formals, which are those of the scope that declares the
/// covergroup (13.5.3); it does in the items of the covergroup (19.8.1),
/// `items`, where `item` in a `with` expression is the value the filter is
/// asked about (19.5.1.1).
fn let_name(
    tree: &AnalysisContext<'_>,
    declaration: &celox_sv_syntax::CovergroupDeclaration,
    ports: &[celox_sv_syntax::TokenRange],
    hidden: &[String],
    items: Option<&compile::Bound>,
) -> Option<String> {
    use celox_sv_syntax::ExprKind;
    let native = tree.declarations();
    if native.lets.is_empty() || ports.is_empty() {
        return None;
    }
    let element = |tokens| classes::enclosing_unit(native, tokens).map(|unit| unit.tokens);
    let here = element(declaration.tokens);
    let within = |tokens: celox_sv_syntax::TokenRange| {
        ports
            .iter()
            .any(|port| port.start <= tokens.start && tokens.start < port.end)
    };
    tree.spans()
        .covergroup_bound
        .0
        .within(ports)
        .into_iter()
        .find_map(|id| {
            let expression = native.expression(id);
            let ExprKind::Name(name) = &expression.kind else {
                return None;
            };
            if !within(expression.tokens) {
                return None;
            }
            let name = instances::normalize_identifier(&name.spelling);
            if hidden.contains(&name)
                || items.is_some_and(|items| items.names_item(native, &name, expression.tokens))
            {
                return None;
            }
            native
                .lets
                .iter()
                .any(|declared| {
                    instances::normalize_identifier(&declared.name.spelling) == name
                        && element(declared.tokens) == here
                })
                .then_some(name)
        })
}

/// Declares scope-level covergroup `id` of the scope being analyzed, in
/// elaborated generate scope `generate_scope` when it is in one: its model,
/// which the uses of the type in the scope and in the scopes that import it
/// find by its name.
pub(in crate::ast) fn declare(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: CovergroupId,
    generate_scope: Option<&str>,
    context: &PackedDimensions,
) -> Converted<Arc<Covergroup>> {
    let names = scope_names(tree, session, id, generate_scope);
    let model = Arc::new(build(tree, session, id, names, None, context)?);
    session
        .covergroups
        .insert(model.name.clone(), model.clone());
    Ok(model)
}
