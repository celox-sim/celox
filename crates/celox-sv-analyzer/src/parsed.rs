//! Reusable syntax and module metadata for hierarchy specialization.

use std::path::{Path, PathBuf};

use fxhash::FxHashMap as HashMap;

use crate::{AnalyzerError, Ir, ModuleInterfaces, PackageSource, analyze, ast, ir, syntax};

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

    pub fn module_names(&self) -> &[String] {
        self.index.names()
    }

    pub fn module_interfaces(&self) -> &ModuleInterfaces {
        &self.index.interfaces
    }

    pub fn packages(&self) -> Result<Vec<PackageSource>, AnalyzerError> {
        ast::with_call_sites(|| ast::packages::source_packages(&self.code, &self.tree))
    }

    /// Analyze a module, rewriting its type parameters and package imports
    /// when needed. Numeric overrides reuse the original syntax tree.
    pub fn analyze_module(
        &self,
        module_name: &str,
        overrides: &HashMap<String, ir::ConstExpr>,
        type_overrides: &[(String, String)],
        interfaces: &ModuleInterfaces,
        packages: &HashMap<String, PackageSource>,
    ) -> Result<Ir, AnalyzerError> {
        let typed = if type_overrides.is_empty() {
            None
        } else {
            ast::packages::apply_type_parameter_overrides(
                &self.code,
                &self.tree,
                module_name,
                type_overrides,
            )?
        };
        let typed_source = typed
            .as_deref()
            .map(|code| Self::parse(code, &self.path))
            .transpose()?;
        let source = typed_source.as_ref().unwrap_or(self);
        let inlined = if packages.is_empty() {
            None
        } else {
            ast::packages::inline_packages(&source.code, &source.tree, module_name, packages)?
        };
        let inlined_source = inlined
            .as_deref()
            .map(|code| Self::parse(code, &self.path))
            .transpose()?;
        let source = inlined_source.as_ref().unwrap_or(source);
        source.analyze_module_with_parameter_expr_overrides(module_name, overrides, interfaces)
    }

    /// Analyze without rewriting the source, retaining literal override types.
    pub fn analyze_module_with_parameter_expr_overrides(
        &self,
        module_name: &str,
        overrides: &HashMap<String, ir::ConstExpr>,
        interfaces: &ModuleInterfaces,
    ) -> Result<Ir, AnalyzerError> {
        let overrides = overrides
            .iter()
            .map(|(name, value)| (name.clone(), value.clone().into()))
            .collect();
        ast::with_call_sites(|| {
            let source = ast::Source::from_indexed_syntax_module(
                &self.tree,
                &self.index,
                module_name,
                &overrides,
                interfaces,
            )?;
            analyze::analyze_source(source)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn rewrites_type_parameters_and_packages_before_analysis() {
        let code = r#"
            package p; localparam V = 3; endpackage
            module Top #(parameter type T = logic [3:0]) (output T y);
                import p::*;
                assign y = V;
            endmodule
        "#;
        let path = Path::new("rewritten.sv");
        let source = ParsedSource::parse(code, path).unwrap();
        let packages = source
            .packages()
            .unwrap()
            .into_iter()
            .map(|p| (p.name.clone(), p))
            .collect();
        let types = vec![("T".into(), "logic [7:0]".into())];
        let actual = source
            .analyze_module(
                "Top",
                &HashMap::default(),
                &types,
                &ModuleInterfaces::default(),
                &packages,
            )
            .unwrap();
        let typed = crate::apply_module_type_parameters(code, path, "Top", &types)
            .unwrap()
            .unwrap();
        let inlined = crate::inline_module_packages(&typed, path, "Top", &packages)
            .unwrap()
            .unwrap();
        let expected = crate::analyze_source(&inlined, path).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            actual.modules()[0].ports()[0].r#type().resolved_width(),
            Some(8)
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
