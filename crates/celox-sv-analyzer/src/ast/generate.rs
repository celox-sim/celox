//! Shared, bounded elaboration of generate scopes before collecting RTL objects.
//!
//! IEEE 1800-2023 sections 12.5 and 27.4–27.6 define the comparison and scope
//! rules used here. Generate-local typedefs and functions in loop-generate
//! remain unsupported; inactive branches are not lowered.
use super::*;
use celox_sv_syntax::ExprId;
mod elaboration;

mod dependency_order;
use dependency_order::DependencyOrder;
mod scope_views;
pub(super) use scope_views::ScopeViews;

// Temporary identifiers protect definition-site bindings while functions are
// inlined into a generate scope. They are removed when that item's names are
// qualified, and never reach the public analyzer IR.
pub(super) fn root_subroutines_only(state: &AnalysisState) -> bool {
    state.root_subroutines.get()
}

/// Seed module functions before conditions need to call them. The full pass
/// still selects generate scopes and reports every generate diagnostic.
pub(super) fn with_root_subroutines<T>(
    state: &std::rc::Rc<AnalysisState>,
    f: impl FnOnce() -> T,
) -> T {
    struct Restore {
        state: std::rc::Rc<AnalysisState>,
        previous: bool,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            self.state.root_subroutines.set(self.previous);
        }
    }
    let _restore = Restore {
        state: state.clone(),
        previous: state.root_subroutines.replace(true),
    };
    f()
}

const OUTER_BINDING: &str = "\0generate_outer:";

// Whitespace terminates an escaped identifier (IEEE 1800-2023 5.6.1).
// Preserve that terminator before appending hierarchy delimiters or loop indices.
fn scope_component(name: &str) -> String {
    if name.starts_with('\\') {
        format!("{name} ")
    } else {
        name.to_string()
    }
}

#[derive(Clone)]
pub(super) struct Item {
    state: std::rc::Rc<AnalysisState>,
    pub id: celox_sv_syntax::BodyItemId,
    pub env: SharedMap<i128>,
    pub literals: SharedMap<Expr>,
    parameter_dimensions: Arc<VariablePackedDimensions>,
    qualified_types: SharedMap<Type>,
    names: SharedMap<String>,
    imports: SharedMap<String>,
    shadowed: Arc<HashSet<String>>,
    scope: String,
}

/// The names the imports of the enclosing generate blocks bind to another
/// declaration than the module does (see `imports::resolve_imports`), unless
/// a declaration of an inner block hides them again. The module-wide tables
/// hold those declarations under their qualified names.
#[derive(Clone, Copy)]
struct BlockImports<'s> {
    state: &'s std::rc::Rc<AnalysisState>,
    names: &'s HashMap<String, String>,
    imports: &'s HashMap<String, String>,
    /// The names the blocks declare or import.
    shadowed: &'s HashSet<String>,
}

impl<'s> BlockImports<'s> {
    fn bindings(self) -> impl Iterator<Item = (&'s String, &'s String)> {
        self.imports
            .iter()
            .filter(move |(name, target)| self.names.get(*name) == Some(*target))
    }

    /// The type aliases visible here: a name the blocks declare or import
    /// hides the module's type of that name, and an imported name denotes
    /// the type of its declaration.
    fn type_aliases<'m>(
        self,
        aliases: &'m HashMap<String, Type>,
    ) -> Cow<'m, HashMap<String, Type>> {
        let mut visible = Cow::Borrowed(aliases);
        let hidden: Vec<&String> = if aliases.len() <= self.shadowed.len() {
            aliases
                .keys()
                .filter(|name| self.shadowed.contains(*name))
                .collect()
        } else {
            self.shadowed
                .iter()
                .filter(|name| aliases.contains_key(*name))
                .collect()
        };
        for name in hidden {
            visible.to_mut().remove(name);
        }
        for (name, target) in self.bindings() {
            if let Some(r#type) = aliases.get(target) {
                visible.to_mut().insert(name.clone(), r#type.clone());
            }
        }
        visible
    }

    /// Make each imported name call the constant function of its
    /// declaration, until the guard is dropped; a name that imports no
    /// constant function calls none. Calls in the bodies of the functions it
    /// hides keep their definition-site meaning.
    fn install_constant_functions(self) -> Option<const_functions::Installed> {
        let installed = const_functions::installed(&self.state.const_functions);
        let rebound: Vec<_> = self
            .bindings()
            .filter(|(name, target)| installed.contains(name) || installed.contains(target))
            .collect();
        if rebound.is_empty() {
            return None;
        }
        let hidden: HashMap<String, String> = rebound
            .iter()
            .filter(|(name, _)| installed.contains(name))
            .map(|(name, _)| ((*name).clone(), format!("{OUTER_BINDING}{name}")))
            .collect();
        let rename = scope::Renamer { names: &hidden };
        let mut visible =
            installed.renamed(&mut |name| rename.name(name), &mut |stmt| rename.stmt(stmt));
        for (name, target) in rebound {
            visible.alias(name, target);
        }
        Some(const_functions::install(self.state, visible))
    }
}

impl Item {
    /// Executable ownership is the elaborated native body item.
    pub fn continuous_assignment<'n>(
        &self,
        native: &'n celox_sv_syntax::Declarations,
    ) -> Converted<Option<&'n celox_sv_syntax::AssignSyntax>> {
        Ok(match native.body_item(self.id).kind {
            celox_sv_syntax::BodyItemKind::Assign(id) => {
                let assignment = native.assign_statement(id);
                let owner = native.body_item(self.id).tokens;
                if assignment.tokens.start < owner.start || assignment.tokens.end > owner.end {
                    return Err(AnalyzerError::Parse(
                        "native continuous assignment is outside its owner".into(),
                    ));
                }
                Some(assignment)
            }
            _ => None,
        })
    }

    pub fn procedure<'n>(
        &self,
        native: &'n celox_sv_syntax::Declarations,
    ) -> Converted<Option<&'n celox_sv_syntax::Procedure>> {
        Ok(match native.body_item(self.id).kind {
            celox_sv_syntax::BodyItemKind::Procedure(id) => Some(native.procedure(id)),
            _ => None,
        })
    }

    fn block_imports(&self) -> BlockImports<'_> {
        BlockImports {
            state: &self.state,
            names: &self.names,
            imports: &self.imports,
            shadowed: &self.shadowed,
        }
    }

    /// The type aliases this item sees, given the module's.
    pub fn type_aliases<'m>(
        &self,
        aliases: &'m HashMap<String, Type>,
    ) -> Cow<'m, HashMap<String, Type>> {
        if self.qualified_types.is_empty() {
            return self.block_imports().type_aliases(aliases);
        }
        let mut all = aliases.clone();
        all.extend(
            self.qualified_types
                .iter()
                .map(|(name, ty)| (name.clone(), ty.clone())),
        );
        Cow::Owned(self.block_imports().type_aliases(&all).into_owned())
    }

    /// Make the constant functions this item sees the installed ones, until
    /// the guard is dropped.
    pub fn enter(&self) -> Option<const_functions::Installed> {
        self.block_imports().install_constant_functions()
    }

    pub fn scope_name(&self) -> &str {
        &self.scope
    }

    pub fn interface_scope(&self) -> Option<scope::Installed> {
        let imported = scope::imported(&self.state);
        if imported.interface_scopes.is_empty() {
            return None;
        }
        let mut scopes = imported
            .interface_scopes
            .iter()
            .filter(|(name, _)| {
                self.scope == **name
                    || self
                        .scope
                        .strip_prefix(name.as_str())
                        .is_some_and(|rest| rest.starts_with('.'))
            })
            .collect::<Vec<_>>();
        if scopes.is_empty() {
            return None;
        }
        scopes.sort_by_key(|(name, _)| name.len());
        let mut local = (*imported).clone();
        for (_, bindings) in scopes {
            local.interface_members.extend(bindings.members.clone());
            local.interface_types.extend(bindings.types.clone());
            local.interface_arrays.extend(bindings.arrays.clone());
            local.interface_instances.extend(bindings.instances.clone());
        }
        Some(scope::install(&self.state, Arc::new(local)))
    }

    /// Parameters have already populated the enclosing constant environment.
    pub fn is_parameter_declaration(&self, native: &celox_sv_syntax::Declarations) -> bool {
        matches!(
            native.body_item(self.id).kind,
            celox_sv_syntax::BodyItemKind::Parameter(_)
        )
    }

    pub fn name(&self, name: &str) -> String {
        if let Some(name) = name.strip_prefix(OUTER_BINDING) {
            return name.to_string();
        }
        self.names
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    pub fn instance_name(&self, name: &str) -> String {
        if self.scope.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", self.scope, scope_component(name))
        }
    }

    pub fn parameter_literals(&self, inherited: &HashMap<String, Expr>) -> HashMap<String, Expr> {
        let mut literals = inherited.clone();
        for name in self.shadowed.iter() {
            if let Some(value) = inherited.get(name) {
                literals.insert(format!("{OUTER_BINDING}{name}"), value.clone());
            }
        }
        literals.extend(self.literals.clone());
        for name in self.names.keys() {
            literals.remove(name);
        }
        literals
    }

    pub fn dimensions(&self, dimensions: &PackedDimensions) -> PackedDimensions {
        let mut local = dimensions.clone();
        local.const_env = self.env.clone();
        local.parameter_values =
            if self.shadowed.is_empty() && self.names.is_empty() && self.literals.is_empty() {
                dimensions.parameter_values.clone()
            } else {
                self.parameter_literals(&dimensions.parameter_values).into()
            };
        if !self.shadowed.is_empty() || !self.names.is_empty() {
            local
                .array_parameter_values
                .retain(|name, _| !self.shadowed.contains(name) && !self.names.contains_key(name));
        }
        // Ordinary module items keep the module's function bindings. Rewriting
        // every function body for every item costs O(items * function size).
        if !self.shadowed.is_empty() || !self.names.is_empty() {
            if !dimensions.functions.is_empty() {
                local.functions = Arc::new(self.functions(&dimensions.functions));
            }
            if !dimensions.subroutine_param_names.is_empty() {
                local.subroutine_param_names =
                    Arc::new(self.function_aliases((*dimensions.subroutine_param_names).clone()));
            }
            if !dimensions.subroutine_param_shapes.is_empty() {
                local.subroutine_param_shapes =
                    Arc::new(self.function_aliases((*dimensions.subroutine_param_shapes).clone()));
            }
            if !dimensions.function_return_types.is_empty() {
                local.function_return_types =
                    self.function_aliases(dimensions.function_return_types.clone());
            }
        }
        if let Cow::Owned(aliases) = self.type_aliases(&dimensions.type_aliases) {
            local.type_aliases = aliases;
        }
        let mut signedness = dimensions.expression_signedness.clone();
        for name in self.shadowed.iter() {
            let protected = format!("{OUTER_BINDING}{name}");
            if let Some(value) = dimensions.get(name) {
                local.insert(protected.clone(), value.clone());
                signedness.insert(protected.clone(), value.signed);
            }
            if let Some(value) = dimensions.const_env.get(name) {
                local.const_env.insert(protected.clone(), *value);
                if let Some(ty) = parameter_type_from_const_env(&dimensions.const_env, name) {
                    insert_parameter_type_markers(&mut local.const_env, &protected, ty);
                }
            }
        }
        for name in self.shadowed.iter() {
            local.remove(name);
            signedness.remove(name);
        }
        local.extend(
            self.parameter_dimensions
                .iter()
                .map(|(name, dims)| (name.clone(), dims.clone())),
        );
        signedness.extend(
            self.parameter_dimensions
                .iter()
                .map(|(name, dimensions)| (name.clone(), dimensions.signed)),
        );
        for (name, qualified) in self.names.iter() {
            if let Some(value) = dimensions.get(qualified) {
                local.insert(name.clone(), value.clone());
                signedness.insert(name.clone(), value.signed);
            }
        }
        local.expression_signedness = signedness;
        local
    }

    pub fn qualify_function(&self, function: &mut Function) {
        let mut bindings = self.clone();
        for parameter in &function.params {
            bindings.names.remove(&parameter.name);
        }
        if let Some(body) = &mut function.body {
            bindings.expr(body);
        }
    }

    fn functions(&self, functions: &HashMap<String, Function>) -> HashMap<String, Function> {
        let mut functions = functions.clone();
        let mut bindings = self.clone();
        bindings.names = self
            .shadowed
            .iter()
            .map(|name| (name.clone(), format!("{OUTER_BINDING}{name}")))
            .collect::<HashMap<_, _>>()
            .into();
        for function in functions.values_mut() {
            bindings.qualify_function(function);
        }
        self.function_aliases(functions)
    }

    fn function_aliases<T: Clone>(&self, mut functions: HashMap<String, T>) -> HashMap<String, T> {
        // Keep definition-site calls to hidden module functions available to the
        // inliner, then expose only the functions visible in this lexical scope.
        for name in self.shadowed.iter() {
            if let Some(function) = functions.remove(name) {
                functions.insert(format!("{OUTER_BINDING}{name}"), function);
            }
        }
        for (name, qualified) in self.names.iter() {
            if let Some(function) = functions.get(qualified).cloned() {
                functions.insert(name.clone(), function);
            }
        }
        functions
    }

    pub fn expr(&self, expr: &mut Expr) {
        match expr {
            Expr::Ident(name) => *name = self.name(name),
            Expr::Literal(_) => {}
            Expr::Select { expr, msb, lsb, .. } => {
                self.expr(expr);
                self.constant(msb);
                self.constant(lsb);
            }
            Expr::Concat(parts) => parts.iter_mut().for_each(|e| self.expr(e)),
            Expr::RepeatConcat { count, parts } => {
                self.constant(count);
                parts.iter_mut().for_each(|e| self.expr(e));
            }
            Expr::Resize { expr, .. } => self.expr(expr),
            Expr::Unary { expr, .. } => self.expr(expr),
            Expr::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                self.expr(condition);
                self.expr(then_expr);
                self.expr(else_expr);
            }
            Expr::Inside { expr, items } => {
                self.expr(expr);
                for item in items {
                    match item {
                        InsideItem::Value(value) => self.expr(value),
                        InsideItem::Range { low, high } => {
                            self.expr(low);
                            self.expr(high);
                        }
                    }
                }
            }
            Expr::Call { callee, args, .. } => {
                if let Callee::Subroutine(name) = callee {
                    *name = self.name(name);
                }
                args.iter_mut().flatten().for_each(|e| self.expr(e));
            }
        }
    }

    fn constant(&self, expr: &mut ConstExpr) {
        match expr {
            ConstExpr::Ident(name) => *name = self.name(name),
            ConstExpr::Literal(_) => {}
            ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                self.constant(expr);
                self.constant(msb);
                self.constant(lsb);
            }
            ConstExpr::Select { expr, bit } => {
                self.constant(expr);
                self.constant(bit);
            }
            ConstExpr::Function { callee, args, .. } => {
                if let Callee::Subroutine(name) = callee {
                    *name = self.name(name);
                }
                args.iter_mut().for_each(|e| self.constant(e));
            }
            ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => self.constant(expr),
            ConstExpr::Binary { left, right, .. } => {
                self.constant(left);
                self.constant(right);
            }
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                self.constant(condition);
                self.constant(then_expr);
                self.constant(else_expr);
            }
        }
    }

    pub fn assignment(&self, assignment: &mut Assignment) {
        self.lvalue(&mut assignment.lhs);
        self.expr(&mut assignment.rhs);
    }

    pub fn lvalue(&self, lvalue: &mut LValue) {
        match lvalue {
            LValue::Ident(name) => *name = self.name(name),
            LValue::Select {
                name,
                msb,
                lsb,
                array_slice_width,
                ..
            } => {
                *name = self.name(name);
                self.constant(msb);
                self.constant(lsb);
                if let Some(width) = array_slice_width {
                    self.constant(width);
                }
            }
        }
    }
}

