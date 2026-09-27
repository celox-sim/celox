//! Module declarations, ports, signals, and syntax-node lookup.

use super::*;

pub(super) fn module_name_from_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
) -> Result<String, AnalyzerError> {
    let id = unwrap_node!(node, ModuleIdentifier)
        .ok_or_else(|| AnalyzerError::Unsupported("module without identifier".to_string()))?;
    let locate = identifier_locate(id)
        .ok_or_else(|| AnalyzerError::Unsupported("unsupported module identifier".to_string()))?;
    syntax_tree
        .get_str(&locate)
        .map(str::to_string)
        .ok_or_else(|| AnalyzerError::Unsupported("invalid module identifier span".to_string()))
}

pub(super) fn identifier_locate(node: RefNode<'_>) -> Option<Locate> {
    match unwrap_node!(node, SimpleIdentifier, EscapedIdentifier) {
        Some(RefNode::SimpleIdentifier(identifier)) => Some(identifier.nodes.0),
        Some(RefNode::EscapedIdentifier(identifier)) => Some(identifier.nodes.0),
        _ => None,
    }
}

pub(super) fn ports_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<Vec<Port>, AnalyzerError> {
    let mut ports = Vec::new();
    let mut inherited_direction = PortDirection::Unspecified;
    let mut inherited_type = Type::implicit();
    for child in node {
        match child {
            RefNode::AnsiPortDeclarationNet(port) => {
                let header = port.nodes.0.as_ref();
                let direction = header
                    .and_then(|header| direction_from_ref_node(header.into()))
                    .unwrap_or(inherited_direction);
                let r#type = match header {
                    Some(header) => {
                        type_from_net_port_header(header, syntax_tree, const_env, type_aliases)
                            .ok_or_else(|| {
                                AnalyzerError::Unsupported("unsupported port data type".to_string())
                            })?
                    }
                    None => inherited_type.clone(),
                };
                let r#type = type_with_fallback_ranges_with_env(
                    r#type,
                    RefNode::AnsiPortDeclarationNet(port),
                    syntax_tree,
                    const_env,
                    type_aliases,
                );
                let inherited_type_base = r#type.clone();
                let r#type = type_with_unpacked_ranges(
                    r#type,
                    unpacked_ranges_from_dimensions_with_env(
                        &port.nodes.2,
                        syntax_tree,
                        const_env,
                        type_aliases,
                    )?,
                );
                let name = port_name(RefNode::PortIdentifier(&port.nodes.1), syntax_tree)?;
                inherited_direction = direction;
                inherited_type = inherited_type_base;
                ports.push(Port::new(name, direction, r#type, true));
            }
            RefNode::AnsiPortDeclarationVariable(port) => {
                let header = port.nodes.0.as_ref();
                let direction = header
                    .and_then(|header| direction_from_ref_node(header.into()))
                    .unwrap_or(inherited_direction);
                let r#type = match header {
                    Some(header) => {
                        type_from_variable_port_header(header, syntax_tree, const_env, type_aliases)
                            .ok_or_else(|| {
                                AnalyzerError::Unsupported("unsupported port data type".to_string())
                            })?
                    }
                    None => inherited_type.clone(),
                };
                let r#type = type_with_fallback_ranges_with_env(
                    r#type,
                    RefNode::AnsiPortDeclarationVariable(port),
                    syntax_tree,
                    const_env,
                    type_aliases,
                );
                let inherited_type_base = r#type.clone();
                let r#type = type_with_unpacked_ranges(
                    r#type,
                    unpacked_ranges_from_variable_dimensions_with_env(
                        &port.nodes.2,
                        syntax_tree,
                        const_env,
                        type_aliases,
                    )?,
                );
                let name = port_name(RefNode::PortIdentifier(&port.nodes.1), syntax_tree)?;
                inherited_direction = direction;
                inherited_type = inherited_type_base;
                ports.push(Port::new(name, direction, r#type, false));
            }
            RefNode::AnsiPortDeclarationParen(port) => {
                let explicit_direction = port.nodes.0.as_ref().map(direction_from_port_direction);
                let direction = explicit_direction.unwrap_or(inherited_direction);
                let r#type = if explicit_direction.is_some() {
                    Type::implicit()
                } else {
                    inherited_type.clone()
                };
                let name = port_name(RefNode::PortIdentifier(&port.nodes.2), syntax_tree)?;
                inherited_direction = direction;
                inherited_type = r#type.clone();
                ports.push(Port::new(name, direction, r#type, false));
            }
            _ => {}
        }
    }
    Ok(ports)
}

