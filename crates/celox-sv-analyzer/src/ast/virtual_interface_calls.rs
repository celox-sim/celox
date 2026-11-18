//! Tasks and functions of an interface called through a virtual interface
//! (IEEE 1800-2023 25.9).
//!
//! A call `vif.drive(req)` calls the subroutine `I$vif$drive(vif, req)`,
//! where `I` is the interface of the handle's type
//! ([`virtual_interfaces::subroutine_name`]). This module builds one such
//! subroutine for each task and function of each interface a virtual
//! interface type names: the subroutine of the interface, analyzed in the
//! interface's own scope, with every member of the interface renamed to the
//! placeholder of that member (`$vifany$I$m`), its body in one case item on
//! the handle labelled with the instance placeholder (`$vifinstance$I`).
//! Expanding a module's virtual interface placeholders repeats that item
//! for each instance of the interface, with the members of that instance
//! ([`crate::ir::Module::expand_instances`]), as it does
//! for any other access through a virtual interface.
//!
//! One subroutine serves every instance. Its formals belong to each call,
//! as the call binds them before it knows the instance; its static
//! variables belong to each instance (13.3.2): they are placeholders
//! (`$vifstatic$I$v`) that the expansion repeats for every instance, held
//! by the scope that makes the call, as the static storage of a subroutine
//! called through an interface port is held by the calling module.

use super::*;
use virtual_interfaces::{DefinitionId, VirtualInterface, VirtualInterfaceSignal};

/// The subroutines that call the tasks and functions of interfaces through
/// virtual interfaces, which every scope that may make such a call imports.
#[derive(Debug, Clone, Default)]
pub(crate) struct Subroutines {
    pub(super) subroutines: Vec<Arc<Subroutine>>,
    /// The shapes of the formals of each, the handle's first.
    pub(super) shapes: HashMap<String, Arc<Vec<VariableDimensions>>>,
    /// Their formals and variables, which a scope declares with them.
    pub(super) locals: Vec<Arc<LocalVariable>>,
    /// The result types of the functions among them, published as those of
    /// any function (see [`scope::ScopeSymbols::function_return_types`]).
    pub(super) function_return_types: HashMap<String, FunctionReturnMetadata>,
    /// Their static variables, as placeholders for each instance.
    pub(super) statics: Vec<Signal>,
    /// The initializer of those static variables.
    pub(super) static_initializer: Option<InitialProcess>,
}

/// The subroutines of the specializations of parameterized interfaces, by
/// the key of each specialization (see [`VirtualInterface::key`]).
pub(crate) type Specializations = Arc<HashMap<String, Arc<Subroutines>>>;

impl Subroutines {
    fn extend(&mut self, found: Self) {
        self.subroutines.extend(found.subroutines);
        self.shapes.extend(found.shapes);
        self.locals.extend(found.locals);
        self.function_return_types
            .extend(found.function_return_types);
        self.statics.extend(found.statics);
        if let Some(process) = found.static_initializer {
            match &mut self.static_initializer {
                Some(initializer) => initializer.body.extend(process.body),
                None => self.static_initializer = Some(process),
            }
        }
    }
}

/// Declare the subroutines `calls` in `symbols`, the symbols of a scope
/// that may call them, which copies those it calls.
pub(super) fn import(symbols: &mut scope::ScopeSymbols, calls: &Subroutines) {
    for subroutine in &calls.subroutines {
        if symbols
            .subroutines
            .iter()
            .any(|known| known.name == subroutine.name)
        {
            continue;
        }
        symbols.subroutine_params.insert(
            subroutine.name.clone(),
            subroutine
                .params
                .iter()
                .map(|param| param.name.clone())
                .collect(),
        );
        if let Some(shapes) = calls.shapes.get(&subroutine.name) {
            symbols
                .subroutine_shapes
                .insert(subroutine.name.clone(), shapes.clone());
        }
        symbols.subroutines.push(subroutine.clone());
    }
    for (name, metadata) in &calls.function_return_types {
        symbols
            .function_return_types
            .entry(name.clone())
            .or_insert_with(|| metadata.clone());
    }
    for signal in &calls.statics {
        if !symbols
            .state_signals
            .iter()
            .any(|known| known.name == signal.name)
        {
            symbols.state_signals.push(signal.clone());
        }
    }
    if let Some(initializer) = &calls.static_initializer {
        symbols.initial_processes.push(initializer.clone());
    }
    for local in &calls.locals {
        if !symbols.locals.iter().any(|known| known.name == local.name) {
            symbols.locals.push(local.clone());
        }
    }
}

