//! Unsupported-construct checks and bounded procedural-loop validation.

use super::*;

pub(super) fn reject_unsupported_multidimensional_packed_bounds(
    ports: &[Port],
    signals: &[Signal],
    const_env: &HashMap<String, i128>,
) -> Result<(), AnalyzerError> {
    let unsupported = ports
        .iter()
        .map(Port::r#type)
        .chain(signals.iter().map(Signal::r#type))
        .any(|r#type| {
            let ranges = r#type.packed_ranges();
            ranges.len() > 1
                && ranges.iter().any(|range| {
                    let left = eval_ast_const_expr(range.left(), const_env);
                    let right = eval_ast_const_expr(range.right(), const_env);
                    !matches!(
                        (left, right),
                        (Some(left), Some(right))
                            if (right == 0 && left >= 0) || (left == 0 && right >= 0)
                    )
                })
        });
    if unsupported {
        Err(AnalyzerError::Unsupported(
            "non-zero-based multidimensional packed range".to_string(),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn static_for_loop_iterations(
    loop_statement: &sv_parser::LoopStatement,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Option<(String, Vec<i128>)> {
    let sv_parser::LoopStatement::For(loop_statement) = loop_statement else {
        return None;
    };
    let (_, _, condition, _, step) = &loop_statement.nodes.1.nodes.1;
    let (name, initial_value) =
        static_for_loop_initial_value(loop_statement, syntax_tree, const_env)?;
    if for_loop_body_writes_index(loop_statement, &name, syntax_tree) {
        return None;
    }
    let condition = const_expr_from_expr(condition.as_ref()?, syntax_tree)?;
    let steps = step.as_ref()?.nodes.0.contents();
    let [step] = steps.as_slice() else {
        return None;
    };

    let mut values = Vec::new();
    let mut value = initial_value;
    for _ in 0..MAX_STATIC_PROCEDURAL_LOOP_EXPANSION {
        let mut loop_env = const_env.clone();
        loop_env.insert(name.clone(), value);
        insert_parameter_type_markers(
            &mut loop_env,
            &name,
            ExprType {
                width: 32,
                signed: true,
            },
        );
        let condition_value = eval_ast_const_expr(&condition, &loop_env)?;
        if condition_value == 0 {
            return Some((name, values));
        }
        values.push(value);
        let next = next_for_loop_value(step, &name, value, syntax_tree, &loop_env);
        value = coerce_for_loop_index_value(next?)?;
    }

    let mut loop_env = const_env.clone();
    loop_env.insert(name.clone(), value);
    insert_parameter_type_markers(
        &mut loop_env,
        &name,
        ExprType {
            width: 32,
            signed: true,
        },
    );
    (eval_ast_const_expr(&condition, &loop_env) == Some(0)).then_some((name, values))
}

fn for_loop_body_writes_index(
    loop_statement: &sv_parser::LoopStatementFor,
    name: &str,
    syntax_tree: &SyntaxTree,
) -> bool {
    RefNode::StatementOrNull(&loop_statement.nodes.2)
        .into_iter()
        .any(|node| {
            let lvalue = match node {
                RefNode::BlockingAssignment(assignment) => match assignment {
                    sv_parser::BlockingAssignment::Variable(assignment) => &assignment.nodes.0,
                    sv_parser::BlockingAssignment::OperatorAssignment(assignment) => {
                        &assignment.nodes.0
                    }
                    _ => return false,
                },
                RefNode::NonblockingAssignment(assignment) => &assignment.nodes.0,
                _ => return false,
            };
            for_loop_variable_lvalue_name(lvalue, syntax_tree).as_deref() == Some(name)
        })
}

pub(super) fn static_for_loop_initial_value(
    loop_statement: &sv_parser::LoopStatementFor,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Option<(String, i128)> {
    let initialization = loop_statement.nodes.1.nodes.1.0.as_ref()?;
    let (name, value) = for_loop_initialization(initialization, syntax_tree)?;
    let value = eval_ast_const_expr(&value, const_env)?;
    Some((name, coerce_for_loop_index_value(value)?))
}

fn coerce_for_loop_index_value(value: i128) -> Option<i128> {
    const MODULUS: i128 = 1i128 << 32;
    const SIGN_BIT: i128 = 1i128 << 31;
    let value = value.rem_euclid(MODULUS);
    Some(if value >= SIGN_BIT {
        value - MODULUS
    } else {
        value
    })
}

fn validate_static_for_loops_in_statement_or_null(
    statement: &sv_parser::StatementOrNull,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    remaining_expansion: &mut usize,
) -> Result<(), AnalyzerError> {
    if let sv_parser::StatementOrNull::Statement(statement) = statement {
        validate_static_for_loops_in_statement_with_budget(
            statement,
            syntax_tree,
            const_env,
            remaining_expansion,
        )?;
    }
    Ok(())
}

fn validate_static_for_loops_in_statement(
    statement: &sv_parser::Statement,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Result<(), AnalyzerError> {
    let mut remaining_expansion = MAX_STATIC_PROCEDURAL_LOOP_EXPANSION;
    validate_static_for_loops_in_statement_with_budget(
        statement,
        syntax_tree,
        const_env,
        &mut remaining_expansion,
    )
}

fn validate_static_for_loops_in_statement_with_budget(
    statement: &sv_parser::Statement,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    remaining_expansion: &mut usize,
) -> Result<(), AnalyzerError> {
    match &statement.nodes.2 {
        sv_parser::StatementItem::ProceduralTimingControlStatement(timing) => {
            validate_static_for_loops_in_statement_or_null(
                &timing.nodes.1,
                syntax_tree,
                const_env,
                remaining_expansion,
            )?;
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            for statement in &block.nodes.3 {
                validate_static_for_loops_in_statement_or_null(
                    statement,
                    syntax_tree,
                    const_env,
                    remaining_expansion,
                )?;
            }
        }
        sv_parser::StatementItem::ConditionalStatement(conditional) => {
            validate_static_for_loops_in_statement_or_null(
                &conditional.nodes.3,
                syntax_tree,
                const_env,
                remaining_expansion,
            )?;
            for (_, _, _, branch) in &conditional.nodes.4 {
                validate_static_for_loops_in_statement_or_null(
                    branch,
                    syntax_tree,
                    const_env,
                    remaining_expansion,
                )?;
            }
            if let Some((_, branch)) = &conditional.nodes.5 {
                validate_static_for_loops_in_statement_or_null(
                    branch,
                    syntax_tree,
                    const_env,
                    remaining_expansion,
                )?;
            }
        }
        sv_parser::StatementItem::CaseStatement(case) => {
            let sv_parser::CaseStatement::Normal(case) = &**case else {
                return Ok(());
            };
            for item in std::iter::once(&case.nodes.3).chain(case.nodes.4.iter()) {
                let statement = match item {
                    sv_parser::CaseItem::NonDefault(item) => &item.nodes.2,
                    sv_parser::CaseItem::Default(item) => &item.nodes.2,
                };
                validate_static_for_loops_in_statement_or_null(
                    statement,
                    syntax_tree,
                    const_env,
                    remaining_expansion,
                )?;
            }
        }
        sv_parser::StatementItem::LoopStatement(loop_statement) => {
            let (name, values) = static_for_loop_iterations(loop_statement, syntax_tree, const_env)
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("procedural loop inside always_ff".to_string())
                })?;
            *remaining_expansion =
                remaining_expansion
                    .checked_sub(values.len())
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported(
                            "procedural loop unroll limit exceeded".to_string(),
                        )
                    })?;
            let sv_parser::LoopStatement::For(loop_statement) = &**loop_statement else {
                unreachable!();
            };
            for value in values {
                let mut loop_env = const_env.clone();
                loop_env.insert(name.clone(), value);
                insert_parameter_type_markers(
                    &mut loop_env,
                    &name,
                    ExprType {
                        width: 32,
                        signed: true,
                    },
                );
                validate_static_for_loops_in_statement_or_null(
                    &loop_statement.nodes.2,
                    syntax_tree,
                    &loop_env,
                    remaining_expansion,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn for_loop_initialization(
    initialization: &sv_parser::ForInitialization,
    syntax_tree: &SyntaxTree,
) -> Option<(String, ConstExpr)> {
    match initialization {
        sv_parser::ForInitialization::Declaration(declaration) => {
            let declarations = declaration.nodes.0.contents();
            let [declaration] = declarations.as_slice() else {
                return None;
            };
            if !for_loop_index_type_is_supported(&declaration.nodes.1) {
                return None;
            }
            let assignments = declaration.nodes.2.contents();
            let [assignment] = assignments.as_slice() else {
                return None;
            };
            let name = identifier_text(RefNode::VariableIdentifier(&assignment.0), syntax_tree)?;
            let value = const_expr_from_expr(&assignment.2, syntax_tree)?;
            Some((name, value))
        }
        // An assignment-style initializer targets a variable in the enclosing
        // procedural scope. Its initialization and loop steps are observable
        // assignments, so it cannot be treated as a declaration-scoped
        // compile-time unrolling constant.
        sv_parser::ForInitialization::ListOfVariableAssignments(_) => None,
    }
}

fn for_loop_index_type_is_supported(data_type: &sv_parser::DataType) -> bool {
    matches!(
        data_type,
        sv_parser::DataType::Atom(data_type)
            if matches!(
                data_type.nodes.0,
                sv_parser::IntegerAtomType::Int(_) | sv_parser::IntegerAtomType::Integer(_)
            ) && !matches!(data_type.nodes.1, Some(sv_parser::Signing::Unsigned(_)))
    )
}

fn for_loop_variable_lvalue_name(
    lvalue: &sv_parser::VariableLvalue,
    syntax_tree: &SyntaxTree,
) -> Option<String> {
    let sv_parser::VariableLvalue::Identifier(identifier) = lvalue else {
        return None;
    };
    identifier_text(
        RefNode::HierarchicalVariableIdentifier(&identifier.nodes.1),
        syntax_tree,
    )
}

fn next_for_loop_value(
    step: &sv_parser::ForStepAssignment,
    name: &str,
    value: i128,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Option<i128> {
    match step {
        sv_parser::ForStepAssignment::OperatorAssignment(step) => {
            if for_loop_variable_lvalue_name(&step.nodes.0, syntax_tree)? != name {
                return None;
            }
            let operator = syntax_tree.get_str(&step.nodes.1.nodes.0)?.trim();
            let rhs = const_expr_from_expr(&step.nodes.2, syntax_tree)?;
            if operator == "=" {
                return eval_ast_const_expr(&rhs, const_env);
            }
            let op = match operator {
                "+=" => BinaryOp::Add,
                "-=" => BinaryOp::Sub,
                "*=" => BinaryOp::Mul,
                "/=" => BinaryOp::Div,
                "%=" => BinaryOp::Mod,
                _ => return None,
            };
            let lhs = ConstExpr::Literal(format_typed_parameter_literal(value, 32, true));
            let expression = ConstExpr::Binary {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
            };
            eval_ast_const_expr(&expression, const_env)
        }
        sv_parser::ForStepAssignment::IncOrDecExpression(step) => {
            let (lvalue, operator) = match &**step {
                sv_parser::IncOrDecExpression::Prefix(step) => (&step.nodes.2, &step.nodes.0),
                sv_parser::IncOrDecExpression::Suffix(step) => (&step.nodes.0, &step.nodes.2),
            };
            if for_loop_variable_lvalue_name(lvalue, syntax_tree)? != name {
                return None;
            }
            match syntax_tree.get_str(&operator.nodes.0)? {
                "++" => value.checked_add(1),
                "--" => value.checked_sub(1),
                _ => None,
            }
        }
        sv_parser::ForStepAssignment::FunctionSubroutineCall(_) => None,
    }
}

pub(super) fn reject_silently_ignored_constructs(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<(), AnalyzerError> {
    let is_module = matches!(node, RefNode::ModuleDeclarationAnsi(_));
    let generated_nodes: Vec<_> = if is_module {
        node.clone()
            .into_iter()
            .filter(|n| {
                matches!(
                    n,
                    RefNode::ConditionalGenerateConstruct(_) | RefNode::LoopGenerateConstruct(_)
                )
            })
            .flat_map(|n| n.into_iter())
            .collect()
    } else {
        Vec::new()
    };
    for child in node.clone() {
        if generated_nodes.iter().any(|n| n == &child) {
            continue;
        }
        match child {
            RefNode::Cast(cast)
                if !cast_is_supported(cast, syntax_tree, const_env, type_aliases) =>
            {
                return Err(AnalyzerError::Unsupported("cast expression".to_string()));
            }
            RefNode::ConstantCast(cast)
                if !constant_cast_is_supported(cast, syntax_tree, const_env, type_aliases) =>
            {
                return Err(AnalyzerError::Unsupported("constant cast expression".to_string()));
            }
            RefNode::AnsiPortDeclarationNet(port) if port.nodes.3.is_some() => {
                return Err(AnalyzerError::Unsupported(
                    "ANSI port default value".to_string(),
                ));
            }
            RefNode::AnsiPortDeclarationVariable(port) if port.nodes.3.is_some() => {
                return Err(AnalyzerError::Unsupported(
                    "ANSI port default value".to_string(),
                ));
            }
            RefNode::AnsiPortDeclarationVariable(port)
                if RefNode::AnsiPortDeclarationVariable(port)
                    .into_iter()
                    .any(|node| matches!(node, RefNode::DataTypeEnum(_))) =>
            {
                return Err(AnalyzerError::Unsupported("enum port".to_string()));
            }
            RefNode::AnsiPortDeclarationNet(port)
                if RefNode::AnsiPortDeclarationNet(port)
                    .into_iter()
                    .any(|node| matches!(node, RefNode::DataTypeEnum(_))) =>
            {
                return Err(AnalyzerError::Unsupported("enum port".to_string()));
            }
            RefNode::AlwaysConstruct(always) => {
                if matches!(
                    always.nodes.0,
                    sv_parser::AlwaysKeyword::Always(_) | sv_parser::AlwaysKeyword::AlwaysLatch(_)
                ) {
                    return Err(AnalyzerError::Unsupported(
                        "always and always_latch processes".to_string(),
                    ));
                }
                let body = RefNode::Statement(&always.nodes.1);
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_))
                    && body
                        .clone()
                        .into_iter()
                        .any(|node| matches!(node, RefNode::BlockingAssignment(_)))
                {
                    return Err(AnalyzerError::Unsupported(
                        "blocking assignment inside always_ff".to_string(),
                    ));
                }
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_)) {
                    validate_static_for_loops_in_statement(&always.nodes.1, syntax_tree, const_env)?;
                }
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysComb(_)) {
                    validate_static_for_loops_in_statement(&always.nodes.1, syntax_tree, const_env)
                        .map_err(|error| match error {
                            AnalyzerError::Unsupported(construct)
                                if construct == "procedural loop inside always_ff" =>
                            {
                                AnalyzerError::Unsupported(
                                    "procedural loop inside always_comb".to_string(),
                                )
                            }
                            error => error,
                        })?;
                }
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_))
                    && body.clone().into_iter().any(|node| {
                        matches!(
                            node,
                            RefNode::CaseStatement(case)
                                if !matches!(
                                    case,
                                    sv_parser::CaseStatement::Normal(case)
                                        if matches!(case.nodes.1, sv_parser::CaseKeyword::Case(_))
                                )
                        )
                    })
                {
                    return Err(AnalyzerError::Unsupported(
                        "casez, casex, or pattern case inside always_ff".to_string(),
                    ));
                }
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysComb(_))
                    && body
                        .clone()
                        .into_iter()
                        .any(|node| matches!(node, RefNode::NonblockingAssignment(_)))
                {
                    return Err(AnalyzerError::Unsupported(
                        "nonblocking assignment inside always_comb".to_string(),
                    ));
                }
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_))
                    && body.clone().into_iter().any(|node| {
                        matches!(
                            node,
                            RefNode::EventExpressionExpression(event)
                                if event.nodes.2.is_some()
                        )
                    })
                {
                    return Err(AnalyzerError::Unsupported(
                        "iff-qualified always_ff event".to_string(),
                    ));
                }
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_))
                    && body
                        .clone()
                        .into_iter()
                        .any(|node| matches!(node, RefNode::VariableLvalueLvalue(_)))
                {
                    return Err(AnalyzerError::Unsupported(
                        "concatenated always_ff assignment target".to_string(),
                    ));
                }
                if body
                    .into_iter()
                    .any(|node| matches!(node, RefNode::DataDeclaration(_)))
                {
                    let detail = if matches!(
                        always.nodes.0,
                        sv_parser::AlwaysKeyword::AlwaysComb(_)
                    ) {
                        "block-local declaration inside always_comb"
                    } else {
                        "procedural local data declaration"
                    };
                    return Err(AnalyzerError::Unsupported(detail.to_string()));
                }
            }
            RefNode::NetDeclAssignment(assignment) if assignment.nodes.2.is_some() => {
                return Err(AnalyzerError::Unsupported(
                    "net declaration assignment".to_string(),
                ));
            }
            RefNode::InitialConstruct(_) => {
                return Err(AnalyzerError::Unsupported("initial construct".to_string()));
            }
            RefNode::FinalConstruct(_) => {
                return Err(AnalyzerError::Unsupported("final construct".to_string()));
            }
            RefNode::ElaborationSystemTask(_) => {
                return Err(AnalyzerError::Unsupported(
                    "elaboration system task".to_string(),
                ));
            }
            RefNode::BindDirective(_) => {
                return Err(AnalyzerError::Unsupported("bind directive".to_string()));
            }
            RefNode::SpecifyBlock(_) => {
                return Err(AnalyzerError::Unsupported("specify block".to_string()));
            }
            RefNode::MintypmaxExpressionTernary(_)
            | RefNode::ConstantMintypmaxExpressionTernary(_) => {
                return Err(AnalyzerError::Unsupported(
                    "mintypmax expression".to_string(),
                ));
            }
            RefNode::ConcurrentAssertionItem(_) => {
                return Err(AnalyzerError::Unsupported(
                    "concurrent assertion".to_string(),
                ));
            }
            RefNode::ProceduralAssertionStatement(_) => {
                return Err(AnalyzerError::Unsupported(
                    "procedural assertion statement".to_string(),
                ));
            }
            RefNode::VariableDeclAssignmentVariable(assignment) if assignment.nodes.2.is_some() => {
                return Err(AnalyzerError::Unsupported(
                    "variable declaration initializer".to_string(),
                ));
            }
            RefNode::IndexedRange(_) | RefNode::ConstantIndexedRange(_) => {
                return Err(AnalyzerError::Unsupported(
                    "indexed part-select".to_string(),
                ));
            }
            RefNode::DataTypeStructUnion(_) => {
                return Err(AnalyzerError::Unsupported(
                    "packed struct or union type".to_string(),
                ));
            }
            RefNode::ConstantFunctionCall(call)
                if matches!(
                    &call.nodes.0.nodes.0,
                    sv_parser::SubroutineCall::TfCall(call) if call.nodes.2.is_some()
                ) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "user constant function call".to_string(),
                ));
            }
            RefNode::NamedPortConnectionAsterisk(_) => {
                return Err(AnalyzerError::Unsupported(
                    "wildcard port connection".to_string(),
                ));
            }
            RefNode::ContinuousAssign(sv_parser::ContinuousAssign::Net(assign))
                if assign.nodes.2.is_some() =>
            {
                return Err(AnalyzerError::Unsupported(
                    "delayed continuous assignment".to_string(),
                ));
            }
            RefNode::ContinuousAssign(sv_parser::ContinuousAssign::Variable(assign))
                if assign.nodes.1.is_some() =>
            {
                return Err(AnalyzerError::Unsupported(
                    "delayed continuous assignment".to_string(),
                ));
            }
            RefNode::FunctionDeclaration(function)
                if function_has_static_local_state(function) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "static function-local state".to_string(),
                ));
            }
            RefNode::FunctionDeclaration(function)
                if RefNode::FunctionDeclaration(function)
                    .into_iter()
                    .any(non_input_function_port) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "output or inout function argument".to_string(),
                ));
            }
            RefNode::FunctionDeclaration(function)
                if RefNode::FunctionDeclaration(function).into_iter().any(|node| {
                    matches!(
                        node,
                        RefNode::CaseStatement(case)
                            if !matches!(
                                case,
                                sv_parser::CaseStatement::Normal(case)
                                    if matches!(case.nodes.1, sv_parser::CaseKeyword::Case(_))
                            )
                    )
                }) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "casez or casex inside function".to_string(),
                ));
            }
            RefNode::FunctionDeclaration(function)
                if RefNode::FunctionDeclaration(function)
                    .into_iter()
                    .any(|node| matches!(node, RefNode::BlockingAssignment(assignment) if blocking_assignment_has_non_plain_lvalue(assignment))) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "selected or composite assignment inside function".to_string(),
                ));
            }
            RefNode::GateInstantiation(_) => {
                return Err(AnalyzerError::Unsupported(
                    "gate primitive instantiation".to_string(),
                ));
            }
            RefNode::DefparamAssignment(_) => {
                return Err(AnalyzerError::Unsupported(
                    "defparam assignment".to_string(),
                ));
            }
            RefNode::NetType(
                sv_parser::NetType::Supply0(_)
                | sv_parser::NetType::Supply1(_)
                | sv_parser::NetType::Tri0(_)
                | sv_parser::NetType::Tri1(_),
            ) => {
                return Err(AnalyzerError::Unsupported(
                    "pull or supply net type".to_string(),
                ));
            }
            RefNode::NetType(sv_parser::NetType::Trireg(_)) => {
                return Err(AnalyzerError::Unsupported(
                    "trireg charge storage".to_string(),
                ));
            }
            RefNode::PackedDimensionRange(range)
                if const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(&range.nodes.0.nodes.1.nodes.0),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )
                .is_none()
                    || const_expr_from_ref_node_with_env(
                        RefNode::ConstantExpression(&range.nodes.0.nodes.1.nodes.2),
                        syntax_tree,
                        const_env,
                        type_aliases,
                    )
                    .is_none() =>
            {
                return Err(AnalyzerError::Unsupported(
                    "unsupported packed range".to_string(),
                ));
            }
            RefNode::PackageImportDeclaration(_) | RefNode::PackageScope(_) => {
                return Err(AnalyzerError::Unsupported(
                    "package-dependent systemverilog module".to_string(),
                ));
            }
            RefNode::ParamAssignment(parameter)
                if RefNode::ParamAssignment(parameter).into_iter().any(|node| {
                    matches!(
                        node,
                        RefNode::ConstantExpression(
                            sv_parser::ConstantExpression::Unary(unary)
                        ) if syntax_tree
                            .get_str(&unary.nodes.0.nodes.0.nodes.0)
                            .is_some_and(|op| op == "~")
                            && matches!(
                                const_expr_from_ref_node(
                                    RefNode::ConstantPrimary(&unary.nodes.2),
                                    syntax_tree,
                                ),
                                Some(ConstExpr::Ident(_))
                            )
                    )
                }) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "width-dependent complement in parameter expression".to_string(),
                ));
            }
            RefNode::ParamAssignment(parameter)
                if RefNode::ParamAssignment(parameter).into_iter().any(|node| {
                    matches!(
                        node,
                        RefNode::UnaryOperator(operator)
                            if syntax_tree
                                .get_str(&operator.nodes.0)
                                .is_some_and(|op| matches!(op, "&" | "|" | "^" | "~&" | "~|" | "~^" | "^~"))
                    )
                }) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "reduction operator in parameter expression".to_string(),
                ));
            }
            _ => {}
        }
    }
    if is_module {
        for item in generate::items(node, syntax_tree, const_env, type_aliases)? {
            reject_silently_ignored_constructs(
                RefNode::ModuleOrGenerateItem(item.node),
                syntax_tree,
                &item.env,
                type_aliases,
            )?;
        }
    }

    Ok(())
}

