//! The syntax and specialization bindings of the scope being analyzed.

use super::*;

/// Syntax and specialization bindings for one scope within an analysis session.
/// Child modules share the session and receive their own parameter bindings.
#[derive(Clone)]
pub(crate) struct AnalysisContext<'a> {
    pub(super) syntax_tree: &'a SyntaxTree,
    pub(super) parameter_overrides: Cow<'a, HashMap<String, ConstExpr>>,
    /// The symbols the scope being analyzed imports (IEEE 1800-2023 26.3).
    pub(super) imported: Arc<scope::ScopeSymbols>,
    /// The package being analyzed, inside which `p::x` names its own item.
    pub(super) package: Option<Arc<str>>,
    /// The constant functions that calls in constant expressions of the
    /// scope call (IEEE 1800-2023 13.4.3).
    pub(crate) functions: const_functions::ConstantFunctions,
    /// Whether generate scopes elaborate only their items, and their
    /// subroutines are collected later.
    pub(super) root_subroutines: bool,
    /// The interface instances being analyzed, outermost first.
    pub(super) active_interfaces: Arc<Vec<interface_ast::ActiveInstance>>,
    /// The time scopes that time queries of the scope read (IEEE 1800-2023
    /// 3.14.2).
    pub(super) timescale: Arc<timescales::QueryContext>,
    /// The hierarchical path of the interface instance being analyzed.
    pub(super) interface_instance: Option<Arc<str>>,
    /// Whether the scope is a package or a compilation unit, whose free
    /// subroutines see every value name they may read in their tables, so
    /// that another one is undeclared (IEEE 1800-2023 6.6, 23.9).
    pub(super) declared_values: bool,
}

impl<'a> AnalysisContext<'a> {
    #[cfg(test)]
    pub(super) fn new(
        syntax_tree: &'a SyntaxTree,
        parameter_overrides: &'a HashMap<String, ConstExpr>,
    ) -> Self {
        Self {
            syntax_tree,
            parameter_overrides: Cow::Borrowed(parameter_overrides),
            imported: Arc::default(),
            package: None,
            functions: const_functions::ConstantFunctions::default(),
            root_subroutines: false,
            active_interfaces: Arc::default(),
            timescale: Arc::default(),
            interface_instance: None,
            declared_values: false,
        }
    }

    pub(crate) fn unspecialized(syntax_tree: &'a SyntaxTree) -> Self {
        Self {
            syntax_tree,
            parameter_overrides: Cow::Owned(HashMap::default()),
            imported: Arc::default(),
            package: None,
            functions: const_functions::ConstantFunctions::default(),
            root_subroutines: false,
            active_interfaces: Arc::default(),
            timescale: Arc::default(),
            interface_instance: None,
            declared_values: false,
        }
    }

    pub(super) fn child<'b>(
        &self,
        syntax_tree: &'b SyntaxTree,
        parameter_overrides: &'b HashMap<String, ConstExpr>,
    ) -> AnalysisContext<'b> {
        AnalysisContext {
            syntax_tree,
            parameter_overrides: Cow::Borrowed(parameter_overrides),
            imported: self.imported.clone(),
            package: self.package.clone(),
            functions: self.functions.clone(),
            root_subroutines: self.root_subroutines,
            active_interfaces: self.active_interfaces.clone(),
            timescale: self.timescale.clone(),
            interface_instance: self.interface_instance.clone(),
            declared_values: self.declared_values,
        }
    }

    /// This context analyzing a scope that imports `symbols`.
    pub(super) fn importing(&self, symbols: Arc<scope::ScopeSymbols>) -> Self {
        Self {
            imported: symbols,
            ..self.clone()
        }
    }

    /// This context with `functions` as its constant functions.
    pub(super) fn with_functions(&self, functions: const_functions::ConstantFunctions) -> Self {
        Self {
            functions,
            ..self.clone()
        }
    }

    /// This context analyzing package `name`, inside which `name::x` names
    /// its own item `x`.
    pub(super) fn in_package(&self, name: &str) -> Self {
        Self {
            package: Some(name.into()),
            ..self.clone()
        }
    }
}

impl Deref for AnalysisContext<'_> {
    type Target = SyntaxTree;

    fn deref(&self) -> &Self::Target {
        self.syntax_tree
    }
}
