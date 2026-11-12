//! The constants of a scope: its parameters, types and enumeration members.

use super::*;

impl ScopeElaboration<'_> {
    /// The parameters, constants, types, enumeration members and array parameters
    /// of the scope. Returns the context that calls its constant functions, which
    /// the later phases bind in.
    pub(super) fn constants<'t>(
        &mut self,
        syntax_tree: &AnalysisContext<'t>,
        session: &mut Session,
    ) -> Result<AnalysisContext<'t>, AnalyzerError> {
        let with_imported = |mut functions: const_functions::ConstantFunctions| {
            functions.extend_missing(&self.imported.constant_functions);
            functions
        };
        // The scope's tables are shared with the contexts that bind in it.
        self.type_aliases = type_aliases_from_scope(syntax_tree, session)?.into();
        // Constant functions see the module constants known so far, by
        // their values and declaration types; they are collected again once
        // the parameters are known.
        let preliminary =
            syntax_tree.with_functions(with_imported(const_functions::module_constant_functions(
                syntax_tree,
                session,
                &self.imported.const_env,
                &self.type_aliases,
                &self.constant_types(),
                &self.imported.parameter_values,
            )));
        let syntax_tree = &preliminary;
        self.parameters = parameters_from_scope(
            syntax_tree,
            session,
            &self.type_aliases,
            &self.imported.const_env,
            self.applicable_parameter_overrides,
        )?;
        let mut parameter_names = HashSet::default();
        if let Some(parameter) = self
            .parameters
            .iter()
            .find(|parameter| !parameter_names.insert(parameter.name()))
        {
            return Err(AnalyzerError::DuplicateParameter {
                module: self.name.clone(),
                name: parameter.name().to_string(),
            });
        }
        if self.name == self.override_module_name {
            apply_parameter_overrides(&mut self.parameters, self.parameter_overrides)?;
        }
        self.const_env = self.imported.const_env.clone().into();
        self.const_env.extend(const_env_from_parameters(
            syntax_tree,
            session,
            &self.parameters,
        ));
        self.type_aliases =
            type_aliases_from_scope_with_env(syntax_tree, session, &self.const_env)?.into();
        self.enum_constants = enum_member_constants_from_scope(
            syntax_tree,
            session,
            &self.const_env,
            &self.type_aliases,
            self.applicable_parameter_overrides,
        )?;
        for (name, value) in &self.enum_constants.numbers {
            self.const_env.entry(name.clone()).or_insert(*value);
            self.const_env.insert(enum_marker(name), *value);
            if let Some(r#type) = self.enum_constants.types.get(name) {
                insert_parameter_type_markers(&mut self.const_env, name, *r#type);
            }
        }
        self.enum_constants
            .extend_missing(&self.imported.enum_constants);
        // Alias ranges may themselves contain casts sized by enum members.
        // Rebuild them before assigning declared parameter widths.
        self.type_aliases =
            type_aliases_from_scope_with_env(syntax_tree, session, &self.const_env)?.into();
        // Constant casts are evaluated while parameter syntax is lowered.
        // Repeat that lowering after enum constants become available so a
        // cast operand such as `byte_t'(ENUM_MEMBER)` is not permanently
        // discarded during the initial pass. Materialize only enum-member
        // references so exported parameter expressions retain dependencies
        // on other parameters for later specialization.
        self.parameters = parameters_from_scope(
            syntax_tree,
            session,
            &self.type_aliases,
            &self.const_env,
            self.applicable_parameter_overrides,
        )?;
        for parameter in &mut self.parameters {
            parameter.value = parameter.value.take().map(|value| {
                substitute_typed_parameter_literals(
                    value,
                    &self.enum_constants.numbers,
                    &self.enum_constants.types,
                )
            });
        }
        if self.name == self.override_module_name {
            apply_parameter_overrides(&mut self.parameters, self.parameter_overrides)?;
        }
        extend_const_env_with_parameters(
            syntax_tree,
            session,
            &mut self.const_env,
            &self.parameters,
        );
        self.type_aliases =
            type_aliases_from_scope_with_env(syntax_tree, session, &self.const_env)?.into();
        // Contract dependency analysis must see module parameter references
        // before the normal procedural lowering substitutes default literals.
        let symbolic_parameters = self
            .parameters
            .iter()
            .map(|parameter| parameter.name().to_string())
            .collect();
        // Class parameter defaults are bound where the class is declared.
        classes::specializations::declare_scopes(
            syntax_tree,
            session,
            &self.const_env,
            &self.type_aliases,
        );
        self.declaration_subroutines =
            with_imported(const_functions::symbolic_module_constant_functions(
                syntax_tree,
                session,
                &self.const_env,
                &self.type_aliases,
                &self.constant_types(),
                &self.imported.parameter_values,
                &symbolic_parameters,
                // The declarations describe the functions that the
                // types of a module's header may call, which a package
                // or compilation unit has none of.
                if self.is_package {
                    procedural::ClassMethods::None
                } else {
                    procedural::ClassMethods::StaticFunctions
                },
            ))
            .declarations();
        let parameter_values = parameter_value_env_with_imported(
            syntax_tree,
            session,
            &self.parameters,
            &self.const_env,
            self.imported,
        );
        let parameterized =
            syntax_tree.with_functions(with_imported(const_functions::module_constant_functions(
                syntax_tree,
                session,
                &self.const_env,
                &self.type_aliases,
                &self.constant_types(),
                &parameter_values,
            )));
        let syntax_tree = &parameterized;
        extend_const_env_with_parameters(
            syntax_tree,
            session,
            &mut self.const_env,
            &self.parameters,
        );

        self.array_parameters = array_parameters::array_parameters_from_scope(
            syntax_tree,
            session,
            &self.const_env,
            &self.type_aliases,
            &self.parameters,
        )?;
        for parameter in &self.array_parameters {
            self.const_env.insert(
                parameters::unbounded_parameter_marker(&parameter.signal.name),
                0,
            );
        }
        if self.array_parameters.is_empty() {
            return Ok(parameterized);
        }
        // A constant function may read an unpacked array parameter of the
        // scope, a variable there, by its shape (IEEE 1800-2023 13.4.3).
        let variables = self
            .array_parameters
            .iter()
            .map(|parameter| {
                (
                    parameter.signal.name().to_string(),
                    procedural::dimensions_from_type(parameter.signal.r#type()),
                )
            })
            .collect();
        let functions = with_imported(const_functions::module_constant_functions_with_variables(
            syntax_tree,
            session,
            &self.const_env,
            &self.type_aliases,
            &self.constant_types(),
            &parameter_values,
            &variables,
        ));
        Ok(syntax_tree.with_functions(functions))
    }

    /// Rejects the constructs the analysis would silently ignore. A parameter
    /// initializer that asks for the type of a port or signal is evaluated again
    /// with those declarations, and so are the constants that depend on it.
    pub(super) fn reject_ignored_constructs(
        &mut self,
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
    ) -> Result<(), AnalyzerError> {
        if !self.imported.declarations_only {
            let parameter_values = parameter_value_env_with_imported(
                syntax_tree,
                session,
                &self.parameters,
                &self.const_env,
                self.imported,
            );
            match reject_silently_ignored_constructs(
                syntax_tree,
                session,
                &self.const_env,
                &self.type_aliases,
                &parameter_packed_dimensions_with_imported(&self.parameters, self.imported).into(),
                &parameter_values,
            ) {
                Ok(()) => {}
                // A parameter initializer may inspect a port or signal type
                // through `$bits`/`$size`. Collect declarations only when that
                // missing type metadata is the remaining validation failure, so
                // unsupported constructs keep their original diagnostics.
                Err(AnalyzerError::Unsupported(construct))
                    if construct == "constant cast expression" =>
                {
                    let preliminary_ports = ports_from_scope(
                        syntax_tree,
                        session,
                        &self.const_env,
                        &self.type_aliases,
                        self.header_ports,
                    )?;
                    let preliminary_signals = signals_from_scope(
                        syntax_tree,
                        session,
                        &self.const_env,
                        &self.type_aliases,
                    )?;
                    extend_const_env_with_variable_types(
                        &syntax_tree.functions,
                        &mut self.const_env,
                        preliminary_ports
                            .iter()
                            .map(|port| (port.name(), port.r#type()))
                            .chain(
                                preliminary_signals
                                    .iter()
                                    .map(|signal| (signal.name(), signal.r#type())),
                            ),
                    );
                    self.parameters = parameters_from_scope(
                        syntax_tree,
                        session,
                        &self.type_aliases,
                        &self.const_env,
                        self.applicable_parameter_overrides,
                    )?;
                    for parameter in &mut self.parameters {
                        parameter.value = parameter.value.take().map(|value| {
                            substitute_typed_parameter_literals(
                                value,
                                &self.enum_constants.numbers,
                                &self.enum_constants.types,
                            )
                        });
                    }
                    if self.name == self.override_module_name {
                        apply_parameter_overrides(&mut self.parameters, self.parameter_overrides)?;
                    }
                    extend_const_env_with_parameters(
                        syntax_tree,
                        session,
                        &mut self.const_env,
                        &self.parameters,
                    );
                    self.type_aliases =
                        type_aliases_from_scope_with_env(syntax_tree, session, &self.const_env)?
                            .into();
                    self.array_parameters = array_parameters::array_parameters_from_scope(
                        syntax_tree,
                        session,
                        &self.const_env,
                        &self.type_aliases,
                        &self.parameters,
                    )?;
                    let parameter_values = parameter_value_env_with_imported(
                        syntax_tree,
                        session,
                        &self.parameters,
                        &self.const_env,
                        self.imported,
                    );
                    reject_silently_ignored_constructs(
                        syntax_tree,
                        session,
                        &self.const_env,
                        &self.type_aliases,
                        &parameter_packed_dimensions_with_imported(&self.parameters, self.imported)
                            .into(),
                        &parameter_values,
                    )?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}
