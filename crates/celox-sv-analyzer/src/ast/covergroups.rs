//! Covergroups (IEEE 1800-2023 clause 19).
//!
//! A covergroup declaration declares a type. Its handles are class handles
//! of a reserved class name that no class declaration has, as those of
//! mailboxes are: they are assigned, compared with `null`, stored in
//! properties, arrays and containers and traced by the collector with no
//! typing of their own. The class has no layout; what an instance collects
//! is in the heap of the simulator (`celox_heap::coverage`), named by the
//! handle of the object its `new` returns.
//!
//! The analyzer gives each type three subroutines (see
//! `procedural::covergroup_subroutines`):
//! `N::$type` registers its descriptor and returns its key, `N::$new`
//! constructs an instance and `N::$sample` samples one. A `new` and the
//! predefined methods bind to calls of them and to coverage operations.
//!
//! What is not supported is rejected where a `new` or a method of the type
//! is bound, not where the type is declared: a covergroup that nothing
//! constructs costs nothing and keeps a design building (see
//! [`Covergroup::rejection`]).

use super::*;
use celox_sv_syntax::{BodyItemKind, CovergroupId, Declarations, TokenRange};

mod compile;
mod methods;
mod model;

pub(crate) use compile::Bound;
pub(super) use compile::{
    ITEM, Layout, Level, Member, OptionType, SelectTree, cross_name, filter, one_value_transition,
    option_member, point_name, select_tree, variable_names,
};
pub(super) use methods::{
    check_place, construct, construct_at_site, option_assignment, overall, result_shape,
    result_type, sample_statement, sampled_elsewhere, value,
};
pub(crate) use model::is_constructor;
pub(super) use model::{
    Covergroup, Formal, ITEM_FORMAL, Named, build, count_formals, declare, in_covergroup_terms,
    instance_subroutine, is_instance_subroutine, is_sampler, is_spawner, name_subroutines,
    outside_a_site, rejected,
};

/// The start of the reserved class name of every covergroup type.
const PREFIX: &str = "$covergroup::";

/// The start of the name of every subroutine of a covergroup type of an
/// interface in an instance or a port view of the interface (see
/// `model::instance_subroutine`).
const INSTANCE_PREFIX: &str = "$covergroup@";

/// The call that marks a task or function of an interface called through a
/// virtual interface with a formal or a result of a covergroup type of the
/// interface: no scope has the subroutine, so a scope that reaches it is
/// rejected as one that reaches a subroutine of the covergroup through a
/// virtual interface (see `module_elaboration::reachable_imports`).
pub(super) const THROUGH_A_VIRTUAL_INTERFACE: &str = "$covergroup@$vif::$handle";

/// Whether class `name` is a covergroup type, or subroutine `name` one of a
/// covergroup type.
pub(super) fn is_class(name: &str) -> bool {
    name.starts_with(PREFIX) || name.starts_with(INSTANCE_PREFIX)
}

/// What follows [`PREFIX`] in the reserved class name of every covergroup
/// embedded in a class (see [`embedded_names`]).
const EMBEDDED: &str = "class::";

/// What follows [`PREFIX`] in the reserved class name of every covergroup
/// declared in an interface (see [`scope_names`]).
const INTERFACE: &str = "interface::";

/// Whether class `name` is a covergroup type declared in an interface.
pub(crate) fn is_interface_class(name: &str) -> bool {
    name.strip_prefix(PREFIX)
        .is_some_and(|name| name.starts_with(INTERFACE))
}

/// Whether a value of type `ty` holds a handle of a covergroup type of an
/// interface: a handle, a fixed-size array of them, an unpacked structure
/// with such a member, also in another structure, and a queue, dynamic or
/// associative array of any of these, by the semantic type of `ty` where
/// `types` has it and by the classes its retained container type names
/// otherwise (IEEE 1800-2023 19.3, 7.2, 7.4, 7.5, 7.8, 7.10).
pub(super) fn holds_interface_handle(
    ty: &crate::ir::Type,
    types: Option<&semantic_types::TyTable>,
) -> bool {
    ty.class().is_some_and(is_interface_class)
        || ty
            .structure_leaves()
            .iter()
            .any(|(_, leaf)| holds_interface_handle(leaf, types))
        || types
            .zip(ty.semantic_type())
            .is_some_and(|(types, id)| types.holds_class(id, &is_interface_class))
        || ty
            .container()
            .is_some_and(|container| container.retains_class(&is_interface_class))
}

