//! Function declaration validation and function-body expression construction.

use super::*;

pub(super) fn functions_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
) -> Result<HashMap<String, Function>, AnalyzerError> {
    let mut functions = HashMap::default();
    let type_aliases =
        type_aliases_from_module_node_with_env(node.clone(), syntax_tree, const_env)?;
    let active = generate::items(node.clone(), syntax_tree, const_env, &type_aliases)?;
    for (item, child) in active.iter().flat_map(|item| {
        RefNode::ModuleOrGenerateItem(item.node)
            .into_iter()
            .map(move |child| (item, child))
    }) {
        let RefNode::FunctionDeclaration(declaration) = child else {
            continue;
        };
        let mut literals = item.literals.clone();
        let const_env = &item.env;
        let function_dimensions = item.dimensions(packed_dimensions);
        let packed_dimensions = &function_dimensions;
        validate_function_return_type(declaration, syntax_tree, const_env, &type_aliases)?;
        validate_function_formal_types(declaration, syntax_tree, const_env, &type_aliases)?;
        validate_function_local_names(declaration, syntax_tree, const_env, &type_aliases)?;
        validate_function_declaration_statements(
            declaration,
            syntax_tree,
            const_env,
            &type_aliases,
            packed_dimensions,
        )?;
        if let Some(mut function) = function_from_declaration(
            declaration,
            syntax_tree,
            const_env,
            &type_aliases,
            packed_dimensions,
        ) {
            for parameter in &function.params {
                literals.remove(&parameter.name);
            }
            function.body = substitute_expr_idents(function.body, &literals);
            item.qualify_function(&mut function);
            function.name = item.name(&function.name);
            let name = function.name.clone();
            let mut parameter_names = HashSet::default();
            if let Some(parameter) = function
                .params
                .iter()
                .find(|parameter| !parameter_names.insert(parameter.name.as_str()))
            {
                return Err(AnalyzerError::Unsupported(format!(
                    "duplicate function argument `{}`",
                    parameter.name
                )));
            }
            if functions.insert(name.clone(), function).is_some() {
                return Err(AnalyzerError::Unsupported(format!(
                    "duplicate function declaration `{name}`"
                )));
            }
        }
    }
    Ok(functions)
}

fn validate_function_local_names(
    declaration: &sv_parser::FunctionDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<(), AnalyzerError> {
    let (params, local_types) = match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => {
            let params = body
                .nodes
                .3
                .nodes
                .1
                .as_ref()
                .map(|ports| tf_params(ports, syntax_tree, const_env, type_aliases))
                .unwrap_or_default();
            let local_types = function_local_types_from_block_items(
                &body.nodes.5,
                syntax_tree,
                const_env,
                type_aliases,
            )
            .unwrap_or_default();
            (params, local_types)
        }
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => {
            let params = tf_item_params(&body.nodes.4, syntax_tree, const_env, type_aliases);
            let block_items = body.nodes.4.iter().filter_map(|item| match item {
                sv_parser::TfItemDeclaration::BlockItemDeclaration(item) => Some(&**item),
                sv_parser::TfItemDeclaration::TfPortDeclaration(_) => None,
            });
            let local_types = function_local_types_from_block_item_iter(
                block_items,
                syntax_tree,
                const_env,
                type_aliases,
            )
            .unwrap_or_default();
            (params, local_types)
        }
    };
    if let Some(param) = params
        .iter()
        .find(|param| local_types.contains_key(&param.name))
    {
        Err(AnalyzerError::Unsupported(format!(
            "function local shadows formal `{}`",
            param.name
        )))
    } else {
        Ok(())
    }
}

