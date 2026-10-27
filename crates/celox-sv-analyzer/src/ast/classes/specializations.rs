//! Parameterized classes (IEEE 1800-2023 8.25).
//!
//! Each unique set of parameter values and types of a parameterized class is
//! a specialization: a class of its own, named by the class and the set,
//! such as `C#(N=4,T=bit)`. A specialization keeps the constants and types
//! its declarations see: the parameters, and the scope `C::`, which inside
//! the class refers to the specialization being declared, while the type
//! `C` is the default specialization there too. A class without parameters
//! is its own only specialization.

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
fn lineage(tree: &AnalysisContext<'_>, name: &str) -> Vec<Rc<Specialization>> {
    let mut lineage: Vec<Rc<Specialization>> = Vec::new();
    let mut seen = HashSet::default();
    // A class that extends itself is rejected where it is laid out.
    let mut pending = vec![(name.to_owned(), false)];
    while let Some((name, visited)) = pending.pop() {
        let Some(specialization) = lookup(tree, &name) else {
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

/// A specialization in `class_specializations_building`, which it leaves
/// when dropped.
struct Building<'a>(&'a std::cell::RefCell<Vec<String>>);

impl Drop for Building<'_> {
    fn drop(&mut self) {
        self.0.borrow_mut().pop();
    }
}

/// The specialization being made, which [`specialize`] leaves when dropped.
struct Nesting<'a>(&'a std::cell::Cell<usize>);

impl Drop for Nesting<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

/// The key under which the scope of a class finds the specialization being
/// declared: inside a parameterized class and its out-of-block methods,
/// its name alone as the prefix of `::` refers to it, while as a type the
/// name alone is the default specialization (IEEE 1800-2023 8.25, 8.25.1).
pub(in crate::ast) fn scope_key(generic: &str) -> String {
    format!("{generic}::")
}

/// The specialization `name`, if one has been made.
pub(in crate::ast) fn lookup(tree: &AnalysisContext<'_>, name: &str) -> Option<Rc<Specialization>> {
    tree.state
        .class_specializations
        .borrow()
        .iter()
        .find(|(known, _)| known == name)
        .map(|(_, specialization)| specialization.clone())
}

/// The class declaration that class `name` specializes.
pub(in crate::ast) fn generic(tree: &AnalysisContext<'_>, name: &str) -> String {
    lookup(tree, name).map_or_else(
        || name.to_owned(),
        |specialization| specialization.generic.clone(),
    )
}

/// The specializations made of the class declared as `generic`, in the
/// order they were made.
pub(in crate::ast) fn of(tree: &AnalysisContext<'_>, generic: &str) -> Vec<String> {
    tree.state
        .class_specializations
        .borrow()
        .iter()
        .filter(|(_, specialization)| specialization.generic == generic)
        .map(|(name, _)| name.clone())
        .collect()
}

