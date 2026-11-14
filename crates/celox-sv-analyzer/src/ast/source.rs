//! Analysis of the modules of one source.

use super::*;

impl Source {
    pub(crate) fn from_syntax(syntax_tree: &SyntaxTree) -> Result<Self, AnalyzerError> {
        Self::from_syntax_with_module_parameter_overrides(syntax_tree, "", &ConstEnv::default())
    }

    pub(crate) fn from_syntax_with_module_parameter_overrides(
        syntax_tree: &SyntaxTree,
        module_name: &str,
        parameter_overrides: &ConstEnv,
    ) -> Result<Self, AnalyzerError> {
        let packages = packages::Packages::analyze(&[(syntax_tree, Path::new(""))])?;
        // An illegal compilation unit is an error also where no module uses
        // it (IEEE 1800-2023 3.12.1).
        if let Some(error) = packages.compilation_unit_illegality() {
            return Err(error.clone());
        }
        let session = &mut Session::with_packages(&packages, &mut packages.design());
        let context = AnalysisContext::unspecialized(syntax_tree);
        let syntax_tree = &context;
        let parameter_overrides = parameter_overrides
            .iter()
            .map(|(name, value)| (name.clone(), const_expr_from_i128(*value)))
            .collect();
        let interfaces = Self::module_interfaces_from_syntax(syntax_tree)?;
        let timed = AnalysisContext {
            timescale: timescales::for_tree(packages.timescales.clone(), syntax_tree)?,
            ..syntax_tree.clone()
        };
        let syntax_tree = &timed;
        let mut modules = Vec::new();
        for handle in syntax_tree.root_scopes()? {
            let scoped = syntax_tree.scoped(handle)?;
            let scoped_context = timed.child(&scoped, &parameter_overrides);
            let syntax_tree = &scoped_context;
            let unit = syntax_tree.root_unit()?;
            if !unit.kind.is_module_like() {
                continue;
            }
            match unit.port_style {
                celox_sv_syntax::PortStyle::Ansi => {
                    let imported = imports::imported_symbols(
                        syntax_tree,
                        &packages,
                        packages.unit(Path::new("")),
                    )?;
                    modules.push(
                        Module::from_scope_with_parameter_overrides(
                            syntax_tree,
                            session,
                            module_name,
                            &parameter_overrides,
                            &InterfaceLookup {
                                local: &interfaces,
                                extra: &ModuleInterfaces::default(),
                            },
                            Arc::new(imported),
                        )
                        .map_err(|error| error.with_warnings(session.diagnostics.clone()))?,
                    );
                }
                celox_sv_syntax::PortStyle::NonAnsi => {
                    return Err(AnalyzerError::Unsupported(
                        "non-ANSI port expressions".to_string(),
                    ));
                }
                _ => {}
            }
        }

        Ok(Self {
            modules,
            diagnostics: session.diagnostics.clone(),
        })
    }

    pub(crate) fn from_syntax_module_with_parameter_overrides(
        syntax_tree: &SyntaxTree,
        module_name: &str,
        parameter_overrides: &ConstEnv,
    ) -> Result<Self, AnalyzerError> {
        let parameter_overrides = parameter_overrides
            .iter()
            .map(|(name, value)| (name.clone(), const_expr_from_i128(*value)))
            .collect();
        Self::from_syntax_module_with_parameter_expr_overrides(
            syntax_tree,
            module_name,
            &parameter_overrides,
            &ModuleInterfaces::default(),
        )
    }

    /// `extra_interfaces` describes modules declared in other sources; the
    /// modules of `syntax_tree` are always known.
    pub(crate) fn from_syntax_module_with_parameter_expr_overrides(
        syntax_tree: &SyntaxTree,
        module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        extra_interfaces: &ModuleInterfaces,
    ) -> Result<Self, AnalyzerError> {
        let index = module_index::ModuleIndex::new(syntax_tree)?;
        let packages = packages::Packages::analyze(&[(syntax_tree, Path::new(""))])?;
        // An illegal compilation unit is an error also where no module uses
        // it (IEEE 1800-2023 3.12.1).
        if let Some(error) = packages.compilation_unit_illegality() {
            return Err(error.clone());
        }
        Self::from_indexed_syntax_module(
            syntax_tree,
            Path::new(""),
            &index,
            module_name,
            ModuleParameterBindings {
                declarations_only: false,
                values: parameter_overrides,
                types: &[],
                patterns: &[],
                hierarchical: &[],
            },
            extra_interfaces,
            &packages,
            &mut packages.design(),
        )
    }