fn validate_function_return_type(
    declaration: &sv_parser::FunctionDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<(), AnalyzerError> {
    let node = match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => &body.nodes.0,
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => &body.nodes.0,
    };
    let unsupported = match node {
        sv_parser::FunctionDataTypeOrImplicit::DataTypeOrVoid(data_type) => {
            let sv_parser::DataTypeOrVoid::DataType(data_type) = &**data_type else {
                return Ok(());
            };
            let r#type = type_from_ref_node(RefNode::DataType(data_type), syntax_tree)
                .or_else(|| type_alias_from_data_type(data_type, syntax_tree, type_aliases));
            r#type
                .as_ref()
                .is_some_and(|r#type| !r#type.unpacked_ranges().is_empty())
                || value_type_from_data_type(data_type, syntax_tree, const_env, type_aliases)
                    .is_none()
        }
        sv_parser::FunctionDataTypeOrImplicit::ImplicitDataType(_) => false,
    };
    if unsupported {
        Err(AnalyzerError::Unsupported(
            "unsupported function return data type or unpacked dimension".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn validate_function_formal_types(
    declaration: &sv_parser::FunctionDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<(), AnalyzerError> {
    let unsupported = |node: &sv_parser::DataTypeOrImplicit| {
        matches!(node, sv_parser::DataTypeOrImplicit::DataType(_))
            && value_type_from_data_type_or_implicit(node, syntax_tree, const_env, type_aliases)
                .is_none()
    };
    let has_unpacked_dimensions =
        |node: &sv_parser::DataTypeOrImplicit, dimensions: &[sv_parser::VariableDimension]| {
            !dimensions.is_empty()
                || type_from_ref_node(RefNode::DataTypeOrImplicit(node), syntax_tree)
                    .or_else(|| {
                        type_alias_from_ref_node(
                            RefNode::DataTypeOrImplicit(node),
                            syntax_tree,
                            type_aliases,
                        )
                    })
                    .is_some_and(|r#type| !r#type.unpacked_ranges().is_empty())
        };
    let invalid = match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => {
            body.nodes.3.nodes.1.as_ref().is_some_and(|ports| {
                ports
                    .nodes
                    .0
                    .contents()
                    .iter()
                    // A type-only item can be the parser's representation of
                    // the shorthand `input logic a, b`; only validate entries
                    // that actually carry a formal identifier here.
                    .any(|port| {
                        port.nodes.4.as_ref().is_some_and(|(_, dimensions, _)| {
                            unsupported(&port.nodes.3)
                                || has_unpacked_dimensions(&port.nodes.3, dimensions)
                        })
                    })
            })
        }
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => body.nodes.4.iter().any(|item| {
            let sv_parser::TfItemDeclaration::TfPortDeclaration(port) = item else {
                return false;
            };
            unsupported(&port.nodes.3)
                || port
                    .nodes
                    .4
                    .nodes
                    .0
                    .contents()
                    .iter()
                    .any(|(_, dimensions, _)| has_unpacked_dimensions(&port.nodes.3, dimensions))
        }),
    };
    if invalid {
        Err(AnalyzerError::Unsupported(
            "unsupported function formal data type or unpacked dimension".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn validate_function_declaration_statements(
    declaration: &sv_parser::FunctionDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    packed_dimensions: &PackedDimensions,
) -> Result<(), AnalyzerError> {
    let function_name = match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => &body.nodes.2,
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => &body.nodes.2,
    };
    let function_name = identifier_text(RefNode::FunctionIdentifier(function_name), syntax_tree);
    let (statements, params, local_types) = match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => {
            let params = body
                .nodes
                .3
                .nodes
                .1
                .as_ref()
                .map(|ports| tf_params(ports, syntax_tree, const_env, type_aliases))
                .unwrap_or_default();
            let local_types = function_local_types_from_block_items(
                &body.nodes.5,
                syntax_tree,
                const_env,
                type_aliases,
            )
            .ok_or_else(|| {
                AnalyzerError::Unsupported("unsupported function local data type".to_string())
            })?;
            (&body.nodes.6, params, local_types)
        }
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => {
            let params = tf_item_params(&body.nodes.4, syntax_tree, const_env, type_aliases);
            let block_items = body.nodes.4.iter().filter_map(|item| match item {
                sv_parser::TfItemDeclaration::BlockItemDeclaration(item) => Some(&**item),
                sv_parser::TfItemDeclaration::TfPortDeclaration(_) => None,
            });
            let local_types = function_local_types_from_block_item_iter(
                block_items,
                syntax_tree,
                const_env,
                type_aliases,
            )
            .ok_or_else(|| {
                AnalyzerError::Unsupported("unsupported function local data type".to_string())
            })?;
            (&body.nodes.5, params, local_types)
        }
    };
    let mut assignment_targets = local_types.into_keys().collect::<HashSet<_>>();
    assignment_targets.extend(params.into_iter().map(|param| param.name));
    // Assigning the function's own name sets its return value.
    assignment_targets.extend(function_name);
    for node in RefNode::FunctionDeclaration(declaration) {
        let RefNode::BlockingAssignment(assignment) = node else {
            continue;
        };
        let lhs = match assignment {
            sv_parser::BlockingAssignment::Variable(assignment) => &assignment.nodes.0,
            sv_parser::BlockingAssignment::OperatorAssignment(assignment) => &assignment.nodes.0,
            _ => continue,
        };
        let Some(LValue::Ident(name)) =
            variable_lvalue_from_node(lhs, syntax_tree, packed_dimensions)
        else {
            continue;
        };
        if !assignment_targets.contains(&name) {
            return Err(AnalyzerError::Unsupported(format!(
                "function assignment target outside local scope `{name}`"
            )));
        }
    }
    for statement in statements {
        let sv_parser::FunctionStatementOrNull::Statement(statement) = statement else {
            continue;
        };
        validate_function_statement(&statement.nodes.0, syntax_tree, packed_dimensions)?;
    }
    Ok(())
}

fn validate_function_statement_or_null(
    statement: &sv_parser::StatementOrNull,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Result<(), AnalyzerError> {
    let sv_parser::StatementOrNull::Statement(statement) = statement else {
        return Ok(());
    };
    validate_function_statement(statement, syntax_tree, packed_dimensions)
}

fn validate_function_statement(
    statement: &sv_parser::Statement,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Result<(), AnalyzerError> {
    match &statement.nodes.2 {
        sv_parser::StatementItem::JumpStatement(statement)
            if matches!(&**statement, sv_parser::JumpStatement::Return(_)) =>
        {
            let sv_parser::JumpStatement::Return(statement) = &**statement else {
                unreachable!();
            };
            let expr = statement.nodes.1.as_ref().ok_or_else(|| {
                AnalyzerError::Unsupported("expressionless function return".to_string())
            })?;
            expr_from_expression_with_types(expr, syntax_tree, packed_dimensions).ok_or_else(
                || AnalyzerError::Unsupported("unsupported function return expression".to_string()),
            )?;
            Ok(())
        }
        sv_parser::StatementItem::BlockingAssignment(assignment) => {
            let rhs = match &assignment.0 {
                sv_parser::BlockingAssignment::Variable(assignment) => &assignment.nodes.3,
                sv_parser::BlockingAssignment::OperatorAssignment(assignment) => {
                    &assignment.nodes.2
                }
                _ => {
                    return Err(AnalyzerError::Unsupported(
                        "unsupported function assignment".to_string(),
                    ));
                }
            };
            expr_from_expression_with_types(rhs, syntax_tree, packed_dimensions).ok_or_else(
                || {
                    AnalyzerError::Unsupported(
                        "unsupported function assignment expression".to_string(),
                    )
                },
            )?;
            Ok(())
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            for statement in &block.nodes.3 {
                validate_function_statement_or_null(statement, syntax_tree, packed_dimensions)?;
            }
            Ok(())
        }
        sv_parser::StatementItem::ConditionalStatement(statement) => {
            expr_from_cond_predicate(&statement.nodes.2.nodes.1, syntax_tree, packed_dimensions)
                .ok_or_else(|| {
                    AnalyzerError::Unsupported(
                        "unsupported function conditional predicate".to_string(),
                    )
                })?;
            validate_function_statement_or_null(
                &statement.nodes.3,
                syntax_tree,
                packed_dimensions,
            )?;
            for (_, _, predicate, branch) in &statement.nodes.4 {
                expr_from_cond_predicate(&predicate.nodes.1, syntax_tree, packed_dimensions)
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported(
                            "unsupported function conditional predicate".to_string(),
                        )
                    })?;
                validate_function_statement_or_null(branch, syntax_tree, packed_dimensions)?;
            }
            if let Some((_, branch)) = &statement.nodes.5 {
                validate_function_statement_or_null(branch, syntax_tree, packed_dimensions)?;
            }
            Ok(())
        }
        sv_parser::StatementItem::CaseStatement(statement) => {
            let sv_parser::CaseStatement::Normal(statement) = &**statement else {
                return Err(AnalyzerError::Unsupported(
                    "unsupported statement inside function".to_string(),
                ));
            };
            expr_from_expression_with_types(
                &statement.nodes.2.nodes.1.nodes.0,
                syntax_tree,
                packed_dimensions,
            )
            .ok_or_else(|| {
                AnalyzerError::Unsupported("unsupported function case selector".to_string())
            })?;
            for item in std::iter::once(&statement.nodes.3).chain(statement.nodes.4.iter()) {
                let branch = match item {
                    sv_parser::CaseItem::NonDefault(item) => {
                        for expr in item.nodes.0.contents() {
                            expr_from_expression_with_types(
                                &expr.nodes.0,
                                syntax_tree,
                                packed_dimensions,
                            )
                            .ok_or_else(|| {
                                AnalyzerError::Unsupported(
                                    "unsupported function case item expression".to_string(),
                                )
                            })?;
                        }
                        &item.nodes.2
                    }
                    sv_parser::CaseItem::Default(item) => &item.nodes.2,
                };
                validate_function_statement_or_null(branch, syntax_tree, packed_dimensions)?;
            }
            Ok(())
        }
        _ => Err(AnalyzerError::Unsupported(
            "unsupported statement inside function".to_string(),
        )),
    }
}

pub(super) fn function_from_declaration(
    declaration: &sv_parser::FunctionDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    packed_dimensions: &PackedDimensions,
) -> Option<Function> {
    match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => {
            let name = identifier_text(RefNode::FunctionIdentifier(&body.nodes.2), syntax_tree)?;
            let params = body
                .nodes
                .3
                .nodes
                .1
                .as_ref()
                .map(|ports| tf_params(ports, syntax_tree, const_env, type_aliases))
                .unwrap_or_default();
            let mut local_types = function_local_types_from_block_items(
                &body.nodes.5,
                syntax_tree,
                const_env,
                type_aliases,
            )?;
            let mut function_packed_dimensions = packed_dimensions.clone();
            function_packed_dimensions.extend(params.iter().map(|param| {
                (
                    param.name.clone(),
                    VariableDimensions {
                        packed: param.packed_dimensions.clone(),
                        unpacked: Vec::new(),
                        signed: param.signed,
                        is_2state: param.is_2state,
                        members: Vec::new(),
                    },
                )
            }));
            function_packed_dimensions.extend(function_local_packed_dimensions_from_block_items(
                &body.nodes.5,
                syntax_tree,
                const_env,
                type_aliases,
            )?);
            let local_names = local_types.keys().cloned().collect::<HashSet<_>>();
            insert_function_param_types(&params, &mut local_types);
            let output_names = params
                .iter()
                .filter(|param| param.direction.is_written())
                .map(|param| param.name.clone())
                .collect::<Vec<_>>();
            let (expr, outputs) = function_body_expr(
                &body.nodes.6,
                syntax_tree,
                &function_packed_dimensions,
                &local_types,
                &local_names,
                Some(&name),
                &output_names,
            )?;
            let return_type =
                function_return_type(&body.nodes.0, syntax_tree, const_env, type_aliases);
            let return_first_packed_dimension_width = function_return_first_packed_dimension_width(
                &body.nodes.0,
                syntax_tree,
                const_env,
                type_aliases,
                return_type,
            );
            let return_is_2state =
                function_return_is_2state(&body.nodes.0, syntax_tree, type_aliases);
            Some(Function {
                name,
                params,
                body: expr,
                outputs,
                return_width: return_type.map(|r#type| r#type.width),
                return_first_packed_dimension_width,
                return_signed: return_type.is_some_and(|r#type| r#type.signed),
                return_is_2state,
            })
        }
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => {
            let name = identifier_text(RefNode::FunctionIdentifier(&body.nodes.2), syntax_tree)?;
            let params = tf_item_params(&body.nodes.4, syntax_tree, const_env, type_aliases);
            let block_items = body
                .nodes
                .4
                .iter()
                .filter_map(|item| match item {
                    sv_parser::TfItemDeclaration::BlockItemDeclaration(item) => Some(&**item),
                    sv_parser::TfItemDeclaration::TfPortDeclaration(_) => None,
                })
                .collect::<Vec<_>>();
            let mut local_types = function_local_types_from_block_item_iter(
                block_items.iter().copied(),
                syntax_tree,
                const_env,
                type_aliases,
            )?;
            let mut function_packed_dimensions = packed_dimensions.clone();
            function_packed_dimensions.extend(params.iter().map(|param| {
                (
                    param.name.clone(),
                    VariableDimensions {
                        packed: param.packed_dimensions.clone(),
                        unpacked: Vec::new(),
                        signed: param.signed,
                        is_2state: param.is_2state,
                        members: Vec::new(),
                    },
                )
            }));
            function_packed_dimensions.extend(
                function_local_packed_dimensions_from_block_item_iter(
                    block_items.iter().copied(),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )?,
            );
            let local_names = local_types.keys().cloned().collect::<HashSet<_>>();
            insert_function_param_types(&params, &mut local_types);
            let output_names = params
                .iter()
                .filter(|param| param.direction.is_written())
                .map(|param| param.name.clone())
                .collect::<Vec<_>>();
            let (expr, outputs) = function_body_expr(
                &body.nodes.5,
                syntax_tree,
                &function_packed_dimensions,
                &local_types,
                &local_names,
                Some(&name),
                &output_names,
            )?;
            let return_type =
                function_return_type(&body.nodes.0, syntax_tree, const_env, type_aliases);
            let return_first_packed_dimension_width = function_return_first_packed_dimension_width(
                &body.nodes.0,
                syntax_tree,
                const_env,
                type_aliases,
                return_type,
            );
            let return_is_2state =
                function_return_is_2state(&body.nodes.0, syntax_tree, type_aliases);
            Some(Function {
                name,
                params,
                body: expr,
                outputs,
                return_width: return_type.map(|r#type| r#type.width),
                return_first_packed_dimension_width,
                return_signed: return_type.is_some_and(|r#type| r#type.signed),
                return_is_2state,
            })
        }
    }
}

fn insert_function_param_types(
    params: &[FunctionParam],
    local_types: &mut HashMap<String, FunctionLocalType>,
) {
    for param in params {
        if let Some(width) = param.width {
            local_types.insert(
                param.name.clone(),
                FunctionLocalType {
                    width,
                    signed: param.signed,
                    is_2state: param.is_2state,
                },
            );
        }
    }
}

fn function_local_types_from_block_items(
    items: &[sv_parser::BlockItemDeclaration],
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<HashMap<String, FunctionLocalType>> {
    function_local_types_from_block_item_iter(items.iter(), syntax_tree, const_env, type_aliases)
}

pub(super) fn function_local_packed_dimensions_from_block_items(
    items: &[sv_parser::BlockItemDeclaration],
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<VariablePackedDimensions> {
    function_local_packed_dimensions_from_block_item_iter(
        items.iter(),
        syntax_tree,
        const_env,
        type_aliases,
    )
}

pub(super) fn function_local_packed_dimensions_from_block_item_iter<'a>(
    items: impl IntoIterator<Item = &'a sv_parser::BlockItemDeclaration>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<VariablePackedDimensions> {
    let mut dimensions = HashMap::default();
    for item in items {
        let sv_parser::BlockItemDeclaration::Data(item) = item else {
            continue;
        };
        let signals = signals_from_data_declaration(
            &item.nodes.1,
            syntax_tree,
            type_aliases,
            const_env,
            None,
        )
        .ok()?;
        dimensions.extend(signals.into_iter().map(|signal| {
            (
                signal.name().to_string(),
                VariableDimensions {
                    packed: function_packed_dimension_widths(signal.r#type().packed_ranges()),
                    unpacked: unpacked_dimension_widths(signal.r#type().unpacked_ranges()),
                    signed: signal.r#type().is_signed(),
                    is_2state: signal.r#type().kind() == TypeKind::Bit,
                    members: signal.r#type().members.clone(),
                },
            )
        }));
    }
    Some(dimensions)
}

fn function_local_types_from_block_item_iter<'a>(
    items: impl IntoIterator<Item = &'a sv_parser::BlockItemDeclaration>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<HashMap<String, FunctionLocalType>> {
    let mut local_types = HashMap::default();
    for item in items {
        let sv_parser::BlockItemDeclaration::Data(item) = item else {
            continue;
        };
        let signals = signals_from_data_declaration(
            &item.nodes.1,
            syntax_tree,
            type_aliases,
            const_env,
            None,
        )
        .ok()?;
        for signal in signals {
            let r#type = signal.r#type();
            if !r#type.unpacked_ranges().is_empty() {
                return None;
            }
            let width = if r#type.packed_ranges().is_empty() {
                1
            } else {
                r#type
                    .packed_ranges()
                    .iter()
                    .try_fold(1usize, |acc, range| {
                        let left = eval_ast_const_expr(range.left(), const_env)?;
                        let right = eval_ast_const_expr(range.right(), const_env)?;
                        acc.checked_mul(left.abs_diff(right) as usize + 1)
                    })?
            };
            local_types.insert(
                signal.name().to_string(),
                FunctionLocalType {
                    width,
                    signed: r#type.is_signed(),
                    is_2state: r#type.kind() == TypeKind::Bit,
                },
            );
        }
    }
    Some(local_types)
}

pub(super) fn function_return_is_2state(
    node: &sv_parser::FunctionDataTypeOrImplicit,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
) -> bool {
    let r#type = match node {
        sv_parser::FunctionDataTypeOrImplicit::DataTypeOrVoid(data_type) => match &**data_type {
            sv_parser::DataTypeOrVoid::DataType(data_type) => {
                type_from_ref_node(RefNode::DataType(data_type), syntax_tree)
                    .or_else(|| type_alias_from_data_type(data_type, syntax_tree, type_aliases))
            }
            sv_parser::DataTypeOrVoid::Void(_) => None,
        },
        sv_parser::FunctionDataTypeOrImplicit::ImplicitDataType(data_type) => {
            type_from_ref_node(RefNode::ImplicitDataType(data_type), syntax_tree).or_else(|| {
                type_alias_from_ref_node(
                    RefNode::ImplicitDataType(data_type),
                    syntax_tree,
                    type_aliases,
                )
            })
        }
    };
    r#type.is_some_and(|r#type| r#type.kind() == TypeKind::Bit)
}

pub(super) fn function_return_type(
    node: &sv_parser::FunctionDataTypeOrImplicit,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    match node {
        sv_parser::FunctionDataTypeOrImplicit::DataTypeOrVoid(data_type) => match &**data_type {
            sv_parser::DataTypeOrVoid::DataType(data_type) => {
                value_type_from_data_type(data_type, syntax_tree, const_env, type_aliases)
            }
            sv_parser::DataTypeOrVoid::Void(_) => None,
        },
        sv_parser::FunctionDataTypeOrImplicit::ImplicitDataType(data_type) => {
            value_type_from_ref_node(
                RefNode::ImplicitDataType(data_type),
                syntax_tree,
                const_env,
                type_aliases,
            )
        }
    }
}

pub(super) fn function_return_first_packed_dimension_width(
    node: &sv_parser::FunctionDataTypeOrImplicit,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    return_type: Option<ExprType>,
) -> Option<usize> {
    let r#type = function_type_from_ref_node(
        RefNode::FunctionDataTypeOrImplicit(node),
        syntax_tree,
        const_env,
        type_aliases,
    );
    let Some(first) = r#type
        .as_ref()
        .and_then(|r#type| r#type.packed_ranges().first())
    else {
        return return_type.map(|r#type| r#type.width);
    };
    let left = eval_ast_const_expr(first.left(), const_env)?;
    let right = eval_ast_const_expr(first.right(), const_env)?;
    usize::try_from(left.abs_diff(right))
        .ok()
        .and_then(|width| width.checked_add(1))
}

pub(super) fn tf_params(
    list: &sv_parser::TfPortList,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Vec<FunctionParam> {
    let mut params = Vec::new();
    let mut previous_type = None;
    let mut previous_is_2state = false;
    let mut previous_packed_dimensions = Vec::new();
    let mut direction = ParamDirection::Input;
    for port in list.nodes.0.contents() {
        // An omitted direction repeats the previous argument's.
        if let Some(declared) = port.nodes.1.as_ref().and_then(ParamDirection::from_tf_port) {
            direction = declared;
        }
        let type_node = RefNode::DataTypeOrImplicit(&port.nodes.3);
        let inferred_type = value_type_from_data_type_or_implicit(
            &port.nodes.3,
            syntax_tree,
            const_env,
            type_aliases,
        );
        let inferred_is_2state = type_from_ref_node(type_node.clone(), syntax_tree)
            .or_else(|| type_alias_from_ref_node(type_node.clone(), syntax_tree, type_aliases))
            .is_some_and(|r#type| r#type.kind() == TypeKind::Bit);
        let inferred_packed_dimensions =
            function_param_packed_dimensions(&port.nodes.3, syntax_tree, const_env, type_aliases);
        let omitted_type = matches!(
            port.nodes.3,
            sv_parser::DataTypeOrImplicit::ImplicitDataType(_)
        ) && is_signed_from_ref_node(type_node.clone()).is_none()
            && inferred_packed_dimensions.is_empty();
        let (name, r#type, is_2state, packed_dimensions) =
            if let Some((identifier, _, _)) = port.nodes.4.as_ref() {
                let Some(name) = identifier_text(RefNode::PortIdentifier(identifier), syntax_tree)
                else {
                    continue;
                };
                let r#type = if port.nodes.1.is_none() && omitted_type {
                    previous_type.or(inferred_type)
                } else {
                    inferred_type
                };
                let is_2state = if port.nodes.1.is_none() && omitted_type {
                    previous_is_2state
                } else {
                    inferred_is_2state
                };
                let packed_dimensions = if port.nodes.1.is_none() && omitted_type {
                    previous_packed_dimensions.clone()
                } else {
                    inferred_packed_dimensions
                };
                (name, r#type, is_2state, packed_dimensions)
            } else {
                // An identifier following a comma is syntactically ambiguous with a
                // user-defined type. sv-parser represents the shorthand `a, b` as a
                // type-only item, so reinterpret an unknown type name as the next
                // parameter and inherit the preceding item's type.
                if type_alias_from_data_type_or_implicit(&port.nodes.3, syntax_tree, type_aliases)
                    .is_some()
                {
                    continue;
                }
                let Some(name) = identifier_text(type_node, syntax_tree) else {
                    continue;
                };
                let r#type = if port.nodes.1.is_none() {
                    previous_type
                } else {
                    Some(ExprType {
                        width: 1,
                        signed: false,
                    })
                };
                let is_2state = port.nodes.1.is_none() && previous_is_2state;
                let packed_dimensions = if port.nodes.1.is_none() {
                    previous_packed_dimensions.clone()
                } else {
                    inferred_packed_dimensions
                };
                (name, r#type, is_2state, packed_dimensions)
            };
        previous_type = r#type;
        previous_is_2state = is_2state;
        previous_packed_dimensions = packed_dimensions.clone();
        params.push(FunctionParam {
            direction,
            name,
            width: r#type.map(|r#type| r#type.width),
            signed: r#type.is_some_and(|r#type| r#type.signed),
            is_2state,
            packed_dimensions,
        });
    }
    params
}

pub(super) fn tf_item_params(
    items: &[sv_parser::TfItemDeclaration],
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Vec<FunctionParam> {
    let mut params = Vec::new();
    for item in items {
        let sv_parser::TfItemDeclaration::TfPortDeclaration(declaration) = item else {
            continue;
        };
        let r#type = value_type_from_data_type_or_implicit(
            &declaration.nodes.3,
            syntax_tree,
            const_env,
            type_aliases,
        );
        let is_2state = type_from_ref_node(
            RefNode::DataTypeOrImplicit(&declaration.nodes.3),
            syntax_tree,
        )
        .or_else(|| {
            type_alias_from_ref_node(
                RefNode::DataTypeOrImplicit(&declaration.nodes.3),
                syntax_tree,
                type_aliases,
            )
        })
        .is_some_and(|r#type| r#type.kind() == TypeKind::Bit);
        let packed_dimensions = function_param_packed_dimensions(
            &declaration.nodes.3,
            syntax_tree,
            const_env,
            type_aliases,
        );
        for (identifier, _, _) in declaration.nodes.4.nodes.0.contents() {
            let Some(name) = identifier_text(RefNode::PortIdentifier(identifier), syntax_tree)
            else {
                continue;
            };
            params.push(FunctionParam {
                direction: ParamDirection::from_tf_port(&declaration.nodes.1)
                    .unwrap_or(ParamDirection::Input),
                name,
                width: r#type.map(|r#type| r#type.width),
                signed: r#type.is_some_and(|r#type| r#type.signed),
                is_2state,
                packed_dimensions: packed_dimensions.clone(),
            });
        }
    }
    params
}

fn value_type_from_data_type_or_implicit(
    node: &sv_parser::DataTypeOrImplicit,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    value_type_from_ref_node(
        RefNode::DataTypeOrImplicit(node),
        syntax_tree,
        const_env,
        type_aliases,
    )
}

fn value_type_from_data_type(
    node: &sv_parser::DataType,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    value_type_from_ref_node(
        RefNode::DataType(node),
        syntax_tree,
        const_env,
        type_aliases,
    )
}

fn value_type_from_ref_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    let r#type = function_type_from_ref_node(node, syntax_tree, const_env, type_aliases)?;
    expr_type_from_type(&r#type, const_env)
}

pub(super) fn function_type_from_ref_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    let node = match node {
        RefNode::DataTypeOrImplicit(node) => match node {
            sv_parser::DataTypeOrImplicit::DataType(node) => RefNode::DataType(node),
            sv_parser::DataTypeOrImplicit::ImplicitDataType(node) => {
                RefNode::ImplicitDataType(node)
            }
        },
        RefNode::FunctionDataTypeOrImplicit(node) => match node {
            sv_parser::FunctionDataTypeOrImplicit::DataTypeOrVoid(node) => match &**node {
                sv_parser::DataTypeOrVoid::DataType(node) => RefNode::DataType(node),
                sv_parser::DataTypeOrVoid::Void(_) => return None,
            },
            sv_parser::FunctionDataTypeOrImplicit::ImplicitDataType(node) => {
                RefNode::ImplicitDataType(node)
            }
        },
        node => node,
    };
    // Only the declared type can supply an alias. A typedef used in a range
    // bound's cast must not cause the built-in type's dimensions to be added twice.
    let r#type = match &node {
        RefNode::DataType(data_type) => {
            let Some(alias) = type_alias_from_data_type(data_type, syntax_tree, type_aliases)
            else {
                return type_from_ref_node_with_env(node, syntax_tree, const_env, type_aliases);
            };
            alias
        }
        RefNode::ImplicitDataType(_) => Type::implicit(),
        _ => return None,
    };
    Some(type_with_fallback_ranges_with_env(
        r#type,
        node,
        syntax_tree,
        const_env,
        type_aliases,
    ))
}

pub(super) fn integer_atom_expr_type(node: RefNode<'_>) -> Option<ExprType> {
    let atom = unwrap_node!(node.clone(), IntegerAtomType)?;
    let RefNode::IntegerAtomType(atom) = atom else {
        return None;
    };
    let (width, default_signed) = match atom {
        sv_parser::IntegerAtomType::Byte(_) => (8, true),
        sv_parser::IntegerAtomType::Shortint(_) => (16, true),
        sv_parser::IntegerAtomType::Int(_) | sv_parser::IntegerAtomType::Integer(_) => (32, true),
        sv_parser::IntegerAtomType::Longint(_) => (64, true),
        sv_parser::IntegerAtomType::Time(_) => (64, false),
    };
    Some(ExprType {
        width,
        signed: is_signed_from_ref_node(node).unwrap_or(default_signed),
    })
}

fn function_body_expr(
    statements: &[sv_parser::FunctionStatementOrNull],
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    local_types: &HashMap<String, FunctionLocalType>,
    local_names: &HashSet<String>,
    return_variable: Option<&str>,
    output_names: &[String],
) -> Option<(Expr, Vec<(String, Expr)>)> {
    let mut locals = local_types
        .iter()
        .map(|(name, r#type)| {
            let initial = if local_names.contains(name) {
                coerce_function_local_assignment(Expr::Literal("'x".to_string()), *r#type)
            } else {
                Expr::Ident(name.clone())
            };
            (name.clone(), initial)
        })
        .collect::<HashMap<_, _>>();
    let statements = statements
        .iter()
        .filter_map(|statement| {
            let sv_parser::FunctionStatementOrNull::Statement(statement) = statement else {
                return None;
            };
            Some(&statement.nodes.0)
        })
        .collect::<Vec<_>>();
    if let Some(name) = return_variable {
        // The value of an unassigned return variable is unknown.
        locals.insert(name.to_string(), Expr::Literal("'x".to_string()));
    }
    let returned = function_expr_from_sequence(
        &statements,
        &mut locals,
        syntax_tree,
        packed_dimensions,
        local_types,
    );
    // The values the `output` / `inout` arguments hold when the body ends.
    let outputs = output_names
        .iter()
        .map(|name| Some((name.clone(), locals.get(name).cloned()?)))
        .collect::<Option<Vec<_>>>()?;
    if returned.is_some() && !outputs.is_empty() {
        // An early `return` would need the arguments' values at that point.
        return None;
    }
    // Falling off the end returns whatever was assigned to the function name;
    // a function with only `output` arguments returns nothing.
    let value = returned
        .or_else(|| return_variable.and_then(|name| locals.get(name).cloned()))
        .or_else(|| (!outputs.is_empty()).then(|| Expr::Literal("'x".to_string())))?;
    Some((value, outputs))
}

/// Lower a returning branch with the remaining statements as its continuation.
/// Each branch owns its local state, so assignments after a return cannot alter
/// that path's result or leak into a sibling path.
fn function_expr_from_sequence(
    statements: &[&sv_parser::Statement],
    locals: &mut HashMap<String, Expr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    local_types: &HashMap<String, FunctionLocalType>,
) -> Option<Expr> {
    for (index, statement) in statements.iter().enumerate() {
        let rest = &statements[index + 1..];
        if let sv_parser::StatementItem::SeqBlock(block) = &statement.nodes.2 {
            let sequence = block
                .nodes
                .3
                .iter()
                .filter_map(function_statement_ref)
                .chain(rest.iter().copied())
                .collect::<Vec<_>>();
            return function_expr_from_sequence(
                &sequence,
                locals,
                syntax_tree,
                packed_dimensions,
                local_types,
            );
        }
        if let sv_parser::StatementItem::ConditionalStatement(conditional) = &statement.nodes.2
            && RefNode::ConditionalStatement(conditional)
                .into_iter()
                .any(|node| matches!(node, RefNode::JumpStatement(_)))
        {
            let lower_branch = |branch: Option<&sv_parser::StatementOrNull>| {
                let sequence = branch
                    .and_then(function_statement_ref)
                    .into_iter()
                    .chain(rest.iter().copied())
                    .collect::<Vec<_>>();
                function_expr_from_sequence(
                    &sequence,
                    &mut locals.clone(),
                    syntax_tree,
                    packed_dimensions,
                    local_types,
                )
            };
            let mut result = lower_branch(conditional.nodes.5.as_ref().map(|(_, branch)| branch))?;
            let branches = std::iter::once((&conditional.nodes.2.nodes.1, &conditional.nodes.3))
                .chain(
                    conditional
                        .nodes
                        .4
                        .iter()
                        .map(|(_, _, predicate, branch)| (&predicate.nodes.1, branch)),
                )
                .collect::<Vec<_>>();
            for (predicate, branch) in branches.into_iter().rev() {
                let condition =
                    expr_from_cond_predicate(predicate, syntax_tree, packed_dimensions)?;
                result = Expr::Mux {
                    condition: Box::new(procedural_truth_condition(substitute_expr_idents(
                        condition, locals,
                    ))),
                    then_expr: Box::new(lower_branch(Some(branch))?),
                    else_expr: Box::new(result),
                };
            }
            return Some(result);
        }
        if let sv_parser::StatementItem::CaseStatement(case) = &statement.nodes.2
            && RefNode::CaseStatement(case)
                .into_iter()
                .any(|node| matches!(node, RefNode::JumpStatement(_)))
        {
            let sv_parser::CaseStatement::Normal(case) = &**case else {
                return None;
            };
            let selector = substitute_expr_idents(
                expr_from_expression_with_types(
                    &case.nodes.2.nodes.1.nodes.0,
                    syntax_tree,
                    packed_dimensions,
                )?,
                locals,
            );
            let lower_branch = |branch: Option<&sv_parser::StatementOrNull>| {
                let sequence = branch
                    .and_then(function_statement_ref)
                    .into_iter()
                    .chain(rest.iter().copied())
                    .collect::<Vec<_>>();
                function_expr_from_sequence(
                    &sequence,
                    &mut locals.clone(),
                    syntax_tree,
                    packed_dimensions,
                    local_types,
                )
            };
            let mut default = None;
            let mut branches = Vec::new();
            for item in std::iter::once(&case.nodes.3).chain(case.nodes.4.iter()) {
                match item {
                    sv_parser::CaseItem::NonDefault(item) => {
                        let condition = item
                            .nodes
                            .0
                            .contents()
                            .into_iter()
                            .map(|expr| {
                                let label = expr_from_expression_with_types(
                                    &expr.nodes.0,
                                    syntax_tree,
                                    packed_dimensions,
                                )?;
                                Some(case_item_condition(
                                    selector.clone(),
                                    substitute_expr_idents(label, locals),
                                    case_keyword_is_wildcard(&case.nodes.1),
                                ))
                            })
                            .collect::<Option<Vec<_>>>()?
                            .into_iter()
                            .reduce(|left, right| Expr::Binary {
                                left: Box::new(left),
                                op: BinaryOp::LogicOr,
                                right: Box::new(right),
                            })?;
                        branches.push((condition, &item.nodes.2));
                    }
                    sv_parser::CaseItem::Default(item) => default = Some(&item.nodes.2),
                }
            }
            // A missing default follows the continuation with the incoming
            // locals. Earlier matching labels retain priority over later ones.
            let mut result = lower_branch(default)?;
            for (condition, branch) in branches.into_iter().rev() {
                result = Expr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(lower_branch(Some(branch))?),
                    else_expr: Box::new(result),
                };
            }
            return Some(result);
        }
        if let Some(expr) = function_expr_from_statement(
            statement,
            locals,
            syntax_tree,
            packed_dimensions,
            local_types,
        ) {
            return Some(expr);
        }
    }
    None
}

fn function_statement_ref(statement: &sv_parser::StatementOrNull) -> Option<&sv_parser::Statement> {
    match statement {
        sv_parser::StatementOrNull::Statement(statement) => Some(statement),
        _ => None,
    }
}

fn function_expr_from_statement_or_null_stmt(
    statement: &sv_parser::StatementOrNull,
    locals: &mut HashMap<String, Expr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    local_types: &HashMap<String, FunctionLocalType>,
) -> Option<Expr> {
    let sv_parser::StatementOrNull::Statement(statement) = statement else {
        return None;
    };
    function_expr_from_statement(
        statement,
        locals,
        syntax_tree,
        packed_dimensions,
        local_types,
    )
}

fn function_expr_from_statement(
    statement: &sv_parser::Statement,
    locals: &mut HashMap<String, Expr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    local_types: &HashMap<String, FunctionLocalType>,
) -> Option<Expr> {
    match &statement.nodes.2 {
        sv_parser::StatementItem::JumpStatement(statement) => {
            let sv_parser::JumpStatement::Return(statement) = &**statement else {
                return None;
            };
            let expr = statement.nodes.1.as_ref()?;
            expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)
                .map(|expr| substitute_expr_idents(expr, locals))
        }
        sv_parser::StatementItem::BlockingAssignment(assignment) => {
            let (lhs, rhs) = match &assignment.0 {
                sv_parser::BlockingAssignment::Variable(assignment) => (
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions),
                    expr_from_expression_with_types(
                        &assignment.nodes.3,
                        syntax_tree,
                        packed_dimensions,
                    ),
                ),
                sv_parser::BlockingAssignment::OperatorAssignment(assignment) => {
                    let lhs = variable_lvalue_from_node(
                        &assignment.nodes.0,
                        syntax_tree,
                        packed_dimensions,
                    );
                    let rhs = expr_from_expression_with_types(
                        &assignment.nodes.2,
                        syntax_tree,
                        packed_dimensions,
                    );
                    let op = syntax_tree.get_str(&assignment.nodes.1.nodes.0.nodes.0);
                    let rhs = match (&lhs, rhs, op) {
                        (_, Some(rhs), Some("=")) => Some(rhs),
                        (Some(lhs), Some(rhs), Some(op)) => {
                            assignment_op_expr(lhs, op, rhs, packed_dimensions)
                        }
                        _ => None,
                    };
                    (lhs, rhs)
                }
                _ => (None, None),
            };
            let Some(LValue::Ident(name)) = lhs else {
                return None;
            };
            if let Some(rhs) = rhs {
                let rhs = substitute_expr_idents(rhs, locals);
                let rhs = local_types
                    .get(&name)
                    .copied()
                    .map(|r#type| coerce_function_local_assignment(rhs.clone(), r#type))
                    .unwrap_or(rhs);
                locals.insert(name, rhs);
            }
            None
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            let mut block_locals = locals.clone();
            for statement in &block.nodes.3 {
                if let Some(expr) = function_expr_from_statement_or_null_stmt(
                    statement,
                    &mut block_locals,
                    syntax_tree,
                    packed_dimensions,
                    local_types,
                ) {
                    return Some(expr);
                }
            }
            *locals = block_locals;
            None
        }
        sv_parser::StatementItem::ConditionalStatement(statement) => {
            function_expr_from_conditional_statement(
                statement,
                locals,
                syntax_tree,
                packed_dimensions,
                local_types,
            )
        }
        sv_parser::StatementItem::CaseStatement(statement) => function_expr_from_case_statement(
            statement,
            locals,
            syntax_tree,
            packed_dimensions,
            local_types,
        ),
        _ => None,
    }
}

fn function_expr_from_conditional_statement(
    statement: &sv_parser::ConditionalStatement,
    locals: &mut HashMap<String, Expr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    local_types: &HashMap<String, FunctionLocalType>,
) -> Option<Expr> {
    let mut branches = Vec::new();
    let if_condition =
        expr_from_cond_predicate(&statement.nodes.2.nodes.1, syntax_tree, packed_dimensions)?;
    let mut then_locals = locals.clone();
    let then_expr = function_expr_from_statement_or_null_stmt(
        &statement.nodes.3,
        &mut then_locals,
        syntax_tree,
        packed_dimensions,
        local_types,
    );
    branches.push((
        procedural_truth_condition(substitute_expr_idents(if_condition, locals)),
        then_expr,
        then_locals,
    ));

    for (_, _, predicate, branch) in &statement.nodes.4 {
        let condition =
            expr_from_cond_predicate(&predicate.nodes.1, syntax_tree, packed_dimensions)?;
        let mut branch_locals = locals.clone();
        let branch_expr = function_expr_from_statement_or_null_stmt(
            branch,
            &mut branch_locals,
            syntax_tree,
            packed_dimensions,
            local_types,
        );
        branches.push((
            procedural_truth_condition(substitute_expr_idents(condition, locals)),
            branch_expr,
            branch_locals,
        ));
    }

    let (else_expr, else_locals) = if let Some((_, branch)) = &statement.nodes.5 {
        let mut branch_locals = locals.clone();
        (
            function_expr_from_statement_or_null_stmt(
                branch,
                &mut branch_locals,
                syntax_tree,
                packed_dimensions,
                local_types,
            ),
            branch_locals,
        )
    } else {
        (None, locals.clone())
    };

    if branches.iter().all(|(_, expr, _)| expr.is_some()) && else_expr.is_some() {
        let mut result = else_expr?;
        for (condition, branch_expr, _) in branches.into_iter().rev() {
            result = Expr::Mux {
                condition: Box::new(condition),
                then_expr: Box::new(branch_expr?),
                else_expr: Box::new(result),
            };
        }
        return Some(result);
    }

    if branches.iter().all(|(_, expr, _)| expr.is_none()) && else_expr.is_none() {
        let mut merged = locals.clone();
        let mut names = locals.keys().cloned().collect::<HashSet<_>>();
        names.extend(else_locals.keys().cloned());
        names.extend(
            branches
                .iter()
                .flat_map(|(_, _, branch_locals)| branch_locals.keys().cloned()),
        );
        for name in names {
            let mut value = else_locals
                .get(&name)
                .or_else(|| locals.get(&name))
                .cloned()
                .unwrap_or_else(|| Expr::Ident(name.clone()));
            for (condition, _, branch_locals) in branches.iter().rev() {
                let branch_value = branch_locals
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| value.clone());
                if branch_value != value {
                    value = Expr::Mux {
                        condition: Box::new(condition.clone()),
                        then_expr: Box::new(branch_value),
                        else_expr: Box::new(value),
                    };
                }
            }
            merged.insert(name, value);
        }
        *locals = merged;
        return None;
    }

    Some(Expr::Call {
        name: "$unsupported_mixed_function_conditional".to_string(),
        args: Vec::new(),
    })
}

fn coerce_function_local_assignment(expr: Expr, r#type: FunctionLocalType) -> Expr {
    let expr = Expr::Resize {
        expr: Box::new(expr),
        width: r#type.width,
        signed: r#type.signed,
    };
    if r#type.is_2state {
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(expr),
        }
    } else {
        expr
    }
}

pub(super) fn procedural_truth_condition(condition: Expr) -> Expr {
    Expr::Unary {
        op: UnaryOp::RedOr,
        expr: Box::new(Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(condition),
        }),
    }
}

fn function_expr_from_case_statement(
    statement: &sv_parser::CaseStatement,
    locals: &mut HashMap<String, Expr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    local_types: &HashMap<String, FunctionLocalType>,
) -> Option<Expr> {
    let sv_parser::CaseStatement::Normal(statement) = statement else {
        return None;
    };
    let case_expr = expr_from_expression_with_types(
        &statement.nodes.2.nodes.1.nodes.0,
        syntax_tree,
        packed_dimensions,
    )?;
    let case_expr = substitute_expr_idents(case_expr, locals);
    let mut default_branch = (None, locals.clone());
    let mut branches = Vec::new();
    for item in std::iter::once(&statement.nodes.3).chain(statement.nodes.4.iter()) {
        match item {
            sv_parser::CaseItem::NonDefault(item) => {
                let mut branch_locals = locals.clone();
                let branch_expr = function_expr_from_statement_or_null_stmt(
                    &item.nodes.2,
                    &mut branch_locals,
                    syntax_tree,
                    packed_dimensions,
                    local_types,
                );
                let conditions = item
                    .nodes
                    .0
                    .contents()
                    .into_iter()
                    .filter_map(|expr| {
                        expr_from_expression_with_types(
                            &expr.nodes.0,
                            syntax_tree,
                            packed_dimensions,
                        )
                    })
                    .map(|expr| substitute_expr_idents(expr, locals))
                    .map(|expr| {
                        case_item_condition(
                            case_expr.clone(),
                            expr,
                            case_keyword_is_wildcard(&statement.nodes.1),
                        )
                    })
                    .collect::<Vec<_>>();
                let condition = conditions.into_iter().reduce(|left, right| Expr::Binary {
                    left: Box::new(left),
                    op: BinaryOp::LogicOr,
                    right: Box::new(right),
                })?;
                branches.push((condition, branch_expr, branch_locals));
            }
            sv_parser::CaseItem::Default(item) => {
                let mut branch_locals = locals.clone();
                default_branch = (
                    function_expr_from_statement_or_null_stmt(
                        &item.nodes.2,
                        &mut branch_locals,
                        syntax_tree,
                        packed_dimensions,
                        local_types,
                    ),
                    branch_locals,
                );
            }
        }
    }

    if branches.iter().all(|(_, expr, _)| expr.is_some()) && default_branch.0.is_some() {
        let mut result = default_branch.0?;
        for (condition, branch_expr, _) in branches.into_iter().rev() {
            result = Expr::Mux {
                condition: Box::new(condition),
                then_expr: Box::new(branch_expr?),
                else_expr: Box::new(result),
            };
        }
        return Some(result);
    }

    if branches.iter().all(|(_, expr, _)| expr.is_none()) && default_branch.0.is_none() {
        let mut merged = locals.clone();
        let mut names = locals.keys().cloned().collect::<HashSet<_>>();
        names.extend(default_branch.1.keys().cloned());
        names.extend(
            branches
                .iter()
                .flat_map(|(_, _, branch_locals)| branch_locals.keys().cloned()),
        );
        for name in names {
            let mut value = default_branch
                .1
                .get(&name)
                .or_else(|| locals.get(&name))
                .cloned()
                .unwrap_or_else(|| Expr::Ident(name.clone()));
            for (condition, _, branch_locals) in branches.iter().rev() {
                let branch_value = branch_locals
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| value.clone());
                if branch_value != value {
                    value = Expr::Mux {
                        condition: Box::new(condition.clone()),
                        then_expr: Box::new(branch_value),
                        else_expr: Box::new(value),
                    };
                }
            }
            merged.insert(name, value);
        }
        *locals = merged;
        return None;
    }

    Some(Expr::Call {
        name: "$unsupported_mixed_function_case".to_string(),
        args: Vec::new(),
    })
}

/// Whether a `case` keyword treats `z`/`?` (and `x` for `casex`) in a case
/// item as a wildcard.
pub(super) fn case_keyword_is_wildcard(keyword: &sv_parser::CaseKeyword) -> bool {
    !matches!(keyword, sv_parser::CaseKeyword::Case(_))
}

pub(super) fn case_item_condition(case_expr: Expr, item_expr: Expr, wildcard: bool) -> Expr {
    Expr::Binary {
        left: Box::new(case_expr),
        op: if wildcard {
            BinaryOp::EqWildcard
        } else {
            BinaryOp::EqCase
        },
        right: Box::new(item_expr),
    }
}
