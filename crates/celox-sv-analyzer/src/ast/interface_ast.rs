//! Native interface scope analysis and public type contracts.
use super::*;

type ActiveInstance = (
    std::path::PathBuf,
    String,
    HashMap<String, ConstExpr>,
    Vec<(String, Type)>,
);
#[derive(Default)]
pub(super) struct State {
    active_instances: RefCell<Vec<ActiveInstance>>,
}
struct InstanceGuard {
    state: std::rc::Rc<AnalysisState>,
    revision: u64,
}
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        self.state.interfaces.active_instances.borrow_mut().pop();
        self.state.revision.restore(self.revision);
    }
}

/// Analyze the original interface body and retain its symbols for the scope
/// that instantiates it. The structural view borrows every token's original
/// source location, including the lexical time scope of processes/functions.
pub(super) fn analyze(
    tree: &AnalysisContext<'_>,
    path: &Path,
    packages: &packages::Packages,
    overrides: &HashMap<String, ConstExpr>,
    types: &[(String, Type)],
    interfaces: &ModuleInterfaces,
    declarations_only: bool,
) -> Converted<Module> {
    let mut imported = imports::imported_symbols(tree, packages, packages.unit(path))?;
    imported.declarations_only = declarations_only;
    imported.type_parameter_bindings = type_parameters::bind(tree, types)?;
    let name = tree.root_name()?;
    let instance = (
        path.to_path_buf(),
        name.clone(),
        overrides.clone(),
        types.to_vec(),
    );
    {
        let mut instances = tree.state.interfaces.active_instances.borrow_mut();
        if instances.len() >= 64 {
            return Err(unsupported("interface instance nesting exceeds 64 levels"));
        }
        if instances.contains(&instance) {
            return Err(unsupported(format!(
                "recursive interface instantiation `{name}`"
            )));
        }
        instances.push(instance);
    }
    let _instance = InstanceGuard {
        state: tree.state.clone(),
        revision: tree.state.revision.change(),
    };
    let _timescales =
        timescales::install_for_tree(&tree.state.timescales, packages.timescales.clone(), tree)?;
    let lookup = InterfaceLookup {
        local: interfaces,
        extra: &ModuleInterfaces::default(),
    };
    let nested = interface_binding::analyze_module(
        tree,
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
        Module::from_scope_with_symbols(tree, &name, overrides, &lookup, Arc::new(imported), true)?
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

pub(super) fn signature(
    tree: &SyntaxTree,
    path: &Path,
    packages: &packages::Packages,
    name: String,
    members: &[String],
    names: &HashSet<String>,
) -> Converted<crate::ir::Module> {
    let projected_tree = interface_binding::declaration_view(tree)?;
    let context = AnalysisContext::unspecialized(&projected_tree);
    let tree = &context;
    let imported = imports::imported_symbols(tree, packages, packages.unit(path))?;
    let _timescales =
        timescales::install_for_tree(&tree.state.timescales, packages.timescales.clone(), tree)?;
    let empty = ModuleInterfaces::default();
    let interfaces = InterfaceLookup {
        local: &empty,
        extra: &empty,
    };
    let mut module = Module::from_scope_with_parameter_overrides(
        tree,
        "",
        &HashMap::default(),
        &interfaces,
        Arc::new(imported.clone()),
    )?;
    // Internal signal lowering freezes localparam-dependent dimensions for
    // execution. A public declaration must retain those dependencies so its
    // consumers can specialize the interface with different header values.
    let _scope = scope::install(&tree.state, Arc::new(imported.clone()));
    let _functions = const_functions::install(&tree.state, module.constant_functions.clone());
    let mut env = imported.const_env.clone();
    extend_const_env_with_parameters(&tree.state, &mut env, &module.parameters);
    let aliases = type_aliases_from_scope_with_env(tree, &env)?;
    let declarations =
        declarations::signals_from_scope_with_declaration_ranges(tree, &env, &aliases, true)?;
    let names = names
        .iter()
        .map(|name| (name.clone(), format!("h${name}")))
        .collect::<HashMap<_, _>>();
    let rename = scope::Renamer { names: &names };
    module.name = name;
    module.ports = members
        .iter()
        .map(|name| {
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
            .with_unresolved_net(signal.unresolved_net))
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
                    } => LValue::Select {
                        name,
                        msb: msb.into(),
                        lsb: lsb.into(),
                        array_slice_width: array_slice_width.map(Into::into),
                        array_slice_reversed,
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
    let ir = crate::analyze::analyze_source(Source {
        modules: vec![module],
        diagnostics: Vec::new(),
    })?;
    ir.modules()
        .first()
        .cloned()
        .ok_or_else(|| unsupported("interface signature"))
}
