//! Reusable syntax and module metadata for hierarchy specialization.

use std::path::{Path, PathBuf};

use fxhash::FxHashMap as HashMap;

use crate::{AnalyzerError, Ir, ModuleInterfaces, Packages, analyze, ast, ir, source_tree};

/// A parsed source that can analyze many modules or parameter specializations.
///
/// The syntax tree and module interfaces are built once. Module lookup follows
/// cached top-level declaration indices, without scanning unrelated bodies.
/// No elaborated AST is cached: overrides and constant-function environments
/// remain local to each analysis. The private parsing context is confined to
/// its parsing thread; consumers can share it there with [`std::rc::Rc`].
pub struct ParsedSource {
    code: std::rc::Rc<str>,
    path: PathBuf,
    tree: std::rc::Rc<source_tree::SourceTree>,
    index: ast::module_index::ModuleIndex,
    /// Immutable declaration diagnostics, indexed once alongside module syntax.
    warnings: HashMap<String, Vec<ir::AnalyzerWarning>>,
}

/// What a module analysis lowers.
#[derive(Clone, Copy)]
enum ModuleAnalysis<'a> {
    /// The whole module, with the signals its hierarchical names denote.
    Full(&'a [ast::HierarchicalSignal]),
    DeclarationsOnly,
}

impl ParsedSource {
    pub fn parse(code: &str, path: &Path) -> Result<Self, AnalyzerError> {
        let tree = source_tree::parse_source(code, path)?;
        let index = ast::module_index::ModuleIndex::from_declarations(&tree, tree.declarations())?;
        let warnings = ast::warnings::components(&tree);
        Ok(Self {
            code: code.into(),
            path: path.to_path_buf(),
            tree: std::rc::Rc::new(tree),
            index,
            warnings,
        })
    }

    pub fn module_names(&self) -> &[String] {
        self.index.names()
    }

    /// Native declaration arenas with original token origins. Opaque body forms
    /// retain token ranges and are checked by semantic support boundaries.
    pub fn declarations(&self) -> &celox_sv_syntax::Declarations {
        self.tree.declarations()
    }

    /// Module and interface declarations share the SV design-element namespace.
    pub fn component_names(&self) -> fxhash::FxHashSet<String> {
        self.declarations()
            .all_units()
            .filter(|unit| {
                matches!(
                    unit.kind,
                    celox_sv_syntax::UnitKind::Module
                        | celox_sv_syntax::UnitKind::Program
                        | celox_sv_syntax::UnitKind::Interface
                ) && !unit.external
                    && unit.port_style != celox_sv_syntax::PortStyle::Wildcard
            })
            .map(|unit| unit.name.spelling.clone())
            .collect()
    }

    pub fn module_interfaces(&self) -> &ModuleInterfaces {
        &self.index.interfaces
    }

    /// Module and interface names referenced by instantiations and ports.
    pub fn component_references(&self) -> fxhash::FxHashSet<String> {
        let mut references = ast::interfaces::body_component_references(&self.tree);
        for unit in self.declarations().all_units() {
            references.extend(
                unit.interface_references(self.declarations())
                    .map(|name| name.spelling.clone()),
            );
        }
        references
    }

    /// Analyze the packages declared in `sources`, each once, after the
    /// packages it depends on.
    pub fn analyze_packages(sources: &[&ParsedSource]) -> Result<Packages, AnalyzerError> {
        Self::analyze_packages_with_options(sources, None, crate::AnalysisOptions::default())
    }

    /// Analyze packages and time queries using the roots selected by the
    /// hierarchy elaborator, rather than inferring roots from all sources.
    pub fn analyze_packages_with_roots(
        sources: &[&ParsedSource],
        roots: &[&str],
    ) -> Result<Packages, AnalyzerError> {
        Self::analyze_packages_with_options(sources, Some(roots), crate::AnalysisOptions::default())
    }

    /// Analyze a design with an explicit timescale compatibility policy.
    pub fn analyze_packages_with_options(
        sources: &[&ParsedSource],
        roots: Option<&[&str]>,
        options: crate::AnalysisOptions,
    ) -> Result<Packages, AnalyzerError> {
        let trees: Vec<_> = sources
            .iter()
            .map(|source| (source.tree.as_ref(), source.path.as_path()))
            .collect();
        let mut warnings = Vec::new();
        for source in sources {
            ast::warnings::append(&mut warnings, ast::warnings::packages(&source.tree));
        }
        let packages = (|| {
            // Module and interface names share a namespace regardless of
            // whether interface ports require a full binding plan. Inspect
            // declarations only, so unrelated interface bodies stay selective.
            let mut design_units = HashMap::default();
            for source in sources {
                for handle in source.tree.root_scopes()? {
                    let Some(unit) = handle.unit(source.tree.declarations()) else {
                        continue;
                    };
                    if !matches!(
                        unit.kind,
                        celox_sv_syntax::UnitKind::Module
                            | celox_sv_syntax::UnitKind::Program
                            | celox_sv_syntax::UnitKind::Interface
                    ) {
                        continue;
                    }
                    let name = ast::normalize_identifier(&unit.name.spelling);
                    if let Some(previous) = design_units.insert(name.clone(), unit.kind)
                        && previous != unit.kind
                    {
                        return Err(AnalyzerError::DuplicateModule { name });
                    }
                }
            }
            let mut packages = Packages::analyze_with_options(&trees, roots, options)?;
            packages.interface_names = design_units
                .into_iter()
                .filter_map(|(name, kind)| {
                    (kind == celox_sv_syntax::UnitKind::Interface).then_some(name)
                })
                .collect();
            ast::warnings::append(&mut warnings, packages.warnings().iter().cloned());
            for source in sources {
                for handle in source.tree.root_scopes()? {
                    let Some(unit) = handle.unit(source.tree.declarations()) else {
                        continue;
                    };
                    if unit.kind != celox_sv_syntax::UnitKind::Interface
                        || unit.port_style != celox_sv_syntax::PortStyle::Ansi
                    {
                        continue;
                    }
                    let name = ast::normalize_identifier(&unit.name.spelling);
                    if packages
                        .interface_sources
                        .insert(name.clone(), (source.tree.clone(), source.path.clone()))
                        .is_some()
                    {
                        return Err(AnalyzerError::DuplicateModule { name });
                    }
                }
            }
            if !packages.interface_sources.is_empty()
                && trees.iter().any(|(tree, _)| {
                    tree.declarations().all_units().any(|unit| {
                        unit.ports.iter().any(|port| {
                            port.data_type.is_some_and(|id| {
                                matches!(
                                    tree.declarations().data_type(id).kind,
                                    celox_sv_syntax::DataTypeKind::Interface { .. }
                                ) || (port.direction.is_none() && port.net_type.is_none() && !port.variable && matches!(&tree.declarations().data_type(id).kind,celox_sv_syntax::DataTypeKind::Named(path) if path.len()==1))
                            })
                        })
                    })
                })
            {
                let sources = sources
                    .iter()
                    .map(|source| (source.code.as_ref(), source.path.as_path()))
                    .collect::<Vec<_>>();
                packages.interface_plan =
                    std::sync::Arc::new(ast::interfaces::plan_interfaces(&sources, &[])?);
            }
            Ok(packages)
        })();
        match packages {
            Ok(packages) => Ok(packages.with_warnings(warnings)),
            Err(error) => Err(error.with_warnings(warnings)),
        }
    }

