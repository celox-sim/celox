//! The symbols a scope exports and the module it elaborates to.

use super::*;

impl ScopeElaboration<'_> {
    /// The symbols of a package, or of a scope that exports them, for the scopes
    /// that use it.
    pub(super) fn symbols(
        &mut self,
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
        inline_randomizations: Vec<classes::constraints::InlineRequest>,
    ) -> Option<scope::ScopeSymbols> {
        if self.is_package || self.export_symbols {
            Arc::make_mut(&mut self.packed_dimensions.constant_types).extend(
                classes::specializations::export_constant_types(syntax_tree, session),
            );
        }
        // A package keeps its symbols for the scopes that use it.
        (self.is_package || self.export_symbols).then(|| scope::ScopeSymbols {
            unit: None,
            package_scopes: self.imported.package_scopes.clone(),
            classes: classes::scope_classes(syntax_tree, session, &self.const_env),
            class_member_types: classes::specializations::export_member_types(syntax_tree, session),
            inline_randomizations,
            declarations_only: self.imported.declarations_only,
            type_parameter_bindings: HashMap::default(),
            pattern_parameter_bindings: HashMap::default(),
            const_env: (*self.const_env).clone(),
            parameter_values: self.parameter_values.clone(),
            parameters: self.parameters.clone(),
            // A class of the scope is a type of it (IEEE 1800-2023 8.3).
            type_aliases: self
                .type_aliases
                .iter()
                .map(|(name, ty)| (name.clone(), ty.clone()))
                .chain(classes::scope_class_types(syntax_tree))
                .collect(),
            // The types the scope declares, also those that have no
            // binding, for its importers (IEEE 1800-2023 26.3). Those it
            // imports are the exporting packages' own, which an import
            // or export makes visible by name.
            type_declarations: types::declared_type_names(syntax_tree).unwrap_or_default(),
            enum_constants: self.enum_constants.clone(),
            constant_types: (*self.packed_dimensions.constant_types).clone(),
            functions: self.functions.clone(),
            function_return_types: (*self.packed_dimensions.function_return_types).clone(),
            constant_functions: syntax_tree.functions.clone(),
            subroutine_params: self.subroutine_params.clone(),
            subroutine_shapes: self.subroutine_shapes.clone(),
            subroutines: self.subroutines.iter().cloned().map(Arc::new).collect(),
            locals: self.locals.clone(),
            dpi_imports: self.dpi_imports.clone(),
            // A package's signals are its array parameters and constant
            // variables, initialized by its initial processes.
            signals: self.signals.clone(),
            ports: Vec::new(),
            state_signals: Vec::new(),
            initial_processes: self.initial_processes.clone(),
            comb_processes: self.comb_processes.clone(),
            ff_processes: self.ff_processes.clone(),
            aliases: self.imported.aliases.clone(),
            interface_members: self.imported.interface_members.clone(),
            local_aliases: self.imported.local_aliases.clone(),
            interface_types: self.imported.interface_types.clone(),
            interface_modports: self.imported.interface_modports.clone(),
            interface_arrays: self.imported.interface_arrays.clone(),
            interface_scopes: self.imported.interface_scopes.clone(),
            interface_declarations: self.imported.interface_declarations.clone(),
            interface_instances: self.imported.interface_instances.clone(),
            generate_imports: HashMap::default(),
        })
    }

    /// Adds the subroutines, locals and DPI imports of the packages the scope
    /// uses. Returns the package parameters it sees.
    pub(super) fn lower_imported(&mut self) -> Vec<Parameter> {
        // The subroutines of the packages the module uses that it can call,
        // and their locals and DPI imports, are lowered with it.
        {
            let mut known: HashSet<&str> = self
                .subroutines
                .iter()
                .map(|subroutine| subroutine.name.as_str())
                .collect();
            let mut imported: HashMap<&str, &Subroutine> = HashMap::default();
            for subroutine in &self.imported.subroutines {
                if !known.contains(subroutine.name.as_str()) {
                    imported
                        .entry(subroutine.name.as_str())
                        .or_insert(subroutine);
                }
            }
            let reachable = self.reachable_imports(&imported);
            let missing = self
                .imported
                .subroutines
                .iter()
                .filter(|subroutine| {
                    reachable.contains(subroutine.name.as_str())
                        && known.insert(subroutine.name.as_str())
                })
                .map(|subroutine| (**subroutine).clone())
                .collect::<Vec<_>>();
            self.subroutines.extend(missing);
            let mut known: HashSet<&str> = self
                .locals
                .iter()
                .map(|local| local.name.as_str())
                .collect();
            let missing = self
                .imported
                .locals
                .iter()
                .filter(|local| known.insert(local.name.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            self.locals.extend(missing);
        }
        for import in &self.imported.dpi_imports {
            if !self
                .dpi_imports
                .iter()
                .any(|known| known.name() == import.name())
            {
                self.dpi_imports.push(import.clone());
            }
        }
        let mut imported_parameters = self.imported.parameters.clone();
        imported_parameters.retain(|parameter| {
            !self
                .parameters
                .iter()
                .any(|local| local.name == parameter.name)
        });
        imported_parameters
    }

    /// Drops the bodies of the functions that declaration contracts cannot
    /// publish as one expression.
    pub(super) fn prune_symbolic_functions(&mut self, syntax_tree: &AnalysisContext<'_>) {
        // Expression construction tracks only whole-variable assignments.
        // Leave calls of functions with selected/concatenated writes symbolic
        // in declaration contracts rather than publishing a partial body.
        // Expression inlining also sizes formal and return conversions at the
        // default specialization; symbolic signature widths cannot be inlined.
        for subroutine in &self.declaration_subroutines {
            let mut selected_write = false;
            for statement in &subroutine.body {
                statement.walk(&mut |statement| {
                    selected_write |= matches!(
                        statement,
                        crate::procedural::StmtBase::Assign {
                            lhs: crate::ir::LValue::Select { .. },
                            ..
                        } | crate::procedural::StmtBase::AssignConcat { .. }
                    );
                });
            }
            let symbolic_type = subroutine
                .return_type
                .as_ref()
                .is_some_and(|ty| ty.has_symbolic_ranges(&syntax_tree.functions))
                || subroutine
                    .params
                    .iter()
                    .any(|param| param.r#type.has_symbolic_ranges(&syntax_tree.functions));
            if (selected_write || symbolic_type)
                && let Some(function) = self.functions.get_mut(&subroutine.name)
            {
                function.body = None;
            }
        }

        // A native default value is not evidence of a representable symbolic
        // body. Keep failed declaration lowerings visible to contract checking.
        for function in self.functions.values_mut() {
            if !self
                .declaration_subroutines
                .iter()
                .any(|subroutine| subroutine.name == function.name)
            {
                function.body = None;
            }
        }
    }

    /// The module the scope elaborated to.
    pub(super) fn finish(
        self,
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
        assignments: Vec<Assignment>,
        symbols: Option<scope::ScopeSymbols>,
        imported_parameters: Vec<Parameter>,
    ) -> Module {
        let mut interface_scope_names = self
            .imported
            .interface_types
            .keys()
            .chain(self.imported.interface_arrays.keys())
            .cloned()
            .collect::<Vec<_>>();
        interface_scope_names.sort_unstable();
        interface_scope_names.dedup();
        let interface_handles = virtual_interfaces::handles(session, self.imported, &self.ports);

        Module {
            name: self.name,
            program: self.program,
            interface_scope_names,
            interface_handles,
            parameters: self.parameters,
            ports: self.ports,
            signals: self.signals,
            instances: self.instances,
            assignments,
            comb_processes: self.comb_processes,
            ff_processes: self.ff_processes,
            initial_processes: self.initial_processes,
            locals: self.locals,
            subroutines: self.subroutines,
            dpi_imports: self.dpi_imports,
            constant_functions: syntax_tree.functions.clone(),
            symbolic_functions: self.functions,
            declaration_subroutines: self.declaration_subroutines,
            imported_parameters,
            symbols,
        }
    }
}
