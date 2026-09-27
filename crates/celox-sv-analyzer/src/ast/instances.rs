//! Module instantiations, parameter overrides, and port connections.

use super::*;

pub(super) fn instances_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<Instance>, AnalyzerError> {
    let type_aliases = type_aliases_from_module_node(node.clone(), syntax_tree)?;
    let active = generate::items(node, syntax_tree, const_env, &type_aliases)?;
    for child in active
        .iter()
        .flat_map(|item| RefNode::ModuleOrGenerateItem(item.node).into_iter())
    {
        let RefNode::ModuleInstantiation(instantiation) = child else {
            continue;
        };
        let module_name = identifier_text(
            RefNode::ModuleIdentifier(&instantiation.nodes.0),
            syntax_tree,
        )
        .ok_or_else(|| {
            AnalyzerError::Unsupported("unsupported module instantiation identifier".to_string())
        })?;
        if !type_aliases.contains_key(&module_name)
            && instantiation
                .nodes
                .2
                .contents()
                .iter()
                .any(|instance| !instance.nodes.0.nodes.1.is_empty())
        {
            return Err(AnalyzerError::Unsupported(
                "module instance array".to_string(),
            ));
        }
    }
    let mut instances = Vec::new();
    for item in active {
        let start = instances.len();
        let dimensions = item.dimensions(packed_dimensions);
        instances_from_module_or_generate_item(
            item.node,
            None,
            syntax_tree,
            &item.env,
            &dimensions,
            &mut instances,
        )?;
        for instance in &mut instances[start..] {
            instance.name = item.instance_name(&instance.name);
            for connection in &mut instance.port_connections {
                if let Some(expr) = &mut connection.actual_expr {
                    *expr = substitute_expr_constants_with_parameter_literals(
                        expr.clone(),
                        &item.env,
                        &item.literals,
                    );
                    item.expr(expr);
                }
            }
        }
    }
    instances.retain(|instance| !type_aliases.contains_key(instance.module_name()));
    Ok(instances)
}

fn instances_from_module_or_generate_item(
    item: &sv_parser::ModuleOrGenerateItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    instances: &mut Vec<Instance>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::ModuleOrGenerateItem::Module(module) = item {
        instances_from_module_instantiation(
            &module.nodes.1,
            condition,
            syntax_tree,
            const_env,
            packed_dimensions,
            instances,
        )?;
    }
    Ok(())
}

fn instances_from_module_instantiation(
    instantiation: &sv_parser::ModuleInstantiation,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    instances: &mut Vec<Instance>,
) -> Result<(), AnalyzerError> {
    let module_name = identifier_text(
        RefNode::ModuleIdentifier(&instantiation.nodes.0),
        syntax_tree,
    )
    .ok_or_else(|| {
        AnalyzerError::Unsupported("unsupported module instantiation identifier".to_string())
    })?;
    let mut parameter_overrides =
        parameter_overrides_from_value_assignment(instantiation.nodes.1.as_ref(), syntax_tree)?;
    for override_ in &mut parameter_overrides {
        if let Some(value) = override_.value.take() {
            let value = substitute_typed_parameter_literals(
                value,
                const_env,
                &parameter_types_from_const_env(const_env),
            );
            let value = expr_to_const(substitute_expr_idents(
                const_expr_to_expr(value),
                &packed_dimensions.parameter_values,
            ))
            .ok_or_else(|| {
                AnalyzerError::Unsupported(format!(
                    "constant module parameter override `{}`",
                    override_.name
                ))
            })?;
            override_.value = Some(substitute_const_expr_constants_preserving_enum_types(
                value, const_env,
            ));
        }
    }
    let condition =
        condition.map(|condition| substitute_const_expr_constants(condition, const_env));
    let parameter_names: Vec<String> = parameter_overrides
        .iter()
        .map(|parameter| parameter.name().to_string())
        .collect();
    for instance in instantiation.nodes.2.contents() {
        let name = identifier_text(
            RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0),
            syntax_tree,
        )
        .ok_or_else(|| AnalyzerError::Unsupported("unsupported instance identifier".to_string()))?;
        let mut port_connections =
            port_connections_from_hierarchical_instance(instance, syntax_tree, packed_dimensions)?;
        for connection in &mut port_connections {
            connection.actual_expr = connection.actual_expr.take().map(|expr| {
                substitute_expr_constants_with_parameter_literals(
                    expr,
                    const_env,
                    &HashMap::default(),
                )
            });
        }
        let port_names = port_connections
            .iter()
            .map(|connection| connection.formal().to_string())
            .collect();
        instances.push(Instance::new(
            module_name.clone(),
            name,
            parameter_names.clone(),
            parameter_overrides.clone(),
            condition.clone(),
            port_names,
            port_connections,
        ));
    }
    Ok(())
}