#[derive(Clone, Default)]
struct Scope {
    state: std::rc::Rc<AnalysisState>,
    path: String,
    in_loop: bool,
    env: SharedMap<i128>,
    literals: SharedMap<Expr>,
    parameters: Vec<Parameter>,
    parameter_dimensions: Arc<VariablePackedDimensions>,
    qualified_types: SharedMap<Type>,
    names: SharedMap<String>,
    imports: SharedMap<String>,
    shadowed: Arc<HashSet<String>>,
}

impl Scope {
    fn type_aliases<'m>(
        &self,
        aliases: &'m HashMap<String, Type>,
    ) -> Cow<'m, HashMap<String, Type>> {
        if self.qualified_types.is_empty() {
            return self.block_imports().type_aliases(aliases);
        }
        let mut all = aliases.clone();
        all.extend(
            self.qualified_types
                .iter()
                .map(|(name, ty)| (name.clone(), ty.clone())),
        );
        Cow::Owned(self.block_imports().type_aliases(&all).into_owned())
    }

    fn block_imports(&self) -> BlockImports<'_> {
        BlockImports {
            state: &self.state,
            names: &self.names,
            imports: &self.imports,
            shadowed: &self.shadowed,
        }
    }
}

struct Elaborator<'a, 'b> {
    tree: &'a AnalysisContext<'a>,
    module_aliases: &'b HashMap<String, Type>,
    /// The type aliases of the scope being elaborated.
    aliases: Cow<'b, HashMap<String, Type>>,
    remaining: usize,
    functions: HashMap<String, Function>,
    items: Vec<Item>,
}

