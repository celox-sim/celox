//! Interface instances bound in semantic scopes, retaining authored syntax.
use super::*;
pub(super) type InstanceKey = (celox_sv_syntax::BodyItemId, usize);

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
    pub instances: HashMap<InstanceKey, InstanceBinding>,
}

/// Elements in declaration order, independent of the numeric index origins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArrayBinding {
    pub elements: Vec<String>,
    pub shape: Vec<usize>,
    /// Authored bounds, retained for selecting actual interface array elements.
    pub ranges: Vec<(i128, i128)>,
}

/// Compatibility shorthand for an unindexed interface-array value parameter.
/// Follow the first element in declaration order, including nonzero bounds.
/// Runtime members and type parameters retain their ordinary indexed lookup.
pub(super) fn unindexed_parameter(
    path: &[String],
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    root: celox_sv_syntax::ExprId,
) -> Option<String> {
    let (member, handle) = path.split_last()?;
    let root_name = handle.first()?;
    if dimensions.get(root_name).is_some()
        || dimensions.const_env.contains_key(root_name)
        || dimensions.parameter_values.contains_key(root_name)
    {
        return None;
    }
    let imported = scope::imported(&tree.state);
    let array = imported.interface_arrays.get(&handle.join("."))?;
    let element = array.elements.first()?;
    let bound = imported
        .interface_members
        .get(&format!("{element}.{member}"))?;
    if !imported.parameter_values.contains_key(bound) {
        return None;
    }
    warnings::append(
        &mut tree.state.diagnostics.borrow_mut(),
        [warnings::AnalyzerWarning::InterfaceArrayParameter {
            path: path.join("."),
            element: element.clone(),
            site: procedural::source_span(tree.declarations().expression(root).tokens, tree),
        }],
    );
    Some(bound.clone())
}

