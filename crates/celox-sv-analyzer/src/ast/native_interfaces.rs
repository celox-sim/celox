//! Interface instances bound in semantic scopes, retaining authored syntax.
use super::*;

pub(super) struct Bindings<'a> {
    pub values: &'a HashMap<String, ConstExpr>,
    pub types: &'a [(String, Type)],
    pub export_symbols: bool,
    pub plan: Option<&'a interfaces::NativeModulePlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InstanceBinding {
    pub module: String,
    pub connections: HashMap<String, Vec<PortConnection>>,
    pub parameters: Vec<ParameterOverride>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ScopeBinding {
    pub members: HashMap<String, String>,
    pub types: HashMap<String, String>,
    pub arrays: HashMap<String, ArrayBinding>,
    pub instances: HashMap<usize, InstanceBinding>,
}

/// Elements in declaration order, independent of the numeric index origins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArrayBinding {
    pub elements: Vec<String>,
    pub shape: Vec<usize>,
}

pub(super) fn analyze_module(
    declaration: &sv_parser::ModuleDeclarationAnsi,
    tree: &AnalysisContext<'_>,
    path: &Path,
    packages: &packages::Packages,
    bindings: Bindings<'_>,
    interfaces: &InterfaceLookup<'_>,
    mut imported: scope::ScopeSymbols,
) -> Converted<Option<Module>> {
    let mut definitions = HashMap::default();
    for (interface_tree, interface_path) in packages.interface_sources.values() {
        for node in interface_tree.as_ref() {
            if let RefNode::InterfaceDeclarationAnsi(declaration) = node {
                let name = identifier_text(
                    RefNode::InterfaceIdentifier(&declaration.nodes.0.nodes.3),
                    interface_tree,
                )
                .unwrap();
                definitions.insert(
                    name,
                    (
                        declaration,
                        interface_tree.as_ref(),
                        interface_path.as_path(),
                    ),
                );
            }
        }
    }
    // The direct syntax API also supports a self-contained compilation unit.
    for node in tree {
        if let RefNode::InterfaceDeclarationAnsi(declaration) = node {
            let name = identifier_text(
                RefNode::InterfaceIdentifier(&declaration.nodes.0.nodes.3),
                tree,
            )
            .unwrap();
            definitions.entry(name).or_insert((declaration, tree, path));
        }
    }
    if definitions.is_empty() {
        return Ok(None);
    }
    let context = tree.child(tree, bindings.values);
    let tree = &context;
    let name = module_name_from_node(RefNode::ModuleDeclarationAnsi(declaration), tree)?;
    let plan = bindings.plan;
    let overrides = bindings.values;
    let interface_ports = plan.map(|plan| plan.ports.as_slice()).unwrap_or_default();
    let mut view = declaration.clone();
    remove_interface_ports(&mut view, interface_ports, tree);
    let supplied = overrides;
    let overrides = overrides
        .iter()
        .filter(|(name, _)| {
            !interface_ports
                .iter()
                .any(|port| name.starts_with(&format!("{}$", port.name)))
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<HashMap<_, _>>();
    let mut instantiations = Vec::new();
    view.nodes.2.retain(|item| {
        if let sv_parser::NonPortModuleItem::ModuleOrGenerateItem(item) = item
            && let sv_parser::ModuleOrGenerateItem::Module(instance) = item.as_ref()
            && identifier_text(RefNode::ModuleIdentifier(&instance.nodes.1.nodes.0), tree)
                .is_some_and(|name| definitions.contains_key(&name))
        {
            instantiations.push(instance.nodes.1.clone());
            false
        } else {
            true
        }
    });
    let has_generated_interfaces = RefNode::ModuleDeclarationAnsi(&view)
        .into_iter()
        .any(|node| {
            if let RefNode::ModuleInstantiation(instance) = node {
                identifier_text(RefNode::ModuleIdentifier(&instance.nodes.0), tree)
                    .is_some_and(|name| definitions.contains_key(&name))
            } else {
                false
            }
        });
    if instantiations.is_empty() && interface_ports.is_empty() && !has_generated_interfaces {
        return Ok(None);
    }
    let array_scope = if interface_ports.iter().any(|port| port.array_rank != 0) {
        let header = binding_declaration_view(&view, &instantiations, tree, &imported)?;
        let mut header_imports = imported.clone();
        header_imports.declarations_only = true;
        Some(Module::from_scope_node_with_parameter_overrides(
            RefNode::ModuleDeclarationAnsi(&header),
            tree,
            &name,
            &overrides,
            interfaces,
            Arc::new(header_imports),
            true,
        )?)
    } else {
        None
    };
    for port in interface_ports {
        let (definition, definition_tree, definition_path) = definitions
            .get(&port.interface)
            .ok_or_else(|| unsupported(format!("interface `{}`", port.interface)))?;
        let prefix = format!("{}$", port.name);
        let values = supplied
            .iter()
            .filter_map(|(name, value)| {
                name.strip_prefix(&prefix)
                    .filter(|name| !name.starts_with('$'))
                    .map(|name| (name.to_string(), value.clone()))
            })
            .collect::<HashMap<_, _>>();
        let types = bindings
            .types
            .iter()
            .filter_map(|(name, ty)| {
                name.strip_prefix(&prefix)
                    .map(|name| (name.to_string(), ty.clone()))
            })
            .collect::<Vec<_>>();
        let dimensions = formal_dimensions(declaration, &port.name, tree)?;
        let array_symbols = array_scope
            .as_ref()
            .and_then(|module| module.symbols.as_ref())
            .unwrap_or(&imported);
        let array = instance_elements(
            &port.name,
            &dimensions,
            tree,
            &array_symbols.const_env,
            &array_symbols.type_aliases,
        )?;
        for (dimension, size) in array.shape.iter().enumerate() {
            if let Some(actual) = supplied.get(&format!("{}$$dimension{dimension}", port.name))
                && eval_ast_const_expr(&tree.state, actual, &array_symbols.const_env)
                    != Some(*size as i128)
            {
                return Err(unsupported(format!(
                    "interface array dimension mismatch at `{name}.{}`",
                    port.name
                )));
            }
        }
        if !dimensions.is_empty() {
            imported
                .interface_arrays
                .insert(port.name.clone(), array.clone());
        }
        for (ordinal, handle) in array.elements.iter().enumerate() {
            imported
                .interface_types
                .insert(handle.clone(), port.interface.clone());
            let symbol_prefix = if dimensions.is_empty() {
                port.name.clone()
            } else {
                format!("{}${ordinal}", port.name)
            };
            let interface =
                timescales::with_interface_instance(&tree.state.timescales, handle, || {
                    interface_ast::analyze(
                        definition,
                        &tree.child(definition_tree, &values),
                        definition_path,
                        packages,
                        &values,
                        &types,
                        interfaces.extra,
                        imported.declarations_only,
                    )
                })?;
            let symbols = interface.symbols.as_ref().unwrap();
            let names = binding_names_with_prefix(
                &interface,
                symbols,
                handle,
                &symbol_prefix,
                &mut imported,
            );
            let mut bound = instance_symbols(symbols, &names);
            let functions = port
                .functions
                .iter()
                .filter_map(|name| names.get(name))
                .cloned()
                .collect::<HashSet<_>>();
            bound
                .functions
                .retain(|name, _| name.contains("::") || functions.contains(name));
            bound.subroutines.retain(|function| {
                function.name.contains("::") || functions.contains(&function.name)
            });
            bound
                .constant_functions
                .retain(|name| name.contains("::") || functions.contains(name));
            bound.comb_processes.clear();
            bound.ff_processes.clear();
            let members = port
                .all_members
                .iter()
                .map(|name| names[name].clone())
                .collect::<HashSet<_>>();
            // Declaration initializers of interface variables belong to the
            // actual instance, not to each module receiving its port view.
            bound.initial_processes.retain(|process| {
                process.synthetic && procedural::written_names(&process.body).is_disjoint(&members)
            });
            for (member, direction) in &port.members {
                let signal = bound
                    .signals
                    .iter()
                    .find(|signal| signal.name == names[member])
                    .ok_or_else(|| {
                        unsupported(format!("interface member `{}.{member}`", port.name))
                    })?;
                bound.ports.push(Port::new(
                    signal.name.clone(),
                    *direction,
                    signal.r#type.clone(),
                    signal.is_net,
                ));
            }
            bound
                .signals
                .retain(|signal| !members.contains(&signal.name));
            imported.extend(&bound);
        }
    }
    // Resolve the caller's declarations before evaluating instance overrides.
    let header = binding_declaration_view(&view, &instantiations, tree, &imported)?;
    let mut header_imports = imported.clone();
    header_imports.declarations_only = true;
    let caller = Module::from_scope_node_with_parameter_overrides(
        RefNode::ModuleDeclarationAnsi(&header),
        tree,
        &name,
        &overrides,
        interfaces,
        Arc::new(header_imports),
        true,
    )?;
    let mut caller_symbols = caller.symbols.as_ref().unwrap().clone();
    let _caller_scope = scope::install(&tree.state, Arc::new(caller_symbols.clone()));
    let _functions = const_functions::install(&tree.state, caller.constant_functions.clone());
    let mut port_assignments = Vec::new();
    let mut pending = instantiations
        .iter()
        .cloned()
        .map(|instance| (instance, None))
        .collect::<std::collections::VecDeque<_>>();
    let mut generated = false;
    loop {
        if pending.is_empty() && !generated {
            // Interface constants now exist, so dependent ordinary declarations
            // can be collected before elaborating generate conditions and bounds.
            let header = declaration_view(&view);
            let mut header_imports = imported.clone();
            header_imports.declarations_only = true;
            let caller = Module::from_scope_node_with_parameter_overrides(
                RefNode::ModuleDeclarationAnsi(&header),
                tree,
                &name,
                &overrides,
                interfaces,
                Arc::new(header_imports),
                true,
            )?;
            caller_symbols = caller.symbols.unwrap();
            generated = true;
            let mut current = caller_symbols.clone();
            current.extend(&imported);
            let _scope = scope::install(&tree.state, Arc::new(current.clone()));
            let _functions =
                const_functions::install(&tree.state, current.constant_functions.clone());
            for item in generate::items(
                RefNode::ModuleDeclarationAnsi(&view),
                tree,
                &current.const_env,
                &current.type_aliases,
            )? {
                if let ScopeItem::Module(sv_parser::ModuleOrGenerateItem::Module(instance)) =
                    item.node
                    && identifier_text(RefNode::ModuleIdentifier(&instance.nodes.1.nodes.0), tree)
                        .is_some_and(|name| definitions.contains_key(&name))
                {
                    imported.interface_declarations.insert(
                        identifier_locate(RefNode::ModuleIdentifier(&instance.nodes.1.nodes.0))
                            .unwrap()
                            .offset,
                    );
                    pending.push_back((instance.nodes.1.clone(), Some(item)));
                }
            }
        }
        let Some((instantiation, generated_item)) = pending.pop_front() else {
            break;
        };
        if generated_item.is_none() {
            // Newly bound instances may unlock declarations used by the next
            // instance's overrides. Keep only still-unbound dependencies out.
            let header = binding_declaration_view(&view, &instantiations, tree, &imported)?;
            let mut header_imports = imported.clone();
            header_imports.declarations_only = true;
            caller_symbols = Module::from_scope_node_with_parameter_overrides(
                RefNode::ModuleDeclarationAnsi(&header),
                tree,
                &name,
                &overrides,
                interfaces,
                Arc::new(header_imports),
                true,
            )?
            .symbols
            .unwrap();
        }
        let mut current = caller_symbols.clone();
        current.extend(&imported);
        if let Some(item) = &generated_item {
            current
                .const_env
                .extend(item.env.iter().map(|(name, value)| (name.clone(), *value)));
        }
        let _scope = scope::install(&tree.state, Arc::new(current.clone()));
        let _functions = const_functions::install(&tree.state, current.constant_functions.clone());
        let mut dimensions = packed_dimensions_from_ports_and_signals(
            &current.ports,
            &current.signals,
            &current.const_env,
            &current.type_aliases,
        );
        dimensions.extend(parameter_packed_dimensions(&current.parameters));
        let interface_name =
            identifier_text(RefNode::ModuleIdentifier(&instantiation.nodes.0), tree).unwrap();
        let (definition, definition_tree, definition_path) = definitions[&interface_name];
        let interface_view = interface_ast::module_view(definition)?;
        let interface = module_interface_from_node(
            RefNode::ModuleDeclarationAnsi(&interface_view),
            definition_tree,
        )?;
        let bindings = instances::parameter_overrides_from_value_assignment(
            instantiation.nodes.1.as_ref(),
            tree,
            Some(&interface),
            &dimensions,
            true,
        )?;
        let mut values = HashMap::default();
        let mut types = Vec::new();
        for binding in bindings {
            if let Some(ty) = binding.bound_type {
                types.push((binding.name, ty));
            } else {
                let value = binding
                    .value
                    .ok_or_else(|| unsupported("interface pattern parameter binding"))?;
                let value = substitute_typed_parameter_literals(
                    value,
                    &current.const_env,
                    &parameter_types_from_const_env(&current.const_env),
                );
                values.insert(binding.name, value);
            }
        }
        for instance in instantiation.nodes.2.contents() {
            let handle =
                identifier_text(RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0), tree)
                    .ok_or_else(|| unsupported("interface instance name"))?;
            let elements = instance_elements(
                &handle,
                &instance.nodes.0.nodes.1,
                tree,
                &current.const_env,
                &current.type_aliases,
            )?;
            if !elements.shape.is_empty() {
                let qualified = generated_item
                    .as_ref()
                    .map_or_else(|| handle.clone(), |item| item.instance_name(&handle));
                let absolute = ArrayBinding {
                    elements: elements
                        .elements
                        .iter()
                        .map(|element| {
                            generated_item
                                .as_ref()
                                .map_or_else(|| element.clone(), |item| item.instance_name(element))
                        })
                        .collect(),
                    shape: elements.shape.clone(),
                };
                imported.interface_arrays.insert(qualified, absolute);
                if let Some(item) = &generated_item {
                    imported
                        .interface_scopes
                        .entry(item.scope_name().to_string())
                        .or_default()
                        .arrays
                        .insert(handle.clone(), elements.clone());
                }
            }
            for local_handle in elements.elements {
                let handle = generated_item.as_ref().map_or_else(
                    || local_handle.clone(),
                    |item| item.instance_name(&local_handle),
                );
                imported
                    .interface_types
                    .insert(handle.clone(), interface_name.clone());
                let connections = instances::port_connections_from_hierarchical_instance(
                    instance,
                    tree,
                    &dimensions,
                    Some(&interface),
                )?;
                let mut module =
                    timescales::with_interface_instance(&tree.state.timescales, &handle, || {
                        interface_ast::analyze(
                            definition,
                            &tree.child(definition_tree, &values),
                            definition_path,
                            packages,
                            &values,
                            &types,
                            interfaces.extra,
                            imported.declarations_only,
                        )
                    })?;
                for port in &module.ports {
                    let signal = Signal {
                        name: port.name.clone(),
                        r#type: port.r#type.clone(),
                        is_net: port.is_net,
                        package_variable: None,
                    };
                    module.symbols.as_mut().unwrap().signals.push(signal);
                    let Some(connection) = connections
                        .iter()
                        .find(|connection| connection.formal == port.name)
                    else {
                        continue;
                    };
                    let Some(actual) = connection.actual_expr.clone() else {
                        continue;
                    };
                    let bound = format!("{handle}${}", port.name);
                    let assignment = match port.direction {
                        PortDirection::Input => Assignment::new(LValue::Ident(bound), actual),
                        PortDirection::Output => {
                            Assignment::new(connection_lvalue(actual)?, Expr::Ident(bound))
                        }
                        _ => return Err(unsupported("bidirectional interface declaration port")),
                    };
                    port_assignments.push(assignment);
                }
                let symbols = module.symbols.as_ref().unwrap();
                let names = binding_names(&module, symbols, &handle, &mut imported);
                if let Some(item) = &generated_item {
                    let scope = imported
                        .interface_scopes
                        .entry(item.scope_name().to_string())
                        .or_default();
                    scope
                        .types
                        .insert(local_handle.clone(), interface_name.clone());
                    for (member, bound) in &names {
                        scope
                            .members
                            .insert(format!("{local_handle}.{member}"), bound.clone());
                    }
                }
                let bound = instance_symbols(symbols, &names);
                imported
                    .initial_processes
                    .extend(bound.initial_processes.iter().cloned());
                imported.extend(&bound);
            }
        }
    }
    if let Some(plan) = plan {
        let mut current = caller_symbols.clone();
        current.extend(&imported);
        let _scope = scope::install(&tree.state, Arc::new(current.clone()));
        let _functions = const_functions::install(&tree.state, current.constant_functions.clone());
        for item in generate::items(
            RefNode::ModuleDeclarationAnsi(&view),
            tree,
            &current.const_env,
            &current.type_aliases,
        )? {
            let _interface_scope = item.interface_scope();
            let active_scope = scope::imported(&tree.state);
            let ScopeItem::Module(sv_parser::ModuleOrGenerateItem::Module(instantiation)) =
                item.node
            else {
                continue;
            };
            let instantiation = &instantiation.nodes.1;
            let original =
                identifier_text(RefNode::ModuleIdentifier(&instantiation.nodes.0), tree).unwrap();
            for instance in instantiation.nodes.2.contents() {
                let offset =
                    identifier_locate(RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0))
                        .unwrap()
                        .offset;
                let Some(alias) = plan.children.get(&offset) else {
                    continue;
                };
                let child = &packages.interface_plan.modules[alias];
                let mut connections = HashMap::default();
                let mut parameters = Vec::new();
                for port in &child.ports {
                    let actual = connection_expression(
                        instance,
                        &port.name,
                        interfaces.get(&original),
                        tree,
                    )?;
                    let env = &item.env;
                    let handle = match actual {
                        Some(actual) => {
                            actual_handle_key(actual, tree, env, &active_scope.type_aliases)?
                        }
                        None => port.name.clone(),
                    };
                    let actuals = if port.array_rank == 0 {
                        ArrayBinding {
                            elements: vec![handle.clone()],
                            shape: Vec::new(),
                        }
                    } else {
                        active_scope
                            .interface_arrays
                            .get(&handle)
                            .cloned()
                            .ok_or_else(|| {
                                unsupported(format!("interface array actual `{handle}`"))
                            })?
                    };
                    if actuals.shape.len() != port.array_rank {
                        return Err(unsupported(format!(
                            "interface array rank mismatch at `{original}.{}`",
                            port.name
                        )));
                    }
                    let mut members = Vec::new();
                    for (ordinal, handle) in actuals.elements.iter().enumerate() {
                        let prefix = if port.array_rank == 0 {
                            port.name.clone()
                        } else {
                            format!("{}${ordinal}", port.name)
                        };
                        for (member, _) in &port.members {
                            let actual = active_scope
                                .interface_members
                                .get(&format!("{handle}.{member}"))
                                .ok_or_else(|| {
                                    unsupported(format!(
                                        "unbound interface member `{handle}.{member}`"
                                    ))
                                })?
                                .clone();
                            members.push(PortConnection::new(
                                format!("{prefix}${member}"),
                                actual.clone(),
                                Some(Expr::Ident(actual)),
                            ));
                        }
                    }
                    connections.insert(port.name.clone(), members);
                    for (dimension, size) in actuals.shape.iter().enumerate() {
                        parameters.push(ParameterOverride::new(
                            format!("{}$$dimension{dimension}", port.name),
                            Some(ConstExpr::Literal(size.to_string())),
                        ));
                    }
                    let handle = &actuals.elements[0];
                    for parameter in &port.type_parameters {
                        let bound = active_scope
                            .interface_members
                            .get(&format!("{handle}.{parameter}"))
                            .ok_or_else(|| {
                                unsupported(format!(
                                    "unbound interface type parameter `{handle}.{parameter}`"
                                ))
                            })?;
                        let ty = active_scope.type_aliases.get(bound).ok_or_else(|| {
                            unsupported(format!("interface type parameter `{handle}.{parameter}`"))
                        })?;
                        parameters.push(ParameterOverride::type_override(
                            format!("{}${parameter}", port.name),
                            ty.clone(),
                        ));
                    }
                    for parameter in &port.parameters {
                        let bound = active_scope
                            .interface_members
                            .get(&format!("{handle}.{parameter}"))
                            .ok_or_else(|| {
                                unsupported(format!(
                                    "unbound interface parameter `{handle}.{parameter}`"
                                ))
                            })?;
                        let value = active_scope
                            .parameter_values
                            .get(bound)
                            .cloned()
                            .and_then(|expr| expr_to_const(&tree.state, expr))
                            .ok_or_else(|| {
                                unsupported(format!("interface parameter `{handle}.{parameter}`"))
                            })?;
                        parameters.push(ParameterOverride::new(
                            format!("{}${parameter}", port.name),
                            Some(value),
                        ));
                    }
                }
                let binding = InstanceBinding {
                    module: alias.clone(),
                    connections,
                    parameters,
                };
                if item.scope_name().is_empty() {
                    imported.interface_instances.insert(offset, binding);
                } else {
                    imported
                        .interface_scopes
                        .entry(item.scope_name().to_string())
                        .or_default()
                        .instances
                        .insert(offset, binding);
                }
            }
        }
    }
    for assignment in port_assignments {
        imported.comb_processes.push(CombProcess::new(
            CombProcessKind::ContinuousAssign,
            None,
            vec![assignment],
        ));
    }
    // Check authored declarations before module collection can prefer a local
    // function/type/constant over an imported interface binding of the same name.
    let authored = scope_declared_names(RefNode::ModuleDeclarationAnsi(declaration), tree)?
        .into_iter()
        .collect::<HashSet<_>>();
    for bound in imported.interface_members.values() {
        if authored.contains(bound) {
            return Err(unsupported(format!(
                "interface binding collides with declaration `{bound}`"
            )));
        }
    }
    let module = Module::from_scope_node_with_parameter_overrides(
        RefNode::ModuleDeclarationAnsi(&view),
        tree,
        &name,
        &overrides,
        interfaces,
        Arc::new(imported),
        bindings.export_symbols,
    )?;
    let mut names = HashSet::default();
    for name in module
        .ports
        .iter()
        .map(|port| &port.name)
        .chain(module.signals.iter().map(|signal| &signal.name))
    {
        if !names.insert(name) {
            return Err(unsupported(format!(
                "interface binding collides with declaration `{name}`"
            )));
        }
    }
    Ok(Some(module))
}