    /// Analyze an instance using its value and caller-resolved type bindings.
    /// Every specialization reuses the original syntax tree.
    pub fn analyze_module(
        &self,
        module_name: &str,
        overrides: &HashMap<String, ir::ConstExpr>,
        type_overrides: &[(String, ast::Type)],
        pattern_overrides: &[(String, ast::BoundPattern)],
        interfaces: &ModuleInterfaces,
        packages: &Packages,
    ) -> Result<Ir, AnalyzerError> {
        self.analyze_module_with_packages(
            module_name,
            overrides,
            type_overrides,
            pattern_overrides,
            interfaces,
            packages,
            ModuleAnalysis::Full(&[]),
        )
    }

    /// [`Self::analyze_module`] for a module whose hierarchical names
    /// denote `hierarchical`, signals of other instances.
    #[allow(clippy::too_many_arguments)]
    pub fn analyze_module_with_hierarchy(
        &self,
        module_name: &str,
        overrides: &HashMap<String, ir::ConstExpr>,
        type_overrides: &[(String, ast::Type)],
        pattern_overrides: &[(String, ast::BoundPattern)],
        interfaces: &ModuleInterfaces,
        packages: &Packages,
        hierarchical: &[ast::HierarchicalSignal],
    ) -> Result<Ir, AnalyzerError> {
        self.analyze_module_with_packages(
            module_name,
            overrides,
            type_overrides,
            pattern_overrides,
            interfaces,
            packages,
            ModuleAnalysis::Full(hierarchical),
        )
    }

    /// The declarations and instances of a specialization, without its
    /// procedures, for elaborating the hierarchy below a module before its
    /// hierarchical names are resolved.
    pub fn analyze_module_hierarchy(
        &self,
        module_name: &str,
        overrides: &HashMap<String, ir::ConstExpr>,
        type_overrides: &[(String, ast::Type)],
        pattern_overrides: &[(String, ast::BoundPattern)],
        interfaces: &ModuleInterfaces,
        packages: &Packages,
    ) -> Result<Ir, AnalyzerError> {
        self.analyze_module_with_packages(
            module_name,
            overrides,
            type_overrides,
            pattern_overrides,
            interfaces,
            packages,
            ModuleAnalysis::DeclarationsOnly,
        )
    }

    /// The dotted names `a.b.c` written in module `module_name`, the
    /// candidates for hierarchical names (IEEE 1800-2023 23.6), as their
    /// normalized components. A constant decimal index of a component other
    /// than the last stays part of it, as in `u[1]`. Callers decide which
    /// names denote another instance's signal. A specialization of a module
    /// with generic interface ports is scanned in its authored module.
    pub fn hierarchical_names(&self, module_name: &str, packages: &Packages) -> Vec<Vec<String>> {
        use celox_sv_syntax::TokenKind;
        let original = packages.authored_module_name(module_name);
        let mut names = Vec::new();
        for scope in self.index.scopes(original) {
            let Ok(unit) = scope.root_unit() else {
                continue;
            };
            let tokens = &scope.declarations().tokens;
            let significant: Vec<_> = tokens[unit.tokens.range()]
                .iter()
                .filter(|token| !token.is_trivia())
                .collect();
            let is_name = |index: usize| {
                significant.get(index).is_some_and(|token| {
                    matches!(
                        token.kind,
                        TokenKind::Identifier | TokenKind::EscapedIdentifier
                    ) || (token.kind == TokenKind::SystemIdentifier && token.text == "$root")
                })
            };
            let text = |index: usize| significant.get(index).map(|token| token.text.as_str());
            for start in 0..significant.len() {
                if !is_name(start)
                    || start
                        .checked_sub(1)
                        .and_then(text)
                        .is_some_and(|previous| previous == "." || previous == "::")
                {
                    continue;
                }
                let mut components = vec![ast::normalize_identifier(&significant[start].text)];
                let mut at = start + 1;
                loop {
                    if text(at) == Some("[")
                        && significant
                            .get(at + 1)
                            .is_some_and(|token| token.kind == TokenKind::Number)
                        && text(at + 2) == Some("]")
                        && text(at + 3) == Some(".")
                        && let Ok(index) = text(at + 1).unwrap().replace('_', "").parse::<u64>()
                    {
                        components
                            .last_mut()
                            .unwrap()
                            .push_str(&format!("[{index}]"));
                        at += 3;
                    }
                    if text(at) == Some(".") && is_name(at + 1) {
                        components.push(ast::normalize_identifier(&significant[at + 1].text));
                        at += 2;
                    } else {
                        break;
                    }
                }
                if components.len() > 1 && !names.contains(&components) {
                    names.push(components);
                }
            }
        }
        names
    }

