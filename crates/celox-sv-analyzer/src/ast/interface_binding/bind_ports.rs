//! Interface ports of a module, bound to the members of the interfaces they receive.

use super::*;

impl ModuleBinding<'_> {
    /// The declarations of the module that the dimensions of its interface array
    /// ports may name, if it has such ports.
    pub(super) fn array_scope(
        &self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
    ) -> Converted<Option<Module>> {
        let array_scope = if self.interface_ports.iter().any(|port| port.array_rank != 0) {
            let (header_ports, header_tree) = binding_declaration_view(
                &self.view_ports,
                &self.instantiations,
                self.interface_ports,
                tree,
                &self.imported,
            )?;
            let mut header_imports = self.imported.clone();
            header_imports.declarations_only = true;
            Some(Module::from_selected_scope(
                &tree.child(&header_tree, &self.overrides),
                session,
                &self.name,
                &self.overrides,
                self.interfaces,
                Arc::new(header_imports),
                true,
                Some(&header_ports),
            )?)
        } else {
            None
        };
        Ok(array_scope)
    }

    /// Binds the interface ports of the module: the members of each interface a
    /// port receives become symbols the module imports.
    pub(super) fn bind_ports(
        &mut self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        array_scope: &Option<Module>,
    ) -> Converted<()> {
        for port in self.interface_ports {
            let (definition_tree, definition_path) = self
                .definitions
                .get(&port.interface)
                .ok_or_else(|| unsupported(format!("interface `{}`", port.interface)))?;
            let prefix = format!("{}$", port.name);
            let values = self
                .supplied
                .iter()
                .filter_map(|(name, value)| {
                    name.strip_prefix(&prefix)
                        .filter(|name| !name.starts_with('$'))
                        .map(|name| (name.to_string(), value.clone()))
                })
                .collect::<HashMap<_, _>>();
            let types = self
                .bindings
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
                .unwrap_or(&self.imported);
            let array = instance_elements(
                &port.name,
                &dimensions,
                tree,
                session,
                &array_symbols.const_env,
                &array_symbols.type_aliases,
            )?;
            for (dimension, size) in array.shape.iter().enumerate() {
                if let Some(actual) = self
                    .supplied
                    .get(&format!("{}$$dimension{dimension}", port.name))
                    && eval_ast_const_expr(&tree.functions, actual, &array_symbols.const_env)
                        != Some(*size as i128)
                {
                    return Err(unsupported(format!(
                        "interface array dimension mismatch at `{name}.{}`",
                        port.name,
                        name = self.name
                    )));
                }
            }
            if !dimensions.is_empty() {
                self.imported
                    .interface_arrays
                    .insert(port.name.clone(), array.clone());
            }
            let arguments =
                virtual_interfaces::instance_arguments(session, &port.interface, &values)?;
            for (ordinal, handle) in array.elements.iter().enumerate() {
                self.imported
                    .interface_types
                    .insert(handle.clone(), port.interface.clone());
                if !arguments.is_empty() {
                    self.imported
                        .interface_arguments
                        .insert(handle.clone(), arguments.clone());
                }
                if let Some(modport) = &port.modport {
                    self.imported
                        .interface_modports
                        .insert(handle.clone(), modport.clone());
                }
                self.imported
                    .state_signals
                    .extend(virtual_interfaces::port_identity_signal(
                        session,
                        handle,
                        &port.interface,
                        &arguments,
                    ));
                let symbol_prefix = if dimensions.is_empty() {
                    port.name.clone()
                } else {
                    format!("{}${ordinal}", port.name)
                };
                let mut interface = {
                    let tree = &AnalysisContext {
                        // A port's body runs in the instance it denotes,
                        // which the homes of its sites name (see below),
                        // not this scope.
                        interface_instance: None,
                        instance_scope: None,
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
                        &[],
                    )
                    .map_err(|error| interface_port_reference(error, &port.name))
                }?;
                // Header ports are members of the interface view, just like body signals.
                for port in &interface.ports {
                    interface.symbols.as_mut().unwrap().signals.push(Signal {
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
                    });
                }
                let symbols = interface.symbols.as_ref().unwrap();
                let names = binding_names_with_prefix(
                    &interface,
                    symbols,
                    handle,
                    &symbol_prefix,
                    &mut self.imported,
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
                // An instance of a covergroup type of the interface with a
                // clocking event joins the samplers of the interface
                // instance, which a port view does not hold: a function the
                // module calls through the port may not construct one.
                if bound
                    .subroutines
                    .iter()
                    .any(|function| covergroups::is_instance_subroutine(&function.name))
                {
                    if bound.subroutines.iter().any(|function| {
                        let mut calls = Vec::new();
                        module_elaboration::subroutine_calls(function, &mut calls);
                        functions.contains(&function.name)
                            && calls.into_iter().any(covergroups::is_spawner)
                    }) {
                        return Err(covergroups::spawned_through_a_port());
                    }
                    // Nor does it hold the hidden variables of the samplers.
                    bound
                        .signals
                        .retain(|signal| !covergroups::is_hidden_variable(&signal.name));
                }
                // The subroutines the port imports run in the instance it
                // denotes, whose scope their `%m` names.
                let home = celox_design::SiteHome::Path {
                    up: Some(0),
                    down: Vec::new(),
                    within: Some(handle.to_string()),
                };
                for subroutine in &mut bound.subroutines {
                    let subroutine = Arc::make_mut(subroutine);
                    crate::procedural::set_site_home(&mut subroutine.body, &home);
                    crate::procedural::set_site_home(&mut subroutine.sensitivity_body, &home);
                }
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
                // actual instance, not to each module receiving its port view,
                // as do the samplers of its covergroups.
                bound.initial_processes.retain(|process| {
                    process.synthetic
                        && !process
                            .site
                            .as_ref()
                            .is_some_and(|site| site.construct == covergroups::SAMPLING)
                        && procedural::written_names(&process.body).is_disjoint(&members)
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
                        .with_unresolved_net(signal.unresolved_net)
                        .with_net_resolution(signal.net_resolution),
                    );
                }
                bound
                    .signals
                    .retain(|signal| !members.contains(&signal.name));
                covergroups::record_outside_names(session, &tree.functions, &bound);
                self.imported.extend(&bound);
            }
        }
        Ok(())
    }

    /// The declarations of the module itself, which the overrides of its interface
    /// instances may name. Returns the module they were collected as.
    pub(super) fn caller(
        &mut self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
    ) -> Converted<Module> {
        // Resolve the caller's declarations before evaluating instance overrides.
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
        self.caller_symbols = caller.symbols.as_ref().unwrap().clone();
        Ok(caller)
    }
}

/// A hierarchical name of the body of an interface that interface port
/// `port` holds denotes an object relative to where the instance is, not to
/// the module the port is of (IEEE 1800-2023 23.8): that module cannot
/// resolve it, so it is rejected rather than looked up there.
fn interface_port_reference(error: AnalyzerError, port: &str) -> AnalyzerError {
    match error {
        AnalyzerError::HierarchicalReference { path, .. } => unsupported(format!(
            "hierarchical name `{path}` in the body of an interface that interface port `{port}` connects"
        )),
        AnalyzerError::WithWarnings { error, warnings } => AnalyzerError::WithWarnings {
            error: Box::new(interface_port_reference(*error, port)),
            warnings,
        },
        error => error,
    }
}