/// Delay declarations that reference local interface instances (and declarations
/// depending on those names) until the instances have populated semantic scopes.
fn binding_declaration_view(
    module: &sv_parser::ModuleDeclarationAnsi,
    instances: &[sv_parser::ModuleInstantiation],
    tree: &SyntaxTree,
    imported: &scope::ScopeSymbols,
) -> Converted<sv_parser::ModuleDeclarationAnsi> {
    let mut deferred = instances
        .iter()
        .flat_map(|instance| instance.nodes.2.contents())
        .filter_map(|instance| {
            identifier_text(RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0), tree)
        })
        .filter(|name| {
            !imported.interface_types.contains_key(name)
                && !imported.interface_arrays.contains_key(name)
        })
        .collect::<HashSet<_>>();
    let mut header = declaration_view(module);
    loop {
        let mut removed = false;
        let mut kept = Vec::new();
        for item in header.nodes.2 {
            if references_deferred(RefNode::NonPortModuleItem(&item), tree, &deferred) {
                deferred.extend(scope_declared_names(
                    RefNode::NonPortModuleItem(&item),
                    tree,
                )?);
                removed = true;
            } else {
                kept.push(item);
            }
        }
        header.nodes.2 = kept;
        if let Some(ports) = &mut header.nodes.0.nodes.6
            && let Some(list) = &ports.nodes.0.nodes.1
        {
            let comma = list.nodes.1.first().map(|(comma, _)| comma.clone());
            let mut kept = Vec::new();
            let mut inherited_deferred = false;
            for port in list.contents() {
                let inherits_type = match &port.1 {
                    sv_parser::AnsiPortDeclaration::Net(port) => port.nodes.0.is_none(),
                    sv_parser::AnsiPortDeclaration::Variable(port) => port.nodes.0.is_none(),
                    sv_parser::AnsiPortDeclaration::Paren(_) => false,
                };
                inherited_deferred = (inherits_type && inherited_deferred)
                    || references_deferred(RefNode::AnsiPortDeclaration(&port.1), tree, &deferred);
                if inherited_deferred {
                    deferred.extend(scope_declared_names(
                        RefNode::AnsiPortDeclaration(&port.1),
                        tree,
                    )?);
                    removed = true;
                } else {
                    kept.push(port.clone());
                }
            }
            let mut kept = kept.into_iter();
            ports.nodes.0.nodes.1 = kept.next().map(|first| sv_parser::List {
                nodes: (
                    first,
                    kept.map(|port| (comma.as_ref().unwrap().clone(), port))
                        .collect(),
                ),
            });
        }
        if !removed {
            break;
        }
    }
    Ok(header)
}