    /// Inspect declaration shapes without lowering procedures or tasks.
    /// The returned IR is metadata for tooling, not an executable simulation.
    pub fn analyze_module_declarations(
        &self,
        module_name: &str,
        overrides: &HashMap<String, ir::ConstExpr>,
        packages: &Packages,
    ) -> Result<Ir, AnalyzerError> {
        self.validate_module_namespace(module_name, packages)?;
        let overrides = overrides
            .iter()
            .map(|(name, value)| (name.clone(), value.clone().into()))
            .collect();
        let source = ast::Source::from_indexed_syntax_module(
            &self.tree,
            &self.path,
            &self.index,
            module_name,
            ast::ModuleParameterBindings {
                declarations_only: true,
                values: &overrides,
                types: &[],
                patterns: &[],
                hierarchical: &[],
            },
            &ModuleInterfaces::default(),
            packages,
        )?;
        analyze::analyze_source(source)
    }

    /// Analyze without rewriting the source, retaining literal override
    /// types. The module may use the packages this source declares.
    pub fn analyze_module_with_parameter_expr_overrides(
        &self,
        module_name: &str,
        overrides: &HashMap<String, ir::ConstExpr>,
        interfaces: &ModuleInterfaces,
    ) -> Result<Ir, AnalyzerError> {
        let packages = Self::analyze_packages(&[self])?;
        self.analyze_module_with_packages(
            module_name,
            overrides,
            &[],
            &[],
            interfaces,
            &packages,
            ModuleAnalysis::Full(&[]),
        )
    }

