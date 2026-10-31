//! Parameterized classes (IEEE 1800-2023 8.25).
//!
//! Each unique set of parameter values and types of a parameterized class is
//! a specialization: a class of its own, named by the class and the set,
//! such as `C#(N=4,T=bit)`. A specialization keeps the constants and types
//! its declarations see: the parameters, and the scope `C::`, which inside
//! the class refers to the specialization being declared, while the type
//! `C` is the default specialization there too. A class without parameters
//! is its own only specialization.

use crate::ast::Session;
use std::rc::Rc;

use super::*;
use celox_sv_syntax::{Argument, ExprKind, ParameterKind};

/// The parameters of one specialization of a class.
#[derive(Debug)]
pub(in crate::ast) struct Specialization {
    /// The name of the class declaration.
    pub generic: String,
    /// The values of the value parameters, with their markers.
    pub env: HashMap<String, i128>,
    /// The constants the value parameters hide without a value of their
    /// own, as a string or four-state value does.
    pub hidden: Vec<String>,
    /// The constants and types the class declares itself, which a class
    /// extending it inherits without the scope around the declaration.
    pub declared_env: HashMap<String, Option<i128>>,
    pub declared_aliases: HashMap<String, Type>,
    /// The literals that replace the value parameters.
    pub literals: HashMap<String, Expr>,
    /// The type parameters and typedefs, and the class name itself.
    pub aliases: HashMap<String, Type>,
    /// The class the specialization extends, or the interface classes an
    /// interface class extends, whose parameters and typedefs it inherits
    /// (IEEE 1800-2023 8.13, 8.26.3).
    pub bases: Vec<String>,
    /// The parameters of the port list, for `$typename`.
    pub arguments: Vec<type_names::ClassArgument>,
}

/// The types that a scope inside the class declared as `own` sees from
/// specialization `specialization` of its lineage: not the name of an
/// inherited class, which outside that class does not refer to its
/// specialization.
fn inherited<'a>(
    own: &'a str,
    specialization: &'a Specialization,
) -> impl Iterator<Item = (String, Type)> + 'a {
    specialization
        .aliases
        .iter()
        .filter(move |(key, _)| {
            specialization.generic == own || **key != scope_key(&specialization.generic)
        })
        .map(|(key, ty)| (key.clone(), ty.clone()))
}

/// The specialization `name` and the ones it inherits from, each after
/// those it inherits from.
fn lineage(session: &Session, name: &str) -> Vec<Rc<Specialization>> {
    let mut lineage: Vec<Rc<Specialization>> = Vec::new();
    let mut seen = HashSet::default();
    // A class that extends itself is rejected where it is laid out.
    let mut pending = vec![(name.to_owned(), false)];
    while let Some((name, visited)) = pending.pop() {
        let Some(specialization) = lookup(session, &name) else {
            continue;
        };
        if visited {
            lineage.push(specialization);
            continue;
        }
        if !seen.insert(name.clone()) {
            continue;
        }
        pending.push((name, true));
        pending.extend(
            specialization
                .bases
                .iter()
                .rev()
                .map(|base| (base.clone(), false)),
        );
    }
    lineage
}

/// Bound on the specializations of one design, which a class that
/// specializes itself with ever new parameters would exceed.
const MAX_SPECIALIZATIONS: usize = 1024;

/// Bound on specializations made while making another, which a class that
/// extends a specialization of itself would exceed.
const MAX_NESTING: usize = 64;

/// The key under which the scope of a class finds the specialization being
/// declared: inside a parameterized class and its out-of-block methods,
/// its name alone as the prefix of `::` refers to it, while as a type the
/// name alone is the default specialization (IEEE 1800-2023 8.25, 8.25.1).
pub(in crate::ast) fn scope_key(generic: &str) -> String {
    format!("{generic}::")
}

/// The specialization `name`, if one has been made.
pub(in crate::ast) fn lookup(session: &Session, name: &str) -> Option<Rc<Specialization>> {
    session
        .class_specializations
        .iter()
        .find(|(known, _)| known == name)
        .map(|(_, specialization)| specialization.clone())
}

/// The class declaration that class `name` specializes.
pub(in crate::ast) fn generic(session: &Session, name: &str) -> String {
    lookup(session, name).map_or_else(
        || {
            session
                .foreign
                .generic(name)
                .unwrap_or_else(|| name.to_owned())
        },
        |specialization| specialization.generic.clone(),
    )
}

/// The specializations made of the class declared as `generic`, in the
/// order they were made.
pub(in crate::ast) fn of(session: &mut Session, generic: &str) -> Vec<String> {
    session
        .class_specializations
        .iter()
        .filter(|(_, specialization)| specialization.generic == generic)
        .map(|(name, _)| name.clone())
        .collect()
}

