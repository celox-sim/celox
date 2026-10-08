//! Combinational process collection: `always_comb`, `always @*`, and continuous assignments.

use super::*;

pub(super) fn comb_processes_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut procedural::BodyState<'_>,
) -> Result<Vec<CombProcess>, AnalyzerError> {
    let mut processes = Vec::new();
    for item in generate::items(
        node,
        syntax_tree,
        const_env,
        &packed_dimensions.type_aliases,
    )? {
        let start = processes.len();
        let mut base_dimensions = packed_dimensions.clone();
        base_dimensions.functions = Arc::new(functions.clone());
        base_dimensions.expression_signedness = Arc::new(expression_signedness.clone());
        let dimensions = item.dimensions(&base_dimensions);
        let literals = item.parameter_literals(parameter_literals);
        comb_processes_from_module_or_generate_item(
            item.node,
            None,
            syntax_tree,
            &item.env,
            &dimensions,
            &dimensions.functions,
            &dimensions.expression_signedness,
            &literals,
            &mut processes,
            state,
        )?;
        for process in &mut processes[start..] {
            for assignment in &mut process.assignments {
                item.assignment(assignment);
            }
            for stmt in &mut process.body {
                procedural::qualify_stmt(&item, stmt);
            }
        }
    }
    Ok(processes)
}

fn comb_processes_from_module_or_generate_item(
    item: &sv_parser::ModuleOrGenerateItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
    processes: &mut Vec<CombProcess>,
    state: &mut procedural::BodyState<'_>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = item {
        comb_processes_from_module_common_item(
            &item.nodes.1,
            condition,
            syntax_tree,
            const_env,
            packed_dimensions,
            functions,
            expression_signedness,
            parameter_literals,
            processes,
            state,
        )?;
    }
    Ok(())
}

