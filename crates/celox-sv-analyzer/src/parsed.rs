//! Reusable syntax and module metadata for hierarchy specialization.

use std::path::{Path, PathBuf};

use fxhash::FxHashMap as HashMap;

use crate::{AnalyzerError, Ir, ModuleInterfaces, Packages, analyze, ast, ir, syntax};

/// A parsed source that can analyze many modules or parameter specializations.
///
/// The syntax tree and module interfaces are built once. Module lookup follows
/// cached top-level declaration indices, without scanning unrelated bodies.
/// No elaborated AST is cached: overrides and constant-function environments
/// remain local to each analysis. Like `sv-parser`'s tree, this value is confined
/// to the parsing thread; consumers can share it there with [`std::rc::Rc`].
pub struct ParsedSource {
    code: String,
    path: PathBuf,
    tree: sv_parser::SyntaxTree,
    index: ast::module_index::ModuleIndex,
}

impl ParsedSource {
    pub fn parse(code: &str, path: &Path) -> Result<Self, AnalyzerError> {
        let tree = syntax::parse_source(code, path)?;
        let index = ast::with_call_sites(|| ast::module_index::ModuleIndex::new(&tree))?;
        Ok(Self {
            code: code.to_string(),
            path: path.to_path_buf(),
            tree,
            index,
        })
    }

    /// Bind assignment-pattern parameters with values frozen in the caller.
    pub fn with_pattern_parameter_overrides(
        &self,
        module_name: &str,
        overrides: &[(String, String)],
    ) -> Result<Option<Self>, AnalyzerError> {
        ast::type_parameters::apply_pattern_parameter_overrides(
            &self.code,
            &self.tree,
            module_name,
            overrides,
        )?
        .as_deref()
        .map(|code| Self::parse(code, &self.path))
        .transpose()
    }

    pub fn module_names(&self) -> &[String] {
        self.index.names()
    }

    /// Module and interface declarations share the SV design-element namespace.
    pub fn component_names(&self) -> fxhash::FxHashSet<String> {
        let mut names = self
            .module_names()
            .iter()
            .cloned()
            .collect::<fxhash::FxHashSet<_>>();
        for node in &self.tree {
            let identifier = match node {
                sv_parser::RefNode::InterfaceDeclarationAnsi(interface) => {
                    &interface.nodes.0.nodes.3
                }
                sv_parser::RefNode::InterfaceDeclarationNonansi(interface) => {
                    &interface.nodes.0.nodes.3
                }
                _ => continue,
            };
            if let Some(name) = ast::component_identifier(identifier, &self.tree) {
                names.insert(name);
            }
        }
        names
    }

    pub fn module_interfaces(&self) -> &ModuleInterfaces {
        &self.index.interfaces
    }

    /// Module and interface names referenced by instantiations and ports.
    pub fn component_references(&self) -> fxhash::FxHashSet<String> {
        ast::interfaces::component_references(&self.tree)
    }

    /// Analyze the packages declared in `sources`, each once, after the
    /// packages it depends on.
    pub fn analyze_packages(sources: &[&ParsedSource]) -> Result<Packages, AnalyzerError> {
        Self::analyze_packages_impl(sources, None)
    }

    /// Analyze packages and time queries using the roots selected by the
    /// hierarchy elaborator, rather than inferring roots from all sources.
    pub fn analyze_packages_with_roots(
        sources: &[&ParsedSource],
        roots: &[&str],
    ) -> Result<Packages, AnalyzerError> {
        Self::analyze_packages_impl(sources, Some(roots))
    }

    fn analyze_packages_impl(
        sources: &[&ParsedSource],
        roots: Option<&[&str]>,
    ) -> Result<Packages, AnalyzerError> {
        let trees: Vec<_> = sources
            .iter()
            .map(|source| (&source.tree, source.path.as_path()))
            .collect();
        ast::with_call_sites(|| Packages::analyze_with_roots(&trees, roots))
    }

    /// Analyze an instance using its value and caller-resolved type bindings.
    /// Every specialization reuses the original syntax tree.
    pub fn analyze_module(
        &self,
        module_name: &str,
        overrides: &HashMap<String, ir::ConstExpr>,
        type_overrides: &[(String, ast::Type)],
        interfaces: &ModuleInterfaces,
        packages: &Packages,
    ) -> Result<Ir, AnalyzerError> {
        self.analyze_module_with_packages(
            module_name,
            overrides,
            type_overrides,
            interfaces,
            packages,
        )
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
        self.analyze_module_with_packages(module_name, overrides, &[], interfaces, &packages)
    }

    fn analyze_module_with_packages(
        &self,
        module_name: &str,
        overrides: &HashMap<String, ir::ConstExpr>,
        type_overrides: &[(String, ast::Type)],
        interfaces: &ModuleInterfaces,
        packages: &Packages,
    ) -> Result<Ir, AnalyzerError> {
        let overrides = overrides
            .iter()
            .map(|(name, value)| (name.clone(), value.clone().into()))
            .collect();
        ast::with_call_sites(|| {
            let source = ast::Source::from_indexed_syntax_module(
                &self.tree,
                &self.path,
                &self.index,
                module_name,
                ast::ModuleParameterBindings {
                    values: &overrides,
                    types: type_overrides,
                },
                interfaces,
                packages,
            )?;
            analyze::analyze_source(source)
        })
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
            module Legacy(y); output y; endmodule
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
        let rebound = source
            .with_pattern_parameter_overrides("Top", &[("Q".into(), "'{-3, -5}".into())])
            .unwrap()
            .unwrap();
        assert!(rebound.code.contains("localparam int Q = 9;"));
        let ir = rebound
            .analyze_module_with_parameter_expr_overrides(
                "Top",
                &HashMap::default(),
                &ModuleInterfaces::default(),
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
            let pattern = instance.parameter_overrides()[0].pattern_text().unwrap();
            assert!(!pattern.contains('N'), "{pattern}");
            let child = ParsedSource::parse(
                "module Child #(parameter int N = 1, parameter byte Q [2] = '{1, 2}); endmodule",
                Path::new("child.sv"),
            )
            .unwrap();
            let rebound = child
                .with_pattern_parameter_overrides("Child", &[("Q".into(), pattern.into())])
                .unwrap()
                .unwrap();
            rebound
                .analyze_module_with_parameter_expr_overrides(
                    "Child",
                    &HashMap::default(),
                    &ModuleInterfaces::default(),
                )
                .unwrap();
        }
    }
}
