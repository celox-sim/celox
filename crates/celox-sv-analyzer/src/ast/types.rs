//! Type aliases, declaration types, and packed/unpacked range parsing.

use super::*;

pub(super) fn type_aliases_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
) -> Result<HashMap<String, Type>, AnalyzerError> {
    type_aliases_from_module_node_with_env(node, syntax_tree, &HashMap::default())
}

pub(super) fn type_aliases_from_module_node_with_env(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Result<HashMap<String, Type>, AnalyzerError> {
    let mut aliases = HashMap::default();
    if let Some(parameter_port_list) = module_parameter_port_list(node.clone()) {
        if let RefNode::ParameterPortList(parameter_port_list) = parameter_port_list {
            add_type_aliases_from_parameter_port_list(
                parameter_port_list,
                syntax_tree,
                const_env,
                &mut aliases,
            );
        }
        for child in parameter_port_list {
            match child {
                RefNode::LocalParameterDeclaration(localparam) => {
                    add_type_aliases_from_localparam(
                        localparam,
                        syntax_tree,
                        const_env,
                        &mut aliases,
                    );
                }
                RefNode::ParameterDeclaration(parameter) => {
                    add_type_aliases_from_parameter(
                        parameter,
                        syntax_tree,
                        const_env,
                        &mut aliases,
                    );
                }
                _ => {}
            }
        }
    }
    for item in module_scope_items(node) {
        let Some(declaration) = package_or_generate_declaration_from_module_item(item) else {
            continue;
        };
        match declaration {
            sv_parser::PackageOrGenerateItemDeclaration::DataDeclaration(declaration) => {
                add_type_alias_from_data_declaration(
                    declaration,
                    syntax_tree,
                    const_env,
                    &mut aliases,
                )?;
            }
            sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(localparam) => {
                add_type_aliases_from_localparam(
                    &localparam.0,
                    syntax_tree,
                    const_env,
                    &mut aliases,
                );
            }
            sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(parameter) => {
                add_type_aliases_from_parameter(&parameter.0, syntax_tree, const_env, &mut aliases);
            }
            _ => {}
        }
    }
    // Alias dimensions belong to their definition scope, not the scope that
    // later declares a signal of that type. Rebuilt for each specialization.
    // A non-negative bound is written as a plain decimal so that it does not
    // widen the index arithmetic of a run-time select to 128 bits.
    for ty in aliases.values_mut() {
        for range in &mut ty.packed_ranges {
            if let Some(value) = eval_ast_const_expr(&range.left, const_env) {
                range.left = bound_literal(value);
            }
            if let Some(value) = eval_ast_const_expr(&range.right, const_env) {
                range.right = bound_literal(value);
            }
        }
        for range in &mut ty.unpacked_ranges {
            if let Some(value) = eval_ast_const_expr(&range.left, const_env) {
                range.left = bound_literal(value);
            }
            if let Some(value) = eval_ast_const_expr(&range.right, const_env) {
                range.right = bound_literal(value);
            }
            if let Some(value) = range
                .size
                .as_ref()
                .and_then(|size| eval_ast_const_expr(size, const_env))
            {
                range.size = Some(bound_literal(value));
            }
        }
    }
    Ok(aliases)
}

fn bound_literal(value: i128) -> ConstExpr {
    if value >= 0 {
        ConstExpr::Literal(value.to_string())
    } else {
        ConstExpr::Literal(format_typed_parameter_literal(value, 128, true))
    }
}

fn add_type_alias_from_data_declaration(
    declaration: &sv_parser::DataDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    aliases: &mut HashMap<String, Type>,
) -> Result<(), AnalyzerError> {
    let sv_parser::DataDeclaration::TypeDeclaration(declaration) = declaration else {
        return Ok(());
    };
    let sv_parser::TypeDeclaration::DataType(declaration) = &**declaration else {
        return Ok(());
    };
    let Some(name) = identifier_text(RefNode::TypeIdentifier(&declaration.nodes.2), syntax_tree)
    else {
        return Ok(());
    };
    let r#type = if let sv_parser::DataType::Enum(r#enum) = &declaration.nodes.1 {
        if let Some(base) = &r#enum.nodes.1 {
            type_from_ref_node_with_env(
                RefNode::EnumBaseType(base),
                syntax_tree,
                const_env,
                aliases,
            )
            .or_else(|| type_alias_from_ref_node(RefNode::EnumBaseType(base), syntax_tree, aliases))
        } else {
            let mut r#type = Type::new(TypeKind::Bit);
            r#type.is_signed = true;
            r#type.packed_ranges.push(PackedRange::new(
                ConstExpr::Literal("31".to_string()),
                ConstExpr::Literal("0".to_string()),
            ));
            Some(r#type)
        }
    } else {
        type_from_ref_node_with_env(
            RefNode::DataType(&declaration.nodes.1),
            syntax_tree,
            const_env,
            aliases,
        )
        .or_else(|| type_alias_from_data_type(&declaration.nodes.1, syntax_tree, aliases))
    };
    let Some(r#type) = r#type else {
        return Ok(());
    };
    let r#type = type_with_fallback_ranges_with_env(
        r#type,
        RefNode::DataType(&declaration.nodes.1),
        syntax_tree,
        const_env,
        aliases,
    );
    let r#type = type_with_unpacked_ranges(
        r#type,
        unpacked_ranges_from_variable_dimensions_with_env(
            &declaration.nodes.3,
            syntax_tree,
            const_env,
            aliases,
        )?,
    );
    aliases.insert(name, r#type);
    Ok(())
}

