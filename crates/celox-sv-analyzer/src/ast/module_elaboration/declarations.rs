//! The declarations of a scope: ports, signals, subroutine arguments, instances and functions.

use super::*;

impl ScopeElaboration<'_> {
    /// The ports and signals of the scope, with those of the packages it uses,
    /// and the static properties of its class specializations.
    pub(super) fn ports_and_signals(
        &mut self,
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
    ) -> Result<(), AnalyzerError> {
        self.ports = ports_from_scope(
            syntax_tree,
            session,
            &self.const_env,
            &self.type_aliases,
            self.header_ports,
        )?;
        if let Some(port) = self.ports.iter().find(|port| port.r#type.holds_chandle()) {
            return Err(AnalyzerError::IllegalChandleUse {
                detail: format!("port `{}` of chandle type", port.name),
            });
        }
        self.ports.extend(self.imported.ports.iter().cloned());
        let mut port_names = HashSet::default();
        if let Some(port) = self
            .ports
            .iter()
            .find(|port| !port_names.insert(port.name()))
        {
            return Err(AnalyzerError::DuplicatePort {
                module: self.name.clone(),
                name: port.name().to_string(),
            });
        }
        if self
            .ports
            .iter()
            .any(|port| port.direction() == PortDirection::Ref)
        {
            return Err(AnalyzerError::Unsupported("ref port direction".to_string()));
        }
        // The static properties of each class specialization are variables
        // of the module (IEEE 1800-2023 8.9, 8.25). A class declaration
        // that names no visible type is an error also when nothing uses it.
        if let Some(error) = classes::specializations::collect(
            syntax_tree,
            session,
            &PackedDimensions::new(HashMap::default(), &self.const_env, &self.type_aliases),
        ) {
            return Err(error);
        }
        self.signals =
            signals_from_scope(syntax_tree, session, &self.const_env, &self.type_aliases)?;
        self.signals.extend(
            self.array_parameters
                .iter()
                .map(|parameter| parameter.signal.clone()),
        );
        // The array parameters of the packages the module uses are constant
        // signals of the module, like its own.
        self.signals.extend(self.imported.signals.iter().cloned());
        self.signals
            .extend(self.imported.state_signals.iter().cloned());
        for r#type in self
            .ports
            .iter()
            .map(Port::r#type)
            .chain(self.signals.iter().map(Signal::r#type))
        {
            validate_unpacked_dimension_sizes(
                &syntax_tree.functions,
                r#type.unpacked_ranges(),
                &self.const_env,
            )?;
        }
        let signal_names: HashSet<_> = self.signals.iter().map(Signal::name).collect();
        if let Some(parameter) = self.parameters.iter().find(|parameter| {
            port_names.contains(parameter.name()) || signal_names.contains(parameter.name())
        }) {
            return Err(AnalyzerError::Unsupported(format!(
                "parameter name collides with port or signal `{}`",
                parameter.name()
            )));
        }
        Ok(())
    }

    /// The shapes of the ports and signals, the types of the constants and the
    /// values and dimensions of the parameters, as expressions of the scope see
    /// them.
    pub(super) fn declared_shapes(
        &mut self,
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
    ) {
        self.packed_dimensions = packed_dimensions_from_ports_and_signals(
            &self.ports,
            &self.signals,
            &self.const_env,
            &self.type_aliases,
        );
        self.packed_dimensions.constant_types = self.constant_types();
        // Four-state parameter values cannot be represented by the numeric
        // environment. Keep their expressions for constant case analysis.
        self.packed_dimensions.parameter_values = parameter_value_env_with_imported(
            syntax_tree,
            session,
            &self.parameters,
            &self.const_env,
            self.imported,
        )
        .into();
        self.packed_dimensions
            .parameter_values
            .retain(|name, _| !self.const_env.contains_key(name));
        self.packed_dimensions
            .extend(parameter_packed_dimensions_with_imported(
                &self.parameters,
                self.imported,
            ));
    }

    /// The declaration types of the constants the scope sees: the enum
    /// constants and parameters of the packages it uses and its own, as far
    /// as they are known; a parameter has no binding until its type is.
    pub(super) fn constant_types(&self) -> Arc<HashMap<String, semantic_types::ConstantType>> {
        let mut bindings = self.imported.constant_types.clone();
        bindings.extend(self.enum_constants.declarations.clone());
        for parameter in self.imported.parameters.iter().chain(&self.parameters) {
            bindings.remove(parameter.name());
            if let Some(ty) = &parameter.semantic_type {
                bindings.insert(parameter.name().into(), ty.clone());
            }
        }
        Arc::new(bindings)
    }

    /// The argument names, shapes and directions of the subroutines the scope may
    /// call: its own, those of the packages it uses and its DPI imports.
    pub(super) fn subroutine_arguments(
        &mut self,
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
    ) -> Result<(), AnalyzerError> {
        // Instance connections may call subroutines, of the module or of the
        // packages it uses, with untyped assignment patterns.
        (
            self.subroutine_params,
            self.subroutine_shapes,
            self.subroutine_directions,
        ) = procedural::subroutine_argument_names(
            syntax_tree,
            session,
            &self.const_env,
            &self.type_aliases,
        )?;
        for (name, params) in &self.imported.subroutine_params {
            self.subroutine_params
                .entry(name.clone())
                .or_insert_with(|| params.clone());
        }
        for (name, shapes) in &self.imported.subroutine_shapes {
            self.subroutine_shapes
                .entry(name.clone())
                .or_insert_with(|| shapes.clone());
        }
        // The subroutines of the packages the module uses, by their qualified
        // names and by the names its imports give them.
        for subroutine in &self.imported.subroutines {
            self.subroutine_directions
                .entry(subroutine.name.clone())
                .or_insert_with(|| {
                    subroutine
                        .params
                        .iter()
                        .map(|param| param.direction)
                        .collect()
                });
        }
        for (name, target) in &self.imported.aliases {
            if let Some(directions) = self.subroutine_directions.get(target).cloned() {
                self.subroutine_directions
                    .entry(name.clone())
                    .or_insert(directions);
            }
        }
        self.dpi_items =
            generate::items(syntax_tree, session, &self.const_env, &self.type_aliases)?;
        self.dpi_imports = dpi::imports(&self.dpi_items, syntax_tree, session, &self.type_aliases)?;
        for import in &self.dpi_imports {
            // Shapes check the actuals as assignments to the formals.
            self.subroutine_shapes.insert(
                import.name().to_string(),
                import
                    .arguments()
                    .iter()
                    .filter_map(|argument| {
                        let r#type = match argument.r#type() {
                            crate::ir::DpiType::Bit => type_binding::builtin_type("bit", None),
                            crate::ir::DpiType::Logic => type_binding::builtin_type("logic", None),
                            crate::ir::DpiType::String => {
                                type_binding::builtin_type("string", None)
                            }
                            crate::ir::DpiType::Chandle => {
                                type_binding::builtin_type("chandle", None)
                            }
                            crate::ir::DpiType::BitVector { width, signed }
                            | crate::ir::DpiType::LogicVector { width, signed } => {
                                Some(Type::resolved(
                                    if argument.r#type().is_4state() {
                                        TypeKind::Logic
                                    } else {
                                        TypeKind::Bit
                                    },
                                    signed,
                                    None,
                                    &[(width as i128 - 1, 0)],
                                    &[],
                                ))
                            }
                            crate::ir::DpiType::Integer { width, signed } => {
                                type_binding::builtin_type(
                                    match width {
                                        8 => "byte",
                                        16 => "shortint",
                                        32 => "int",
                                        _ => "longint",
                                    },
                                    Some(signed),
                                )
                            }
                        }?;
                        Some(procedural::dimensions_from_type(&r#type))
                    })
                    .collect(),
            );
            self.subroutine_params.insert(
                import.name().to_string(),
                import
                    .arguments()
                    .iter()
                    .map(|arg| arg.name().to_string())
                    .collect(),
            );
            self.subroutine_directions.insert(
                import.name().to_string(),
                import
                    .arguments()
                    .iter()
                    .map(|argument| dpi_direction(argument.direction()))
                    .collect(),
            );
        }
        self.packed_dimensions.subroutine_param_shapes = Arc::new(self.subroutine_shapes.clone());
        self.packed_dimensions.subroutine_directions = Arc::new(self.subroutine_directions.clone());
        self.packed_dimensions.subroutine_param_names = Arc::new(self.subroutine_params.clone());
        Ok(())
    }

    /// The values of the array parameters. Returns the processes that initialize
    /// them and the values of those that are packed.
    pub(super) fn array_parameter_values(
        &mut self,
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
    ) -> Result<(Vec<InitialProcess>, HashMap<String, Expr>), AnalyzerError> {
        // Array parameters retain a constant value as well as their signal layout.
        // Bind their values in declaration order before child parameter arguments.
        for process in &self.imported.initial_processes {
            for statement in &process.body {
                if let Stmt::Assign {
                    lhs: LValue::Ident(name),
                    rhs,
                    ..
                } = statement
                {
                    self.packed_dimensions
                        .array_parameter_values
                        .insert(name.clone(), rhs.clone());
                }
            }
        }
        let mut array_initial_processes = Vec::new();
        let mut packed_pattern_values = HashMap::default();
        for parameter in &self.array_parameters {
            let mut process =
                parameter.initial_process(syntax_tree, session, &self.packed_dimensions)?;
            // The value may name constants, such as enumeration members
            // (IEEE 1800-2023 6.19), which every other process is given as
            // values the same way.
            for stmt in &mut process.body {
                procedural::substitute_stmt_constants(
                    syntax_tree,
                    session,
                    stmt,
                    &self.const_env,
                    &self.enum_constants.exprs,
                );
            }
            if let Stmt::Assign { rhs, .. } = &process.body[0] {
                self.packed_dimensions
                    .array_parameter_values
                    .insert(parameter.signal.name.clone(), rhs.clone());
                let name = parameter.signal.name();
                let shape = self
                    .packed_dimensions
                    .get(name)
                    .expect("array parameter shape");
                if shape.unpacked.is_empty()
                    && !shape.real
                    && !shape.string
                    && shape.class.is_none()
                {
                    // An integral value parameter is self-determined before
                    // conversion at a receiving declaration (6.20.2). The
                    // pattern's concatenation must retain its declared sign.
                    let signed = shape.signed;
                    let is_2state = shape.is_2state;
                    let width = expr_static_width(
                        syntax_tree,
                        &Expr::Ident(name.to_string()),
                        &self.packed_dimensions,
                    )
                    .ok_or_else(|| unsupported("packed pattern parameter width"))?;
                    let mut value = Expr::Resize {
                        expr: Box::new(rhs.clone()),
                        width,
                        signed,
                    };
                    if is_2state {
                        value = Expr::Unary {
                            op: UnaryOp::ToTwoState,
                            expr: Box::new(value),
                        };
                    }
                    let value = fold_const_integral_expr_preserving_mask(
                        syntax_tree,
                        session,
                        value,
                        &self.packed_dimensions.const_env,
                    );
                    self.packed_dimensions
                        .parameter_values
                        .insert(name.to_string(), value.clone());
                    packed_pattern_values.insert(name.to_string(), value);
                }
            }
            array_initial_processes.push(process);
        }
        Ok((array_initial_processes, packed_pattern_values))
    }

    /// The instances of the scope, whose names and generate scopes must not
    /// collide with its other declarations.
    pub(super) fn instances(
        &mut self,
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
    ) -> Result<(), AnalyzerError> {
        self.instances = instances_from_scope(
            syntax_tree,
            session,
            &self.const_env,
            &self.packed_dimensions,
            self.interfaces,
        )?;
        let mut instance_names = HashSet::default();
        if let Some(instance) = self
            .instances
            .iter()
            .filter(|instance| {
                instance
                    .condition()
                    .and_then(|condition| {
                        eval_ast_const_expr(&syntax_tree.functions, condition, &self.const_env)
                    })
                    .is_none_or(|value| value != 0)
            })
            .find(|instance| !instance_names.insert(instance.name()))
        {
            return Err(AnalyzerError::DuplicateInstance {
                module: self.name.clone(),
                name: instance.name().to_string(),
            });
        }
        // A generate block shares its name space with the module's other
        // declarations (IEEE 1800-2023 27.6, 23.9).
        // The members of a variable of unpacked structure type are named
        // like declarations of a scope of the variable's name.
        let aggregates: HashSet<&str> = self
            .signals
            .iter()
            .filter(|signal| signal.is_aggregate())
            .map(Signal::name)
            .collect();
        let scope_heads: HashSet<&str> = self
            .signals
            .iter()
            .filter(|signal| signal.hierarchical.is_none())
            .map(Signal::name)
            .chain(self.instances.iter().map(Instance::name))
            .filter_map(generate_scope_head)
            .filter(|head| !aggregates.contains(head))
            .collect();
        if let Some(conflict) = self
            .ports
            .iter()
            .map(Port::name)
            .chain(self.signals.iter().map(Signal::name))
            .chain(self.instances.iter().map(Instance::name))
            .find(|conflict| scope_heads.contains(conflict))
        {
            return Err(AnalyzerError::DuplicateGenerateScope {
                module: self.name.clone(),
                name: conflict.to_string(),
            });
        }
        Ok(())
    }

    /// The parameter values and functions that bodies see, and the return types
    /// of the functions, class methods and DPI imports. Expands the function calls
    /// of instance connections and completes the types of the scope.
    pub(super) fn functions(
        &mut self,
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
        packed_pattern_values: HashMap<String, Expr>,
    ) -> Result<(), AnalyzerError> {
        self.parameter_values = parameter_value_env_with_imported(
            syntax_tree,
            session,
            &self.parameters,
            &self.const_env,
            self.imported,
        );
        self.parameter_values.extend(packed_pattern_values);
        for (name, value) in &self.enum_constants.exprs {
            self.parameter_values
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }
        let mut expression_signedness = self
            .ports
            .iter()
            .map(|port| (port.name().to_string(), port.r#type().is_signed()))
            .chain(
                self.signals
                    .iter()
                    .map(|signal| (signal.name().to_string(), signal.r#type().is_signed())),
            )
            .collect::<HashMap<_, _>>();
        expression_signedness.extend(
            parameter_types_from_const_env(&self.const_env)
                .into_iter()
                .map(|(name, r#type)| (name, r#type.signed)),
        );
        self.functions = functions_from_scope(
            syntax_tree,
            session,
            &self.const_env,
            &self.packed_dimensions,
        )?;
        for (name, function) in &self.imported.functions {
            self.functions
                .entry(name.clone())
                .or_insert_with(|| function.clone());
        }
        for (name, metadata) in &self.imported.function_return_types {
            self.packed_dimensions
                .function_return_types
                .entry(name.clone())
                .or_insert_with(|| metadata.clone());
        }
        self.packed_dimensions
            .function_return_types
            .extend(self.functions.iter().map(|(name, function)| {
                (
                    name.clone(),
                    FunctionReturnMetadata::from_function(function),
                )
            }));
        self.packed_dimensions
            .function_return_types
            .extend(self.dpi_imports.iter().filter_map(|import| {
                FunctionReturnMetadata::from_dpi(import, &mut session.types)
                    .map(|metadata| (import.name().to_string(), metadata))
            }));
        // Class methods are subroutines `class::method` (IEEE 1800-2023 8.6).
        dimensions::all_class_signatures(syntax_tree, session, &mut self.packed_dimensions);
        self.packed_dimensions.functions = Arc::new(self.functions.clone());
        self.packed_dimensions.expression_signedness = expression_signedness.clone().into();
        for instance in &mut self.instances {
            for connection in &mut instance.port_connections {
                if let Some(pattern) = &mut connection.bound_pattern {
                    pattern.resolve_types(&self.packed_dimensions)?;
                }
                connection.map_expressions(|expr| {
                    let expr =
                        expand_expr_calls(expr, &self.functions, &expression_signedness, 0, true);
                    substitute_expr_constants_with_parameter_literals(
                        syntax_tree,
                        session,
                        expr,
                        &self.const_env,
                        &self.enum_constants.exprs,
                    )
                });
            }
        }
        // Bodies now have the active module's declarations and function types.
        // Declaration-time queries still use syntax discovery while metadata
        // is incomplete; generated/procedural scopes overlay this complete base.
        self.packed_dimensions
            .bind_types(&mut session.types, &syntax_tree.functions);
        self.packed_dimensions.scope_types_complete = true;
        // Every type visible in the scope, so that a use of a name no type
        // has is an error (IEEE 1800-2023 6.18).
        self.packed_dimensions.type_declarations =
            Some(types::scope_type_declarations(syntax_tree)?);
        Ok(())
    }
}
