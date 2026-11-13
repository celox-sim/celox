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
            for (ordinal, handle) in array.elements.iter().enumerate() {
                self.imported
                    .interface_types
                    .insert(handle.clone(), port.interface.clone());
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
                    ));
                let symbol_prefix = if dimensions.is_empty() {
                    port.name.clone()
                } else {
                    format!("{}${ordinal}", port.name)
                };
                let mut interface = {
                    let tree = &AnalysisContext {
                        interface_instance: Some(timescales::interface_instance(
                            tree.interface_instance.as_deref(),
                            handle,
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
                    process.synthetic
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