/// `env` and `aliases` with the parameters of class `name` added: the
/// scope the declarations of the class see.
pub(in crate::ast) fn scope(
    tree: &AnalysisContext<'_>,
    name: &str,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> (HashMap<String, i128>, HashMap<String, Type>) {
    let mut env = env.clone();
    let mut aliases = aliases.clone();
    let own = generic(tree, name);
    for specialization in lineage(tree, name) {
        env.extend(specialization.env.clone());
        for key in &specialization.hidden {
            env.remove(key);
        }
        aliases.extend(inherited(&own, &specialization));
    }
    (env, aliases)
}

/// `env` and `aliases` with what class `name` and its bases declare added,
/// which class `own` extending it inherits (IEEE 1800-2023 8.13): not the
/// scope around their declarations.
fn inherit(
    tree: &AnalysisContext<'_>,
    name: &str,
    own: &str,
    env: &mut HashMap<String, i128>,
    aliases: &mut HashMap<String, Type>,
) {
    if lookup(tree, name).is_none()
        && let Some(members) = scope::imported(&tree.state).class_member_types.get(name)
    {
        aliases.extend(members.clone());
    }
    for specialization in lineage(tree, name) {
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
    tree: &AnalysisContext<'_>,
    name: &str,
    dimensions: &PackedDimensions,
    literals: &HashMap<String, Expr>,
) -> (PackedDimensions, HashMap<String, Expr>) {
    let mut dimensions = dimensions.clone();
    let mut literals = literals.clone();
    let own = generic(tree, name);
    for specialization in lineage(tree, name) {
        dimensions.const_env.extend(specialization.env.clone());
        for key in &specialization.hidden {
            dimensions.const_env.remove(key);
        }
        dimensions
            .type_aliases
            .extend(inherited(&own, &specialization));
        literals.extend(specialization.literals.clone());
    }
    (dimensions, literals)
}

/// The value of parameter `parameter` of class `name`, as a literal.
pub(in crate::ast) fn parameter(
    tree: &AnalysisContext<'_>,
    name: &str,
    parameter: &str,
) -> Option<Expr> {
    lineage(tree, name)
        .iter()
        .rev()
        .find_map(|specialization| specialization.literals.get(parameter).cloned())
}

/// The type parameter or typedef `member` of class `name`, declared in it
/// or inherited.
pub(in crate::ast) fn member_type(
    tree: &AnalysisContext<'_>,
    name: &str,
    member: &str,
) -> Option<Type> {
    let generic = generic(tree, name);
    lineage(tree, name)
        .iter()
        .rev()
        .find_map(|specialization| {
            // The class name itself is not a member.
            (member != scope_key(&specialization.generic) && member != scope_key(&generic))
                .then(|| specialization.aliases.get(member).cloned())
                .flatten()
        })
        .or_else(|| {
            scope::imported(&tree.state)
                .class_member_types
                .get(name)?
                .get(member)
                .cloned()
        })
}

/// Only types declared by a class and its bases cross the scope boundary.
/// Resolve bounds in their owning specialization, before caller names can
/// reinterpret them (6.18, 8.13, 8.23, 8.25).
pub(in crate::ast) fn export_member_types(
    tree: &AnalysisContext<'_>,
) -> HashMap<String, HashMap<String, Type>> {
    let imported = scope::imported(&tree.state);
    let mut result = imported.class_member_types.clone();
    for declaration in super::scope_declarations(tree) {
        for name in super::specializations_of(tree, declaration) {
            let mut members = HashMap::default();
            for specialization in lineage(tree, &name) {
                for base in &specialization.bases {
                    if lookup(tree, base).is_none()
                        && let Some(inherited) = imported.class_member_types.get(base)
                    {
                        members.extend(inherited.clone());
                    }
                }
                for (member, ty) in &specialization.declared_aliases {
                    if let Some(ty) =
                        type_parameters::resolve(&tree.state, ty.clone(), &specialization.env)
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
    generic: &str,
    arguments: &[Argument],
    context: &PackedDimensions,
) -> Converted<String> {
    let depth = &tree.state.class_specialization_depth;
    if depth.get() >= MAX_NESTING {
        return Err(unsupported(format!(
            "class specializations nested more than {MAX_NESTING} deep, as when class `{generic}` extends a specialization of itself"
        )));
    }
    depth.set(depth.get() + 1);
    let _nesting = Nesting(depth);
    // Parameter binding and body construction have large temporary values.
    // Keep those phases off the stack while resolving an inherited class.
    let pending = match prepare(tree, generic, arguments, context)? {
        Preparation::Existing(name) => return Ok(name),
        Preparation::Pending(pending) => pending,
    };
    tree.state
        .class_specializations_building
        .borrow_mut()
        .push(pending.name.clone());
    let _building = Building(&tree.state.class_specializations_building);
    let bases = bind_bases(tree, generic, &pending.env, &pending.aliases)?;
    finish_specialization(tree, generic, pending, bases)
}

struct PendingSpecialization {
    name: String,
    parameters: Parameters,
    env: HashMap<String, i128>,
    aliases: HashMap<String, Type>,
}

enum Preparation {
    Existing(String),
    Pending(Box<PendingSpecialization>),
}

// These boundaries keep the recursive inheritance frame small in debug and
// optimized builds alike; the specialization limit must produce a diagnostic
// before the host thread runs out of stack.
#[inline(never)]
fn prepare(
    tree: &AnalysisContext<'_>,
    generic: &str,
    arguments: &[Argument],
    context: &PackedDimensions,
) -> Converted<Preparation> {
    let native = tree.declarations();
    let declaration = super::declaration(tree, generic)
        .ok_or_else(|| unsupported(format!("class `{generic}`")))?;
    if tree
        .state
        .class_declarations_repeated
        .borrow()
        .contains(generic)
    {
        return Err(unsupported(format!(
            "class `{generic}` declared in a loop generate block"
        )));
    }
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
            ParameterKind::Type { .. } => {
                types.insert(name, type_argument(tree, value, context)?);
            }
            ParameterKind::Value { .. } => {
                values.insert(name, value_argument(tree, value, context)?);
            }
        }
    }
    // The declarations of the class are bound in the scope that declares it,
    // the actuals in the scope that names the specialization.
    let (mut env, mut aliases) = declaration_scope(tree, generic)
        .unwrap_or_else(|| ((*context.const_env).clone(), context.type_aliases.clone()));
    let mut parameters = Parameters {
        values,
        types,
        literals: HashMap::default(),
        lowered: Vec::new(),
        spelled: Vec::new(),
        own_env: HashMap::default(),
        own_aliases: HashMap::default(),
    };
    if let Some(id) = declaration.parameter_ports {
        parameters.bind(tree, declaration, id, true, &mut env, &mut aliases)?;
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
    // A declaration of the class may name the specialization being made,
    // as the type `C` of the default specialization does (8.25).
    if lookup(tree, &name).is_some()
        || tree
            .state
            .class_specializations_building
            .borrow()
            .contains(&name)
    {
        return Ok(Preparation::Existing(name));
    }
    if tree.state.class_specializations_frozen.get() {
        return Err(unsupported(format!(
            "specialization `{name}` that only a procedure names, as with a value of a procedural localparam"
        )));
    }
    if tree.state.class_specializations.borrow().len() >= MAX_SPECIALIZATIONS {
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
    generic: &str,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Vec<String>> {
    let native = tree.declarations();
    let declaration = super::declaration(tree, generic)
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
        if let Some(base) = type_binding::procedural_type(id, native, tree, env, aliases)?
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
    let declaration = super::declaration(tree, generic)
        .ok_or_else(|| unsupported(format!("class `{generic}`")))?;
    // The body sees the parameters and types of the bases, hidden by the
    // class's own, and the scope of the specialization (8.25.1).
    for base in &bases {
        inherit(tree, base, generic, &mut env, &mut aliases);
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
            Ok(id) => parameters.bind(tree, declaration, id, false, &mut env, &mut aliases)?,
            Err(id) => {
                let alias = native.type_alias(id);
                let celox_sv_syntax::TypeAliasTarget::DataType(target) = alias.target else {
                    continue;
                };
                // The type is fixed in the scope of the parameters. A typedef
                // that cannot be bound is left out; a use of it reports the
                // type unknown.
                let context = PackedDimensions::new(HashMap::default(), &env, &aliases);
                let ty = type_binding::procedural_type(target, native, tree, &env, &aliases)
                    .ok()
                    .flatten()
                    .and_then(|ty| {
                        let ranges = constant_binding::bind_unpacked_ranges(
                            &alias.dimensions,
                            tree,
                            &context,
                        )
                        .ok()?;
                        type_parameters::resolve(
                            &tree.state,
                            types::type_with_unpacked_ranges(ty, ranges),
                            &env,
                        )
                    });
                if let Some(ty) = ty {
                    let name = instances::normalize_identifier(&alias.name.spelling);
                    parameters.own_aliases.insert(name.clone(), ty.clone());
                    aliases.insert(name, ty);
                }
            }
        }
    }
    tree.state.class_specializations.borrow_mut().push((
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
    /// The constants and types the class declares itself, which hide those
    /// of its bases; a constant without a value hides one of a base too.
    own_env: HashMap<String, Option<i128>>,
    own_aliases: HashMap<String, Type>,
}

impl Parameters {
    /// Bind parameter declaration `id` of class `declaration`, one of its
    /// parameter port list when `port`, in `env` and `aliases`.
    fn bind(
        &mut self,
        tree: &AnalysisContext<'_>,
        declaration: &celox_sv_syntax::ClassDeclaration,
        id: celox_sv_syntax::ParameterDeclarationId,
        port: bool,
        env: &mut HashMap<String, i128>,
        aliases: &mut HashMap<String, Type>,
    ) -> Converted<()> {
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
                            type_binding::procedural_type(id, native, tree, env, aliases)?
                                .ok_or_else(|| {
                                    unsupported(format!("type of type parameter `{name}`"))
                                })?
                        }
                    };
                    // The type is fixed in the scope that assigns it.
                    let ty = type_parameters::resolve(&tree.state, ty, env)
                        .ok_or_else(|| unsupported(format!("type of type parameter `{name}`")))?;
                    if port {
                        self.spelled.push(format!("{name}={}", spelling(&ty)));
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
                            &tree.state,
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
                            return Err(unsupported(format!(
                                "class parameter `{name}` without a constant value"
                            )));
                        }
                    };
                    if port {
                        self.spelled.push(format!("{name}={spelling}"));
                    }
                    for key in keys {
                        let value = env.get(&key).copied();
                        self.own_env.insert(key, value);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Record `env` and `aliases`, those of the current scope, as the scope that
/// declares each class of it, until [`collect`] finds the generate scopes
/// around their declarations: elaborating those scopes binds the types they
/// declare, specializations among them.
pub(in crate::ast) fn declare_scopes(
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) {
    let native = tree.declarations();
    let scope = tree.root_unit().ok().map(|unit| unit.tokens);
    let mut scopes = tree.state.class_declaration_scopes.borrow_mut();
    for class in &native.classes {
        if super::enclosing_unit(native, class.tokens).map(|unit| unit.tokens) == scope {
            scopes.insert(
                super::declared_name(tree, class),
                (env.clone(), aliases.clone()),
            );
        }
    }
}

/// The constants and types of the scope that declares class `generic`,
/// which [`collect`] records.
fn declaration_scope(
    tree: &AnalysisContext<'_>,
    generic: &str,
) -> Option<(HashMap<String, i128>, HashMap<String, Type>)> {
    tree.state
        .class_declaration_scopes
        .borrow()
        .get(generic)
        .cloned()
}

/// The type that argument `id` of a type parameter names.
fn type_argument(
    tree: &AnalysisContext<'_>,
    id: celox_sv_syntax::ExprId,
    context: &PackedDimensions,
) -> Converted<Type> {
    let native = tree.declarations();
    let ty = match &native.expression(id).kind {
        ExprKind::Type(ty) => type_binding::bind(*ty, tree, context)?,
        ExprKind::Name(name) => {
            let name = instances::normalize_identifier(&name.spelling);
            match context.type_aliases.get(&name) {
                Some(ty) => Some(ty.clone()),
                None => match super::resolve(tree, &name) {
                    Some(name) => {
                        let name = specialize(tree, &name, &[], context)?;
                        Some(super::handle(&name))
                    }
                    None => None,
                },
            }
        }
        ExprKind::Parameterized {
            expression,
            arguments,
        } => match &native.expression(*expression).kind {
            ExprKind::Name(name) => {
                match super::resolve(tree, &instances::normalize_identifier(&name.spelling)) {
                    Some(name) => {
                        let name = specialize(tree, &name, arguments, context)?;
                        Some(super::handle(&name))
                    }
                    None => None,
                }
            }
            _ => None,
        },
        // A type of a class, `C::T` or `C#(...)::T`, or of a package
        // (IEEE 1800-2023 8.23, 26.3).
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "::" => {
            let member = instances::normalize_identifier(&name.spelling);
            let class = match &native.expression(*expression).kind {
                ExprKind::Name(scope) => {
                    let scope = instances::normalize_identifier(&scope.spelling);
                    if let Some(class) = super::resolve(tree, &scope) {
                        // Inside the class, its name refers to the
                        // specialization being declared (8.25).
                        match context
                            .type_aliases
                            .get(&scope_key(&scope))
                            .or_else(|| context.type_aliases.get(&scope_key(&class)))
                            .and_then(|ty| ty.class().map(str::to_owned))
                        {
                            Some(specialization) => Some(specialization),
                            None if super::is_declared(tree, &class) => {
                                Some(specialize(tree, &class, &[], context)?)
                            }
                            None => Some(class),
                        }
                    } else {
                        let qualified = scope::qualified_name(&tree.state, &scope, &member);
                        return finish(
                            context.type_aliases.get(&qualified).cloned(),
                            context,
                            tree,
                        );
                    }
                }
                ExprKind::Parameterized {
                    expression,
                    arguments,
                } => match &native.expression(*expression).kind {
                    ExprKind::Name(scope) => {
                        match super::resolve(
                            tree,
                            &instances::normalize_identifier(&scope.spelling),
                        ) {
                            Some(class) => Some(specialize(tree, &class, arguments, context)?),
                            None => None,
                        }
                    }
                    _ => None,
                },
                _ => None,
            };
            class.and_then(|class: String| member_type(tree, &class, &member))
        }
        _ => None,
    };
    finish(ty, context, tree)
}

/// The type argument `ty`, bound in `context`, fixed there and checked.
fn finish(
    ty: Option<Type>,
    context: &PackedDimensions,
    tree: &AnalysisContext<'_>,
) -> Converted<Type> {
    let ty = ty.ok_or_else(|| unsupported("class type parameter value"))?;
    // The bounds are those of the scope that assigns the type.
    let ty = type_parameters::resolve(&tree.state, ty, &context.const_env)
        .ok_or_else(|| unsupported("class type parameter value"))?;
    // Each enumeration declaration is a type of its own, which the bound
    // type does not tell apart from another of the same base (6.19).
    if ty.is_enum {
        return Err(unsupported(
            "enumeration type as a class type parameter value",
        ));
    }
    // So is each struct or union declaration (6.22.1), whose identity the
    // bound type does not keep either.
    if !ty.members.is_empty() {
        return Err(unsupported(
            "struct or union type as a class type parameter value",
        ));
    }
    Ok(ty)
}

/// The constant value of argument `id` of a value parameter, as a literal
/// independent of the scope it was written in, with the width and signing
/// of the expression, which a parameter without a type takes (6.20.2).
fn value_argument(
    tree: &AnalysisContext<'_>,
    id: celox_sv_syntax::ExprId,
    context: &PackedDimensions,
) -> Converted<ConstExpr> {
    let expr = constant_binding::bind(id, tree, context)?
        .ok_or_else(|| unsupported("class parameter value that is not constant"))?;
    // An unbased unsized literal such as `'1` fills the width of the formal,
    // which it takes when the parameter is bound (5.7.1).
    if let ConstExpr::Literal(literal) = &expr
        && literal.len() == 2
        && literal.starts_with('\'')
    {
        return Ok(expr);
    }
    let Some(value) = eval_ast_const_expr(&tree.state, &expr, &context.const_env) else {
        // A string or four-state value is its literal, and so is the value
        // of a real expression.
        return match expr {
            ConstExpr::Literal(_) => Ok(expr),
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
    let ty = type_binding::expression_type(id, tree, context)?
        .ok_or_else(|| unsupported("class parameter value without a type"))?;
    Ok(ConstExpr::Literal(format_typed_parameter_literal(
        value, ty.width, ty.signed,
    )))
}

/// A spelling of type `ty` that tells it apart from other types, for the
/// name of a specialization.
fn spelling(ty: &Type) -> String {
    let mut spelled = match (ty.class(), ty.kind()) {
        (Some(class), _) => class.to_owned(),
        (None, TypeKind::Real) => "real".to_owned(),
        (None, TypeKind::Shortreal) => "shortreal".to_owned(),
        (None, TypeKind::String) => "string".to_owned(),
        (None, TypeKind::Object) => "object".to_owned(),
        (None, TypeKind::Bit) => "bit".to_owned(),
        // `reg` is `logic` (6.11.2).
        (None, TypeKind::Logic | TypeKind::Implicit | TypeKind::Reg) => "logic".to_owned(),
    };
    let integral = ty.class().is_none()
        && !matches!(
            ty.kind(),
            TypeKind::Real | TypeKind::Shortreal | TypeKind::String | TypeKind::Object
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
    spelled
}

/// Make the specializations that the types and class scopes of the current
/// module name, and the default specialization of each parameterized
/// interface class, which is checked even when nothing names it. A name
/// inside a class is bound in each specialization of the class, until no
/// new specialization appears.
pub(in crate::ast) fn collect(tree: &AnalysisContext<'_>, context: &PackedDimensions) {
    tree.state.class_specializations_frozen.set(false);
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
    if classes.is_empty() {
        return;
    }
    // The classes of the scope by the names its declarations use.
    let names = classes
        .iter()
        .map(|class| {
            (
                instances::normalize_identifier(&class.name.spelling),
                super::declared_name(tree, class),
            )
        })
        .collect::<HashMap<_, _>>();
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
    for expression in &native.expressions {
        if !in_scope(expression.tokens) {
            continue;
        }
        if let ExprKind::Parameterized {
            expression: scope,
            arguments,
        } = &expression.kind
            && let ExprKind::Name(name) = &native.expression(*scope).kind
        {
            let name = instances::normalize_identifier(&name.spelling);
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
    let items =
        generate::items(tree, &context.const_env, &context.type_aliases).unwrap_or_default();
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
                (*item.env).clone(),
                item.type_aliases(&context.type_aliases).into_owned(),
            ),
            None => ((*context.const_env).clone(), context.type_aliases.clone()),
        };
        let name = super::declared_name(tree, class);
        if item.is_some_and(|item| items.iter().filter(|other| other.id == item.id).count() > 1) {
            tree.state
                .class_declarations_repeated
                .borrow_mut()
                .insert(name.clone());
        }
        tree.state
            .class_declaration_scopes
            .borrow_mut()
            .insert(name, scope);
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
                scope.type_aliases = item.type_aliases(&context.type_aliases).into_owned();
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
    loop {
        let before = tree.state.class_specializations.borrow().len();
        for (index, (scope, name, arguments, tokens)) in references.iter().enumerate() {
            let scopes = match scope {
                None => {
                    if done.insert((index, String::new())) {
                        for context in module_scopes(*tokens) {
                            // A specialization that cannot be made reports
                            // why where it is used.
                            let _ = specialize(tree, name, arguments, &context);
                        }
                    }
                    continue;
                }
                Some(scope) => of(tree, scope),
            };
            for scope in scopes {
                if !done.insert((index, scope.clone())) {
                    continue;
                }
                let mut context = context.clone();
                let (env, aliases) =
                    self::scope(tree, &scope, &context.const_env, &context.type_aliases);
                context.const_env = env.into();
                context.type_aliases = aliases;
                let _ = specialize(tree, name, arguments, &context);
            }
        }
        let after = tree.state.class_specializations.borrow().len();
        if after == before || after >= MAX_SPECIALIZATIONS {
            break;
        }
    }
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