/// [`holds_interface_handle`] where no semantic type is known: of a
/// reference that a hierarchical name makes.
pub(crate) fn holds_interface_handles(ty: &crate::ir::Type) -> bool {
    holds_interface_handle(ty, None)
}

/// Whether `property` is the variable of a covergroup embedded in its class
/// (IEEE 1800-2023 19.4): the property the declaration of the covergroup
/// makes, whose type nothing else names. A property of a covergroup type
/// that a scope declares is an ordinary handle property.
pub(super) fn embedded_property(
    property: &classes::Property,
    table: &semantic_types::TyTable,
) -> bool {
    property.field.handle.is_some()
        && property.r#type.class_in(table).is_some_and(|class| {
            class
                .strip_prefix(PREFIX)
                .and_then(|name| name.strip_prefix(EMBEDDED))
                .and_then(|name| name.strip_suffix(property.name.as_str()))
                .and_then(|name| name.strip_suffix("::"))
                .is_some_and(|name| {
                    name == property.owner
                        || name
                            .strip_suffix(property.owner.as_str())
                            .is_some_and(|scope| scope.ends_with("::"))
                })
        })
}

/// Where a covergroup is declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Place {
    /// In a design element, a package, the compilation unit or a generate
    /// block: its name is a type of that scope (19.3).
    Scope,
    /// In a class: an embedded covergroup (19.4).
    Class,
    /// In a generate loop, where each iteration would declare a type of its
    /// own (27.4).
    LoopGenerate,
    /// In a checker, which is not elaborated.
    Checker,
}

/// The covergroup declarations of a file by where they are declared,
/// indexed when its arenas are built.
#[derive(Debug, Default)]
pub(crate) struct CovergroupIndex {
    places: HashMap<CovergroupId, Place>,
}

impl CovergroupIndex {
    pub(crate) fn new(native: &Declarations) -> Self {
        // A file without covergroups pays nothing.
        if native.covergroups.is_empty() {
            return Self::default();
        }
        let mut places: HashMap<CovergroupId, Place> = native
            .classes
            .iter()
            .flat_map(|class| class.covergroups.iter().map(|id| (*id, Place::Class)))
            .collect();
        let mut loops = Vec::new();
        let mut checkers = Vec::new();
        for item in &native.body_items {
            match &item.kind {
                BodyItemKind::Loop { .. } => loops.push(item.tokens),
                BodyItemKind::Unconverted { category, .. } if category == "checker" => {
                    checkers.push(item.tokens);
                }
                _ => {}
            }
        }
        let within = |ranges: &[TokenRange], tokens: TokenRange| {
            ranges
                .iter()
                .any(|range| range.start <= tokens.start && tokens.end <= range.end)
        };
        for item in &native.body_items {
            if let BodyItemKind::Covergroup(id) = item.kind {
                places.entry(id).or_insert_with(|| {
                    if within(&checkers, item.tokens) {
                        Place::Checker
                    } else if within(&loops, item.tokens) {
                        Place::LoopGenerate
                    } else {
                        Place::Scope
                    }
                });
            }
        }
        Self { places }
    }

    /// Where covergroup `id` is declared.
    pub(super) fn place(&self, id: CovergroupId) -> Place {
        self.places.get(&id).copied().unwrap_or(Place::Scope)
    }
}

/// Where covergroup `id` of the file of `tree` is declared.
pub(super) fn place(tree: &AnalysisContext<'_>, id: CovergroupId) -> Place {
    tree.spans().covergroup_index.place(id)
}

