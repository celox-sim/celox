//! Native interface scope analysis and public type contracts.
use super::*;

pub(super) type ActiveInstance = (
    std::path::PathBuf,
    String,
    HashMap<String, ConstExpr>,
    Vec<(String, Type)>,
);
/// Analyze the original interface body and retain its symbols for the scope
/// that instantiates it. The structural view borrows every token's original
/// source location, including the lexical time scope of processes/functions.
pub(super) fn analyze(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    path: &Path,
    packages: &packages::Packages,
    overrides: &HashMap<String, ConstExpr>,
    types: &[(String, Type)],
    interfaces: &ModuleInterfaces,
    declarations_only: bool,
    outward: &[HierarchicalSignal],
) -> Converted<Module> {
    let mut imported = imports::imported_symbols(tree, packages, packages.unit(path))?;
    imported.declarations_only = declarations_only;
    // The hierarchical names of the body that the scope instantiating it
    // resolved (IEEE 1800-2023 23.8) read and write the signals of that
    // scope they denote, which it declares.
    for signal in outward {
        for path in &signal.paths {
            imported
                .interface_members
                .insert(path.clone(), signal.name.clone());
        }
        imported.state_signals.push(Signal {
            is_const: false,
            name: signal.name.clone(),
            r#type: signal.r#type.clone(),
            is_net: signal.is_net,
            unresolved_net: false,
            net_resolution: NetResolution::Wire,
            net_drive: NetDrive::None,
            package_variable: None,
            hierarchical: signal.paths.first().cloned(),
            force_control: None,
        });
    }
    imported.type_parameter_bindings = type_parameters::bind(tree, types)?;
    let name = tree.root_name()?;
    let instance = (
        path.to_path_buf(),
        name.clone(),
        overrides.clone(),
        types.to_vec(),
    );
    if tree.active_interfaces.len() >= 64 {
        return Err(unsupported("interface instance nesting exceeds 64 levels"));
    }
    if tree.active_interfaces.contains(&instance) {
        return Err(unsupported(format!(
            "recursive interface instantiation `{name}`"
        )));
    }
    let mut active = (*tree.active_interfaces).clone();
    active.push(instance);
    let tree = &AnalysisContext {
        active_interfaces: Arc::new(active),
        ..tree.clone()
    };
    let timed = AnalysisContext {
        timescale: timescales::for_tree(packages.timescales.clone(), tree)?,
        ..tree.clone()
    };
    let tree = &timed;
    let lookup = InterfaceLookup {
        local: interfaces,
        extra: &ModuleInterfaces::default(),
    };
    let nested = interface_binding::analyze_module(
        tree,
        session,
        path,
        packages,
        interface_binding::Bindings {
            values: overrides,
            types,
            export_symbols: true,
            plan: None,
        },
        &lookup,
        imported.clone(),
    )?;
    let mut module = if let Some(module) = nested {
        module
    } else {
        Module::from_scope_with_symbols(
            tree,
            session,
            &name,
            overrides,
            &lookup,
            Arc::new(imported),
            true,
        )?
    };
    let symbols = module.symbols.as_mut().unwrap();
    let mut parameters = module.imported_parameters.clone();
    parameters.extend(
        symbols
            .parameters
            .iter()
            .filter(|parameter| {
                !module
                    .imported_parameters
                    .iter()
                    .any(|imported| imported.name == parameter.name)
            })
            .cloned(),
    );
    symbols.parameters = parameters;
    Ok(module)
}

/// What analyzing the signature of an interface again takes, for a
/// specialization of it: the interface's declarations and the symbols it
/// imports, without the packages they came from (see [`specialize`]).
pub(super) struct SignatureSource {
    tree: SyntaxTree,
    /// The types the signature was analyzed in, which the imported symbols
    /// are bound in: a specialization is analyzed in a copy of them, apart
    /// from the analysis that names it.
    types: semantic_types::TyTable,
    imported: scope::ScopeSymbols,
    timescale: Arc<timescales::QueryContext>,
    name: String,
    members: Vec<String>,
    names: HashSet<String>,
}

impl std::fmt::Debug for SignatureSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignatureSource")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl SignatureSource {
    fn new(
        tree: &SyntaxTree,
        path: &Path,
        packages: &packages::Packages,
        name: String,
        members: &[String],
        names: &HashSet<String>,
    ) -> Converted<Self> {
        let tree = interface_binding::declaration_view(tree)?;
        let context = AnalysisContext::unspecialized(&tree);
        let imported = imports::imported_symbols(&context, packages, packages.unit(path))?;
        let timescale = timescales::for_tree(packages.timescales.clone(), &context)?;
        Ok(Self {
            tree,
            types: semantic_types::TyTable::default(),
            imported,
            timescale,
            name,
            members: members.to_vec(),
            names: names.clone(),
        })
    }
}

