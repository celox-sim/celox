//! Shared, bounded elaboration of generate scopes before collecting RTL objects.
//!
//! IEEE 1800-2023 sections 12.5 and 27.4–27.6 define the comparison and scope
//! rules used here. Generate-local typedefs and functions in loop-generate
//! remain unsupported; inactive branches are not lowered.
use super::*;

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

#[derive(Clone)]
pub(super) struct Item<'a> {
    pub node: ScopeItem<'a>,
    pub env: SharedMap<i128>,
    pub literals: SharedMap<Expr>,
    parameter_dimensions: Arc<VariablePackedDimensions>,
    names: SharedMap<String>,
    shadowed: Arc<HashSet<String>>,
    scope: String,
}

impl Item<'_> {
    pub fn common(&self) -> Option<&sv_parser::ModuleCommonItem> {
        match self.node {
            ScopeItem::Module(sv_parser::ModuleOrGenerateItem::ModuleItem(item)) => {
                Some(&item.nodes.1)
            }
            _ => None,
        }
    }

    /// These declarations have already populated the constant environment;
    /// instance/process/subroutine collectors cannot produce bodies from them.
    pub fn is_parameter_declaration(&self) -> bool {
        let Some(declaration) = self.node.declaration() else {
            return false;
        };
        matches!(
            declaration,
            sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(_)
                | sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(_)
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
        // Ordinary module items keep the module's function bindings. Rewriting
        // every function body for every item costs O(items * function size).
        if !self.shadowed.is_empty() || !self.names.is_empty() {
            if !dimensions.functions.is_empty() {
                local.functions = Arc::new(self.functions(&dimensions.functions));
            }
            if !dimensions.function_return_types.is_empty() {
                local.function_return_types =
                    self.function_aliases(dimensions.function_return_types.clone());
            }
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
            Expr::Call { name, args } => {
                *name = self.name(name);
                args.iter_mut().for_each(|e| self.expr(e));
            }
        }
    }

    fn constant(&self, expr: &mut ConstExpr) {
        match expr {
            ConstExpr::Ident(name) => *name = self.name(name),
            ConstExpr::Literal(_) => {}
            ConstExpr::Select { expr, bit } => {
                self.constant(expr);
                self.constant(bit);
            }
            ConstExpr::Function { name, args, .. } => {
                *name = self.name(name);
                args.iter_mut().for_each(|e| self.constant(e));
            }
            ConstExpr::Unary { expr, .. } => self.constant(expr),
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
    path: String,
    in_loop: bool,
    env: SharedMap<i128>,
    literals: SharedMap<Expr>,
    parameters: Vec<Parameter>,
    parameter_dimensions: Arc<VariablePackedDimensions>,
    names: SharedMap<String>,
    shadowed: Arc<HashSet<String>>,
}

struct Elaborator<'a, 'b> {
    tree: &'a SyntaxTree,
    aliases: &'b HashMap<String, Type>,
    remaining: usize,
    functions: HashMap<String, Function>,
    items: Vec<Item<'a>>,
}

pub(super) fn items<'a>(
    node: RefNode<'a>,
    tree: &'a SyntaxTree,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Result<Vec<Item<'a>>, AnalyzerError> {
    let mut elaborator = Elaborator {
        tree,
        aliases,
        remaining: MAX_GENERATE_LOOP_EXPANSION,
        functions: HashMap::default(),
        items: Vec::new(),
    };
    let parameters =
        parameters_from_module_node(node.clone(), tree, aliases, env, &HashMap::default())?;
    let mut literals = parameter_value_env(&parameters, env);
    if node.clone().into_iter().any(|node| matches!(node, RefNode::ConstantFunctionCall(call) if matches!(&call.nodes.0.nodes.0, sv_parser::SubroutineCall::TfCall(_)))) {
        elaborator.functions = module_constant_functions(node.clone(), tree, env, aliases, &literals);
    }
    // Numeric values and their type markers are already carried by `env`.
    literals.retain(|name, _| !env.contains_key(name));
    let scope = Scope {
        env: env.clone().into(),
        literals: literals.into(),
        parameter_dimensions: Arc::new(parameter_packed_dimensions(&parameters)),
        parameters,
        ..Scope::default()
    };
    let mut ordinal = 0;
    if let RefNode::PackageDeclaration(package) = node {
        for declaration in package_declarations(package) {
            elaborator.push(ScopeItem::Package(declaration), &scope);
        }
        return Ok(elaborator.items);
    }
    for item in module_non_port_items(node) {
        match item {
            sv_parser::NonPortModuleItem::GenerateRegion(region) => {
                for item in &region.nodes.1 {
                    elaborator.generate_item(item, &scope, &mut ordinal)?;
                }
            }
            sv_parser::NonPortModuleItem::ModuleOrGenerateItem(item) => {
                elaborator.item(item, &scope, &mut ordinal)?
            }
            _ => {}
        }
    }
    Ok(elaborator.items)
}

impl<'a> Elaborator<'a, '_> {
    /// A constant expression in this scope, or `None` when it has no
    /// constant value Celox can determine.
    fn expression(
        &self,
        expr: &sv_parser::ConstantExpression,
        scope: &Scope,
    ) -> Result<Option<ConstExpr>, AnalyzerError> {
        let Some(expr) = const_expr_from_ref_node_with_env(
            RefNode::ConstantExpression(expr),
            self.tree,
            &scope.env,
            self.aliases,
        )?
        else {
            return Ok(None);
        };
        let Some(mut expr) = expr_to_const(substitute_expr_idents(
            const_expr_to_expr(expr),
            &scope.literals,
        )) else {
            return Ok(None);
        };
        Ok(self.expand_constant_calls(&mut expr, scope).map(|()| expr))
    }

    fn expand_constant_calls(&self, expr: &mut ConstExpr, scope: &Scope) -> Option<()> {
        match expr {
            ConstExpr::Function { name, args, .. } => {
                for arg in args {
                    self.expand_constant_calls(arg, scope)?;
                }
                if self.functions.contains_key(name) {
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
                    let expanded = simplify_constant_mux_conditions(expanded, &scope.env);
                    // Fold only the call: surrounding case arithmetic still needs
                    // the common comparison width, while function results are self-determined.
                    *expr = expr_to_const(fold_const_integral_expr_preserving_mask(
                        expanded, &scope.env,
                    ))?;
                }
            }
            ConstExpr::Unary { expr, .. } => self.expand_constant_calls(expr, scope)?,
            ConstExpr::Select { expr, bit } => {
                self.expand_constant_calls(expr, scope)?;
                self.expand_constant_calls(bit, scope)?;
            }
            ConstExpr::Binary { left, right, .. } => {
                self.expand_constant_calls(left, scope)?;
                self.expand_constant_calls(right, scope)?;
            }
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                self.expand_constant_calls(condition, scope)?;
                self.expand_constant_calls(then_expr, scope)?;
                self.expand_constant_calls(else_expr, scope)?;
            }
            ConstExpr::Ident(_) | ConstExpr::Literal(_) => {}
        }
        Some(())
    }

    fn value(
        &self,
        expr: &sv_parser::ConstantExpression,
        scope: &Scope,
        detail: &str,
    ) -> Result<i128, AnalyzerError> {
        self.expression(expr, scope)?
            .and_then(|expr| eval_ast_const_expr(&expr, &scope.env))
            .ok_or_else(|| AnalyzerError::Unsupported(detail.to_string()))
    }

    fn condition(
        &self,
        expr: &sv_parser::ConstantExpression,
        scope: &Scope,
        detail: &str,
    ) -> Result<bool, AnalyzerError> {
        let types = parameter_types_from_const_env(&scope.env)
            .into_iter()
            .map(|(name, ty)| (name, (ty.width, ty.signed)))
            .collect();
        self.expression(expr, scope)?
            .and_then(|expr| {
                typecheck::eval_const_integral_literal_with_types(&expr.into(), &scope.env, &types)
            })
            // Unknown truth takes the false branch; a known one bit still makes
            // a vector true even when other bits are X/Z (IEEE 1800-2023 12.4).
            .map(|literal| typecheck::integral_literal_truth(&literal) == Some(true))
            .ok_or_else(|| AnalyzerError::Unsupported(detail.to_string()))
    }

    fn generate_item(
        &mut self,
        item: &'a sv_parser::GenerateItem,
        scope: &Scope,
        ordinal: &mut usize,
    ) -> Result<(), AnalyzerError> {
        match item {
            sv_parser::GenerateItem::ModuleOrGenerateItem(item) => self.item(item, scope, ordinal),
            _ => Err(AnalyzerError::Unsupported(
                "non-module generate item".to_string(),
            )),
        }
    }

    fn item(
        &mut self,
        item: &'a sv_parser::ModuleOrGenerateItem,
        scope: &Scope,
        ordinal: &mut usize,
    ) -> Result<(), AnalyzerError> {
        if let sv_parser::ModuleOrGenerateItem::ModuleItem(common) = item {
            match &common.nodes.1 {
                sv_parser::ModuleCommonItem::ConditionalGenerateConstruct(generate) => {
                    *ordinal += 1;
                    match &**generate {
                        sv_parser::ConditionalGenerateConstruct::If(generate) => {
                            let value = self.condition(
                                &generate.nodes.1.nodes.1,
                                scope,
                                "unknown conditional-generate condition",
                            )?;
                            let selected = if value {
                                Some(&generate.nodes.2)
                            } else {
                                generate.nodes.3.as_ref().map(|(_, b)| b)
                            };
                            if let Some(block) = selected {
                                self.block(block, scope, *ordinal, None)?;
                            }
                        }
                        sv_parser::ConditionalGenerateConstruct::Case(generate) => {
                            let selector = self
                                .expression(&generate.nodes.1.nodes.1, scope)?
                                .ok_or_else(|| {
                                    AnalyzerError::Unsupported(
                                        "unknown case-generate selector".to_string(),
                                    )
                                })?;
                            let types = parameter_types_from_const_env(&scope.env)
                                .into_iter()
                                .map(|(name, ty)| (name, (ty.width, ty.signed)))
                                .collect::<HashMap<_, _>>();
                            let integral = |expr: &ConstExpr| {
                                typecheck::eval_const_integral_literal_with_types(
                                    &expr.clone().into(),
                                    &scope.env,
                                    &types,
                                )
                                .ok_or_else(|| {
                                    AnalyzerError::Unsupported(
                                        "nonconstant case-generate expression".to_string(),
                                    )
                                })
                            };
                            let selector_value = integral(&selector).map_err(|_| {
                                AnalyzerError::Unsupported(
                                    "unknown case-generate selector".to_string(),
                                )
                            })?;
                            let self_width = |expr: &ConstExpr, width| {
                                if matches!(expr, ConstExpr::Literal(literal) if resize_unbased_fill_literal_for_cast(literal, 1, false).is_some())
                                {
                                    1
                                } else {
                                    width
                                }
                            };
                            let mut width = self_width(&selector, selector_value.width);
                            let mut signed = selector_value.signed;
                            let mut labels = Vec::new();
                            let mut default = None;
                            for item in &generate.nodes.2 {
                                match item {
                                    sv_parser::CaseGenerateItem::Nondefault(item) => {
                                        for label in item.nodes.0.contents() {
                                            let label = self.expression(label, scope)?.ok_or_else(
                                                || {
                                                    AnalyzerError::Unsupported(
                                                        "unknown case-generate label".to_string(),
                                                    )
                                                },
                                            )?;
                                            let value = integral(&label)?;
                                            width = width.max(self_width(&label, value.width));
                                            signed &= value.signed;
                                            labels.push((label, &item.nodes.2));
                                        }
                                    }
                                    sv_parser::CaseGenerateItem::Default(item) => {
                                        if default.replace(&item.nodes.2).is_some() {
                                            return Err(AnalyzerError::Unsupported(
                                                "duplicate case-generate default".to_string(),
                                            ));
                                        }
                                    }
                                }
                            }
                            // The selector and every label share one width and signing context.
                            // Pairwise comparisons would incorrectly sign-extend a narrow signed
                            // selector when another label makes the overall case unsigned.
                            let normalize = |expr: ConstExpr| {
                                typecheck::eval_generate_case_operand(
                                    &expr.into(),
                                    &scope.env,
                                    &types,
                                    width,
                                    signed,
                                )
                                .ok_or_else(|| {
                                    AnalyzerError::Unsupported(
                                        "case-generate comparison".to_string(),
                                    )
                                })
                            };
                            let selector = normalize(selector)?;
                            let mut selected = None;
                            for (label, block) in labels {
                                let label = normalize(label)?;
                                if label.value == selector.value
                                    && label.mask == selector.mask
                                    && selected.is_none()
                                {
                                    selected = Some(block);
                                }
                            }
                            if let Some(block) = selected.or(default) {
                                self.block(block, scope, *ordinal, None)?;
                            }
                        }
                    }
                    return Ok(());
                }
                sv_parser::ModuleCommonItem::LoopGenerateConstruct(generate) => {
                    *ordinal += 1;
                    let name = identifier_text(
                        RefNode::GenvarIdentifier(&generate.nodes.1.nodes.1.0.nodes.1),
                        self.tree,
                    )
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported("loop-generate variable".to_string())
                    })?;
                    let step_name = RefNode::GenvarIteration(&generate.nodes.1.nodes.1.4)
                        .into_iter()
                        .find_map(|node| match node {
                            RefNode::GenvarIdentifier(name) => {
                                identifier_text(RefNode::GenvarIdentifier(name), self.tree)
                            }
                            _ => None,
                        });
                    if step_name.as_deref() != Some(name.as_str()) {
                        return Err(AnalyzerError::Unsupported(
                            "genvar update targets a different variable".to_string(),
                        ));
                    }
                    let mut value = self.value(
                        &generate.nodes.1.nodes.1.0.nodes.3,
                        scope,
                        "loop-generate initializer",
                    )?;
                    loop {
                        // Assignment to a genvar truncates and sign-extends to its
                        // signed 32-bit type, including after every loop update.
                        value = i128::from(value as i32);
                        let mut iteration = scope.clone();
                        iteration.env.insert(name.clone(), value);
                        insert_parameter_type_markers(
                            &mut iteration.env,
                            &name,
                            ExprType {
                                width: 32,
                                signed: true,
                            },
                        );
                        iteration.names.remove(&name);
                        Arc::make_mut(&mut iteration.shadowed).insert(name.clone());
                        iteration
                            .parameters
                            .retain(|parameter| parameter.name() != name);
                        iteration.parameter_dimensions =
                            Arc::new(parameter_packed_dimensions(&iteration.parameters));
                        iteration.literals.remove(&name);
                        if !self.condition(
                            &generate.nodes.1.nodes.1.2.nodes.0,
                            &iteration,
                            "loop-generate condition",
                        )? {
                            break;
                        }
                        self.remaining = self.remaining.checked_sub(1).ok_or_else(|| {
                            AnalyzerError::Unsupported(
                                "loop-generate unroll limit exceeded".to_string(),
                            )
                        })?;
                        self.block(
                            &generate.nodes.2,
                            &iteration,
                            *ordinal,
                            Some((&name, value)),
                        )?;
                        value = next_genvar_value(
                            value,
                            &generate.nodes.1.nodes.1.4,
                            self.tree,
                            &iteration.env,
                            |expr| self.expression(expr, &iteration),
                        )?
                        .ok_or_else(|| {
                            AnalyzerError::Unsupported("genvar update operator".to_string())
                        })?;
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
        if scope.in_loop
            && RefNode::ModuleOrGenerateItem(item)
                .into_iter()
                .any(|node| matches!(node, RefNode::FunctionDeclaration(_)))
        {
            return Err(AnalyzerError::Unsupported(
                "function declaration inside loop-generate".to_string(),
            ));
        }
        self.push(ScopeItem::Module(item), scope);
        Ok(())
    }

    fn push(&mut self, node: ScopeItem<'a>, scope: &Scope) {
        self.items.push(Item {
            node,
            env: scope.env.clone(),
            literals: scope.literals.clone(),
            parameter_dimensions: scope.parameter_dimensions.clone(),
            names: scope.names.clone(),
            shadowed: scope.shadowed.clone(),
            scope: scope.path.clone(),
        });
    }

    fn constants_and_signal_types(
        &self,
        children: &[&sv_parser::GenerateItem],
        scope: &mut Scope,
        declared: &mut HashSet<String>,
    ) -> Result<(), AnalyzerError> {
        let mut signal_declarations = Vec::new();
        for item in children {
            let sv_parser::GenerateItem::ModuleOrGenerateItem(item) = item else {
                continue;
            };
            let is_signal = match &**item {
                sv_parser::ModuleOrGenerateItem::Module(module) => identifier_text(
                    RefNode::ModuleIdentifier(&module.nodes.1.nodes.0), self.tree,
                ).is_some_and(|name| self.aliases.contains_key(&name)),
                sv_parser::ModuleOrGenerateItem::ModuleItem(common) => {
                    if let sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(declaration) = &common.nodes.1
                        && let sv_parser::ModuleOrGenerateItemDeclaration::PackageOrGenerateItemDeclaration(declaration) = &**declaration
                    {
                        matches!(&**declaration,
                            sv_parser::PackageOrGenerateItemDeclaration::DataDeclaration(_)
                            | sv_parser::PackageOrGenerateItemDeclaration::NetDeclaration(_))
                    } else { false }
                }
                _ => false,
            };
            if !is_signal {
                continue;
            }
            let node = RefNode::ModuleOrGenerateItem(item);
            let shared_dependencies: HashSet<_> = node
                .clone()
                .into_iter()
                .filter_map(|node| match node {
                    RefNode::DataDeclaration(sv_parser::DataDeclaration::Variable(variable)) => {
                        Some(RefNode::DataTypeOrImplicit(&variable.nodes.3))
                    }
                    RefNode::NetDeclaration(sv_parser::NetDeclaration::NetType(net)) => {
                        Some(RefNode::DataTypeOrImplicit(&net.nodes.3))
                    }
                    _ => None,
                })
                .flat_map(|node| dimension_dependencies(node, self.tree))
                .collect();
            let declarators = node.into_iter().filter_map(|node| {
                let identifier = match node {
                    RefNode::VariableDeclAssignment(
                        sv_parser::VariableDeclAssignment::Variable(assignment),
                    ) => RefNode::VariableIdentifier(&assignment.nodes.0),
                    RefNode::NetDeclAssignment(assignment) => {
                        RefNode::NetIdentifier(&assignment.nodes.0)
                    }
                    RefNode::HierarchicalInstance(instance) => {
                        RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0)
                    }
                    _ => return None,
                };
                identifier_text(identifier, self.tree).map(|name| (name, node))
            });
            for (name, declarator) in declarators {
                if !declared.insert(name.clone()) {
                    return Err(AnalyzerError::Unsupported(format!(
                        "duplicate generate-local declaration `{name}`"
                    )));
                }
                // An inner declaration hides inherited values and type metadata
                // before any local size query or dimension is evaluated.
                for key in [
                    name.clone(),
                    parameter_marker(&name),
                    local_parameter_marker(&name),
                    enum_marker(&name),
                    parameter_width_marker(&name),
                    parameter_signed_marker(&name),
                    variable_bits_marker(&name),
                    variable_size_marker(&name),
                    variable_signed_marker(&name),
                ] {
                    scope.env.remove(&key);
                }
                scope.literals.remove(&name);
                // The packed type is shared, but each declarator owns its
                // unpacked bounds. A sibling's bounds must not block this type.
                let mut dependencies = shared_dependencies.clone();
                dependencies.extend(dimension_dependencies(declarator, self.tree));
                signal_declarations.push((name, &**item, dependencies));
            }
        }
        let mut pending = Vec::new();
        for item in children {
            let sv_parser::GenerateItem::ModuleOrGenerateItem(item) = item else {
                continue;
            };
            let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = &**item else {
                continue;
            };
            let sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(declaration) =
                &item.nodes.1
            else {
                continue;
            };
            let sv_parser::ModuleOrGenerateItemDeclaration::PackageOrGenerateItemDeclaration(
                declaration,
            ) = &**declaration
            else {
                continue;
            };
            // IEEE 1800-2023 6.20.1: parameter declarations in a
            // generate block are localparams as well.
            let (node, data_type) = match &**declaration {
                sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(
                    declaration,
                ) => {
                    let sv_parser::LocalParameterDeclaration::Param(parameter) = &declaration.0
                    else {
                        return Err(AnalyzerError::Unsupported(
                            "generate-local type parameter".to_string(),
                        ));
                    };
                    (
                        RefNode::LocalParameterDeclaration(&declaration.0),
                        &parameter.nodes.1,
                    )
                }
                sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(declaration) => {
                    let sv_parser::ParameterDeclaration::Param(parameter) = &declaration.0 else {
                        return Err(AnalyzerError::Unsupported(
                            "generate-local type parameter".to_string(),
                        ));
                    };
                    (
                        RefNode::ParameterDeclaration(&declaration.0),
                        &parameter.nodes.1,
                    )
                }
                _ => continue,
            };
            let assignments: Vec<_> = node
                .clone()
                .into_iter()
                .filter_map(|child| match child {
                    RefNode::ParamAssignment(assignment) => Some(assignment),
                    _ => None,
                })
                .collect();
            let single = assignments.len() == 1;
            for assignment in assignments {
                let name = parameter_name(RefNode::ParamAssignment(assignment), self.tree)?;
                if !declared.insert(name.clone()) {
                    return Err(AnalyzerError::Unsupported(format!(
                        "duplicate generate-local declaration `{name}`"
                    )));
                }
                // Include both initializer and declared range dependencies. Bind all
                // names before evaluation so a forward local hides an outer parameter.
                let dependencies: HashSet<_> =
                    RefNode::DataTypeOrImplicit(data_type)
                        .into_iter()
                        .chain(assignment.nodes.2.iter().flat_map(|(_, value)| {
                            RefNode::ConstantParamExpression(value).into_iter()
                        }))
                        .filter(|node| {
                            matches!(
                                node,
                                RefNode::SimpleIdentifier(_) | RefNode::EscapedIdentifier(_)
                            )
                        })
                        .filter_map(|node| identifier_text(node, self.tree))
                        .collect();
                for key in [
                    name.clone(),
                    parameter_marker(&name),
                    local_parameter_marker(&name),
                    enum_marker(&name),
                    parameter_width_marker(&name),
                    parameter_signed_marker(&name),
                    variable_bits_marker(&name),
                    variable_size_marker(&name),
                    variable_signed_marker(&name),
                ] {
                    scope.env.remove(&key);
                }
                scope.literals.remove(&name);
                scope
                    .parameters
                    .retain(|parameter| parameter.name() != name);
                scope.names.remove(&name);
                Arc::make_mut(&mut scope.shadowed).insert(name.clone());
                pending.push((name, node.clone(), dependencies, single));
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
                let (name, item, _) = signal_declarations[index]
                    .take()
                    .expect("ready signal is unbound");
                let mut signals = Vec::new();
                signals_from_module_or_generate_item(
                    item,
                    self.tree,
                    self.aliases,
                    &scope.env,
                    Some(&name),
                    &mut signals,
                )?;
                extend_const_env_with_variable_types(
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
                    let mut environment =
                        parameters::ParameterEnvironment::new(&scope.parameters, &base);
                    let mut types = parameter_types_from_const_env(&base);
                    let mut next = index;
                    loop {
                        let (name, node, _, _) = pending[next - signal_count]
                            .take()
                            .expect("ready parameter is unbound");
                        let inherited_count = scope.parameters.len();
                        parameters::parameters_from_ref_node_with_environment(
                            node,
                            self.tree,
                            &mut scope.parameters,
                            true,
                            &base,
                            self.aliases,
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
                let (name, node, _, _) = pending[index - signal_count]
                    .take()
                    .expect("ready parameter is unbound");
                // The declaration may contain siblings. Temporarily append all
                // of them for the existing lowering semantics, then keep only
                // this scheduled parameter, without cloning the inherited prefix.
                let inherited_count = scope.parameters.len();
                parameters_from_ref_node(
                    node,
                    self.tree,
                    &mut scope.parameters,
                    true,
                    &scope.env,
                    self.aliases,
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
                bind_generate_parameter(parameter, &mut scope.env, &mut scope.literals);
            }
            order.complete(index);
        }
        scope.parameter_dimensions = Arc::new(parameter_packed_dimensions(&scope.parameters));
        Ok(())
    }

    fn block(
        &mut self,
        block: &'a sv_parser::GenerateBlock,
        parent: &Scope,
        ordinal: usize,
        index: Option<(&str, i128)>,
    ) -> Result<(), AnalyzerError> {
        // A directly nested conditional generate does not introduce a scope
        // (IEEE 1800-2023 27.5). A loop body still always introduces a scope.
        if index.is_none()
            && let sv_parser::GenerateBlock::GenerateItem(item) = block
            && let sv_parser::GenerateItem::ModuleOrGenerateItem(common) = &**item
            && let sv_parser::ModuleOrGenerateItem::ModuleItem(common) = &**common
            && matches!(
                common.nodes.1,
                sv_parser::ModuleCommonItem::ConditionalGenerateConstruct(_)
            )
        {
            return self.generate_item(item, parent, &mut (ordinal - 1));
        }
        let explicit = match block {
            sv_parser::GenerateBlock::Multiple(block) => block
                .nodes
                .2
                .as_ref()
                .map(|(_, name)| name)
                .or_else(|| block.nodes.0.as_ref().map(|(name, _)| name)),
            _ => None,
        };
        let mut name = explicit
            .and_then(|name| identifier_text(RefNode::GenerateBlockIdentifier(name), self.tree))
            .unwrap_or_else(|| format!("genblk{ordinal}"));
        name = scope_component(&name);
        if let Some((_, index)) = index {
            name.push_str(&format!("[{index}]"));
        }
        let mut scope = parent.clone();
        scope.in_loop |= index.is_some();
        scope.path = if parent.path.is_empty() {
            name
        } else {
            format!("{}.{name}", parent.path)
        };
        let children: Vec<&sv_parser::GenerateItem> = match block {
            sv_parser::GenerateBlock::GenerateItem(item) => vec![item],
            sv_parser::GenerateBlock::Multiple(block) => block.nodes.3.iter().collect(),
        };
        // Bind all declarations before lowering expressions, including forward references.
        let mut declared = HashSet::default();
        if let Some((genvar, _)) = index {
            declared.insert(genvar.to_string());
        }
        self.constants_and_signal_types(&children, &mut scope, &mut declared)?;
        for item in &children {
            let mut signals = Vec::new();
            if let sv_parser::GenerateItem::ModuleOrGenerateItem(item) = item {
                // Do not recurse into nested scopes while gathering direct declarations.
                match &**item {
                    sv_parser::ModuleOrGenerateItem::Module(_) => {
                        signals_from_module_or_generate_item(
                            item,
                            self.tree,
                            self.aliases,
                            &scope.env,
                            None,
                            &mut signals,
                        )?
                    }
                    sv_parser::ModuleOrGenerateItem::ModuleItem(common)
                        if matches!(
                            common.nodes.1,
                            sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(_)
                        ) =>
                    {
                        if RefNode::ModuleOrGenerateItem(item)
                            .into_iter()
                            .any(|n| matches!(n, RefNode::TypeDeclaration(_)))
                        {
                            return Err(AnalyzerError::Unsupported(
                                "type declaration inside generate scope".to_string(),
                            ));
                        }
                        for node in RefNode::ModuleOrGenerateItem(item) {
                            // A subroutine of the scope is known by its
                            // qualified name, like a variable of the scope.
                            let identifier = match node {
                                RefNode::FunctionDeclaration(function) => match &function.nodes.2 {
                                    sv_parser::FunctionBodyDeclaration::WithPort(body) => {
                                        RefNode::FunctionIdentifier(&body.nodes.2)
                                    }
                                    sv_parser::FunctionBodyDeclaration::WithoutPort(body) => {
                                        RefNode::FunctionIdentifier(&body.nodes.2)
                                    }
                                },
                                RefNode::TaskDeclaration(task) => match &task.nodes.2 {
                                    sv_parser::TaskBodyDeclaration::WithPort(body) => {
                                        RefNode::TaskIdentifier(&body.nodes.1)
                                    }
                                    sv_parser::TaskBodyDeclaration::WithoutPort(body) => {
                                        RefNode::TaskIdentifier(&body.nodes.1)
                                    }
                                },
                                _ => continue,
                            };
                            {
                                let name =
                                    identifier_text(identifier, self.tree).ok_or_else(|| {
                                        AnalyzerError::Unsupported("function name".to_string())
                                    })?;
                                if !declared.insert(name.clone()) {
                                    return Err(AnalyzerError::Unsupported(format!(
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
                        signals_from_module_or_generate_item(
                            item,
                            self.tree,
                            self.aliases,
                            &scope.env,
                            None,
                            &mut signals,
                        )?;
                    }
                    _ => {}
                }
            }
            for signal in signals {
                for key in [
                    signal.name.clone(),
                    parameter_marker(&signal.name),
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
        let mut ordinal = 0;
        for item in children {
            self.generate_item(item, &scope, &mut ordinal)?;
        }
        Ok(())
    }
}

// Bootstrap only module-scope functions; collecting the full function map would
// itself elaborate generate blocks and recurse into the conditions being decided.
fn module_constant_functions(
    node: RefNode<'_>,
    tree: &SyntaxTree,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
    literals: &HashMap<String, Expr>,
) -> HashMap<String, Function> {
    let dimensions = PackedDimensions::new(HashMap::default(), env, aliases);
    let types = parameter_types_from_const_env(env);
    let mut functions = HashMap::default();
    let mut calls = HashMap::default();
    for item in scope_items(node) {
        let Some(declaration) = item.declaration() else {
            continue;
        };
        for node in RefNode::PackageOrGenerateItemDeclaration(declaration) {
            let RefNode::FunctionDeclaration(declaration) = node else {
                continue;
            };
            let Some(mut function) =
                function_from_declaration(declaration, tree, env, aliases, &dimensions)
            else {
                continue;
            };
            let mut bindings = HashMap::default();
            for node in RefNode::FunctionDeclaration(declaration) {
                if !matches!(
                    node,
                    RefNode::SimpleIdentifier(_) | RefNode::EscapedIdentifier(_)
                ) {
                    continue;
                }
                let Some(name) = identifier_text(node, tree) else {
                    continue;
                };
                if function
                    .params
                    .iter()
                    .any(|parameter| parameter.name == name)
                {
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
            // Close over definition-site constants. Unresolved module signals must
            // not become constants just because a generate local shadows them.
            function.body = function
                .body
                .map(|body| substitute_expr_idents(body, &bindings));
            calls.insert(
                function.name.clone(),
                RefNode::FunctionDeclaration(declaration)
                    .into_iter()
                    .filter_map(|node| {
                        let RefNode::TfCall(call) = node else {
                            return None;
                        };
                        reference_name(RefNode::PsOrHierarchicalTfIdentifier(&call.nodes.0), tree)
                    })
                    .collect::<Vec<_>>(),
            );
            functions.insert(function.name.clone(), function);
        }
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
    functions
}

// Inspect dimension expressions only, excluding runtime declaration initializers.
fn dimension_dependencies(node: RefNode<'_>, tree: &SyntaxTree) -> HashSet<String> {
    node.into_iter()
        .filter(|node| {
            matches!(
                node,
                RefNode::PackedDimension(_)
                    | RefNode::UnpackedDimension(_)
                    | RefNode::VariableDimension(_)
            )
        })
        .flat_map(|node| node.into_iter())
        .filter(|node| {
            matches!(
                node,
                RefNode::SimpleIdentifier(_) | RefNode::EscapedIdentifier(_)
            )
        })
        .filter_map(|node| identifier_text(node, tree))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn analyze(source: &str) -> Result<Source, AnalyzerError> {
        let tree = crate::syntax::parse_source(source, Path::new("generate.sv"))?;
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
            let tree =
                crate::syntax::parse_source(&code, Path::new("generate_parameter_prefix.sv"))
                    .unwrap();
            let node = tree
                .into_iter()
                .find(|node| matches!(node, RefNode::ModuleDeclarationAnsi(_)))
                .unwrap();
            parameters::PARAMETER_BINDINGS.with(|bindings| bindings.set(0));
            let active = items(node, &tree, &HashMap::default(), &HashMap::default()).unwrap();
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
            bind_generate_parameter(parameter.clone(), &mut rebuilt_env, &mut rebuilt_literals);
            constants::bind_generate_parameter_with_types(
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
        let tree = crate::syntax::parse_source(
            "module Top(); if (1) begin : g logic x; logic y; end endmodule",
            Path::new("generate_name_snapshots.sv"),
        )
        .unwrap();
        let node = tree
            .into_iter()
            .find(|node| matches!(node, RefNode::ModuleDeclarationAnsi(_)))
            .unwrap();
        let active = items(node, &tree, &HashMap::default(), &HashMap::default()).unwrap();
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
                crate::syntax::parse_source(&code, Path::new("scheduled_siblings.sv")).unwrap();
            let node = tree
                .into_iter()
                .find(|node| matches!(node, RefNode::ModuleDeclarationAnsi(_)))
                .unwrap();
            let active = items(
                node,
                &tree,
                &HashMap::from_iter([("B".into(), 99)]),
                &HashMap::default(),
            )
            .unwrap();
            let assignment = active
                .iter()
                .find(|item| {
                    item.node
                        .node()
                        .into_iter()
                        .any(|node| matches!(node, RefNode::ContinuousAssign(_)))
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
                    localparam real R=1.0;
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
        let tree = crate::syntax::parse_source(
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
        let module = tree
            .into_iter()
            .find(|node| matches!(node, RefNode::ModuleDeclarationAnsi(_)))
            .unwrap();
        let active = items(module, &tree, &HashMap::default(), &HashMap::default()).unwrap();
        let outer = FunctionReturnMetadata {
            width: Some(8),
            first_packed_dimension_width: Some(2),
            signed: false,
            is_2state: false,
        };
        let inner = FunctionReturnMetadata {
            width: Some(4),
            first_packed_dimension_width: Some(4),
            signed: true,
            is_2state: true,
        };
        let dimensions = PackedDimensions {
            function_return_types: [("f".to_string(), outer), ("g.f".to_string(), inner)]
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
                "if (1) begin typedef logic T; T tmp; end",
                "type declaration inside generate scope",
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
}