fn add_type_aliases_from_parameter_port_list(
    list: &sv_parser::ParameterPortList,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    aliases: &mut HashMap<String, Type>,
) {
    match list {
        sv_parser::ParameterPortList::Assignment(list) => {
            for (_, declaration) in &list.nodes.1.nodes.1.1 {
                add_type_aliases_from_parameter_port_declaration(
                    declaration,
                    syntax_tree,
                    const_env,
                    aliases,
                );
            }
        }
        sv_parser::ParameterPortList::Declaration(list) => {
            for declaration in list.nodes.1.nodes.1.contents() {
                add_type_aliases_from_parameter_port_declaration(
                    declaration,
                    syntax_tree,
                    const_env,
                    aliases,
                );
            }
        }
        sv_parser::ParameterPortList::Empty(_) => {}
    }
}

fn add_type_aliases_from_parameter_port_declaration(
    declaration: &sv_parser::ParameterPortDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    aliases: &mut HashMap<String, Type>,
) {
    match declaration {
        sv_parser::ParameterPortDeclaration::ParameterDeclaration(declaration) => {
            add_type_aliases_from_parameter(declaration, syntax_tree, const_env, aliases);
        }
        sv_parser::ParameterPortDeclaration::LocalParameterDeclaration(declaration) => {
            add_type_aliases_from_localparam(declaration, syntax_tree, const_env, aliases);
        }
        sv_parser::ParameterPortDeclaration::TypeList(list) => {
            for assignment in list.nodes.1.nodes.0.contents() {
                add_type_alias_from_type_assignment(assignment, syntax_tree, const_env, aliases);
            }
        }
        sv_parser::ParameterPortDeclaration::ParamList(_) => {}
    }
}

fn add_type_aliases_from_localparam(
    declaration: &sv_parser::LocalParameterDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    aliases: &mut HashMap<String, Type>,
) {
    let sv_parser::LocalParameterDeclaration::Type(declaration) = declaration else {
        return;
    };
    for assignment in declaration.nodes.2.nodes.0.contents() {
        add_type_alias_from_type_assignment(assignment, syntax_tree, const_env, aliases);
    }
}

fn add_type_aliases_from_parameter(
    declaration: &sv_parser::ParameterDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    aliases: &mut HashMap<String, Type>,
) {
    let sv_parser::ParameterDeclaration::Type(declaration) = declaration else {
        return;
    };
    for assignment in declaration.nodes.2.nodes.0.contents() {
        add_type_alias_from_type_assignment(assignment, syntax_tree, const_env, aliases);
    }
}