/// Declare the subroutines of the specializations that earlier analyses
/// built (see [`specializations`]) in `symbols`, if the scope reaches the
/// instances of virtual interfaces and does not declare them yet.
pub(super) fn import_specializations(session: &Session, symbols: &mut Arc<scope::ScopeSymbols>) {
    if session.virtual_interface_calls.is_empty()
        || !symbols
            .state_signals
            .iter()
            .any(|signal| virtual_interfaces::is_placeholder_signal(&signal.name))
    {
        return;
    }
    let mut keys: Vec<_> = session.virtual_interface_calls.keys().collect();
    keys.sort();
    for key in keys {
        let calls = &session.virtual_interface_calls[key];
        let declared = |symbols: &scope::ScopeSymbols| {
            calls.subroutines.first().is_none_or(|first| {
                symbols
                    .subroutines
                    .iter()
                    .any(|known| known.name == first.name)
            })
        };
        if !declared(symbols) {
            import(Arc::make_mut(symbols), calls);
        }
    }
}

/// Whether `module` calls a task or function of an interface through a
/// virtual interface whose hierarchical names each instance resolves, before
/// the subroutine is built (see
/// [`virtual_interfaces::VirtualInterfaceDeclaration::outward`]): a call
/// bound by an analysis that did not know it waits, such as one of a class
/// method that its package bound. The module is analyzed again once the
/// hierarchy is elaborated ([`AnalyzerError::VirtualInterfaceOutward`]).
pub(super) fn check_outward(session: &Session, module: &Module) -> Converted<()> {
    let Some(declarations) = session.virtual_interfaces.as_deref() else {
        return Ok(());
    };
    let pending = declarations
        .interfaces
        .iter()
        .filter(|(_, declaration)| !declaration.outward.is_empty())
        .map(|(key, _)| (key, virtual_interfaces::subroutine_name(key, "")))
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(());
    }
    let defined = module
        .subroutines
        .iter()
        .map(|subroutine| subroutine.name.as_str())
        .collect::<HashSet<_>>();
    let bodies = module
        .subroutines
        .iter()
        .map(|subroutine| &subroutine.body)
        .chain(module.initial_processes.iter().map(|process| &process.body))
        .chain(module.ff_processes.iter().map(|process| &process.body))
        .chain(module.comb_processes.iter().map(|process| &process.body));
    let mut called = HashSet::default();
    for body in bodies {
        for stmt in body {
            let mut exprs = Vec::new();
            let mut tasks = Vec::new();
            stmt.visit(&mut |expr| exprs.push(expr), &mut |_| {}, &mut |name| {
                tasks.push(name.to_owned())
            });
            for expr in exprs {
                called_in(expr, &mut called);
            }
            called.extend(tasks);
        }
    }
    for (key, prefix) in pending {
        if called
            .iter()
            .any(|name| name.starts_with(&prefix) && !defined.contains(name.as_str()))
        {
            return Err(AnalyzerError::VirtualInterfaceOutward {
                interface: key.clone(),
            });
        }
    }
    Ok(())
}

/// Add the subroutines that `expr` calls to `called`.
fn called_in(expr: &Expr, called: &mut HashSet<String>) {
    if let Expr::Call {
        callee: Callee::Subroutine(name),
        ..
    } = expr
    {
        called.insert(name.clone());
    }
    for child in virtual_interfaces::children(expr) {
        called_in(child, called);
    }
}

/// The subroutines of the interfaces in `declarations` that have no
/// parameters, built with the analyzed `packages` (see the module
/// documentation). Those of a parameterized interface belong to each of its
/// specializations (see [`specializations`]). The declarations record the
/// names that the subroutines of each interface use and it does not
/// declare, whose subroutines are not built then.
pub(super) fn build(
    packages: &packages::Packages,
    declarations: &mut virtual_interfaces::VirtualInterfaces,
) -> Converted<Subroutines> {
    let mut built = Subroutines::default();
    let mut pending = Vec::new();
    let mut interfaces: Vec<_> = declarations
        .interfaces
        .iter()
        .filter(|(_, declaration)| {
            !declaration.subroutines.is_empty()
                && declaration.template.is_none()
                && declaration.arguments.is_empty()
        })
        .collect();
    interfaces.sort_by(|left, right| left.0.cmp(right.0));
    for (key, declaration) in interfaces {
        let (subroutines, outward) = specialization(
            packages,
            &mut packages.design(),
            key,
            declaration,
            &HashMap::default(),
            &[],
        )?;
        built.extend(subroutines);
        if !outward.is_empty() {
            pending.push((key.clone(), outward));
        }
    }
    for (key, outward) in pending {
        if let Some(declaration) = declarations.interfaces.get_mut(&key) {
            declaration.outward = outward;
        }
    }
    Ok(built)
}

