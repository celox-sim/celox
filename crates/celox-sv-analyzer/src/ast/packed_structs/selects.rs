//! Packed structure member paths and typed vector selections.

use super::*;

fn hierarchical_path(
    node: RefNode<'_>,
    syntax_tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Option<Vec<String>> {
    let Some(RefNode::HierarchicalIdentifier(identifier)) =
        unwrap_node!(node.clone(), HierarchicalIdentifier)
    else {
        return identifier_text(node, syntax_tree).map(|name| vec![name]);
    };
    if identifier.nodes.0.is_some() {
        return None;
    }
    let mut path = Vec::new();
    for (name, indices, _) in &identifier.nodes.1 {
        let mut name = identifier_text(RefNode::Identifier(name), syntax_tree)?;
        for index in &indices.nodes.0 {
            let value = const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(&index.nodes.1),
                syntax_tree,
                &dimensions.const_env,
                &dimensions.type_aliases,
            )
            .ok()
            .flatten()?;
            let value = eval_ast_const_expr(&syntax_tree.state, &value, &dimensions.const_env)?;
            name.push_str(&format!("[{value}]"));
        }
        path.push(name);
    }
    path.push(identifier_text(
        RefNode::Identifier(&identifier.nodes.2),
        syntax_tree,
    )?);
    Some(path)
}

pub(in crate::ast) fn has_member_access(node: RefNode<'_>, select: RefNode<'_>) -> bool {
    let hierarchy = matches!(unwrap_node!(node, HierarchicalIdentifier),
        Some(RefNode::HierarchicalIdentifier(identifier)) if !identifier.nodes.1.is_empty());
    hierarchy
        || match select {
            RefNode::Select(select) => select.nodes.0.is_some(),
            RefNode::ConstantSelect(select) => select.nodes.0.is_some(),
            _ => false,
        }
}

/// Collapse an interface handle/member prefix using its bound symbols. Any
/// remaining path selects a packed-structure member of the interface signal.
fn bind_interface_path(
    state: &AnalysisState,
    path: &mut Vec<String>,
    dimensions: &PackedDimensions,
) -> bool {
    // A lexical variable wins over an enclosing interface handle, even if its
    // member path is invalid. Never fall back to the interface in that case.
    if path
        .first()
        .is_some_and(|root| dimensions.get(root).is_some())
    {
        return false;
    }
    let imported = scope::imported(state);
    for length in (2..=path.len()).rev() {
        if let Some(name) = imported.interface_members.get(&path[..length].join(".")) {
            path.splice(..length, [name.clone()]);
            return true;
        }
    }
    false
}

fn selected_member<'a>(
    path: &[String],
    dimensions: &'a PackedDimensions,
) -> Option<(usize, &'a Type)> {
    let root = dimensions.get(path.first()?)?;
    if !root.unpacked.is_empty() || root.packed.len() != 1 {
        return None;
    }
    let mut members = root.members.as_slice();
    let mut offset = 0usize;
    let mut selected = None;
    for name in &path[1..] {
        let member = members.iter().find(|member| &member.name == name)?;
        offset = offset.checked_add(member.offset)?;
        selected = Some(&member.r#type);
        members = if member.r#type.packed_ranges.len() == 1 {
            &member.r#type.members
        } else {
            &[]
        };
    }
    Some((offset, selected?))
}