pub(super) fn items(
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Result<Vec<Item>, AnalyzerError> {
    items_with_literal_use(tree, env, aliases, LiteralUse::All)
}

/// Elaborate the same scopes and diagnostics, retaining module parameter
/// literals needed by subroutines, declarations and generate constructs.
pub(super) fn subroutine_items(
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Result<Vec<Item>, AnalyzerError> {
    items_with_literal_use(
        tree,
        env,
        aliases,
        if root_subroutines_only(&tree.state) {
            LiteralUse::RootSubroutines
        } else {
            LiteralUse::Subroutines
        },
    )
}

#[derive(Clone, Copy)]
enum LiteralUse {
    All,
    Subroutines,
    RootSubroutines,
}

fn items_with_literal_use(
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
    literal_use: LiteralUse,
) -> Result<Vec<Item>, AnalyzerError> {
    let mut elaborator = Elaborator {
        tree,
        module_aliases: aliases,
        aliases: Cow::Borrowed(aliases),
        remaining: MAX_GENERATE_LOOP_EXPANSION,
        functions: HashMap::default(),
        items: Vec::new(),
    };
    let parameters = parameters_from_scope(tree, aliases, env, &tree.parameter_overrides)?;
    let references = parameter_literal_references(tree, literal_use)?;
    let mut literals =
        parameters::parameter_value_env_for_references(&tree.state, &parameters, env, &references);
    if elaboration::needs_functions(tree)? {
        elaborator.functions = module_constant_functions(tree, env, aliases, &literals)?;
    }
    // Numeric values and their type markers are already carried by `env`.
    literals.retain(|name, _| !env.contains_key(name));
    let scope = Scope {
        state: tree.state.clone(),
        env: env.clone().into(),
        literals: literals.into(),
        parameter_dimensions: Arc::new(parameter_packed_dimensions(&parameters)),
        parameters,
        ..Scope::default()
    };
    let is_package = tree
        .scope_handle()
        .unit(tree.declarations())
        .is_none_or(|unit| unit.kind == celox_sv_syntax::UnitKind::Package);
    if is_package {
        for id in tree.body_membership()? {
            elaborator.push(id, &scope);
        }
    } else {
        elaborator.elaborate_root(scope, literal_use)?;
    }
    Ok(elaborator.items)
}

fn parameter_literal_references(
    tree: &AnalysisContext<'_>,
    literal_use: LiteralUse,
) -> Converted<HashSet<String>> {
    use celox_sv_syntax::{BodyItemKind, ExprKind, LocalDeclaration, StatementKind};
    let native = tree.declarations();
    let mut ranges = Vec::new();
    for id in tree.body_membership()? {
        let item = native.body_item(id);
        // Scalar initializers are already represented by the parameter binding
        // environment. Array defaults are lowered later and still need literals.
        if let BodyItemKind::Parameter(id) = item.kind
            && !native
                .parameter_declaration(id)
                .parameters
                .iter()
                .any(|parameter| array_parameters::is_array_parameter(parameter, native))
        {
            continue;
        }
        if matches!(
            literal_use,
            LiteralUse::Subroutines | LiteralUse::RootSubroutines
        ) {
            match item.kind {
                BodyItemKind::Assign(_) => continue,
                BodyItemKind::Procedure(id)
                    if !native.procedure(id).authored_statements.iter().any(|id| {
                        matches!(
                            native.statement(*id).kind,
                            StatementKind::Declaration(
                                LocalDeclaration::Variables(_)
                                    | LocalDeclaration::Constructed(_)
                                    | LocalDeclaration::Unconverted(_)
                            )
                        )
                    }) =>
                {
                    continue;
                }
                _ => {}
            }
        }
        // Generate containers retain all authored branches here; elaboration
        // decides active execution separately. Arena nodes preserve escaped names.
        ranges.push(item.tokens);
    }
    let mut references = HashSet::default();
    for (_, expression) in native.expression_nodes() {
        if !ranges.iter().any(|range| {
            range.start <= expression.tokens.start && expression.tokens.end <= range.end
        }) {
            continue;
        }
        if let ExprKind::Name(name) | ExprKind::Member { name, .. } = &expression.kind {
            references.insert(name.spelling.clone());
        }
    }
    Ok(references)
}

impl<'a> Elaborator<'a, '_> {
    /// A constant expression in this scope, or `None` when it has no
    /// constant value Celox can determine.
    fn expression(&self, root: ExprId, scope: &Scope) -> Result<Option<ConstExpr>, AnalyzerError> {
        let Some(expr) = constant_binding::expression(
            root,
            self.tree.declarations(),
            self.tree,
            &scope.env,
            &self.aliases,
        )?
        else {
            return Ok(None);
        };
        let dimensions = PackedDimensions {
            const_env: scope.env.clone(),
            ..PackedDimensions::default()
        };
        let Some(mut expr) = expr_to_index_const(
            &self.tree.state,
            substitute_expr_idents(const_expr_to_expr(expr), &scope.literals),
            &dimensions,
        ) else {
            return Ok(None);
        };
        Ok(self
            .expand_constant_calls(&self.tree.state, &mut expr, scope)
            .map(|()| expr))
    }

    fn expand_constant_calls(
        &self,
        state: &AnalysisState,
        expr: &mut ConstExpr,
        scope: &Scope,
    ) -> Option<()> {
        match expr {
            ConstExpr::Function { callee, args, .. } => {
                for arg in args {
                    self.expand_constant_calls(state, arg, scope)?;
                }
                // A block import binds the call to the qualified declaration,
                // which the installed constant functions evaluate.
                if let Callee::Subroutine(name) = callee
                    && let Some(target) = scope
                        .block_imports()
                        .bindings()
                        .find_map(|(imported, target)| (imported == name).then_some(target))
                {
                    *name = target.clone();
                }
                if let Callee::Subroutine(name) = callee
                    && self.functions.contains_key(name)
                {
                    if scope.shadowed.contains(name) {
                        return None;
                    }
                    let signedness: HashMap<_, _> = parameter_types_from_const_env(&scope.env)
                        .into_iter()
                        .map(|(name, ty)| (name, ty.signed))
                        .collect();
                    let expanded = expand_expr_calls(
                        const_expr_to_expr(expr.clone()),
                        &self.functions,
                        &signedness,
                        0,
                        true,
                    );
                    let expanded = simplify_constant_mux_conditions(state, expanded, &scope.env);
                    // Fold only the call: surrounding case arithmetic still needs
                    // the common comparison width, while function results are self-determined.
                    *expr = expr_to_const(
                        state,
                        fold_const_integral_expr_preserving_mask(state, expanded, &scope.env),
                    )?;
                }
            }
            ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
                self.expand_constant_calls(state, expr, scope)?
            }
            ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                self.expand_constant_calls(state, expr, scope)?;
                self.expand_constant_calls(state, msb, scope)?;
                self.expand_constant_calls(state, lsb, scope)?;
            }
            ConstExpr::Select { expr, bit } => {
                self.expand_constant_calls(state, expr, scope)?;
                self.expand_constant_calls(state, bit, scope)?;
            }
            ConstExpr::Binary { left, right, .. } => {
                self.expand_constant_calls(state, left, scope)?;
                self.expand_constant_calls(state, right, scope)?;
            }
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                self.expand_constant_calls(state, condition, scope)?;
                self.expand_constant_calls(state, then_expr, scope)?;
                self.expand_constant_calls(state, else_expr, scope)?;
            }
            ConstExpr::Ident(_) | ConstExpr::Literal(_) => {}
        }
        Some(())
    }

    fn value(&self, root: ExprId, scope: &Scope, detail: &str) -> Result<i128, AnalyzerError> {
        self.expression(root, scope)?
            .and_then(|expr| eval_ast_const_expr(&self.tree.state, &expr, &scope.env))
            .ok_or_else(|| AnalyzerError::Unsupported(detail.to_string()))
    }

    fn condition(&self, root: ExprId, scope: &Scope, detail: &str) -> Result<bool, AnalyzerError> {
        let types = parameter_types_from_const_env(&scope.env)
            .into_iter()
            .map(|(name, ty)| (name, (ty.width, ty.signed)))
            .collect();
        self.expression(root, scope)?
            .and_then(|expr| {
                let expr = expr.into();
                // Preserve four-state truth first (IEEE 1800-2023 12.4).
                // The numeric interpreter additionally handles user functions
                // kept symbolic for declaration-contract specialization.
                typecheck::eval_const_integral_literal_with_types(&expr, &scope.env, &types)
                    .map(|literal| typecheck::integral_literal_truth(&literal) == Some(true))
                    .or_else(|| {
                        typecheck::eval_const_expr_with_types_in_env(
                            &expr,
                            &const_functions::Environment {
                                constants: &*scope.env,
                                functions: &self.tree.state.const_functions,
                            },
                            &|name| types.get(name).copied(),
                        )
                        .map(|value| value != 0)
                    })
            })
            .ok_or_else(|| AnalyzerError::Unsupported(detail.to_string()))
    }

    fn push(&mut self, id: celox_sv_syntax::BodyItemId, scope: &Scope) {
        self.items.push(Item {
            state: self.tree.state.clone(),
            id,
            env: scope.env.clone(),
            literals: scope.literals.clone(),
            parameter_dimensions: scope.parameter_dimensions.clone(),
            qualified_types: scope.qualified_types.clone(),
            names: scope.names.clone(),
            imports: scope.imports.clone(),
            shadowed: scope.shadowed.clone(),
            scope: scope.path.clone(),
        });
    }

    fn constants_and_signal_types(
        &self,
        children: &[celox_sv_syntax::BodyItemId],
        scope: &mut Scope,
        declared: &mut HashSet<String>,
    ) -> Result<(), AnalyzerError> {
        use celox_sv_syntax::{BodyItemKind, DataDeclaration, ParameterKind};
        let native = self.tree.declarations();
        let mut signal_declarations = Vec::new();
        for id in children {
            let (data_type, variables): (_, Vec<_>) = match native.body_item(*id).kind {
                BodyItemKind::Data(id) => match native.data_declaration(id) {
                    DataDeclaration::Variables(value) | DataDeclaration::Constructed(value) => (
                        Some(value.data_type),
                        value
                            .variables
                            .iter()
                            .filter(|variable| {
                                type_binding::ordinary_variable(variable, value.data_type, native)
                            })
                            .map(|variable| (&variable.name, variable.dimensions.as_slice()))
                            .collect(),
                    ),
                    DataDeclaration::Unconverted(_) => continue,
                },
                BodyItemKind::Net(id) => {
                    let value = native.net_declaration(id);
                    (
                        Some(value.data_type),
                        value
                            .variables
                            .iter()
                            .map(|variable| (&variable.name, variable.dimensions.as_slice()))
                            .collect(),
                    )
                }
                BodyItemKind::Instance(id) => {
                    let value = native.instantiation(id);
                    if !self
                        .aliases
                        .contains_key(&instances::normalize_identifier(&value.name.spelling))
                    {
                        continue;
                    }
                    (
                        None,
                        value
                            .instances
                            .iter()
                            .map(|variable| (&variable.name, variable.dimensions.as_slice()))
                            .collect(),
                    )
                }
                _ => continue,
            };
            let shared_dependencies = data_type
                .map(|id| native_type_dependencies(id, native))
                .unwrap_or_default();
            for (name, dimensions) in variables {
                let name = instances::normalize_identifier(&name.spelling);
                if !declared.insert(name.clone()) {
                    return Err(unsupported(format!(
                        "duplicate generate-local declaration `{name}`"
                    )));
                }
                for key in [
                    name.clone(),
                    parameter_marker(&name),
                    parameters::unbounded_parameter_marker(&name),
                    local_parameter_marker(&name),
                    enum_marker(&name),
                    parameter_width_marker(&name),
                    parameter_signed_marker(&name),
                    parameter_rank_marker(&name),
                    variable_bits_marker(&name),
                    variable_size_marker(&name),
                    variable_signed_marker(&name),
                    variable_unpacked_dimensions_marker(&name),
                    variable_dimensions_marker(&name),
                ] {
                    scope.env.remove(&key);
                }
                scope.literals.remove(&name);
                let mut dependencies = shared_dependencies.clone();
                dependencies.extend(native_dimension_dependencies(dimensions, native));
                signal_declarations.push((name, *id, dependencies));
            }
        }
        let mut pending = Vec::new();
        for id in children {
            let BodyItemKind::Parameter(declaration) = native.body_item(*id).kind else {
                continue;
            };
            let assignments = &native.parameter_declaration(declaration).parameters;
            let single = assignments.len() == 1;
            for assignment in assignments {
                let ParameterKind::Value {
                    data_type, default, ..
                } = assignment.kind
                else {
                    return Err(unsupported("generate-local type parameter"));
                };
                let name = instances::normalize_identifier(&assignment.name.spelling);
                if !declared.insert(name.clone()) {
                    return Err(unsupported(format!(
                        "duplicate generate-local declaration `{name}`"
                    )));
                }
                let mut dependencies = native_type_dependencies(data_type, native);
                if let Some(root) = default {
                    dependencies.extend(native_expression_dependencies(root, native));
                }
                for key in [
                    name.clone(),
                    parameter_marker(&name),
                    parameters::unbounded_parameter_marker(&name),
                    local_parameter_marker(&name),
                    enum_marker(&name),
                    parameter_width_marker(&name),
                    parameter_signed_marker(&name),
                    parameter_rank_marker(&name),
                    variable_bits_marker(&name),
                    variable_size_marker(&name),
                    variable_signed_marker(&name),
                    variable_unpacked_dimensions_marker(&name),
                    variable_dimensions_marker(&name),
                ] {
                    scope.env.remove(&key);
                }
                scope.literals.remove(&name);
                scope
                    .parameters
                    .retain(|parameter| parameter.name() != name);
                scope.names.remove(&name);
                Arc::make_mut(&mut scope.shadowed).insert(name.clone());
                pending.push((name, *id, dependencies, single));
            }
        }
        // Signal dimensions and localparams can depend on each other. Preserve
        // signal-first/source-order priority, but build local dependency edges
        // once rather than rescanning all unresolved declarations after each bind.
        let signal_count = signal_declarations.len();
        let mut order = DependencyOrder::new(
            signal_declarations
                .iter()
                .map(|(name, _, deps)| (name.as_str(), deps))
                .chain(
                    pending
                        .iter()
                        .map(|(name, _, deps, _)| (name.as_str(), deps)),
                ),
        );
        let mut signal_declarations: Vec<_> = signal_declarations.into_iter().map(Some).collect();
        let mut pending: Vec<_> = pending.into_iter().map(Some).collect();
        while !order.is_complete() {
            let index = order.pop_ready().ok_or_else(|| {
                AnalyzerError::Unsupported("cyclic generate-local parameter dependency".to_string())
            })?;
            if index < signal_count {
                let (name, id, _) = signal_declarations[index]
                    .take()
                    .expect("ready signal is unbound");
                let mut signals = Vec::new();
                signals_from_body_item(
                    id,
                    self.tree,
                    &self.aliases,
                    &scope.env,
                    Some(&name),
                    &mut signals,
                )?;
                // Validate before exporting this type: a symbolic bound must
                // never reach IR resolution with a same-named outer number.
                for signal in &signals {
                    let ty = signal.r#type();
                    for bound in ty
                        .packed_ranges()
                        .iter()
                        .flat_map(|r| [r.left(), r.right()])
                        .chain(
                            ty.unpacked_ranges()
                                .iter()
                                .flat_map(|r| [r.left(), r.right()]),
                        )
                    {
                        if parameters::contains_unbounded_operand(bound, &scope.env)
                            && eval_ast_const_expr(&self.tree.state, bound, &scope.env).is_none()
                        {
                            return Err(AnalyzerError::Unsupported(
                                "numeric use of unbounded parameter in generate signal range"
                                    .into(),
                            ));
                        }
                    }
                }
                extend_const_env_with_variable_types(
                    &self.tree.state,
                    &mut scope.env,
                    signals
                        .iter()
                        .map(|signal| (signal.name(), signal.r#type())),
                );
            } else {
                if pending[index - signal_count]
                    .as_ref()
                    .is_some_and(|(_, _, _, single)| *single)
                {
                    // Reuse the declaration-order prefix while parameters remain
                    // consecutive in the original priority order. Signals can
                    // publish new type metadata; grouped declarations temporarily
                    // bind siblings. Either ends this run before rebuilding it.
                    let base = scope.env.clone();
                    let mut environment = parameters::ParameterEnvironment::new(
                        &self.tree.state,
                        &scope.parameters,
                        &base,
                        &pending
                            .iter()
                            .flatten()
                            .map(|(name, _, _, _)| name.clone())
                            .collect::<Vec<_>>(),
                    );
                    let mut types = parameter_types_from_const_env(&base);
                    let mut next = index;
                    loop {
                        let (name, id, _, _) = pending[next - signal_count]
                            .take()
                            .expect("ready parameter is unbound");
                        let inherited_count = scope.parameters.len();
                        parameters::parameters_from_native_with_environment(
                            &parameters::ownership::body(id, self.tree)?,
                            self.tree,
                            &mut scope.parameters,
                            true,
                            &base,
                            &self.aliases,
                            &HashMap::default(),
                            &mut environment,
                        )?;
                        let parameter = scope
                            .parameters
                            .get(inherited_count)
                            .filter(|parameter| parameter.name() == name)
                            .cloned()
                            .ok_or_else(|| {
                                AnalyzerError::Unsupported(format!(
                                    "generate-local parameter `{name}`"
                                ))
                            })?;
                        constants::bind_generate_parameter_with_types(
                            &self.tree.state,
                            parameter,
                            &mut scope.env,
                            &mut scope.literals,
                            &mut types,
                        );
                        order.complete(next);
                        let Some(ready) = order.peek_ready().filter(|ready| {
                            *ready >= signal_count
                                && pending[*ready - signal_count]
                                    .as_ref()
                                    .is_some_and(|(_, _, _, single)| *single)
                        }) else {
                            break;
                        };
                        next = order
                            .pop_ready()
                            .expect("the ready frontier was just inspected");
                        debug_assert_eq!(next, ready);
                    }
                    continue;
                }
                let (name, id, _, _) = pending[index - signal_count]
                    .take()
                    .expect("ready parameter is unbound");
                // The declaration may contain siblings. Temporarily append all
                // of them for the existing lowering semantics, then keep only
                // this scheduled parameter, without cloning the inherited prefix.
                let inherited_count = scope.parameters.len();
                parameters::parameters_from_native(
                    &parameters::ownership::body(id, self.tree)?,
                    self.tree,
                    &mut scope.parameters,
                    true,
                    &scope.env,
                    &self.aliases,
                    &HashMap::default(),
                )?;
                let parameter = scope
                    .parameters
                    .drain(inherited_count..)
                    .find(|parameter| parameter.name() == name)
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported(format!("generate-local parameter `{name}`"))
                    })?;
                scope.parameters.push(parameter.clone());
                bind_generate_parameter(
                    &self.tree.state,
                    parameter,
                    &mut scope.env,
                    &mut scope.literals,
                );
            }
            order.complete(index);
        }
        scope.parameter_dimensions = Arc::new(parameter_packed_dimensions(&scope.parameters));
        Ok(())
    }

    fn prepare_block_contents(
        &mut self,
        children: &[celox_sv_syntax::BodyItemId],
        mut scope: Scope,
        index: Option<(&str, i128)>,
    ) -> Result<Scope, AnalyzerError> {
        // Bind all declarations before lowering expressions, including forward references.
        let mut declared = HashSet::default();
        if let Some((genvar, value)) = index {
            declared.insert(genvar.to_string());
            // The implicit localparam in each loop body (IEEE 1800-2023 27.4)
            // must override inherited literals during expression substitution.
            // Mark it as a bounded value parameter for queries such as $isunbounded.
            scope
                .env
                .insert(parameters::unbounded_parameter_marker(genvar), 0);
            scope.literals.insert(
                genvar.to_string(),
                Expr::Literal(format_typed_parameter_literal(value, 32, true)),
            );
        }
        // Native declarations remain in their owning scope. Carry qualified
        // bindings with every item rather than leaking a sibling's typedefs.
        let type_names = children
            .iter()
            .filter_map(|id| {
                if let celox_sv_syntax::BodyItemKind::TypeAlias(alias) =
                    self.tree.declarations().body_item(*id).kind
                {
                    Some(instances::normalize_identifier(
                        &self.tree.declarations().type_alias(alias).name.spelling,
                    ))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if !type_names.is_empty() {
            let mut enum_names = HashSet::default();
            for id in children {
                if let celox_sv_syntax::BodyItemKind::TypeAlias(alias) =
                    self.tree.declarations().body_item(*id).kind
                    && let celox_sv_syntax::TypeAliasTarget::DataType(ty) =
                        self.tree.declarations().type_alias(alias).target
                    && let celox_sv_syntax::DataTypeKind::Enum { items, .. } =
                        &self.tree.declarations().data_type(ty).kind
                {
                    for item in items {
                        let name = instances::normalize_identifier(&item.name.spelling);
                        if !enum_names.insert(name.clone()) {
                            return Err(unsupported(format!(
                                "duplicate generate-local declaration `{name}`"
                            )));
                        }
                    }
                }
            }
            let mut aliases = types::type_aliases_from_body_with_env(
                self.tree,
                children,
                &scope.env,
                &self.aliases,
            )?;
            let enums = parameters::enum_member_constants_from_items(
                self.tree,
                children,
                &scope.env,
                &aliases,
                &HashMap::default(),
            )?;
            for name in type_names {
                if !declared.insert(name.clone()) {
                    return Err(unsupported(format!(
                        "duplicate generate-local declaration `{name}`"
                    )));
                }
                let ty = aliases
                    .get(&name)
                    .ok_or_else(|| {
                        unsupported(format!("unresolved generate-local typedef `{name}`"))
                    })?
                    .clone();
                // Freeze definition-site dimensions before a nested block or
                // another loop iteration shadows the names in their bounds.
                let ty = procedural::scoped_type(ty, &scope.env);
                if ty.packed_ranges().iter().any(|range| {
                    eval_ast_const_expr(&self.tree.state, range.left(), &scope.env).is_none()
                        || eval_ast_const_expr(&self.tree.state, range.right(), &scope.env)
                            .is_none()
                }) {
                    return Err(unsupported(format!(
                        "unresolved generate-local typedef bounds `{name}`"
                    )));
                }
                let qualified = format!("{}.{}", scope.path, scope_component(&name));
                scope.qualified_types.insert(qualified.clone(), ty.clone());
                scope.names.insert(name.clone(), qualified.clone());
                scope.imports.insert(name.clone(), qualified.clone());
                Arc::make_mut(&mut scope.shadowed).insert(name.clone());
                aliases.insert(qualified, ty.clone());
                aliases.insert(name, ty);
            }
            for (name, number) in enums.numbers {
                if !declared.insert(name.clone()) {
                    return Err(unsupported(format!(
                        "duplicate generate-local declaration `{name}`"
                    )));
                }
                scope.env.insert(name.clone(), number);
                insert_parameter_type_markers(&mut scope.env, &name, enums.types[&name]);
                scope.env.insert(enum_marker(&name), 1);
                scope
                    .literals
                    .insert(name.clone(), enums.exprs[&name].clone());
                scope.names.insert(
                    name.clone(),
                    format!("{}.{}", scope.path, scope_component(&name)),
                );
                Arc::make_mut(&mut scope.shadowed).insert(name);
            }
            self.aliases = Cow::Owned(aliases);
        }
        self.constants_and_signal_types(children, &mut scope, &mut declared)?;
        for id in children {
            if let celox_sv_syntax::BodyItemKind::Subroutine(subroutine) =
                self.tree.declarations().body_item(*id).kind
            {
                let subroutine = self.tree.declarations().subroutine(subroutine);
                if subroutine.kind == celox_sv_syntax::SubroutineKind::Function {
                    let name = instances::normalize_identifier(&subroutine.name.spelling);
                    if !declared.insert(name.clone()) {
                        return Err(unsupported(format!(
                            "duplicate generate-local declaration `{name}`"
                        )));
                    }
                    Arc::make_mut(&mut scope.shadowed).insert(name.clone());
                    scope.names.insert(
                        name.clone(),
                        format!("{}.{}", scope.path, scope_component(&name)),
                    );
                }
            }
            let mut signals = Vec::new();
            signals_from_body_item(
                *id,
                self.tree,
                &self.aliases,
                &scope.env,
                None,
                &mut signals,
            )?;
            for signal in signals {
                for key in [
                    signal.name.clone(),
                    parameter_marker(&signal.name),
                    parameters::unbounded_parameter_marker(&signal.name),
                    local_parameter_marker(&signal.name),
                    enum_marker(&signal.name),
                    parameter_width_marker(&signal.name),
                    parameter_signed_marker(&signal.name),
                ] {
                    scope.env.remove(&key);
                }
                scope.literals.remove(&signal.name);
                Arc::make_mut(&mut scope.shadowed).insert(signal.name.clone());
                scope
                    .parameters
                    .retain(|parameter| parameter.name() != signal.name);
                scope.names.insert(
                    signal.name.clone(),
                    format!("{}.{}", scope.path, scope_component(&signal.name)),
                );
            }
        }
        Ok(scope)
    }
}

/// Make `name` in `scope` denote the package item `target`, as an import of
/// the generate block does.
fn bind_import(scope: &mut Scope, name: &str, target: &str) {
    let imported = scope::imported(&scope.state);
    Arc::make_mut(&mut scope.shadowed).insert(name.to_string());
    scope.names.insert(name.to_string(), target.to_string());
    scope.imports.insert(name.to_string(), target.to_string());
    // The constant value and type markers of the item, under the name.
    let names = |key: &str| -> Option<String> {
        if key == target {
            return Some(name.to_string());
        }
        let (prefix, marked) = scope::split_marker(key)?;
        (marked == target).then(|| format!("{prefix}{name}"))
    };
    scope.env.retain(|key, _| {
        key != name && scope::split_marker(key).is_none_or(|(_, marked)| marked != name)
    });
    let constants: Vec<(String, i128)> = scope
        .env
        .iter()
        .filter_map(|(key, value)| Some((names(key)?, *value)))
        .collect();
    scope.env.extend(constants);
    let literal = scope
        .literals
        .get(target)
        .cloned()
        .or_else(|| imported.parameter_values.get(target).cloned());
    match &literal {
        Some(value) => {
            scope.literals.insert(name.to_string(), value.clone());
        }
        None => {
            scope.literals.remove(name);
        }
    }
    scope
        .parameters
        .retain(|parameter| parameter.name() != name);
    // Keep the imported binding's shape and four-state literal as well as its
    // numeric markers. Later localparam selections must use this block's
    // import, which can shadow a module import with different declared bounds.
    if let Some(parameter) = imported
        .parameters
        .iter()
        .find(|parameter| parameter.name() == target)
    {
        let mut parameter = parameter.clone();
        parameter.name = name.to_string();
        // The exported literal is already evaluated in the package scope.
        // Keep that snapshot when rebuilding declaration-order environments;
        // the initializer may name a package dependency absent from this scope.
        if let Some(value) = literal.and_then(|expr| expr_to_const(&scope.state, expr)) {
            parameter.value = Some(value);
        }
        scope.parameters.push(parameter);
    }
}

// Bootstrap only module-scope functions; collecting the full function map would
// itself elaborate generate blocks and recurse into the conditions being decided.
fn module_constant_functions(
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
    literals: &HashMap<String, Expr>,
) -> Converted<HashMap<String, Function>> {
    let dimensions = PackedDimensions::new(HashMap::default(), env, aliases);
    let types = parameter_types_from_const_env(env);
    let mut roots = Vec::new();
    let mut pending = tree.body_membership()?;
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(AnalyzerError::Parse(
                "repeated native function scope owner".into(),
            ));
        }
        match &tree.declarations().body_item(id).kind {
            celox_sv_syntax::BodyItemKind::Region(items) => {
                pending.extend(items);
                continue;
            }
            celox_sv_syntax::BodyItemKind::Unit(_)
            | celox_sv_syntax::BodyItemKind::Conditional { .. }
            | celox_sv_syntax::BodyItemKind::Case { .. }
            | celox_sv_syntax::BodyItemKind::Loop { .. } => continue,
            _ => {}
        }
        roots.push(Item {
            state: tree.state.clone(),
            id,
            env: env.clone().into(),
            literals: literals.clone().into(),
            parameter_dimensions: Arc::default(),
            qualified_types: SharedMap::default(),
            names: SharedMap::default(),
            imports: SharedMap::default(),
            shadowed: Arc::default(),
            scope: String::new(),
        });
    }
    let mut functions = functions::native_functions(
        &roots,
        tree,
        &dimensions,
        lifetimes::automatic_by_default(tree),
    )?;
    let mut calls = HashMap::default();
    for function in functions.values_mut() {
        let mut pending = function.body.iter().collect::<Vec<_>>();
        let mut used = HashSet::default();
        let mut coordinates = Vec::new();
        let mut callees = Vec::new();
        while let Some(expr) = pending.pop() {
            match expr {
                Expr::Ident(name) => {
                    used.insert(name.clone());
                }
                Expr::Select { expr, msb, lsb, .. } => {
                    pending.push(expr);
                    coordinates.extend([msb, lsb]);
                }
                Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => pending.push(expr),
                Expr::Concat(parts) => pending.extend(parts),
                Expr::RepeatConcat { count, parts } => {
                    pending.extend(parts);
                    coordinates.push(count);
                }
                Expr::Binary { left, right, .. } => pending.extend([left.as_ref(), right.as_ref()]),
                Expr::Mux {
                    condition,
                    then_expr,
                    else_expr,
                } => pending.extend([condition.as_ref(), then_expr.as_ref(), else_expr.as_ref()]),
                Expr::Call { callee, args, .. } => {
                    if let Callee::Subroutine(name) = callee {
                        callees.push(name.clone());
                    }
                    pending.extend(args.iter().flatten());
                }
                Expr::Inside { expr, items } => {
                    pending.push(expr);
                    pending.extend(items.iter().flat_map(|item| item.exprs()));
                }
                _ => {}
            }
        }
        while let Some(expr) = coordinates.pop() {
            match expr {
                ConstExpr::Ident(name) => {
                    used.insert(name.clone());
                }
                ConstExpr::Select { expr, bit } => {
                    coordinates.extend([expr.as_ref(), bit.as_ref()])
                }
                ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                    coordinates.extend([expr.as_ref(), msb.as_ref(), lsb.as_ref()])
                }
                ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
                    coordinates.push(expr)
                }
                ConstExpr::Binary { left, right, .. } => {
                    coordinates.extend([left.as_ref(), right.as_ref()])
                }
                ConstExpr::Mux {
                    condition,
                    then_expr,
                    else_expr,
                } => {
                    coordinates.extend([condition.as_ref(), then_expr.as_ref(), else_expr.as_ref()])
                }
                ConstExpr::Function { callee, args, .. } => {
                    if let Callee::Subroutine(name) = callee {
                        callees.push(name.clone());
                    }
                    coordinates.extend(args);
                }
                ConstExpr::Literal(_) => {}
            }
        }
        let mut bindings = HashMap::default();
        for name in used {
            if function.params.iter().any(|param| param.name == name) {
                continue;
            }
            let value = literals
                .get(&name)
                .cloned()
                .or_else(|| {
                    env.get(&name).map(|value| {
                        Expr::Literal(types.get(&name).map_or_else(
                            || value.to_string(),
                            |ty| format_typed_parameter_literal(*value, ty.width, ty.signed),
                        ))
                    })
                })
                .unwrap_or_else(|| Expr::Ident(format!("\0constant_function:{name}")));
            bindings.insert(name, value);
        }
        function.body = function
            .body
            .take()
            .map(|body| substitute_expr_idents(body, &bindings));
        calls.insert(function.name.clone(), callees);
    }
    // The existing inliner bounds call depth, but branching recursion would
    // still expand exponentially. Keep recursive call graphs unsupported here.
    fn acyclic(
        name: &str,
        calls: &HashMap<String, Vec<String>>,
        active: &mut HashSet<String>,
    ) -> bool {
        let Some(children) = calls.get(name) else {
            return true;
        };
        if !active.insert(name.to_string()) {
            return false;
        }
        let valid = children.iter().all(|child| acyclic(child, calls, active));
        active.remove(name);
        valid
    }
    functions.retain(|name, _| acyclic(name, &calls, &mut HashSet::default()));
    Ok(functions)
}

