//! Module instantiations, parameter overrides, and port connections.

use super::*;

pub(super) fn instances_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    interfaces: &InterfaceLookup<'_>,
) -> Result<Vec<Instance>, AnalyzerError> {
    let type_aliases = type_aliases_from_module_node(node.clone(), syntax_tree)?;
    let active = generate::items(node, syntax_tree, const_env, &type_aliases)?;
    let mut instances = Vec::new();
    for item in active {
        // Packages declare no instances, signals or processes here.
        let ScopeItem::Module(node) = item.node else {
            continue;
        };
        if item.is_parameter_declaration() {
            continue;
        }
        let start = instances.len();
        let dimensions = item.dimensions(packed_dimensions);
        instances_from_module_or_generate_item(
            node,
            None,
            syntax_tree,
            &item.env,
            &dimensions,
            interfaces,
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
    interfaces: &InterfaceLookup<'_>,
    instances: &mut Vec<Instance>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::ModuleOrGenerateItem::Module(module) = item {
        instances_from_module_instantiation(
            &module.nodes.1,
            condition,
            syntax_tree,
            const_env,
            packed_dimensions,
            interfaces,
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
    interfaces: &InterfaceLookup<'_>,
    instances: &mut Vec<Instance>,
) -> Result<(), AnalyzerError> {
    let module_name = identifier_text(
        RefNode::ModuleIdentifier(&instantiation.nodes.0),
        syntax_tree,
    )
    .ok_or_else(|| {
        AnalyzerError::Unsupported("unsupported module instantiation identifier".to_string())
    })?;
    let interface = interfaces.get(&module_name);
    let mut parameter_overrides = parameter_overrides_from_value_assignment(
        instantiation.nodes.1.as_ref(),
        syntax_tree,
        interface,
        packed_dimensions,
    )?;
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
        let mut port_connections = port_connections_from_hierarchical_instance(
            instance,
            syntax_tree,
            packed_dimensions,
            interface,
        )?;
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
        // `Child c[3:0](...)` is an array of instances: one dimension with
        // constant bounds. `my_t mem [N];` parses the same way but declares a
        // variable of a typedef'd type, and is dropped below.
        let dimensions = &instance.nodes.0.nodes.1;
        let array_range =
            if dimensions.is_empty() || packed_dimensions.type_aliases.contains_key(&module_name) {
                None
            } else {
                let ranges = unpacked_ranges_from_dimensions_with_env(
                    dimensions,
                    syntax_tree,
                    const_env,
                    &packed_dimensions.type_aliases,
                )?;
                let [range] = ranges.as_slice() else {
                    return Err(AnalyzerError::Unsupported(
                        "multidimensional module instance array".to_string(),
                    ));
                };
                let left = eval_ast_const_expr(range.left(), const_env);
                let right = eval_ast_const_expr(range.right(), const_env);
                let bounds = left
                    .zip(right)
                    .filter(|(left, right)| left.abs_diff(*right) < 4096)
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported("module instance array bounds".to_string())
                    })?;
                Some(bounds)
            };
        instances.push(Instance::new(
            module_name.clone(),
            name,
            parameter_names.clone(),
            parameter_overrides.clone(),
            condition.clone(),
            port_names,
            port_connections,
            array_range,
        ));
    }
    Ok(())
}

fn parameter_overrides_from_value_assignment(
    assignment: Option<&sv_parser::ParameterValueAssignment>,
    syntax_tree: &SyntaxTree,
    interface: Option<&ModuleInterface>,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<ParameterOverride>, AnalyzerError> {
    let Some(assignment) = assignment else {
        return Ok(Vec::new());
    };
    let Some(assignments) = assignment.nodes.1.nodes.1.as_ref() else {
        return Ok(Vec::new());
    };
    let assignments = match assignments {
        sv_parser::ListOfParameterAssignments::Named(assignments) => assignments,
        sv_parser::ListOfParameterAssignments::Ordered(assignments) => {
            // Positional values bind to the parameters of the instantiated
            // module's `#(...)` list, in order.
            let parameters = interface
                .map(|interface| interface.parameters.as_slice())
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("ordered parameter assignment".to_string())
                })?;
            let mut overrides = Vec::new();
            for (position, assignment) in assignments.nodes.0.contents().into_iter().enumerate() {
                let name = parameters.get(position).ok_or_else(|| {
                    AnalyzerError::Unsupported("ordered parameter assignment".to_string())
                })?;
                overrides.push(parameter_override(
                    name.clone(),
                    &assignment.nodes.0,
                    syntax_tree,
                    packed_dimensions,
                )?);
            }
            return Ok(overrides);
        }
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
        let Some(expr) = assignment.nodes.2.nodes.1.as_ref() else {
            return Err(AnalyzerError::Unsupported(format!(
                "empty parameter override `{name}`"
            )));
        };
        overrides.push(parameter_override(
            name,
            expr,
            syntax_tree,
            packed_dimensions,
        )?);
    }
    Ok(overrides)
}

