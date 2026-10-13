//! Validate skipped assignment-pattern shapes without expanding their values.

use super::*;

pub(in crate::ast) fn validate_skipped_patterns(
    initializer: &sv_parser::ConstantParamExpression,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<()> {
    let patterns: Vec<_> = initializer
        .into_iter()
        .filter_map(|node| match node {
            RefNode::AssignmentPattern(pattern) => Some(pattern),
            _ => None,
        })
        .collect();
    if patterns.is_empty() {
        return Ok(());
    }
    let mut checked = HashSet::default();
    let mut call_shapes = None;
    for node in initializer.into_iter() {
        match node {
            RefNode::AssignmentPatternExpression(pattern) if pattern.nodes.0.is_some() => {
                let shape =
                    procedural::dimensions_from_type(&typed_pattern_type(pattern, tree, dims)?);
                validate_shape(&pattern.nodes.1, &shape, tree, dims, &mut checked)?;
            }
            RefNode::Cast(cast) => {
                if let Some(pattern) = pattern_expression(&cast.nodes.2.nodes.1) {
                    let shape = cast_shape(&cast.nodes.0, tree, dims)?;
                    validate_element(pattern, &shape, tree, dims, &mut checked)?;
                }
            }
            RefNode::ConstantCast(cast) => {
                if let sv_parser::ConstantExpression::ConstantPrimary(primary) =
                    &cast.nodes.2.nodes.1
                    && let sv_parser::ConstantPrimary::ConstantAssignmentPatternExpression(pattern) =
                        &**primary
                {
                    let shape = cast_shape(&cast.nodes.0, tree, dims)?;
                    validate_element(&pattern.nodes.0, &shape, tree, dims, &mut checked)?;
                }
            }
            RefNode::TfCall(call) if call.nodes.2.is_some() => {
                let name =
                    reference_name(RefNode::PsOrHierarchicalTfIdentifier(&call.nodes.0), tree)
                        .ok_or_else(|| unsupported("pattern argument function name"))?;
                let (bindings, _) = expressions::bind_call_arguments(
                    &name,
                    call.nodes.2.as_ref().map(|paren| &paren.nodes.1),
                    tree,
                    &const_functions::argument_names(),
                )?;
                for (position, argument) in bindings.into_iter().enumerate() {
                    let Some(pattern) = argument.and_then(pattern_expression) else {
                        continue;
                    };
                    if pattern.nodes.0.is_some() {
                        continue;
                    }
                    let shapes = call_shapes.get_or_insert_with(|| {
                        let mut shapes = dimensions::containing_subroutine_param_shapes(
                            node.clone(),
                            tree,
                            &dims.const_env,
                            &dims.type_aliases,
                        );
                        shapes.extend(
                            dims.subroutine_param_shapes
                                .iter()
                                .map(|(name, shapes)| (name.clone(), shapes.clone())),
                        );
                        shapes
                    });
                    let shape = shapes
                        .get(&name)
                        .and_then(|shapes| shapes.get(position))
                        .ok_or_else(|| unsupported("assignment pattern argument target"))?;
                    validate_element(pattern, shape, tree, dims, &mut checked)?;
                }
            }
            _ => {}
        }
    }
    for pattern in patterns {
        if !checked.contains(&(pattern as *const sv_parser::AssignmentPattern)) {
            return Err(unsupported("assignment pattern without a target type"));
        }
    }
    Ok(())
}

fn cast_shape(
    target: &sv_parser::CastingType,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<VariableDimensions> {
    // Retain aggregate metadata for named types; a numeric size cast supplies
    // just a packed vector shape.
    if let Some(name) = reference_name(RefNode::CastingType(target), tree)
        && let Some(ty) = dims.type_aliases.get(&name)
    {
        return Ok(procedural::dimensions_from_type(ty));
    }
    let ty = casts::cast_target_type(target, tree, &dims.const_env, &dims.type_aliases)
        .ok_or_else(|| unsupported("assignment pattern cast target"))?;
    let mut target = Type::new(TypeKind::Logic);
    target.packed_ranges = vec![PackedRange::new(
        ConstExpr::Literal((ty.width - 1).to_string()),
        ConstExpr::Literal("0".into()),
    )];
    Ok(procedural::dimensions_from_type(&target))
}

fn validate_element(
    pattern: &sv_parser::AssignmentPatternExpression,
    shape: &VariableDimensions,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
    checked: &mut HashSet<*const sv_parser::AssignmentPattern>,
) -> Converted<()> {
    if pattern.nodes.0.is_some() {
        let shape = procedural::dimensions_from_type(&typed_pattern_type(pattern, tree, dims)?);
        validate_shape(&pattern.nodes.1, &shape, tree, dims, checked)
    } else {
        validate_shape(&pattern.nodes.1, shape, tree, dims, checked)
    }
}

fn validate_shape(
    pattern: &sv_parser::AssignmentPattern,
    shape: &VariableDimensions,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
    checked: &mut HashSet<*const sv_parser::AssignmentPattern>,
) -> Converted<()> {
    checked.insert(pattern as *const sv_parser::AssignmentPattern);
    if shape.unpacked.is_empty() && !shape.members.is_empty() {
        if shape.members.len() > 1 && shape.members.iter().all(|member| member.offset() == 0) {
            return Err(unsupported("assignment pattern for a packed union"));
        }
        let values = struct_pattern_values(pattern, shape, tree)?;
        for (member, value) in shape.members.iter().zip(values) {
            if let Some(pattern) = pattern_expression(value) {
                let member_shape = procedural::dimensions_from_type(member.r#type());
                validate_element(pattern, &member_shape, tree, dims, checked)?;
            }
        }
        return Ok(());
    }
    let (left, right, count, element) =
        if let Some((dimension, rest)) = shape.unpacked.split_first() {
            let (left, right, count) = dimension_bounds(&dimension.left, &dimension.right, dims)?;
            (
                left,
                right,
                count,
                VariableDimensions {
                    unpacked: rest.to_vec(),
                    ..shape.clone()
                },
            )
        } else if let Some((dimension, rest)) = shape.packed.split_first()
            && !rest.is_empty()
        {
            let (left, right, count) = dimension_bounds(&dimension.left, &dimension.right, dims)?;
            (
                left,
                right,
                count,
                VariableDimensions {
                    packed: rest.to_vec(),
                    ..shape.clone()
                },
            )
        } else {
            let width = shape_width(shape, dims)
                .ok_or_else(|| unsupported("assignment pattern target width"))?;
            (
                width as i128 - 1,
                0,
                width,
                VariableDimensions {
                    packed: Vec::new(),
                    unpacked: Vec::new(),
                    members: Vec::new(),
                    signed: false,
                    is_2state: shape.is_2state,
                    signed_element_depth: None,
                },
            )
        };
    let values = sparse_items(pattern, count, left.min(right), left.max(right), tree, dims)?;
    for value in values {
        if let Some(pattern) = pattern_expression(value) {
            validate_element(pattern, &element, tree, dims, checked)?;
        }
    }
    Ok(())
}

/// Return each source item once. Counts and coverage need no per-target slot
/// allocation, and a repeated payload needs only one recursive shape check.
fn sparse_items<'p>(
    pattern: &'p sv_parser::AssignmentPattern,
    count: usize,
    low: i128,
    high: i128,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<Vec<&'p sv_parser::Expression>> {
    let wrong_count = || unsupported("assignment pattern items do not match the target shape");
    match pattern {
        sv_parser::AssignmentPattern::List(list) => {
            let values = list.nodes.0.nodes.1.contents();
            if values.len() != count {
                return Err(wrong_count());
            }
            Ok(values)
        }
        sv_parser::AssignmentPattern::Repeat(repeat) => {
            let (times, items) = &repeat.nodes.0.nodes.1;
            let times =
                selects::indexed_select_base(RefNode::ConstantExpression(times), tree, dims)?;
            let times = eval(&times, dims)
                .and_then(|times| usize::try_from(times).ok())
                .ok_or_else(|| unsupported("assignment pattern repetition count"))?;
            let values = items.nodes.1.contents();
            if times.checked_mul(values.len()) != Some(count) {
                return Err(wrong_count());
            }
            Ok(values)
        }
        sv_parser::AssignmentPattern::Array(array) => {
            let mut indices = HashSet::default();
            let mut default = false;
            let mut values = Vec::new();
            for (key, _, value) in array.nodes.0.nodes.1.contents() {
                match key {
                    sv_parser::ArrayPatternKey::ConstantExpression(index) => {
                        let index = selects::indexed_select_base(
                            RefNode::ConstantExpression(index),
                            tree,
                            dims,
                        )?;
                        let index = eval(&index, dims)
                            .ok_or_else(|| unsupported("assignment pattern index key"))?;
                        if !(low..=high).contains(&index) {
                            return Err(unsupported("assignment pattern index outside the target"));
                        }
                        indices.insert(index);
                    }
                    sv_parser::ArrayPatternKey::AssignmentPatternKey(key) => {
                        if !matches!(&**key, sv_parser::AssignmentPatternKey::Default(_)) {
                            return Err(unsupported("assignment pattern type key"));
                        }
                        default = true;
                    }
                }
                values.push(value);
            }
            if !default && indices.len() != count {
                return Err(wrong_count());
            }
            Ok(values)
        }
        sv_parser::AssignmentPattern::Structure(structure) => {
            let mut values = Vec::new();
            for (key, _, value) in structure.nodes.0.nodes.1.contents() {
                if !matches!(key, sv_parser::StructurePatternKey::AssignmentPatternKey(key)
                    if matches!(&**key, sv_parser::AssignmentPatternKey::Default(_)))
                {
                    return Err(unsupported(
                        "member or type key in an array assignment pattern",
                    ));
                }
                values.push(value);
            }
            if values.is_empty() {
                return Err(wrong_count());
            }
            Ok(values)
        }
    }
}
