//! Packed structure declaration types and field layout.

use super::*;

pub(in crate::ast) fn declaration(node: RefNode<'_>) -> Option<&sv_parser::DataTypeStructUnion> {
    match unwrap_node!(node, DataType, DataTypeStructUnion)? {
        RefNode::DataType(sv_parser::DataType::StructUnion(data)) => Some(data),
        RefNode::DataTypeStructUnion(data) => Some(data),
        _ => None,
    }
}

pub(in crate::ast) fn parse_type(
    data: &sv_parser::DataTypeStructUnion,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Option<Type> {
    if !matches!(data.nodes.0, sv_parser::StructUnion::Struct(_)) {
        return None;
    }
    let (_, signing) = data.nodes.1.as_ref()?;
    let mut members = Vec::new();
    let mut names = HashSet::default();
    for member in std::iter::once(&data.nodes.2.nodes.1.0).chain(&data.nodes.2.nodes.1.1) {
        if member.nodes.1.is_some() {
            return None;
        }
        let sv_parser::DataTypeOrVoid::DataType(data_type) = &member.nodes.2 else {
            return None;
        };
        let r#type = type_from_ref_node_with_env(
            RefNode::DataType(data_type),
            syntax_tree,
            const_env,
            aliases,
        )
        .or_else(|| type_alias_from_data_type(data_type, syntax_tree, aliases))?;
        let mut r#type = type_with_fallback_ranges_with_env(
            r#type,
            RefNode::DataType(data_type),
            syntax_tree,
            const_env,
            aliases,
        );
        if !r#type.unpacked_ranges.is_empty() {
            return None;
        }
        // Capture bounds where the type is declared. A generated scope can
        // later shadow a parameter used by a member's packed dimensions.
        for range in &mut r#type.packed_ranges {
            range.left = ConstExpr::Literal(format_typed_parameter_literal(
                eval_ast_const_expr(&range.left, const_env)?,
                128,
                true,
            ));
            range.right = ConstExpr::Literal(format_typed_parameter_literal(
                eval_ast_const_expr(&range.right, const_env)?,
                128,
                true,
            ));
        }
        for assignment in member.nodes.3.nodes.0.contents() {
            let sv_parser::VariableDeclAssignment::Variable(assignment) = assignment else {
                return None;
            };
            if !assignment.nodes.1.is_empty() || assignment.nodes.2.is_some() {
                return None;
            }
            let name = identifier_text(
                RefNode::VariableIdentifier(&assignment.nodes.0),
                syntax_tree,
            )?;
            if !names.insert(name.clone()) {
                return None;
            }
            members.push(PackedMember {
                name,
                offset: 0,
                r#type: r#type.clone(),
            });
        }
    }
    let mut width = 0usize;
    // The first declared member occupies the most significant bits.
    for member in members.iter_mut().rev() {
        member.offset = width;
        width = width.checked_add(expr_type_from_type(&member.r#type, const_env)?.width)?;
    }
    let kind = if members
        .iter()
        .all(|member| member.r#type.kind == TypeKind::Bit)
    {
        TypeKind::Bit
    } else {
        TypeKind::Logic
    };
    let mut r#type = Type::new(kind);
    r#type.is_signed = matches!(signing, Some(sv_parser::Signing::Signed(_)));
    for dimension in &data.nodes.3 {
        let sv_parser::PackedDimension::Range(range) = dimension else {
            return None;
        };
        let range = &range.nodes.0.nodes.1;
        r#type.packed_ranges.push(PackedRange::new(
            const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(&range.nodes.0),
                syntax_tree,
                const_env,
                aliases,
            )?,
            const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(&range.nodes.2),
                syntax_tree,
                const_env,
                aliases,
            )?,
        ));
    }
    r#type.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal(width.checked_sub(1)?.to_string()),
        ConstExpr::Literal("0".into()),
    ));
    r#type.members = members;
    Some(r#type)
}