/// The name introduced by a declaration, rather than identifiers used by its
/// type or initializer. The caller determines which lexical scopes to collect.
fn declaration_identifier(node: RefNode<'_>) -> Option<RefNode<'_>> {
    Some(match node {
        RefNode::VariableDeclAssignmentVariable(d) => RefNode::VariableIdentifier(&d.nodes.0),
        RefNode::NetDeclAssignment(d) => RefNode::NetIdentifier(&d.nodes.0),
        RefNode::ParamAssignment(d) => RefNode::ParameterIdentifier(&d.nodes.0),
        RefNode::TypeAssignment(d) => RefNode::TypeIdentifier(&d.nodes.0),
        RefNode::TypeDeclarationDataType(d) => RefNode::TypeIdentifier(&d.nodes.2),
        RefNode::EnumNameDeclaration(d) => RefNode::EnumIdentifier(&d.nodes.0),
        RefNode::AnsiPortDeclarationNet(d) => RefNode::PortIdentifier(&d.nodes.1),
        RefNode::AnsiPortDeclarationVariable(d) => RefNode::PortIdentifier(&d.nodes.1),
        RefNode::AnsiPortDeclarationParen(d) => RefNode::PortIdentifier(&d.nodes.2),
        RefNode::TfPortItem(d) => RefNode::PortIdentifier(&d.nodes.4.as_ref()?.0),
        RefNode::FunctionDeclaration(d) => {
            unwrap_node!(RefNode::FunctionDeclaration(d), FunctionIdentifier)?
        }
        RefNode::TaskDeclaration(d) => unwrap_node!(RefNode::TaskDeclaration(d), TaskIdentifier)?,
        _ => return None,
    })
}

/// Collect only names introduced in this scope. Subroutine names belong here,
/// but their formals/locals, process locals, and structure members do not.
fn scope_declared_names(root: RefNode<'_>, tree: &SyntaxTree) -> Converted<Vec<String>> {
    let mut names = Vec::new();
    let mut depth = 0usize;
    for event in root.into_iter().event() {
        let (entering, node) = match event {
            sv_parser::NodeEvent::Enter(node) => (true, node),
            sv_parser::NodeEvent::Leave(node) => (false, node),
        };
        let subroutine = matches!(
            node,
            RefNode::FunctionDeclaration(_) | RefNode::TaskDeclaration(_)
        );
        let nested = subroutine
            || matches!(
                node,
                RefNode::StructUnionMember(_)
                    | RefNode::InitialConstruct(_)
                    | RefNode::AlwaysConstruct(_)
                    | RefNode::FinalConstruct(_)
                    | RefNode::ConditionalGenerateConstruct(_)
                    | RefNode::LoopGenerateConstruct(_)
            );
        if entering
            && depth == 0
            && (!nested || subroutine)
            && let Some(identifier) = declaration_identifier(node.clone())
            && let Some(name) = identifier_text(identifier, tree)
        {
            names.push(name);
        }
        if nested {
            if entering {
                depth += 1;
            } else {
                depth -= 1;
            }
        }
    }
    Ok(names)
}

fn references_deferred(node: RefNode<'_>, tree: &SyntaxTree, deferred: &HashSet<String>) -> bool {
    // Only names looked up in the enclosing scope can depend on an unbound
    // instance. Qualified package names, member names, and references shadowed
    // by nested declarations belong to other bindings.
    super::imports::unqualified_names(node, tree)
        .keys()
        .any(|name| deferred.contains(name))
}

fn declaration_view(module: &sv_parser::ModuleDeclarationAnsi) -> sv_parser::ModuleDeclarationAnsi {
    let mut header = module.clone();
    header.nodes.2.retain(|item| match item {
        sv_parser::NonPortModuleItem::TimeunitsDeclaration(_) => true,
        sv_parser::NonPortModuleItem::ModuleOrGenerateItem(item) => matches!(item.as_ref(),
            sv_parser::ModuleOrGenerateItem::ModuleItem(item)
                if matches!(&item.nodes.1, sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(_))),
        _ => false,
    });
    header
}

fn formal_dimensions(
    module: &sv_parser::ModuleDeclarationAnsi,
    port_name: &str,
    tree: &SyntaxTree,
) -> Converted<Vec<sv_parser::UnpackedDimension>> {
    for node in RefNode::ModuleDeclarationAnsi(module) {
        let RefNode::AnsiPortDeclaration(port) = node else {
            continue;
        };
        let Some(identifier) = unwrap_node!(RefNode::AnsiPortDeclaration(port), PortIdentifier)
        else {
            continue;
        };
        if identifier_text(identifier, tree).as_deref() != Some(port_name) {
            continue;
        }
        return match port {
            sv_parser::AnsiPortDeclaration::Net(port) => Ok(port.nodes.2.clone()),
            sv_parser::AnsiPortDeclaration::Variable(port) => port
                .nodes
                .2
                .iter()
                .map(|dimension| match dimension {
                    sv_parser::VariableDimension::UnpackedDimension(dimension) => {
                        Ok((**dimension).clone())
                    }
                    _ => Err(unsupported("non-static interface port array dimension")),
                })
                .collect(),
            _ => Err(unsupported("interface port declaration")),
        };
    }
    Err(unsupported(format!("missing interface port `{port_name}`")))
}

fn connection_lvalue(expr: Expr) -> Converted<LValue> {
    match expr {
        Expr::Ident(name) => Ok(LValue::Ident(name)),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => {
            let Expr::Ident(name) = *expr else {
                return Err(unsupported("interface output connection"));
            };
            Ok(LValue::Select {
                name,
                msb,
                lsb,
                signed,
                is_2state: false,
                array_slice_width: None,
                array_slice_reversed: false,
            })
        }
        _ => Err(unsupported("interface output connection")),
    }
}

// Nested handles are private to the interface whose body has already been
// analyzed. Export its bound objects and processes without leaking those
// lexical handle paths into the caller.
fn instance_symbols(
    symbols: &scope::ScopeSymbols,
    names: &HashMap<String, String>,
) -> scope::ScopeSymbols {
    let mut bound = symbols.renamed(names);
    bound.interface_members.clear();
    bound.interface_types.clear();
    bound.interface_arrays.clear();
    bound.interface_scopes.clear();
    bound.interface_instances.clear();
    bound
}

fn binding_names(
    module: &Module,
    symbols: &scope::ScopeSymbols,
    handle: &str,
    imported: &mut scope::ScopeSymbols,
) -> HashMap<String, String> {
    binding_names_with_prefix(module, symbols, handle, handle, imported)
}

fn binding_names_with_prefix(
    module: &Module,
    symbols: &scope::ScopeSymbols,
    handle: &str,
    prefix: &str,
    imported: &mut scope::ScopeSymbols,
) -> HashMap<String, String> {
    let mut names = symbols.local_names();
    names.extend(
        module
            .parameters
            .iter()
            .map(|parameter| parameter.name.clone()),
    );
    names.extend(
        symbols
            .parameters
            .iter()
            .filter(|parameter| parameter.name.contains('$'))
            .map(|parameter| parameter.name.clone()),
    );
    names.extend(module.signals.iter().map(|signal| signal.name.clone()));
    names.extend(module.ports.iter().map(|port| port.name.clone()));
    names.extend(symbols.type_aliases.keys().cloned());
    names.extend(symbols.functions.keys().cloned());
    names.extend(symbols.enum_constants.numbers.keys().cloned());
    names.retain(|name| !name.contains("::"));
    let mut names: HashMap<_, _> = names
        .into_iter()
        .map(|name| {
            let bound = format!("{prefix}${name}");
            imported
                .interface_members
                .insert(format!("{handle}.{name}"), bound.clone());
            (name, bound)
        })
        .collect();
    // An imported package constant is not an instance-local declaration.
    // Bind references in exported parameter defaults to its qualified name;
    // the caller need not import the package's simple name itself.
    for (alias, target) in &symbols.aliases {
        names.entry(alias.clone()).or_insert_with(|| target.clone());
    }
    names
}

fn remove_interface_ports(
    module: &mut sv_parser::ModuleDeclarationAnsi,
    ports: &[interfaces::NativePortPlan],
    tree: &SyntaxTree,
) {
    let Some(declarations) = &mut module.nodes.0.nodes.6 else {
        return;
    };
    let Some(list) = &declarations.nodes.0.nodes.1 else {
        return;
    };
    let comma = list.nodes.1.first().map(|(comma, _)| comma.clone());
    let kept = list
        .contents()
        .into_iter()
        .filter(|(_, port)| {
            let name = identifier_text(RefNode::AnsiPortDeclaration(port), tree);
            // The declaration's first identifier can be its type; take its port identifier.
            let name = unwrap_node!(RefNode::AnsiPortDeclaration(port), PortIdentifier)
                .and_then(|node| identifier_text(node, tree))
                .or(name);
            !ports.iter().any(|port| Some(&port.name) == name.as_ref())
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut kept = kept.into_iter();
    declarations.nodes.0.nodes.1 = kept.next().map(|first| sv_parser::List {
        nodes: (
            first,
            kept.map(|port| (comma.as_ref().unwrap().clone(), port))
                .collect(),
        ),
    });
}

fn connection_expression<'a>(
    instance: &'a sv_parser::HierarchicalInstance,
    formal: &str,
    interface: Option<&ModuleInterface>,
    tree: &SyntaxTree,
) -> Converted<Option<&'a sv_parser::Expression>> {
    let found = match instance.nodes.1.nodes.1.as_ref() {
        Some(sv_parser::ListOfPortConnections::Named(connections)) => connections
            .nodes
            .0
            .contents()
            .into_iter()
            .find_map(|connection| {
                let sv_parser::NamedPortConnection::Identifier(connection) = connection else {
                    return None;
                };
                if identifier_text(RefNode::PortIdentifier(&connection.nodes.2), tree).as_deref()
                    != Some(formal)
                {
                    return None;
                }
                match &connection.nodes.3 {
                    None => Some(None),
                    Some(actual) => actual.nodes.1.as_ref().map(Some),
                }
            }),
        Some(sv_parser::ListOfPortConnections::Ordered(connections)) => interface
            .and_then(|interface| interface.ports.iter().position(|name| name == formal))
            .and_then(|index| {
                connections
                    .nodes
                    .0
                    .contents()
                    .get(index)
                    .and_then(|connection| connection.nodes.1.as_ref())
                    .map(Some)
            }),
        _ => None,
    };
    found.ok_or_else(|| unsupported(format!("missing interface connection `{formal}`")))
}

pub(super) fn method_call<'a>(
    call: &'a sv_parser::MethodCall,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<(String, Option<&'a sv_parser::ListOfArguments>)> {
    let sv_parser::MethodCallRoot::Primary(root) = &call.nodes.0 else {
        return Err(unsupported("method call"));
    };
    let sv_parser::Primary::Hierarchical(root) = root.as_ref() else {
        return Err(unsupported("interface method root"));
    };
    let handle = primary_handle_key(root, tree, &dimensions.const_env, &dimensions.type_aliases)?;
    let (identifier, arguments) = match &call.nodes.2 {
        sv_parser::MethodCallBody::User(method) => (
            &method.nodes.0,
            method.nodes.2.as_ref().map(|paren| &paren.nodes.1),
        ),
        sv_parser::MethodCallBody::BuiltInMethodCall(method) => {
            let sv_parser::BuiltInMethodCall::ArrayManipulationCall(method) = method.as_ref()
            else {
                return Err(unsupported("interface builtin method"));
            };
            let sv_parser::ArrayMethodName::MethodIdentifier(identifier) = &method.nodes.0 else {
                return Err(unsupported("interface builtin method"));
            };
            if method.nodes.3.is_some() {
                return Err(unsupported("with clause on interface method"));
            }
            (
                identifier.as_ref(),
                method.nodes.2.as_ref().map(|paren| &paren.nodes.1),
            )
        }
    };
    let member = identifier_text(RefNode::MethodIdentifier(identifier), tree)
        .ok_or_else(|| unsupported("interface method name"))?;
    let name = scope::imported(&tree.state)
        .interface_members
        .get(&format!("{handle}.{member}"))
        .cloned()
        .ok_or_else(|| unsupported("method call"))?;
    Ok((name, arguments))
}

