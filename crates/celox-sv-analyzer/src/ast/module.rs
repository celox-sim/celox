//! What an analyzed module gives its users.

use super::*;

impl Module {
    pub(crate) fn declaration_expr(
        &self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        expr: &ConstExpr,
        signedness: &HashMap<String, bool>,
    ) -> ConstExpr {
        let expanded = expand_expr_calls(
            const_expr_to_expr(expr.clone()),
            &self.symbolic_functions,
            signedness,
            0,
            true,
        );
        expr_to_index_const(tree, session, expanded, &PackedDimensions::default())
            .unwrap_or_else(|| expr.clone())
    }

    pub(crate) fn declaration_subroutines(&self) -> &[crate::ir::Subroutine] {
        &self.declaration_subroutines
    }

    /// A context that calls the module's constant functions, for constant
    /// expressions of the analyzed module, which no longer read its syntax:
    /// `syntax` is an empty tree.
    pub(crate) fn constant_context<'a>(&self, syntax: &'a SyntaxTree) -> AnalysisContext<'a> {
        AnalysisContext::unspecialized(syntax).with_functions(self.constant_functions.clone())
    }

    pub(crate) fn interface_scope_names(&self) -> &[String] {
        &self.interface_scope_names
    }

    pub(crate) fn interface_handles(&self) -> &[crate::ir::InterfaceHandle] {
        &self.interface_handles
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn ports(&self) -> &[Port] {
        &self.ports
    }

    pub fn signals(&self) -> &[Signal] {
        &self.signals
    }

    /// The package parameters the module sees.
    pub fn imported_parameters(&self) -> &[Parameter] {
        &self.imported_parameters
    }

    pub fn parameters(&self) -> &[Parameter] {
        &self.parameters
    }

    pub fn instances(&self) -> &[Instance] {
        &self.instances
    }

    pub fn assignments(&self) -> &[Assignment] {
        &self.assignments
    }

    pub fn comb_processes(&self) -> &[CombProcess] {
        &self.comb_processes
    }

    pub fn ff_processes(&self) -> &[FfProcess] {
        &self.ff_processes
    }

    pub fn initial_processes(&self) -> &[InitialProcess] {
        &self.initial_processes
    }

    pub fn locals(&self) -> &[LocalVariable] {
        &self.locals
    }

    pub fn subroutines(&self) -> &[Subroutine] {
        &self.subroutines
    }

    /// Move the executable bodies out of the module, which keeps its
    /// declarations.
    pub(crate) fn take_bodies(&mut self) -> ModuleBodies {
        ModuleBodies {
            assignments: std::mem::take(&mut self.assignments),
            comb_processes: std::mem::take(&mut self.comb_processes),
            ff_processes: std::mem::take(&mut self.ff_processes),
            initial_processes: std::mem::take(&mut self.initial_processes),
            subroutines: std::mem::take(&mut self.subroutines),
        }
    }

    pub fn dpi_imports(&self) -> &[crate::ir::DpiImport] {
        &self.dpi_imports
    }

    /// Whether this is a program (IEEE 1800-2023 24.3).
    pub fn is_program(&self) -> bool {
        self.program
    }
}
