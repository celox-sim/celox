//! Package class templates instantiated by scopes that use the package.
use super::*;
use crate::ast::scope::ScopeSymbols;
use specializations::BoundArguments;

#[derive(Debug)]
struct Owner {
    tree: Rc<SyntaxTree>,
    imports: ScopeSymbols,
    timescales: Arc<timescales::Timescales>,
}

#[derive(Clone, Debug)]
struct Request {
    generic: String,
    arguments: BoundArguments,
    name: Option<String>,
}

/// Bound declarations inherited from a class, excluding its enclosing package.
#[derive(Clone, Debug, Default)]
pub(in crate::ast) struct DeclarationScope {
    pub env: HashMap<String, Option<i128>>,
    pub types: HashMap<String, Type>,
    pub literals: HashMap<String, Expr>,
    pub constant_types: HashMap<String, semantic_types::ConstantType>,
    pub nonconstant_members: HashSet<String>,
}

impl DeclarationScope {
    pub fn apply(&self, env: &mut ConstEnv, types: &mut TypeAliases) {
        for (name, value) in &self.env {
            match value {
                Some(value) => {
                    env.insert(name.clone(), *value);
                }
                None => {
                    env.remove(name);
                }
            }
        }
        types.extend(self.types.clone());
    }
}

#[derive(Clone, Debug, Default)]
pub(in crate::ast) struct Registry {
    owners: HashMap<String, Rc<Owner>>,
    templates: HashMap<String, String>,
    requests: HashMap<String, Vec<Request>>,
    prepared: HashMap<String, ScopeSymbols>,
    complete: HashSet<String>,
    argument_symbols: ScopeSymbols,
    active: HashSet<String>,
    states: HashMap<String, crate::ir::Module>,
    revision: u64,
    generics: HashMap<String, String>,
    specializations: HashMap<String, Rc<specializations::Specialization>>,
    declarations: HashMap<String, Rc<DeclarationScope>>,
    /// What the last analysis of each package lowered for its classes.
    lowered: HashMap<String, Rc<crate::ast::procedural::Lowered>>,
}

impl Registry {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Whether `name` is a package whose classes other scopes may ask
    /// specializations of.
    pub fn owns(&self, name: &str) -> bool {
        self.owners.contains_key(name)
    }
    /// What the last analysis of package `name` lowered for its classes.
    pub fn lowered(&self, name: &str) -> Option<Rc<crate::ast::procedural::Lowered>> {
        self.lowered.get(name).cloned()
    }
    pub fn record_lowered(&mut self, name: String, lowered: crate::ast::procedural::Lowered) {
        self.lowered.insert(name, Rc::new(lowered));
    }
    pub fn has_templates(&self) -> bool {
        !self.templates.is_empty()
    }
    pub fn generic(&self, name: &str) -> Option<String> {
        self.generics.get(name).cloned()
    }
    /// The specialization `name` that the scope of its class made.
    pub fn specialization(&self, name: &str) -> Option<Rc<specializations::Specialization>> {
        self.specializations.get(name).cloned()
    }
    pub fn parameter(&self, name: &str, parameter: &str) -> Option<Expr> {
        self.declarations
            .get(name)?
            .literals
            .get(parameter)
            .cloned()
    }
    pub fn declarations(&self, name: &str) -> Option<Rc<DeclarationScope>> {
        self.declarations.get(name).cloned()
    }
    pub fn member_type(&self, name: &str, member: &str) -> Option<Type> {
        self.prepared
            .values()
            .find_map(|symbols| symbols.class_member_types.get(name)?.get(member).cloned())
    }
    pub fn names(&self) -> Vec<String> {
        self.templates.keys().cloned().collect()
    }
    pub fn contains(&self, name: &str) -> bool {
        self.templates.contains_key(name)
    }
    pub fn register(
        &mut self,
        name: &str,
        tree: &SyntaxTree,
        imports: &ScopeSymbols,
        timescales: Arc<timescales::Timescales>,
    ) {
        if self.owners.contains_key(name) {
            return;
        }
        let scope = tree.root_unit().ok().map(|unit| unit.tokens);
        for declaration in &tree.declarations().classes {
            if declaration.parameter_ports.is_some()
                && enclosing_unit(tree.declarations(), declaration.tokens).map(|unit| unit.tokens)
                    == scope
            {
                self.templates.insert(
                    format!(
                        "{}::{}",
                        name,
                        instances::normalize_identifier(&declaration.name.spelling)
                    ),
                    name.to_owned(),
                );
            }
        }
        self.owners.insert(
            name.into(),
            Rc::new(Owner {
                tree: Rc::new(tree.clone()),
                imports: imports.clone(),
                timescales,
            }),
        );
    }
    pub fn extend(&self, tree: &AnalysisContext<'_>, symbols: &mut ScopeSymbols) {
        let local_classes = scope_declarations(tree)
            .map(|class| declared_name(tree, class))
            .collect::<HashSet<_>>();
        // A member of a class or of one of its specializations: `class::`
        // or `class#(`.
        let local_member = |name: &str| {
            name.match_indices("::")
                .chain(name.match_indices("#("))
                .any(|(end, _)| local_classes.contains(&name[..end]))
        };
        for (owner, extra) in self.prepared.iter() {
            if self.active.contains(owner) {
                continue;
            }
            // An owner may import a caller's static field while binding T's
            // methods. Returning those symbols to the caller must not redeclare
            // storage that the caller's own class declarations already supply.
            // Storage the symbols already hold, as a signal or as state, is
            // not declared again under the other kind: a scope analyzed inside
            // another, such as a module bound to an interface instance, starts
            // from symbols this registry already extended.
            let present = symbols
                .signals
                .iter()
                .chain(&symbols.state_signals)
                .map(|signal| signal.name())
                .collect::<HashSet<_>>();
            let local_storage = extra
                .signals
                .iter()
                .chain(&extra.state_signals)
                .filter(|signal| present.contains(signal.name()) || local_member(signal.name()))
                .map(|signal| signal.name().to_owned())
                .collect::<HashSet<_>>();
            let declarations_only = symbols.declarations_only;
            if local_storage.is_empty() {
                symbols.extend(extra);
            } else {
                symbols.extend(&extra.clone().without(&local_storage));
            }
            symbols.declarations_only = declarations_only;
        }
    }
    pub fn states(&self) -> &HashMap<String, crate::ir::Module> {
        &self.states
    }
}