fn add_type_alias_from_type_assignment(
    assignment: &sv_parser::TypeAssignment,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    aliases: &mut HashMap<String, Type>,
) {
    let Some((_, data_type)) = &assignment.nodes.1 else {
        return;
    };
    let Some(name) = identifier_text(RefNode::TypeIdentifier(&assignment.nodes.0), syntax_tree)
    else {
        return;
    };
    let Some(r#type) = type_from_ref_node_with_env(
        RefNode::DataType(data_type),
        syntax_tree,
        const_env,
        aliases,
    ) else {
        return;
    };
    aliases.insert(name, r#type);
}

pub(super) fn direction_from_ref_node(node: RefNode<'_>) -> Option<PortDirection> {
    let direction = unwrap_node!(node, PortDirection)?;
    match direction {
        RefNode::PortDirection(direction) => Some(direction_from_port_direction(direction)),
        _ => None,
    }
}

pub(super) fn direction_from_port_direction(direction: &sv_parser::PortDirection) -> PortDirection {
    match direction {
        sv_parser::PortDirection::Input(_) => PortDirection::Input,
        sv_parser::PortDirection::Output(_) => PortDirection::Output,
        sv_parser::PortDirection::Inout(_) => PortDirection::Inout,
        sv_parser::PortDirection::Ref(_) => PortDirection::Ref,
    }
}

pub(super) fn type_from_ref_node(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Option<Type> {
    type_from_ref_node_with_env(node, syntax_tree, &HashMap::default(), &HashMap::default())
}

pub(super) fn type_from_ref_node_with_env(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    if let Some(data) = packed_structs::declaration(node.clone()) {
        return packed_structs::parse_type(data, syntax_tree, const_env, type_aliases);
    }
    if let Some(atom) = integer_atom_expr_type(node.clone()) {
        let kind = if integer_atom_is_2state(node.clone()) {
            TypeKind::Bit
        } else {
            TypeKind::Logic
        };
        let mut r#type = Type::new(kind);
        r#type.is_signed = atom.signed;
        r#type.packed_ranges = vec![PackedRange::new(
            ConstExpr::Literal((atom.width - 1).to_string()),
            ConstExpr::Literal("0".to_string()),
        )];
        return Some(r#type);
    }
    let integer_vector = unwrap_node!(node.clone(), IntegerVectorType)?;
    let kind = match integer_vector {
        RefNode::IntegerVectorType(integer_vector) => match integer_vector {
            sv_parser::IntegerVectorType::Bit(_) => TypeKind::Bit,
            sv_parser::IntegerVectorType::Logic(_) => TypeKind::Logic,
            sv_parser::IntegerVectorType::Reg(_) => TypeKind::Reg,
        },
        _ => return None,
    };
    let mut r#type = Type::new(kind);
    r#type.is_signed = is_signed_from_ref_node(node.clone()).unwrap_or(false);
    r#type.packed_ranges =
        packed_ranges_from_ref_node_with_env(node, syntax_tree, const_env, type_aliases);
    Some(r#type)
}

fn integer_atom_is_2state(node: RefNode<'_>) -> bool {
    matches!(
        unwrap_node!(node, IntegerAtomType),
        Some(RefNode::IntegerAtomType(
            sv_parser::IntegerAtomType::Byte(_)
                | sv_parser::IntegerAtomType::Shortint(_)
                | sv_parser::IntegerAtomType::Int(_)
                | sv_parser::IntegerAtomType::Longint(_)
        ))
    )
}

pub(super) fn type_from_net_port_header(
    header: &sv_parser::NetPortHeaderOrInterfacePortHeader,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    let sv_parser::NetPortHeaderOrInterfacePortHeader::NetPortHeader(header) = header else {
        return None;
    };
    match &header.nodes.1 {
        sv_parser::NetPortType::DataType(data_type) => match &data_type.nodes.1 {
            sv_parser::DataTypeOrImplicit::ImplicitDataType(_) => Some(Type::implicit()),
            sv_parser::DataTypeOrImplicit::DataType(_) => type_from_ref_node_with_env(
                RefNode::DataTypeOrImplicit(&data_type.nodes.1),
                syntax_tree,
                const_env,
                type_aliases,
            )
            .or_else(|| {
                type_alias_from_ref_node(
                    RefNode::DataTypeOrImplicit(&data_type.nodes.1),
                    syntax_tree,
                    type_aliases,
                )
            }),
        },
        sv_parser::NetPortType::NetTypeIdentifier(identifier) => {
            let name = identifier_text(RefNode::NetTypeIdentifier(identifier), syntax_tree)?;
            type_aliases.get(&name).cloned()
        }
        sv_parser::NetPortType::Interconnect(_) => None,
    }
}

pub(super) fn type_from_variable_port_header(
    header: &sv_parser::VariablePortHeader,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    match &header.nodes.1.nodes.0 {
        sv_parser::VarDataType::DataType(data_type) => type_from_ref_node_with_env(
            RefNode::DataType(data_type),
            syntax_tree,
            const_env,
            type_aliases,
        )
        .or_else(|| type_alias_from_data_type(data_type, syntax_tree, type_aliases)),
        sv_parser::VarDataType::Var(data_type) => match &data_type.nodes.1 {
            sv_parser::DataTypeOrImplicit::ImplicitDataType(_) => Some(Type::implicit()),
            sv_parser::DataTypeOrImplicit::DataType(_) => type_from_ref_node_with_env(
                RefNode::DataTypeOrImplicit(&data_type.nodes.1),
                syntax_tree,
                const_env,
                type_aliases,
            )
            .or_else(|| {
                type_alias_from_data_type_or_implicit(&data_type.nodes.1, syntax_tree, type_aliases)
            }),
        },
    }
}

pub(super) fn type_alias_from_data_type_or_implicit(
    data_type: &sv_parser::DataTypeOrImplicit,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    let sv_parser::DataTypeOrImplicit::DataType(data_type) = data_type else {
        return None;
    };
    type_alias_from_data_type(data_type, syntax_tree, type_aliases)
}

pub(super) fn type_alias_from_data_type(
    data_type: &sv_parser::DataType,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    let name = match data_type {
        sv_parser::DataType::Type(data_type) => {
            identifier_text(RefNode::TypeIdentifier(&data_type.nodes.1), syntax_tree)?
        }
        sv_parser::DataType::ClassType(data_type) => {
            identifier_text(RefNode::PsClassIdentifier(&data_type.nodes.0), syntax_tree)?
        }
        _ => return None,
    };
    type_aliases.get(&name).cloned()
}

pub(super) fn type_with_fallback_ranges_with_env(
    mut r#type: Type,
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Type {
    if packed_structs::declaration(node.clone()).is_some() {
        return r#type;
    }
    let direct_ranges =
        packed_ranges_from_ref_node_with_env(node.clone(), syntax_tree, const_env, type_aliases);
    if type_alias_from_ref_node(node.clone(), syntax_tree, type_aliases).is_some() {
        // Use-site dimensions enclose the aliased packed type. The packed
        // array they form is unsigned, whatever its elements (IEEE 1800-2023
        // 7.4.1); a type name takes no signing of its own.
        if !direct_ranges.is_empty() {
            r#type.signed_element_depth = if r#type.is_signed {
                Some(direct_ranges.len())
            } else {
                r#type
                    .signed_element_depth
                    .map(|depth| depth + direct_ranges.len())
            };
            r#type.is_signed = false;
        }
        let mut ranges = direct_ranges;
        ranges.extend(r#type.packed_ranges);
        r#type.packed_ranges = ranges;
        return r#type;
    } else if r#type.packed_ranges.is_empty() {
        r#type.packed_ranges = direct_ranges;
    }
    if !r#type.is_signed && r#type.members.is_empty() {
        r#type.is_signed = is_signed_from_ref_node(node).unwrap_or(false);
    }
    r#type
}

/// The signed element depth of a declaration whose type is `node`: use-site
/// packed dimensions of a type name enclose that type's elements.
pub(super) fn signed_element_depth_from_ref_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
) -> Option<usize> {
    let alias = type_alias_from_ref_node(node.clone(), syntax_tree, type_aliases)?;
    let use_site = node
        .into_iter()
        .filter(|child| matches!(child, RefNode::PackedDimension(_)))
        .count();
    if use_site > 0 && alias.is_signed {
        Some(use_site)
    } else {
        alias.signed_element_depth.map(|depth| depth + use_site)
    }
}