pub(super) fn parameters_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    base_const_env: &HashMap<String, i128>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Result<Vec<Parameter>, AnalyzerError> {
    let mut parameters = Vec::new();
    if let Some(parameter_port_list) = module_parameter_port_list(node.clone()) {
        let RefNode::ParameterPortList(parameter_port_list) = parameter_port_list else {
            unreachable!();
        };
        parameters_from_parameter_port_list(
            parameter_port_list,
            syntax_tree,
            &mut parameters,
            base_const_env,
            type_aliases,
            parameter_overrides,
        )?;
    }

    for item in module_non_port_items(node.clone()) {
        if let Some(declaration) = package_or_generate_declaration_from_non_port_item(item) {
            match declaration {
                sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(
                    localparam,
                ) => parameters_from_ref_node(
                    RefNode::LocalParameterDeclaration(&localparam.0),
                    syntax_tree,
                    &mut parameters,
                    true,
                    base_const_env,
                    type_aliases,
                    parameter_overrides,
                )?,
                sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(parameter) => {
                    parameters_from_ref_node(
                        RefNode::ParameterDeclaration(&parameter.0),
                        syntax_tree,
                        &mut parameters,
                        false,
                        base_const_env,
                        type_aliases,
                        parameter_overrides,
                    )?
                }
                _ => {}
            }
        }
    }

    Ok(parameters)
}