fn blocking_assignment_has_non_plain_lvalue(assignment: &sv_parser::BlockingAssignment) -> bool {
    let lvalue = match assignment {
        sv_parser::BlockingAssignment::Variable(assignment) => &assignment.nodes.0,
        sv_parser::BlockingAssignment::OperatorAssignment(assignment) => &assignment.nodes.0,
        _ => return true,
    };
    let sv_parser::VariableLvalue::Identifier(identifier) = lvalue else {
        return true;
    };
    let select = &identifier.nodes.2;
    select.nodes.0.is_some() || !select.nodes.1.nodes.0.is_empty() || select.nodes.2.is_some()
}

fn non_input_function_port(node: RefNode<'_>) -> bool {
    let direction = match node {
        RefNode::TfPortItem(port) => port.nodes.1.as_ref(),
        RefNode::TfPortDeclaration(port) => Some(&port.nodes.1),
        _ => return false,
    };
    match direction {
        None => false,
        Some(sv_parser::TfPortDirection::PortDirection(direction)) => {
            !matches!(&**direction, sv_parser::PortDirection::Input(_))
        }
        Some(sv_parser::TfPortDirection::ConstRef(_)) => true,
    }
}

fn function_has_static_local_state(function: &sv_parser::FunctionDeclaration) -> bool {
    let function_is_static = !matches!(function.nodes.1, Some(sv_parser::Lifetime::Automatic(_)));
    let local_is_static = |item: &sv_parser::BlockItemDeclaration| {
        let sv_parser::BlockItemDeclaration::Data(item) = item else {
            return false;
        };
        let sv_parser::DataDeclaration::Variable(variable) = &item.nodes.1 else {
            return false;
        };
        match &variable.nodes.2 {
            Some(sv_parser::Lifetime::Automatic(_)) => false,
            Some(sv_parser::Lifetime::Static(_)) => true,
            None => function_is_static,
        }
    };
    match &function.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => {
            body.nodes.5.iter().any(local_is_static)
        }
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => body.nodes.4.iter().any(|item| {
            matches!(
                item,
                sv_parser::TfItemDeclaration::BlockItemDeclaration(item)
                    if local_is_static(item)
            )
        }),
    }
}