pub(super) fn type_with_unpacked_ranges(mut r#type: Type, ranges: Vec<UnpackedRange>) -> Type {
    let mut unpacked_ranges = ranges;
    unpacked_ranges.extend(r#type.unpacked_ranges);
    r#type.unpacked_ranges = unpacked_ranges;
    r#type
}

pub(super) fn is_signed_from_ref_node(node: RefNode<'_>) -> Option<bool> {
    match unwrap_node!(node, Signing)? {
        RefNode::Signing(signing) => match signing {
            sv_parser::Signing::Signed(_) => Some(true),
            sv_parser::Signing::Unsigned(_) => Some(false),
        },
        _ => None,
    }
}

pub(super) fn packed_ranges_from_ref_node_with_env(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Vec<PackedRange> {
    let mut ranges = Vec::new();
    for child in node {
        if let RefNode::PackedDimensionRange(range) = child {
            let constant_range = &range.nodes.0.nodes.1;
            let left = const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(&constant_range.nodes.0),
                syntax_tree,
                const_env,
                type_aliases,
            );
            let right = const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(&constant_range.nodes.2),
                syntax_tree,
                const_env,
                type_aliases,
            );
            // Validation rejects the bounds this cannot convert.
            if let (Ok(Some(left)), Ok(Some(right))) = (left, right) {
                ranges.push(PackedRange::new(left, right));
            }
        }
    }
    ranges
}