/// The name of the specialization of package class template `generic` with
/// `arguments`, which the scope of the package makes.
pub(in crate::ast) fn request(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    generic: &str,
    arguments: &[celox_sv_syntax::Argument],
    context: &PackedDimensions,
) -> Converted<Option<String>> {
    let registry = &session.foreign;
    let Some(owner_name) = registry.templates.get(generic).cloned() else {
        return Ok(None);
    };
    let owner = registry.owners[&owner_name].clone();
    let native = owner.tree.declarations();
    // The source can declare classes of the same name in other scopes;
    // the template is the one the owner package declares, as `register`
    // found it.
    let scope = owner.tree.root_unit().ok().map(|unit| unit.tokens);
    let declaration = native
        .classes
        .iter()
        .find(|class| {
            enclosing_unit(native, class.tokens).map(|unit| unit.tokens) == scope
                && format!(
                    "{}::{}",
                    owner_name,
                    instances::normalize_identifier(&class.name.spelling)
                ) == generic
        })
        .ok_or_else(|| unsupported(format!("class template `{generic}`")))?;
    let Some(arguments) = specializations::bind_arguments(
        tree,
        session,
        generic,
        declaration,
        native,
        arguments,
        context,
    )?
    else {
        return Ok(None);
    };
    // Module-local class names do not yet carry instance identity. They
    // cannot safely key a design-wide package specialization (8.25).
    fn module_class<'a>(
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        ty: &'a Type,
    ) -> Option<&'a str> {
        ty.class()
            .filter(|class| {
                !class.contains("::")
                    || super::declaration(tree, session, class).is_some_and(|declaration| {
                        enclosing_unit(tree.declarations(), declaration.tokens)
                            .is_some_and(|unit| unit.kind != celox_sv_syntax::UnitKind::Package)
                    })
            })
            .or_else(|| {
                let container = ty.container.as_ref()?;
                module_class(tree, session, &container.element).or_else(|| {
                    container
                        .key
                        .as_ref()
                        .and_then(|key| module_class(tree, session, key))
                })
            })
    }
    if let Some(class) = arguments
        .types
        .values()
        .find_map(|ty| module_class(tree, session, ty))
    {
        return Err(unsupported(format!(
            "module-local class `{class}` as a package class type argument"
        )));
    }
    let index = {
        let registry = &mut session.foreign;
        let requests = registry.requests.entry(owner_name.clone()).or_default();
        match requests
            .iter()
            .position(|r| r.generic == generic && r.arguments == arguments)
        {
            Some(index) => index,
            None => {
                if requests.len() >= 1024 {
                    return Err(unsupported("more than 1024 foreign class specializations"));
                }
                requests.push(Request {
                    generic: generic.into(),
                    arguments,
                    name: None,
                });
                registry.complete.remove(&owner_name);
                requests.len() - 1
            }
        }
    };
    if session.foreign.requests[&owner_name][index].name.is_none() {
        build(session, &owner_name, None)?;
    }
    Ok(session.foreign.requests[&owner_name][index].name.clone())
}