    fn validate_module_namespace(
        &self,
        module_name: &str,
        packages: &Packages,
    ) -> Result<(), AnalyzerError> {
        let name = ast::normalize_identifier(module_name);
        if packages.interface_names.contains(&name)
            && self
                .index
                .names()
                .iter()
                .any(|module| ast::normalize_identifier(module) == name)
        {
            return Err(
                AnalyzerError::DuplicateModule { name }.with_warnings(packages.warnings().to_vec())
            );
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn analyze_module_with_packages(
        &self,
        module_name: &str,
        overrides: &HashMap<String, ir::ConstExpr>,
        type_overrides: &[(String, ast::Type)],
        pattern_overrides: &[(String, ast::BoundPattern)],
        interfaces: &ModuleInterfaces,
        packages: &Packages,
        analysis: ModuleAnalysis<'_>,
    ) -> Result<Ir, AnalyzerError> {
        let overrides = overrides
            .iter()
            .map(|(name, value)| (name.clone(), value.clone().into()))
            .collect();
        let original = packages.authored_module_name(module_name);
        let warnings = self.warnings.get(original).cloned().unwrap_or_default();
        let ir = (|| {
            self.validate_module_namespace(original, packages)?;
            let source = ast::Source::from_indexed_syntax_module(
                &self.tree,
                &self.path,
                &self.index,
                module_name,
                ast::ModuleParameterBindings {
                    declarations_only: matches!(analysis, ModuleAnalysis::DeclarationsOnly),
                    values: &overrides,
                    types: type_overrides,
                    patterns: pattern_overrides,
                    hierarchical: match analysis {
                        ModuleAnalysis::Full(hierarchical) => hierarchical,
                        ModuleAnalysis::DeclarationsOnly => &[],
                    },
                },
                interfaces,
                packages,
            )?;
            analyze::analyze_source(source)
        })();
        crate::finish_analysis(ir, warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_binding(text: &str) -> ast::Type {
        let code = format!("module Caller; Child #(.T({text})) c(); endmodule");
        let ir = crate::analyze_source(&code, Path::new("caller.sv")).unwrap();
        ir.modules()[0].instances()[0].parameter_overrides()[0]
            .bound_type()
            .unwrap()
            .clone()
    }

    #[test]
    fn named_type_arguments_preserve_use_site_dimensions() {
        let code = r#"
            module Top;
                typedef logic signed [3:0] nibble_t;
                localparam N = 2;
                Child #(.T(nibble_t [N-1:0])) one_dimension();
            endmodule
        "#;
        let ir = crate::analyze_source(code, Path::new("named_type_dimensions.sv")).unwrap();
        let ty = ir.modules()[0].instances()[0].parameter_overrides()[0]
            .bound_type()
            .unwrap();
        assert_eq!(ty.packed_ranges().len(), 2);
        for (range, left) in ty.packed_ranges().iter().zip(["1", "3"]) {
            assert_eq!(range.left(), &ast::ConstExpr::Literal(left.into()));
            assert_eq!(range.right(), &ast::ConstExpr::Literal("0".into()));
        }
        assert!(!ty.is_signed());
    }

    #[test]
    fn specializations_keep_constants_and_function_bindings_independent() {
        let code = r#"
            module A #(parameter N = 2) (output logic [N-1:0] y);
                function automatic int f(); return N; endfunction
                for (genvar i = 0; i < f(); i++) begin : g
                    logic [N-1:0] s;
                    assign s = N;
                end
                assign y = N;
            endmodule
            module B(output logic [7:0] y);
                function automatic int f(); return 7; endfunction
                localparam V = f();
                assign y = V;
            endmodule
        "#;
        let path = Path::new("specializations.sv");
        let source = ParsedSource::parse(code, path).unwrap();
        for n in [2, 5, 1, 2] {
            let overrides =
                HashMap::from_iter([("N".into(), ir::ConstExpr::Literal(n.to_string()))]);
            let actual = source
                .analyze_module_with_parameter_expr_overrides(
                    "A",
                    &overrides,
                    &ModuleInterfaces::default(),
                )
                .unwrap();
            let fresh = crate::analyze_source_with_module_parameter_overrides(
                code,
                path,
                "A",
                &HashMap::from_iter([("N".into(), n)]),
            )
            .unwrap();
            let expected = &fresh.modules()[0];
            let actual = &actual.modules()[0];
            assert_eq!(actual.parameters(), expected.parameters());
            assert_eq!(actual.ports(), expected.ports());
            assert_eq!(actual.signals(), expected.signals());
            assert_eq!(actual.comb_processes(), expected.comb_processes());
            assert_eq!(actual.signals().len(), n as usize);
            let b = source
                .analyze_module_with_parameter_expr_overrides(
                    "B",
                    &HashMap::default(),
                    &ModuleInterfaces::default(),
                )
                .unwrap();
            assert_eq!(b.modules()[0].parameters()[0].resolved_value(), Some(7));
        }
    }

    #[test]
    fn local_interfaces_override_external_ones_and_other_sources_bind_positionally() {
        let source = ParsedSource::parse(
            r#"
            module Child(input logic a); endmodule
            module Top(input logic x);
                Child c(x);
                External #(3) e(x);
            endmodule
        "#,
            Path::new("interfaces.sv"),
        )
        .unwrap();
        let mut interfaces = HashMap::default();
        interfaces.insert(
            "Child".into(),
            crate::ModuleInterface {
                ports: vec!["wrong".into()],
                parameters: Vec::new(),
            },
        );
        interfaces.insert(
            "External".into(),
            crate::ModuleInterface {
                ports: vec!["ext".into()],
                parameters: vec!["N".into()],
            },
        );
        let ir = source
            .analyze_module_with_parameter_expr_overrides("Top", &HashMap::default(), &interfaces)
            .unwrap();
        let instances = ir.modules()[0].instances();
        assert_eq!(instances[0].port_connections()[0].formal(), "a");
        assert_eq!(instances[1].port_connections()[0].formal(), "ext");
        assert_eq!(instances[1].parameter_overrides()[0].name(), "N");
    }

    #[test]
    fn binds_type_parameters_and_resolves_packages() {
        let code = r#"
            package p; localparam V = 3; endpackage
            module Top #(parameter type T = logic [3:0]) (output T y);
                import p::*;
                assign y = V;
            endmodule
        "#;
        let path = Path::new("rewritten.sv");
        let source = ParsedSource::parse(code, path).unwrap();
        let packages = ParsedSource::analyze_packages(&[&source]).unwrap();
        let types = vec![("T".into(), type_binding("logic [7:0]"))];
        let actual = source
            .analyze_module(
                "Top",
                &HashMap::default(),
                &types,
                &[],
                &ModuleInterfaces::default(),
                &packages,
            )
            .unwrap();
        let typed = code.replace("logic [3:0]", "logic [7:0]");
        let expected = crate::analyze_source(&typed, path).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            actual.modules()[0].ports()[0].r#type().resolved_width(),
            Some(8)
        );
    }

    #[test]
    fn type_bindings_preserve_dependent_defaults_and_instance_isolation() {
        let code = r#"
            module Child #(parameter type T = logic [1:0], parameter type U = T)
                (output T first, output U second, output logic [7:0] width);
                typedef U alias_t;
                function automatic alias_t value(); return '1; endfunction
                if ($bits(T) > 4) begin : wide
                    T local_value;
                    assign local_value = '1;
                end
                assign first = value();
                assign second = value();
                assign width = $bits(T);
            endmodule
        "#;
        let source = ParsedSource::parse(code, Path::new("type_bindings.sv")).unwrap();
        let packages = ParsedSource::analyze_packages(&[&source]).unwrap();
        for width in [8, 4, 8, 2] {
            let bindings = if width == 2 {
                Vec::new()
            } else {
                vec![(
                    "T".into(),
                    type_binding(&format!("logic [{}:0]", width - 1)),
                )]
            };
            let ir = source
                .analyze_module(
                    "Child",
                    &HashMap::default(),
                    &bindings,
                    &[],
                    &ModuleInterfaces::default(),
                    &packages,
                )
                .unwrap();
            let module = &ir.modules()[0];
            assert_eq!(module.ports()[0].r#type().resolved_width(), Some(width));
            assert_eq!(module.ports()[1].r#type().resolved_width(), Some(width));
            if width > 4 {
                let signal = module
                    .signals()
                    .iter()
                    .find(|s| s.name().ends_with("local_value"))
                    .unwrap();
                assert_eq!(signal.r#type().resolved_width(), Some(width));
            }
        }
    }

    #[test]
    fn type_bindings_support_shorthand_and_missing_defaults() {
        let source = ParsedSource::parse(
            "module Child #(type T)(output T y); assign y='1; endmodule",
            Path::new("required_type.sv"),
        )
        .unwrap();
        let packages = ParsedSource::analyze_packages(&[&source]).unwrap();
        let ir = source
            .analyze_module(
                "Child",
                &HashMap::default(),
                &[("T".into(), type_binding("bit signed [7:0]"))],
                &[],
                &ModuleInterfaces::default(),
                &packages,
            )
            .unwrap();
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(8)
        );
        assert!(ir.modules()[0].ports()[0].r#type().is_signed());
        assert_eq!(source.module_interfaces()["Child"].parameters, ["T"]);
    }

    #[test]
    fn type_bindings_reject_unknown_local_and_duplicate_parameters() {
        let source = ParsedSource::parse(
            "module Child #(parameter type T = logic, localparam type L = logic, type M = logic, parameter type N = logic)(output T y); parameter type B = logic; assign y='1; endmodule",
            Path::new("invalid_bindings.sv"),
        ).unwrap();
        let packages = ParsedSource::analyze_packages(&[&source]).unwrap();
        let ty = type_binding("logic [7:0]");
        assert_eq!(source.module_interfaces()["Child"].parameters, ["T", "N"]);
        for bindings in [
            vec![("missing".into(), ty.clone())],
            vec![("L".into(), ty.clone())],
            vec![("M".into(), ty.clone())],
            vec![("B".into(), ty.clone())],
            vec![("T".into(), ty.clone()), ("T".into(), ty.clone())],
        ] {
            assert!(
                source
                    .analyze_module(
                        "Child",
                        &HashMap::default(),
                        &bindings,
                        &[],
                        &ModuleInterfaces::default(),
                        &packages
                    )
                    .is_err()
            );
        }
        let ir = source
            .analyze_module(
                "Child",
                &HashMap::default(),
                &[],
                &[],
                &ModuleInterfaces::default(),
                &packages,
            )
            .unwrap();
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(1)
        );
    }

    #[test]
    fn indexed_lookup_preserves_nested_escaped_and_duplicate_modules() {
        let source = ParsedSource::parse(
            r#"
            module Outer;
                module Inner(output logic y); assign y = 1'b1; endmodule
            endmodule
            module \escaped.name (output logic y); assign y = 1'b0; endmodule
            module Duplicate; endmodule
            module Duplicate; endmodule
            module Legacy({y}); output y; endmodule
        "#,
            Path::new("index.sv"),
        )
        .unwrap();
        assert_eq!(
            source.module_names(),
            [
                "Outer",
                "Inner",
                "\\escaped.name",
                "Duplicate",
                "Duplicate",
                "Legacy"
            ]
        );
        for name in ["Inner", "\\escaped.name"] {
            let ir = source
                .analyze_module_with_parameter_expr_overrides(
                    name,
                    &HashMap::default(),
                    &ModuleInterfaces::default(),
                )
                .unwrap();
            assert_eq!(ir.modules()[0].name(), name);
        }
        assert!(matches!(
            source.analyze_module_with_parameter_expr_overrides(
                "Duplicate",
                &HashMap::default(),
                &ModuleInterfaces::default(),
            ),
            Err(AnalyzerError::DuplicateModule { .. })
        ));
        assert!(matches!(
            source.analyze_module_with_parameter_expr_overrides(
                "Legacy",
                &HashMap::default(),
                &ModuleInterfaces::default(),
            ),
            Err(AnalyzerError::Unsupported(_))
        ));
        assert!(
            source
                .analyze_module_with_parameter_expr_overrides(
                    "Missing",
                    &HashMap::default(),
                    &ModuleInterfaces::default(),
                )
                .unwrap()
                .modules()
                .is_empty()
        );
    }
}

#[cfg(test)]
mod pattern_override_tests {
    use super::*;

    #[test]
    fn pattern_specialization_preserves_shadowed_local_parameter_declarations() {
        let source = ParsedSource::parse(
            r#"
                module Top #(parameter byte Q [2] = '{1, 2})(output int y);
                    always_comb begin
                        localparam int Q = 9;
                        y = Q;
                    end
                endmodule
            "#,
            Path::new("pattern_scope.sv"),
        )
        .unwrap();
        let caller = crate::analyze_source(
            "module Caller; Top #(.Q('{-3, -5})) t(); endmodule",
            Path::new("caller.sv"),
        )
        .unwrap();
        let bound = caller.modules()[0].instances()[0].parameter_overrides()[0]
            .bound_pattern()
            .unwrap()
            .clone();
        let packages = ParsedSource::analyze_packages(&[&source]).unwrap();
        let ir = source
            .analyze_module(
                "Top",
                &HashMap::default(),
                &[],
                &[("Q".into(), bound)],
                &ModuleInterfaces::default(),
                &packages,
            )
            .unwrap();
        assert_eq!(ir.modules()[0].initial_processes().len(), 1);
        assert_eq!(
            ir.modules()[0].signals()[0].r#type().resolved_width(),
            Some(16)
        );
    }

    #[test]
    fn pattern_override_leaves_counts_and_keys_use_the_callers_scope() {
        let ir = crate::analyze_source(
            r#"
                module Child #(parameter int N = 1, parameter byte Q [2] = '{1, 2});
                endmodule
                module Top #(parameter int N = 2);
                    Child #(.Q('{N{-N}})) repeated_values();
                    Child #(.Q('{N-2: -N, default: N+1})) keyed_values();
                endmodule
            "#,
            Path::new("pattern_caller.sv"),
        )
        .unwrap();
        let top = ir
            .modules()
            .iter()
            .find(|module| module.name() == "Top")
            .unwrap();
        for instance in top.instances() {
            let pattern = instance.parameter_overrides()[0].bound_pattern().unwrap();
            let child = ParsedSource::parse(
                "module Child #(parameter int N = 1, parameter byte Q [2] = '{1, 2}); endmodule",
                Path::new("child.sv"),
            )
            .unwrap();
            let packages = ParsedSource::analyze_packages(&[&child]).unwrap();
            child
                .analyze_module(
                    "Child",
                    &HashMap::default(),
                    &[],
                    &[("Q".into(), pattern.clone())],
                    &ModuleInterfaces::default(),
                    &packages,
                )
                .unwrap();
        }
    }
}

#[cfg(test)]
mod native_interface_tests {
    use super::*;

    #[test]
    fn escaped_interface_handles_do_not_match_type_keywords() {
        let source = ParsedSource::parse(
            "interface I;logic d;endinterface module Child(I p);endmodule module Top;logic ordinary;if(1)begin:g I \\logic ();Child u(.p(\\logic ));end endmodule",
            Path::new("native-keyword-interface-handle.sv"),
        ).unwrap();
        source
            .analyze_module_with_parameter_expr_overrides(
                "Top",
                &HashMap::default(),
                &ModuleInterfaces::default(),
            )
            .unwrap();
    }

    #[test]
    fn escaped_interface_names_keep_native_binding_identity() {
        let source = ParsedSource::parse(
            "interface \\Bus.Name ;logic d;endinterface module Top;\\Bus.Name h();endmodule",
            Path::new("native-escaped-interface.sv"),
        )
        .unwrap();
        let ir = source
            .analyze_module_with_parameter_expr_overrides(
                "Top",
                &HashMap::default(),
                &ModuleInterfaces::default(),
            )
            .unwrap();
        assert!(ir.modules()[0].instances().is_empty());
        assert_eq!(ir.modules()[0].signals()[0].name(), "h$d");
    }

    #[test]
    fn binds_generated_interface_instances_in_their_own_scopes() {
        let source = ParsedSource::parse(
            r#"
            interface Bus #(parameter W = 4);
                timeunit 1ns / 1ps;
                logic [W-1:0] value;
            endinterface
            module Child(Bus bus);
                timeunit 1us / 1ns;
                logic [7:0] copy;
                assign copy = bus.value;
            endmodule
            module Top(output int u);
                timeunit 1us / 1ns;
                for (genvar i=0; i<2; i++) begin : g
                    Bus #(.W(i+3)) local_info();
                    assign local_info.value = i;
                    Child child(.bus(local_info));
                end
                if (1) begin : h
                    Bus local_info();
                    assign local_info.value = 1;
                    assign u = $timeunit(local_info);
                end
            endmodule
            "#,
            Path::new("native_generated_interface.sv"),
        )
        .unwrap();
        let ir = source
            .analyze_module_with_parameter_expr_overrides(
                "Top",
                &HashMap::default(),
                &ModuleInterfaces::default(),
            )
            .unwrap();
        let top = &ir.modules()[0];
        assert_eq!(top.instances().len(), 2);
        for (index, instance) in top.instances().iter().enumerate() {
            assert_eq!(
                instance.port_connections()[0].actual_expr(),
                Some(&ir::Expr::Ident(format!("g[{index}].local_info$value")))
            );
        }
        for (name, width) in [
            ("g[0].local_info$value", 3),
            ("g[1].local_info$value", 4),
            ("h.local_info$value", 4),
        ] {
            assert_eq!(
                top.signals()
                    .iter()
                    .find(|signal| signal.name() == name)
                    .unwrap()
                    .r#type()
                    .resolved_width(),
                Some(width)
            );
        }
        assert_eq!(top.assignments().len(), 4);
    }

    #[test]
    fn resolves_interface_parameters_in_array_and_select_bounds() {
        let source = ParsedSource::parse(
            r#"
            interface Cfg #(parameter N = 3); endinterface
            interface Lane; logic [3:0] data; endinterface
            module Top(input logic [7:0] a, output logic [3:0] y);
                Cfg cfg();
                Lane rows[cfg.N]();
                assign y = a[cfg.N:0];
            endmodule
            "#,
            Path::new("native_interface_bounds.sv"),
        )
        .unwrap();
        let ir = source
            .analyze_module_with_parameter_expr_overrides(
                "Top",
                &HashMap::default(),
                &ModuleInterfaces::default(),
            )
            .unwrap();
        let top = &ir.modules()[0];
        assert_eq!(top.signals().len(), 3);
        assert_eq!(top.signals()[2].name(), "rows[2]$data");
        assert_eq!(top.assignments().len(), 1);
    }

    #[test]
    fn keeps_interface_timescales_and_instance_names_in_original_syntax() {
        let source = ParsedSource::parse(
            r#"
            `define UNIT $timeunit()
            interface ClockInfo;
                timeunit 100ps / 10fs;
                localparam U = `UNIT;
                initial $printtimescale;
            endinterface
            module Top;
                timeunit 1ms / 1us;
                ClockInfo info[1:2]();
                localparam U = `UNIT;
            endmodule
            "#,
            Path::new("native_interface_timescales.sv"),
        )
        .unwrap();
        let ir = source
            .analyze_module_with_parameter_expr_overrides(
                "Top",
                &HashMap::default(),
                &ModuleInterfaces::default(),
            )
            .unwrap();
        let top = &ir.modules()[0];
        assert_eq!(
            top.parameters()
                .iter()
                .find(|p| p.name() == "U")
                .unwrap()
                .resolved_value(),
            Some(-3)
        );
        let messages = top
            .initial_processes()
            .iter()
            .flat_map(|process| process.body())
            .filter_map(|statement| {
                if let ir::Stmt::SystemTask { args, .. } = statement
                    && let Some(ir::SystemTaskArg::Str(message)) = args.first()
                {
                    Some(message.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            [
                "Time scale of (%m.info[1]) is 100ps / 10fs",
                "Time scale of (%m.info[2]) is 100ps / 10fs",
            ]
        );
    }

    #[test]
    fn binds_generic_child_interfaces_without_cloning_source() {
        let source = ParsedSource::parse(
            r#"
            interface Bus #(parameter W = 4);
                logic [W-1:0] request, response;
            endinterface
            module Child(interface bus);
                assign bus.response = bus.request + 1;
            endmodule
            module Top(input logic [7:0] a, output logic [7:0] y);
                Bus #(.W(8)) bus();
                Child child(.bus(bus));
                assign bus.request = a;
                assign y = bus.response;
            endmodule
        "#,
            Path::new("generic.sv"),
        )
        .unwrap();
        let packages = ParsedSource::analyze_packages(&[&source]).unwrap();
        let ir = source
            .analyze_module(
                "Top",
                &HashMap::default(),
                &[],
                &[],
                &ModuleInterfaces::default(),
                &packages,
            )
            .unwrap();
        let instance = &ir.modules()[0].instances()[0];
        assert_eq!(instance.port_connections().len(), 2);
        let overrides = instance
            .parameter_overrides()
            .iter()
            .map(|parameter| {
                (
                    parameter.name().to_string(),
                    parameter.value().unwrap().clone(),
                )
            })
            .collect();
        let child = source
            .analyze_module(
                instance.module_name(),
                &overrides,
                &[],
                &[],
                &ModuleInterfaces::default(),
                &packages,
            )
            .unwrap();
        assert_eq!(child.modules()[0].name(), instance.module_name());
        assert_eq!(child.modules()[0].ports().len(), 2);
        assert!(
            child.modules()[0]
                .ports()
                .iter()
                .all(|port| port.r#type().resolved_width() == Some(8))
        );
    }

    #[test]
    fn binds_modport_members_and_parameter_widths_without_source_elaboration() {
        let source = ParsedSource::parse(
            r#"
            interface Bus #(parameter W = 4);
                logic [W-1:0] request, response;
                modport slave(input request, output response);
            endinterface
            module Child(Bus.slave bus);
                assign bus.response = bus.request + 1;
            endmodule
        "#,
            Path::new("modport.sv"),
        )
        .unwrap();
        let overrides =
            HashMap::from_iter([("bus$W".to_string(), ir::ConstExpr::Literal("8".to_string()))]);
        let ir = source
            .analyze_module_with_parameter_expr_overrides(
                "Child",
                &overrides,
                &ModuleInterfaces::default(),
            )
            .unwrap();
        let module = &ir.modules()[0];
        assert_eq!(module.ports().len(), 2);
        assert!(module.signals().is_empty());
        assert_eq!(module.ports()[0].name(), "bus$request");
        assert_eq!(module.ports()[0].r#type().resolved_width(), Some(8));
        assert_eq!(module.ports()[0].direction(), ir::PortDirection::Input);
        assert_eq!(module.ports()[1].direction(), ir::PortDirection::Output);
    }

    #[test]
    fn binds_interface_processes_from_another_compilation_unit() {
        let bus = ParsedSource::parse(
            r#"
            interface Bus #(parameter W = 4)(input logic clk);
                logic [W-1:0] request, response;
                initial response = 1;
                always_ff @(posedge clk) response <= request + 1;
            endinterface
        "#,
            Path::new("bus.sv"),
        )
        .unwrap();
        let top = ParsedSource::parse(
            r#"
            module Top(input logic clk, input logic [7:0] a, output logic [7:0] y);
                Bus #(.W(8)) bus(.clk(clk));
                assign bus.request = a;
                assign y = bus.response;
            endmodule
        "#,
            Path::new("top.sv"),
        )
        .unwrap();
        let packages = ParsedSource::analyze_packages(&[&bus, &top]).unwrap();
        let ir = top
            .analyze_module(
                "Top",
                &HashMap::default(),
                &[],
                &[],
                &ModuleInterfaces::default(),
                &packages,
            )
            .unwrap();
        let module = &ir.modules()[0];
        assert!(module.instances().is_empty());
        assert_eq!(module.ff_processes().len(), 1);
        assert_eq!(module.ff_processes()[0].events()[0].signal(), "bus$clk");
        assert_eq!(module.initial_processes().len(), 1);
        assert_eq!(module.comb_processes().len(), 3);
    }

    #[test]
    fn binds_interface_instances_without_source_elaboration() {
        let source = ParsedSource::parse(
            r#"
            interface Bus #(parameter W = 4);
                logic [W-1:0] request, response;
                assign response = request + 1;
            endinterface
            module Top #(parameter N = 8)(input logic [N-1:0] a, output logic [N-1:0] y);
                Bus #(.W(N)) bus();
                assign bus.request = a;
                assign y = bus.response;
            endmodule
        "#,
            Path::new("native_interface.sv"),
        )
        .unwrap();
        let ir = source
            .analyze_module_with_parameter_expr_overrides(
                "Top",
                &HashMap::default(),
                &ModuleInterfaces::default(),
            )
            .unwrap();
        let top = &ir.modules()[0];
        assert!(top.instances().is_empty());
        assert_eq!(
            top.signals()
                .iter()
                .find(|signal| signal.name() == "bus$request")
                .unwrap()
                .r#type()
                .resolved_width(),
            Some(8)
        );
        assert_eq!(top.comb_processes().len(), 3);
    }
}

#[cfg(test)]
mod declaration_tests {
    use super::*;

    #[test]
    fn native_headers_supply_metadata_and_original_type_syntax() {
        let code = "`define WIDTH 7\ninterface Bus; endinterface module M #(parameter type T=logic[`WIDTH:0],localparam type L=int,type K=int,parameter N=2)(input T a,b,Bus.master bus); parameter BODY=3; endmodule";
        let source = ParsedSource::parse(code, Path::new("native_headers.sv")).unwrap();
        let interface = &source.module_interfaces()["M"];
        assert_eq!(interface.ports, ["a", "b", "bus"]);
        assert_eq!(interface.parameters, ["T", "N"]);
        assert_eq!(
            source.component_names(),
            fxhash::FxHashSet::from_iter(["M".into(), "Bus".into()])
        );
        assert!(source.component_references().contains("Bus"));
        let unit = source
            .declarations()
            .all_units()
            .find(|unit| unit.name.value() == "M")
            .unwrap();
        let celox_sv_syntax::ParameterKind::Type {
            default: Some(data_type),
        } = &unit.parameters.as_ref().unwrap()[0].kind
        else {
            panic!()
        };
        let tokens = &source.declarations().tokens
            [source.declarations().data_type(*data_type).tokens.range()];
        assert!(
            tokens
                .iter()
                .any(|token| token.text == "7" && token.origin.expansion.is_some())
        );
        assert!(unit.body_parameters[0].local);
    }

    #[test]
    fn native_header_success_does_not_accept_a_malformed_body() {
        let code = "module M #(parameter int N=1)(input logic a); and g(a,,a); endmodule";
        assert!(celox_sv_syntax::Declarations::parse(code, Path::new("invalid_body.sv")).is_err());
        assert!(matches!(
            ParsedSource::parse(code, Path::new("invalid_body.sv")),
            Err(AnalyzerError::Parse(_))
        ));
        assert!(source_tree::parse_source(code, Path::new("invalid_body.sv")).is_err());
    }

    #[test]
    fn body_type_parameter_metadata_preserves_the_existing_binding_contract() {
        let source = ParsedSource::parse(
            "module M; parameter type T=logic; parameter int N=3; endmodule",
            Path::new("body_types.sv"),
        )
        .unwrap();
        assert_eq!(source.module_interfaces()["M"].parameters, ["N"]);
        assert_eq!(
            source.declarations().units[0]
                .overridable_parameters()
                .map(|parameter| parameter.name.value())
                .collect::<Vec<_>>(),
            ["T", "N"]
        );
    }

    #[test]
    fn inspects_timed_module_shapes_without_editing_procedure_tokens() {
        let source = ParsedSource::parse(
            r#"
            module Top #(parameter W = 3)(input logic [2*W-1:0] a, output logic [W-1:0] y);
                logic [W-1:0] memory[2];
                wire unused;
                task automatic delayed(); #2 y = a; endtask
                initial forever begin #1; delayed(); end
                final $display("done");
            endmodule
            "#,
            Path::new("timed_declarations.sv"),
        )
        .unwrap();
        let packages = ParsedSource::analyze_packages(&[&source]).unwrap();
        let overrides =
            HashMap::from_iter([("W".to_string(), ir::ConstExpr::Literal("9".to_string()))]);
        assert!(
            source
                .analyze_module(
                    "Top",
                    &overrides,
                    &[],
                    &[],
                    &ModuleInterfaces::default(),
                    &packages
                )
                .is_ok()
        );
        let ir = source
            .analyze_module_declarations("Top", &overrides, &packages)
            .unwrap();
        let module = &ir.modules()[0];
        assert_eq!(module.ports()[0].r#type().resolved_width(), Some(18));
        assert_eq!(module.ports()[1].r#type().resolved_width(), Some(9));
        assert_eq!(
            module
                .signals()
                .iter()
                .find(|s| s.name() == "memory")
                .unwrap()
                .r#type()
                .resolved_width(),
            Some(18)
        );
        assert!(module.initial_processes().is_empty());
        assert!(module.ff_processes().is_empty());
        assert!(module.comb_processes().is_empty());
    }

    #[test]
    fn inspects_interfaces_with_timed_processes_from_original_declarations() {
        let source = ParsedSource::parse(
            r#"
            interface Bus #(parameter W = 4);
                logic [W-1:0] value;
                initial forever #1 value = value + 1;
            endinterface
            module Top(output logic [7:0] y);
                Bus #(.W(8)) bus();
                assign y = bus.value;
            endmodule
            "#,
            Path::new("timed_interface_declarations.sv"),
        )
        .unwrap();
        let packages = ParsedSource::analyze_packages(&[&source]).unwrap();
        let ir = source
            .analyze_module_declarations("Top", &HashMap::default(), &packages)
            .unwrap();
        let module = &ir.modules()[0];
        assert_eq!(module.ports()[0].r#type().resolved_width(), Some(8));
        assert_eq!(
            module
                .signals()
                .iter()
                .find(|s| s.name() == "bus$value")
                .unwrap()
                .r#type()
                .resolved_width(),
            Some(8)
        );
        assert!(module.initial_processes().is_empty());
    }
}

#[cfg(test)]
mod shared_namespace_tests {
    use super::*;

    #[test]
    fn rejects_module_interface_name_collisions_without_interface_ports() {
        for (first, second) in [
            ("interface I; endinterface", "module I; endmodule"),
            ("module I; endmodule", "interface I; endinterface"),
            (
                "interface I; endinterface",
                "module I(y); output y; endmodule",
            ),
        ] {
            for split in [false, true] {
                let text = format!(
                    "{first} {} module Top; I i(); endmodule",
                    if split { "" } else { second }
                );
                let a = ParsedSource::parse(&text, Path::new("namespace-a.sv")).unwrap();
                let b = ParsedSource::parse(second, Path::new("namespace-b.sv")).unwrap();
                let sources = if split { vec![&a, &b] } else { vec![&a] };
                assert!(
                    matches!(ParsedSource::analyze_packages(&sources),
                    Err(AnalyzerError::DuplicateModule { name }) if name == "I"),
                    "accepted conflicting design-unit names: {first}; {second}; split={split}"
                );
            }
        }
    }

    #[test]
    fn namespace_collision_preserves_package_warnings() {
        for function in [
            "package P; function int f(); $exit; return 0; endfunction endpackage",
            "function int f(); $exit; return 0; endfunction",
        ] {
            let source = ParsedSource::parse(
                &format!("{function} interface I; endinterface module I; endmodule"),
                Path::new("namespace-warnings.sv"),
            )
            .unwrap();
            let error = ParsedSource::analyze_packages(&[&source]).unwrap_err();
            assert!(error.to_string().contains("I"));
            assert_eq!(error.warnings().len(), 1, "{error:?}");
        }
    }

    #[test]
    fn rejects_module_collision_with_prebuilt_interface_packages() {
        for interface in [
            "interface I; endinterface",
            "interface I(a); input a; endinterface",
        ] {
            let source = ParsedSource::parse(interface, Path::new("interface.sv")).unwrap();
            let packages = ParsedSource::analyze_packages(&[&source]).unwrap();
            for module in ["module I; endmodule", "module I(y); output y; endmodule"] {
                let error = crate::analyze_source_module_with_packages(
                    module,
                    Path::new("module.sv"),
                    "I",
                    &HashMap::default(),
                    &packages,
                )
                .unwrap_err();
                assert!(matches!(error, AnalyzerError::DuplicateModule { name } if name == "I"));
                let parsed = ParsedSource::parse(module, Path::new("module.sv")).unwrap();
                assert!(
                    matches!(parsed.analyze_module_declarations("I", &HashMap::default(), &packages),
                Err(AnalyzerError::DuplicateModule { name }) if name == "I")
                );
            }
            // Namespace metadata must not eagerly analyze the non-ANSI body.
            crate::analyze_source_module_with_packages(
                "module Top; endmodule",
                Path::new("top.sv"),
                "Top",
                &HashMap::default(),
                &packages,
            )
            .unwrap();
        }
    }

    #[test]
    fn namespace_validation_does_not_analyze_unused_interface_semantics() {
        let source = ParsedSource::parse(
            "interface Unused; real unsupported_value; endinterface module Top(output logic y); assign y = 1; endmodule",
            Path::new("unused-interface.sv"),
        ).unwrap();
        let packages = ParsedSource::analyze_packages(&[&source]).unwrap();
        source
            .analyze_module(
                "Top",
                &HashMap::default(),
                &[],
                &[],
                &ModuleInterfaces::default(),
                &packages,
            )
            .unwrap();
    }
}
