//! Interface instances of a module, bound to their members and port connections.

use super::*;

impl ModuleBinding<'_> {
    /// Binds the interface instances of the module, those it declares directly and
    /// then those its generate constructs elaborate, in the order they are found.
    pub(super) fn bind_instances(
        &mut self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
    ) -> Converted<()> {
        self.port_assignments = Vec::new();
        let mut pending = self
            .instantiations
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
                let header_ports = self.view_ports.clone();
                let mut header_imports = self.imported.clone();
                header_imports.declarations_only = true;
                let caller = Module::from_selected_scope(
                    &tree.child(&header_tree, &self.overrides),
                    session,
                    &self.name,
                    &self.overrides,
                    self.interfaces,
                    Arc::new(header_imports),
                    true,
                    Some(&header_ports),
                )?;
                self.caller_ports = caller.ports.clone();
                self.caller_symbols = caller.symbols.unwrap();
                generated = true;
                let mut current = self.caller_symbols.clone();
                current.extend(&self.imported);
                let tree = &tree.importing(Arc::new(current.clone()));
                let tree = &tree.with_functions(current.constant_functions.clone());
                for item in
                    generate::items(tree, session, &current.const_env, &current.type_aliases)?
                {
                    if let celox_sv_syntax::BodyItemKind::Instance(native) =
                        tree.declarations().body_item(item.id).kind
                        && self
                            .definitions
                            .contains_key(&instances::normalize_identifier(
                                &tree.declarations().instantiation(native).name.spelling,
                            ))
                    {
                        self.imported.interface_declarations.insert(item.id);
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
                    &self.view_ports,
                    &self.instantiations,
                    self.interface_ports,
                    tree,
                    &self.imported,
                )?;
                let mut header_imports = self.imported.clone();
                header_imports.declarations_only = true;
                let caller = Module::from_selected_scope(
                    &tree.child(&header_tree, &self.overrides),
                    session,
                    &self.name,
                    &self.overrides,
                    self.interfaces,
                    Arc::new(header_imports),
                    true,
                    Some(&header_ports),
                )?;
                self.caller_ports = caller.ports.clone();
                self.caller_symbols = caller.symbols.unwrap();
            }
            self.bind_instance(tree, session, id, generated_item)?;
        }
        Ok(())
    }

    /// Binds the instances of one interface instantiation, `generated_item` if a
    /// generate construct elaborated it: their members become symbols the module
    /// imports, and their port connections assignments of the module.
    pub(super) fn bind_instance(
        &mut self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        id: celox_sv_syntax::BodyItemId,
        generated_item: Option<generate::Item>,
    ) -> Converted<()> {
        let mut current = self.caller_symbols.clone();
        current.extend(&self.imported);
        if let Some(item) = &generated_item {
            current
                .const_env
                .extend(item.env.iter().map(|(name, value)| (name.clone(), *value)));
        }
        let tree = &tree.importing(Arc::new(current.clone()));
        let tree = &tree.with_functions(current.constant_functions.clone());
        // Exported scope symbols omit ordinary module ports. Keep the
        // selected caller header available for connection width discovery.
        let mut connection_ports = self.caller_ports.clone();
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
        let (definition_tree, definition_path) = &self.definitions[&interface_name];
        let interface =
            module_interface_from_scope(&tree.child(definition_tree, &HashMap::default()))?;
        let bindings = instances::parameter_overrides(
            native.parameters.as_deref(),
            tree,
            session,
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
                session,
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
                self.imported.interface_arrays.insert(qualified, absolute);
                if let Some(item) = &generated_item {
                    self.imported
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
                self.imported
                    .interface_types
                    .insert(handle.clone(), interface_name.clone());
                self.imported
                    .state_signals
                    .extend(virtual_interfaces::identity_signal(
                        session,
                        &handle,
                        &interface_name,
                    ));
                let connections = instances::port_connections(
                    (id, index),
                    authored,
                    tree,
                    session,
                    &dimensions,
                    Some(&interface),
                )?;
                let mut module = {
                    let tree = &AnalysisContext {
                        interface_instance: Some(timescales::interface_instance(
                            tree.interface_instance.as_deref(),
                            &handle,
                        )),
                        ..tree.clone()
                    };
                    interface_ast::analyze(
                        &tree.child(definition_tree, &values),
                        session,
                        definition_path,
                        self.packages,
                        &values,
                        &types,
                        self.interfaces.extra,
                        self.imported.declarations_only,
                    )
                }?;
                let port_syntax = SyntaxTree::empty();
                let port_tree = module.constant_context(&port_syntax);
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
                        net_resolution: port.net_resolution,
                        net_drive: NetDrive::None,
                        is_const: false,
                        package_variable: None,
                        hierarchical: None,
                        force_control: None,
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
                                tree,
                                session,
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
                        pattern.lower(&port_tree, session, shape, &port_dimensions)?
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
                            &port_tree.functions,
                            &LValue::Ident(port.name.clone()),
                            &port_dimensions,
                        )
                        .ok_or_else(|| unsupported("interface array port width"))?
                        .width;
                        let actual_width = expr_static_width(tree, &actual, &dimensions)
                            .ok_or_else(|| unsupported("interface array connection width"))?;
                        let actual_shape = if let Some(root) =
                            connection_expression(authored, &port.name, Some(&interface))?
                        {
                            dimensions::operand_shape(root, tree, session, &dimensions)?
                        } else if let Expr::Ident(name) = &actual {
                            dimensions.get(name).cloned()
                        } else {
                            None
                        };
                        let unpacked_sizes =
                            |shape: &VariableDimensions,
                             functions: &const_functions::ConstantFunctions,
                             env: &ConstEnv| {
                                shape
                                    .unpacked
                                    .iter()
                                    .map(|dimension| {
                                        usize::try_from(eval_ast_const_expr(
                                            functions,
                                            &dimension.width,
                                            env,
                                        )?)
                                        .ok()
                                    })
                                    .collect::<Option<Vec<_>>>()
                            };
                        let actual_shape = actual_shape
                            .as_ref()
                            .map(|shape| {
                                unpacked_sizes(shape, &tree.functions, &dimensions.const_env)
                            })
                            .unwrap_or_else(|| Some(Vec::new()))
                            .ok_or_else(|| unsupported("interface array actual shape"))?;
                        let port_shape = unpacked_sizes(
                            port_shape,
                            &port_tree.functions,
                            &port_dimensions.const_env,
                        )
                        .ok_or_else(|| unsupported("interface array port shape"))?;
                        let (origin, ascending) =
                            connection_slice_origin(&actual, &tree.functions, &dimensions)?;
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
                    self.port_assignments.push(assignment);
                }
                let symbols = module.symbols.as_ref().unwrap();
                let names = binding_names(&module, symbols, &handle, &mut self.imported);
                if let Some(item) = &generated_item {
                    let scope = self
                        .imported
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
                self.imported
                    .initial_processes
                    .extend(bound.initial_processes.iter().cloned());
                self.imported.extend(&bound);
            }
        }
        Ok(())
    }
}