fn build(session: &mut Session, name: &str, caller: Option<&ScopeSymbols>) -> Converted<()> {
    if session.foreign.active.len() >= 16 {
        return Err(unsupported("foreign class scopes nested more than 16 deep"));
    }
    if !session.foreign.active.insert(name.into()) {
        return Ok(());
    }
    let result = build_inner(session, name, caller);
    session.foreign.active.remove(name);
    result
}

/// Analyzes package `name` for the specializations requested of its class
/// templates, in a session of its own that takes over the registry, the
/// types and the call sites of `session` and gives them back.
fn build_inner(session: &mut Session, name: &str, caller: Option<&ScopeSymbols>) -> Converted<()> {
    let owner = session.foreign.owners[name].clone();
    let requests = &session.foreign.requests[name];
    let mut scope = Session {
        foreign_arguments: requests
            .iter()
            .map(|r| (r.generic.clone(), r.arguments.clone()))
            .collect(),
        foreign: std::mem::take(&mut session.foreign),
        types: std::mem::take(&mut session.types),
        virtual_interfaces: session.virtual_interfaces.clone(),
        next_call_site: session.next_call_site,
        ..Session::default()
    };
    let result = specialize(&mut scope, &owner, name, caller);
    session.foreign = scope.foreign;
    session.types = scope.types;
    session.next_call_site = scope.next_call_site;
    let mut symbols = result?;
    let registry = &mut session.foreign;
    for request in registry.requests.get_mut(name).unwrap() {
        if let Some((_, _, bound)) = scope.foreign_names.iter().find(|(generic, arguments, _)| {
            generic == &request.generic && arguments == &request.arguments
        }) {
            request.name = Some(bound.clone());
        }
    }
    for (class, specialization) in &scope.class_specializations {
        registry
            .generics
            .insert(class.clone(), specialization.generic.clone());
        registry
            .specializations
            .insert(class.clone(), specialization.clone());
    }
    registry.declarations.extend(symbols.declarations.drain());
    registry.prepared.insert(name.into(), symbols.symbols);
    if caller.is_some() {
        registry.complete.insert(name.into());
        if let Some(module) = symbols.state_module {
            registry.states.insert(name.into(), module);
        }
    }
    registry.revision += 1;
    Ok(())
}

/// What analyzing a package for its specializations made.
struct Specialized {
    symbols: ScopeSymbols,
    declarations: HashMap<String, Rc<DeclarationScope>>,
    state_module: Option<crate::ir::Module>,
}

