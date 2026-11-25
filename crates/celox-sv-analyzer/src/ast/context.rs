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
    /// The path, below the instance that runs it, of the interface instance
    /// whose body is analyzed, which `%m` in the body names (IEEE 1800-2023
    /// 21.2.1.5); `None` where the body runs in an instance this scope does
    /// not name, as an interface port's does.
    pub(super) instance_scope: Option<Arc<str>>,
    /// Whether the scope is a package or a compilation unit, whose free
    /// subroutines see every value name they may read in their tables, so
    /// that another one is undeclared (IEEE 1800-2023 6.6, 23.9).
    pub(super) declared_values: bool,
    /// The instance of a generate block whose declarations are being
    /// bound, which declares types of its own (see
    /// [`types::declaring_scope`]).
    pub(super) generate_scope: Option<Arc<types::GenerateScope>>,
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
            instance_scope: None,
            declared_values: false,
            generate_scope: None,
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
            instance_scope: None,
            declared_values: false,
            generate_scope: None,
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
            instance_scope: self.instance_scope.clone(),
            declared_values: self.declared_values,
            // A child is a design element of its own, outside the generate
            // blocks of this one.
            generate_scope: None,
        }
    }

    /// This context analyzing a scope that imports `symbols`.
    pub(super) fn importing(&self, symbols: Arc<scope::ScopeSymbols>) -> Self {
        Self {
            imported: symbols,
            ..self.clone()
        }
    }

    /// This context binding the declarations at `tokens` of the instance
    /// `path` of a generate block, as the path of its elaborated scope
    /// names it (`g[0].h[1]`).
    pub(super) fn in_generate_scope(
        &self,
        path: &str,
        tokens: celox_sv_syntax::TokenRange,
    ) -> Self {
        Self {
            generate_scope: (!path.is_empty())
                .then(|| Arc::new(types::GenerateScope::new(path, self.declarations(), tokens))),
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