/// The formals of covergroup `declaration` and of its sample method (IEEE
/// 1800-2023 19.3, 19.8.1).
pub(crate) fn formal_ports(
    declaration: &celox_sv_syntax::CovergroupDeclaration,
) -> impl Iterator<Item = &celox_sv_syntax::SubroutinePort> {
    let (ports, sample) = match &declaration.kind {
        celox_sv_syntax::CovergroupKind::Declared { ports, event } => (
            ports.as_deref(),
            match event {
                Some(celox_sv_syntax::CoverageEvent::Sample(ports)) => Some(ports.as_slice()),
                _ => None,
            },
        ),
        celox_sv_syntax::CovergroupKind::Extends => (None, None),
    };
    ports.into_iter().chain(sample).flatten()
}

/// The identifier covergroup `id` is declared with.
pub(super) fn identifier(native: &Declarations, id: CovergroupId) -> String {
    instances::normalize_identifier(&native.covergroup(id).name.spelling)
}

/// The names of a covergroup type: the reserved class name that identifies
/// it in the design, and the name reports and messages show.
pub(super) struct Names {
    pub class: String,
    pub display: String,
}

/// The names of scope-level covergroup `id` of the file of `tree`, declared
/// in elaborated generate scope `generate_scope` when it is in one. A
/// covergroup of a module, interface or program is a type of the elaborated
/// scope: instances whose parameters have the same values share it (see
/// `ScopeElaboration::covergroup_scope`). One of a package is a
/// type of the design, and one of the compilation unit of its source file
/// (IEEE 1800-2023 6.22, 3.12.1).
pub(super) fn scope_names(
    tree: &AnalysisContext<'_>,
    session: &Session,
    id: CovergroupId,
    generate_scope: Option<&str>,
) -> Names {
    let native = tree.declarations();
    let declaration = native.covergroup(id);
    let identifier = identifier(native, id);
    let local = match generate_scope.filter(|scope| !scope.is_empty()) {
        Some(scope) => format!("{scope}.{identifier}"),
        None => identifier,
    };
    match classes::enclosing_unit(native, declaration.tokens) {
        Some(unit) if unit.kind == celox_sv_syntax::UnitKind::Package => {
            let package = instances::normalize_identifier(&unit.name.spelling);
            Names {
                class: format!("{PREFIX}{package}::{local}"),
                display: format!("{package}::{local}"),
            }
        }
        Some(unit) => {
            let element = instances::normalize_identifier(&unit.name.spelling);
            // The elaborated scope by the values of its parameters (see
            // `ScopeElaboration::covergroup_scope`): instances with the same
            // values share the type, also one that takes a default and one
            // that writes it. A type of an interface is marked as one,
            // `interface` being a keyword that names no scope.
            let scope = session
                .covergroup_scope
                .clone()
                .or_else(|| types::unit_scope(Some(unit), session));
            let scope = scope.as_deref().unwrap_or(&element);
            let marker = if unit.kind == celox_sv_syntax::UnitKind::Interface {
                INTERFACE
            } else {
                ""
            };
            Names {
                class: format!("{PREFIX}{marker}{scope}::{local}"),
                display: format!("{element}.{local}"),
            }
        }
        None => Names {
            class: format!(
                "{PREFIX}{}",
                packages::unit_item_identity(tree.compilation_path(), &local)
            ),
            display: format!("{}::{local}", packages::UNIT),
        },
    }
}

/// The names of covergroup `id` embedded in class `class`, a declared name
/// or that of a specialization: an anonymous type of the class (19.4). A
/// class of a module is named by its identifier alone, so its elaborated
/// scope tells its covergroups from those of another module's class.
pub(super) fn embedded_names(
    tree: &AnalysisContext<'_>,
    session: &Session,
    id: CovergroupId,
    class: &str,
) -> Names {
    let native = tree.declarations();
    let identifier = identifier(native, id);
    let scope = classes::enclosing_unit(native, native.covergroup(id).tokens)
        .and_then(|unit| types::unit_scope(Some(unit), session));
    let spelled = match classes::unit_class_identifier(class) {
        Some(identifier) => format!("{}::{identifier}", packages::UNIT),
        None => class.to_owned(),
    };
    Names {
        class: match scope {
            Some(scope) => format!("{PREFIX}{EMBEDDED}{scope}::{class}::{identifier}"),
            None => format!("{PREFIX}{EMBEDDED}{class}::{identifier}"),
        },
        display: format!("{spelled}::{identifier}"),
    }
}

