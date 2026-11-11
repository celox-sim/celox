//! Elaboration of one module, package or compilation-unit scope.

use super::*;

impl Module {
    /// Analyze a module or a package. The scope starts from the `imported`
    /// symbols of the packages it uses.
    pub(super) fn from_scope_with_parameter_overrides(
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
        override_module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        interfaces: &InterfaceLookup<'_>,
        imported: Arc<scope::ScopeSymbols>,
    ) -> Result<Self, AnalyzerError> {
        Self::from_scope_with_symbols(
            syntax_tree,
            session,
            override_module_name,
            parameter_overrides,
            interfaces,
            imported,
            false,
        )
    }

    pub(super) fn from_scope_with_symbols(
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
        override_module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        interfaces: &InterfaceLookup<'_>,
        imported: Arc<scope::ScopeSymbols>,
        export_symbols: bool,
    ) -> Result<Self, AnalyzerError> {
        Self::from_selected_scope(
            syntax_tree,
            session,
            override_module_name,
            parameter_overrides,
            interfaces,
            imported,
            export_symbols,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn from_selected_scope(
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
        override_module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        interfaces: &InterfaceLookup<'_>,
        imported: Arc<scope::ScopeSymbols>,
        export_symbols: bool,
        header_ports: Option<&port_owners::Selection<'_>>,
    ) -> Result<Self, AnalyzerError> {
        for _ in 0..1024 {
            let revision = session.foreign.revision();
            let mut symbols = (*imported).clone();
            session.foreign.extend(syntax_tree, &mut symbols);
            let declarations_only = symbols.declarations_only;
            let export_symbols = export_symbols || session.foreign.has_templates();
            let result = Self::from_selected_scope_once(
                syntax_tree,
                session,
                override_module_name,
                parameter_overrides,
                interfaces,
                Arc::new(symbols),
                export_symbols,
                header_ports,
            );
            if let Ok(module) = &result
                && !declarations_only
                && let Some(symbols) = &module.symbols
            {
                classes::foreign::finish(syntax_tree, session, symbols)?;
            }
            if session.foreign.revision() == revision {
                return result;
            }
            session.classes.clear();
            session.class_specializations.clear();
            session.class_specialization_names.clear();
            session.foreign_error.take();
            session.foreign_names.clear();
        }
        Err(unsupported(
            "foreign class specialization discovery did not converge",
        ))
    }

    pub(super) fn from_selected_scope_once(
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
        override_module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        interfaces: &InterfaceLookup<'_>,
        imported: Arc<scope::ScopeSymbols>,
        export_symbols: bool,
        header_ports: Option<&port_owners::Selection<'_>>,
    ) -> Result<Self, AnalyzerError> {
        // The scope makes its class specializations until its class methods
        // have signatures.
        session.class_specializations_frozen = false;
        let unit = syntax_tree.scope_handle().unit(syntax_tree.declarations());
        let is_package = unit.is_none_or(|unit| unit.kind == celox_sv_syntax::UnitKind::Package);
        let program = unit.is_some_and(|unit| unit.kind == celox_sv_syntax::UnitKind::Program);
        let name = unit.map_or_else(
            || packages::UNIT.to_string(),
            |unit| instances::normalize_identifier(&unit.name.spelling),
        );
        let empty_parameter_overrides = HashMap::default();
        let applicable_parameter_overrides = if name == override_module_name {
            parameter_overrides
        } else {
            &empty_parameter_overrides
        };
        let specialization =
            syntax_tree.child(syntax_tree.syntax_tree, applicable_parameter_overrides);
        let specialization = specialization.importing(imported.clone());
        let syntax_tree = &specialization;
        // The collectors below each elaborate the same generate scopes.
        let previous = generate::memoize(syntax_tree, session);
        let module = (|session: &mut Session| -> Result<Self, AnalyzerError> {
            let with_imported = |mut functions: const_functions::ConstantFunctions| {
                functions.extend_missing(&imported.constant_functions);
                functions
            };
            // The scope's tables are shared with the contexts that bind in it.
            let mut type_aliases: SharedAliases =
                type_aliases_from_scope(syntax_tree, session)?.into();
            // Constant functions see the module constants known so far; they are
            // collected again once the parameters are known.
            let preliminary = syntax_tree.with_functions(with_imported(
                const_functions::module_constant_functions(
                    syntax_tree,
                    session,
                    &imported.const_env,
                    &type_aliases,
                    &imported.parameter_values,
                ),
            ));
            let syntax_tree = &preliminary;
            let mut parameters = parameters_from_scope(
                syntax_tree,
                session,
                &type_aliases,
                &imported.const_env,
                applicable_parameter_overrides,
            )?;
            let mut parameter_names = HashSet::default();
            if let Some(parameter) = parameters
                .iter()
                .find(|parameter| !parameter_names.insert(parameter.name()))
            {
                return Err(AnalyzerError::DuplicateParameter {
                    module: name,
                    name: parameter.name().to_string(),
                });
            }
            if name == override_module_name {
                apply_parameter_overrides(&mut parameters, parameter_overrides)?;
            }
            let mut const_env: SharedEnv = imported.const_env.clone().into();
            const_env.extend(const_env_from_parameters(syntax_tree, session, &parameters));
            type_aliases =
                type_aliases_from_scope_with_env(syntax_tree, session, &const_env)?.into();
            let mut enum_constants = enum_member_constants_from_scope(
                syntax_tree,
                session,
                &const_env,
                &type_aliases,
                applicable_parameter_overrides,
            )?;
            for (name, value) in &enum_constants.numbers {
                const_env.entry(name.clone()).or_insert(*value);
                const_env.insert(enum_marker(name), *value);
                if let Some(r#type) = enum_constants.types.get(name) {
                    insert_parameter_type_markers(&mut const_env, name, *r#type);
                }
            }
            enum_constants.extend_missing(&imported.enum_constants);
            // Alias ranges may themselves contain casts sized by enum members.
            // Rebuild them before assigning declared parameter widths.
            type_aliases =
                type_aliases_from_scope_with_env(syntax_tree, session, &const_env)?.into();
            // Constant casts are evaluated while parameter syntax is lowered.
            // Repeat that lowering after enum constants become available so a
            // cast operand such as `byte_t'(ENUM_MEMBER)` is not permanently
            // discarded during the initial pass. Materialize only enum-member
            // references so exported parameter expressions retain dependencies
            // on other parameters for later specialization.
            parameters = parameters_from_scope(
                syntax_tree,
                session,
                &type_aliases,
                &const_env,
                applicable_parameter_overrides,
            )?;
            for parameter in &mut parameters {
                parameter.value = parameter.value.take().map(|value| {
                    substitute_typed_parameter_literals(
                        value,
                        &enum_constants.numbers,
                        &enum_constants.types,
                    )
                });
            }
            if name == override_module_name {
                apply_parameter_overrides(&mut parameters, parameter_overrides)?;
            }
            extend_const_env_with_parameters(syntax_tree, session, &mut const_env, &parameters);
            type_aliases =
                type_aliases_from_scope_with_env(syntax_tree, session, &const_env)?.into();
            // Contract dependency analysis must see module parameter references
            // before the normal procedural lowering substitutes default literals.
            let symbolic_parameters = parameters
                .iter()
                .map(|parameter| parameter.name().to_string())
                .collect();
            // Class parameter defaults are bound where the class is declared.
            classes::specializations::declare_scopes(
                syntax_tree,
                session,
                &const_env,
                &type_aliases,
            );
            let declaration_subroutines =
                with_imported(const_functions::symbolic_module_constant_functions(
                    syntax_tree,
                    session,
                    &const_env,
                    &type_aliases,
                    &imported.parameter_values,
                    &symbolic_parameters,
                    // The declarations describe the functions that the
                    // types of a module's header may call, which a package
                    // or compilation unit has none of.
                    if is_package {
                        procedural::ClassMethods::None
                    } else {
                        procedural::ClassMethods::StaticFunctions
                    },
                ))
                .declarations();
            let parameter_values = parameter_value_env_with_imported(
                syntax_tree,
                session,
                &parameters,
                &const_env,
                &imported,
            );
            let parameterized = syntax_tree.with_functions(with_imported(
                const_functions::module_constant_functions(
                    syntax_tree,
                    session,
                    &const_env,
                    &type_aliases,
                    &parameter_values,
                ),
            ));
            let syntax_tree = &parameterized;
            extend_const_env_with_parameters(syntax_tree, session, &mut const_env, &parameters);

            let mut array_parameters = array_parameters::array_parameters_from_scope(
                syntax_tree,
                session,
                &const_env,
                &type_aliases,
            )?;
            for parameter in &array_parameters {
                const_env.insert(
                    parameters::unbounded_parameter_marker(&parameter.signal.name),
                    0,
                );
            }

            if !imported.declarations_only {
                let parameter_values = parameter_value_env_with_imported(
                    syntax_tree,
                    session,
                    &parameters,
                    &const_env,
                    &imported,
                );
                match reject_silently_ignored_constructs(
                    syntax_tree,
                    session,
                    &const_env,
                    &type_aliases,
                    &parameter_packed_dimensions_with_imported(&parameters, &imported).into(),
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
                            &const_env,
                            &type_aliases,
                            header_ports,
                        )?;
                        let preliminary_signals =
                            signals_from_scope(syntax_tree, session, &const_env, &type_aliases)?;
                        extend_const_env_with_variable_types(
                            &syntax_tree.functions,
                            &mut const_env,
                            preliminary_ports
                                .iter()
                                .map(|port| (port.name(), port.r#type()))
                                .chain(
                                    preliminary_signals
                                        .iter()
                                        .map(|signal| (signal.name(), signal.r#type())),
                                ),
                        );
                        parameters = parameters_from_scope(
                            syntax_tree,
                            session,
                            &type_aliases,
                            &const_env,
                            applicable_parameter_overrides,
                        )?;
                        for parameter in &mut parameters {
                            parameter.value = parameter.value.take().map(|value| {
                                substitute_typed_parameter_literals(
                                    value,
                                    &enum_constants.numbers,
                                    &enum_constants.types,
                                )
                            });
                        }
                        if name == override_module_name {
                            apply_parameter_overrides(&mut parameters, parameter_overrides)?;
                        }
                        extend_const_env_with_parameters(
                            syntax_tree,
                            session,
                            &mut const_env,
                            &parameters,
                        );
                        type_aliases =
                            type_aliases_from_scope_with_env(syntax_tree, session, &const_env)?
                                .into();
                        array_parameters = array_parameters::array_parameters_from_scope(
                            syntax_tree,
                            session,
                            &const_env,
                            &type_aliases,
                        )?;
                        let parameter_values = parameter_value_env_with_imported(
                            syntax_tree,
                            session,
                            &parameters,
                            &const_env,
                            &imported,
                        );
                        reject_silently_ignored_constructs(
                            syntax_tree,
                            session,
                            &const_env,
                            &type_aliases,
                            &parameter_packed_dimensions_with_imported(&parameters, &imported)
                                .into(),
                            &parameter_values,
                        )?;
                    }
                    Err(error) => return Err(error),
                }
            }
            let mut ports = ports_from_scope(
                syntax_tree,
                session,
                &const_env,
                &type_aliases,
                header_ports,
            )?;
            if let Some(port) = ports.iter().find(|port| port.r#type.holds_chandle()) {
                return Err(AnalyzerError::IllegalChandleUse {
                    detail: format!("port `{}` of chandle type", port.name),
                });
            }
            ports.extend(imported.ports.iter().cloned());
            let mut port_names = HashSet::default();
            if let Some(port) = ports.iter().find(|port| !port_names.insert(port.name())) {
                return Err(AnalyzerError::DuplicatePort {
                    module: name,
                    name: port.name().to_string(),
                });
            }
            if ports
                .iter()
                .any(|port| port.direction() == PortDirection::Ref)
            {
                return Err(AnalyzerError::Unsupported("ref port direction".to_string()));
            }
            // The static properties of each class specialization are variables
            // of the module (IEEE 1800-2023 8.9, 8.25).
            classes::specializations::collect(
                syntax_tree,
                session,
                &PackedDimensions::new(HashMap::default(), &const_env, &type_aliases),
            );
            let mut signals = signals_from_scope(syntax_tree, session, &const_env, &type_aliases)?;
            signals.extend(
                array_parameters
                    .iter()
                    .map(|parameter| parameter.signal.clone()),
            );
            // The array parameters of the packages the module uses are constant
            // signals of the module, like its own.
            signals.extend(imported.signals.iter().cloned());
            signals.extend(imported.state_signals.iter().cloned());
            for r#type in ports
                .iter()
                .map(Port::r#type)
                .chain(signals.iter().map(Signal::r#type))
            {
                validate_unpacked_dimension_sizes(
                    &syntax_tree.functions,
                    r#type.unpacked_ranges(),
                    &const_env,
                )?;
            }
            let signal_names: HashSet<_> = signals.iter().map(Signal::name).collect();
            if let Some(parameter) = parameters.iter().find(|parameter| {
                port_names.contains(parameter.name()) || signal_names.contains(parameter.name())
            }) {
                return Err(AnalyzerError::Unsupported(format!(
                    "parameter name collides with port or signal `{}`",
                    parameter.name()
                )));
            }
            let mut packed_dimensions = packed_dimensions_from_ports_and_signals(
                &ports,
                &signals,
                &const_env,
                &type_aliases,
            );
            packed_dimensions.constant_types = Arc::new(enum_constants.declarations.clone());
            for parameter in imported.parameters.iter().chain(&parameters) {
                let bindings = Arc::make_mut(&mut packed_dimensions.constant_types);
                bindings.remove(parameter.name());
                if let Some(ty) = &parameter.semantic_type {
                    bindings.insert(parameter.name().into(), ty.clone());
                }
            }
            // Four-state parameter values cannot be represented by the numeric
            // environment. Keep their expressions for constant case analysis.
            packed_dimensions.parameter_values = parameter_value_env_with_imported(
                syntax_tree,
                session,
                &parameters,
                &const_env,
                &imported,
            )
            .into();
            packed_dimensions
                .parameter_values
                .retain(|name, _| !const_env.contains_key(name));
            packed_dimensions.extend(parameter_packed_dimensions_with_imported(
                &parameters,
                &imported,
            ));
            // Instance connections may call subroutines, of the module or of the
            // packages it uses, with untyped assignment patterns.
            let (mut subroutine_params, mut subroutine_shapes, mut subroutine_directions) =
                procedural::subroutine_argument_names(
                    syntax_tree,
                    session,
                    &const_env,
                    &type_aliases,
                )?;
            for (name, params) in &imported.subroutine_params {
                subroutine_params
                    .entry(name.clone())
                    .or_insert_with(|| params.clone());
            }
            for (name, shapes) in &imported.subroutine_shapes {
                subroutine_shapes
                    .entry(name.clone())
                    .or_insert_with(|| shapes.clone());
            }
            // The subroutines of the packages the module uses, by their qualified
            // names and by the names its imports give them.
            for subroutine in &imported.subroutines {
                subroutine_directions
                    .entry(subroutine.name.clone())
                    .or_insert_with(|| {
                        subroutine
                            .params
                            .iter()
                            .map(|param| param.direction)
                            .collect()
                    });
            }
            for (name, target) in &imported.aliases {
                if let Some(directions) = subroutine_directions.get(target).cloned() {
                    subroutine_directions
                        .entry(name.clone())
                        .or_insert(directions);
                }
            }
            let dpi_items = generate::items(syntax_tree, session, &const_env, &type_aliases)?;
            let mut dpi_imports = dpi::imports(&dpi_items, syntax_tree, session, &type_aliases)?;
            for import in &dpi_imports {
                // Shapes check the actuals as assignments to the formals.
                subroutine_shapes.insert(
                    import.name().to_string(),
                    import
                        .arguments()
                        .iter()
                        .filter_map(|argument| {
                            let r#type = match argument.r#type() {
                                crate::ir::DpiType::Bit => type_binding::builtin_type("bit", None),
                                crate::ir::DpiType::Logic => {
                                    type_binding::builtin_type("logic", None)
                                }
                                crate::ir::DpiType::String => {
                                    type_binding::builtin_type("string", None)
                                }
                                crate::ir::DpiType::Chandle => {
                                    type_binding::builtin_type("chandle", None)
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
                subroutine_params.insert(
                    import.name().to_string(),
                    import
                        .arguments()
                        .iter()
                        .map(|arg| arg.name().to_string())
                        .collect(),
                );
                subroutine_directions.insert(
                    import.name().to_string(),
                    import
                        .arguments()
                        .iter()
                        .map(|argument| {
                            if argument.is_output() {
                                crate::procedural::ParamDirection::Output
                            } else {
                                crate::procedural::ParamDirection::Input
                            }
                        })
                        .collect(),
                );
            }
            packed_dimensions.subroutine_param_shapes = Arc::new(subroutine_shapes.clone());
            packed_dimensions.subroutine_directions = Arc::new(subroutine_directions.clone());
            packed_dimensions.subroutine_param_names = Arc::new(subroutine_params.clone());
            // Array parameters retain a constant value as well as their signal layout.
            // Bind their values in declaration order before child parameter arguments.
            for process in &imported.initial_processes {
                for statement in &process.body {
                    if let Stmt::Assign {
                        lhs: LValue::Ident(name),
                        rhs,
                        ..
                    } = statement
                    {
                        packed_dimensions
                            .array_parameter_values
                            .insert(name.clone(), rhs.clone());
                    }
                }
            }
            let mut array_initial_processes = Vec::new();
            let mut packed_pattern_values = HashMap::default();
            for parameter in &array_parameters {
                let mut process =
                    parameter.initial_process(syntax_tree, session, &packed_dimensions)?;
                // The value may name constants, such as enumeration members
                // (IEEE 1800-2023 6.19), which every other process is given as
                // values the same way.
                for stmt in &mut process.body {
                    procedural::substitute_stmt_constants(
                        syntax_tree,
                        session,
                        stmt,
                        &const_env,
                        &enum_constants.exprs,
                    );
                }
                if let Stmt::Assign { rhs, .. } = &process.body[0] {
                    packed_dimensions
                        .array_parameter_values
                        .insert(parameter.signal.name.clone(), rhs.clone());
                    let name = parameter.signal.name();
                    let shape = packed_dimensions.get(name).expect("array parameter shape");
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
                            &packed_dimensions,
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
                            &packed_dimensions.const_env,
                        );
                        packed_dimensions
                            .parameter_values
                            .insert(name.to_string(), value.clone());
                        packed_pattern_values.insert(name.to_string(), value);
                    }
                }
                array_initial_processes.push(process);
            }
            let mut instances = instances_from_scope(
                syntax_tree,
                session,
                &const_env,
                &packed_dimensions,
                interfaces,
            )?;
            let mut instance_names = HashSet::default();
            if let Some(instance) = instances
                .iter()
                .filter(|instance| {
                    instance
                        .condition()
                        .and_then(|condition| {
                            eval_ast_const_expr(&syntax_tree.functions, condition, &const_env)
                        })
                        .is_none_or(|value| value != 0)
                })
                .find(|instance| !instance_names.insert(instance.name()))
            {
                return Err(AnalyzerError::DuplicateInstance {
                    module: name,
                    name: instance.name().to_string(),
                });
            }
            // A generate block shares its name space with the module's other
            // declarations (IEEE 1800-2023 27.6, 23.9).
            // The members of a variable of unpacked structure type are named
            // like declarations of a scope of the variable's name.
            let aggregates: HashSet<&str> = signals
                .iter()
                .filter(|signal| signal.is_aggregate())
                .map(Signal::name)
                .collect();
            let scope_heads: HashSet<&str> = signals
                .iter()
                .filter(|signal| signal.hierarchical.is_none())
                .map(Signal::name)
                .chain(instances.iter().map(Instance::name))
                .filter_map(generate_scope_head)
                .filter(|head| !aggregates.contains(head))
                .collect();
            if let Some(conflict) = ports
                .iter()
                .map(Port::name)
                .chain(signals.iter().map(Signal::name))
                .chain(instances.iter().map(Instance::name))
                .find(|conflict| scope_heads.contains(conflict))
            {
                return Err(AnalyzerError::DuplicateGenerateScope {
                    module: name,
                    name: conflict.to_string(),
                });
            }
            let mut parameter_values = parameter_value_env_with_imported(
                syntax_tree,
                session,
                &parameters,
                &const_env,
                &imported,
            );
            parameter_values.extend(packed_pattern_values);
            for (name, value) in &enum_constants.exprs {
                parameter_values
                    .entry(name.clone())
                    .or_insert_with(|| value.clone());
            }
            let mut expression_signedness = ports
                .iter()
                .map(|port| (port.name().to_string(), port.r#type().is_signed()))
                .chain(
                    signals
                        .iter()
                        .map(|signal| (signal.name().to_string(), signal.r#type().is_signed())),
                )
                .collect::<HashMap<_, _>>();
            expression_signedness.extend(
                parameter_types_from_const_env(&const_env)
                    .into_iter()
                    .map(|(name, r#type)| (name, r#type.signed)),
            );
            let mut functions =
                functions_from_scope(syntax_tree, session, &const_env, &packed_dimensions)?;
            for (name, function) in &imported.functions {
                functions
                    .entry(name.clone())
                    .or_insert_with(|| function.clone());
            }
            for (name, metadata) in &imported.function_return_types {
                packed_dimensions
                    .function_return_types
                    .entry(name.clone())
                    .or_insert_with(|| metadata.clone());
            }
            packed_dimensions
                .function_return_types
                .extend(functions.iter().map(|(name, function)| {
                    (
                        name.clone(),
                        FunctionReturnMetadata::from_function(function),
                    )
                }));
            packed_dimensions
                .function_return_types
                .extend(dpi_imports.iter().filter_map(|import| {
                    FunctionReturnMetadata::from_dpi(import, &mut session.types)
                        .map(|metadata| (import.name().to_string(), metadata))
                }));
            // Class methods are subroutines `class::method` (IEEE 1800-2023 8.6).
            dimensions::all_class_signatures(syntax_tree, session, &mut packed_dimensions);
            packed_dimensions.functions = Arc::new(functions.clone());
            packed_dimensions.expression_signedness = expression_signedness.clone().into();
            for instance in &mut instances {
                for connection in &mut instance.port_connections {
                    if let Some(pattern) = &mut connection.bound_pattern {
                        pattern.resolve_types(&packed_dimensions)?;
                    }
                    connection.map_expressions(|expr| {
                        let expr =
                            expand_expr_calls(expr, &functions, &expression_signedness, 0, true);
                        substitute_expr_constants_with_parameter_literals(
                            syntax_tree,
                            session,
                            expr,
                            &const_env,
                            &enum_constants.exprs,
                        )
                    });
                }
            }
            // Bodies now have the active module's declarations and function types.
            // Declaration-time queries still use syntax discovery while metadata
            // is incomplete; generated/procedural scopes overlay this complete base.
            packed_dimensions.bind_types(&mut session.types, &syntax_tree.functions);
            packed_dimensions.scope_types_complete = true;
            let mut locals = Vec::new();
            let mut local_counter = procedural::first_local(syntax_tree, session);
            let mut body_state = procedural::BodyState {
                locals: &mut locals,
                counter: &mut local_counter,
                subroutine_params: &subroutine_params,
                subroutine_directions: &subroutine_directions,
                automatic: lifetimes::automatic_by_default(syntax_tree),
                statics: Vec::new(),
                explicit_statics: HashSet::default(),
                static_initializers: Vec::new(),
                constant_evaluation: false,
            };
            let (mut subroutines, mut comb_processes, mut ff_processes, mut initial_processes) =
                if imported.declarations_only {
                    (Vec::new(), Vec::new(), Vec::new(), Vec::new())
                } else {
                    let subroutines = procedural::subroutines_from_scope(
                        syntax_tree,
                        session,
                        &const_env,
                        &packed_dimensions,
                        &parameter_values,
                        &mut body_state,
                    )?;
                    let mut comb_processes = comb_processes_from_scope(
                        syntax_tree,
                        session,
                        &const_env,
                        &packed_dimensions,
                        &parameter_values,
                        &mut body_state,
                    )?
                    .into_iter()
                    .map(|process| {
                        substitute_process_constants_with_parameter_literals(
                            syntax_tree,
                            session,
                            process,
                            &const_env,
                            &parameter_values,
                        )
                    })
                    .collect::<Vec<_>>();
                    let mut ff_processes = ff_processes_from_scope(
                        syntax_tree,
                        session,
                        &const_env,
                        &parameter_values,
                        &packed_dimensions,
                        &mut body_state,
                    )?;
                    comb_processes.extend(imported.comb_processes.iter().cloned());
                    ff_processes.extend(imported.ff_processes.iter().cloned());
                    let mut initial_processes = array_initial_processes;
                    initial_processes.extend(imported.initial_processes.iter().cloned());
                    initial_processes.extend(procedural::initial_processes_from_scope(
                        syntax_tree,
                        session,
                        &const_env,
                        &packed_dimensions,
                        &parameter_values,
                        &mut body_state,
                    )?);
                    (subroutines, comb_processes, ff_processes, initial_processes)
                };
            net_delays::lower(
                syntax_tree,
                &mut packed_dimensions,
                &ports,
                &instances,
                &mut signals,
                &mut comb_processes,
            )?;
            net_drives::lower(&ports, &instances, &signals, &mut comb_processes);
            continuous_delays::lower(
                syntax_tree,
                &packed_dimensions,
                &mut signals,
                &mut comb_processes,
                &mut initial_processes,
            )?;
            let (inline_subroutines, inline_randomizations) =
                procedural::randomization::inline_subroutines(
                    syntax_tree,
                    session,
                    &mut body_state,
                    &packed_dimensions,
                )?;
            subroutines.extend(inline_subroutines);
            // Static locals of the processes that keep their value are variables
            // of the module, initialized once with the declaration initializers.
            let statics: HashMap<String, Type> = body_state.statics.drain(..).collect();
            let (mut kept, mut static_initializers) = lifetimes::keep_static_locals(
                syntax_tree,
                session,
                &statics,
                &subroutine_directions,
                comb_processes
                    .iter_mut()
                    .map(|process| (&mut process.body, false))
                    .chain(
                        ff_processes
                            .iter_mut()
                            .map(|process| (&mut process.body, false)),
                    )
                    .chain(
                        initial_processes
                            .iter_mut()
                            .map(|process| (&mut process.body, !process.repeats)),
                    ),
                &body_state.explicit_statics,
                &[],
                false,
            )?;
            for subroutine in &subroutines {
                kept.extend(subroutine.persistent_locals.iter().cloned());
            }
            // Statics of subroutines are initialized first: a run-time
            // initializer of a process local may call the subroutine.
            static_initializers.splice(0..0, body_state.static_initializers.drain(..));
            {
                let kept: HashSet<&str> = kept.iter().map(String::as_str).collect();
                locals.retain(|local| !kept.contains(local.name.as_str()));
            }
            // The package or compilation unit this scope is, whose variables
            // are its own, not imported.
            let own_package = syntax_tree
                .root_unit()
                .ok()
                .filter(|unit| unit.kind == celox_sv_syntax::UnitKind::Package)
                .map(|unit| instances::normalize_identifier(&unit.name.spelling))
                .or_else(|| {
                    (syntax_tree.scope_handle() == crate::source_tree::ScopeHandle::Compilation)
                        .then(|| packages::UNIT.to_string())
                });
            for name in kept {
                // A static variable is named by its owner and declaration (see
                // `BodyBuilder::declare_static`), so a scope that lowers a
                // package subroutine again, such as a method of a class
                // specialization an earlier analysis made, may import the
                // same variable. Another package's is that variable; one of
                // this scope's own package is this declaration.
                if let Some(index) = signals.iter().position(|signal| signal.name == name) {
                    let own = signals[index]
                        .package_variable
                        .as_ref()
                        .is_some_and(|(package, _)| Some(package) == own_package.as_ref());
                    if !own {
                        continue;
                    }
                    signals.remove(index);
                }
                signals.push(Signal::new(name.clone(), statics[&name].clone()));
            }
            if !static_initializers.is_empty() {
                // Declaration initializers run before the other initial blocks.
                let first_block = initial_processes
                    .iter()
                    .position(|process| !process.is_initializer())
                    .unwrap_or(initial_processes.len());
                // They may read module variables, so they also stay after those
                // initializers when processes are reordered by creation.
                let creation_order = initial_processes
                    .iter()
                    .filter(|process| process.is_initializer())
                    .map(|process| process.creation_order)
                    .max()
                    .unwrap_or(0);
                initial_processes.insert(
                    first_block,
                    InitialProcess {
                        creation_order,
                        site: None,
                        condition: None,
                        body: static_initializers,
                        kind: celox_design::ProcessKind::Initializer,
                        validation_body: Vec::new(),
                        synthetic: false,
                        repeats: false,
                    },
                );
            }
            // A call of a name an import binds calls the package subroutine or
            // DPI import. Calls of subroutines declared in a generate block were
            // already bound to those.
            let calls: HashMap<String, String> = imported
                .aliases
                .iter()
                .filter(|(_, target)| {
                    imported
                        .subroutines
                        .iter()
                        .any(|subroutine| subroutine.name == **target)
                        || imported
                            .dpi_imports
                            .iter()
                            .any(|import| import.name() == target.as_str())
                })
                .map(|(name, target)| (name.clone(), target.clone()))
                .collect();
            if !calls.is_empty() {
                let rename = scope::Renamer { names: &calls };
                let body =
                    |body: Vec<Stmt>| body.into_iter().map(|stmt| rename.stmt(stmt)).collect();
                for process in &mut comb_processes {
                    process.body = body(std::mem::take(&mut process.body));
                    process.sensitivity_body = body(std::mem::take(&mut process.sensitivity_body));
                    for assignment in &mut process.assignments {
                        assignment.rhs = rename.expr(assignment.rhs.clone());
                    }
                }
                for process in &mut ff_processes {
                    process.body = body(std::mem::take(&mut process.body));
                }
                for process in &mut initial_processes {
                    process.body = body(std::mem::take(&mut process.body));
                    process.validation_body = body(std::mem::take(&mut process.validation_body));
                }
                for subroutine in &mut subroutines {
                    *subroutine = rename.subroutine(subroutine.clone());
                }
                for instance in &mut instances {
                    for connection in &mut instance.port_connections {
                        connection.map_expressions(|expr| rename.expr(expr));
                    }
                }
            }
            let nets = signals
                .iter()
                .filter(|signal| signal.is_net())
                .map(Signal::name)
                .chain(ports.iter().filter(|port| port.is_net).map(Port::name))
                .collect();
            procedural::references::validate(
                subroutines.iter().chain(imported.subroutines.iter()),
                &comb_processes,
                &ff_processes,
                &initial_processes,
                &subroutine_directions,
                &subroutine_params,
                &nets,
            )?;
            procedural::timed_tasks::classify(
                subroutines.iter().chain(imported.subroutines.iter()),
                &mut comb_processes,
                &mut ff_processes,
                &mut initial_processes,
            )?;
            let assignments = comb_processes
                .iter()
                .flat_map(|process| process.assignments().iter().cloned())
                .collect();
            if is_package || export_symbols {
                enum_constants.declarations.extend(
                    classes::specializations::export_enum_constants(syntax_tree, session),
                );
            }
            // A package keeps its symbols for the scopes that use it.
            let symbols = (is_package || export_symbols).then(|| scope::ScopeSymbols {
                package_scopes: imported.package_scopes.clone(),
                classes: classes::scope_classes(syntax_tree, session, &const_env),
                class_member_types: classes::specializations::export_member_types(
                    syntax_tree,
                    session,
                ),
                inline_randomizations,
                declarations_only: imported.declarations_only,
                type_parameter_bindings: HashMap::default(),
                pattern_parameter_bindings: HashMap::default(),
                const_env: (*const_env).clone(),
                parameter_values: parameter_values.clone(),
                parameters: parameters.clone(),
                // A class of the scope is a type of it (IEEE 1800-2023 8.3).
                type_aliases: type_aliases
                    .iter()
                    .map(|(name, ty)| (name.clone(), ty.clone()))
                    .chain(classes::scope_class_types(syntax_tree))
                    .collect(),
                enum_constants: enum_constants.clone(),
                functions: functions.clone(),
                function_return_types: (*packed_dimensions.function_return_types).clone(),
                constant_functions: syntax_tree.functions.clone(),
                subroutine_params: subroutine_params.clone(),
                subroutine_shapes: subroutine_shapes.clone(),
                subroutines: subroutines.clone(),
                locals: locals.clone(),
                dpi_imports: dpi_imports.clone(),
                // A package's signals are its array parameters and constant
                // variables, initialized by its initial processes.
                signals: signals.clone(),
                ports: Vec::new(),
                state_signals: Vec::new(),
                initial_processes: initial_processes.clone(),
                comb_processes: comb_processes.clone(),
                ff_processes: ff_processes.clone(),
                aliases: imported.aliases.clone(),
                interface_members: imported.interface_members.clone(),
                local_aliases: imported.local_aliases.clone(),
                interface_types: imported.interface_types.clone(),
                interface_modports: imported.interface_modports.clone(),
                interface_arrays: imported.interface_arrays.clone(),
                interface_scopes: imported.interface_scopes.clone(),
                interface_declarations: imported.interface_declarations.clone(),
                interface_instances: imported.interface_instances.clone(),
                generate_imports: HashMap::default(),
            });
            // The subroutines, locals and DPI imports of the packages the module
            // uses are lowered with it.
            {
                let mut known: HashSet<&str> = subroutines
                    .iter()
                    .map(|subroutine| subroutine.name.as_str())
                    .collect();
                let missing = imported
                    .subroutines
                    .iter()
                    .filter(|subroutine| known.insert(subroutine.name.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                subroutines.extend(missing);
                let mut known: HashSet<&str> =
                    locals.iter().map(|local| local.name.as_str()).collect();
                let missing = imported
                    .locals
                    .iter()
                    .filter(|local| known.insert(local.name.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                locals.extend(missing);
            }
            for import in &imported.dpi_imports {
                if !dpi_imports
                    .iter()
                    .any(|known| known.name() == import.name())
                {
                    dpi_imports.push(import.clone());
                }
            }
            let mut imported_parameters = imported.parameters.clone();
            imported_parameters
                .retain(|parameter| !parameters.iter().any(|local| local.name == parameter.name));

            // Expression construction tracks only whole-variable assignments.
            // Leave calls of functions with selected/concatenated writes symbolic
            // in declaration contracts rather than publishing a partial body.
            // Expression inlining also sizes formal and return conversions at the
            // default specialization; symbolic signature widths cannot be inlined.
            for subroutine in &declaration_subroutines {
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
                    && let Some(function) = functions.get_mut(&subroutine.name)
                {
                    function.body = None;
                }
            }

            // A native default value is not evidence of a representable symbolic
            // body. Keep failed declaration lowerings visible to contract checking.
            for function in functions.values_mut() {
                if !declaration_subroutines
                    .iter()
                    .any(|subroutine| subroutine.name == function.name)
                {
                    function.body = None;
                }
            }

            let mut interface_scope_names = imported
                .interface_types
                .keys()
                .chain(imported.interface_arrays.keys())
                .cloned()
                .collect::<Vec<_>>();
            interface_scope_names.sort_unstable();
            interface_scope_names.dedup();
            let interface_handles = virtual_interfaces::handles(session, &imported, &ports);

            Ok(Self {
                name,
                program,
                interface_scope_names,
                interface_handles,
                parameters,
                ports,
                signals,
                instances,
                assignments,
                comb_processes,
                ff_processes,
                initial_processes,
                locals,
                subroutines,
                dpi_imports,
                constant_functions: syntax_tree.functions.clone(),
                symbolic_functions: functions,
                declaration_subroutines,
                imported_parameters,
                symbols,
            })
        })(session);
        generate::restore(session, previous);
        module
    }
}

/// The declared dimensions of `parameters` and of the package parameters a
/// scope imports, so that selects use their declared ranges.
pub(super) fn parameter_packed_dimensions_with_imported(
    parameters: &[Parameter],
    imported: &scope::ScopeSymbols,
) -> VariablePackedDimensions {
    let mut dimensions = parameter_packed_dimensions(&imported.parameters);
    // A local scalar or implicitly typed parameter also shadows an imported
    // packed parameter, even though it contributes no explicit packed ranges.
    for parameter in parameters {
        dimensions.remove(parameter.name());
    }
    dimensions.extend(parameter_packed_dimensions(parameters));
    dimensions
}

/// The literal values of `parameters`, and of the package parameters and
/// enum members a scope imports.
pub(super) fn parameter_value_env_with_imported(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    parameters: &[Parameter],
    const_env: &ConstEnv,
    imported: &scope::ScopeSymbols,
) -> HashMap<String, Expr> {
    let mut values = parameter_value_env(tree, session, parameters, const_env);
    let local_names: HashSet<_> = parameters.iter().map(Parameter::name).collect();
    for (name, value) in &imported.parameter_values {
        if !local_names.contains(name.as_str()) {
            values.entry(name.clone()).or_insert_with(|| value.clone());
        }
    }
    values
}

/// The outermost generate scope of a scoped name such as `g.x` or `g[0].x`,
/// or `None` for a name declared directly in the module. An escaped scope
/// component ends at its terminating space.
pub(super) fn generate_scope_head(name: &str) -> Option<&str> {
    let end = if name.starts_with('\\') {
        name.find(' ')? + 1
    } else {
        name.find(['.', '['])?
    };
    Some(&name[..end])
}
