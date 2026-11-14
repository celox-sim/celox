//! The declarations a scope sees while its expressions are bound.

use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PackedDimensions {
    pub(super) variables: ScopedMap<VariableDimensions>,
    pub(super) const_env: SharedEnv,
    pub(super) type_aliases: SharedAliases,
    /// The names of every type visible in the scope of a typedef being
    /// bound, also those `type_aliases` has no binding for (yet), which tell
    /// an undeclared name from a type not bound; `None` where the context
    /// does not know all of them.
    pub(super) type_declarations: Option<Arc<types::TypeDeclarations>>,
    pub(super) function_return_types: SharedMap<FunctionReturnMetadata>,
    pub(super) functions: Arc<HashMap<String, Function>>,
    pub(super) parameter_values: SharedMap<Expr>,
    /// Constant array initializers for binding child parameter values.
    pub(super) array_parameter_values: SharedMap<Expr>,
    pub(super) expression_signedness: ScopedMap<bool>,
    pub(super) constant_indexed_base: bool,
    /// The bodies bound in this context are those of constant functions,
    /// for evaluation in constant expressions (13.4.3).
    pub(super) constant_function_bodies: bool,
    /// The current lexical scope has already collected its visible declarations.
    /// Preliminary parameter/function/range lowering must keep this false.
    pub(super) scope_types_complete: bool,
    /// Calls are checked for legality while their operands remain unexecuted.
    pub(super) type_query: bool,
    /// The declared shape of each argument of each subroutine, for
    /// assignment patterns passed as arguments.
    pub(super) subroutine_param_shapes: Arc<HashMap<String, Vec<VariableDimensions>>>,
    pub(super) subroutine_directions: Arc<lifetimes::Directions>,
    pub(super) subroutine_param_names: Arc<HashMap<String, Vec<String>>>,
    /// The class whose method is being bound.
    pub(super) class_scope: Option<String>,
    /// The method being bound is the constructor of `class_scope`.
    pub(super) constructor: bool,
    /// The name of the method being bound, which a call `name(...)` in its
    /// body calls rather than reads its result variable (IEEE 1800-2023
    /// 13.4.1).
    pub(super) method_name: Option<String>,
    /// The name `%m` prints in the body being bound, when the body is a
    /// subroutine's; `None` names the calling instance.
    pub(super) scope_name: Option<Arc<expression_binding::ScopeName>>,
    /// Where array locator methods declare their iterator locals; `None`
    /// outside a procedural body.
    pub(super) locator_locals: Option<locators::LocalSink>,
    /// The iterators of the enclosing `with` expressions, innermost last.
    pub(super) locator_iterators: Arc<Vec<locators::Iterator>>,
    /// The values of the operands whose side effects a procedural body has
    /// already executed before their statement (see [`effects`]).
    pub(super) effect_values: Arc<HashMap<celox_sv_syntax::ExprId, Expr>>,
    /// The saved index of the last element of a queue that a select `q[$]`
    /// of an assignment target names, by the select (IEEE 1800-2023 7.10.1).
    pub(super) last_indices: Arc<HashMap<celox_sv_syntax::ExprId, Expr>>,
    /// The frozen declaration types of visible enumerators and scalar
    /// parameters, independent of their numeric values.
    pub(super) constant_types: Arc<HashMap<String, semantic_types::ConstantType>>,
    /// Inside a class specialization, the tables of the scope around the
    /// class, before the specialization's parameters were added.
    pub(super) class_base: Option<ClassBase>,
    /// The names in a method's body that denote static properties of its
    /// class, until a local or formal of the name hides one (IEEE 1800-2023
    /// 6.21): `p.rand_mode()` names the static property only while `p`
    /// is one of them.
    pub(super) class_static_members: Arc<HashSet<String>>,
}

/// The tables of the scope that declares a class, which
/// [`classes::specializations::body_scope`] adds the parameters of one of
/// its specializations to. The scope's declarations are discovered from
/// these, the same for all specializations, and each query in the class
/// adds the specialization back. Two are equal when they are the same.
#[derive(Debug, Clone)]
pub(super) struct ClassBase(pub(super) Arc<ClassTables>);

#[derive(Debug)]
pub(super) struct ClassTables {
    pub(super) const_env: SharedEnv,
    pub(super) type_aliases: SharedAliases,
    pub(super) parameter_values: SharedMap<Expr>,
    pub(super) constant_types: Arc<HashMap<String, semantic_types::ConstantType>>,
}

impl PartialEq for ClassBase {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ClassBase {}

impl ClassBase {
    /// The base of a class scope added to the tables of `outer`. A base
    /// `outer` already carries is not reused: `outer` may hold more than
    /// that base and its class, as the constants of a package analyzed for
    /// one of its specializations.
    pub(super) fn of(outer: &PackedDimensions) -> Self {
        Self(Arc::new(ClassTables {
            const_env: outer.const_env.clone(),
            type_aliases: outer.type_aliases.clone(),
            parameter_values: outer.parameter_values.clone(),
            constant_types: outer.constant_types.clone(),
        }))
    }
}

impl PackedDimensions {
    /// Bind the completed declaration set without changing lexical map identity.
    pub(super) fn bind_types(
        &self,
        table: &mut semantic_types::TyTable,
        functions: &const_functions::ConstantFunctions,
    ) {
        for (_, shape) in self.iter() {
            shape.bind_type(table, functions, &self.const_env);
        }
    }

    pub(super) fn new(
        variables: impl Into<ScopedMap<VariableDimensions>>,
        const_env: impl ToShared<i128, ConstEnv>,
        type_aliases: impl ToShared<Type, TypeAliases>,
    ) -> Self {
        Self {
            variables: variables.into(),
            const_env: const_env.to_shared(),
            type_aliases: type_aliases.to_shared(),
            type_declarations: None,
            function_return_types: SharedMap::default(),
            functions: Arc::default(),
            parameter_values: SharedMap::default(),
            array_parameter_values: SharedMap::default(),
            expression_signedness: ScopedMap::default(),
            constant_indexed_base: false,
            constant_function_bodies: false,
            scope_types_complete: false,
            type_query: false,
            subroutine_param_shapes: Arc::default(),
            subroutine_directions: Arc::default(),
            subroutine_param_names: Arc::default(),
            class_scope: None,
            constructor: false,
            method_name: None,
            scope_name: None,
            locator_locals: None,
            locator_iterators: Arc::default(),
            effect_values: Arc::default(),
            last_indices: Arc::default(),
            constant_types: Arc::default(),
            class_base: None,
            class_static_members: Arc::default(),
        }
    }
}

impl Deref for PackedDimensions {
    type Target = ScopedMap<VariableDimensions>;

    fn deref(&self) -> &Self::Target {
        &self.variables
    }
}

impl DerefMut for PackedDimensions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.variables
    }
}

impl Signedness for PackedDimensions {
    fn signedness(&self, name: &str) -> Option<bool> {
        self.get(name).map(|dimensions| dimensions.signed)
    }
}
