//! Shared, bounded elaboration of generate scopes before collecting RTL objects.
//!
//! IEEE 1800-2023 sections 12.5 and 27.4–27.6 define the comparison and scope
//! rules used here. Generate-local typedefs and functions in loop-generate
//! remain unsupported; inactive branches are not lowered.
use super::*;
use celox_sv_syntax::ExprId;
mod elaboration;
mod memo;
pub(super) use memo::{CallSites, memoize, restore};

mod dependency_order;
use dependency_order::DependencyOrder;
mod scope_views;
pub(super) use scope_views::ScopeViews;

// Temporary identifiers protect definition-site bindings while functions are
// inlined into a generate scope. They are removed when that item's names are
// qualified, and never reach the public analyzer IR.
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

/// The name of the instance that `path` names from generate scope `scope`
/// (the module for an empty one), as [`Item::instance_name`] spells it.
/// Each component keeps its parsed generate index separate from its name:
/// an escaped name may itself contain `[` (IEEE 1800-2023 5.6.1), which is
/// not an index.
pub(super) fn scoped_instance_name(scope: &str, path: &[(String, Option<i128>)]) -> String {
    match path {
        [(name, None)] if scope.is_empty() => name.clone(),
        [(name, Some(index))] if scope.is_empty() => format!("{name}[{index}]"),
        _ => {
            let mut name = scope.to_string();
            for (component, index) in path {
                if !name.is_empty() {
                    name.push('.');
                }
                // The index of a block follows its name.
                name.push_str(&scope_component(component));
                if let Some(index) = index {
                    name.push_str(&format!("[{index}]"));
                }
            }
            name
        }
    }
}

/// The generate scope that encloses `scope`.
pub(super) fn parent_scope(scope: &str) -> &str {
    scope.rsplit_once('.').map_or("", |(parent, _)| parent)
}

#[derive(Clone)]
pub(super) struct Item {
    /// The symbols the scope that elaborated the item imports.
    imported: Arc<scope::ScopeSymbols>,
    pub id: celox_sv_syntax::BodyItemId,
    pub env: SharedEnv,
    pub literals: SharedMap<Expr>,
    parameter_dimensions: Arc<VariablePackedDimensions>,
    qualified_types: SharedAliases,
    constant_types: SharedMap<semantic_types::ConstantType>,
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
    fn type_aliases<'m>(self, aliases: &'m TypeAliases) -> Cow<'m, TypeAliases> {
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

    /// `installed` with each imported name calling the constant function of
    /// its declaration, or `None` if no name rebinds one; a name that imports
    /// no constant function calls none. Calls in the bodies of the functions
    /// it hides keep their definition-site meaning.
    fn visible_constant_functions(
        self,
        installed: &const_functions::ConstantFunctions,
    ) -> Option<const_functions::ConstantFunctions> {
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
        Some(visible)
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
            names: &self.names,
            imports: &self.imports,
            shadowed: &self.shadowed,
        }
    }

    /// The type aliases this item sees, given the module's.
    pub fn type_aliases<'m>(&self, aliases: &'m TypeAliases) -> Cow<'m, TypeAliases> {
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

    /// [`Self::type_aliases`] of a shared table, which stays shared when
    /// the item sees it unchanged.
    pub fn shared_type_aliases(&self, aliases: &SharedAliases) -> SharedAliases {
        match self.type_aliases(aliases) {
            Cow::Borrowed(_) => aliases.clone(),
            Cow::Owned(visible) => visible.into(),
        }
    }

    /// `tree` calling the constant functions this item sees.
    pub fn enter<'a>(&self, tree: &AnalysisContext<'a>) -> AnalysisContext<'a> {
        match self
            .block_imports()
            .visible_constant_functions(&tree.functions)
        {
            Some(functions) => tree.with_functions(functions),
            None => tree.clone(),
        }
    }

    pub fn scope_name(&self) -> &str {
        &self.scope
    }

    /// `tree` analyzing the item, which sees the interface bindings of the
    /// generate scopes that enclose it.
    pub fn scoped<'a>(&self, tree: &AnalysisContext<'a>) -> AnalysisContext<'a> {
        match self.interface_scope() {
            Some(symbols) => tree.importing(symbols),
            None => tree.clone(),
        }
    }

    fn interface_scope(&self) -> Option<Arc<scope::ScopeSymbols>> {
        let imported = &self.imported;
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
        let mut local = (**imported).clone();
        for (_, bindings) in scopes {
            local.interface_members.extend(bindings.members.clone());
            local.interface_types.extend(bindings.types.clone());
            local.interface_arrays.extend(bindings.arrays.clone());
            local.interface_instances.extend(bindings.instances.clone());
        }
        Some(Arc::new(local))
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
        if !self.shadowed.is_empty() || !self.names.is_empty() || !self.constant_types.is_empty() {
            let mut bindings = self.function_aliases((*dimensions.constant_types).clone());
            bindings.extend(
                self.constant_types
                    .iter()
                    .map(|(n, t)| (n.clone(), t.clone())),
            );
            local.constant_types = Arc::new(bindings);
        }
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
            if !dimensions.subroutine_directions.is_empty() {
                local.subroutine_directions =
                    Arc::new(self.function_aliases((*dimensions.subroutine_directions).clone()));
            }
            if !dimensions.function_return_types.is_empty() {
                local.function_return_types = self
                    .function_aliases((*dimensions.function_return_types).clone())
                    .into();
            }
        }
        if let Cow::Owned(aliases) = self.type_aliases(&dimensions.type_aliases) {
            local.type_aliases = aliases.into();
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
            LValue::Object { .. } => lowered_object(),
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
    imported: Arc<scope::ScopeSymbols>,
    path: String,
    in_loop: bool,
    env: SharedEnv,
    literals: SharedMap<Expr>,
    parameters: Vec<Parameter>,
    parameter_dimensions: Arc<VariablePackedDimensions>,
    qualified_types: SharedAliases,
    constant_types: SharedMap<semantic_types::ConstantType>,
    names: SharedMap<String>,
    imports: SharedMap<String>,
    shadowed: Arc<HashSet<String>>,
}