fn instance_elements(
    handle: &str,
    dimensions: &[sv_parser::UnpackedDimension],
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<ArrayBinding> {
    let ranges = unpacked_ranges_from_dimensions_with_env(dimensions, tree, env, aliases)?;
    validate_unpacked_dimension_sizes(&tree.state, &ranges, env)?;
    let mut shape = Vec::new();
    let mut elements = vec![handle.to_string()];
    for range in ranges {
        let left = eval_ast_const_expr(&tree.state, &range.left, env)
            .ok_or_else(|| unsupported("interface array bound"))?;
        let right = eval_ast_const_expr(&tree.state, &range.right, env)
            .ok_or_else(|| unsupported("interface array bound"))?;
        let count = usize::try_from(left.abs_diff(right))
            .ok()
            .and_then(|count| count.checked_add(1))
            .filter(|count| {
                elements
                    .len()
                    .checked_mul(*count)
                    .is_some_and(|count| count <= 4096)
            })
            .ok_or_else(|| unsupported("interface array elaboration limit exceeded"))?;
        shape.push(count);
        elements = elements
            .into_iter()
            .flat_map(|prefix| {
                (0..count).map(move |offset| {
                    let index = if left <= right {
                        left + offset as i128
                    } else {
                        left - offset as i128
                    };
                    format!("{prefix}[{index}]")
                })
            })
            .collect();
    }
    Ok(ArrayBinding { elements, shape })
}

fn actual_handle_key(
    expression: &sv_parser::Expression,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<String> {
    let sv_parser::Expression::Primary(primary) = expression else {
        return Err(unsupported("interface handle expression"));
    };
    let sv_parser::Primary::Hierarchical(primary) = primary.as_ref() else {
        return Err(unsupported("interface handle expression"));
    };
    primary_handle_key(primary, tree, env, aliases)
}

fn primary_handle_key(
    primary: &sv_parser::PrimaryHierarchical,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<String> {
    if primary.nodes.2.nodes.2.is_some() {
        return Err(unsupported("range selection of interface handle"));
    }
    let mut handle = identifier_text(RefNode::HierarchicalIdentifier(&primary.nodes.1), tree)
        .ok_or_else(|| unsupported("interface handle"))?;
    for bit in &primary.nodes.2.nodes.1.nodes.0 {
        let dimensions = PackedDimensions::new(HashMap::default(), env, aliases);
        let expression = expr_from_expression_with_types(&bit.nodes.1, tree, &dimensions)?;
        let value = expr_to_const(&tree.state, expression)
            .and_then(|value| eval_ast_const_expr(&tree.state, &value, env))
            .ok_or_else(|| unsupported("nonconstant interface index"))?;
        handle.push_str(&format!("[{value}]"));
    }
    Ok(handle)
}