fn specialize(
    session: &mut Session,
    owner: &Owner,
    name: &str,
    caller: Option<&ScopeSymbols>,
) -> Converted<Specialized> {
    let context = AnalysisContext {
        syntax_tree: &owner.tree,
        parameter_overrides: Cow::Owned(HashMap::default()),
        imported: Arc::default(),
        functions: const_functions::ConstantFunctions::default(),
        root_subroutines: false,
        active_interfaces: Arc::default(),
        // Inside the package, `p::x` names its own item `x`.
        package: Some(name.into()),
        timescale: timescales::for_tree(owner.timescales.clone(), &owner.tree)?,
        interface_instance: None,
    };

    let mut imports = owner.imports.clone();
    if let Some(caller) = caller {
        // Preserve the owner's existing bindings when caller metadata also
        // contains imported classes or routines from that package.
        imports.extend(&ScopeSymbols {
            classes: caller.classes.clone(),
            class_member_types: caller.class_member_types.clone(),
            // Only bound class members cross the template boundary. Module
            // constants must not become visible in the package's lexical scope.
            constant_types: caller
                .constant_types
                .iter()
                .filter(|(name, _)| {
                    name.rsplit_once("::")
                        .is_some_and(|(owner, _)| caller.class_member_types.contains_key(owner))
                })
                .map(|(name, ty)| (name.clone(), ty.clone()))
                .collect(),
            state_signals: caller
                .state_signals
                .iter()
                .filter(|signal| {
                    !signal.name().starts_with(&format!("{name}::"))
                        && signal
                            .package_variable
                            .as_ref()
                            .is_none_or(|(owner, _)| owner != name)
                })
                .cloned()
                .collect(),
            // Only qualified routines, the methods of classes the actual
            // type arguments name: a body of the package resolves an
            // unqualified call in its own scope, not the caller's (26.3).
            subroutine_params: qualified(&caller.subroutine_params),
            subroutine_shapes: qualified(&caller.subroutine_shapes),
            subroutines: caller
                .subroutines
                .iter()
                .filter(|subroutine| subroutine.name.contains("::"))
                .cloned()
                .collect(),
            // With the variables they declare (see `handed_over`).
            locals: {
                let declared = caller
                    .subroutines
                    .iter()
                    .filter(|subroutine| subroutine.name.contains("::"))
                    .flat_map(declared_names)
                    .collect::<HashSet<_>>();
                caller
                    .locals
                    .iter()
                    .filter(|local| declared.contains(local.name.as_str()))
                    .cloned()
                    .collect()
            },
            function_return_types: qualified(&caller.function_return_types),
            ..Default::default()
        });
    } else {
        imports.declarations_only = true;
    }
    // Like every scope, a specialization may reach the interface
    // instances through virtual interfaces (IEEE 1800-2023 25.9).
    if let Some(declarations) = crate::ast::virtual_interfaces::declarations(session) {
        for signal in crate::ast::virtual_interfaces::member_signals(declarations) {
            if !imports
                .state_signals
                .iter()
                .any(|known| known.name == signal.name)
            {
                imports.state_signals.push(signal);
            }
        }
    }
    // The classes whose specializations may extend a class of the scopes
    // that ask for them, when nothing else hides a name the package
    // qualifies with its own.
    let open = packages::self_qualified_names_are_plain(name, &context, |package| {
        owner.imports.package_scopes.contains(package)
    })?;
    let mut package = packages::analyze_package_with_symbols(
        name,
        &context,
        session,
        imports.clone(),
        HashMap::default(),
        None,
    )?;
    if !open.is_some_and(|open| inherits_within(name, &package.symbols.classes, &open)) {
        let context = AnalysisContext {
            package: None,
            ..context.clone()
        };
        // Class statics already have qualified names. Unlike ordinary
        // package variables, their new declarations would collide with
        // the first pass's imported storage.
        let statics = package
            .symbols
            .classes
            .iter()
            .flat_map(|class| class.statics.iter())
            .filter(|property| property.owner.starts_with(&format!("{name}::")))
            .map(|property| property.variable.clone())
            .collect();
        let own_items = package.symbols.clone().without(&statics);
        package = packages::analyze_package_with_symbols(
            name,
            &context,
            session,
            imports,
            HashMap::default(),
            Some(&own_items),
        )?;
    }
    if let Some(error) = session.foreign_error.take() {
        return Err(error);
    }
    let mut symbols = package.symbols;
    symbols.aliases.clear();
    symbols.declarations_only = false;
    let declarations =
        specializations::export_declarations(&context, session, &symbols.class_member_types);
    Ok(Specialized {
        symbols,
        declarations,
        state_module: package.state_module.filter(|_| caller.is_some()),
    })
}

/// Whether each of `classes` that specializes one of the classes `open` of
/// package `name` inherits from classes of the package alone, whose members
/// its syntax tells: through the class it extends, and for an interface
/// class through those it extends (IEEE 1800-2023 8.26.3).
fn inherits_within(name: &str, classes: &[Class], open: &HashSet<String>) -> bool {
    if open.is_empty() {
        return true;
    }
    let prefix = format!("{name}::");
    let by_name = classes
        .iter()
        .map(|class| (class.name.as_str(), class))
        .collect::<HashMap<_, _>>();
    // The name of a specialization is that of its class with its
    // arguments after it; the name itself may hold any text.
    let specializes = |class: &Class| {
        class.name.strip_prefix(&prefix).is_some_and(|generic| {
            open.iter().any(|open| {
                generic
                    .strip_prefix(open.as_str())
                    .is_some_and(|arguments| arguments.is_empty() || arguments.starts_with("#("))
            })
        })
    };
    let mut seen = HashSet::default();
    let mut pending = classes
        .iter()
        .filter(|class| specializes(class))
        .collect::<Vec<_>>();
    while let Some(class) = pending.pop() {
        if !seen.insert(class.name.as_str()) {
            continue;
        }
        let parents = class.base.iter().chain(
            class
                .is_interface
                .then_some(&class.interfaces)
                .into_iter()
                .flatten(),
        );
        for parent in parents {
            match by_name.get(parent.as_str()) {
                Some(parent) if parent.name.starts_with(&prefix) => pending.push(parent),
                _ => return false,
            }
        }
    }
    true
}

