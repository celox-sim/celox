//! Shared, bounded elaboration of generate scopes before collecting RTL objects.
//!
//! IEEE 1800-2023 sections 12.5 and 27.4–27.6 define the comparison and scope
//! rules used here. Generate-local typedefs and functions in loop-generate
//! remain unsupported; inactive branches are not lowered.
use super::*;

// Temporary identifiers protect definition-site bindings while functions are
// inlined into a generate scope. They are removed when that item's names are
// qualified, and never reach the public analyzer IR.
const OUTER_BINDING: &str = "\0generate_outer:";

#[derive(Clone)]
pub(super) struct Item<'a> {
    pub node: &'a sv_parser::ModuleOrGenerateItem,
    pub env: HashMap<String, i128>,
    pub literals: HashMap<String, Expr>,
    names: HashMap<String, String>,
    shadowed: HashSet<String>,
    scope: String,
}

impl Item<'_> {
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
            format!("{}.{name}", self.scope)
        }
    }

    pub fn parameter_literals(&self, inherited: &HashMap<String, Expr>) -> HashMap<String, Expr> {
        let mut literals = inherited.clone();
        for name in &self.shadowed {
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
        local.parameter_values = self.parameter_literals(&dimensions.parameter_values);
        local.functions = Arc::new(self.functions(&dimensions.functions));
        let mut signedness = (*dimensions.expression_signedness).clone();
        for name in &self.shadowed {
            let protected = format!("{OUTER_BINDING}{name}");
            if let Some(value) = dimensions.get(name) {
                local.insert(protected.clone(), value.clone());
                signedness.insert(protected.clone(), value.signed);
            }
            if let Some(value) = dimensions.const_env.get(name) {
                local.const_env.insert(protected.clone(), *value);
                if let Some(ty) = parameter_types_from_const_env(&dimensions.const_env).get(name) {
                    insert_parameter_type_markers(&mut local.const_env, &protected, *ty);
                }
            }
        }
        for (name, qualified) in &self.names {
            if let Some(value) = dimensions.get(qualified) {
                local.insert(name.clone(), value.clone());
                signedness.insert(name.clone(), value.signed);
            }
        }
        local.expression_signedness = Arc::new(signedness);
        local
    }

    pub fn qualify_function(&self, function: &mut Function) {
        let mut bindings = self.clone();
        for parameter in &function.params {
            bindings.names.remove(&parameter.name);
        }
        bindings.expr(&mut function.body);
    }

    fn functions(&self, functions: &HashMap<String, Function>) -> HashMap<String, Function> {
        let mut functions = functions.clone();
        let mut bindings = self.clone();
        bindings.names = self
            .shadowed
            .iter()
            .map(|name| (name.clone(), format!("{OUTER_BINDING}{name}")))
            .collect();
        for function in functions.values_mut() {
            bindings.qualify_function(function);
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
            Expr::Call { args, .. } => args.iter_mut().for_each(|e| self.expr(e)),
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
            ConstExpr::Function { args, .. } => args.iter_mut().for_each(|e| self.constant(e)),
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
        match &mut assignment.lhs {
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
        self.expr(&mut assignment.rhs);
    }
}

#[derive(Clone, Default)]
struct Scope {
    path: String,
    env: HashMap<String, i128>,
    literals: HashMap<String, Expr>,
    names: HashMap<String, String>,
    shadowed: HashSet<String>,
}

struct Elaborator<'a, 'b> {
    tree: &'a SyntaxTree,
    aliases: &'b HashMap<String, Type>,
    remaining: usize,
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
        items: Vec::new(),
    };
    let parameters =
        parameters_from_module_node(node.clone(), tree, aliases, env, &HashMap::default())?;
    let mut literals = parameter_value_env(&parameters, env);
    // Numeric values and their type markers are already carried by `env`.
    literals.retain(|name, _| !env.contains_key(name));
    let scope = Scope {
        env: env.clone(),
        literals,
        ..Scope::default()
    };
    let mut ordinal = 0;
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
    fn expression(&self, expr: &sv_parser::ConstantExpression, scope: &Scope) -> Option<ConstExpr> {
        let expr = const_expr_from_ref_node_with_env(
            RefNode::ConstantExpression(expr),
            self.tree,
            &scope.env,
            self.aliases,
        )?;
        expr_to_const(substitute_expr_idents(
            const_expr_to_expr(expr),
            &scope.literals,
        ))
    }

    fn value(
        &self,
        expr: &sv_parser::ConstantExpression,
        scope: &Scope,
        detail: &str,
    ) -> Result<i128, AnalyzerError> {
        self.expression(expr, scope)
            .and_then(|expr| eval_ast_const_expr(&expr, &scope.env))
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
                            let value = self.value(
                                &generate.nodes.1.nodes.1,
                                scope,
                                "unknown conditional-generate condition",
                            )?;
                            let selected = if value != 0 {
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
                                .expression(&generate.nodes.1.nodes.1, scope)
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
                                            let label =
                                                self.expression(label, scope).ok_or_else(|| {
                                                    AnalyzerError::Unsupported(
                                                        "unknown case-generate label".to_string(),
                                                    )
                                                })?;
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
                        iteration.shadowed.insert(name.clone());
                        iteration.literals.remove(&name);
                        if self.value(
                            &generate.nodes.1.nodes.1.2.nodes.0,
                            &iteration,
                            "loop-generate condition",
                        )? == 0
                        {
                            break;
                        }
                        self.remaining = self.remaining.checked_sub(1).ok_or_else(|| {
                            AnalyzerError::Unsupported(
                                "loop-generate unroll limit exceeded".to_string(),
                            )
                        })?;
                        self.block(&generate.nodes.2, &iteration, *ordinal, Some(value))?;
                        value = next_genvar_value(
                            value,
                            &generate.nodes.1.nodes.1.4,
                            self.tree,
                            &iteration.env,
                            self.aliases,
                        )
                        .ok_or_else(|| {
                            AnalyzerError::Unsupported("genvar update operator".to_string())
                        })?;
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
        if scope.path.contains('[')
            && RefNode::ModuleOrGenerateItem(item)
                .into_iter()
                .any(|node| matches!(node, RefNode::FunctionDeclaration(_)))
        {
            return Err(AnalyzerError::Unsupported(
                "function declaration inside loop-generate".to_string(),
            ));
        }
        self.items.push(Item {
            node: item,
            env: scope.env.clone(),
            literals: scope.literals.clone(),
            names: scope.names.clone(),
            shadowed: scope.shadowed.clone(),
            scope: scope.path.clone(),
        });
        Ok(())
    }

    fn block(
        &mut self,
        block: &'a sv_parser::GenerateBlock,
        parent: &Scope,
        ordinal: usize,
        index: Option<i128>,
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
        if let Some(index) = index {
            name.push_str(&format!("[{index}]"));
        }
        let mut scope = parent.clone();
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
        for item in &children {
            if add_localparams_from_generate_item_with_literals(
                item,
                self.tree,
                &mut scope.env,
                self.aliases,
                Some(&mut scope.literals),
            ) {
                for node in RefNode::GenerateItem(item) {
                    if let RefNode::ParamAssignment(parameter) = node {
                        let name = parameter_name(RefNode::ParamAssignment(parameter), self.tree)?;
                        if !declared.insert(name.clone()) {
                            return Err(AnalyzerError::Unsupported(format!(
                                "duplicate generate-local declaration `{name}`"
                            )));
                        }
                        scope.names.remove(&name);
                        scope.shadowed.insert(name);
                    }
                }
                continue;
            }
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
                        signals_from_module_or_generate_item(
                            item,
                            self.tree,
                            self.aliases,
                            &scope.env,
                            &mut signals,
                        )?;
                    }
                    _ => {}
                }
            }
            for signal in signals {
                if !declared.insert(signal.name.clone()) {
                    return Err(AnalyzerError::Unsupported(format!(
                        "duplicate generate-local declaration `{}`",
                        signal.name
                    )));
                }
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
                scope.shadowed.insert(signal.name.clone());
                scope.names.insert(
                    signal.name.clone(),
                    format!("{}.{}", scope.path, signal.name),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn analyze(source: &str) -> Result<Source, AnalyzerError> {
        let tree = crate::syntax::parse_source(source, Path::new("generate.sv"))?;
        Source::from_syntax(&tree)
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