/// The signature of the specialization of the interface `source` declares
/// whose parameter value assignment `overrides` gives, its parameters
/// evaluated as any instance's are (IEEE 1800-2023 23.10, 25.9): its
/// declarations alone, which the members are, without the bodies of its
/// processes and subroutines.
pub(super) fn specialize(
    source: &SignatureSource,
    overrides: &HashMap<String, ConstExpr>,
) -> Converted<(crate::ir::Module, Vec<(String, Type)>)> {
    let mut session = Session {
        types: source.types.clone(),
        ..Session::default()
    };
    signature_from(&mut session, source, overrides, true)
}

/// The signature of the interface `tree` declares, analyzed with the types,
/// specializations and call sites of `design`, the design of `packages`,
/// which the declarations it imports refer to.
pub(super) fn signature(
    tree: &SyntaxTree,
    path: &Path,
    packages: &packages::Packages,
    design: &mut packages::Design,
    name: String,
    members: &[String],
    names: &HashSet<String>,
) -> Converted<(crate::ir::Module, Vec<(String, Type)>, SignatureSource)> {
    let mut attempt = |declarations_only: bool| {
        let mut session = Session::with_packages(packages, design);
        let signature = signature_in(
            &mut session,
            tree,
            path,
            packages,
            name.clone(),
            members,
            names,
            declarations_only,
        );
        session.restore(design);
        signature.map(|(module, enumerations, mut source)| {
            source.types = design.types.clone();
            (module, enumerations, source)
        })
    };
    // A hierarchical name in a body is resolved where the body runs: from
    // the parent of each instance (IEEE 1800-2023 23.8), or, for a task or
    // function called through a virtual interface, from that of each
    // instance once the hierarchy is elaborated. The signature declares no
    // body, so it takes the declarations alone.
    match attempt(false) {
        Err(error) if hierarchical_reference(&error) => attempt(true),
        signature => signature,
    }
}