fn member_dimensions(name: &str, r#type: &Type, dimensions: &PackedDimensions) -> PackedDimensions {
    let mut result = dimensions.clone();
    result.insert(
        name.to_string(),
        VariableDimensions {
            packed: function_packed_dimension_widths(&r#type.packed_ranges),
            unpacked: Vec::new(),
            signed: r#type.is_signed,
            is_2state: r#type.kind == TypeKind::Bit,
            members: r#type.members.clone(),
            signed_element_depth: r#type.signed_element_depth,
        },
    );
    result
}

fn finish_lvalue(
    state: &AnalysisState,
    path: &[String],
    offset: usize,
    r#type: &Type,
    relative: LValue,
    dimensions: &PackedDimensions,
) -> Option<LValue> {
    let width = expr_type_from_type(state, r#type, &dimensions.const_env)?.width;
    let (msb, lsb, signed) = match relative {
        LValue::Ident(_) => (width.checked_sub(1)? as i128, 0, r#type.is_signed),
        LValue::Select {
            msb, lsb, signed, ..
        } => (
            eval_ast_const_expr(state, &msb, &dimensions.const_env)?,
            eval_ast_const_expr(state, &lsb, &dimensions.const_env)?,
            signed,
        ),
    };
    // Constant member selects must stay within the member, never hit a neighbor.
    if lsb < 0 || msb < lsb || usize::try_from(msb).ok()? >= width {
        return None;
    }
    Some(LValue::Select {
        name: path[0].clone(),
        msb: ConstExpr::Literal(offset.checked_add(msb as usize)?.to_string()),
        lsb: ConstExpr::Literal(offset.checked_add(lsb as usize)?.to_string()),
        signed,
        array_slice_width: None,
        array_slice_reversed: false,
        is_2state: r#type.kind == TypeKind::Bit,
    })
}

fn variable_path(
    node: RefNode<'_>,
    select: &sv_parser::Select,
    syntax_tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Option<Vec<String>> {
    let mut path = hierarchical_path(node, syntax_tree, dimensions)?;
    if let Some((members, _, last)) = &select.nodes.0 {
        for (_, name, indices) in members {
            if !indices.nodes.0.is_empty() {
                return None;
            }
            path.push(identifier_text(
                RefNode::MemberIdentifier(name),
                syntax_tree,
            )?);
        }
        path.push(identifier_text(
            RefNode::MemberIdentifier(last),
            syntax_tree,
        )?);
    }
    Some(path)
}

pub(in crate::ast) fn member_first_dimension_width(
    node: RefNode<'_>,
    select: &sv_parser::Select,
    syntax_tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Option<usize> {
    let mut path = variable_path(node, select, syntax_tree, dimensions)?;
    bind_interface_path(&syntax_tree.state, &mut path, dimensions);
    let (_, r#type) = selected_member(&path, dimensions)?;
    if let Some(range) = &select.nodes.2 {
        let sv_parser::PartSelectRange::ConstantRange(range) = &range.nodes.1 else {
            return None;
        };
        let bound = |expr| {
            let expr = const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(expr),
                syntax_tree,
                &dimensions.const_env,
                &dimensions.type_aliases,
            )
            .ok()
            .flatten()?;
            eval_ast_const_expr(&syntax_tree.state, &expr, &dimensions.const_env)
        };
        return usize::try_from(bound(&range.nodes.0)?.abs_diff(bound(&range.nodes.2)?))
            .ok()?
            .checked_add(1);
    }
    let index_count = select.nodes.1.nodes.0.len();
    if index_count == r#type.packed_ranges.len() {
        return Some(1);
    }
    let range = r#type.packed_ranges.get(index_count)?;
    let left = eval_ast_const_expr(&syntax_tree.state, range.left(), &dimensions.const_env)?;
    let right = eval_ast_const_expr(&syntax_tree.state, range.right(), &dimensions.const_env)?;
    usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)
}

pub(in crate::ast) fn member_dimension_count(
    node: RefNode<'_>,
    select: &sv_parser::Select,
    syntax_tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Option<usize> {
    let mut path = variable_path(node, select, syntax_tree, dimensions)?;
    bind_interface_path(&syntax_tree.state, &mut path, dimensions);
    let (_, ty) = selected_member(&path, dimensions)?;
    let remaining = ty
        .packed_ranges
        .len()
        .checked_sub(select.nodes.1.nodes.0.len())?;
    if select.nodes.2.is_some() && remaining == 0 {
        return None;
    }
    Some(remaining)
}

/// A member of the structure `node` names. `base`, when given, is the name
/// of that structure, such as the qualified name of a package parameter.
pub(in crate::ast) fn variable_member(
    node: RefNode<'_>,
    base: Option<String>,
    select: &sv_parser::Select,
    syntax_tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<LValue> {
    let mut path = variable_path(node, select, syntax_tree, dimensions)
        .ok_or_else(|| unsupported("member access through an indexed structure"))?;
    let bound_interface = bind_interface_path(&syntax_tree.state, &mut path, dimensions);
    if !bound_interface && let Some(base) = base {
        path[0] = base;
    }
    let mut leaf_select = select.clone();
    leaf_select.nodes.0 = None;
    if bound_interface && path.len() == 1 {
        return super::super::selects::lvalue_from_select(
            path[0].clone(),
            &leaf_select,
            syntax_tree,
            dimensions,
            false,
        );
    }
    let (offset, r#type) = selected_member(&path, dimensions)
        .ok_or_else(|| unsupported(format!("structure member `{}`", path.join("."))))?;
    let relative = super::super::selects::lvalue_from_select(
        path[0].clone(),
        &leaf_select,
        syntax_tree,
        &member_dimensions(&path[0], r#type, dimensions),
        true,
    )?;
    finish_lvalue(
        &syntax_tree.state,
        &path,
        offset,
        r#type,
        relative,
        dimensions,
    )
    .ok_or_else(|| {
        unsupported(format!(
            "select of structure member `{}` outside the member",
            path.join(".")
        ))
    })
}

pub(in crate::ast) fn net_member(
    node: RefNode<'_>,
    select: &sv_parser::ConstantSelect,
    syntax_tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<LValue> {
    let indexed = || unsupported("member access through an indexed structure");
    let mut path = hierarchical_path(node, syntax_tree, dimensions).ok_or_else(indexed)?;
    if let Some((members, _, last)) = &select.nodes.0 {
        for (_, name, indices) in members {
            if !indices.nodes.0.is_empty() {
                return Err(indexed());
            }
            path.push(
                identifier_text(RefNode::MemberIdentifier(name), syntax_tree)
                    .ok_or_else(|| unsupported("structure member name"))?,
            );
        }
        path.push(
            identifier_text(RefNode::MemberIdentifier(last), syntax_tree)
                .ok_or_else(|| unsupported("structure member name"))?,
        );
    }
    let bound_interface = bind_interface_path(&syntax_tree.state, &mut path, dimensions);
    let mut leaf_select = select.clone();
    leaf_select.nodes.0 = None;
    if bound_interface && path.len() == 1 {
        return super::super::selects::lvalue_from_constant_select(
            path[0].clone(),
            &leaf_select,
            syntax_tree,
            dimensions,
            false,
        );
    }
    let (offset, r#type) = selected_member(&path, dimensions)
        .ok_or_else(|| unsupported(format!("structure member `{}`", path.join("."))))?;
    let relative = super::super::selects::lvalue_from_constant_select(
        path[0].clone(),
        &leaf_select,
        syntax_tree,
        &member_dimensions(&path[0], r#type, dimensions),
        true,
    )?;
    finish_lvalue(
        &syntax_tree.state,
        &path,
        offset,
        r#type,
        relative,
        dimensions,
    )
    .ok_or_else(|| {
        unsupported(format!(
            "select of structure member `{}` outside the member",
            path.join(".")
        ))
    })
}