pub(super) fn analyze_module(
    tree: &AnalysisContext<'_>,
    path: &Path,
    packages: &packages::Packages,
    bindings: Bindings<'_>,
    interfaces: &InterfaceLookup<'_>,
    mut imported: scope::ScopeSymbols,
) -> Converted<Option<Module>> {
    let mut definitions = HashMap::default();
    for (interface_tree, interface_path) in packages.interface_sources.values() {
        for handle in interface_tree.root_scopes()? {
            let scoped = interface_tree.scoped(handle)?;
            if scoped.root_unit()?.kind == celox_sv_syntax::UnitKind::Interface {
                let name = instances::normalize_identifier(&scoped.root_name()?);
                definitions.insert(name, (scoped, interface_path.as_path()));
            }
        }
    }
    for handle in tree.root_scopes()? {
        let scoped = tree.scoped(handle)?;
        if scoped.root_unit()?.kind == celox_sv_syntax::UnitKind::Interface {
            let name = instances::normalize_identifier(&scoped.root_name()?);
            definitions.entry(name).or_insert((scoped, path));
        }
    }
    if definitions.is_empty() {
        return Ok(None);
    }
    let name = tree.root_name()?;
    let plan = bindings.plan;
    let overrides = bindings.values;
    let interface_ports = plan.map(|plan| plan.ports.as_slice()).unwrap_or_default();
    let mut view_ports = port_owners::Selection::authored(tree)?;
    remove_interface_ports(&mut view_ports, interface_ports, tree)?;
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
    // Direct interface declarations retain native ownership before the
    // structural view removes their original nodes. Explicit generate regions
    // remain in the later generated-item scheduling path.
    let mut instantiations = Vec::new();
    for id in tree.body_membership()? {
        if let celox_sv_syntax::BodyItemKind::Instance(native) =
            tree.declarations().body_item(id).kind
            && definitions.contains_key(&instances::normalize_identifier(
                &tree.declarations().instantiation(native).name.spelling,
            ))
        {
            instantiations.push(id);
        }
    }
    let selected_header_tree = view_ports.project(tree)?;
    let projected_tree = retain_body(
        &selected_header_tree,
        |id| Ok(!instantiations.contains(&id)),
    )?;
    let projected_context = tree.child(&projected_tree, bindings.values);
    let tree = &projected_context;
    let has_generated_interfaces =
        native_body_items(tree)?
            .iter()
            .any(|id| match tree.declarations().body_item(*id).kind {
                celox_sv_syntax::BodyItemKind::Instance(id) => {
                    definitions.contains_key(&instances::normalize_identifier(
                        &tree.declarations().instantiation(id).name.spelling,
                    ))
                }
                _ => false,
            });
    if instantiations.is_empty() && interface_ports.is_empty() && !has_generated_interfaces {
        return Ok(None);
    }
    let array_scope = if interface_ports.iter().any(|port| port.array_rank != 0) {
        let (header_ports, header_tree) = binding_declaration_view(
            &view_ports,
            &instantiations,
            interface_ports,
            tree,
            &imported,
        )?;
        let mut header_imports = imported.clone();
        header_imports.declarations_only = true;
        Some(Module::from_selected_scope(
            &tree.child(&header_tree, &overrides),
            &name,
            &overrides,
            interfaces,
            Arc::new(header_imports),
            true,
            Some(&header_ports),
        )?)
    } else {
        None
    };
    for port in interface_ports {
        let (definition_tree, definition_path) = definitions
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
        let dimensions = formal_dimensions(&port.name, tree)?;
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
                bound.ports.push(
                    Port::new(
                        signal.name.clone(),
                        *direction,
                        signal.r#type.clone(),
                        signal.is_net,
                    )
                    .with_unresolved_net(signal.unresolved_net),
                );
            }
            bound
                .signals
                .retain(|signal| !members.contains(&signal.name));
            imported.extend(&bound);
        }
    }
    // Resolve the caller's declarations before evaluating instance overrides.
    let (header_ports, header_tree) = binding_declaration_view(
        &view_ports,
        &instantiations,
        interface_ports,
        tree,
        &imported,
    )?;
    let mut header_imports = imported.clone();
    header_imports.declarations_only = true;
    let caller = Module::from_selected_scope(
        &tree.child(&header_tree, &overrides),
        &name,
        &overrides,
        interfaces,
        Arc::new(header_imports),
        true,
        Some(&header_ports),
    )?;
    let mut caller_ports = caller.ports.clone();
    let mut caller_symbols = caller.symbols.as_ref().unwrap().clone();
    let _caller_scope = scope::install(&tree.state, Arc::new(caller_symbols.clone()));
    let _functions = const_functions::install(&tree.state, caller.constant_functions.clone());
    let mut port_assignments = Vec::new();
    let mut pending = instantiations
        .iter()
        .copied()
        .map(|id| (id, None))
        .collect::<std::collections::VecDeque<_>>();
    let mut generated = false;
    loop {
        if pending.is_empty() && !generated {
            // Interface constants now exist, so dependent ordinary declarations
            // can be collected before elaborating generate conditions and bounds.
            let header_tree = declaration_view(tree)?;
            let header_ports = view_ports.clone();
            let mut header_imports = imported.clone();
            header_imports.declarations_only = true;
            let caller = Module::from_selected_scope(
                &tree.child(&header_tree, &overrides),
                &name,
                &overrides,
                interfaces,
                Arc::new(header_imports),
                true,
                Some(&header_ports),
            )?;
            caller_ports = caller.ports.clone();
            caller_symbols = caller.symbols.unwrap();
            generated = true;
            let mut current = caller_symbols.clone();
            current.extend(&imported);
            let _scope = scope::install(&tree.state, Arc::new(current.clone()));
            let _functions =
                const_functions::install(&tree.state, current.constant_functions.clone());
            for item in generate::items(tree, &current.const_env, &current.type_aliases)? {
                if let celox_sv_syntax::BodyItemKind::Instance(native) =
                    tree.declarations().body_item(item.id).kind
                    && definitions.contains_key(&instances::normalize_identifier(
                        &tree.declarations().instantiation(native).name.spelling,
                    ))
                {
                    imported.interface_declarations.insert(item.id);
                    pending.push_back((item.id, Some(item)));
                }
            }
        }
        let Some((id, generated_item)) = pending.pop_front() else {
            break;
        };
        if generated_item.is_none() {
            // Newly bound instances may unlock declarations used by the next
            // instance's overrides. Keep only still-unbound dependencies out.
            let (header_ports, header_tree) = binding_declaration_view(
                &view_ports,
                &instantiations,
                interface_ports,
                tree,
                &imported,
            )?;
            let mut header_imports = imported.clone();
            header_imports.declarations_only = true;
            let caller = Module::from_selected_scope(
                &tree.child(&header_tree, &overrides),
                &name,
                &overrides,
                interfaces,
                Arc::new(header_imports),
                true,
                Some(&header_ports),
            )?;
            caller_ports = caller.ports.clone();
            caller_symbols = caller.symbols.unwrap();
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
        // Exported scope symbols omit ordinary module ports. Keep the
        // selected caller header available for connection width discovery.
        let mut connection_ports = caller_ports.clone();
        connection_ports.extend(current.ports.iter().cloned());
        let mut dimensions = packed_dimensions_from_ports_and_signals(
            &connection_ports,
            &current.signals,
            &current.const_env,
            &current.type_aliases,
        );
        dimensions.extend(parameter_packed_dimensions(&current.parameters));
        dimensions.functions = Arc::new(current.functions.clone());
        dimensions
            .function_return_types
            .extend(current.function_return_types.clone());
        if let Some(item) = &generated_item {
            let mut views = generate::ScopeViews::new(&dimensions);
            dimensions = views.dimensions(item).clone();
        }
        let native = instances::declaration_from_item(id, tree)?;
        let interface_name = instances::normalize_identifier(&native.name.spelling);
        let (definition_tree, definition_path) = &definitions[&interface_name];
        let interface =
            module_interface_from_scope(&tree.child(definition_tree, &HashMap::default()))?;
        let bindings = instances::parameter_overrides(
            native.parameters.as_deref(),
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
        for (index, authored) in native.instances.iter().enumerate() {
            let handle = instances::normalize_identifier(&authored.name.spelling);
            let elements = instance_elements(
                &handle,
                &authored.dimensions,
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
                    ranges: elements.ranges.clone(),
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
            for (ordinal, local_handle) in elements.elements.iter().enumerate() {
                let handle = generated_item.as_ref().map_or_else(
                    || local_handle.clone(),
                    |item| item.instance_name(local_handle),
                );
                imported
                    .interface_types
                    .insert(handle.clone(), interface_name.clone());
                let connections = instances::port_connections(
                    (id, index),
                    authored,
                    tree,
                    &dimensions,
                    Some(&interface),
                )?;
                let mut module =
                    timescales::with_interface_instance(&tree.state.timescales, &handle, || {
                        interface_ast::analyze(
                            &tree.child(definition_tree, &values),
                            definition_path,
                            packages,
                            &values,
                            &types,
                            interfaces.extra,
                            imported.declarations_only,
                        )
                    })?;
                let port_state = module.constant_function_state();
                let port_symbols = module.symbols.as_ref().unwrap();
                let port_dimensions = packed_dimensions_from_ports_and_signals(
                    &module.ports,
                    &module.signals,
                    &port_symbols.const_env,
                    &port_symbols.type_aliases,
                );
                for port in &module.ports {
                    let signal = Signal {
                        name: port.name.clone(),
                        r#type: port.r#type.clone(),
                        is_net: port.is_net,
                        unresolved_net: port.unresolved_net,
                        is_const: false,
                        package_variable: None,
                        hierarchical: None,
                    };
                    module.symbols.as_mut().unwrap().signals.push(signal);
                    let Some(connection) = connections
                        .iter()
                        .find(|connection| connection.formal == port.name)
                    else {
                        continue;
                    };
                    let actual = if let Some(pattern) = &connection.bound_pattern {
                        if port.direction != PortDirection::Input {
                            return Err(unsupported(
                                "output/inout/ref interface port assignment pattern",
                            ));
                        }
                        let mut pattern = pattern.clone();
                        pattern.resolve_types(&dimensions)?;
                        pattern.map_expressions(&mut |expr| {
                            let mut expr = substitute_expr_constants_with_parameter_literals(
                                &tree.state,
                                expr,
                                &dimensions.const_env,
                                &current.parameter_values,
                            );
                            if let Some(item) = &generated_item {
                                item.expr(&mut expr);
                            }
                            expr
                        });
                        let shape = port_dimensions.get(&port.name).ok_or_else(|| {
                            unsupported("interface assignment pattern port shape")
                        })?;
                        pattern.lower(&port_state, shape, &port_dimensions)?
                    } else if let Some(actual) = connection.actual_expr.clone() {
                        actual
                    } else {
                        continue;
                    };
                    // An untyped pattern takes the single formal's context,
                    // so every element of an interface array receives it.
                    let actual = if elements.shape.is_empty() || connection.bound_pattern.is_some()
                    {
                        actual
                    } else {
                        let port_shape = port_dimensions.get(&port.name).unwrap();
                        let port_width = lvalue_expr_type(
                            &port_state,
                            &LValue::Ident(port.name.clone()),
                            &port_dimensions,
                        )
                        .ok_or_else(|| unsupported("interface array port width"))?
                        .width;
                        let actual_width = expr_static_width(&tree.state, &actual, &dimensions)
                            .ok_or_else(|| unsupported("interface array connection width"))?;
                        let actual_shape = if let Some(root) =
                            connection_expression(authored, &port.name, Some(&interface))?
                        {
                            dimensions::operand_shape(root, tree, &dimensions)?
                        } else if let Expr::Ident(name) = &actual {
                            dimensions.get(name).cloned()
                        } else {
                            None
                        };
                        let unpacked_sizes =
                            |shape: &VariableDimensions,
                             state: &AnalysisState,
                             env: &HashMap<String, i128>| {
                                shape
                                    .unpacked
                                    .iter()
                                    .map(|dimension| {
                                        usize::try_from(eval_ast_const_expr(
                                            state,
                                            &dimension.width,
                                            env,
                                        )?)
                                        .ok()
                                    })
                                    .collect::<Option<Vec<_>>>()
                            };
                        let actual_shape = actual_shape
                            .as_ref()
                            .map(|shape| unpacked_sizes(shape, &tree.state, &dimensions.const_env))
                            .unwrap_or_else(|| Some(Vec::new()))
                            .ok_or_else(|| unsupported("interface array actual shape"))?;
                        let port_shape =
                            unpacked_sizes(port_shape, &port_state, &port_dimensions.const_env)
                                .ok_or_else(|| unsupported("interface array port shape"))?;
                        let (origin, ascending) =
                            connection_slice_origin(&actual, &tree.state, &dimensions)?;
                        let shape = ConnectionShape {
                            unpacked: actual_shape,
                            width: actual_width,
                            origin,
                            ascending,
                        };
                        distribute_port_connection(
                            actual,
                            &shape,
                            &port_shape,
                            port_width,
                            &elements,
                            ordinal,
                        )?
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
        for item in generate::items(tree, &current.const_env, &current.type_aliases)? {
            let _interface_scope = item.interface_scope();
            let active_scope = scope::imported(&tree.state);
            let celox_sv_syntax::BodyItemKind::Instance(native) =
                tree.declarations().body_item(item.id).kind
            else {
                continue;
            };
            let native = tree.declarations().instantiation(native);
            let original = instances::normalize_identifier(&native.name.spelling);
            for (index, authored) in native.instances.iter().enumerate() {
                let Some(alias) = plan.children.get(&(item.id, index)) else {
                    continue;
                };
                let child = &packages.interface_plan.modules[alias];
                let mut connections = HashMap::default();
                let mut parameters = Vec::new();
                for port in &child.ports {
                    let actual =
                        connection_expression(authored, &port.name, interfaces.get(&original))?;
                    let env = &item.env;
                    let actuals = connection_array(actual, &port.name, tree, env, &active_scope)?;
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
                    imported
                        .interface_instances
                        .insert((item.id, index), binding);
                } else {
                    imported
                        .interface_scopes
                        .entry(item.scope_name().to_string())
                        .or_default()
                        .instances
                        .insert((item.id, index), binding);
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
    let authored = imports::scope_names(tree)?;
    for (path, bound) in &imported.interface_members {
        if authored.contains(bound) && !imported.local_aliases.contains(path) {
            return Err(unsupported(format!(
                "interface binding collides with declaration `{bound}`"
            )));
        }
    }
    let module = Module::from_selected_scope(
        tree,
        &name,
        &overrides,
        interfaces,
        Arc::new(imported),
        bindings.export_symbols,
        Some(&view_ports),
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

fn native_body_items(tree: &SyntaxTree) -> Converted<Vec<celox_sv_syntax::BodyItemId>> {
    use celox_sv_syntax::BodyItemKind;
    let mut pending = tree.body_membership()?;
    let mut seen = HashSet::default();
    let mut items = Vec::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(AnalyzerError::Parse(
                "repeated native interface body owner".into(),
            ));
        }
        items.push(id);
        match &tree.declarations().body_item(id).kind {
            BodyItemKind::Region(children)
            | BodyItemKind::Block {
                items: children, ..
            }
            | BodyItemKind::Unconverted {
                items: children, ..
            } => pending.extend(children),
            BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } => {
                pending.push(*then_body);
                pending.extend(otherwise);
            }
            BodyItemKind::Case { arms, .. } => pending.extend(arms.iter().map(|arm| arm.body)),
            BodyItemKind::Loop { body, .. } => pending.push(*body),
            _ => {}
        }
    }
    Ok(items)
}

/// Delay declarations that reference unbound interface instances or ports (and declarations
/// depending on those names) until the instances have populated semantic scopes.
fn binding_declaration_view<'a>(
    selected: &port_owners::Selection<'a>,
    instances: &[celox_sv_syntax::BodyItemId],
    ports: &[interfaces::NativePortPlan],
    tree: &AnalysisContext<'_>,
    imported: &scope::ScopeSymbols,
) -> Converted<(port_owners::Selection<'a>, SyntaxTree)> {
    let mut selected = selected.clone();
    let mut deferred = instances
        .iter()
        .filter_map(|id| instances::declaration_from_item(*id, tree).ok())
        .flat_map(|instance| instance.instances.iter())
        .map(|instance| instances::normalize_identifier(&instance.name.spelling))
        .chain(ports.iter().map(|port| port.name.clone()))
        .filter(|name| {
            !imported.interface_types.contains_key(name)
                && !imported.interface_arrays.contains_key(name)
        })
        .collect::<HashSet<_>>();
    let references = imports::unqualified_names(tree);
    let mut header_tree = declaration_view(tree)?;
    loop {
        let mut removed = false;
        header_tree = retain_body(&header_tree, |id| {
            if references_deferred(
                tree.declarations().body_item(id).tokens,
                &references,
                &deferred,
            ) {
                deferred.extend(imports::item_names(id, tree));
                removed = true;
                Ok(false)
            } else {
                Ok(true)
            }
        })?;
        let unit = tree.root_unit()?;
        let native_ports = port_owners::scope(Some(&selected), tree)?
            .into_iter()
            .filter(|port| {
                port.tokens.start >= unit.header.start && port.tokens.end <= unit.header.end
            });
        let mut inherited_deferred = false;
        let mut membership = Vec::new();
        for native in native_ports {
            let inherits_type = !matches!(native.kind, celox_sv_syntax::PortKind::Explicit { .. })
                && native.data_type.is_some_and(|id| {
                    tree.declarations().data_type(id).tokens.start < native.tokens.start
                });
            inherited_deferred = (inherits_type && inherited_deferred)
                || references_deferred(native.tokens, &references, &deferred);
            membership.push(!inherited_deferred);
            if inherited_deferred {
                match &native.kind {
                    celox_sv_syntax::PortKind::Named(name)
                    | celox_sv_syntax::PortKind::Explicit { name, .. } => {
                        deferred.insert(instances::normalize_identifier(&name.spelling));
                    }
                    _ => {}
                }
                removed = true;
            }
        }
        selected.retain(&membership)?;
        if !removed {
            break;
        }
    }
    let header_tree = selected.project(&header_tree)?;
    Ok((selected, header_tree))
}

fn references_deferred(
    range: celox_sv_syntax::TokenRange,
    references: &HashMap<String, Vec<usize>>,
    deferred: &HashSet<String>,
) -> bool {
    references.iter().any(|(name, uses)| {
        deferred.contains(name)
            && uses
                .iter()
                .any(|index| (range.start..range.end).contains(index))
    })
}

pub(super) fn retain_body(
    tree: &SyntaxTree,
    mut keep: impl FnMut(celox_sv_syntax::BodyItemId) -> Converted<bool>,
) -> Converted<SyntaxTree> {
    let mut membership = Vec::new();
    for id in tree.body_membership()? {
        if keep(id)? {
            membership.push(id);
        }
    }
    tree.with_body_membership(membership)
}

pub(super) fn declaration_view(tree: &SyntaxTree) -> Converted<SyntaxTree> {
    retain_body(tree, |id| {
        use celox_sv_syntax::BodyItemKind::*;
        Ok(matches!(
            tree.declarations().body_item(id).kind,
            Data(_)
                | Net(_)
                | Dpi(_)
                | Parameter(_)
                | Subroutine(_)
                | TypeAlias(_)
                | PackageBinding(_)
                | Unconverted { .. }
        ))
    })
}

fn formal_dimensions(
    port_name: &str,
    tree: &AnalysisContext<'_>,
) -> Converted<Vec<celox_sv_syntax::Dimension>> {
    tree.root_unit()?
        .ports
        .iter()
        .find(|port| match &port.kind {
            celox_sv_syntax::PortKind::Named(name)
            | celox_sv_syntax::PortKind::Explicit { name, .. } => {
                instances::normalize_identifier(&name.spelling) == port_name
            }
            _ => false,
        })
        .map(|port| port.dimensions.clone())
        .ok_or_else(|| unsupported(format!("missing interface port `{port_name}`")))
}

struct ConnectionShape {
    unpacked: Vec<usize>,
    width: usize,
    origin: ConstExpr,
    ascending: bool,
}

/// Single packed dimensions retain authored indices in analyzer expressions;
/// unpacked and multi-packed selections already use flat storage coordinates.
fn connection_slice_origin(
    actual: &Expr,
    state: &AnalysisState,
    dimensions: &PackedDimensions,
) -> Converted<(ConstExpr, bool)> {
    let (name, selected_origin) = match actual {
        Expr::Ident(name) => (name, None),
        Expr::Select { expr, lsb, .. } => {
            let Expr::Ident(name) = expr.as_ref() else {
                return Ok((ConstExpr::Literal("0".into()), false));
            };
            (name, Some(lsb.clone()))
        }
        _ => return Ok((ConstExpr::Literal("0".into()), false)),
    };
    if let Some(shape) = dimensions.get(name)
        && shape.unpacked.is_empty()
        && let [dimension] = shape.packed.as_slice()
        && !dimension.normalize_single
    {
        let left = eval_ast_const_expr(state, &dimension.left, &dimensions.const_env)
            .ok_or_else(|| unsupported("interface connection packed bound"))?;
        let right = eval_ast_const_expr(state, &dimension.right, &dimensions.const_env)
            .ok_or_else(|| unsupported("interface connection packed bound"))?;
        return Ok((
            selected_origin.unwrap_or_else(|| dimension.right.clone()),
            left < right,
        ));
    }
    Ok((
        selected_origin.unwrap_or_else(|| ConstExpr::Literal("0".into())),
        false,
    ))
}

/// IEEE 1800-2023 23.3.3.5. Packed actuals distribute MSB-first;
/// unpacked actuals use the compiler's declaration-order element layout.
fn distribute_port_connection(
    actual: Expr,
    shape: &ConnectionShape,
    port_shape: &[usize],
    port_width: usize,
    elements: &ArrayBinding,
    ordinal: usize,
) -> Converted<Expr> {
    let actual_shape = shape.unpacked.as_slice();
    let actual_width = shape.width;
    if actual_shape == port_shape && actual_width == port_width {
        return Ok(actual);
    }
    let packed = actual_shape.is_empty() && port_shape.is_empty();
    let distributed_shape = elements
        .shape
        .iter()
        .chain(port_shape)
        .copied()
        .collect::<Vec<_>>();
    if (!packed && actual_shape != distributed_shape)
        || port_width.checked_mul(elements.elements.len()) != Some(actual_width)
    {
        return Err(unsupported(format!(
            "interface array connection shape/width: {actual_width} bits {actual_shape:?} for {} ports of {port_width} bits {port_shape:?}",
            elements.elements.len(),
        )));
    }
    let offset = if packed {
        elements.elements.len() - ordinal - 1
    } else {
        ordinal
    } * port_width;
    let coordinate = |offset: usize| {
        let offset = const_expr_from_i128(offset as i128);
        if shape.ascending {
            ConstExpr::Binary {
                left: Box::new(shape.origin.clone()),
                op: BinaryOp::Sub,
                right: Box::new(offset),
            }
        } else {
            add_expr(shape.origin.clone(), offset)
        }
    };
    let msb = coordinate(offset + port_width - 1);
    let lsb = coordinate(offset);
    let expr = match actual {
        Expr::Select { expr, .. } if matches!(expr.as_ref(), Expr::Ident(_)) => expr,
        value => Box::new(value),
    };
    Ok(Expr::Select {
        expr,
        msb,
        lsb,
        signed: false,
    })
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
    selected: &mut port_owners::Selection<'_>,
    ports: &[interfaces::NativePortPlan],
    tree: &SyntaxTree,
) -> Converted<()> {
    let unit = tree.root_unit()?;
    let membership = port_owners::scope(Some(selected), tree)?
        .into_iter()
        .filter(|port| port.tokens.start >= unit.header.start && port.tokens.end <= unit.header.end)
        .map(|native| {
            let name = match &native.kind {
                celox_sv_syntax::PortKind::Named(name)
                | celox_sv_syntax::PortKind::Explicit { name, .. } => {
                    Some(instances::normalize_identifier(&name.spelling))
                }
                _ => None,
            };
            !ports.iter().any(|port| Some(&port.name) == name.as_ref())
        })
        .collect::<Vec<_>>();
    selected.retain(&membership)
}

fn connection_expression(
    instance: &celox_sv_syntax::InstanceDeclarator,
    formal: &str,
    interface: Option<&ModuleInterface>,
) -> Converted<Option<celox_sv_syntax::ExprId>> {
    use celox_sv_syntax::{ConnectionValue, InstanceConnectionKind};
    let found = if instance
        .connections
        .iter()
        .any(|connection| matches!(connection.kind, InstanceConnectionKind::Named { .. }))
    {
        instance
            .connections
            .iter()
            .find_map(|connection| match &connection.kind {
                InstanceConnectionKind::Named { name, value }
                    if instances::normalize_identifier(&name.spelling) == formal =>
                {
                    match value {
                        ConnectionValue::Expression(root) => Some(Some(*root)),
                        ConnectionValue::Implicit => Some(None),
                        ConnectionValue::Empty => None,
                    }
                }
                _ => None,
            })
    } else {
        interface
            .and_then(|interface| interface.ports.iter().position(|name| name == formal))
            .and_then(|index| instance.connections.get(index))
            .and_then(|connection| match connection.kind {
                InstanceConnectionKind::Ordered(Some(root)) => Some(Some(root)),
                _ => None,
            })
    };
    found.ok_or_else(|| unsupported(format!("missing interface connection `{formal}`")))
}

fn instance_elements(
    handle: &str,
    dimensions: &[celox_sv_syntax::Dimension],
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<ArrayBinding> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    let ranges = constant_binding::bind_unpacked_ranges(dimensions, tree, &context)?;
    validate_unpacked_dimension_sizes(&tree.state, &ranges, env)?;
    let mut shape = Vec::new();
    let mut bounds = Vec::new();
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
        bounds.push((left, right));
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
    Ok(ArrayBinding {
        elements,
        shape,
        ranges: bounds,
    })
}

/// Bind an interface actual as a selection of elaborated instances. Selections
/// consume authored dimensions; a slice retains its dimension while an index
/// removes it. Elements stay in declaration order for left-to-left port mapping.
fn connection_array(
    actual: Option<celox_sv_syntax::ExprId>,
    implicit: &str,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    scope: &scope::ScopeSymbols,
) -> Converted<ArrayBinding> {
    use celox_sv_syntax::ExprKind;
    let native = tree.declarations();
    let mut selections = Vec::new();
    let mut root = actual;
    while let Some(id) = root {
        let id = expression_binding::ungroup(id, native);
        if let ExprKind::Select {
            expression,
            dimension,
        } = &native.expression(id).kind
        {
            selections.push(&dimension.kind);
            root = Some(*expression);
        } else {
            root = Some(id);
            break;
        }
    }
    let handle = match root {
        Some(root) => actual_handle_key(root, tree, env, &scope.type_aliases)?,
        None => implicit.to_string(),
    };
    let Some(array) = scope.interface_arrays.get(&handle) else {
        // A selection in a hierarchical prefix may name a generated scope;
        // retain the existing scalar-handle resolution for that case.
        return Ok(ArrayBinding {
            elements: vec![match actual {
                Some(actual) => actual_handle_key(actual, tree, env, &scope.type_aliases)?,
                None => handle,
            }],
            shape: Vec::new(),
            ranges: Vec::new(),
        });
    };
    if selections.is_empty() {
        return Ok(array.clone());
    }
    if selections.len() > array.shape.len() {
        return Err(unsupported("too many interface array selections"));
    }
    if selections
        .iter()
        .filter(|selection| {
            matches!(
                selection,
                celox_sv_syntax::DimensionKind::Range { .. }
                    | celox_sv_syntax::DimensionKind::Indexed { .. }
            )
        })
        .count()
        > 1
    {
        return Err(unsupported("interface array slices in multiple dimensions"));
    }
    let context = PackedDimensions::new(HashMap::default(), env, &scope.type_aliases);
    let evaluate = |root| -> Converted<i128> {
        constant_binding::bind(root, tree, &context)?
            .and_then(|value| eval_ast_const_expr(&tree.state, &value, env))
            .ok_or_else(|| unsupported("nonconstant interface array selection"))
    };
    let selections = selections.into_iter().rev().collect::<Vec<_>>();
    let mut offsets = vec![0];
    let mut shape = Vec::new();
    let mut ranges = Vec::new();
    for (dimension, (&size, &(left, right))) in array.shape.iter().zip(&array.ranges).enumerate() {
        let (selected_left, selected_right, retain) = match selections.get(dimension) {
            None => (left, right, true),
            Some(celox_sv_syntax::DimensionKind::Index(index)) => {
                let index = evaluate(*index)?;
                (index, index, false)
            }
            Some(selection) => {
                let (start, end) = selection_range(selection, left <= right, &evaluate)?;
                (start, end, true)
            }
        };
        if selected_left < left.min(right)
            || selected_left > left.max(right)
            || selected_right < left.min(right)
            || selected_right > left.max(right)
        {
            return Err(unsupported(
                "interface array selection outside its declared range",
            ));
        }
        if selected_left != selected_right && (selected_left < selected_right) != (left < right) {
            return Err(unsupported("reversed interface array selection"));
        }
        // Bounds were checked against an elaborated dimension (at most 4096
        // elements), so these differences and products fit in usize.
        let start = left.abs_diff(selected_left) as usize;
        let end = left.abs_diff(selected_right) as usize;
        offsets = offsets
            .into_iter()
            .flat_map(|prefix| (start..=end).map(move |index| prefix * size + index))
            .collect();
        if retain {
            shape.push(end - start + 1);
            ranges.push((selected_left, selected_right));
        }
    }
    Ok(ArrayBinding {
        elements: offsets
            .into_iter()
            .map(|offset| array.elements[offset].clone())
            .collect(),
        shape,
        ranges,
    })
}

fn selection_range(
    selection: &celox_sv_syntax::DimensionKind,
    ascending: bool,
    evaluate: &impl Fn(celox_sv_syntax::ExprId) -> Converted<i128>,
) -> Converted<(i128, i128)> {
    use celox_sv_syntax::DimensionKind;
    match selection {
        DimensionKind::Range { left, right } => Ok((evaluate(*left)?, evaluate(*right)?)),
        DimensionKind::Indexed {
            base,
            operator,
            width,
        } => {
            let base = evaluate(*base)?;
            let width = evaluate(*width)?;
            let extent = width
                .checked_sub(1)
                .filter(|_| width > 0)
                .ok_or_else(|| unsupported("nonpositive interface array selection width"))?;
            let (low, high) = if operator == "+:" {
                (Some(base), base.checked_add(extent))
            } else {
                (base.checked_sub(extent), Some(base))
            };
            let (low, high) = low
                .zip(high)
                .ok_or_else(|| unsupported("interface array selection bound overflow"))?;
            Ok(if ascending { (low, high) } else { (high, low) })
        }
        _ => Err(unsupported("interface array selection")),
    }
}

pub(super) fn actual_handle_key(
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<String> {
    use celox_sv_syntax::{DimensionKind, ExprKind};
    enum Suffix {
        Member(String),
        Index(celox_sv_syntax::ExprId),
    }
    let native = tree.declarations();
    let mut root = root;
    let mut suffixes = Vec::new();
    let mut handle = loop {
        root = expression_binding::ungroup(root, native);
        match &native.expression(root).kind {
            ExprKind::Name(name) => break instances::normalize_identifier(&name.spelling),
            ExprKind::Member {
                expression,
                name,
                separator,
            } if separator == "." => {
                suffixes.push(Suffix::Member(instances::normalize_identifier(
                    &name.spelling,
                )));
                root = *expression;
            }
            ExprKind::Select {
                expression,
                dimension,
            } => {
                let DimensionKind::Index(index) = dimension.kind else {
                    return Err(unsupported("interface handle expression"));
                };
                suffixes.push(Suffix::Index(index));
                root = *expression;
            }
            _ => return Err(unsupported("interface handle expression")),
        }
    };
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    for suffix in suffixes.into_iter().rev() {
        match suffix {
            Suffix::Member(name) => {
                handle.push('.');
                handle.push_str(&name);
            }
            Suffix::Index(root) => {
                let value = constant_binding::bind(root, tree, &context)?
                    .and_then(|value| eval_ast_const_expr(&tree.state, &value, env))
                    .ok_or_else(|| unsupported("nonconstant interface index"))?;
                handle.push_str(&format!("[{value}]"));
            }
        }
    }
    Ok(handle)
}