/// The scope-level covergroup that body item `item` declares, as a type of
/// its scope: its identifier and its handle type. None for an item that
/// declares none, and for a covergroup the analysis does not model, whose
/// uses [`unmodelled`] rejects.
pub(super) fn item_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    item: celox_sv_syntax::BodyItemId,
    generate_scope: Option<&str>,
) -> Option<(String, Type)> {
    let native = tree.declarations();
    let BodyItemKind::Covergroup(id) = native.body_item(item).kind else {
        return None;
    };
    if place(tree, id) != Place::Scope {
        return None;
    }
    let names = scope_names(tree, session, id, generate_scope);
    Some((
        identifier(native, id),
        classes::handle(&mut session.types, &names.class),
    ))
}

/// Why type name `name`, which nothing visible at token `at` declares as a
/// type the analysis has, cannot be used, when a covergroup the analysis
/// does not model declares it in the scope of `tree`.
pub(super) fn unmodelled(tree: &AnalysisContext<'_>, name: &str) -> Option<AnalyzerError> {
    let native = tree.declarations();
    if native.covergroups.is_empty() {
        return None;
    }
    let scope = tree.root_unit().ok().map(|unit| unit.tokens);
    native.body_items.iter().find_map(|item| {
        let BodyItemKind::Covergroup(id) = item.kind else {
            return None;
        };
        if identifier(native, id) != name
            || classes::enclosing_unit(native, item.tokens).map(|unit| unit.tokens) != scope
        {
            return None;
        }
        match place(tree, id) {
            Place::LoopGenerate => Some(unsupported(format!(
                "covergroup `{name}` declared in a loop generate block (IEEE 1800-2023 19.3, 27.4)"
            ))),
            Place::Checker => Some(unsupported(
                "covergroup declared in a checker (IEEE 1800-2023 19.3)",
            )),
            Place::Scope | Place::Class => None,
        }
    })
}

/// The model of covergroup type `name`: one the scope being analyzed
/// declares, one a scope it imports declares, or one embedded in class
/// `owner` or in a base of it.
pub(super) fn model(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
    name: &str,
    owner: Option<&str>,
) -> Converted<Arc<Covergroup>> {
    if let Some(model) = session.covergroups.get(name) {
        // A class of an interface reads the members of the first instance
        // of the interface in every instance (classes declared in interfaces
        // are not told apart by instance), so it does not use the
        // covergroups of the interface.
        if model.interface && dimensions.class_scope.is_some() {
            return Err(in_a_class());
        }
        return Ok(model.clone());
    }
    if let Some(model) = scope::imported(tree)
        .covergroups
        .iter()
        .find(|model| model.name == name)
    {
        // The subroutines of a type of an interface are those of each of
        // its instances, which the body of the instance calls; a scope
        // that binds the instance, or receives it through a port, names
        // none of them.
        if model.interface {
            return Err(outside_the_interface());
        }
        return Ok(model.clone());
    }
    if let Some(owner) = owner
        && let Some(model) = expression_binding::object_class(tree, session, dimensions, owner)?
            .covergroups
            .iter()
            .find(|model| model.name == name)
    {
        return Ok(model.clone());
    }
    // The class of an embedded covergroup is known from the variable that
    // names it alone.
    Err(
        if name
            .strip_prefix(PREFIX)
            .is_some_and(|name| name.starts_with(EMBEDDED))
        {
            through_expression()
        } else if is_interface_class(name) {
            outside_the_interface()
        } else {
            unsupported(
                "covergroup type used outside the scopes that declare or import it (IEEE 1800-2023 19.3)",
            )
        },
    )
}

/// The rejection of a `new` or a method call of a covergroup type that an
/// interface declares, bound outside the body of the interface.
pub(crate) fn outside_the_interface() -> AnalyzerError {
    unsupported(
        "covergroup declared in an interface used from outside the interface (IEEE 1800-2023 19.3, 25.3)",
    )
}