/// `env` and `aliases` with the parameters of class `name` added: the
/// scope the declarations of the class see.
pub(in crate::ast) fn scope(
    session: &mut Session,
    name: &str,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> (HashMap<String, i128>, HashMap<String, Type>) {
    let (env, aliases) = shared_scope(session, name, &env.clone().into(), &aliases.clone().into());
    (env.into_iter().collect(), aliases.into_iter().collect())
}

/// [`scope`] of shared tables, which are copied only when the class adds
/// parameters or inherited types to them.
pub(in crate::ast) fn shared_scope(
    session: &mut Session,
    name: &str,
    env: &SharedMap<i128>,
    aliases: &SharedMap<Type>,
) -> (SharedMap<i128>, SharedMap<Type>) {
    let mut env = env.clone();
    let mut aliases = aliases.clone();
    let edits = ScopeEdits::of(session, name);
    if !edits.env.is_empty() {
        for (key, value) in edits.env {
            match value {
                Some(value) => env.insert(key, value),
                None => env.remove(&key),
            };
        }
    }
    if !edits.aliases.is_empty() {
        aliases.extend(edits.aliases);
    }
    (env, aliases)
}

/// What class `name` adds to the scope around it, in order: its parameters
/// and those it inherits, which may also hide a constant, and the types it
/// inherits.
struct ScopeEdits {
    env: Vec<(String, Option<i128>)>,
    aliases: Vec<(String, Type)>,
}

impl ScopeEdits {
    fn of(session: &mut Session, name: &str) -> Self {
        let mut edits = Self {
            env: Vec::new(),
            aliases: Vec::new(),
        };
        let own = generic(session, name);
        for specialization in lineage(session, name) {
            for base in &specialization.bases {
                if lookup(session, base).is_none()
                    && let Some(scope) = session.foreign.declarations(base)
                {
                    edits
                        .env
                        .extend(scope.env.iter().map(|(key, value)| (key.clone(), *value)));
                    edits.aliases.extend(
                        scope
                            .types
                            .iter()
                            .map(|(key, ty)| (key.clone(), ty.clone())),
                    );
                }
            }
            edits.env.extend(
                specialization
                    .env
                    .iter()
                    .map(|(key, value)| (key.clone(), Some(*value))),
            );
            edits
                .env
                .extend(specialization.hidden.iter().map(|key| (key.clone(), None)));
            edits.aliases.extend(inherited(&own, &specialization));
        }
        edits
    }
}

/// One copy of a scope's tables that the scopes of its classes are made
/// in, each from what its class adds, which is taken back afterwards:
/// making the scope of each specialization costs what the class adds, not
/// a copy of the tables around it.
pub(in crate::ast) struct ClassScopes {
    base: (SharedMap<i128>, SharedMap<Type>),
    env: SharedMap<i128>,
    aliases: SharedMap<Type>,
}

impl ClassScopes {
    pub(in crate::ast) fn new(env: &SharedMap<i128>, aliases: &SharedMap<Type>) -> Self {
        Self {
            base: (env.clone(), aliases.clone()),
            env: env.clone(),
            aliases: aliases.clone(),
        }
    }

    /// Call `f` with the scope of class `name` in the tables `env` and
    /// `aliases`, the same as [`shared_scope`] gives.
    pub(in crate::ast) fn with<R>(
        &mut self,
        session: &mut Session,
        name: &str,
        env: &SharedMap<i128>,
        aliases: &SharedMap<Type>,
        f: impl FnOnce(&mut Session, &SharedMap<i128>, &SharedMap<Type>) -> R,
    ) -> R {
        // The copy follows the tables it was made from.
        if self.base.0.identity() != env.identity() || self.base.1.identity() != aliases.identity()
        {
            *self = Self::new(env, aliases);
        }
        let edits = ScopeEdits::of(session, name);
        #[cfg(test)]
        if !edits.env.is_empty() && self.env.is_shared()
            || !edits.aliases.is_empty() && self.aliases.is_shared()
        {
            super::super::dimensions::CLASS_SCOPE_COPIES.with(|count| count.set(count.get() + 1));
        }
        // The entries before each edit, restored in reverse.
        let mut env_undo = Vec::with_capacity(edits.env.len());
        for (key, value) in edits.env {
            let old = match value {
                Some(value) => self.env.insert(key.clone(), value),
                None => self.env.remove(&key),
            };
            env_undo.push((key, old));
        }
        let mut aliases_undo = Vec::with_capacity(edits.aliases.len());
        for (key, ty) in edits.aliases {
            let old = self.aliases.insert(key.clone(), ty);
            aliases_undo.push((key, old));
        }
        let result = f(session, &self.env, &self.aliases);
        for (key, old) in env_undo.into_iter().rev() {
            match old {
                Some(old) => self.env.insert(key, old),
                None => self.env.remove(&key),
            };
        }
        for (key, old) in aliases_undo.into_iter().rev() {
            match old {
                Some(old) => self.aliases.insert(key, old),
                None => self.aliases.remove(&key),
            };
        }
        result
    }
}

/// `env` and `aliases` with what class `name` and its bases declare added,
/// which class `own` extending it inherits (IEEE 1800-2023 8.13): not the
/// scope around their declarations.
fn inherit(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
    own: &str,
    env: &mut HashMap<String, i128>,
    aliases: &mut HashMap<String, Type>,
) {
    if lookup(session, name).is_none() {
        if let Some(scope) = session.foreign.declarations(name) {
            scope.apply(env, aliases);
        }
        if let Some(members) = scope::imported(tree).class_member_types.get(name) {
            aliases.extend(members.clone());
        }
    }
    for specialization in lineage(session, name) {
        for (key, value) in &specialization.declared_env {
            match value {
                Some(value) => env.insert(key.clone(), *value),
                None => env.remove(key),
            };
        }
        aliases.extend(
            specialization
                .declared_aliases
                .iter()
                .filter(|(key, _)| {
                    **key != scope_key(&specialization.generic) || specialization.generic == own
                })
                .map(|(key, ty)| (key.clone(), ty.clone())),
        );
    }
}

/// `dimensions` and `literals` with the parameters of class `name` added,
/// for binding the bodies of its methods.
pub(in crate::ast) fn body_scope(
    session: &mut Session,
    name: &str,
    dimensions: &PackedDimensions,
    literals: &HashMap<String, Expr>,
) -> (PackedDimensions, HashMap<String, Expr>) {
    let mut dimensions = dimensions.clone();
    let mut literals = literals.clone();
    let own = generic(session, name);
    for specialization in lineage(session, name) {
        for base in &specialization.bases {
            if lookup(session, base).is_none()
                && let Some(scope) = session.foreign.declarations(base)
            {
                scope.apply(&mut dimensions.const_env, &mut dimensions.type_aliases);
                literals.extend(scope.literals.clone());
            }
        }
        dimensions.const_env.extend(specialization.env.clone());
        // The parameters hide constants of the scope around the class, also
        // the literals of those without an integral value.
        for key in &specialization.hidden {
            dimensions.const_env.remove(key);
            dimensions.parameter_values.remove(key);
        }
        dimensions
            .parameter_values
            .extend(specialization.literals.clone());
        dimensions
            .type_aliases
            .extend(inherited(&own, &specialization));
        literals.extend(specialization.literals.clone());
    }
    (dimensions, literals)
}

/// The value of parameter `parameter` of class `name`, as a literal.
pub(in crate::ast) fn parameter(
    session: &mut Session,
    name: &str,
    parameter: &str,
) -> Option<Expr> {
    lineage(session, name)
        .iter()
        .rev()
        .find_map(|specialization| {
            specialization.literals.get(parameter).cloned().or_else(|| {
                specialization
                    .bases
                    .iter()
                    .filter(|base| lookup(session, base).is_none())
                    .find_map(|base| session.foreign.parameter(base, parameter))
            })
        })
        .or_else(|| session.foreign.parameter(name, parameter))
}

pub(in crate::ast) fn export_declarations(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    members: &HashMap<String, HashMap<String, Type>>,
) -> HashMap<String, Rc<super::foreign::DeclarationScope>> {
    // Only compilation-unit and package declarations have a design-wide name.
    if tree
        .root_unit()
        .is_ok_and(|unit| unit.kind != celox_sv_syntax::UnitKind::Package)
    {
        return HashMap::default();
    }
    let own = super::scope_declarations(tree)
        .map(|class| super::declared_name(tree, class))
        .collect::<HashSet<_>>();
    session
        .class_specializations
        .iter()
        .filter(|(_, specialization)| own.contains(&specialization.generic))
        .map(|(name, _)| {
            let mut scope = super::foreign::DeclarationScope::default();
            for specialization in lineage(session, name) {
                for base in &specialization.bases {
                    if lookup(session, base).is_none()
                        && let Some(parent) = session.foreign.declarations(base)
                    {
                        scope.env.extend(parent.env.clone());
                        scope.literals.extend(parent.literals.clone());
                    }
                }
                scope.env.extend(specialization.declared_env.clone());
                scope.literals.extend(specialization.literals.clone());
            }
            scope.types = members.get(name).cloned().unwrap_or_default();
            (name.clone(), Rc::new(scope))
        })
        .collect()
}

/// The type parameter or typedef `member` of class `name`, declared in it
/// or inherited.
pub(in crate::ast) fn member_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
    member: &str,
) -> Option<Type> {
    if name == super::super::processes::CLASS {
        return (member == "state").then(super::super::processes::state_type);
    }
    let generic = generic(session, name);
    lineage(session, name)
        .iter()
        .rev()
        .find_map(|specialization| {
            // The class name itself is not a member.
            (member != scope_key(&specialization.generic) && member != scope_key(&generic))
                .then(|| specialization.aliases.get(member).cloned())
                .flatten()
        })
        .or_else(|| {
            scope::imported(tree)
                .class_member_types
                .get(name)?
                .get(member)
                .cloned()
        })
        .or_else(|| session.foreign.member_type(name, member))
}

/// Only types declared by a class and its bases cross the scope boundary.
/// Resolve bounds in their owning specialization, before caller names can
/// reinterpret them (6.18, 8.13, 8.23, 8.25).
pub(in crate::ast) fn export_member_types(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
) -> HashMap<String, HashMap<String, Type>> {
    let imported = scope::imported(tree);
    let mut result = imported.class_member_types.clone();
    for declaration in super::scope_declarations(tree) {
        for name in super::specializations_of(tree, session, declaration) {
            let mut members = HashMap::default();
            for specialization in lineage(session, &name) {
                for base in &specialization.bases {
                    if lookup(session, base).is_none()
                        && let Some(inherited) = imported.class_member_types.get(base)
                    {
                        members.extend(inherited.clone());
                    }
                }
                for (member, ty) in &specialization.declared_aliases {
                    if let Some(ty) =
                        type_parameters::resolve(&tree.functions, ty.clone(), &specialization.env)
                    {
                        members.insert(member.clone(), ty);
                    }
                }
            }
            result.insert(name, members);
        }
    }
    result
}