/// Records what scope `tree` declared that specializations of package class
/// templates may name, and makes the specializations still to be made.
pub(in crate::ast) fn finish(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    caller: &ScopeSymbols,
) -> Converted<()> {
    let declarations =
        specializations::export_declarations(tree, session, &caller.class_member_types);
    session.foreign.declarations.extend(declarations);
    let package_owner = match tree.root_unit() {
        Ok(unit) if unit.kind == celox_sv_syntax::UnitKind::Package => {
            Some(instances::normalize_identifier(&unit.name.spelling))
        }
        Err(_) => Some(packages::UNIT.to_owned()),
        _ => None,
    };
    {
        let known = &mut session.foreign.argument_symbols;
        known
            .class_member_types
            .extend(caller.class_member_types.clone());
        // The static properties of its classes, and the static variables
        // of their methods, are variables of the scope, which a package
        // lowering those methods for a specialization shares (IEEE
        // 1800-2023 6.21, 8.9).
        let method_statics = caller
            .subroutines
            .iter()
            .filter(|subroutine| {
                caller.classes.iter().any(|class| {
                    subroutine
                        .name
                        .strip_prefix(class.name.as_str())
                        .is_some_and(|method| method.starts_with("::"))
                })
            })
            .flat_map(|subroutine| subroutine.persistent_locals.iter().map(String::as_str));
        let variables = caller
            .classes
            .iter()
            .flat_map(|class| {
                class
                    .statics
                    .iter()
                    .map(|property| property.variable.as_str())
            })
            .chain(method_statics)
            .collect::<HashSet<_>>();
        for signal in caller
            .signals
            .iter()
            .chain(&caller.state_signals)
            .filter(|signal| variables.contains(signal.name()))
        {
            if !known
                .state_signals
                .iter()
                .any(|old| old.name == signal.name)
            {
                let mut signal = signal.clone();
                if signal.package_variable.is_none()
                    && let Some(owner) = &package_owner
                {
                    signal.package_variable = Some((owner.clone(), signal.name().into()));
                }
                known.state_signals.push(signal);
            }
        }

        for class in &caller.classes {
            if !known.classes.iter().any(|old| old.name == class.name) {
                known.classes.push(class.clone());
            }
        }
        known
            .subroutine_params
            .extend(caller.subroutine_params.clone());
        known
            .subroutine_shapes
            .extend(caller.subroutine_shapes.clone());
        for subroutine in &caller.subroutines {
            if !known
                .subroutines
                .iter()
                .any(|old| old.name == subroutine.name)
            {
                let (subroutine, locals) =
                    handed_over(subroutine, &caller.locals, package_owner.as_deref());
                known.subroutines.push(subroutine);
                for local in locals {
                    if !known.locals.iter().any(|old| old.name == local.name) {
                        known.locals.push(local);
                    }
                }
            }
        }
        known
            .function_return_types
            .extend(caller.function_return_types.clone());
    }
    let registry = &session.foreign;
    let caller = registry.argument_symbols.clone();
    let names = registry
        .requests
        .keys()
        .filter(|name| !registry.complete.contains(*name) && !registry.active.contains(*name))
        .cloned()
        .collect::<Vec<_>>();
    for name in names {
        build(session, &name, Some(&caller))?;
    }
    Ok(())
}

/// The variables `subroutine` declares: its formals, result and locals.
fn declared_names(subroutine: &Subroutine) -> HashSet<&str> {
    let mut declared: HashSet<&str> = subroutine
        .params
        .iter()
        .flat_map(|param| std::iter::once(&param.name).chain(&param.leaves))
        .chain(&subroutine.return_var)
        .chain(&subroutine.automatic_locals)
        .chain(&subroutine.persistent_locals)
        .map(String::as_str)
        .collect();
    for stmt in &subroutine.body {
        stmt.walk(&mut |stmt| {
            if let crate::procedural::StmtBase::Local { name, .. } = stmt {
                declared.insert(name.as_str());
            }
        });
    }
    declared
}