/// Build the subroutines of each specialization of a parameterized
/// interface that the analyses of `design` named and that has tasks or
/// functions, if they are not built yet. Whether any were: an analysis that
/// named one is repeated, to call them (see
/// [`crate::ast::source`]).
pub(super) fn specializations(
    packages: &packages::Packages,
    design: &mut packages::Design,
) -> Converted<bool> {
    let Some(declarations) = design.virtual_interfaces.clone() else {
        return Ok(false);
    };
    let mut missing: Vec<_> = declarations
        .interfaces
        .iter()
        .filter(|(key, declaration)| {
            !declaration.arguments.is_empty()
                && !declaration.subroutines.is_empty()
                && !design.virtual_interface_calls.contains_key(*key)
        })
        .collect();
    if missing.is_empty() {
        return Ok(false);
    }
    missing.sort_by(|left, right| left.0.cmp(right.0));
    let mut repeat = false;
    for (key, declaration) in missing {
        let overrides = declarations
            .interfaces
            .get(&declaration.interface)
            .and_then(|base| base.template.as_ref())
            .map(|template| template.overrides(&declaration.arguments))
            .unwrap_or_default();
        let overrides = overrides
            .into_iter()
            .map(|(name, value)| (name, parameters::const_expr_from_i128(value)))
            .collect();
        // Built in the types of the design, which the analyses that call
        // them share.
        let (built, outward) = specialization(packages, design, key, declaration, &overrides, &[])?;
        repeat |= !built.subroutines.is_empty();
        Arc::make_mut(&mut design.virtual_interface_calls).insert(key.clone(), Arc::new(built));
        if !outward.is_empty()
            && let Some(declarations) = design.virtual_interfaces.as_mut()
            && let Some(declaration) = Arc::make_mut(declarations).interfaces.get_mut(key)
        {
            declaration.outward = outward;
        }
    }
    Ok(repeat)
}