/// Whether `error` is a hierarchical name that the scope does not resolve.
pub(super) fn hierarchical_reference(error: &AnalyzerError) -> bool {
    match error {
        AnalyzerError::HierarchicalReference { .. } => true,
        AnalyzerError::WithWarnings { error, .. } => hierarchical_reference(error),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn signature_in(
    session: &mut Session,
    tree: &SyntaxTree,
    path: &Path,
    packages: &packages::Packages,
    name: String,
    members: &[String],
    names: &HashSet<String>,
    declarations_only: bool,
) -> Converted<(crate::ir::Module, Vec<(String, Type)>, SignatureSource)> {
    let source = SignatureSource::new(tree, path, packages, name, members, names)?;
    let (module, enumerations) =
        signature_from(session, &source, &HashMap::default(), declarations_only)?;
    Ok((module, enumerations, source))
}

fn signature_from(
    session: &mut Session,
    source: &SignatureSource,
    overrides: &HashMap<String, ConstExpr>,
    declarations_only: bool,
) -> Converted<(crate::ir::Module, Vec<(String, Type)>)> {
    let (name, members, names) = (source.name.clone(), &source.members, &source.names);
    let context = AnalysisContext::unspecialized(&source.tree);
    let context = context.child(&source.tree, overrides);
    let tree = &context;
    let mut imported = source.imported.clone();
    imported.declarations_only = declarations_only;
    let timed = AnalysisContext {
        timescale: source.timescale.clone(),
        ..tree.clone()
    };
    let tree = &timed;
    let interface = instances::normalize_identifier(&tree.root_name()?);
    let empty = ModuleInterfaces::default();
    let interfaces = InterfaceLookup {
        local: &empty,
        extra: &empty,
    };
    let mut module = Module::from_scope_with_parameter_overrides(
        tree,
        session,
        &interface,
        overrides,
        &interfaces,
        Arc::new(imported.clone()),
    )?;
    // Internal signal lowering freezes localparam-dependent dimensions for
    // execution. A public declaration must retain those dependencies so its
    // consumers can specialize the interface with different header values.
    let tree = &tree.importing(Arc::new(imported.clone()));
    let tree = &tree.with_functions(module.constant_functions.clone());
    let mut env = imported.const_env.clone();
    extend_const_env_with_parameters(tree, session, &mut env, &module.parameters);
    let aliases = type_aliases_from_scope_with_env(tree, session, &env)?;
    let declarations = declarations::signals_from_scope_with_declaration_ranges(
        tree, session, &env, &aliases, true,
    )?;
    // The members of an enumeration type, header ports included, whose type
    // the module's ports do not keep. An enumeration type is declared outside
    // the interface (one inside it is rejected, see
    // `interfaces::InterfaceDecl::collect`), whose bounds took their values
    // where it was declared.
    let enumerations = module
        .ports
        .iter()
        .map(|port| (port.name.as_str(), &port.r#type))
        .chain(
            declarations
                .iter()
                .map(|signal| (signal.name(), signal.r#type())),
        )
        .filter(|(name, ty)| {
            members.iter().any(|member| member == name)
                && ty.enumeration_in(&session.types).domain.is_some()
        })
        .map(|(name, ty)| (name.to_owned(), ty.clone()))
        .collect::<Vec<_>>();
    let names = names
        .iter()
        .map(|name| (name.clone(), format!("h${name}")))
        .collect::<HashMap<_, _>>();
    let rename = scope::Renamer { names: &names };
    module.name = name;
    let header_ports = module.ports.clone();
    module.ports = members
        .iter()
        .map(|name| {
            if let Some(port) = header_ports.iter().find(|port| port.name == *name) {
                return Ok(Port::new(
                    rename.name(name),
                    PortDirection::Input,
                    rename.r#type(port.r#type.clone()),
                    port.is_net,
                )
                .with_unresolved_net(port.unresolved_net)
                .with_net_resolution(port.net_resolution));
            }
            let signal = declarations
                .iter()
                .find(|signal| signal.name() == name)
                .ok_or_else(|| unsupported(format!("interface member `{name}`")))?;
            Ok(Port::new(
                rename.name(name),
                PortDirection::Input,
                rename.r#type(signal.r#type.clone()),
                signal.is_net,
            )
            .with_unresolved_net(signal.unresolved_net)
            .with_net_resolution(signal.net_resolution))
        })
        .collect::<Converted<_>>()?;
    module
        .signals
        .retain(|signal| !members.contains(&signal.name));
    for signal in &mut module.signals {
        signal.name = rename.name(&signal.name);
        signal.r#type = rename.r#type(signal.r#type.clone());
    }
    module.parameters = module
        .parameters
        .into_iter()
        .map(|p| rename.parameter(p))
        .collect();
    module.constant_functions = module
        .constant_functions
        .renamed(&mut |name| rename.name(name), &mut |stmt| rename.stmt(stmt));
    module.symbolic_functions = module
        .symbolic_functions
        .into_iter()
        .map(|(name, function)| (rename.name(&name), rename.function(function)))
        .collect();
    module.declaration_subroutines = module
        .declaration_subroutines
        .into_iter()
        .map(|subroutine| {
            let subroutine = subroutine.map(
                &mut Into::into,
                &mut |value| match value {
                    crate::ir::LValue::Ident(name) => LValue::Ident(name),
                    crate::ir::LValue::Select {
                        name,
                        msb,
                        lsb,
                        array_slice_width,
                        array_slice_reversed,
                        window,
                    } => LValue::Select {
                        name,
                        msb: msb.into(),
                        lsb: lsb.into(),
                        array_slice_width: array_slice_width.map(|width| (*width).into()),
                        array_slice_reversed,
                        window: window.map(Into::into),
                        signed: false,
                        is_2state: false,
                    },
                },
                &mut |ty| ty.map_range_expressions(|expr| rename.const_expr(expr.into()).into()),
            );
            rename
                .subroutine(subroutine)
                .map(&mut Into::into, &mut Into::into, &mut |ty| ty)
        })
        .collect();
    module.assignments.clear();
    module.comb_processes.clear();
    module.ff_processes.clear();
    module.initial_processes.clear();
    module.subroutines.clear();
    module.locals.clear();
    module.instances.clear();
    let (ir, publication) = crate::analyze::analyze_source_keeping(Source {
        modules: vec![module],
        diagnostics: Vec::new(),
        types: std::mem::take(&mut session.types),
    });
    session.types = publication.0;
    let ir = ir?;
    let module = ir
        .modules()
        .first()
        .cloned()
        .ok_or_else(|| unsupported("interface signature"))?;
    Ok((module, enumerations))
}