fn comb_processes_from_module_common_item(
    item: &sv_parser::ModuleCommonItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
    processes: &mut Vec<CombProcess>,
    state: &mut procedural::BodyState<'_>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::ModuleCommonItem::ContinuousAssign(assign) => {
            processes.extend(
                assignments_from_continuous_assign(assign, syntax_tree, packed_dimensions)?
                    .into_iter()
                    .map(|assignment| {
                        substitute_assignment_constants_with_parameter_literals(
                            assignment,
                            const_env,
                            parameter_literals,
                        )
                    })
                    .map(|assignment| {
                        CombProcess::new(
                            CombProcessKind::ContinuousAssign,
                            condition.clone().map(|condition| {
                                substitute_const_expr_constants(condition, const_env)
                            }),
                            vec![assignment],
                        )
                    }),
            );
        }
        sv_parser::ModuleCommonItem::AlwaysConstruct(always) => {
            let mut local_packed_dimensions = packed_dimensions.clone();
            local_packed_dimensions.const_env = const_env.clone();
            local_packed_dimensions.parameter_values = parameter_literals.clone();
            let _ = (functions, expression_signedness);
            if let Some(process) = comb_process_from_always_construct(
                always,
                condition,
                syntax_tree,
                &local_packed_dimensions,
                parameter_literals,
                state,
            )? {
                processes.push(substitute_process_constants(process, const_env));
            }
        }
        sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(declaration) => {
            // `wire [7:0] w = expr;` declares a net and drives it continuously.
            for assignment in
                net_declaration_assignments(declaration, syntax_tree, packed_dimensions)?
            {
                let assignment = substitute_assignment_constants_with_parameter_literals(
                    assignment,
                    const_env,
                    parameter_literals,
                );
                processes.push(CombProcess::new(
                    CombProcessKind::ContinuousAssign,
                    condition
                        .clone()
                        .map(|condition| substitute_const_expr_constants(condition, const_env)),
                    vec![assignment],
                ));
            }
        }
        sv_parser::ModuleCommonItem::NetAlias(_) => {
            return Err(AnalyzerError::Unsupported(
                "module-level net alias".to_string(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn net_declaration_assignments(
    declaration: &sv_parser::ModuleOrGenerateItemDeclaration,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<Assignment>, AnalyzerError> {
    let sv_parser::ModuleOrGenerateItemDeclaration::PackageOrGenerateItemDeclaration(declaration) =
        declaration
    else {
        return Ok(Vec::new());
    };
    let sv_parser::PackageOrGenerateItemDeclaration::NetDeclaration(net) = &**declaration else {
        return Ok(Vec::new());
    };
    let sv_parser::NetDeclaration::NetType(net) = &**net else {
        return Ok(Vec::new());
    };
    let mut assignments = Vec::new();
    for assignment in net.nodes.5.nodes.0.contents() {
        let Some((_, expression)) = &assignment.nodes.2 else {
            continue;
        };
        let name = identifier_text(RefNode::NetIdentifier(&assignment.nodes.0), syntax_tree)
            .ok_or_else(|| AnalyzerError::Unsupported("net declaration assignment".to_string()))?;
        if let Some(target) = packed_dimensions.get(&name) {
            check_unpacked_array_assignment(
                expression,
                target,
                || "net declaration assignment".to_string(),
                syntax_tree,
                packed_dimensions,
            )?;
        }
        let rhs = expr_from_expression_with_types(expression, syntax_tree, packed_dimensions)?;
        assignments.push(Assignment::new(LValue::Ident(name), rhs));
    }
    Ok(assignments)
}

fn assignments_from_continuous_assign(
    assign: &sv_parser::ContinuousAssign,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<Assignment>, AnalyzerError> {
    match assign {
        sv_parser::ContinuousAssign::Net(assign) => assign
            .nodes
            .3
            .nodes
            .0
            .contents()
            .into_iter()
            .map(|assignment| {
                if let Some(target) =
                    net_lvalue_unpacked_shape(&assignment.nodes.0, syntax_tree, packed_dimensions)
                {
                    check_unpacked_array_assignment(
                        &assignment.nodes.2,
                        &target,
                        || "continuous assignment".to_string(),
                        syntax_tree,
                        packed_dimensions,
                    )?;
                }
                let lhs =
                    net_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)?;
                let rhs = expr_from_expression_for_lvalue(
                    &assignment.nodes.2,
                    &lhs,
                    syntax_tree,
                    packed_dimensions,
                )?;
                let rhs = if matches!(
                    lhs,
                    LValue::Select {
                        is_2state: true,
                        ..
                    }
                ) {
                    coerce_procedural_assignment_rhs(rhs, &lhs, packed_dimensions)
                } else {
                    rhs
                };
                Ok(Assignment::new(lhs, rhs))
            })
            .collect(),
        sv_parser::ContinuousAssign::Variable(assign) => assign
            .nodes
            .2
            .nodes
            .0
            .contents()
            .into_iter()
            .map(|assignment| {
                if let Some(target) = variable_lvalue_unpacked_shape(
                    &assignment.nodes.0,
                    syntax_tree,
                    packed_dimensions,
                ) {
                    check_unpacked_array_assignment(
                        &assignment.nodes.2,
                        &target,
                        || "continuous assignment".to_string(),
                        syntax_tree,
                        packed_dimensions,
                    )?;
                }
                let lhs =
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)?;
                let rhs = expr_from_expression_for_lvalue(
                    &assignment.nodes.2,
                    &lhs,
                    syntax_tree,
                    packed_dimensions,
                )?;
                let rhs = if matches!(
                    lhs,
                    LValue::Select {
                        is_2state: true,
                        ..
                    }
                ) {
                    coerce_procedural_assignment_rhs(rhs, &lhs, packed_dimensions)
                } else {
                    rhs
                };
                Ok(Assignment::new(lhs, rhs))
            })
            .collect(),
    }
}

fn comb_process_from_always_construct(
    always: &sv_parser::AlwaysConstruct,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut procedural::BodyState<'_>,
) -> Result<Option<CombProcess>, AnalyzerError> {
    let Some(body) = always_comb_body(always) else {
        return Ok(None);
    };
    let mut builder = procedural::BodyBuilder::new(
        syntax_tree,
        packed_dimensions,
        state,
        system_functions::Body::Always,
    );
    let mut body = builder.statement(body)?;
    for stmt in &mut body {
        procedural::substitute_stmt_constants(stmt, &HashMap::default(), parameter_literals);
    }
    Ok(Some(CombProcess::procedural(
        CombProcessKind::AlwaysComb,
        condition,
        body,
    )))
}