/// The specialization of class `generic` that `arguments`, bound in
/// `context`, assign its parameters (IEEE 1800-2023 8.25), made if it is new.
pub(in crate::ast) fn specialize(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    generic: &str,
    arguments: &[Argument],
    context: &PackedDimensions,
) -> Converted<String> {
    match bind_specialization(tree, session, generic, arguments, context)? {
        Binding::Ready(name) => Ok(name),
        Binding::Deferred(error) => Err(error),
    }
}

/// Initial typedef discovery runs before enclosing constants are collected.
/// A missing value, or a type argument whose bounds need such a constant, may
/// be retried after that collection; actual type uses and
/// subsequent typedef passes still require every parameter to be constant.
pub(in crate::ast) fn specialize_if_ready(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    generic: &str,
    arguments: &[Argument],
    context: &PackedDimensions,
) -> Converted<Option<String>> {
    Ok(
        match bind_specialization(tree, session, generic, arguments, context)? {
            Binding::Ready(name) => Some(name),
            Binding::Deferred(_) => None,
        },
    )
}

enum Binding {
    Ready(String),
    /// Waiting for enclosing constants, with the error a use reports.
    Deferred(AnalyzerError),
}

fn unresolved_parameter(name: &str) -> AnalyzerError {
    unsupported(format!("class parameter `{name}` without a constant value"))
}

fn bind_specialization(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    generic: &str,
    arguments: &[Argument],
    context: &PackedDimensions,
) -> Converted<Binding> {
    if session.class_specialization_depth >= MAX_NESTING {
        return Err(unsupported(format!(
            "class specializations nested more than {MAX_NESTING} deep, as when class `{generic}` extends a specialization of itself"
        )));
    }
    session.class_specialization_depth += 1;
    let binding = bind_nested_specialization(tree, session, generic, arguments, context);
    session.class_specialization_depth -= 1;
    binding
}

/// [`bind_specialization`] inside the specializations being made.
fn bind_nested_specialization(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    generic: &str,
    arguments: &[Argument],
    context: &PackedDimensions,
) -> Converted<Binding> {
    if super::foreign::is_foreign(tree, session, generic) {
        return Ok(
            match super::foreign::request(tree, session, generic, arguments, context)? {
                Some(name) => Binding::Ready(name),
                None => Binding::Deferred(unsupported("class type parameter value")),
            },
        );
    }
    // Parameter binding and body construction have large temporary values.
    // Keep those phases off the stack while resolving an inherited class.
    let pending = match prepare(tree, session, generic, arguments, context)? {
        Preparation::Existing(name) => return Ok(Binding::Ready(name)),
        Preparation::Deferred(name) => return Ok(Binding::Deferred(name)),
        Preparation::Pending(pending) => pending,
    };
    session
        .class_specializations_building
        .push(pending.name.clone());
    let name = bind_bases(tree, session, generic, &pending.env, &pending.aliases)
        .and_then(|bases| finish_specialization(tree, session, generic, pending, bases));
    session.class_specializations_building.pop();
    name.map(Binding::Ready)
}

struct PendingSpecialization {
    name: String,
    parameters: Parameters,
    env: HashMap<String, i128>,
    aliases: HashMap<String, Type>,
}

enum Preparation {
    Existing(String),
    Deferred(AnalyzerError),
    Pending(Box<PendingSpecialization>),
}

/// The specialization that `arguments` named in the tables `env` and
/// `aliases`. The entry keeps their identities from being reused, not their
/// entries.
#[derive(Debug)]
pub(in crate::ast) struct Named {
    arguments: BoundArguments,
    env: std::sync::Weak<HashMap<String, i128>>,
    aliases: std::sync::Weak<HashMap<String, Type>>,
    name: String,
}

impl Named {
    fn of(&self, env: &SharedMap<i128>, aliases: &SharedMap<Type>) -> bool {
        self.env.as_ptr() as usize == env.identity()
            && self.aliases.as_ptr() as usize == aliases.identity()
    }

    /// Whether the tables have been dropped, so that no lookup finds it.
    fn stale(&self) -> bool {
        self.env.strong_count() == 0 || self.aliases.strong_count() == 0
    }
}

#[derive(Clone, Debug, Default)]
pub(in crate::ast) struct BoundArguments {
    pub values: HashMap<String, ConstExpr>,
    pub types: HashMap<String, Type>,
}

// Ordinary type equality ignores nominal declaration identity; specialization
// matching must distinguish separately declared enum and aggregate types (8.25).
impl PartialEq for BoundArguments {
    fn eq(&self, other: &Self) -> bool {
        self.values == other.values
            && self.types.len() == other.types.len()
            && self.types.iter().all(|(name, ty)| {
                other
                    .types
                    .get(name)
                    .is_some_and(|other| spelling(ty) == spelling(other))
            })
    }
}

impl Eq for BoundArguments {}

pub(in crate::ast) fn bind_arguments(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    generic: &str,
    declaration: &celox_sv_syntax::ClassDeclaration,
    native: &celox_sv_syntax::Declarations,
    arguments: &[Argument],
    context: &PackedDimensions,
) -> Converted<Option<BoundArguments>> {
    // The parameters an assignment may set: those of the parameter port
    // list that are not local, in order.
    let ports = declaration
        .parameter_ports
        .iter()
        .flat_map(|&id| &native.parameter_declaration(id).parameters)
        .filter(|parameter| !parameter.local)
        .collect::<Vec<_>>();
    let mut values = HashMap::<String, ConstExpr>::default();
    let mut types = HashMap::<String, Type>::default();
    // The formals assigned, also by an empty named assignment.
    let mut assigned = HashSet::<String>::default();
    let mut named = false;
    for (index, argument) in arguments.iter().enumerate() {
        let parameter = match &argument.name {
            Some(name) => {
                named = true;
                let name = instances::normalize_identifier(&name.spelling);
                ports
                    .iter()
                    .find(|parameter| {
                        instances::normalize_identifier(&parameter.name.spelling) == name
                    })
                    .ok_or_else(|| AnalyzerError::UnknownClassParameter {
                        class: generic.to_owned(),
                        name,
                    })?
            }
            // An ordered assignment cannot follow a named one (A.1.3).
            None if named => {
                return Err(unsupported("positional parameter value after a named one"));
            }
            None => ports.get(index).ok_or_else(|| {
                unsupported(format!("more parameter values than class `{generic}` has"))
            })?,
        };
        let name = instances::normalize_identifier(&parameter.name.spelling);
        if !assigned.insert(name.clone()) {
            return Err(unsupported(format!(
                "duplicate parameter assignment `{name}`"
            )));
        }
        // An empty named assignment keeps the default.
        let Some(value) = argument.value else {
            continue;
        };
        match parameter.kind {
            ParameterKind::Type { .. } => match type_argument(tree, session, value, context)? {
                Some(ty) => {
                    types.insert(name, ty);
                }
                None => {
                    return Ok(None);
                }
            },
            ParameterKind::Value { .. } => {
                values.insert(name, value_argument(tree, session, value, context)?);
            }
        }
    }
    Ok(Some(BoundArguments { values, types }))
}

// These boundaries keep the recursive inheritance frame small in debug and
// optimized builds alike; the specialization limit must produce a diagnostic
// before the host thread runs out of stack.
#[inline(never)]
fn prepare(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    generic: &str,
    arguments: &[Argument],
    context: &PackedDimensions,
) -> Converted<Preparation> {
    let native = tree.declarations();
    let declaration = super::declaration(tree, session, generic)
        .ok_or_else(|| unsupported(format!("class `{generic}`")))?;
    if session.class_declarations_repeated.contains(generic) {
        return Err(unsupported(format!(
            "class `{generic}` declared in a loop generate block"
        )));
    }
    let Some(arguments) = bind_arguments(
        tree,
        session,
        generic,
        declaration,
        native,
        arguments,
        context,
    )?
    else {
        return Ok(Preparation::Deferred(unsupported(
            "class type parameter value",
        )));
    };
    prepare_bound(tree, session, generic, arguments, context)
}