fn parameters_from_parameter_port_list(
    list: &sv_parser::ParameterPortList,
    syntax_tree: &SyntaxTree,
    parameters: &mut Vec<Parameter>,
    base_const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Result<(), AnalyzerError> {
    match list {
        sv_parser::ParameterPortList::Assignment(list) => {
            parameters_from_ref_node(
                RefNode::ListOfParamAssignments(&list.nodes.1.nodes.1.0),
                syntax_tree,
                parameters,
                false,
                base_const_env,
                type_aliases,
                parameter_overrides,
            )?;
            for (_, declaration) in &list.nodes.1.nodes.1.1 {
                parameters_from_parameter_port_declaration(
                    declaration,
                    syntax_tree,
                    parameters,
                    base_const_env,
                    type_aliases,
                    parameter_overrides,
                )?;
            }
        }
        sv_parser::ParameterPortList::Declaration(list) => {
            for declaration in list.nodes.1.nodes.1.contents() {
                parameters_from_parameter_port_declaration(
                    declaration,
                    syntax_tree,
                    parameters,
                    base_const_env,
                    type_aliases,
                    parameter_overrides,
                )?;
            }
        }
        sv_parser::ParameterPortList::Empty(_) => {}
    }
    Ok(())
}

fn parameters_from_parameter_port_declaration(
    declaration: &sv_parser::ParameterPortDeclaration,
    syntax_tree: &SyntaxTree,
    parameters: &mut Vec<Parameter>,
    base_const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Result<(), AnalyzerError> {
    let is_local = matches!(
        declaration,
        sv_parser::ParameterPortDeclaration::LocalParameterDeclaration(_)
    );
    if matches!(
        declaration,
        sv_parser::ParameterPortDeclaration::TypeList(_)
    ) {
        return Ok(());
    }
    parameters_from_ref_node(
        RefNode::ParameterPortDeclaration(declaration),
        syntax_tree,
        parameters,
        is_local,
        base_const_env,
        type_aliases,
        parameter_overrides,
    )
}

pub(super) fn signals_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<Vec<Signal>, AnalyzerError> {
    let mut signals = Vec::new();
    for item in generate::items(node, syntax_tree, const_env, type_aliases)? {
        let start = signals.len();
        signals_from_module_or_generate_item(
            item.node,
            syntax_tree,
            type_aliases,
            &item.env,
            None,
            &mut signals,
        )?;
        for signal in &mut signals[start..] {
            signal.name = item.name(&signal.name);
        }
    }
    signals.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(name) = signals
        .windows(2)
        .find(|pair| pair[0].name == pair[1].name)
        .map(|pair| pair[0].name.clone())
    {
        return Err(AnalyzerError::Unsupported(format!(
            "duplicate internal signal `{name}`"
        )));
    }
    Ok(signals)
}

pub(super) fn signals_from_module_or_generate_item(
    item: &sv_parser::ModuleOrGenerateItem,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
    selected_name: Option<&str>,
    signals: &mut Vec<Signal>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::ModuleOrGenerateItem::Module(module) => {
            let mut alias_signals = signals_from_type_alias_instantiation(
                &module.nodes.1,
                syntax_tree,
                type_aliases,
                const_env,
                selected_name,
            )?;
            substitute_signal_local_constants(&mut alias_signals, const_env);
            signals.extend(alias_signals);
        }
        sv_parser::ModuleOrGenerateItem::ModuleItem(item) => {
            signals_from_module_common_item(
                &item.nodes.1,
                syntax_tree,
                type_aliases,
                const_env,
                selected_name,
                signals,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn signals_from_module_common_item(
    item: &sv_parser::ModuleCommonItem,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
    selected_name: Option<&str>,
    signals: &mut Vec<Signal>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(declaration) = item {
        let sv_parser::ModuleOrGenerateItemDeclaration::PackageOrGenerateItemDeclaration(
            declaration,
        ) = &**declaration
        else {
            return Ok(());
        };
        let mut declared = match &**declaration {
            sv_parser::PackageOrGenerateItemDeclaration::DataDeclaration(data) => {
                signals_from_data_declaration(
                    data,
                    syntax_tree,
                    type_aliases,
                    const_env,
                    selected_name,
                )?
            }
            sv_parser::PackageOrGenerateItemDeclaration::NetDeclaration(net) => {
                signals_from_net_declaration(
                    net,
                    syntax_tree,
                    type_aliases,
                    const_env,
                    selected_name,
                )?
            }
            _ => Vec::new(),
        };
        substitute_signal_local_constants(&mut declared, const_env);
        signals.extend(declared);
    }
    Ok(())
}

fn substitute_signal_local_constants(signals: &mut [Signal], const_env: &HashMap<String, i128>) {
    for signal in signals {
        for range in &mut signal.r#type.packed_ranges {
            range.left = substitute_dimension_constants(range.left.clone(), const_env);
            range.right = substitute_dimension_constants(range.right.clone(), const_env);
        }
        for range in &mut signal.r#type.unpacked_ranges {
            range.left = substitute_dimension_constants(range.left.clone(), const_env);
            range.right = substitute_dimension_constants(range.right.clone(), const_env);
            range.size = range
                .size
                .take()
                .map(|size| substitute_dimension_constants(size, const_env));
        }
    }
}

fn signals_from_net_declaration(
    net: &sv_parser::NetDeclaration,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
    selected_name: Option<&str>,
) -> Result<Vec<Signal>, AnalyzerError> {
    let (r#type, assignments, is_net) = match net {
        sv_parser::NetDeclaration::NetType(net) => {
            let r#type = type_from_ref_node_with_env(
                RefNode::DataTypeOrImplicit(&net.nodes.3),
                syntax_tree,
                const_env,
                type_aliases,
            )
            .or_else(|| {
                type_alias_from_ref_node(
                    RefNode::DataTypeOrImplicit(&net.nodes.3),
                    syntax_tree,
                    type_aliases,
                )
            });
            let r#type = match (&net.nodes.3, r#type) {
                (_, Some(r#type)) => r#type,
                (sv_parser::DataTypeOrImplicit::ImplicitDataType(_), None) => Type::implicit(),
                (sv_parser::DataTypeOrImplicit::DataType(_), None) => {
                    return Err(AnalyzerError::Unsupported(
                        "unsupported net data type".to_string(),
                    ));
                }
            };
            let r#type = type_with_fallback_ranges_with_env(
                r#type,
                RefNode::DataTypeOrImplicit(&net.nodes.3),
                syntax_tree,
                const_env,
                type_aliases,
            );
            (r#type, net.nodes.5.nodes.0.contents(), true)
        }
        sv_parser::NetDeclaration::NetTypeIdentifier(net) => {
            let Some(name) = identifier_text(RefNode::NetTypeIdentifier(&net.nodes.0), syntax_tree)
            else {
                return Ok(Vec::new());
            };
            let Some(r#type) = type_aliases.get(&name).cloned() else {
                return Ok(Vec::new());
            };
            (r#type, net.nodes.2.nodes.0.contents(), false)
        }
        sv_parser::NetDeclaration::Interconnect(_) => {
            return Err(AnalyzerError::Unsupported(
                "interconnect net declaration".to_string(),
            ));
        }
    };
    let mut signals = Vec::new();
    for assignment in assignments {
        let name = identifier_text(RefNode::NetIdentifier(&assignment.nodes.0), syntax_tree)
            .ok_or_else(|| {
                AnalyzerError::Unsupported("unsupported signal identifier".to_string())
            })?;
        if selected_name.is_some_and(|selected| selected != name) {
            continue;
        }
        let signal_type = type_with_unpacked_ranges(
            r#type.clone(),
            unpacked_ranges_from_dimensions_with_env(
                &assignment.nodes.1,
                syntax_tree,
                const_env,
                type_aliases,
            )?,
        );
        signals.push(if is_net {
            Signal::new_net(name, signal_type)
        } else {
            Signal::new(name, signal_type)
        });
    }
    Ok(signals)
}

fn signals_from_type_alias_instantiation(
    instantiation: &sv_parser::ModuleInstantiation,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
    selected_name: Option<&str>,
) -> Result<Vec<Signal>, AnalyzerError> {
    let mut signals = Vec::new();
    let module_name = identifier_text(
        RefNode::ModuleIdentifier(&instantiation.nodes.0),
        syntax_tree,
    )
    .ok_or_else(|| {
        AnalyzerError::Unsupported("unsupported module instantiation identifier".to_string())
    })?;
    let Some(r#type) = type_aliases.get(&module_name) else {
        return Ok(signals);
    };
    for instance in instantiation.nodes.2.contents() {
        let name = identifier_text(
            RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0),
            syntax_tree,
        )
        .ok_or_else(|| AnalyzerError::Unsupported("unsupported signal identifier".to_string()))?;
        if selected_name.is_some_and(|selected| selected != name) {
            continue;
        }
        let signal_type = type_with_unpacked_ranges(
            r#type.clone(),
            unpacked_ranges_from_dimensions_with_env(
                &instance.nodes.0.nodes.1,
                syntax_tree,
                const_env,
                type_aliases,
            )?,
        );
        signals.push(Signal::new(name, signal_type));
    }
    Ok(signals)
}

pub(super) fn signals_from_data_declaration(
    data: &sv_parser::DataDeclaration,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
    selected_name: Option<&str>,
) -> Result<Vec<Signal>, AnalyzerError> {
    let sv_parser::DataDeclaration::Variable(variable) = data else {
        return Ok(Vec::new());
    };
    let r#type = type_from_ref_node_with_env(
        RefNode::DataTypeOrImplicit(&variable.nodes.3),
        syntax_tree,
        const_env,
        type_aliases,
    )
    .or_else(|| {
        type_alias_from_ref_node(
            RefNode::DataTypeOrImplicit(&variable.nodes.3),
            syntax_tree,
            type_aliases,
        )
    });
    let r#type = match (&variable.nodes.3, r#type) {
        (_, Some(r#type)) => r#type,
        (sv_parser::DataTypeOrImplicit::ImplicitDataType(_), None) => Type::implicit(),
        (sv_parser::DataTypeOrImplicit::DataType(_), None) => {
            return Err(AnalyzerError::Unsupported(
                "unsupported internal data type".to_string(),
            ));
        }
    };
    let r#type = type_with_fallback_ranges_with_env(
        r#type,
        RefNode::DataTypeOrImplicit(&variable.nodes.3),
        syntax_tree,
        const_env,
        type_aliases,
    );
    let mut signals = Vec::new();
    for assignment in variable.nodes.4.nodes.0.contents() {
        let sv_parser::VariableDeclAssignment::Variable(assignment) = assignment else {
            continue;
        };
        let name = identifier_text(
            RefNode::VariableIdentifier(&assignment.nodes.0),
            syntax_tree,
        )
        .ok_or_else(|| AnalyzerError::Unsupported("unsupported signal identifier".to_string()))?;
        if selected_name.is_some_and(|selected| selected != name) {
            continue;
        }
        let signal_type = type_with_unpacked_ranges(
            r#type.clone(),
            unpacked_ranges_from_variable_dimensions_with_env(
                &assignment.nodes.1,
                syntax_tree,
                const_env,
                type_aliases,
            )?,
        );
        signals.push(Signal::new(name, signal_type));
    }
    Ok(signals)
}

pub(super) fn type_alias_from_ref_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    // A cast within a built-in type's range does not make the declared type
    // an alias. Inspect the outer type before searching wrapper nodes.
    match &node {
        RefNode::DataType(data_type) => {
            return type_alias_from_data_type(data_type, syntax_tree, type_aliases);
        }
        RefNode::DataTypeOrImplicit(data_type) => {
            return type_alias_from_data_type_or_implicit(data_type, syntax_tree, type_aliases);
        }
        _ => {}
    }
    if let Some(RefNode::DataType(data_type)) = unwrap_node!(node.clone(), DataType)
        && let Some(r#type) = type_alias_from_data_type(data_type, syntax_tree, type_aliases)
    {
        return Some(r#type);
    }
    if let Some(RefNode::DataTypeOrImplicit(data_type)) =
        unwrap_node!(node.clone(), DataTypeOrImplicit)
        && let Some(r#type) =
            type_alias_from_data_type_or_implicit(data_type, syntax_tree, type_aliases)
    {
        return Some(r#type);
    }
    let name =
        if let Some(RefNode::DataTypeType(data_type)) = unwrap_node!(node.clone(), DataTypeType) {
            identifier_text(RefNode::TypeIdentifier(&data_type.nodes.1), syntax_tree)?
        } else {
            let RefNode::TypeIdentifier(identifier) = unwrap_node!(node, TypeIdentifier)? else {
                return None;
            };
            identifier_text(RefNode::TypeIdentifier(identifier), syntax_tree)?
        };
    type_aliases.get(&name).cloned()
}

pub(super) fn module_parameter_port_list(node: RefNode<'_>) -> Option<RefNode<'_>> {
    match node {
        RefNode::ModuleDeclarationAnsi(module) => module
            .nodes
            .0
            .nodes
            .5
            .as_ref()
            .map(RefNode::ParameterPortList),
        RefNode::ModuleDeclarationNonansi(module) => module
            .nodes
            .0
            .nodes
            .5
            .as_ref()
            .map(RefNode::ParameterPortList),
        _ => None,
    }
}

pub(super) fn module_non_port_items(node: RefNode<'_>) -> Vec<&sv_parser::NonPortModuleItem> {
    match node {
        RefNode::ModuleDeclarationAnsi(module) => module.nodes.2.iter().collect(),
        RefNode::ModuleDeclarationNonansi(_) => Vec::new(),
        _ => Vec::new(),
    }
}

// Explicit generate regions do not introduce a scope. Conditional/loop
// constructs and function bodies do, and must not leak declarations here.
pub(super) fn module_scope_items(node: RefNode<'_>) -> Vec<&sv_parser::ModuleOrGenerateItem> {
    let mut direct = Vec::new();
    for item in module_non_port_items(node) {
        match item {
            sv_parser::NonPortModuleItem::ModuleOrGenerateItem(item) => direct.push(&**item),
            sv_parser::NonPortModuleItem::GenerateRegion(region) => {
                for item in &region.nodes.1 {
                    if let sv_parser::GenerateItem::ModuleOrGenerateItem(item) = item {
                        direct.push(&**item);
                    }
                }
            }
            _ => {}
        }
    }
    direct
}

pub(super) fn package_or_generate_declaration_from_non_port_item(
    item: &sv_parser::NonPortModuleItem,
) -> Option<&sv_parser::PackageOrGenerateItemDeclaration> {
    let sv_parser::NonPortModuleItem::ModuleOrGenerateItem(item) = item else {
        return None;
    };
    package_or_generate_declaration_from_module_item(item)
}

pub(super) fn package_or_generate_declaration_from_module_item(
    item: &sv_parser::ModuleOrGenerateItem,
) -> Option<&sv_parser::PackageOrGenerateItemDeclaration> {
    let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = item else {
        return None;
    };
    let sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(declaration) = &item.nodes.1
    else {
        return None;
    };
    let sv_parser::ModuleOrGenerateItemDeclaration::PackageOrGenerateItemDeclaration(declaration) =
        &**declaration
    else {
        return None;
    };
    Some(declaration)
}

pub(super) fn parameter_name(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
) -> Result<String, AnalyzerError> {
    let locate = identifier_locate(node).ok_or_else(|| {
        AnalyzerError::Unsupported("unsupported parameter identifier".to_string())
    })?;
    syntax_tree
        .get_str(&locate)
        .map(str::to_string)
        .ok_or_else(|| AnalyzerError::Unsupported("invalid parameter identifier span".to_string()))
}

fn port_name(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Result<String, AnalyzerError> {
    let locate = identifier_locate(node)
        .ok_or_else(|| AnalyzerError::Unsupported("unsupported port identifier".to_string()))?;
    syntax_tree
        .get_str(&locate)
        .map(str::to_string)
        .ok_or_else(|| AnalyzerError::Unsupported("invalid port identifier span".to_string()))
}