/// Records the names that `bound`, the symbols an interface instance or port
/// view binds into the scope being analyzed, gives the members whose types
/// hold handles of a covergroup type of the interface, and the tasks and
/// functions with a formal or a result of such a type: the scope's own code
/// may not name them (see [`reject_outside_names`]). A handle the scope read
/// could be handed to another instance, whose subroutines of the type would
/// sample the members of that instance (19.3).
pub(super) fn record_outside_names(
    session: &mut Session,
    functions: &const_functions::ConstantFunctions,
    bound: &scope::ScopeSymbols,
) {
    if !bound.covergroups.iter().any(|model| model.interface) {
        return;
    }
    let mut names = Vec::new();
    for (name, ty) in bound
        .signals
        .iter()
        .map(|signal| (&signal.name, &signal.r#type))
        .chain(bound.ports.iter().map(|port| (&port.name, &port.r#type)))
    {
        if semantic_types::of_type(&mut session.types, functions, ty, &ConstEnv::default())
            .is_some_and(|id| session.types.holds_class(id, &is_interface_class))
            || ty
                .class_in(&session.types)
                .is_some_and(|class| is_interface_class(&class))
        {
            names.push(name.clone());
        }
    }
    for subroutine in &bound.subroutines {
        if subroutine
            .params
            .iter()
            .map(|param| &param.r#type)
            .chain(&subroutine.return_type)
            .any(|ty| holds_interface_handle(ty, Some(&session.types)))
            && !is_class(&subroutine.name)
        {
            names.push(subroutine.name.clone());
        }
    }
    session.interface_covergroup_names.extend(names);
}

/// Rejects the own code of the scope being analyzed, its processes and
/// subroutines, when it names what [`record_outside_names`] recorded: a
/// handle of a covergroup type of an interface outside the body of the
/// instance that holds it.
pub(super) fn reject_outside_names(
    session: &Session,
    subroutines: &[Subroutine],
    comb_processes: &[CombProcess],
    ff_processes: &[FfProcess],
    initial_processes: &[InitialProcess],
) -> Converted<()> {
    if session.interface_covergroup_names.is_empty() {
        return Ok(());
    }
    let marked = session
        .interface_covergroup_names
        .iter()
        .map(|name| (name.clone(), format!("{name}\u{0}")))
        .collect::<HashMap<_, _>>();
    let renamer = scope::Renamer { names: &marked };
    let names = subroutines
        .iter()
        .any(|subroutine| renamer.subroutine(subroutine.clone()) != *subroutine)
        || comb_processes
            .iter()
            .any(|process| renamer.comb_process(process.clone()) != *process)
        || ff_processes
            .iter()
            .any(|process| renamer.ff_process(process.clone()) != *process)
        || initial_processes.iter().any(|process| {
            process
                .body
                .iter()
                .any(|stmt| renamer.stmt(stmt.clone()) != *stmt)
        });
    if names {
        return Err(outside_the_interface());
    }
    Ok(())
}

/// The rejection of a use of a covergroup type of an interface in a class
/// the interface declares, or through a handle that a property of an object
/// holds.
pub(super) fn in_a_class() -> AnalyzerError {
    unsupported(
        "covergroup declared in an interface used through a class (IEEE 1800-2023 19.3, 25.3)",
    )
}

/// The rejection of a function that a module calls through an interface
/// port and that constructs an instance of a covergroup type of the
/// interface with a clocking event.
pub(super) fn spawned_through_a_port() -> AnalyzerError {
    unsupported(
        "covergroup sampling event of an instance of a covergroup declared in an interface constructed by a function called through an interface port (IEEE 1800-2023 19.3, 25.5)",
    )
}

/// The rejection of a task or function of an interface called through a
/// virtual interface that constructs or uses an instance of a covergroup
/// type of the interface.
pub(super) fn through_a_virtual_interface() -> AnalyzerError {
    unsupported(
        "covergroup declared in an interface used by a task or function called through a virtual interface (IEEE 1800-2023 19.3, 25.9)",
    )
}

/// The rejection of an instance of an interface that uses a covergroup type
/// of its own, in the body of another interface.
pub(super) fn nested_instance() -> AnalyzerError {
    unsupported(
        "covergroup declared in an interface instantiated in another interface (IEEE 1800-2023 19.3, 25.3)",
    )
}

/// What the names of the hidden variables of the samplers of a scope start
/// with (see `procedural::covergroup_sites`).
pub(super) const HIDDEN: &str = "%covergroup_";

/// Whether signal `name`, as an interface instance or port binds it, is a
/// hidden variable of the samplers of the covergroups of the interface.
pub(super) fn is_hidden_variable(name: &str) -> bool {
    name.starts_with(HIDDEN) || name.contains(&format!("${HIDDEN}"))
}

/// The rejection of a use of an embedded covergroup that does not name its
/// variable, `cg` in a method of its class or `e.cg`: the subroutines of the
/// covergroup take the object that holds it (IEEE 1800-2023 19.4).
fn through_expression() -> AnalyzerError {
    unsupported(
        "covergroup embedded in a class used through an expression that is not its variable (IEEE 1800-2023 19.4)",
    )
}

/// The formals of the subroutines of `model`, each with its name: source
/// names, directions and types. `first` is the type of the first formal of
/// `N::$new`, `N::$sample` and `N::$spawn` where they take one: the handle
/// type of the class of an embedded covergroup, whose object they take, or
/// else that of the covergroup, whose handle `N::$sample` takes. `handle` is
/// that of the covergroup, whose handle `N::$counts` and `N::$spawn` take.
/// `N::$sample` takes the `ref` formals of `new` after the handle (see
/// [`Covergroup::references`]). `N::$spawn` is a subroutine of a type with
/// a clocking event alone (see [`Covergroup::spawns`]).
pub(super) fn signatures(
    model: &Covergroup,
    first: &Type,
    handle: &Type,
) -> Vec<(
    String,
    Vec<(String, crate::procedural::ParamDirection, Type)>,
)> {
    use crate::procedural::ParamDirection;
    let formals = |leading: Option<(&str, &Type)>, formals: &[Formal]| {
        leading
            .map(|(name, ty)| (name.to_owned(), ParamDirection::Input, ty.clone()))
            .into_iter()
            .chain(
                formals
                    .iter()
                    .map(|formal| (formal.name.clone(), formal.direction, formal.r#type.clone())),
            )
            .collect::<Vec<_>>()
    };
    let embedded = model.owner.is_some();
    let counts = count_formals();
    let sample_formals = model
        .references()
        .map(|(_, formal)| formal.clone())
        .chain(model.sample_formals.iter().cloned())
        .collect::<Vec<_>>();
    let spawn = model.spawns().then(|| {
        let mut formals = formals(embedded.then_some((classes::THIS, first)), &[]);
        formals.push((HANDLE.to_owned(), ParamDirection::Input, handle.clone()));
        (model.spawn_subroutine(), formals)
    });
    [
        (model.type_subroutine(), Vec::new()),
        (
            model.new_subroutine(),
            formals(
                embedded.then_some((classes::THIS, first)),
                &model.new_formals,
            ),
        ),
        (
            model.sample_subroutine(),
            formals(
                Some((if embedded { classes::THIS } else { HANDLE }, first)),
                &sample_formals,
            ),
        ),
        (
            model.counts_subroutine(),
            formals(Some((HANDLE, handle)), &counts),
        ),
        (model.type_counts_subroutine(), formals(None, &counts)),
    ]
    .into_iter()
    .chain(spawn)
    .collect()
}

/// The construct of the processes and procedures that sample the instances
/// of a covergroup at its clocking event (see `procedural::covergroup_sites`).
pub(super) const SAMPLING: &str = "covergroup sampling";

/// The source name of the handle of the instance in the subroutines of a
/// covergroup, which no identifier of a source spells.
pub(super) const HANDLE: &str = "$cg";
