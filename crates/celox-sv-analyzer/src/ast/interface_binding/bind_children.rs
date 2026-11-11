//! Interface ports of child instances, and the module the bindings give.

use super::*;

impl ModuleBinding<'_> {
    /// Binds the interface ports of the child instances the plan of the module
    /// names, to the interface instances the module connects to them.
    pub(super) fn bind_children(
        &mut self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
    ) -> Converted<()> {
        if let Some(plan) = self.plan {
            let mut current = self.caller_symbols.clone();
            current.extend(&self.imported);
            let tree = &tree.importing(Arc::new(current.clone()));
            let tree = &tree.with_functions(current.constant_functions.clone());
            for item in generate::items(tree, session, &current.const_env, &current.type_aliases)? {
                let tree = &item.scoped(tree);
                let active_scope = scope::imported(tree);
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
                    let child = &self.packages.interface_plan.modules[alias];
                    let mut connections = HashMap::default();
                    let mut parameters = Vec::new();
                    for port in &child.ports {
                        let actual = connection_expression(
                            authored,
                            &port.name,
                            self.interfaces.get(&original),
                        )?;
                        let env = &item.env;
                        let actuals = connection_array(
                            actual,
                            &port.name,
                            tree,
                            session,
                            env,
                            &active_scope,
                        )?;
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
                                unsupported(format!(
                                    "interface type parameter `{handle}.{parameter}`"
                                ))
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
                                .and_then(|expr| expr_to_const(session, expr))
                                .ok_or_else(|| {
                                    unsupported(format!(
                                        "interface parameter `{handle}.{parameter}`"
                                    ))
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
                        self.imported
                            .interface_instances
                            .insert((item.id, index), binding);
                    } else {
                        self.imported
                            .interface_scopes
                            .entry(item.scope_name().to_string())
                            .or_default()
                            .instances
                            .insert((item.id, index), binding);
                    }
                }
            }
        }
        Ok(())
    }

    /// The module, analyzed with the symbols its interfaces were bound to.
    pub(super) fn finish(
        mut self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
    ) -> Converted<Option<Module>> {
        for assignment in self.port_assignments {
            self.imported.comb_processes.push(CombProcess::new(
                CombProcessKind::ContinuousAssign,
                None,
                vec![assignment],
            ));
        }
        // Check authored declarations before module collection can prefer a local
        // function/type/constant over an imported interface binding of the same name.
        let authored = imports::scope_names(tree)?;
        for (path, bound) in &self.imported.interface_members {
            if authored.contains(bound) && !self.imported.local_aliases.contains(path) {
                return Err(unsupported(format!(
                    "interface binding collides with declaration `{bound}`"
                )));
            }
        }
        let module = Module::from_selected_scope(
            tree,
            session,
            &self.name,
            &self.overrides,
            self.interfaces,
            Arc::new(self.imported),
            self.bindings.export_symbols,
            Some(&self.view_ports),
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
}