fn port_connections_from_hierarchical_instance(
    instance: &sv_parser::HierarchicalInstance,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    interface: Option<&ModuleInterface>,
) -> Result<Vec<PortConnection>, AnalyzerError> {
    let Some(connections) = instance.nodes.1.nodes.1.as_ref() else {
        return Ok(Vec::new());
    };
    let sv_parser::ListOfPortConnections::Named(connections) = connections else {
        let sv_parser::ListOfPortConnections::Ordered(connections) = connections else {
            return Err(AnalyzerError::Unsupported(
                "ordered port connection".to_string(),
            ));
        };
        let connections = connections.nodes.0.contents();
        if connections
            .iter()
            .all(|connection| connection.nodes.1.is_none())
        {
            return Ok(Vec::new());
        }
        // Positional connections bind to the ports of the instantiated module,
        // in declaration order.
        let ports = interface
            .map(|interface| interface.ports.as_slice())
            .ok_or_else(|| AnalyzerError::Unsupported("ordered port connection".to_string()))?;
        let mut lowered = Vec::new();
        for (position, connection) in connections.into_iter().enumerate() {
            let formal = ports
                .get(position)
                .ok_or_else(|| AnalyzerError::Unsupported("ordered port connection".to_string()))?;
            let Some(expr) = connection.nodes.1.as_ref() else {
                continue;
            };
            let actual_expr =
                expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)?;
            let actual = expr_ident_name(&actual_expr).unwrap_or_else(|| formal.clone());
            lowered.push(PortConnection::new(
                formal.clone(),
                actual,
                Some(actual_expr),
            ));
        }
        return Ok(lowered);
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
                        Some(expr) => Some(expr_from_expression_with_types(
                            expr,
                            syntax_tree,
                            packed_dimensions,
                        )?),
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
pub(super) fn collect_connected_nets<'a>(expr: &'a Expr, names: &mut HashSet<&'a str>) {
    match expr {
        Expr::Ident(actual) => {
            names.insert(actual);
        }
        Expr::Select { expr, .. }
        | Expr::Resize { expr, .. }
        | Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr,
        } => collect_connected_nets(expr, names),
        Expr::Concat(parts) => {
            for part in parts {
                collect_connected_nets(part, names);
            }
        }
        _ => {}
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
    syntax_tree.get_str(&locate).map(normalize_identifier)
}

/// An escaped identifier is the same identifier as its text without the
/// backslash (IEEE 1800-2023 5.6.1): `\a` names `a`. One that cannot be written
/// without escaping, such as `\a.b`, keeps its backslash.
pub(super) fn normalize_identifier(text: &str) -> String {
    match text.strip_prefix('\\') {
        Some(name)
            if name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') =>
        {
            name.to_string()
        }
        _ => text.to_string(),
    }
}

/// The source text a node spans, from its first to its last token.
pub(super) fn node_source_text(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Option<String> {
    let mut range: Option<(usize, usize)> = None;
    for child in node {
        if let RefNode::Locate(locate) = child {
            let (start, end) = (locate.offset, locate.offset + locate.len);
            range = Some(match range {
                None => (start, end),
                Some((low, high)) => (low.min(start), high.max(end)),
            });
        }
    }
    let (start, end) = range?;
    syntax_tree
        .get_str(&sv_parser::Locate {
            offset: start,
            line: 0,
            len: end - start,
        })
        .map(|text| text.trim().to_string())
}

/// One `.name(expr)` binding: a `parameter type` when `expr` is a data type or
/// the name of a type, a value otherwise.
fn parameter_override(
    name: String,
    expr: &sv_parser::ParamExpression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Result<ParameterOverride, AnalyzerError> {
    if let sv_parser::ParamExpression::DataType(data_type) = expr {
        let text = node_source_text(RefNode::DataType(data_type), syntax_tree)
            .ok_or_else(|| AnalyzerError::Unsupported("parameter type override".to_string()))?;
        return Ok(ParameterOverride::type_override(name, text));
    }
    let value = const_expr_from_param_expression(expr, syntax_tree)?
        .ok_or_else(|| AnalyzerError::Unsupported("parameter override expression".to_string()))?;
    // A bare name that denotes a type (a typedef, or the instantiating
    // module's own `parameter type`) is passed on as that type.
    if let ConstExpr::Ident(type_name) = &value
        && let Some(r#type) = packed_dimensions.type_aliases.get(type_name)
    {
        let text = type_source_text(r#type, &packed_dimensions.const_env).ok_or_else(|| {
            AnalyzerError::Unsupported(format!("parameter type override `{name}`"))
        })?;
        return Ok(ParameterOverride::type_override(name, text));
    }
    Ok(ParameterOverride::new(name, Some(value)))
}

/// Source text for a plain (possibly signed) packed vector type.
fn type_source_text(r#type: &Type, const_env: &HashMap<String, i128>) -> Option<String> {
    if !r#type.unpacked_ranges().is_empty() || !r#type.members.is_empty() {
        return None;
    }
    let mut text = match r#type.kind() {
        TypeKind::Bit => "bit",
        _ => "logic",
    }
    .to_string();
    if r#type.is_signed() {
        text.push_str(" signed");
    }
    for range in r#type.packed_ranges() {
        let left = eval_ast_const_expr(range.left(), const_env)?;
        let right = eval_ast_const_expr(range.right(), const_env)?;
        text.push_str(&format!(" [{left}:{right}]"));
    }
    Some(text)
}