/// Build the subroutines of the interface, or specialization, `key`, whose
/// hierarchical names that it does not declare (see
/// [`virtual_interfaces::VirtualInterfaceDeclaration::outward`]) denote
/// objects of `objects`, by their components, with their types and whether
/// each is a net: in every instance, the object its parent finds (IEEE
/// 1800-2023 23.8). Each becomes a member of the declaration, which a call
/// through a virtual interface reads and writes in the instance it denotes.
pub(super) fn resolve_outward(
    packages: &packages::Packages,
    design: &mut packages::Design,
    key: &str,
    objects: &[(Vec<String>, Type, bool)],
) -> Converted<()> {
    let Some(declarations) = design.virtual_interfaces.as_mut() else {
        return Ok(());
    };
    let declarations = Arc::make_mut(declarations);
    let overrides = declarations
        .interfaces
        .get(key)
        .and_then(|declaration| {
            let base = declarations.interfaces.get(&declaration.interface)?;
            Some(base.template.as_ref()?.overrides(&declaration.arguments))
        })
        .unwrap_or_default()
        .into_iter()
        .map(|(name, value)| (name, parameters::const_expr_from_i128(value)))
        .collect();
    let Some(declaration) = declarations.interfaces.get_mut(key) else {
        return Ok(());
    };
    // An interface without instances has no objects for the names, which a
    // call through a virtual interface, of null, never reaches: they keep
    // provisional types.
    let provisional_objects;
    let objects = if objects.is_empty() {
        provisional_objects = declaration
            .outward
            .iter()
            .map(|components| {
                let object = provisional(components);
                (components.clone(), object.r#type, object.is_net)
            })
            .collect::<Vec<_>>();
        &provisional_objects[..]
    } else {
        objects
    };
    for (components, r#type, is_net) in objects {
        let name = virtual_interfaces::outward_member(components);
        if !declaration.members.iter().any(|member| member.name == name) {
            declaration
                .members
                .push(virtual_interfaces::VirtualInterfaceMember {
                    name,
                    r#type: virtual_interfaces::MemberType::Value(r#type.clone()),
                    is_net: *is_net,
                });
        }
    }
    declaration.outward.clear();
    let declaration = declaration.clone();
    let seeds = objects
        .iter()
        .map(|(components, r#type, is_net)| HierarchicalSignal {
            paths: vec![components.join(".")],
            name: virtual_interfaces::outward_member(components),
            r#type: r#type.clone(),
            is_net: *is_net,
            local: false,
            interface_instance: None,
            aliased: None,
        })
        .collect::<Vec<_>>();
    let (built, outward) = specialization(packages, design, key, &declaration, &overrides, &seeds)?;
    if let Some(components) = outward.first() {
        return Err(unsupported(format!(
            "hierarchical name `{}` in a task or function of interface `{}` called through a virtual interface",
            components.join("."),
            declaration.interface
        )));
    }
    Arc::make_mut(&mut design.virtual_interface_calls).insert(key.to_owned(), Arc::new(built));
    Ok(())
}

/// The subroutines of the specialization `key` of the interface `declaration`
/// declares, whose parameters `overrides` give, and the hierarchical names
/// they use that the interface does not declare, by their components,
/// beyond those `resolved` gives the objects of. While there are any, no
/// subroutine is built: each instance finds them from its parent (IEEE
/// 1800-2023 23.8), which the elaborated hierarchy tells (see
/// [`crate::AnalyzerError::VirtualInterfaceOutward`]).
fn specialization(
    packages: &packages::Packages,
    design: &mut packages::Design,
    key: &str,
    declaration: &virtual_interfaces::VirtualInterfaceDeclaration,
    overrides: &HashMap<String, ConstExpr>,
    resolved: &[HierarchicalSignal],
) -> Converted<(Subroutines, Vec<Vec<String>>)> {
    let mut built = Subroutines::default();
    let mut outward = Vec::new();
    let interface = &declaration.interface;
    let Some((tree, path)) = packages.interface_sources.get(interface) else {
        return Ok((built, outward));
    };
    for handle in tree.root_scopes()? {
        let scoped = tree.scoped(handle)?;
        if instances::normalize_identifier(&scoped.root_name()?) != *interface {
            continue;
        }
        // The names the interface does not declare are found one analysis
        // at a time, each standing for an object of a provisional type for
        // the next, which the subroutines built later do not keep.
        let mut seeds = resolved.to_vec();
        loop {
            // An interface whose subroutines cannot be analyzed has none
            // to call through a virtual interface; a design that calls none
            // keeps working, and a call names the missing subroutine.
            let mut found = Subroutines::default();
            let mut session = Session::with_packages(packages, design);
            let subroutines = interface_subroutines(
                packages,
                &mut session,
                &scoped,
                path,
                key,
                declaration,
                overrides,
                &seeds,
                &mut found,
            );
            session.restore(design);
            match subroutines {
                Ok(()) => {
                    if outward.is_empty() {
                        built.extend(found);
                    }
                    break;
                }
                Err(error) => match outward_name(&error) {
                    Some(components)
                        if !outward.contains(components) && outward.len() < MAX_OUTWARD =>
                    {
                        seeds.push(provisional(components));
                        outward.push(components.clone());
                    }
                    _ => break,
                },
            }
        }
    }
    Ok((built, outward))
}

/// How many names an interface's subroutines may use that it does not
/// declare, a bound on the analyses that find them.
const MAX_OUTWARD: usize = 256;

/// The components of the hierarchical name that `error` reports the body
/// does not resolve.
fn outward_name(error: &AnalyzerError) -> Option<&Vec<String>> {
    match error {
        AnalyzerError::HierarchicalReference {
            components,
            interface: None,
            ..
        } => Some(components),
        AnalyzerError::WithWarnings { error, .. } => outward_name(error),
        _ => None,
    }
}

/// A provisional object for name `components` of the body, which lets the
/// analysis go on to the next name.
fn provisional(components: &[String]) -> HierarchicalSignal {
    HierarchicalSignal {
        paths: vec![components.join(".")],
        name: virtual_interfaces::outward_member(components),
        r#type: Type::new(TypeKind::Logic),
        is_net: false,
        local: false,
        interface_instance: None,
        aliased: None,
    }
}

/// Add the subroutines of the specialization `interface` (its key) of the
/// interface whose declaration `scoped` is, with the parameter values
/// `overrides` give, to `built`.
#[allow(clippy::too_many_arguments)]
fn interface_subroutines(
    packages: &packages::Packages,
    session: &mut Session,
    scoped: &SyntaxTree,
    path: &std::path::Path,
    interface: &str,
    declaration: &virtual_interfaces::VirtualInterfaceDeclaration,
    overrides: &HashMap<String, ConstExpr>,
    outward: &[HierarchicalSignal],
    built: &mut Subroutines,
) -> Converted<()> {
    let context = AnalysisContext::unspecialized(scoped);
    session.shares_static_formals = true;
    let module = interface_ast::analyze(
        &context.child(scoped, overrides),
        session,
        path,
        packages,
        overrides,
        &[],
        &ModuleInterfaces::default(),
        false,
        outward,
    )?;
    let symbols = module
        .symbols
        .as_ref()
        .ok_or_else(|| unsupported("interface without symbols"))?;
    let prefix = format!("{interface}$vif");
    let mut scratch = scope::ScopeSymbols::default();
    let mut names =
        interface_binding::binding_names_with_prefix(&module, symbols, "", &prefix, &mut scratch);
    for member in &declaration.members {
        names.insert(
            member.name.clone(),
            VirtualInterfaceSignal::Placeholder {
                definition: DefinitionId::of(interface),
                member: member.name.clone(),
            }
            .name(),
        );
    }
    // Every task and function the interface declares is renamed to the
    // subroutine that calls it through a virtual interface, whatever else
    // the binding names cover.
    for name in declaration.subroutines.keys() {
        names.insert(
            name.clone(),
            virtual_interfaces::subroutine_name(interface, name),
        );
    }
    // The static variables of the subroutines belong to each instance (13.3.2),
    // the formals of a static task kept with them (see
    // `Session::shares_static_formals`).
    for subroutine in &symbols.subroutines {
        for name in &subroutine.persistent_locals {
            names.insert(
                name.clone(),
                VirtualInterfaceSignal::StaticPlaceholder {
                    definition: DefinitionId::of(interface),
                    name: name.clone(),
                }
                .name(),
            );
        }
    }
    let bound = symbols.renamed(&names);
    let is_static = |name: &str| {
        matches!(
            VirtualInterfaceSignal::of(name),
            Some(VirtualInterfaceSignal::StaticPlaceholder { .. })
        )
    };
    // Their types may be sized by the interface's parameters, which the
    // scopes that hold them do not declare: they take the values of the
    // specialization.
    let constants: ConstEnv = symbols
        .const_env
        .iter()
        .filter_map(|(name, value)| Some((names.get(name)?.clone(), *value)))
        .collect();
    let mut statics: Vec<Signal> = bound
        .signals
        .iter()
        .filter(|signal| is_static(&signal.name))
        .cloned()
        .collect();
    declarations::substitute_signal_local_constants(&mut statics, &constants);
    built.statics.extend(statics);
    for process in bound
        .initial_processes
        .iter()
        .filter(|p| p.is_initializer())
    {
        let body: Vec<_> = process
            .body
            .iter()
            .filter(|stmt| matches!(stmt, Stmt::Assign { lhs, .. } if is_static(lhs.name())))
            .cloned()
            .collect();
        if body.is_empty() {
            continue;
        }
        match &mut built.static_initializer {
            Some(initializer) => initializer.body.extend(body),
            None => {
                let mut initializer = process.clone();
                initializer.body = body;
                initializer.validation_body = Vec::new();
                built.static_initializer = Some(initializer);
            }
        }
    }
    let own: HashSet<String> = declaration
        .subroutines
        .keys()
        .map(|name| virtual_interfaces::subroutine_name(interface, name))
        .collect();
    let ty = virtual_interfaces::value_type(
        &mut session.types,
        VirtualInterface::new(
            declaration.interface.clone(),
            None,
            declaration.arguments.clone(),
        ),
    );
    let handle_shape = procedural::dimensions_from_type(&ty);
    let ty = type_nodes::ir_type(session, &context.functions, ty, &ConstEnv::default());
    for subroutine in &bound.subroutines {
        if !own.contains(&subroutine.name) {
            continue;
        }
        let mut shapes = vec![handle_shape.clone()];
        shapes.extend(
            bound
                .subroutine_shapes
                .get(&subroutine.name)
                .map(|shapes| shapes.as_ref().clone())
                .unwrap_or_default(),
        );
        built
            .shapes
            .insert(subroutine.name.clone(), Arc::new(shapes));
        if let Some(metadata) = bound.function_return_types.get(&subroutine.name) {
            built
                .function_return_types
                .insert(subroutine.name.clone(), metadata.clone());
        }
        // Each subroutine names its handle apart from the others', which
        // the handle of a call of another one reads.
        let handle = format!("{}$$handle", subroutine.name);
        let (dispatching, temporaries) = dispatching(subroutine, interface, &handle, &ty, &own);
        built.subroutines.push(Arc::new(dispatching));
        built.locals.extend(temporaries.into_iter().map(Arc::new));
        built.locals.push(Arc::new(LocalVariable {
            name: handle.clone(),
            source_name: handle,
            r#type: ty.clone(),
            owned: true,
        }));
    }
    // The variables of the interface's subroutines, renamed with them.
    for local in &bound.locals {
        if local.name.starts_with(&prefix)
            && !built.locals.iter().any(|known| known.name == local.name)
        {
            built.locals.push(local.clone());
        }
    }
    Ok(())
}

/// Subroutine `subroutine` of interface `interface`, with members renamed
/// to their placeholders, taking the handle as formal `handle` and running
/// its body for the instance the handle denotes.
fn dispatching(
    subroutine: &Subroutine,
    interface: &str,
    handle: &str,
    ty: &crate::ir::Type,
    own: &HashSet<String>,
) -> (Subroutine, Vec<LocalVariable>) {
    let mut subroutine = subroutine.clone();
    let value = Expr::Ident(handle.to_string());
    // A static task's formals are static variables of each instance
    // (13.3.2), which every call shares. The call binds its arguments
    // before it knows the instance, to formals of its own, which the
    // instance's formals take on entry and give back on every exit.
    let is_static = |name: &str| {
        matches!(
            VirtualInterfaceSignal::of(name),
            Some(VirtualInterfaceSignal::StaticPlaceholder { .. })
        )
    };
    let mut temporaries = Vec::new();
    let mut entry = Vec::new();
    let mut exit = Vec::new();
    for (index, param) in subroutine.params.iter_mut().enumerate() {
        if !is_static(&param.name) {
            continue;
        }
        let temporary = format!("{handle}$formal{index}");
        let shared = std::mem::replace(&mut param.name, temporary.clone());
        use crate::procedural::ParamDirection;
        if matches!(
            param.direction,
            ParamDirection::Input | ParamDirection::Inout
        ) {
            entry.push(Stmt::Assign {
                lhs: LValue::Ident(shared.clone()),
                rhs: Expr::Ident(temporary.clone()),
                nonblocking: false,
            });
        }
        if matches!(
            param.direction,
            ParamDirection::Output | ParamDirection::Inout
        ) {
            exit.push(Stmt::Assign {
                lhs: LValue::Ident(temporary.clone()),
                rhs: Expr::Ident(shared),
                nonblocking: false,
            });
        }
        temporaries.push(LocalVariable {
            name: temporary.clone(),
            source_name: temporary,
            r#type: param.r#type.clone(),
            owned: true,
        });
    }
    if !entry.is_empty() || !exit.is_empty() {
        let result = subroutine.return_var.clone();
        let mut body = std::mem::take(&mut subroutine.body);
        procedural::subroutines::before_returns(&mut body, &exit, result.as_deref());
        subroutine.body = entry;
        subroutine.body.extend(body);
        subroutine.body.extend(exit);
    }
    // A call of another subroutine of the interface runs it for the same
    // instance, also one in a default.
    for stmt in &mut subroutine.body {
        pass_handle(stmt, &value, own);
    }
    for param in &mut subroutine.params {
        if let Some(default) = &mut param.default {
            pass_handle_expr(default, &value, own);
        }
    }
    let mut automatic_locals = vec![handle.to_string()];
    automatic_locals.extend(subroutine.params.iter().map(|param| param.name.clone()));
    automatic_locals.extend(subroutine.return_var.clone());
    for name in std::mem::take(&mut subroutine.automatic_locals)
        .into_iter()
        .chain(subroutine.body.iter().flat_map(declared_locals))
    {
        if !automatic_locals.contains(&name) && !subroutine.persistent_locals.contains(&name) {
            automatic_locals.push(name);
        }
    }
    subroutine.automatic = true;
    subroutine.automatic_locals = automatic_locals;
    let instance = VirtualInterfaceSignal::Instance {
        definition: DefinitionId::of(interface),
    }
    .name();
    // A default is evaluated where the subroutine is declared (13.5.3): one
    // that reads a member reads it of the instance the handle denotes.
    for param in &mut subroutine.params {
        if let Some(default) = param.default.take() {
            param.default = Some(if reads_member(&default) {
                Expr::Mux {
                    condition: Box::new(Expr::Binary {
                        left: Box::new(value.clone()),
                        op: BinaryOp::Eq,
                        right: Box::new(Expr::Ident(instance.clone())),
                    }),
                    then_expr: Box::new(default),
                    else_expr: Box::new(virtual_interfaces::null_access(&value, 1, false)),
                }
            } else {
                default
            });
        }
    }
    // The handle is the receiver of the call, which the defaults that read
    // it read as a method's defaults read its object.
    subroutine.params.insert(
        0,
        SubroutineParam {
            name: handle.to_string(),
            source_name: "this".to_string(),
            direction: crate::procedural::ParamDirection::Input,
            r#type: ty.clone(),
            default: None,
            leaves: Vec::new(),
        },
    );
    subroutine.body = vec![Stmt::Case {
        kind: crate::procedural::CaseKind::Exact,
        selector: value.clone(),
        items: vec![crate::procedural::CaseItemBase {
            labels: vec![crate::procedural::CaseLabel::Value(Expr::Ident(instance))],
            body: std::mem::take(&mut subroutine.body),
        }],
        default: Some(vec![Stmt::Eval(virtual_interfaces::null_access(
            &value, 1, false,
        ))]),
    }];
    (subroutine, temporaries)
}

/// The order of the actuals of a call, by formal position, once the handle
/// is the first: it comes first, and the others' positions move by one.
fn receiver_first(arg_order: &mut Vec<usize>) {
    if !arg_order.is_empty() {
        *arg_order = std::iter::once(0)
            .chain(arg_order.iter().map(|position| position + 1))
            .collect();
    }
}

/// Whether `expr` reads a member of an interface through its placeholder.
fn reads_member(expr: &Expr) -> bool {
    match expr {
        Expr::Ident(name) => matches!(
            VirtualInterfaceSignal::of(name),
            Some(VirtualInterfaceSignal::Placeholder { .. })
        ),
        expr => virtual_interfaces::children(expr)
            .into_iter()
            .any(reads_member),
    }
}

/// The names of the variables `stmt` declares.
fn declared_locals(stmt: &Stmt) -> Vec<String> {
    let mut names = Vec::new();
    stmt.walk(&mut |stmt| {
        if let Stmt::Local { name, .. } = stmt {
            names.push(name.clone());
        }
    });
    names
}

/// `stmt` with `handle` passed first to each call of a subroutine in `own`.
fn pass_handle(stmt: &mut Stmt, handle: &Expr, own: &HashSet<String>) {
    stmt.walk_mut(&mut |stmt| {
        if let Stmt::Call {
            name,
            args,
            arg_order,
        } = stmt
            && own.contains(name)
        {
            args.insert(0, Some(handle.clone()));
            receiver_first(arg_order);
        }
    });
    stmt.visit_mut(
        &mut |expr| pass_handle_expr(expr, handle, own),
        &mut |_| {},
        &mut |_| {},
    );
}

fn pass_handle_expr(expr: &mut Expr, handle: &Expr, own: &HashSet<String>) {
    let taken = std::mem::replace(expr, Expr::Literal(String::new()));
    *expr = map_call_args(taken, handle, own);
}

fn map_call_args(expr: Expr, handle: &Expr, own: &HashSet<String>) -> Expr {
    let expr =
        virtual_interfaces::map_children(expr, &mut |child| map_call_args(child, handle, own));
    match expr {
        Expr::Call {
            callee: Callee::Subroutine(name),
            mut args,
            arg_order,
        } if own.contains(&name) => {
            args.insert(0, Some(handle.clone()));
            let mut arg_order = arg_order;
            receiver_first(&mut arg_order);
            Expr::Call {
                callee: Callee::Subroutine(name),
                args,
                arg_order,
            }
        }
        expr => expr,
    }
}