fn prepare_bound(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    generic: &str,
    arguments: BoundArguments,
    context: &PackedDimensions,
) -> Converted<Preparation> {
    let declaration = super::declaration(tree, session, generic)
        .ok_or_else(|| unsupported(format!("class `{generic}`")))?;
    // The declarations of the class are bound in the scope that declares it,
    // the actuals in the scope that names the specialization.
    let (base_env, base_aliases) = declaration_scope(session, generic)
        .unwrap_or_else(|| (context.const_env.clone(), context.type_aliases.clone()));
    // The same arguments in the same tables name the same specialization:
    // a made one needs neither a copy of the tables nor its parameters bound
    // again.
    let named = |session: &Session, name: &String| {
        lookup(session, name).is_some() || session.class_specializations_building.contains(name)
    };
    if let Some(name) = session
        .class_specialization_names
        .get(generic)
        .and_then(|known| {
            known
                .iter()
                .find(|known| known.of(&base_env, &base_aliases) && known.arguments == arguments)
        })
        .map(|known| known.name.clone())
        .filter(|name| named(session, name))
    {
        return Ok(Preparation::Existing(name));
    }
    #[cfg(test)]
    super::super::dimensions::SPECIALIZATION_PREPARATIONS.with(|count| count.set(count.get() + 1));
    let bound = arguments.clone();
    let BoundArguments { values, types } = arguments;
    let mut env = (*base_env).clone();
    let mut aliases = (*base_aliases).clone();
    let mut parameters = Parameters {
        values,
        types,
        literals: HashMap::default(),
        lowered: Vec::new(),
        spelled: Vec::new(),
        arguments: Vec::new(),
        own_env: HashMap::default(),
        own_aliases: HashMap::default(),
    };
    if let Some(id) = declaration.parameter_ports
        && let Some(name) =
            parameters.bind(tree, session, declaration, id, true, &mut env, &mut aliases)?
    {
        return Ok(Preparation::Deferred(unresolved_parameter(&name)));
    }
    if let Some(name) = parameters
        .values
        .keys()
        .chain(parameters.types.keys())
        .next()
    {
        return Err(unsupported(format!(
            "assignment of local parameter `{name}`"
        )));
    }
    let name = if declaration.parameter_ports.is_some() {
        format!("{generic}#({})", parameters.spelled.join(","))
    } else {
        generic.to_owned()
    };
    {
        let names = &mut session.class_specialization_names;
        let known = names.entry(generic.to_owned()).or_default();
        known.retain(|known| !known.stale());
        known.push(Named {
            arguments: bound,
            env: base_env.downgrade(),
            aliases: base_aliases.downgrade(),
            name: name.clone(),
        });
    }
    // A declaration of the class may name the specialization being made,
    // as the type `C` of the default specialization does (8.25).
    if named(session, &name) {
        return Ok(Preparation::Existing(name));
    }
    if session.class_specializations_frozen {
        return Err(unsupported(format!(
            "specialization `{name}` that only a procedure names, as with a value of a procedural localparam"
        )));
    }
    if session.class_specializations.len() >= MAX_SPECIALIZATIONS {
        return Err(unsupported(format!(
            "more than {MAX_SPECIALIZATIONS} class specializations"
        )));
    }
    Ok(Preparation::Pending(Box::new(PendingSpecialization {
        name,
        parameters,
        env,
        aliases,
    })))
}

