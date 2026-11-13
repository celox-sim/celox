//! Elaboration of one module, package or compilation-unit scope.

use super::*;

mod bodies;
mod constants;
mod declarations;
mod finish;
mod reachable_imports;

/// The state [`Module::from_selected_scope_once`] elaborates a scope in: what
/// the scope is elaborated for, and what the phases found of it so far. The
/// phases run in the order `from_selected_scope_once` calls them; a field
/// they find is empty until its phase ran. The session and the context that
/// binds in the scope are passed to each phase.
struct ScopeElaboration<'a> {
    name: String,
    /// See [`Module::is_program`].
    program: bool,
    /// Whether the scope is a package or a compilation unit.
    is_package: bool,
    /// Whether the scope keeps its symbols for the scopes that use it.
    export_symbols: bool,
    /// The module the parameter overrides are for.
    override_module_name: &'a str,
    parameter_overrides: &'a HashMap<String, ConstExpr>,
    /// The parameter overrides, if they are for this scope.
    applicable_parameter_overrides: &'a HashMap<String, ConstExpr>,
    interfaces: &'a InterfaceLookup<'a>,
    /// The symbols of the packages the scope uses.
    imported: &'a scope::ScopeSymbols,
    header_ports: Option<&'a port_owners::Selection<'a>>,
    /// The types the scope declares.
    type_aliases: SharedAliases,
    parameters: Vec<Parameter>,
    /// The constants of the scope and of the packages it uses.
    const_env: SharedEnv,
    enum_constants: EnumMemberConstants,
    /// The functions the types of the scope's header may call.
    declaration_subroutines: Vec<crate::ir::Subroutine>,
    array_parameters: Vec<array_parameters::ArrayParameter>,
    ports: Vec<Port>,
    signals: Vec<Signal>,
    /// What expressions of the scope see of its declarations.
    packed_dimensions: PackedDimensions,
    /// The argument names of each subroutine the scope may call.
    subroutine_params: HashMap<String, Vec<String>>,
    /// The declared shape of each argument of each of those subroutines.
    subroutine_shapes: HashMap<String, Vec<VariableDimensions>>,
    subroutine_directions: lifetimes::Directions,
    /// The generate items the DPI imports were collected from. They are kept
    /// while the scope elaborates, as the tables of their scopes are.
    dpi_items: Vec<generate::Item>,
    dpi_imports: Vec<crate::ir::DpiImport>,
    instances: Vec<Instance>,
    /// The literal values of the parameters and enumeration members.
    parameter_values: HashMap<String, Expr>,
    functions: HashMap<String, Function>,
    locals: Vec<LocalVariable>,
    subroutines: Vec<Subroutine>,
    comb_processes: Vec<CombProcess>,
    ff_processes: Vec<FfProcess>,
    initial_processes: Vec<InitialProcess>,
}

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
            // A specialization of a package class that names a class of the
            // compilation unit, such as `uvm_component_registry #(C)` for a
            // test that UVM creates by name, runs the class's methods and
            // static initializers: the scope uses the unit, as one naming
            // the class does (IEEE 1800-2023 3.12.1, 8.25).
            // Only the unit of this scope's source is taken, for an item
            // declared before the scope: another is not visible here
            // (3.12.1).
            if let Some(unit) = symbols.unit.take()
                && let Some(unit_symbols) = unit.taken(
                    symbols
                        .classes
                        .iter()
                        .map(|class| class.name.as_str())
                        .chain(
                            symbols
                                .subroutines
                                .iter()
                                .map(|subroutine| subroutine.name.as_str()),
                        ),
                )?
            {
                symbols.extend(unit_symbols);
            }
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
            let mut scope = ScopeElaboration {
                name,
                program,
                is_package,
                export_symbols,
                override_module_name,
                parameter_overrides,
                applicable_parameter_overrides,
                interfaces,
                imported: &imported,
                header_ports,
                type_aliases: SharedAliases::default(),
                parameters: Vec::new(),
                const_env: SharedEnv::default(),
                enum_constants: EnumMemberConstants::default(),
                declaration_subroutines: Vec::new(),
                array_parameters: Vec::new(),
                ports: Vec::new(),
                signals: Vec::new(),
                packed_dimensions: PackedDimensions::default(),
                subroutine_params: HashMap::default(),
                subroutine_shapes: HashMap::default(),
                subroutine_directions: lifetimes::Directions::default(),
                dpi_items: Vec::new(),
                dpi_imports: Vec::new(),
                instances: Vec::new(),
                parameter_values: HashMap::default(),
                functions: HashMap::default(),
                locals: Vec::new(),
                subroutines: Vec::new(),
                comb_processes: Vec::new(),
                ff_processes: Vec::new(),
                initial_processes: Vec::new(),
            };
            let parameterized = scope.constants(syntax_tree, session)?;
            let syntax_tree = &parameterized;
            scope.reject_ignored_constructs(syntax_tree, session)?;
            scope.ports_and_signals(syntax_tree, session)?;
            scope.declared_shapes(syntax_tree, session);
            scope.subroutine_arguments(syntax_tree, session)?;
            let (array_initial_processes, packed_pattern_values) =
                scope.array_parameter_values(syntax_tree, session)?;
            scope.instances(syntax_tree, session)?;
            scope.functions(syntax_tree, session, packed_pattern_values)?;
            let inline_randomizations =
                scope.bodies(syntax_tree, session, array_initial_processes)?;
            scope.rename_imported_calls();
            scope.validate_bodies()?;
            let assignments = scope.assignments();
            let symbols = scope.symbols(syntax_tree, session, inline_randomizations);
            let imported_parameters = scope.lower_imported();
            scope.prune_symbolic_functions(syntax_tree);
            Ok(scope.finish(
                syntax_tree,
                session,
                assignments,
                symbols,
                imported_parameters,
            ))
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
