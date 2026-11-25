//! What the analysis knows of a covergroup type where it is used.

use super::*;
use crate::procedural::ParamDirection;
use celox_sv_syntax::{CoverageEvent, CovergroupKind, SubroutinePort};

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

    /// The type, when it can be constructed and sampled.
    pub fn usable(&self) -> Converted<&Self> {
        match &self.rejection {
            Some(rejection) => Err(rejection.clone()),
            None => Ok(self),
        }
    }
}

/// Whether subroutine `name` samples an instance of a covergroup.
pub(in crate::ast) fn is_sampler(name: &str) -> bool {
    is_class(name) && name.ends_with("::$sample")
}

/// Whether subroutine `name` constructs an instance of a covergroup.
pub(crate) fn is_constructor(name: &str) -> bool {
    is_class(name) && name.ends_with("::$new")
}

fn invalid(text: impl Into<String>) -> AnalyzerError {
    AnalyzerError::InvalidCovergroup(text.into())
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
        rejection: None,
    };
    fn reject(model: &mut Covergroup, error: AnalyzerError) {
        model.rejection.get_or_insert(deferred(error));
    }
    let CovergroupKind::Declared { ports, event } = &declaration.kind else {
        reject(
            &mut model,
            unsupported("covergroup extends (IEEE 1800-2023 19.4.1)"),
        );
        return Ok(model);
    };
    // Before the formals are bound: an unexpanded `let` in one would bind
    // as a call of a function nothing declares.
    let port_tokens = formal_ports(declaration)
        .map(|port| port.tokens)
        .collect::<Vec<_>>();
    if let Some(named) = let_name(tree, declaration, &port_tokens, &[]) {
        model.rejection = Some(unsupported(format!(
            "covergroup argument that names `let` `{named}` (IEEE 1800-2023 11.12, 19.3)"
        )));
    }
    match formals(tree, session, ports.as_deref().unwrap_or_default(), context) {
        Ok(formals) => model.new_formals = formals,
        Err(error) => reject(&mut model, error),
    }
    for formal in &model.new_formals {
        match formal.direction {
            ParamDirection::Input => {}
            ParamDirection::Output | ParamDirection::Inout => {
                return Err(invalid(format!(
                    "argument `{}` of covergroup `{}` is an output or inout argument (IEEE 1800-2023 19.3)",
                    formal.name, model.display
                )));
            }
            ParamDirection::Ref | ParamDirection::ConstRef => {
                model
                    .rejection
                    .get_or_insert(unsupported("covergroup ref argument (IEEE 1800-2023 19.3)"));
            }
        }
        if formal.direction == ParamDirection::Input
            && (!formal.r#type.unpacked_ranges.is_empty()
                || formal.r#type.container().is_some()
                || formal.r#type.is_unpacked_struct_in(&session.types))
        {
            model.rejection.get_or_insert(unsupported(
                "covergroup argument of unpacked type (IEEE 1800-2023 19.3)",
            ));
        }
    }
    match event {
        None => {}
        Some(CoverageEvent::Clocking(_)) => reject(
            &mut model,
            unsupported("covergroup sampling event (IEEE 1800-2023 19.3)"),
        ),
        Some(CoverageEvent::Block(_)) => reject(
            &mut model,
            unsupported("covergroup block event (IEEE 1800-2023 19.3)"),
        ),
        Some(CoverageEvent::Sample(ports)) => {
            match formals(tree, session, ports, context) {
                Ok(formals) => model.sample_formals = formals,
                Err(error) => reject(&mut model, error),
            }
            for formal in &model.sample_formals {
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
                if matches!(
                    formal.direction,
                    ParamDirection::Ref | ParamDirection::ConstRef
                ) {
                    model.rejection.get_or_insert(unsupported(
                        "covergroup ref argument of the sample method (IEEE 1800-2023 19.8.1)",
                    ));
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
        let bound = compile::Bound::of(declaration).ranges(tree.declarations());
        if let Some(named) = let_name(tree, declaration, &bound, &hidden) {
            model.rejection = Some(unsupported(format!(
                "covergroup expression that names `let` `{named}` (IEEE 1800-2023 11.12, 19.5)"
            )));
        }
    }
    Ok(model)
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
/// covergroup (13.5.3); it does in the items of the covergroup (19.8.1).
fn let_name(
    tree: &AnalysisContext<'_>,
    declaration: &celox_sv_syntax::CovergroupDeclaration,
    ports: &[celox_sv_syntax::TokenRange],
    hidden: &[String],
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
            if hidden.contains(&name) {
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