    /// `packages` are the packages the module may use, from any source, and
    /// the compilation units of the sources; `path` names the source of
    /// `syntax_tree`.
    pub(crate) fn from_indexed_syntax_module(
        syntax_tree: &SyntaxTree,
        path: &Path,
        index: &module_index::ModuleIndex,
        module_name: &str,
        bindings: ModuleParameterBindings<'_>,
        extra_interfaces: &ModuleInterfaces,
        packages: &packages::Packages,
        design: &mut packages::Design,
    ) -> Result<Self, AnalyzerError> {
        let mut session = Session::with_packages(packages, design);
        // A type argument of the module is a type of the analysis, whether
        // or not a source names it: interning it interns the types it is
        // made of, a chandle element of a container included.
        session.scope = Some(module_scope(module_name, &bindings, &mut session.types));
        let source = Self::from_indexed_syntax_module_in(
            syntax_tree,
            path,
            index,
            module_name,
            bindings,
            extra_interfaces,
            packages,
            &mut session,
        );
        session.restore(design);
        source
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn from_indexed_syntax_module_in(
        syntax_tree: &SyntaxTree,
        path: &Path,
        index: &module_index::ModuleIndex,
        module_name: &str,
        bindings: ModuleParameterBindings<'_>,
        extra_interfaces: &ModuleInterfaces,
        packages: &packages::Packages,
        session: &mut Session,
    ) -> Result<Self, AnalyzerError> {
        let context = AnalysisContext::unspecialized(syntax_tree);
        let syntax_tree = &context;
        let timed = AnalysisContext {
            timescale: timescales::for_tree(packages.timescales.clone(), syntax_tree)?,
            ..syntax_tree.clone()
        };
        let interfaces = InterfaceLookup {
            local: &index.interfaces,
            extra: extra_interfaces,
        };
        let mut modules = Vec::new();
        let interface_plan = packages.interface_plan.modules.get(module_name);
        let original_name = interface_plan.map_or(module_name, |plan| plan.original.as_str());
        for scope_tree in index.scopes(original_name) {
            let scope_context = timed.child(scope_tree, bindings.values);
            let syntax_tree = &scope_context;
            match syntax_tree.root_unit()?.port_style {
                celox_sv_syntax::PortStyle::Ansi => {
                    let mut imported =
                        imports::imported_symbols(syntax_tree, packages, packages.unit(path))?;
                    imported.declarations_only = bindings.declarations_only;
                    let own_types = bindings
                        .types
                        .iter()
                        .filter(|(name, _)| {
                            !interface_plan.is_some_and(|plan| {
                                plan.ports
                                    .iter()
                                    .any(|port| name.starts_with(&format!("{}$", port.name)))
                            })
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    imported.type_parameter_bindings =
                        type_parameters::bind(syntax_tree, &own_types)?;
                    imported.pattern_parameter_bindings =
                        type_parameters::bind_patterns(syntax_tree, bindings.patterns)?;
                    // A hierarchical name reads and writes the local signal
                    // bound to the other instance's object.
                    for signal in bindings.hierarchical {
                        for path in &signal.paths {
                            imported
                                .interface_members
                                .insert(path.clone(), signal.name.clone());
                        }
                        if signal.local {
                            imported.local_aliases.extend(signal.paths.iter().cloned());
                            continue;
                        }
                        imported.state_signals.push(Signal {
                            is_const: false,
                            name: signal.name.clone(),
                            r#type: signal.r#type.clone(),
                            is_net: signal.is_net,
                            unresolved_net: false,
                            net_resolution: NetResolution::Wire,
                            net_drive: NetDrive::None,
                            package_variable: None,
                            hierarchical: signal.paths.first().cloned(),
                        });
                    }
                    if let Some(mut module) = interface_binding::analyze_module(
                        syntax_tree,
                        session,
                        path,
                        packages,
                        interface_binding::Bindings {
                            values: bindings.values,
                            types: bindings.types,
                            export_symbols: false,
                            plan: interface_plan,
                        },
                        &interfaces,
                        imported.clone(),
                    )
                    .map_err(|error| error.with_warnings(session.diagnostics.clone()))?
                    {
                        module.name = module_name.to_string();
                        modules.push(module);
                        continue;
                    }
                    modules.push(
                        Module::from_scope_with_parameter_overrides(
                            syntax_tree,
                            session,
                            module_name,
                            bindings.values,
                            &interfaces,
                            Arc::new(imported),
                        )
                        .map_err(|error| error.with_warnings(session.diagnostics.clone()))?,
                    );
                }
                celox_sv_syntax::PortStyle::NonAnsi if syntax_tree.root_name()? == module_name => {
                    return Err(AnalyzerError::Unsupported(
                        "non-ANSI port expressions".to_string(),
                    ));
                }
                _ => {}
            }
        }
        Ok(Self {
            modules,
            diagnostics: session.diagnostics.clone(),
        })
    }

    pub(crate) fn module_names_from_syntax(
        syntax_tree: &SyntaxTree,
    ) -> Result<Vec<String>, AnalyzerError> {
        let mut names = Vec::new();
        for handle in syntax_tree.root_scopes()? {
            let scoped = syntax_tree.scoped(handle)?;
            let syntax_tree = &scoped;
            let unit = syntax_tree.root_unit()?;
            if unit.kind.is_module_like() && unit.port_style != celox_sv_syntax::PortStyle::Wildcard
            {
                names.push(syntax_tree.root_name()?);
            }
        }
        Ok(names)
    }

    /// The positional interface of every ANSI module declared in `syntax_tree`.
    pub(crate) fn module_interfaces_from_syntax(
        syntax_tree: &SyntaxTree,
    ) -> Result<ModuleInterfaces, AnalyzerError> {
        let mut interfaces = ModuleInterfaces::default();
        for handle in syntax_tree.root_scopes()? {
            let scoped = syntax_tree.scoped(handle)?;
            let syntax_tree = &scoped;
            let unit = syntax_tree.root_unit()?;
            if unit.kind.is_module_like() && unit.port_style == celox_sv_syntax::PortStyle::Ansi {
                let name = syntax_tree.root_name()?;
                let context = AnalysisContext::unspecialized(syntax_tree);
                interfaces.insert(name, module_interface_from_scope(&context)?);
            }
        }
        Ok(interfaces)
    }

    pub fn modules(&self) -> &[Module] {
        &self.modules
    }

    /// The modules, to analyze them from their own bodies.
    pub(crate) fn into_modules(self) -> Vec<Module> {
        self.modules
    }

    pub(crate) fn warnings(&self) -> Vec<warnings::AnalyzerWarning> {
        let mut warnings = self.diagnostics.clone();
        // A program's functions run in its threads, where `$exit` applies.
        for module in self.modules.iter().filter(|module| !module.program) {
            warnings::append(&mut warnings, module.constant_functions.warnings());
        }
        warnings
    }
}

/// The elaborated scope of an analysis of module `module_name` with
/// `bindings`, whose type arguments it interns in `table`: instances with
/// the same parameter values and type arguments share one analysis, and so
/// the types it declares (IEEE 1800-2023 6.22 makes them types of each
/// instance; see the design notes). A type argument is told by its semantic
/// type, which names its declaration, and the bindings by their names, in
/// whatever order the instance gives them. Hierarchical signals do not
/// change a type, so they do not tell scopes apart.
fn module_scope(
    module_name: &str,
    bindings: &ModuleParameterBindings<'_>,
    table: &mut semantic_types::TyTable,
) -> Arc<str> {
    use std::hash::{Hash, Hasher};
    let mut hasher = fxhash::FxHasher::default();
    let mut values = bindings.values.iter().collect::<Vec<_>>();
    values.sort_by(|left, right| left.0.cmp(right.0));
    values.hash(&mut hasher);
    let mut types = bindings
        .types
        .iter()
        .map(|(name, ty)| {
            let id = semantic_types::of_type(
                table,
                &const_functions::ConstantFunctions::default(),
                ty,
                &ConstEnv::default(),
            );
            // A type without a semantic type yet is told by its fields.
            (name, id.ok_or(ty))
        })
        .collect::<Vec<_>>();
    types.sort_by(|left, right| left.0.cmp(right.0));
    types.hash(&mut hasher);
    let mut patterns = bindings.patterns.iter().collect::<Vec<_>>();
    patterns.sort_by(|left, right| left.0.cmp(&right.0));
    patterns.hash(&mut hasher);
    format!("{module_name}#{:016x}", hasher.finish()).into()
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    /// The scope of an analysis of `Leaf` with `values` and `types`.
    fn scope_with(
        table: &mut semantic_types::TyTable,
        values: &[(&str, &str)],
        types: &[(String, Type)],
    ) -> Arc<str> {
        let values = values
            .iter()
            .map(|(name, value)| (name.to_string(), ConstExpr::Literal(value.to_string())))
            .collect::<HashMap<_, _>>();
        module_scope(
            "Leaf",
            &ModuleParameterBindings {
                declarations_only: false,
                values: &values,
                types,
                patterns: &[],
                hierarchical: &[],
            },
            table,
        )
    }

    /// `logic [1:0]` as an enumeration declared by typedef `name`.
    fn enumeration(name: &str) -> Type {
        let mut ty = Type::new(TypeKind::Logic);
        ty.packed_ranges.push(PackedRange::new(
            ConstExpr::Literal("1".into()),
            ConstExpr::Literal("0".into()),
        ));
        ty.is_enum = true;
        ty.spelling.name(
            name.into(),
            type_names::TypedefName::Scope(name.into()),
            None,
        );
        ty
    }

    #[test]
    fn analyses_with_the_same_parameter_values_share_a_scope() {
        let table = &mut semantic_types::TyTable::default();
        let mut scope = |values: &[(&str, &str)]| scope_with(table, values, &[]);
        assert_eq!(
            scope(&[("W", "2"), ("D", "1")]),
            scope(&[("D", "1"), ("W", "2")])
        );
        assert_ne!(scope(&[("W", "2")]), scope(&[("W", "5")]));
        assert_ne!(scope(&[]), scope(&[("W", "2")]));
    }

    #[test]
    fn type_arguments_tell_scopes_apart_by_their_declarations() {
        // Two enumerations of one structure are two types (6.22.1 d), so
        // instances bound to them declare types of their own.
        let table = &mut semantic_types::TyTable::default();
        let first = enumeration("P::a_t");
        let second = enumeration("P::b_t");
        assert_eq!(first, second, "the fields do not tell them apart");
        assert_ne!(
            scope_with(table, &[], &[("T".into(), first.clone())]),
            scope_with(table, &[], &[("T".into(), second)])
        );
        assert_eq!(
            scope_with(table, &[], &[("T".into(), first.clone())]),
            scope_with(table, &[], &[("T".into(), first)])
        );
    }

    #[test]
    fn named_type_arguments_share_a_scope_in_any_order() {
        let table = &mut semantic_types::TyTable::default();
        let (a, b) = (enumeration("P::a_t"), enumeration("P::b_t"));
        assert_eq!(
            scope_with(
                table,
                &[],
                &[("S".into(), a.clone()), ("T".into(), b.clone())]
            ),
            scope_with(
                table,
                &[],
                &[("T".into(), b.clone()), ("S".into(), a.clone())]
            )
        );
        assert_ne!(
            scope_with(
                table,
                &[],
                &[("S".into(), a.clone()), ("T".into(), b.clone())]
            ),
            scope_with(table, &[], &[("S".into(), b), ("T".into(), a)])
        );
    }
}