/// A subroutine of a scope as another scope lowers it, such as a method
/// of a class a package specialization takes, with the variables it
/// declares. A variable named by its subroutine and declaration (see
/// `BodyBuilder::declare_owned`) keeps its name. One the counter of the
/// scope's analysis named would read as a variable of the other scope
/// that its own count gave the same name: it is named after the scope and
/// the subroutine instead.
fn handed_over(
    subroutine: &Subroutine,
    locals: &[LocalVariable],
    scope: Option<&str>,
) -> (Subroutine, Vec<LocalVariable>) {
    let declared = declared_names(subroutine);
    let mut names = HashMap::default();
    for name in &declared {
        let Some((source, number)) = name.rsplit_once('@') else {
            continue;
        };
        if source.contains('%') {
            continue;
        }
        let key = fxhash::hash64(&(scope, subroutine.name.as_str(), *name));
        names.insert(name.to_string(), format!("{source}%{key:016x}@{number}"));
    }
    let rename = scope::Renamer { names: &names };
    let locals = locals
        .iter()
        .filter(|local| declared.contains(local.name.as_str()))
        .map(|local| LocalVariable {
            name: rename.name(&local.name),
            ..local.clone()
        })
        .collect();
    (rename.subroutine(subroutine.clone()), locals)
}

pub(in crate::ast) fn is_foreign(
    tree: &AnalysisContext<'_>,
    session: &Session,
    generic: &str,
) -> bool {
    let Some(owner) = session.foreign.templates.get(generic) else {
        return false;
    };
    tree.root_unit()
        .ok()
        .map(|unit| instances::normalize_identifier(&unit.name.spelling))
        .as_deref()
        != Some(owner.as_str())
}

/// The entries of `table` with qualified names, `scope::name`.
fn qualified<V: Clone>(table: &HashMap<String, V>) -> HashMap<String, V> {
    table
        .iter()
        .filter(|(name, _)| name.contains("::"))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procedural::{ParamDirection, StmtBase, SubroutineParamBase};

    #[test]
    fn handed_over_variables_never_take_a_counted_name_of_the_receiver() {
        // `$unit`'s count named the method's local `v@8`; the package that
        // takes the method may have named a local of its own `v@8` too. The
        // method's must not become that one.
        let logic = crate::ir::Type::from_ast(Type::new(TypeKind::Logic), &HashMap::default());
        let local = |name: &str, source: &str| LocalVariable {
            name: name.into(),
            source_name: source.into(),
            r#type: logic.clone(),
        };
        let this = "this%0123456789abcdef@5";
        let subroutine = Subroutine {
            name: "$unit::Item::code".into(),
            is_task: false,
            automatic: true,
            automatic_locals: Vec::new(),
            persistent_locals: Vec::new(),
            return_type: None,
            params: vec![SubroutineParamBase {
                name: this.into(),
                source_name: "this".into(),
                direction: ParamDirection::Input,
                r#type: logic.clone(),
                default: None,
                leaves: Vec::new(),
            }],
            return_var: None,
            body: vec![
                StmtBase::Local {
                    name: "v@8".into(),
                    init: None,
                },
                StmtBase::Assign {
                    lhs: LValue::Ident("v@8".into()),
                    rhs: Expr::Ident("w@9".into()),
                    nonblocking: false,
                },
            ],
            sensitivity_body: Vec::new(),
        };
        let locals = [local("v@8", "v"), local(this, "this"), local("w@9", "w")];
        let (handed, locals) = handed_over(&subroutine, &locals, Some("$unit"));
        let renamed = match &handed.body[0] {
            StmtBase::Local { name, .. } => name.clone(),
            other => panic!("{other:?}"),
        };
        assert_ne!(renamed, "v@8");
        assert!(
            renamed.starts_with("v%") && renamed.ends_with("@8"),
            "{renamed}"
        );
        assert_eq!(
            handed.body[1],
            StmtBase::Assign {
                lhs: LValue::Ident(renamed.clone()),
                // Not a variable of the method: the name it reads stays.
                rhs: Expr::Ident("w@9".into()),
                nonblocking: false,
            }
        );
        // The formal named by its method keeps its name.
        assert_eq!(handed.params[0].name, this);
        // The declarations go with it, `w@9`, which it does not declare,
        // does not.
        let mut names = locals
            .iter()
            .map(|local| local.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, [this.to_string(), renamed]);
    }
}