fn parameter_overrides_from_value_assignment(
    assignment: Option<&sv_parser::ParameterValueAssignment>,
    syntax_tree: &SyntaxTree,
) -> Result<Vec<ParameterOverride>, AnalyzerError> {
    let Some(assignment) = assignment else {
        return Ok(Vec::new());
    };
    let Some(assignments) = assignment.nodes.1.nodes.1.as_ref() else {
        return Ok(Vec::new());
    };
    let sv_parser::ListOfParameterAssignments::Named(assignments) = assignments else {
        return Err(AnalyzerError::Unsupported(
            "ordered parameter assignment".to_string(),
        ));
    };
    let mut overrides = Vec::new();
    let mut names = HashSet::default();
    for assignment in assignments.nodes.0.contents() {
        let name = identifier_text(
            RefNode::ParameterIdentifier(&assignment.nodes.1),
            syntax_tree,
        )
        .ok_or_else(|| AnalyzerError::Unsupported("parameter override identifier".to_string()))?;
        if !names.insert(name.clone()) {
            return Err(AnalyzerError::Unsupported(format!(
                "duplicate parameter override `{name}`"
            )));
        }
        let value = match assignment.nodes.2.nodes.1.as_ref() {
            Some(expr) => Some(
                const_expr_from_param_expression(expr, syntax_tree).ok_or_else(|| {
                    AnalyzerError::Unsupported("parameter override expression".to_string())
                })?,
            ),
            None => {
                return Err(AnalyzerError::Unsupported(format!(
                    "empty parameter override `{name}`"
                )));
            }
        };
        overrides.push(ParameterOverride::new(name, value));
    }
    Ok(overrides)
}

fn port_connections_from_hierarchical_instance(
    instance: &sv_parser::HierarchicalInstance,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<PortConnection>, AnalyzerError> {
    let Some(connections) = instance.nodes.1.nodes.1.as_ref() else {
        return Ok(Vec::new());
    };
    let sv_parser::ListOfPortConnections::Named(connections) = connections else {
        if let sv_parser::ListOfPortConnections::Ordered(connections) = connections
            && connections
                .nodes
                .0
                .contents()
                .iter()
                .all(|connection| connection.nodes.1.is_none())
        {
            return Ok(Vec::new());
        }
        return Err(AnalyzerError::Unsupported(
            "ordered port connection".to_string(),
        ));
    };
    let mut lowered = Vec::new();
    for connection in connections.nodes.0.contents() {
        match connection {
            sv_parser::NamedPortConnection::Identifier(connection) => {
                let formal =
                    identifier_text(RefNode::PortIdentifier(&connection.nodes.2), syntax_tree)
                        .ok_or_else(|| {
                            AnalyzerError::Unsupported(
                                "named port connection identifier".to_string(),
                            )
                        })?;
                let actual_expr = match connection.nodes.3.as_ref() {
                    None => Some(Expr::Ident(formal.clone())),
                    Some(paren) => match paren.nodes.1.as_ref() {
                        None => None,
                        Some(expr) => Some(
                            expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)
                                .ok_or_else(|| {
                                AnalyzerError::Unsupported(
                                    "named port connection expression".to_string(),
                                )
                            })?,
                        ),
                    },
                };
                let actual = actual_expr
                    .as_ref()
                    .and_then(expr_ident_name)
                    .unwrap_or_else(|| formal.clone());
                lowered.push(PortConnection::new(formal, actual, actual_expr));
            }
            sv_parser::NamedPortConnection::Asterisk(_) => {}
        }
    }
    Ok(lowered)
}

// Child port directions are resolved by the frontend adapter. Defer its net
// driver check for selected/concatenated connections as well as whole nets.
pub(super) fn connection_references_net(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Ident(actual) => actual == name,
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } => {
            connection_references_net(expr, name)
        }
        Expr::Concat(parts) => parts
            .iter()
            .any(|part| connection_references_net(part, name)),
        _ => false,
    }
}

pub(super) fn expr_ident_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(name) => Some(name.clone()),
        _ => None,
    }
}

pub(super) fn identifier_text(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Option<String> {
    let locate = identifier_locate(node)?;
    syntax_tree.get_str(&locate).map(str::to_string)
}