#[inline(never)]
fn bind_bases(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    generic: &str,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Vec<String>> {
    let native = tree.declarations();
    let declaration = super::declaration(tree, session, generic)
        .ok_or_else(|| unsupported(format!("class `{generic}`")))?;
    // The bases are named in the scope of the parameters; a class does not
    // inherit the declarations of the interface classes it implements.
    let inherited = match &declaration.base {
        Some(base) => vec![base.data_type],
        None if declaration.interface => declaration.interfaces.clone(),
        None => Vec::new(),
    };
    let mut bases = Vec::new();
    for id in inherited {
        if let Some(base) = type_binding::procedural_type(id, native, tree, session, env, aliases)?
            .and_then(|ty| ty.class().map(str::to_owned))
        {
            bases.push(base);
        }
    }
    Ok(bases)
}

#[inline(never)]
fn finish_specialization(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    generic: &str,
    pending: Box<PendingSpecialization>,
    bases: Vec<String>,
) -> Converted<String> {
    let PendingSpecialization {
        name,
        mut parameters,
        mut env,
        mut aliases,
    } = *pending;
    let native = tree.declarations();
    let declaration = super::declaration(tree, session, generic)
        .ok_or_else(|| unsupported(format!("class `{generic}`")))?;
    // The body sees the parameters and types of the bases, hidden by the
    // class's own, and the scope of the specialization (8.25.1).
    for base in &bases {
        inherit(tree, session, base, generic, &mut env, &mut aliases);
    }
    for (key, value) in &parameters.own_env {
        match value {
            Some(value) => env.insert(key.clone(), *value),
            None => env.remove(key),
        };
    }
    aliases.extend(
        parameters
            .own_aliases
            .iter()
            .map(|(key, ty)| (key.clone(), ty.clone())),
    );
    aliases.insert(scope_key(generic), super::handle(&name));
    // The body parameters and typedefs in the order the body declares them.
    let mut body = declaration
        .parameters
        .iter()
        .map(|&id| (native.parameter_declaration(id).tokens.start, Ok(id)))
        .chain(
            declaration
                .type_aliases
                .iter()
                .map(|&id| (native.type_alias(id).tokens.start, Err(id))),
        )
        .collect::<Vec<_>>();
    body.sort_by_key(|(start, _)| *start);
    for (_, item) in body {
        match item {
            Ok(id) => {
                if let Some(name) = parameters.bind(
                    tree,
                    session,
                    declaration,
                    id,
                    false,
                    &mut env,
                    &mut aliases,
                )? {
                    return Err(unresolved_parameter(&name));
                }
            }
            Err(id) => {
                let alias = native.type_alias(id);
                let celox_sv_syntax::TypeAliasTarget::DataType(target) = alias.target else {
                    continue;
                };
                // The type is fixed in the scope of the parameters, whose
                // string values without an integral one it reads as their
                // literals. A typedef that cannot be bound is left out; a
                // use of it reports the type unknown.
                let mut context = PackedDimensions::new(HashMap::default(), &env, &aliases);
                context.parameter_values = parameters.literals.clone().into();
                let ty = type_binding::binding::bind_in(target, native, tree, session, &context)
                    .ok()
                    .flatten()
                    .and_then(|ty| {
                        // Its unpacked dimensions may make it a dynamic
                        // container (IEEE 1800-2023 7.5, 7.8, 7.10).
                        let ty = containers::with_dimensions(
                            ty,
                            &alias.dimensions,
                            tree,
                            session,
                            &context,
                        )
                        .ok()?;
                        type_parameters::resolve(&tree.functions, ty, &env)
                    });
                if let Some(mut ty) = ty {
                    let alias = instances::normalize_identifier(&alias.name.spelling);
                    // The members of an enumeration the class declares are
                    // constants of the class scope (IEEE 1800-2023 6.19,
                    // 8.18), with their type, which a derived class inherits
                    // too. A typedef of another enumeration declares none.
                    if matches!(
                        native.data_type(target).kind,
                        celox_sv_syntax::DataTypeKind::Enum { .. }
                    ) && let Some(domain) = ty.enum_domain.clone()
                    {
                        for member in &domain.members {
                            let mut constants = HashMap::default();
                            constants.insert(member.name.clone(), member.value);
                            constants.insert(dimensions::enum_marker(&member.name), member.value);
                            dimensions::insert_parameter_type_markers(
                                &mut constants,
                                &member.name,
                                ExprType {
                                    width: domain.width,
                                    signed: domain.signed,
                                },
                            );
                            for (key, value) in constants {
                                env.insert(key.clone(), value);
                                parameters.own_env.insert(key, Some(value));
                            }
                            // `C::NAME` reads it as a parameter of the class.
                            parameters.literals.insert(
                                member.name.clone(),
                                Expr::Literal(format_typed_parameter_literal(
                                    member.value,
                                    domain.width,
                                    domain.signed,
                                )),
                            );
                        }
                    }
                    ty.name_declaration(|| {
                        (
                            format!("{name}::{alias}"),
                            type_names::TypedefName::Class {
                                class: name.as_str().into(),
                                name: alias.as_str().into(),
                            },
                        )
                    });
                    parameters.own_aliases.insert(alias.clone(), ty.clone());
                    aliases.insert(alias, ty);
                }
            }
        }
    }
    session.class_specializations.push((
        name.clone(),
        Rc::new(Specialization {
            generic: generic.to_owned(),
            hidden: parameters
                .own_env
                .iter()
                .filter(|(key, value)| value.is_none() && !env.contains_key(*key))
                .map(|(key, _)| key.clone())
                .collect(),
            declared_env: parameters.own_env,
            declared_aliases: parameters.own_aliases,
            env,
            literals: parameters.literals,
            aliases,
            bases,
            arguments: parameters.arguments,
        }),
    ));
    Ok(name)
}

/// The parameters of a specialization as [`specialize`] binds them.
struct Parameters {
    /// The values and types the assignment gives, until they are bound.
    values: HashMap<String, ConstExpr>,
    types: HashMap<String, Type>,
    literals: HashMap<String, Expr>,
    lowered: Vec<Parameter>,
    /// The parameters of the port list as the name of the specialization
    /// spells them.
    spelled: Vec<String>,
    /// The parameters of the port list as `$typename` writes them.
    arguments: Vec<type_names::ClassArgument>,
    /// The constants and types the class declares itself, which hide those
    /// of its bases; a constant without a value hides one of a base too.
    own_env: HashMap<String, Option<i128>>,
    own_aliases: HashMap<String, Type>,
}

impl Parameters {
    /// Record type parameter `ty` of the port list for `$typename`. This and
    /// [`Self::record_value`] stay off the recursive specialization frames,
    /// as the phases below do.
    #[inline(never)]
    fn record_type(&mut self, ty: &Type) {
        self.arguments
            .push(type_names::ClassArgument::Type(ty.clone()));
    }

    /// Record the value of value parameter `parameter` of the port list,
    /// lowered from `first` on, for `$typename`, which writes the value of
    /// an integral parameter.
    #[inline(never)]
    fn record_value(
        &mut self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        parameter: &celox_sv_syntax::Parameter,
        first: usize,
        env: &HashMap<String, i128>,
        aliases: &HashMap<String, Type>,
    ) {
        let ParameterKind::Value {
            data_type,
            dimensions,
            ..
        } = &parameter.kind
        else {
            return;
        };
        let integral = dimensions.is_empty()
            && self.lowered[first..]
                .iter()
                .all(|parameter| parameter.floating.is_none())
            && type_binding::procedural_type(
                *data_type,
                tree.declarations(),
                tree,
                session,
                env,
                aliases,
            )
            .ok()
            .flatten()
            .is_some_and(|ty| {
                matches!(
                    ty.kind(),
                    TypeKind::Bit | TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit
                ) && ty.container.is_none()
                    && ty.class().is_none()
                    && ty.unpacked_ranges.is_empty()
            });
        let name = instances::normalize_identifier(&parameter.name.spelling);
        self.arguments.push(type_names::ClassArgument::Value(
            env.get(&name).copied().filter(|_| integral),
        ));
    }

    /// Bind parameter declaration `id` of class `declaration`, one of its
    /// parameter port list when `port`, in `env` and `aliases`. Return the
    /// first value parameter that cannot yet be evaluated, without publishing
    /// a partially bound specialization.
    fn bind(
        &mut self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        declaration: &celox_sv_syntax::ClassDeclaration,
        id: celox_sv_syntax::ParameterDeclarationId,
        port: bool,
        env: &mut HashMap<String, i128>,
        aliases: &mut HashMap<String, Type>,
    ) -> Converted<Option<String>> {
        let native = tree.declarations();
        for parameter in &native.parameter_declaration(id).parameters {
            let name = instances::normalize_identifier(&parameter.name.spelling);
            // A body parameter of a class with a parameter port list is
            // local (8.25, 6.20.1).
            let local = parameter.local || !port && declaration.parameter_ports.is_some();
            match parameter.kind {
                ParameterKind::Type { default } => {
                    let ty = match self.types.remove(&name).filter(|_| !local) {
                        Some(ty) => ty,
                        None => {
                            let id = default.ok_or_else(|| {
                                unsupported(format!("type parameter `{name}` without a type"))
                            })?;
                            type_binding::procedural_type(id, native, tree, session, env, aliases)?
                                .ok_or_else(|| {
                                    unsupported(format!("type of type parameter `{name}`"))
                                })?
                        }
                    };
                    // The type is fixed in the scope that assigns it.
                    let ty = type_parameters::resolve(&tree.functions, ty, env)
                        .ok_or_else(|| unsupported(format!("type of type parameter `{name}`")))?;
                    if port {
                        self.spelled.push(format!("{name}={}", spelling(&ty)));
                        self.record_type(&ty);
                    }
                    aliases.insert(name.clone(), ty.clone());
                    self.own_aliases.insert(name, ty);
                }
                ParameterKind::Value { .. } => {
                    // The parameter hides all of a constant of the same
                    // name, whose value a string or four-state value
                    // does not replace.
                    let mut keys = parameters::binding_keys(&name, env, 0);
                    for key in &keys {
                        env.remove(key);
                    }
                    let mut overrides = HashMap::default();
                    if !local && let Some(value) = self.values.remove(&name) {
                        overrides.insert(name.clone(), value);
                    }
                    let first = self.lowered.len();
                    parameters::parameters_from_native(
                        &[parameter],
                        tree,
                        session,
                        &mut self.lowered,
                        local,
                        env,
                        aliases,
                        &overrides,
                    )?;
                    let mut typed = true;
                    for parameter in self.lowered[first..].iter().cloned() {
                        typed &= parameter.has_declared_type;
                        keys.extend(parameters::binding_keys(
                            &name,
                            env,
                            parameter.packed_ranges.len(),
                        ));
                        constants::bind_generate_parameter(
                            tree,
                            session,
                            parameter,
                            env,
                            &mut self.literals,
                        );
                    }
                    // A parameter without a type takes the type of its value,
                    // which tells specializations apart too (6.20.2). A
                    // string or four-state value has its literal only.
                    let spelling = match (env.get(&name), self.literals.get(&name)) {
                        (Some(value), _) if typed => value.to_string(),
                        (_, Some(Expr::Literal(literal))) => literal.clone(),
                        (Some(value), _) => value.to_string(),
                        _ => {
                            return Ok(Some(name));
                        }
                    };
                    if port {
                        self.spelled.push(format!("{name}={spelling}"));
                        self.record_value(tree, session, parameter, first, env, aliases);
                    }
                    for key in keys {
                        let value = env.get(&key).copied();
                        self.own_env.insert(key, value);
                    }
                }
            }
        }
        Ok(None)
    }
}

/// Record `env` and `aliases`, those of the current scope, as the scope that
/// declares each class of it, until [`collect`] finds the generate scopes
/// around their declarations: elaborating those scopes binds the types they
/// declare, specializations among them.
pub(in crate::ast) fn declare_scopes(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) {
    let native = tree.declarations();
    let scope = tree.root_unit().ok().map(|unit| unit.tokens);
    let scopes = &mut session.class_declaration_scopes;
    // The classes of the scope share its tables.
    let tables = (
        SharedMap::from(env.clone()),
        SharedMap::from(aliases.clone()),
    );
    for class in &native.classes {
        if super::enclosing_unit(native, class.tokens).map(|unit| unit.tokens) == scope {
            scopes.insert(super::declared_name(tree, class), tables.clone());
        }
    }
}

/// The constants and types of the scope that declares class `generic`,
/// which [`collect`] records.
fn declaration_scope(
    session: &mut Session,
    generic: &str,
) -> Option<(SharedMap<i128>, SharedMap<Type>)> {
    session.class_declaration_scopes.get(generic).cloned()
}

/// The type that argument `id` of a type parameter names, or `None` while a
/// bound of it needs a constant that `context` does not know yet.
pub(in crate::ast) fn type_argument(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::ExprId,
    context: &PackedDimensions,
) -> Converted<Option<Type>> {
    let native = tree.declarations();
    let ty = match &native.expression(id).kind {
        ExprKind::Type(ty) => type_binding::bind(*ty, tree, session, context)?,
        ExprKind::Name(name) => {
            let name = instances::normalize_identifier(&name.spelling);
            match context.type_aliases.get(&name) {
                Some(ty) => Some(ty.clone()),
                None => match super::resolve(tree, session, &name) {
                    // A built-in class such as `process` has no declaration.
                    Some(name) if super::builtin(&name) => Some(super::handle(&name)),
                    Some(name) => {
                        let name = specialize(tree, session, &name, &[], context)?;
                        Some(super::handle(&name))
                    }
                    None => None,
                },
            }
        }
        ExprKind::Parameterized {
            expression,
            arguments,
        } => {
            // `mailbox #(T)` is a built-in type, not a class declaration.
            let segment = |name: &celox_sv_syntax::Name, arguments| celox_sv_syntax::TypeSegment {
                name: name.clone(),
                arguments,
            };
            let path = match &native.expression(*expression).kind {
                ExprKind::Name(name) => vec![segment(name, Some(arguments.clone()))],
                ExprKind::Member {
                    expression: scope,
                    separator,
                    name,
                } if separator == "::" => match &native.expression(*scope).kind {
                    ExprKind::Name(scope) => {
                        vec![segment(scope, None), segment(name, Some(arguments.clone()))]
                    }
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            };
            match super::super::mailboxes::named_type(&path, tree, session, context)? {
                Some(ty) => Some(*ty),
                None => expression_binding::named_class(tree, session, id, native, context)?
                    .map(|name| super::handle(&name)),
            }
        }
        ExprKind::Member { separator, .. } if separator == "::" => {
            scoped_type(tree, session, id, context)?
        }
        _ => None,
    };
    finish(ty, context, tree)
}

/// The type that scope-qualified name `id` names: a type of a class, `C::T`
/// or `C#(...)::T`, or of a package (IEEE 1800-2023 8.23, 26.3).
pub(in crate::ast) fn scoped_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::ExprId,
    context: &PackedDimensions,
) -> Converted<Option<Type>> {
    let native = tree.declarations();
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(id).kind
    else {
        return Ok(None);
    };
    if separator != "::" {
        return Ok(None);
    }
    let member = instances::normalize_identifier(&name.spelling);
    let class = match &native.expression(*expression).kind {
        ExprKind::Name(scope) => {
            let scope = instances::normalize_identifier(&scope.spelling);
            if let Some(class) = super::resolve(tree, session, &scope) {
                // Inside the class, its name refers to the
                // specialization being declared (8.25).
                match context
                    .type_aliases
                    .get(&scope_key(&scope))
                    .or_else(|| context.type_aliases.get(&scope_key(&class)))
                    .and_then(|ty| ty.class().map(str::to_owned))
                {
                    Some(specialization) => Some(specialization),
                    None if super::is_declared(tree, session, &class) => {
                        Some(specialize(tree, session, &class, &[], context)?)
                    }
                    None => Some(class),
                }
            } else {
                let qualified = scope::qualified_name(tree, &scope, &member);
                return Ok(context.type_aliases.get(&qualified).cloned().or_else(
                    || match qualified.as_str() {
                        "std::process" => Some(super::handle(super::super::processes::CLASS)),
                        "std::semaphore" => Some(super::handle(super::super::semaphores::CLASS)),
                        _ => None,
                    },
                ));
            }
        }
        // A class of a package, `p::C::T`, or a specialization.
        ExprKind::Member { .. } | ExprKind::Parameterized { .. } => {
            expression_binding::named_class(tree, session, *expression, native, context)?
        }
        _ => None,
    };
    Ok(class.and_then(|class: String| member_type(tree, session, &class, &member)))
}

/// The type argument `ty`, bound in `context`, fixed there and checked.
fn finish(
    ty: Option<Type>,
    context: &PackedDimensions,
    tree: &AnalysisContext<'_>,
) -> Converted<Option<Type>> {
    let ty = ty.ok_or_else(|| unsupported("class type parameter value"))?;
    // The bounds are those of the scope that assigns the type.
    let Some(ty) = type_parameters::resolve(&tree.functions, ty, &context.const_env) else {
        return Ok(None);
    };
    // Each enumeration declaration is a type of its own (6.19), and so is
    // each struct or union declaration (6.22.1): a typedef names it, which
    // tells it apart from another of the same structure. An anonymous one is
    // not told apart.
    let named = ty.declaration_name().is_some();
    if ty.is_enum && !named {
        return Err(unsupported(
            "enumeration type as a class type parameter value",
        ));
    }
    if !ty.members.is_empty() && !named {
        return Err(unsupported(
            "struct or union type as a class type parameter value",
        ));
    }
    Ok(Some(ty))
}

/// The constant value of argument `id` of a value parameter, as a literal
/// independent of the scope it was written in, with the width and signing
/// of the expression, which a parameter without a type takes (6.20.2).
fn value_argument(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::ExprId,
    context: &PackedDimensions,
) -> Converted<ConstExpr> {
    let expr = constant_binding::bind(id, tree, session, context)?
        .ok_or_else(|| unsupported("class parameter value that is not constant"))?;
    // An unbased unsized literal such as `'1` fills the width of the formal,
    // which it takes when the parameter is bound (5.7.1).
    if let ConstExpr::Literal(literal) = &expr
        && literal.len() == 2
        && literal.starts_with('\'')
    {
        return Ok(expr);
    }
    let Some(value) = eval_ast_const_expr(&tree.functions, &expr, &context.const_env) else {
        // A string or four-state value is its literal, and so is the value
        // of a real expression, or of a parameter with such a value.
        return match expr {
            ConstExpr::Literal(_) => Ok(expr),
            ConstExpr::Ident(name)
                if let Some(literal) = literal_parameter(session, &name, context) =>
            {
                Ok(ConstExpr::Literal(literal))
            }
            _ => match parameters::real_literal(
                &expr,
                TypeKind::Real,
                &context.const_env,
                &HashMap::default(),
            ) {
                Some(Expr::Literal(literal)) => Ok(ConstExpr::Literal(literal)),
                _ => Err(unsupported("class parameter value that is not constant")),
            },
        };
    };
    let ty = type_binding::expression_type(id, tree, session, context)?
        .ok_or_else(|| unsupported("class parameter value without a type"))?;
    Ok(ConstExpr::Literal(format_typed_parameter_literal(
        value, ty.width, ty.signed,
    )))
}

/// The literal of parameter `name` without an integral value, such as a
/// string wider than a constant holds: a parameter of the scope, or of a
/// specialization whose declarations the scope is in.
fn literal_parameter(
    session: &mut Session,
    name: &str,
    context: &PackedDimensions,
) -> Option<String> {
    if let Some(value) = context.parameter_values.get(name) {
        return match value {
            Expr::Literal(literal) => Some(literal.clone()),
            _ => None,
        };
    }
    context
        .type_aliases
        .iter()
        .filter(|(key, _)| key.ends_with("::"))
        .filter_map(|(_, ty)| lookup(session, ty.class()?))
        .find_map(|specialization| {
            if !specialization.hidden.iter().any(|hidden| hidden == name) {
                return None;
            }
            match specialization.literals.get(name)? {
                Expr::Literal(literal) => Some(literal.clone()),
                _ => None,
            }
        })
}

/// A spelling of type `ty` that tells it apart from other types, for the
/// name of a specialization.
pub(in crate::ast) fn spelling(ty: &Type) -> String {
    // A virtual interface type is its interface and modport (IEEE 1800-2023
    // 25.9), not the number that holds its value.
    if let Some(interface) = ty.virtual_interface()
        && ty.unpacked_ranges().is_empty()
        && ty.container.is_none()
    {
        return interface.spelling();
    }
    let mut spelled = if let Some(container) = &ty.container {
        format!(
            "container({:?},{},{})",
            container.layout,
            spelling(&container.element),
            container.key.as_ref().map(spelling).unwrap_or_default()
        )
    } else {
        match (ty.class(), ty.kind()) {
            (Some(class), _) => class.to_owned(),
            (None, TypeKind::Real) => "real".to_owned(),
            (None, TypeKind::Shortreal) => "shortreal".to_owned(),
            (None, TypeKind::String) => "string".to_owned(),
            (None, TypeKind::Object) => "object".to_owned(),
            (None, TypeKind::Process) => "process".to_owned(),
            (None, TypeKind::Event) => "event".to_owned(),
            (None, TypeKind::Bit) => "bit".to_owned(),
            // `reg` is `logic` (6.11.2).
            (None, TypeKind::Logic | TypeKind::Implicit | TypeKind::Reg) => "logic".to_owned(),
        }
    };
    let integral = ty.container.is_none()
        && ty.class().is_none()
        && !matches!(
            ty.kind(),
            TypeKind::Real
                | TypeKind::Shortreal
                | TypeKind::String
                | TypeKind::Object
                | TypeKind::Process
                | TypeKind::Event
        );
    if integral && ty.is_signed() {
        spelled.push_str(" signed");
    }
    let bound = |expr: &ConstExpr| match expr {
        ConstExpr::Literal(literal) => literal.clone(),
        expr => format!("{expr:?}"),
    };
    if integral {
        for range in ty.packed_ranges() {
            spelled.push_str(&format!("[{}:{}]", bound(&range.left), bound(&range.right)));
        }
        // An element of a signed named type is signed although the array
        // is not (7.4.1).
        if let Some(depth) = ty.signed_element_depth {
            spelled.push_str(&format!(" signed@{depth}"));
        }
    }
    // Unpacked dimensions of any element type are part of the type, told
    // apart from packed ones; `[2]` is `[0:1]` (7.4.2).
    for range in ty.unpacked_ranges() {
        spelled.push_str(&format!(
            " [{}:{}]",
            bound(&range.left),
            bound(&range.right)
        ));
    }
    if ty.is_enum || !ty.members.is_empty() {
        // An aggregate is told apart by its whole structure.
        use std::hash::{Hash, Hasher};
        let mut hasher = fxhash::FxHasher::default();
        ty.hash(&mut hasher);
        spelled.push_str(&format!("#{:016x}", hasher.finish()));
    }
    if let Some(name) = ty.declaration_name() {
        spelled.push_str(&format!("@{name}"));
    }
    spelled
}

/// Make the specializations that the types and class scopes of the current
/// module name, and the default specialization of each parameterized
/// interface class, which is checked even when nothing names it. A name
/// inside a class is bound in each specialization of the class, until no
/// new specialization appears.
pub(in crate::ast) fn collect(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) {
    session.class_specializations_frozen = false;
    for (generic, arguments) in session.foreign_arguments.clone() {
        let pending = match prepare_bound(tree, session, &generic, arguments.clone(), context) {
            Ok(Preparation::Pending(pending)) => pending,
            Ok(Preparation::Existing(name)) => {
                session.foreign_names.push((generic, arguments, name));
                continue;
            }
            Ok(Preparation::Deferred(error)) | Err(error) => {
                session.foreign_error = Some(error);
                return;
            }
        };
        session
            .class_specializations_building
            .push(pending.name.clone());
        let result = bind_bases(tree, session, &generic, &pending.env, &pending.aliases)
            .and_then(|bases| finish_specialization(tree, session, &generic, pending, bases));
        session.class_specializations_building.pop();
        match result {
            Ok(name) => session.foreign_names.push((generic, arguments, name)),
            Err(error) => {
                session.foreign_error = Some(error);
                return;
            }
        }
    }
    let native = tree.declarations();
    let within = |outer: celox_sv_syntax::TokenRange, inner: celox_sv_syntax::TokenRange| {
        outer.start <= inner.start && inner.end <= outer.end
    };
    // The current scope: a module, interface, program or package, or the
    // compilation unit outside them.
    let unit = tree.root_unit().ok().map(|unit| unit.tokens);
    let in_scope = |tokens: celox_sv_syntax::TokenRange| match unit {
        Some(unit) => within(unit, tokens),
        None => super::enclosing_unit(native, tokens).is_none(),
    };
    let classes = native
        .classes
        .iter()
        .filter(|class| super::enclosing_unit(native, class.tokens).map(|unit| unit.tokens) == unit)
        .collect::<Vec<_>>();
    // The classes of the scope by the names its declarations use.
    let mut names = classes
        .iter()
        .map(|class| {
            (
                instances::normalize_identifier(&class.name.spelling),
                super::declared_name(tree, class),
            )
        })
        .collect::<HashMap<_, _>>();
    for name in session.foreign.names() {
        names.entry(name.clone()).or_insert(name);
    }
    for (alias, name) in &scope::imported(tree).aliases {
        if session.foreign.contains(name) {
            names.entry(alias.clone()).or_insert(name.clone());
        }
    }
    // The class scopes: each class declaration, and each out-of-block
    // method definition, which is in the scope of its class (8.24).
    let mut scopes = classes
        .iter()
        .map(|class| (class.tokens, super::declared_name(tree, class)))
        .collect::<Vec<_>>();
    for id in native.subroutine_ids() {
        let subroutine = native.subroutine(id);
        if let [scope] = subroutine.scope.as_slice()
            && in_scope(subroutine.tokens)
            && let Some(class) = names.get(&instances::normalize_identifier(&scope.name.spelling))
        {
            scopes.push((subroutine.tokens, class.clone()));
        }
    }
    // The innermost class scope that contains `tokens`.
    let enclosing = |tokens: celox_sv_syntax::TokenRange| {
        scopes
            .iter()
            .filter(|(scope, _)| within(*scope, tokens))
            .min_by_key(|(scope, _)| scope.end - scope.start)
            .map(|(_, class)| class.clone())
    };
    let mut references: Vec<(
        Option<String>,
        String,
        &[Argument],
        celox_sv_syntax::TokenRange,
    )> = Vec::new();
    for class in &classes {
        if class.parameter_ports.is_none() || class.interface {
            references.push((None, super::declared_name(tree, class), &[], class.tokens));
        }
    }
    for ty in &native.types {
        if !in_scope(ty.tokens) {
            continue;
        }
        // A class, or the class scope of a type it declares, `C#(...)::T`.
        // An unadorned `C::T` names the specialization being declared
        // inside `C`, and needs a specialization elsewhere (8.25.1).
        if let celox_sv_syntax::DataTypeKind::Named(path) = &ty.kind
            && let [segment] | [segment, _] = path.as_slice()
            && (path.len() == 1 || segment.arguments.is_some())
        {
            let name = instances::normalize_identifier(&segment.name.spelling);
            if let Some(name) = names.get(&name).cloned() {
                references.push((
                    enclosing(ty.tokens),
                    name,
                    segment.arguments.as_deref().unwrap_or_default(),
                    ty.tokens,
                ));
            }
        }
    }
    for ty in &native.types {
        if !in_scope(ty.tokens) {
            continue;
        }
        if let celox_sv_syntax::DataTypeKind::Named(path) = &ty.kind
            && let [package, member] = path.as_slice()
            && member.arguments.is_some()
        {
            let name = format!(
                "{}::{}",
                instances::normalize_identifier(&package.name.spelling),
                instances::normalize_identifier(&member.name.spelling)
            );
            if session.foreign.contains(&name) {
                references.push((
                    enclosing(ty.tokens),
                    name,
                    member.arguments.as_deref().unwrap(),
                    ty.tokens,
                ));
            }
        }
    }
    for expression in &native.expressions {
        if !in_scope(expression.tokens) {
            continue;
        }
        if let ExprKind::Parameterized {
            expression: scope,
            arguments,
        } = &expression.kind
        {
            let name = match &native.expression(*scope).kind {
                ExprKind::Name(name) => instances::normalize_identifier(&name.spelling),
                ExprKind::Member {
                    expression,
                    separator,
                    name,
                } if separator == "::" => {
                    let ExprKind::Name(package) = &native.expression(*expression).kind else {
                        continue;
                    };
                    format!(
                        "{}::{}",
                        instances::normalize_identifier(&package.spelling),
                        instances::normalize_identifier(&name.spelling)
                    )
                }
                _ => continue,
            };
            if let Some(name) = names.get(&name).cloned() {
                references.push((
                    enclosing(expression.tokens),
                    name,
                    arguments,
                    expression.tokens,
                ));
            }
        }
    }
    // Outside classes, an assignment is bound in each generate scope that
    // contains it, which may give its genvars different values.
    let items = generate::items(tree, session, &context.const_env, &context.type_aliases)
        .unwrap_or_default();
    // The scope that declares each class: the innermost generate scope
    // around its declaration, or the module.
    for class in &classes {
        let item = items
            .iter()
            .filter(|item| within(native.body_item(item.id).tokens, class.tokens))
            .min_by_key(|item| {
                let tokens = native.body_item(item.id).tokens;
                tokens.end - tokens.start
            });
        let scope = match item {
            Some(item) => (
                SharedMap::from((*item.env).clone()),
                SharedMap::from(item.type_aliases(&context.type_aliases).into_owned()),
            ),
            None => (context.const_env.clone(), context.type_aliases.clone()),
        };
        let name = super::declared_name(tree, class);
        if item.is_some_and(|item| items.iter().filter(|other| other.id == item.id).count() > 1) {
            session.class_declarations_repeated.insert(name.clone());
        }
        session.class_declaration_scopes.insert(name, scope);
    }
    // Whether `tokens` is in a block of a generate construct.
    let generated = |tokens: celox_sv_syntax::TokenRange| {
        native.body_items.iter().any(|item| {
            let contains =
                |id: &celox_sv_syntax::BodyItemId| within(native.body_item(*id).tokens, tokens);
            in_scope(item.tokens)
                && match &item.kind {
                    celox_sv_syntax::BodyItemKind::Conditional {
                        then_body,
                        otherwise,
                        ..
                    } => contains(then_body) || otherwise.as_ref().is_some_and(contains),
                    celox_sv_syntax::BodyItemKind::Case { arms, .. } => {
                        arms.iter().any(|arm| contains(&arm.body))
                    }
                    celox_sv_syntax::BodyItemKind::Loop { body, .. } => contains(body),
                    _ => false,
                }
        })
    };
    let module_scopes = |tokens: celox_sv_syntax::TokenRange| {
        let mut scopes = items
            .iter()
            .filter(|item| within(native.body_item(item.id).tokens, tokens))
            .map(|item| {
                let mut scope = context.clone();
                scope.const_env = (*item.env).clone().into();
                scope.type_aliases = item.type_aliases(&context.type_aliases).into_owned().into();
                scope
            })
            .collect::<Vec<_>>();
        // A name in a generate block that elaboration does not select is
        // in no scope (27.5).
        if scopes.is_empty() && !generated(tokens) {
            scopes.push(context.clone());
        }
        scopes
    };
    let mut done = HashSet::<(usize, String)>::default();
    // A class scope's references with the same spelling name the same
    // specialization: each is made once, in the scope's context built once.
    let mut spelled = HashSet::<(String, String, String)>::default();
    let mut contexts = HashMap::<String, PackedDimensions>::default();
    loop {
        let before = session.class_specializations.len();
        for (index, (scope, name, arguments, tokens)) in references.iter().enumerate() {
            let scopes = match scope {
                None => {
                    if done.insert((index, String::new())) {
                        for context in module_scopes(*tokens) {
                            // A specialization that cannot be made reports
                            // why where it is used.
                            let _ = specialize(tree, session, name, arguments, &context);
                        }
                    }
                    continue;
                }
                Some(scope) => of(session, scope),
            };
            for scope in scopes {
                let shared = !crate::ast::dimensions::unshared_discovery();
                if !done.insert((index, scope.clone()))
                    || (shared
                        && !spelled.insert((
                            scope.clone(),
                            name.clone(),
                            argument_spelling(native, arguments),
                        )))
                {
                    continue;
                }
                if !shared {
                    contexts.remove(&scope);
                }
                #[cfg(test)]
                crate::ast::dimensions::CLASS_REFERENCE_SPECIALIZATIONS
                    .with(|count| count.set(count.get() + 1));
                let context = contexts.entry(scope.clone()).or_insert_with(|| {
                    let mut context = context.clone();
                    let (env, aliases) =
                        shared_scope(session, &scope, &context.const_env, &context.type_aliases);
                    context.const_env = env;
                    context.type_aliases = aliases;
                    context
                });
                let _ = specialize(tree, session, name, arguments, context);
            }
        }
        let after = session.class_specializations.len();
        if after == before || after >= MAX_SPECIALIZATIONS {
            break;
        }
    }
}

/// The source text of the parameter value assignment `arguments`.
fn argument_spelling(native: &celox_sv_syntax::Declarations, arguments: &[Argument]) -> String {
    arguments
        .iter()
        .map(|argument| {
            let name = argument
                .name
                .as_ref()
                .map_or("", |name| name.spelling.as_str());
            let value = argument.value.map_or_else(String::new, |value| {
                native.token_text(native.expression(value).tokens)
            });
            format!("{name}={value}")
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {

    use std::path::Path;

    #[test]
    fn inheritance_specialization_uses_the_default_thread_stack() {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let mut source = String::from("module Top;");
                for index in 0..16 {
                    source.push_str(&format!(
                        "class Layer{index} #(int N = 0) extends Layer{}#(N + 1); endclass ",
                        index + 1,
                    ));
                }
                source.push_str(
                    "class Layer16 #(int N = 0); localparam int VALUE = N; endclass
                     Layer0#() object; initial $display(Layer0#()::VALUE); endmodule",
                );
                crate::analyze_source(&source, Path::new("deep-class-inheritance.sv")).unwrap();

                let error = crate::analyze_source(
                    "module Top;
                     class Recursive #(int N = 0) extends Recursive#(N + 1); endclass
                     Recursive object; endmodule",
                    Path::new("unbounded-class-inheritance.sv"),
                )
                .unwrap_err()
                .to_string();
                assert!(error.contains("nested more than 64 deep"), "{error}");
                assert!(error.contains("class `Recursive`"), "{error}");
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