// Inspect dimension expressions only, excluding runtime declaration initializers.
fn native_expression_dependencies(
    root: ExprId,
    native: &celox_sv_syntax::Declarations,
) -> HashSet<String> {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    let mut names = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let celox_sv_syntax::ExprKind::Name(name) = &native.expression(id).kind {
            names.insert(instances::normalize_identifier(&name.spelling));
        }
        pending.extend(parameters::skipped::children(id, native));
    }
    names
}
fn native_dimension_dependencies(
    dimensions: &[celox_sv_syntax::Dimension],
    native: &celox_sv_syntax::Declarations,
) -> HashSet<String> {
    use celox_sv_syntax::DimensionKind;
    let mut names = HashSet::default();
    for dimension in dimensions {
        let roots = match dimension.kind {
            DimensionKind::Index(id) | DimensionKind::Queue(Some(id)) => vec![id],
            DimensionKind::Range { left, right } => vec![left, right],
            DimensionKind::Indexed { base, width, .. } => vec![base, width],
            _ => Vec::new(),
        };
        for root in roots {
            names.extend(native_expression_dependencies(root, native));
        }
    }
    names
}
fn native_type_dependencies(
    root: celox_sv_syntax::TypeId,
    native: &celox_sv_syntax::Declarations,
) -> HashSet<String> {
    use celox_sv_syntax::DataTypeKind;
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    let mut names = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let ty = native.data_type(id);
        names.extend(native_dimension_dependencies(&ty.packed_dimensions, native));
        match &ty.kind {
            DataTypeKind::Named(path) => {
                for part in path {
                    names.insert(instances::normalize_identifier(&part.name.spelling));
                    for argument in part.arguments.iter().flatten() {
                        if let Some(root) = argument.value {
                            names.extend(native_expression_dependencies(root, native));
                        }
                    }
                }
            }
            DataTypeKind::TypeOf(root) => {
                names.extend(native_expression_dependencies(*root, native))
            }
            DataTypeKind::Struct { fields, .. } => {
                for field in fields {
                    pending.push(field.data_type);
                    names.extend(native_dimension_dependencies(&field.dimensions, native));
                }
            }
            DataTypeKind::Enum { base, items } => {
                pending.extend(base.iter().copied());
                for item in items {
                    names.extend(native_dimension_dependencies(&item.dimensions, native));
                    if let Some(root) = item.value {
                        names.extend(native_expression_dependencies(root, native));
                    }
                }
            }
            _ => {}
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn analyze(source: &str) -> Result<Source, AnalyzerError> {
        let tree = crate::source_tree::parse_scope_fixture(source, Path::new("generate.sv"))?;
        let tree = AnalysisContext::unspecialized(&tree);
        Source::from_syntax(&tree)
    }

    #[test]
    fn consecutive_parameter_runs_bind_each_prefix_once() {
        for count in [16, 64, 256] {
            let mut code = String::from("module Top(); if (1) begin : g\n");
            for index in 0..count {
                let value = if index + 1 == count {
                    "1".into()
                } else {
                    format!("P{} + 1", index + 1)
                };
                use std::fmt::Write;
                writeln!(code, "localparam logic [31:0] P{index} = {value};").unwrap();
            }
            code.push_str("logic [P0-1:0] data; end endmodule");
            let tree = crate::source_tree::parse_scope_fixture(
                &code,
                Path::new("generate_parameter_prefix.sv"),
            )
            .unwrap();
            let tree = AnalysisContext::unspecialized(&tree);
            parameters::PARAMETER_BINDINGS.with(|bindings| bindings.set(0));
            let active = items(&tree, &HashMap::default(), &HashMap::default()).unwrap();
            let bindings = parameters::PARAMETER_BINDINGS.with(|bindings| bindings.get());
            assert!(
                bindings <= 3 * count,
                "{count} parameters rebound {bindings} times"
            );
            assert_eq!(active.last().unwrap().env["P0"], count as i128);
        }
    }

    #[test]
    fn signals_interrupt_parameter_runs_before_size_queries_resume() {
        let source = analyze("module Top(); if (1) begin : g localparam int A=$bits(s0); logic [3:0] s0; localparam int B=A+2; logic [B-1:0] s1; localparam int C=$bits(s1); localparam int D=C+1; logic [D-1:0] s2; end endmodule").unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        let signals = ir.modules()[0].signals();
        assert_eq!(signals.len(), 3);
        for (signal, width) in signals.iter().zip([4, 6, 7]) {
            assert_eq!(signal.r#type().resolved_width(), Some(width));
        }
    }

    #[test]
    fn cached_generate_types_match_rebuilt_types_for_imports_and_four_state_values() {
        let state = AnalysisState::default();
        let mut base = HashMap::from_iter([("pkg::IMPORTED".into(), -1)]);
        insert_parameter_type_markers(
            &mut base,
            "pkg::IMPORTED",
            ExprType {
                width: 8,
                signed: true,
            },
        );
        let mut cached_env = base.clone();
        let mut rebuilt_env = base;
        let mut cached_literals = HashMap::default();
        let mut rebuilt_literals = HashMap::default();
        let mut types = parameter_types_from_const_env(&cached_env);
        for (name, value, width) in [
            ("A", ConstExpr::Ident("pkg::IMPORTED".into()), None),
            ("X", ConstExpr::Literal("8'bx".into()), Some(8)),
            ("W", ConstExpr::Literal("129'b1".into()), Some(129)),
            ("Y", ConstExpr::Ident("X".into()), Some(8)),
            (
                "Z",
                ConstExpr::Binary {
                    left: Box::new(ConstExpr::Ident("A".into())),
                    op: BinaryOp::Add,
                    right: Box::new(ConstExpr::Literal("2".into())),
                },
                Some(32),
            ),
        ] {
            let parameter = Parameter::new(
                name.into(),
                Some(value),
                width,
                width.map(|_| false),
                false,
                width.is_some(),
                true,
            );
            bind_generate_parameter(
                &state,
                parameter.clone(),
                &mut rebuilt_env,
                &mut rebuilt_literals,
            );
            constants::bind_generate_parameter_with_types(
                &state,
                parameter,
                &mut cached_env,
                &mut cached_literals,
                &mut types,
            );
            assert_eq!(cached_env, rebuilt_env);
            assert_eq!(cached_literals, rebuilt_literals);
            assert_eq!(types, parameter_types_from_const_env(&rebuilt_env));
        }
    }

    #[test]
    fn generated_items_share_names_but_keep_mutations_in_their_own_snapshot() {
        let tree = crate::source_tree::parse_scope_fixture(
            "module Top(); if (1) begin : g logic x; logic y; end endmodule",
            Path::new("generate_name_snapshots.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let active = items(&tree, &HashMap::default(), &HashMap::default()).unwrap();
        assert_eq!(active.len(), 2);
        assert!(std::ptr::eq(&*active[0].names, &*active[1].names));
        assert!(Arc::ptr_eq(&active[0].shadowed, &active[1].shadowed));
        let mut local = active[0].clone();
        local.names.remove("x");
        Arc::make_mut(&mut local.shadowed).remove("x");
        assert_eq!(local.name("x"), "x");
        for sibling in &active {
            assert_eq!(sibling.name("x"), "g.x");
            assert_eq!(sibling.name("y"), "g.y");
            assert!(sibling.shadowed.contains("x"));
        }
    }

    #[test]
    fn grouped_parameters_keep_only_the_scheduled_sibling_and_mask_outer_values() {
        for keyword in ["localparam", "parameter"] {
            let code = format!(
                r#"
                module Top #(parameter B=99) (output logic [31:0] y);
                    if (1) begin : g
                        {keyword} int A=B+1, B=C+1, C=2;
                        {keyword} logic [7:0] X='1, Y=X-1;
                        logic [A-1:0] data;
                        assign y=A+B+C+X+Y+$bits(data);
                    end
                endmodule
            "#
            );
            let tree =
                crate::source_tree::parse_scope_fixture(&code, Path::new("scheduled_siblings.sv"))
                    .unwrap();
            let tree = AnalysisContext::unspecialized(&tree);
            let active = items(
                &tree,
                &HashMap::from_iter([("B".into(), 99)]),
                &HashMap::default(),
            )
            .unwrap();
            let assignment = active
                .iter()
                .find(|item| {
                    matches!(
                        tree.declarations().body_item(item.id).kind,
                        celox_sv_syntax::BodyItemKind::Assign(_)
                    )
                })
                .unwrap();
            for (name, value) in [("A", 4), ("B", 3), ("C", 2), ("X", 255), ("Y", 254)] {
                assert_eq!(assignment.env[name], value, "{keyword} {name}");
            }
            let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
            assert_eq!(ir.modules()[0].signals()[0].name(), "g.data");
            assert_eq!(
                ir.modules()[0].signals()[0].r#type().resolved_width(),
                Some(4)
            );
        }
    }

    #[test]
    fn reports_a_ready_declaration_error_before_a_remaining_cycle() {
        let error = analyze(
            r#"
            module Top();
                if (1) begin : g
                    localparam A=B, B=A;
                    localparam chandle R=null;
                end
            endmodule
        "#,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported parameter data type"),
            "{error}"
        );
    }

    #[test]
    fn coerces_genvar_assignments_before_naming_or_updating_iterations() {
        for (scheme, names) in [
            ("genvar i=32'hffffffff; i!=0; i/=2", vec!["g[-1].tmp"]),
            (
                "genvar i=2147483647; i!=-2147483647; i++",
                vec!["g[-2147483648].tmp", "g[2147483647].tmp"],
            ),
            ("genvar i=1; i!=0; i=64'h100000000", vec!["g[1].tmp"]),
            ("genvar i=1073741824; i!=0; i*=4", vec!["g[1073741824].tmp"]),
        ] {
            let source = analyze(&format!(
                "module Top(); for ({scheme}) begin : g logic tmp; end endmodule"
            ))
            .unwrap();
            let actual: Vec<_> = source.modules()[0]
                .signals()
                .iter()
                .map(|signal| signal.name())
                .collect();
            assert_eq!(actual, names, "{scheme}");
        }
    }

    #[test]
    fn predeclares_generate_functions_before_nested_conditions() {
        let error = analyze(
            r#"
            module Top(output logic y);
                function automatic bit f(); return 1; endfunction
                if (1) begin : g
                    if (f()) assign y=1;
                    else assign y=0;
                    function automatic bit f(); return 0; endfunction
                end
            endmodule
        "#,
        )
        .unwrap_err();
        // IEEE 1800-2023 13.4.3 excludes generate-local constant functions.
        // Never silently select the same-named module function instead.
        assert!(
            error
                .to_string()
                .contains("unknown conditional-generate condition"),
            "{error}"
        );
    }

    #[test]
    fn does_not_use_a_module_function_hidden_by_a_generate_function() {
        let source = r#"
            module Top(output logic y);
                function automatic bit enabled(); return 1; endfunction
                if (1) begin : g
                    function automatic bit enabled(); return 0; endfunction
                    if (enabled()) assign y=1;
                    else assign y=0;
                end
            endmodule
        "#;
        let error = analyze(source).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown conditional-generate condition"),
            "{error}"
        );
    }

    #[test]
    fn rejects_recursive_constant_function_expansion() {
        let source = r#"
            module Top(output logic y);
                function automatic int recurse(); return recurse() + recurse(); endfunction
                if (recurse()) assign y=1;
                else assign y=0;
            endmodule
        "#;
        let error = analyze(source).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown conditional-generate condition"),
            "{error}"
        );
    }

    #[test]
    fn preserves_constant_function_masks_and_case_context() {
        let source = r#"
            module Top(output logic y);
                function automatic logic unknown(); return 1'bx; endfunction
                function automatic logic [3:0] narrow(); return 4'hf; endfunction
                if (unknown()) begin initial $fatal; end
                else begin
                    case (narrow() + 4'd1)
                        8'd16: assign y=1;
                        default: initial $fatal;
                    endcase
                end
            endmodule
        "#;
        analyze(source).unwrap();
    }

    #[test]
    fn rejects_runtime_function_reads_even_when_generate_locals_shadow_them() {
        let source = r#"
            module Top(input logic a, output logic y);
                function automatic logic read_input(); return a; endfunction
                if (1) begin : g
                    localparam a=1;
                    if (read_input()) assign y=1;
                    else assign y=0;
                end
            endmodule
        "#;
        let error = analyze(source).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown conditional-generate condition"),
            "{error}"
        );
    }

    #[test]
    fn retains_loop_ancestry_through_nested_conditional_blocks() {
        let source = r#"
            module Top();
                for (genvar i=0; i<1; i++) begin : g
                    if (1) begin : nested
                        function automatic bit f(); return 1; endfunction
                    end
                end
            endmodule
        "#;
        let error = analyze(source).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("function declaration inside loop-generate"),
            "{error}"
        );
    }

    #[test]
    fn resolves_shared_packed_and_individual_unpacked_dimensions() {
        for declaration in [
            "logic [P-1:0] a[W-1:0], b[N-1:0];",
            "wire [P-1:0] a[W-1:0], b[N-1:0];",
            "T a[W-1:0], b[N-1:0];",
            "logic [P-1:0] a[W-1:0], b[N'(N-1):0];",
        ] {
            let source = format!(
                r#"
                module Top(output logic y);
                    typedef logic [2:0] T;
                    if (1) begin : g
                        localparam N = $size(a);
                        localparam W = 2;
                        localparam P = 3;
                        {declaration}
                        assign a[0] = 0;
                        assign a[1] = 0;
                        assign b[0] = 0;
                        assign b[1] = 0;
                        if (($bits(b) == 6) && ($size(b) == 2)) assign y=1;
                        else begin initial $fatal; end
                    end
                endmodule
            "#
            );
            analyze(&source).unwrap_or_else(|error| panic!("{declaration}: {error}"));
        }
    }

    #[test]
    fn scopes_all_function_return_metadata_and_keeps_hidden_outer_binding() {
        let tree = crate::source_tree::parse_scope_fixture(
            r#"
            module Top();
                if (1) begin : g
                    function automatic bit signed [3:0] f(); return 0; endfunction
                end
            endmodule
        "#,
            Path::new("generate_metadata.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let active = items(&tree, &HashMap::default(), &HashMap::default()).unwrap();
        let outer = FunctionReturnMetadata {
            arguments: None,
            width: Some(8),
            first_packed_dimension_width: Some(2),
            dimensions: Some(2),
            signed: false,
            is_2state: false,
            string: false,
        };
        let inner = FunctionReturnMetadata {
            arguments: None,
            width: Some(4),
            first_packed_dimension_width: Some(4),
            dimensions: Some(1),
            signed: true,
            is_2state: true,
            string: false,
        };
        let dimensions = PackedDimensions {
            function_return_types: [
                ("f".to_string(), outer.clone()),
                ("g.f".to_string(), inner.clone()),
            ]
            .into_iter()
            .collect(),
            ..PackedDimensions::default()
        };
        let local = active[0].dimensions(&dimensions);
        assert_eq!(local.function_return_types["f"], inner);
        assert_eq!(local.function_return_types["g.f"], inner);
        assert_eq!(
            local.function_return_types[&format!("{OUTER_BINDING}f")],
            outer
        );
        assert_eq!(dimensions.function_return_types["f"], outer);
    }

    #[test]
    fn rejects_nonconstant_truth_and_unknown_genvar_values() {
        for body in [
            "if (a) assign y=1; else assign y=0;",
            "for (genvar i=0; a; i++) assign y=0;",
            "for (genvar i=1'bx; i<1; i++) assign y=0;",
        ] {
            let source = format!("module Top(input logic a, output logic y); {body} endmodule");
            assert!(
                analyze(&source).is_err(),
                "accepted nonconstant or invalid generate: {body}"
            );
        }
    }

    #[test]
    fn rejects_cyclic_generate_signal_size_dependencies() {
        let source = "module Top(); if (1) begin : g localparam W=$bits(data); logic [W-1:0] data; end endmodule";
        let error = analyze(source).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cyclic generate-local parameter dependency"),
            "{error}"
        );
    }

    #[test]
    fn genvar_scopes_discard_all_inherited_packed_bounds() {
        let tree = crate::source_tree::parse_scope_fixture(
            "module Top(output logic [1:0] y); localparam logic signed [2:1][4:0] i=9; for (genvar i=0; i<2; i++) begin : g assign y[i]=i; end endmodule",
            Path::new("genvar_packed_bounds.sv"),
        ).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let parameters = parameters_from_scope(
            &tree,
            &HashMap::default(),
            &HashMap::default(),
            &HashMap::default(),
        )
        .unwrap();
        let mut env = HashMap::default();
        extend_const_env_with_parameters(&tree.state, &mut env, &parameters);
        assert_eq!(env[&parameter_dimensions_marker("i")], 2);
        let active = items(&tree, &env, &HashMap::default()).unwrap();
        let iterations: Vec<_> = active
            .iter()
            .filter(|item| !item.scope.is_empty())
            .collect();
        assert_eq!(iterations.len(), 2);
        for (value, item) in iterations.iter().enumerate() {
            assert_eq!(item.env["i"], value as i128);
            assert_eq!(item.env[&parameter_width_marker("i")], 32);
            assert_eq!(item.env[&parameter_signed_marker("i")], 1);
            assert_eq!(item.env[&parameter_rank_marker("i")], 1);
            assert_eq!(item.env[&parameters::unbounded_parameter_marker("i")], 0);
            assert!(!item.env.contains_key(&parameter_dimensions_marker("i")));
            assert!(!item.parameter_dimensions.contains_key("i"));
            for dimension in 0..2 {
                for bound in ["left", "right"] {
                    assert!(
                        !item
                            .env
                            .contains_key(&parameter_dimension_marker("i", dimension, bound))
                    );
                }
            }
        }
        assert_eq!(env[&parameter_dimension_marker("i", 0, "left")], 2);
        assert_eq!(env[&parameter_dimension_marker("i", 1, "right")], 0);
    }

    #[test]
    fn folds_shadowing_genvars_in_nested_selects() {
        let source = analyze(
            "module Top #(parameter logic [7:0] i=9)(output logic [2:0] y); for (genvar i=-1; i<2; i++) begin : g if (1) assign y[i+1]=i; end endmodule",
        ).unwrap();
        assert_eq!(source.modules()[0].assignments().len(), 3);
        for (index, assignment) in source.modules()[0].assignments().iter().enumerate() {
            let LValue::Select { lsb, .. } = &assignment.lhs else {
                panic!("{assignment:?}")
            };
            assert_eq!(
                eval_ast_const_expr(&AnalysisState::default(), lsb, &HashMap::default()),
                Some(index as i128),
                "{assignment:?}"
            );
        }
    }

    #[test]
    fn reserves_genvar_in_its_own_block_but_allows_nested_shadowing() {
        for declaration in ["logic i;", "wire i;", "localparam i = 2;"] {
            let source = format!(
                "module Top(); for (genvar i=0; i<1; i++) begin : g {declaration} end endmodule"
            );
            let error = analyze(&source).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("duplicate generate-local declaration `i`"),
                "{error}"
            );
        }
        analyze("module Top(output logic y); for (genvar i=0; i<1; i++) begin : g if (1) begin : inner localparam i=1; assign y=i; end end endmodule").unwrap();
    }

    #[test]
    fn rejects_cyclic_generate_localparams_even_when_outer_names_exist() {
        for declarations in ["localparam A=B; localparam B=A;", "localparam A=A;"] {
            let source = format!(
                "module Top(); localparam A=1; if (1) begin : g {declarations} end endmodule"
            );
            let error = analyze(&source).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("cyclic generate-local parameter dependency"),
                "{error}"
            );
        }
    }

    #[test]
    fn gives_nested_instances_and_signals_distinct_scopes() {
        let source = analyze(
            r#"
            module Child(input logic a, output logic y); assign y = a; endmodule
            module Top(input logic [3:0] a, output logic [3:0] y);
                for (genvar i = 0; i < 2; i++) begin : rows
                    for (genvar j = 0; j < 2; j++) begin : columns
                        logic tmp;
                        Child child(.a(a[i*2+j]), .y(tmp));
                        assign y[i*2+j] = tmp;
                    end
                end
            endmodule
        "#,
        )
        .unwrap();
        let top = source.modules().iter().find(|m| m.name() == "Top").unwrap();
        let names: Vec<_> = top.signals().iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
            [
                "rows[0].columns[0].tmp",
                "rows[0].columns[1].tmp",
                "rows[1].columns[0].tmp",
                "rows[1].columns[1].tmp"
            ]
        );
        assert_eq!(top.instances()[3].name(), "rows[1].columns[1].child");
    }

    #[test]
    fn case_generate_uses_case_equality_and_selects_only_one_branch() {
        for selector in ["8'hff", "'1", "4'bxz01"] {
            let source = format!(
                r#"
                module Top(output logic y);
                    case ({selector})
                        8'hff, 4'bxz01: assign y = 1'b1;
                        default: initial $fatal;
                    endcase
                endmodule
            "#
            );
            analyze(&source).unwrap_or_else(|error| panic!("selector {selector}: {error}"));
        }
        analyze(
            r#"
            module Top(output logic y);
                case (3)
                    1: initial $fatal;
                    2: initial $fatal;
                endcase
                assign y = 1'b1;
            endmodule
        "#,
        )
        .unwrap();
    }

    #[test]
    fn directly_nested_conditionals_share_the_outer_generate_scope() {
        let source = analyze(
            r#"
            module Child(); endmodule
            module Top();
                if (0) begin : chosen Child child(); end
                else if (1) begin : chosen Child child(); end
            endmodule
        "#,
        )
        .unwrap();
        assert_eq!(source.modules()[1].instances()[0].name(), "chosen.child");
    }

    #[test]
    fn propagates_case_width_before_arithmetic_and_preserves_self_determined_operands() {
        for (selector, label) in [
            ("4'hf + 4'h1", "8'h10"),
            ("4'hf << 2", "8'h3c"),
            ("&4'hf", "8'h01"),
            ("$clog2(8) + 1", "32'd4"),
        ] {
            let source = format!(
                "module Top(output logic y); case ({selector}) {label}: assign y=1; default: initial $fatal; endcase endmodule"
            );
            analyze(&source).unwrap_or_else(|error| panic!("{selector}: {error}"));
        }
    }

    #[test]
    fn validates_only_active_loop_branches_and_preserves_genvar_type() {
        analyze(
            r#"
            module Top(output logic y);
                for (genvar i = -1; i < 0; i++) begin
                    if (&i) assign y = 1'b1;
                    else begin
                        function automatic logic f(); return 0; endfunction
                        initial $fatal;
                    end
                end
            endmodule
        "#,
        )
        .unwrap();
    }

    #[test]
    fn rejects_invalid_generate_bindings_and_nonconstant_cases() {
        for (body, expected) in [
            (
                "if (1) begin logic tmp; logic tmp; end",
                "duplicate generate-local declaration",
            ),
            (
                "if (1) begin localparam P = 0; localparam P = 1; end",
                "duplicate generate-local declaration",
            ),
            (
                "if (1) begin localparam P = 0; logic P; end",
                "duplicate generate-local declaration",
            ),
            (
                "for (genvar i=0; i<1; j++) assign y=0;",
                "genvar update targets a different variable",
            ),
            (
                "case(a) default: assign y=0; endcase",
                "unknown case-generate selector",
            ),
            (
                "case(0) default: assign y=0; default: assign y=1; endcase",
                "duplicate case-generate default",
            ),
            (
                "if (1) begin typedef logic T; typedef bit T; end",
                "duplicate generate-local declaration",
            ),
            (
                "if (1) begin typedef enum logic { A,B } T; typedef enum logic { A,C } U; end",
                "duplicate generate-local declaration",
            ),
            (
                "if (1) begin typedef logic T; logic T; end",
                "duplicate generate-local declaration",
            ),
            (
                "if (1) begin typedef enum logic { A,B } T; localparam A=1; end",
                "duplicate generate-local declaration",
            ),
            (
                "if (1) begin parameter type T=logic; end",
                "generate-local type parameter",
            ),
            (
                "for (genvar i=0; i<1; i++) begin function automatic logic f(); return 0; endfunction end",
                "function declaration inside loop-generate",
            ),
        ] {
            let source = format!("module Top(input logic a, output logic y); {body} endmodule");
            let error = analyze(&source)
                .err()
                .unwrap_or_else(|| panic!("unexpected success: {body}"));
            assert!(error.to_string().contains(expected), "{body}: {error}");
        }
    }

    #[test]
    fn reference_scan_covers_nested_branches_arrays_packages_and_escaped_names() {
        let code = r#"
            module Top;
                localparam UNUSED = missing(ONLY_INITIALIZER);
                localparam A = 'x;
                localparam B = A;
                localparam logic [7:0] ARR [0:0] = '{ARRAY_INPUT};
                function automatic logic f(); return FUNCTION_INPUT; endfunction
                generate
                    if (0) begin : inactive
                        assign out = INACTIVE_INPUT;
                    end
                    for (genvar i = 0; i < LOOP_BOUND; i++) begin : loop_scope
                        localparam C = NESTED_INPUT;
                    end
                endgenerate
                assign out = \escaped.name ;
            endmodule
            package pkg;
                localparam UNUSED = only_in_initializer;
                function automatic logic f(); return PACKAGE_INPUT; endfunction
            endpackage
        "#;
        let tree =
            crate::source_tree::parse_scope_fixture(code, Path::new("literal_references.sv"))
                .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        for handle in tree.root_scopes().unwrap() {
            let selected = tree.scoped(handle).unwrap();
            if selected.root_unit().unwrap().kind == celox_sv_syntax::UnitKind::Module {
                let refs = parameter_literal_references(
                    &AnalysisContext::unspecialized(&selected),
                    LiteralUse::All,
                )
                .unwrap();
                for name in [
                    "ARRAY_INPUT",
                    "FUNCTION_INPUT",
                    "INACTIVE_INPUT",
                    "LOOP_BOUND",
                    "NESTED_INPUT",
                    "\\escaped.name",
                ] {
                    assert!(refs.contains(name), "{name}: {refs:?}");
                }
                assert!(!refs.contains("ONLY_INITIALIZER"));
            } else if selected.root_unit().unwrap().kind == celox_sv_syntax::UnitKind::Package {
                let refs = parameter_literal_references(
                    &AnalysisContext::unspecialized(&selected),
                    LiteralUse::All,
                )
                .unwrap();
                assert!(refs.contains("PACKAGE_INPUT"));
                assert!(!refs.contains("only_in_initializer"));
            }
        }
    }

    #[test]
    fn unused_unresolved_chains_do_not_populate_item_literals() {
        use std::fmt::Write;

        for count in [16, 64, 256] {
            for doubling in [false, true] {
                let declaration = if doubling {
                    // Expanding the previous unresolved value twice at every
                    // declaration would make the unused expression trees grow
                    // exponentially before this function is installed.
                    "function automatic int next_value(input int left, right); return left + right; endfunction\n"
                } else {
                    "function automatic int next_value(input int value); return value + 1; endfunction\n"
                };
                let mut code = format!("module Top; {declaration}");
                for i in 0..count {
                    let arg = match (i, doubling) {
                        (0, false) => "0".into(),
                        (0, true) => "1, 1".into(),
                        (_, false) => format!("P{}", i - 1),
                        (_, true) => format!("P{0}, P{0}", i - 1),
                    };
                    writeln!(code, "localparam int P{i} = next_value({arg});").unwrap();
                }
                code.push_str("endmodule\n");
                let tree =
                    crate::source_tree::parse_scope_fixture(&code, Path::new("unused_chain.sv"))
                        .unwrap();
                let tree = AnalysisContext::unspecialized(&tree);
                let active = items(&tree, &HashMap::default(), &HashMap::default()).unwrap();
                assert!(active.iter().all(|item| item.literals.is_empty()));
                let ir =
                    crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
                for (i, parameter) in ir.modules()[0].parameters().iter().enumerate() {
                    let expected = if !doubling {
                        i as i128 + 1
                    } else if i >= 31 {
                        0
                    } else {
                        (1u32 << (i + 1)) as i32 as i128
                    };
                    assert_eq!(parameter.resolved_value(), Some(expected));
                }
            }
        }
    }

    #[test]
    fn referenced_four_state_dependencies_keep_their_full_values() {
        let code = "module Top(output logic [7:0] out); localparam logic [7:0] A=8'bxz010101; localparam logic [7:0] B=A; localparam logic [7:0] UNUSED='x; assign out=B; endmodule";
        let tree =
            crate::source_tree::parse_scope_fixture(code, Path::new("needed_literals.sv")).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let env = HashMap::default();
        let aliases = HashMap::default();
        let parameters = parameters_from_scope(&tree, &aliases, &env, &HashMap::default()).unwrap();
        let full = parameter_value_env(&tree.state, &parameters, &env);
        let active = items(&tree, &env, &aliases).unwrap();
        for item in active {
            assert_eq!(item.literals.get("B"), full.get("B"));
            assert_eq!(item.literals.get("A"), full.get("A"));
            assert!(!item.literals.contains_key("UNUSED"));
        }
    }

    #[test]
    fn subroutine_reference_scan_excludes_rtl_only_bindings() {
        let code = r#"
            module Top;
                localparam INITIALIZER = ONLY_INITIALIZER;
                function automatic logic f(); return FUNCTION_INPUT; endfunction
                task t(input logic [TASK_WIDTH-1:0] value); endtask
                assign out = RTL_INPUT;
                always_comb out = ALWAYS_INPUT;
                initial out = INITIAL_INPUT;
                initial begin : named logic [PROCEDURAL_WIDTH-1:0] local_signal; end
                logic [RTL_WIDTH-1:0] signal;
                if (CONDITION) begin : g
                    localparam P = LOCAL_INPUT;
                    logic [SIGNAL_WIDTH-1:0] x;
                    assign x = GENERATE_INPUT;
                end
                generate
                    assign other = REGION_INPUT;
                endgenerate
            endmodule
            package pkg;
                localparam INITIALIZER = ONLY_INITIALIZER;
                function automatic logic f(); return PACKAGE_INPUT; endfunction
            endpackage
        "#;
        let tree =
            crate::source_tree::parse_scope_fixture(code, Path::new("subroutine_references.sv"))
                .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        for handle in tree.root_scopes().unwrap() {
            let selected = tree.scoped(handle).unwrap();
            if selected.root_unit().unwrap().kind == celox_sv_syntax::UnitKind::Module {
                let refs = parameter_literal_references(
                    &AnalysisContext::unspecialized(&selected),
                    LiteralUse::Subroutines,
                )
                .unwrap();
                for name in [
                    "FUNCTION_INPUT",
                    "TASK_WIDTH",
                    "RTL_WIDTH",
                    "PROCEDURAL_WIDTH",
                    "CONDITION",
                    "LOCAL_INPUT",
                    "SIGNAL_WIDTH",
                    "GENERATE_INPUT",
                    "REGION_INPUT",
                ] {
                    assert!(refs.contains(name), "{name}: {refs:?}");
                }
                for name in [
                    "ONLY_INITIALIZER",
                    "RTL_INPUT",
                    "ALWAYS_INPUT",
                    "INITIAL_INPUT",
                ] {
                    assert!(!refs.contains(name), "{name}: {refs:?}");
                }
            } else if selected.root_unit().unwrap().kind == celox_sv_syntax::UnitKind::Package {
                let refs = parameter_literal_references(
                    &AnalysisContext::unspecialized(&selected),
                    LiteralUse::Subroutines,
                )
                .unwrap();
                assert!(refs.contains("PACKAGE_INPUT"));
                assert!(!refs.contains("ONLY_INITIALIZER"));
            }
        }
    }

    #[test]
    fn subroutine_literals_preserve_functions_tasks_and_generate_scopes() {
        let code = r#"
            module Top(output logic [7:0] out);
                localparam logic [7:0] A = 8'bxz010101;
                localparam logic [7:0] B = A;
                localparam logic [7:0] RTL_ONLY = 'x;
                function automatic logic [7:0] f(); return B; endfunction
                task t(output logic [7:0] result); result = A; endtask
                if (1) begin : g
                    localparam logic [7:0] C = B;
                    logic [7:0] x;
                    assign x = C;
                end
                assign out = RTL_ONLY;
            endmodule
        "#;
        let tree =
            crate::source_tree::parse_scope_fixture(code, Path::new("subroutine_literals.sv"))
                .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let env = HashMap::default();
        let aliases = HashMap::default();
        let full = items(&tree, &env, &aliases).unwrap();
        let projected = subroutine_items(&tree, &env, &aliases).unwrap();
        assert_eq!(projected.len(), full.len());
        for (full, projected) in full.iter().zip(&projected) {
            assert_eq!(projected.env, full.env);
            assert_eq!(projected.names, full.names);
            assert_eq!(projected.shadowed, full.shadowed);
            assert_eq!(projected.parameter_dimensions, full.parameter_dimensions);
            assert_eq!(projected.scope, full.scope);
            for name in ["A", "B", "C"] {
                assert_eq!(projected.literals.get(name), full.literals.get(name));
            }
            assert!(!projected.literals.contains_key("RTL_ONLY"));
            assert!(full.literals.contains_key("RTL_ONLY"));
        }
        // The emitted RTL still uses the ordinary collector's complete roots.
        let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
        let crate::ir::Expr::Literal(value) = ir.modules()[0].assignments().last().unwrap().rhs()
        else {
            panic!("RTL-only four-state parameter must remain a literal")
        };
        let value = typecheck::parse_integral_literal(value).unwrap();
        assert_eq!(value.width, 8);
        assert_eq!(value.mask, 255u32.into());
    }

    #[test]
    fn rtl_referenced_unresolved_chains_do_not_expand_during_subroutine_discovery() {
        use std::fmt::Write;

        for count in [16, 64, 256] {
            for doubling in [false, true] {
                let declaration = if doubling {
                    "function automatic int next_value(input int left, right); return left + right; endfunction\n"
                } else {
                    "function automatic int next_value(input int value); return value + 1; endfunction\n"
                };
                let mut code = format!("module Top(output int result); {declaration}");
                for i in 0..count {
                    let arg = match (i, doubling) {
                        (0, false) => "0".into(),
                        (0, true) => "1, 1".into(),
                        (_, false) => format!("P{}", i - 1),
                        (_, true) => format!("P{0}, P{0}", i - 1),
                    };
                    writeln!(code, "localparam int P{i} = next_value({arg});").unwrap();
                }
                writeln!(code, "assign result = P{}; endmodule", count - 1).unwrap();
                let tree =
                    crate::source_tree::parse_scope_fixture(&code, Path::new("rtl_chain.sv"))
                        .unwrap();
                let tree = AnalysisContext::unspecialized(&tree);
                let active =
                    subroutine_items(&tree, &HashMap::default(), &HashMap::default()).unwrap();
                assert!(active.iter().all(|item| item.literals.is_empty()));
                let ir =
                    crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
                let parameters = ir.modules()[0].parameters();
                assert_eq!(parameters.len(), count);
                for (i, parameter) in parameters.iter().enumerate() {
                    let expected = if !doubling {
                        i as i128 + 1
                    } else if i >= 31 {
                        0
                    } else {
                        (1u32 << (i + 1)) as i32 as i128
                    };
                    assert_eq!(parameter.resolved_value(), Some(expected));
                }
                let crate::ir::Expr::Literal(value) = ir.modules()[0].assignments()[0].rhs() else {
                    panic!("resolved chain must reach RTL as a literal")
                };
                let value = typecheck::parse_integral_literal(value).unwrap();
                assert_eq!(
                    value.value,
                    if doubling {
                        if count >= 32 { 0 } else { 1u32 << count }
                    } else {
                        count as u32
                    }
                    .into()
                );
                assert_eq!(value.mask, 0u32.into());
                assert_eq!(value.width, 32);
            }
        }
    }

    #[test]
    fn subroutine_projection_preserves_generate_diagnostics() {
        for body in [
            "if (UNKNOWN) begin : g logic x; end",
            "if (1) begin : g parameter type T=logic; end",
            "for (genvar i=0; i<1; i++) begin : g function logic f(); return 0; endfunction end",
            "if (1) begin : g localparam P=Q; localparam Q=P; end",
        ] {
            let code = format!("module Top; {body} endmodule");
            let tree = crate::source_tree::parse_scope_fixture(
                &code,
                Path::new("subroutine_diagnostics.sv"),
            )
            .unwrap();
            let tree = AnalysisContext::unspecialized(&tree);
            let full = items(&tree, &HashMap::default(), &HashMap::default())
                .err()
                .expect(body);
            let projected = subroutine_items(&tree, &HashMap::default(), &HashMap::default())
                .err()
                .expect(body);
            assert_eq!(projected.to_string(), full.to_string(), "{body}");
        }
    }
}