impl Scope {
    fn type_aliases<'m>(&self, aliases: &'m TypeAliases) -> Cow<'m, TypeAliases> {
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
            names: &self.names,
            imports: &self.imports,
            shadowed: &self.shadowed,
        }
    }
}

struct Elaborator<'a, 'b> {
    /// The context of the scope being elaborated, whose constant functions
    /// change with the imports of each generate block.
    tree: AnalysisContext<'a>,
    session: &'b mut Session,
    module_aliases: &'b TypeAliases,
    /// The type aliases of the scope being elaborated.
    aliases: Cow<'b, TypeAliases>,
    remaining: usize,
    functions: HashMap<String, Function>,
    items: Vec<Item>,
}

/// Elaborations of the module being analyzed, see [`memoize`].
pub(super) type Memo = memo::State;

pub(super) fn items(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Result<Vec<Item>, AnalyzerError> {
    items_with_literal_use(tree, session, env, aliases, LiteralUse::All)
}

/// Elaborate the same scopes and diagnostics, retaining module parameter
/// literals needed by subroutines, declarations and generate constructs.
pub(super) fn subroutine_items(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Result<Vec<Item>, AnalyzerError> {
    items_with_literal_use(
        tree,
        session,
        env,
        aliases,
        if tree.root_subroutines {
            LiteralUse::RootSubroutines
        } else {
            LiteralUse::Subroutines
        },
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LiteralUse {
    All,
    Subroutines,
    RootSubroutines,
}

fn items_with_literal_use(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
    literal_use: LiteralUse,
) -> Result<Vec<Item>, AnalyzerError> {
    memo::items(tree, session, env, aliases, literal_use, |session| {
        elaborate(tree, session, env, aliases, literal_use)
    })
}

fn elaborate(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
    literal_use: LiteralUse,
) -> Result<Vec<Item>, AnalyzerError> {
    let parameters = parameters_from_scope(tree, session, aliases, env, &tree.parameter_overrides)?;
    let references = parameter_literal_references(tree, literal_use)?;
    let mut literals = parameters::parameter_value_env_for_references(
        tree,
        session,
        &parameters,
        env,
        &references,
    );
    let functions = if elaboration::needs_functions(tree)? {
        module_constant_functions(tree, session, env, aliases, &literals)?
    } else {
        HashMap::default()
    };
    // Numeric values and their type markers are already carried by `env`.
    literals.retain(|name, _| !env.contains_key(name));
    let scope = Scope {
        imported: scope::imported(tree),
        env: env.clone().into(),
        literals: literals.into(),
        parameter_dimensions: Arc::new(parameter_packed_dimensions(&parameters)),
        parameters,
        ..Scope::default()
    };
    let mut elaborator = Elaborator {
        tree: tree.clone(),
        session,
        module_aliases: aliases,
        aliases: Cow::Borrowed(aliases),
        remaining: MAX_GENERATE_LOOP_EXPANSION,
        functions,
        items: Vec::new(),
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
    let scope = crate::source_tree::Ranges::new(ranges.iter().copied());
    for id in tree.spans().expressions.within(&ranges) {
        let expression = native.expression(id);
        if !scope.contains(expression.tokens) {
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
    fn expression(
        &mut self,
        root: ExprId,
        scope: &Scope,
    ) -> Result<Option<ConstExpr>, AnalyzerError> {
        // A select of a parameter names indices of its declared range,
        // which may be ascending (IEEE 1800-2023 7.4.1, 11.5.1).
        let parameters = PackedDimensions::new(
            scope
                .parameter_dimensions
                .iter()
                .map(|(name, dimensions)| (name.clone(), dimensions.clone()))
                .collect::<HashMap<_, _>>(),
            &scope.env,
            &*self.aliases,
        );
        let Some(expr) = constant_binding::bind(root, &self.tree, self.session, &parameters)?
        else {
            return Ok(None);
        };
        let dimensions = PackedDimensions {
            const_env: scope.env.clone(),
            ..PackedDimensions::default()
        };
        let Some(mut expr) = expr_to_index_const(
            &self.tree,
            self.session,
            substitute_expr_idents(const_expr_to_expr(expr), &scope.literals),
            &dimensions,
        ) else {
            return Ok(None);
        };
        Ok(
            Self::expand_constant_calls(
                &self.functions,
                &self.tree,
                self.session,
                &mut expr,
                scope,
            )
            .map(|()| expr),
        )
    }

    fn expand_constant_calls(
        functions: &HashMap<String, Function>,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        expr: &mut ConstExpr,
        scope: &Scope,
    ) -> Option<()> {
        match expr {
            ConstExpr::Function { callee, args, .. } => {
                for arg in args {
                    Self::expand_constant_calls(functions, tree, session, arg, scope)?;
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
                    && functions.contains_key(name)
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
                        functions,
                        &signedness,
                        0,
                        true,
                    );
                    let expanded =
                        simplify_constant_mux_conditions(tree, session, expanded, &scope.env);
                    let folded = fold_const_integral_expr_preserving_mask(
                        tree, session, expanded, &scope.env,
                    );
                    // Fold only the call: surrounding case arithmetic still needs
                    // the common comparison width, while function results are self-determined.
                    *expr = expr_to_const(session, folded)?;
                }
            }
            ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
                Self::expand_constant_calls(functions, tree, session, expr, scope)?
            }
            ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                Self::expand_constant_calls(functions, tree, session, expr, scope)?;
                Self::expand_constant_calls(functions, tree, session, msb, scope)?;
                Self::expand_constant_calls(functions, tree, session, lsb, scope)?;
            }
            ConstExpr::Select { expr, bit } => {
                Self::expand_constant_calls(functions, tree, session, expr, scope)?;
                Self::expand_constant_calls(functions, tree, session, bit, scope)?;
            }
            ConstExpr::Binary { left, right, .. } => {
                Self::expand_constant_calls(functions, tree, session, left, scope)?;
                Self::expand_constant_calls(functions, tree, session, right, scope)?;
            }
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                Self::expand_constant_calls(functions, tree, session, condition, scope)?;
                Self::expand_constant_calls(functions, tree, session, then_expr, scope)?;
                Self::expand_constant_calls(functions, tree, session, else_expr, scope)?;
            }
            ConstExpr::Ident(_) | ConstExpr::Literal(_) => {}
        }
        Some(())
    }

    fn value(&mut self, root: ExprId, scope: &Scope, detail: &str) -> Result<i128, AnalyzerError> {
        self.expression(root, scope)?
            .and_then(|expr| eval_ast_const_expr(&self.tree.functions, &expr, &scope.env))
            .ok_or_else(|| AnalyzerError::Unsupported(detail.to_string()))
    }

    fn condition(
        &mut self,
        root: ExprId,
        scope: &Scope,
        detail: &str,
    ) -> Result<bool, AnalyzerError> {
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
                typecheck::eval_const_integral_literal_with_types(&expr, &*scope.env, &types)
                    .map(|literal| typecheck::integral_literal_truth(&literal) == Some(true))
                    .or_else(|| {
                        typecheck::eval_const_expr_with_types_in_env(
                            &expr,
                            &const_functions::Environment {
                                constants: &*scope.env,
                                functions: &self.tree.functions,
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
            imported: scope::imported(&self.tree),
            id,
            env: scope.env.clone(),
            literals: scope.literals.clone(),
            parameter_dimensions: scope.parameter_dimensions.clone(),
            qualified_types: scope.qualified_types.clone(),
            constant_types: scope.constant_types.clone(),
            names: scope.names.clone(),
            imports: scope.imports.clone(),
            shadowed: scope.shadowed.clone(),
            scope: scope.path.clone(),
        });
    }

    fn constants_and_signal_types(
        &mut self,
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
                scope.constant_types.remove(&name);
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
                ]
                .into_iter()
                .chain(parameters::member_keys(&name, &scope.env))
                {
                    scope.env.remove(&key);
                }
                scope.literals.remove(&name);
                let mut dependencies = shared_dependencies.clone();
                dependencies.extend(native_dimension_dependencies(dimensions, native));
                signal_declarations.push((name, *id, dependencies));
            }
        }
        let mut pending = Vec::new();
        let mut parameter_names = HashSet::default();
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
                parameter_names.insert(name.clone());
                let mut dependencies = native_type_dependencies(data_type, native);
                if let Some(root) = default {
                    dependencies.extend(native_expression_dependencies(root, native));
                }
                scope.constant_types.remove(&name);
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
                ]
                .into_iter()
                .chain(parameters::member_keys(&name, &scope.env))
                {
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
                    &self.tree,
                    self.session,
                    &self.aliases,
                    None,
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
                            && eval_ast_const_expr(&self.tree.functions, bound, &scope.env)
                                .is_none()
                        {
                            return Err(AnalyzerError::Unsupported(
                                "numeric use of unbounded parameter in generate signal range"
                                    .into(),
                            ));
                        }
                    }
                }
                extend_const_env_with_variable_types(
                    &self.tree.functions,
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
                        &self.tree,
                        self.session,
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
                            &parameters::ownership::body(id, &self.tree)?,
                            &self.tree,
                            self.session,
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
                            &self.tree,
                            self.session,
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
                    &parameters::ownership::body(id, &self.tree)?,
                    &self.tree,
                    self.session,
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
                    &self.tree,
                    self.session,
                    parameter,
                    &mut scope.env,
                    &mut scope.literals,
                );
            }
            order.complete(index);
        }
        scope.parameter_dimensions = Arc::new(parameter_packed_dimensions(&scope.parameters));
        for parameter in scope
            .parameters
            .iter()
            .filter(|p| parameter_names.contains(p.name()))
        {
            scope.constant_types.remove(parameter.name());
            if let Some(ty) = &parameter.semantic_type {
                scope
                    .constant_types
                    .insert(parameter.name().into(), ty.clone());
                scope.constant_types.insert(
                    format!("{}.{}", scope.path, scope_component(parameter.name())),
                    ty.clone(),
                );
            }
        }
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
            scope.constant_types.remove(genvar);
            forget_variable(&mut scope.env, genvar);
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
        // Enumeration types this block declares, by typedef or inline in a
        // data declaration: their members are constants of this block
        // (IEEE 1800-2023 6.19, 27.5) even when it declares no typedef.
        let enum_types = children
            .iter()
            .filter_map(|id| {
                use celox_sv_syntax::{BodyItemKind, DataDeclaration, TypeAliasTarget};
                let native = self.tree.declarations();
                let ty = match native.body_item(*id).kind {
                    BodyItemKind::TypeAlias(alias) => match native.type_alias(alias).target {
                        TypeAliasTarget::DataType(ty) => ty,
                        _ => return None,
                    },
                    BodyItemKind::Data(data) => match native.data_declaration(data) {
                        DataDeclaration::Variables(declaration)
                        | DataDeclaration::Constructed(declaration) => declaration.data_type,
                        DataDeclaration::Unconverted(_) => return None,
                    },
                    _ => return None,
                };
                matches!(
                    native.data_type(ty).kind,
                    celox_sv_syntax::DataTypeKind::Enum { .. }
                )
                .then_some(ty)
            })
            .collect::<Vec<_>>();
        let mut deferred = Vec::new();
        // The parameters of the block, and the typedefs whose bounds name
        // one: such a bound has the block's value, which an enclosing
        // constant of the name hides from the first resolution below.
        let native = self.tree.declarations();
        let block_parameters: HashSet<String> = children
            .iter()
            .filter_map(|id| match native.body_item(*id).kind {
                celox_sv_syntax::BodyItemKind::Parameter(declaration) => Some(declaration),
                _ => None,
            })
            .flat_map(|declaration| native.parameter_declaration(declaration).parameters.iter())
            .map(|parameter| instances::normalize_identifier(&parameter.name.spelling))
            .collect();
        // A typedef that names such a parameter, directly or through
        // another such typedef, until no more are found.
        let block_aliases: Vec<(String, HashSet<String>)> = children
            .iter()
            .filter_map(|id| match native.body_item(*id).kind {
                celox_sv_syntax::BodyItemKind::TypeAlias(alias) => Some(native.type_alias(alias)),
                _ => None,
            })
            .filter_map(|alias| match alias.target {
                celox_sv_syntax::TypeAliasTarget::DataType(ty) => Some((
                    instances::normalize_identifier(&alias.name.spelling),
                    native_type_dependencies(ty, native),
                )),
                _ => None,
            })
            .collect();
        let mut shadowed_bounds: HashSet<String> = HashSet::default();
        loop {
            let before = shadowed_bounds.len();
            for (name, dependencies) in &block_aliases {
                if dependencies.iter().any(|dependency| {
                    block_parameters.contains(dependency) || shadowed_bounds.contains(dependency)
                }) {
                    shadowed_bounds.insert(name.clone());
                }
            }
            if shadowed_bounds.len() == before {
                break;
            }
        }
        if !type_names.is_empty() || !enum_types.is_empty() {
            let mut enum_names = HashSet::default();
            for ty in &enum_types {
                if let celox_sv_syntax::DataTypeKind::Enum { items, .. } =
                    &self.tree.declarations().data_type(*ty).kind
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
            let mut aliases = types::type_aliases_of_generate_scope(
                &self.tree,
                self.session,
                children,
                &scope.env,
                &self.aliases,
                &scope.path,
            )?;
            let enums = parameters::enum_member_constants_from_items(
                &self.tree,
                self.session,
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
                forget_variable(&mut scope.env, &name);
                let ty = aliases
                    .get(&name)
                    .ok_or_else(|| {
                        unsupported(format!("unresolved generate-local typedef `{name}`"))
                    })?
                    .clone();
                // Freeze definition-site dimensions before a nested block or
                // another loop iteration shadows the names in their bounds.
                let ty = procedural::scoped_type(ty, &scope.env);
                if !bounds_known(&self.tree, &ty, &scope.env) || shadowed_bounds.contains(&name) {
                    // The bounds may name a parameter of this block, which
                    // is bound below (IEEE 1800-2023 27.2): resolve the
                    // typedef again then.
                    deferred.push(name.clone());
                }
                let qualified = format!("{}.{}", scope.path, scope_component(&name));
                scope.qualified_types.insert(qualified.clone(), ty.clone());
                scope.names.insert(name.clone(), qualified.clone());
                scope.imports.insert(name.clone(), qualified.clone());
                Arc::make_mut(&mut scope.shadowed).insert(name.clone());
                aliases.insert(qualified, ty.clone());
                aliases.insert(name, ty);
            }
            for (name, binding) in &enums.declarations {
                scope.constant_types.insert(name.clone(), binding.clone());
                scope.constant_types.insert(
                    format!("{}.{}", scope.path, scope_component(name)),
                    binding.clone(),
                );
            }
            for (name, number) in enums.numbers {
                if !declared.insert(name.clone()) {
                    return Err(unsupported(format!(
                        "duplicate generate-local declaration `{name}`"
                    )));
                }
                forget_variable(&mut scope.env, &name);
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
        let declared_before = declared.clone();
        self.constants_and_signal_types(children, &mut scope, &mut declared)?;
        if !deferred.is_empty() {
            let aliases = types::type_aliases_of_generate_scope(
                &self.tree,
                self.session,
                children,
                &scope.env,
                &self.aliases,
                &scope.path,
            )?;
            let mut resolved = self.aliases.clone().into_owned();
            for name in deferred {
                let ty = procedural::scoped_type(
                    aliases
                        .get(&name)
                        .ok_or_else(|| {
                            unsupported(format!("unresolved generate-local typedef `{name}`"))
                        })?
                        .clone(),
                    &scope.env,
                );
                if !bounds_known(&self.tree, &ty, &scope.env) {
                    return Err(unsupported(format!(
                        "unresolved generate-local typedef bounds `{name}`"
                    )));
                }
                let qualified = format!("{}.{}", scope.path, scope_component(&name));
                scope.qualified_types.insert(qualified.clone(), ty.clone());
                resolved.insert(qualified, ty.clone());
                resolved.insert(name, ty);
            }
            self.aliases = Cow::Owned(resolved);
            // The parameters and variables of the block that the deferred
            // typedefs type were bound with their first resolution: bind
            // them again with the block's.
            let mut again = declared_before;
            self.constants_and_signal_types(children, &mut scope, &mut again)?;
            declared = again;
        }
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
                &self.tree,
                self.session,
                &self.aliases,
                None,
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

/// Forget the markers of an outer variable `name` that a declaration of the
/// block hides: a query of the name reads the nearer declaration (IEEE
/// 1800-2023 23.9).
fn forget_variable(env: &mut SharedEnv, name: &str) {
    for key in [
        variable_bits_marker(name),
        variable_size_marker(name),
        variable_signed_marker(name),
        variable_unpacked_dimensions_marker(name),
        variable_dimensions_marker(name),
    ] {
        env.remove(&key);
    }
}

/// Make `name` in `scope` denote the package item `target`, as an import of
/// the generate block does.
fn bind_import(scope: &mut Scope, name: &str, target: &str, session: &mut Session) {
    // Every imported declaration hides an enclosing constant type, including
    // enumerators that are exposed through the qualified module bindings.
    scope.constant_types.remove(name);
    let imported = scope.imported.clone();
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
        if let Some(value) = literal.and_then(|expr| expr_to_const(session, expr)) {
            parameter.value = Some(value);
        }
        if let Some(ty) = &parameter.semantic_type {
            scope.constant_types.insert(name.into(), ty.clone());
        }
        scope.parameters.push(parameter);
    }
}

// Bootstrap only module-scope functions; collecting the full function map would
// itself elaborate generate blocks and recurse into the conditions being decided.
fn module_constant_functions(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
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
            imported: scope::imported(tree),
            id,
            env: env.clone().into(),
            literals: literals.clone().into(),
            parameter_dimensions: Arc::default(),
            qualified_types: SharedMap::default(),
            constant_types: SharedMap::default(),
            names: SharedMap::default(),
            imports: SharedMap::default(),
            shadowed: Arc::default(),
            scope: String::new(),
        });
    }
    let mut functions = functions::native_functions(
        &roots,
        tree,
        session,
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
mod tests;

/// Whether the packed bounds of `ty` have values in `env`.
fn bounds_known(tree: &AnalysisContext<'_>, ty: &Type, env: &SharedEnv) -> bool {
    ty.packed_ranges().iter().all(|range| {
        eval_ast_const_expr(&tree.functions, range.left(), env).is_some()
            && eval_ast_const_expr(&tree.functions, range.right(), env).is_some()
    })
}