pub(super) fn unpacked_ranges_from_dimensions_with_env(
    dimensions: &[sv_parser::UnpackedDimension],
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<Vec<UnpackedRange>, AnalyzerError> {
    dimensions
        .iter()
        .map(|dimension| match dimension {
            sv_parser::UnpackedDimension::Range(range) => {
                let constant_range = &range.nodes.0.nodes.1;
                let left = const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(&constant_range.nodes.0),
                    syntax_tree,
                    const_env,
                    type_aliases,
                );
                let right = const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(&constant_range.nodes.2),
                    syntax_tree,
                    const_env,
                    type_aliases,
                );
                match (left?, right?) {
                    (Some(left), Some(right)) => Ok(UnpackedRange::new(left, right)),
                    _ => Err(AnalyzerError::Unsupported(
                        "unresolved unpacked array dimension".to_string(),
                    )),
                }
            }
            sv_parser::UnpackedDimension::Expression(expression) => {
                let size = const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(&expression.nodes.0.nodes.1),
                    syntax_tree,
                    const_env,
                    type_aliases,
                );
                size?.map(UnpackedRange::sized).ok_or_else(|| {
                    AnalyzerError::Unsupported("unresolved unpacked array dimension".to_string())
                })
            }
        })
        .collect()
}

pub(super) fn validate_unpacked_dimension_sizes(
    ranges: &[UnpackedRange],
    const_env: &HashMap<String, i128>,
) -> Result<(), AnalyzerError> {
    if ranges.iter().any(|range| {
        range
            .size()
            .and_then(|size| eval_ast_const_expr(size, const_env))
            .is_some_and(|size| size <= 0)
    }) {
        return Err(AnalyzerError::Unsupported(
            "nonpositive unpacked array dimension".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn unpacked_ranges_from_variable_dimensions_with_env(
    dimensions: &[sv_parser::VariableDimension],
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<Vec<UnpackedRange>, AnalyzerError> {
    let mut ranges = Vec::new();
    for dimension in dimensions {
        match dimension {
            sv_parser::VariableDimension::UnpackedDimension(dimension) => {
                ranges.extend(unpacked_ranges_from_dimensions_with_env(
                    std::slice::from_ref(&**dimension),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )?);
            }
            sv_parser::VariableDimension::UnsizedDimension(_) => {
                return Err(AnalyzerError::Unsupported(
                    "unsized unpacked array dimension".to_string(),
                ));
            }
            sv_parser::VariableDimension::AssociativeDimension(_) => {
                return Err(AnalyzerError::Unsupported(
                    "associative array dimension".to_string(),
                ));
            }
            sv_parser::VariableDimension::QueueDimension(_) => {
                return Err(AnalyzerError::Unsupported(
                    "queue array dimension".to_string(),
                ));
            }
        }
    }
    Ok(ranges)
}
