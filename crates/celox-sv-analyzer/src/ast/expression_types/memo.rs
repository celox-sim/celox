//! Positive expression types for the current resolved lexical context.
//!
//! Keeping the syntax and immutable scope snapshots alive prevents allocator
//! address reuse from making an old key name a new arena or binding. Only one
//! context is retained, so visiting many temporary scopes does not retain them.
use super::*;

#[derive(Default)]
pub(in crate::ast) struct Memo(Option<Context>);

struct Context {
    source: SyntaxTree,
    dims: PackedDimensions,
    imported: Arc<scope::ScopeSymbols>,
    functions: const_functions::ConstantFunctions,
    overrides: HashMap<String, ConstExpr>,
    package: Option<Arc<str>>,
    interfaces: Arc<Vec<interface_ast::ActiveInstance>>,
    instance: Option<Arc<str>>,
    timescale: Arc<timescales::QueryContext>,
    version: (u64, usize),
    types: HashMap<ExprId, TyId>,
}

impl Context {
    fn applies(
        &self,
        tree: &AnalysisContext<'_>,
        dims: &PackedDimensions,
        version: (u64, usize),
    ) -> bool {
        std::ptr::eq(self.source.declarations(), tree.declarations())
            && self.source.scope_handle() == tree.scope_handle()
            && dimensions::same_tables(&self.dims, dims)
            && self.dims.locator_locals == dims.locator_locals
            && Arc::ptr_eq(&self.dims.locator_iterators, &dims.locator_iterators)
            && Arc::ptr_eq(&self.dims.effect_values, &dims.effect_values)
            && self.dims.property_select_base == dims.property_select_base
            && self.dims.element_select == dims.element_select
            && Arc::ptr_eq(&self.imported, &tree.imported)
            && self.functions.same(&tree.functions)
            && self.overrides == *tree.parameter_overrides
            && self.package == tree.package
            && Arc::ptr_eq(&self.interfaces, &tree.active_interfaces)
            && self.instance == tree.interface_instance
            && Arc::ptr_eq(&self.timescale, &tree.timescale)
            && self.version == version
    }
}

impl Memo {
    pub fn get(
        &self,
        tree: &AnalysisContext<'_>,
        session: &Session,
        id: ExprId,
        native: &Declarations,
        dims: &PackedDimensions,
    ) -> Option<TyId> {
        if !dims.scope_types_complete || !std::ptr::eq(native, tree.declarations()) {
            return None;
        }
        let context = self.0.as_ref()?;
        context
            .applies(
                tree,
                dims,
                (
                    session.foreign.revision(),
                    session.class_specializations.len(),
                ),
            )
            .then(|| context.types.get(&id).copied())
            .flatten()
    }

    pub fn insert(
        &mut self,
        tree: &AnalysisContext<'_>,
        id: ExprId,
        native: &Declarations,
        dims: &PackedDimensions,
        version: (u64, usize),
        ty: TyId,
    ) {
        if !dims.scope_types_complete || !std::ptr::eq(native, tree.declarations()) {
            return;
        }
        if !self
            .0
            .as_ref()
            .is_some_and(|context| context.applies(tree, dims, version))
        {
            self.0 = Some(Context {
                source: tree.syntax_tree.clone(),
                dims: dims.clone(),
                imported: tree.imported.clone(),
                functions: tree.functions.clone(),
                overrides: tree.parameter_overrides.clone().into_owned(),
                package: tree.package.clone(),
                interfaces: tree.active_interfaces.clone(),
                instance: tree.interface_instance.clone(),
                timescale: tree.timescale.clone(),
                version,
                types: HashMap::default(),
            });
        }
        self.0.as_mut().unwrap().types.insert(id, ty);
    }
}
