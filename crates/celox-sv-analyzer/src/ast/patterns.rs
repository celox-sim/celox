//! Assignment patterns (IEEE 1800-2023 10.9): `'{a, b}`, `'{i: a, default:
//! b}`, `'{n{a}}`, and typed `T'{...}`, for unpacked arrays, packed structures
//! and packed vectors. A pattern lowers to the concatenation of its elements
//! in the flat bit layout of the target, element 0 of an unpacked dimension
//! (its left bound) in the least significant bits.

use super::*;

/// The assignment pattern an expression consists of, if any.
pub(super) fn pattern_expression(
    expr: &sv_parser::Expression,
) -> Option<&sv_parser::AssignmentPatternExpression> {
    let sv_parser::Expression::Primary(primary) = expr else {
        return None;
    };
    let sv_parser::Primary::AssignmentPatternExpression(pattern) = &**primary else {
        return None;
    };
    Some(pattern)
}

fn eval(expr: &ConstExpr, dims: &PackedDimensions) -> Option<i128> {
    eval_ast_const_expr(expr, &dims.const_env)
}

/// The number of bits of a value of `shape`.
pub(super) fn shape_width(shape: &VariableDimensions, dims: &PackedDimensions) -> Option<usize> {
    let mut width = 1usize;
    for dimension in &shape.unpacked {
        width = width.checked_mul(usize::try_from(eval(&dimension.width, dims)?).ok()?)?;
    }
    for dimension in &shape.packed {
        width = width.checked_mul(usize::try_from(eval(&dimension.width, dims)?).ok()?)?;
    }
    Some(width)
}

/// A typed pattern `T'{...}`: its value as a `T`.
pub(super) fn typed_pattern(
    pattern: &sv_parser::AssignmentPatternExpression,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<Expr> {
    let pattern_type = || unsupported("assignment pattern type");
    let r#type = match pattern.nodes.0.as_ref().ok_or_else(pattern_type)? {
        sv_parser::AssignmentPatternExpressionType::PsTypeIdentifier(identifier) => {
            let name = identifier_text(RefNode::PsTypeIdentifier(identifier), tree)
                .ok_or_else(pattern_type)?;
            dims.type_aliases
                .get(&name)
                .ok_or_else(|| unsupported(format!("assignment pattern type `{name}`")))?
                .clone()
        }
        sv_parser::AssignmentPatternExpressionType::IntegerAtomType(atom) => {
            let atom =
                integer_atom_expr_type(RefNode::IntegerAtomType(atom)).ok_or_else(pattern_type)?;
            let mut r#type = Type::new(TypeKind::Bit);
            r#type.is_signed = atom.signed;
            r#type.packed_ranges = vec![PackedRange::new(
                ConstExpr::Literal((atom.width - 1).to_string()),
                ConstExpr::Literal("0".to_string()),
            )];
            r#type
        }
        _ => return Err(pattern_type()),
    };
    let shape = procedural::dimensions_from_type(&r#type);
    expr_from_pattern(&pattern.nodes.1, &shape, tree, dims)
}

/// The value of one element of a pattern, as a `shape`.
fn element(
    expr: &sv_parser::Expression,
    shape: &VariableDimensions,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<Expr> {
    if let Some(pattern) = pattern_expression(expr) {
        return if pattern.nodes.0.is_some() {
            typed_pattern(pattern, tree, dims)
        } else {
            expr_from_pattern(&pattern.nodes.1, shape, tree, dims)
        };
    }
    let value = expr_from_expression_with_types(expr, tree, dims)?;
    let width =
        shape_width(shape, dims).ok_or_else(|| unsupported("assignment pattern element width"))?;
    let signed =
        expr_signedness(&value, &dims.expression_signedness, &dims.functions).unwrap_or(false);
    Ok(Expr::Resize {
        expr: Box::new(value),
        width,
        signed,
    })
}

/// The value a `default:` key gives an element of `shape`. An unpacked
/// subarray whose type the value does not match is not set as a whole: the
/// default applies recursively to each of its elements (IEEE 1800-2023
/// 10.9.1), each evaluated in the context of that element's type.
fn default_element(
    expr: &sv_parser::Expression,
    shape: &VariableDimensions,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<Expr> {
    let Some((dimension, rest)) = shape.unpacked.split_first() else {
        return element(expr, shape, tree, dims);
    };
    if pattern_expression(expr).is_some() || unpacked_rank(expr, tree, dims) >= shape.unpacked.len()
    {
        return element(expr, shape, tree, dims);
    }
    let (_, _, count) = dimension_bounds(&dimension.left, &dimension.right, dims)?;
    let subarray = VariableDimensions {
        unpacked: rest.to_vec(),
        ..shape.clone()
    };
    let value = default_element(expr, &subarray, tree, dims)?;
    Ok(match count {
        1 => value,
        _ => Expr::Concat(vec![value; count]),
    })
}

/// The number of unpacked dimensions of the self-determined type of `expr`:
/// those of a variable that its selects leave unselected.
fn unpacked_rank(
    expr: &sv_parser::Expression,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> usize {
    let sv_parser::Expression::Primary(primary) = expr else {
        return 0;
    };
    match &**primary {
        sv_parser::Primary::MintypmaxExpression(paren) => match &paren.nodes.0.nodes.1 {
            sv_parser::MintypmaxExpression::Expression(expr) => unpacked_rank(expr, tree, dims),
            sv_parser::MintypmaxExpression::Ternary(_) => 0,
        },
        sv_parser::Primary::Hierarchical(hierarchical) => {
            let select = &hierarchical.nodes.2;
            if select.nodes.0.is_some() {
                // A member of a packed structure.
                return 0;
            }
            let unpacked =
                identifier_text(RefNode::HierarchicalIdentifier(&hierarchical.nodes.1), tree)
                    .and_then(|name| dims.get(&name))
                    .map_or(0, |shape| shape.unpacked.len());
            let indices = select.nodes.1.nodes.0.len();
            match &select.nodes.2 {
                // A slice keeps the dimension it selects from.
                Some(_) if indices < unpacked => unpacked - indices,
                Some(_) => 0,
                None => unpacked.saturating_sub(indices),
            }
        }
        _ => 0,
    }
}

/// The item of a pattern at one position.
#[derive(Clone, Copy)]
enum Item<'p> {
    /// A list, repetition or index-keyed item: the value of the element.
    Explicit(&'p sv_parser::Expression),
    /// A `default:` value, which applies to each element of an unmatched
    /// subarray rather than to the subarray as a whole (IEEE 1800-2023
    /// 10.9.1).
    Default(&'p sv_parser::Expression),
}

impl<'p> Item<'p> {
    fn expression(self) -> &'p sv_parser::Expression {
        match self {
            Self::Explicit(expr) | Self::Default(expr) => expr,
        }
    }
}

/// The items of a pattern for `count` positions, keyed by position: list
/// items in order, `n{...}` repetitions, index keys, and a `default`.
fn positional_items<'p>(
    pattern: &'p sv_parser::AssignmentPattern,
    count: usize,
    index_offset: impl Fn(i128) -> Option<usize>,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<Vec<Item<'p>>> {
    let item_count = |items: usize| {
        unsupported(format!(
            "assignment pattern with {items} items for {count} elements"
        ))
    };
    match pattern {
        sv_parser::AssignmentPattern::List(list) => {
            let items = list.nodes.0.nodes.1.contents();
            if items.len() != count {
                return Err(item_count(items.len()));
            }
            Ok(items.into_iter().map(Item::Explicit).collect())
        }
        sv_parser::AssignmentPattern::Repeat(repeat) => {
            let (times, items) = &repeat.nodes.0.nodes.1;
            let times = const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(times),
                tree,
                &dims.const_env,
                &dims.type_aliases,
            )?
            .and_then(|times| eval(&times, dims))
            .and_then(|times| usize::try_from(times).ok())
            .ok_or_else(|| unsupported("assignment pattern repetition count"))?;
            let items = items.nodes.1.contents();
            let repeated: Vec<_> = (0..times)
                .flat_map(|_| items.iter().copied().map(Item::Explicit))
                .collect();
            if repeated.len() != count {
                return Err(item_count(repeated.len()));
            }
            Ok(repeated)
        }
        sv_parser::AssignmentPattern::Array(array) => {
            let mut slots: Vec<Option<Item>> = vec![None; count];
            let mut default = None;
            for (key, _, value) in array.nodes.0.nodes.1.contents() {
                match key {
                    sv_parser::ArrayPatternKey::ConstantExpression(index) => {
                        let index = const_expr_from_ref_node_with_env(
                            RefNode::ConstantExpression(index),
                            tree,
                            &dims.const_env,
                            &dims.type_aliases,
                        )?
                        .and_then(|index| eval(&index, dims))
                        .ok_or_else(|| unsupported("assignment pattern index key"))?;
                        let slot = index_offset(index)
                            .and_then(|offset| slots.get_mut(offset))
                            .ok_or_else(|| {
                                unsupported(format!(
                                    "assignment pattern index {index} outside the target"
                                ))
                            })?;
                        *slot = Some(Item::Explicit(value));
                    }
                    sv_parser::ArrayPatternKey::AssignmentPatternKey(key) => match &**key {
                        sv_parser::AssignmentPatternKey::Default(_) => {
                            default = Some(Item::Default(value));
                        }
                        sv_parser::AssignmentPatternKey::SimpleType(_) => {
                            return Err(unsupported("assignment pattern type key"));
                        }
                    },
                }
            }
            slots
                .into_iter()
                .enumerate()
                .map(|(position, slot)| {
                    slot.or(default).ok_or_else(|| {
                        unsupported(format!(
                            "assignment pattern without a value for element {position}"
                        ))
                    })
                })
                .collect()
        }
        sv_parser::AssignmentPattern::Structure(structure) => {
            // Only `default:` applies to an array.
            let mut default = None;
            for (key, _, value) in structure.nodes.0.nodes.1.contents() {
                let sv_parser::StructurePatternKey::AssignmentPatternKey(key) = key else {
                    return Err(unsupported("member key in an array assignment pattern"));
                };
                let sv_parser::AssignmentPatternKey::Default(_) = &**key else {
                    return Err(unsupported("assignment pattern type key"));
                };
                default = Some(value);
            }
            let default =
                default.ok_or_else(|| unsupported("array assignment pattern without a default"))?;
            Ok(vec![Item::Default(default); count])
        }
    }
}

/// The left and right bounds of a dimension and its element count.
fn dimension_bounds(
    left: &ConstExpr,
    right: &ConstExpr,
    dims: &PackedDimensions,
) -> Converted<(i128, i128, usize)> {
    let bounds = || unsupported("assignment pattern target bounds");
    let left = eval(left, dims).ok_or_else(bounds)?;
    let right = eval(right, dims).ok_or_else(bounds)?;
    let count = usize::try_from(left.abs_diff(right))
        .ok()
        .and_then(|count| count.checked_add(1))
        .ok_or_else(bounds)?;
    Ok((left, right, count))
}

/// The value of an assignment pattern for a target of `shape`.
pub(super) fn expr_from_pattern(
    pattern: &sv_parser::AssignmentPattern,
    shape: &VariableDimensions,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<Expr> {
    if let Some((dimension, rest)) = shape.unpacked.split_first() {
        // An unpacked array: element 0 is the left bound, in the low bits.
        let (left, right, count) = dimension_bounds(&dimension.left, &dimension.right, dims)?;
        let element_shape = VariableDimensions {
            packed: shape.packed.clone(),
            unpacked: rest.to_vec(),
            signed: shape.signed,
            is_2state: shape.is_2state,
            members: shape.members.clone(),
            signed_element_depth: shape.signed_element_depth,
        };
        let (low, high) = (left.min(right), left.max(right));
        let items = positional_items(
            pattern,
            count,
            |index| {
                (low..=high)
                    .contains(&index)
                    .then(|| usize::try_from(index.abs_diff(left)).ok())
                    .flatten()
            },
            tree,
            dims,
        )?;
        let mut parts = items
            .into_iter()
            .map(|item| match item {
                Item::Explicit(expr) => element(expr, &element_shape, tree, dims),
                Item::Default(expr) => default_element(expr, &element_shape, tree, dims),
            })
            .collect::<Converted<Vec<_>>>()?;
        parts.reverse();
        return Ok(match <[Expr; 1]>::try_from(parts) {
            Ok([part]) => part,
            Err(parts) => Expr::Concat(parts),
        });
    }
    if !shape.members.is_empty() {
        return struct_pattern(pattern, shape, tree, dims);
    }
    if let Some((dimension, rest)) = shape.packed.split_first()
        && !rest.is_empty()
    {
        // A packed array: the left bound is the most significant element.
        let (left, right, count) = dimension_bounds(&dimension.left, &dimension.right, dims)?;
        // An element of a signed named type is signed (IEEE 1800-2023 7.4.1).
        let element_shape = VariableDimensions {
            packed: rest.to_vec(),
            unpacked: Vec::new(),
            signed: shape.signed_element_depth == Some(1),
            is_2state: shape.is_2state,
            members: Vec::new(),
            signed_element_depth: shape
                .signed_element_depth
                .and_then(|depth| depth.checked_sub(1))
                .filter(|depth| *depth > 0),
        };
        let (low, high) = (left.min(right), left.max(right));
        let items = positional_items(
            pattern,
            count,
            |index| {
                (low..=high)
                    .contains(&index)
                    .then(|| usize::try_from(index.abs_diff(left)).ok())
                    .flatten()
            },
            tree,
            dims,
        )?;
        let parts = items
            .into_iter()
            .map(|item| element(item.expression(), &element_shape, tree, dims))
            .collect::<Converted<Vec<_>>>()?;
        return Ok(Expr::Concat(parts));
    }
    // A vector: `'{default: v}` gives every bit the value `v`.
    let width =
        shape_width(shape, dims).ok_or_else(|| unsupported("assignment pattern target width"))?;
    let bit = VariableDimensions {
        packed: Vec::new(),
        unpacked: Vec::new(),
        signed: false,
        is_2state: shape.is_2state,
        members: Vec::new(),
        signed_element_depth: None,
    };
    let items = positional_items(
        pattern,
        width,
        |index| {
            usize::try_from(index)
                .ok()
                .filter(|index| *index < width)
                .map(|index| width - 1 - index)
        },
        tree,
        dims,
    )?;
    let parts = items
        .into_iter()
        .map(|item| element(item.expression(), &bit, tree, dims))
        .collect::<Converted<Vec<_>>>()?;
    Ok(match <[Expr; 1]>::try_from(parts) {
        Ok([part]) => part,
        Err(parts) => Expr::Concat(parts),
    })
}

/// A pattern for a packed structure: its members in declaration order, the
/// first one the most significant.
fn struct_pattern(
    pattern: &sv_parser::AssignmentPattern,
    shape: &VariableDimensions,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<Expr> {
    let members = &shape.members;
    let values: Vec<&sv_parser::Expression> = match pattern {
        sv_parser::AssignmentPattern::List(list) => {
            let values = list.nodes.0.nodes.1.contents();
            if values.len() != members.len() {
                return Err(unsupported(format!(
                    "assignment pattern with {} items for {} structure members",
                    values.len(),
                    members.len()
                )));
            }
            values
        }
        sv_parser::AssignmentPattern::Structure(structure) => {
            let mut default = None;
            let mut named: Vec<(String, &sv_parser::Expression)> = Vec::new();
            for (key, _, value) in structure.nodes.0.nodes.1.contents() {
                match key {
                    sv_parser::StructurePatternKey::MemberIdentifier(member) => named.push((
                        identifier_text(RefNode::MemberIdentifier(member), tree)
                            .ok_or_else(|| unsupported("assignment pattern member name"))?,
                        value,
                    )),
                    sv_parser::StructurePatternKey::AssignmentPatternKey(key) => {
                        let sv_parser::AssignmentPatternKey::Default(_) = &**key else {
                            return Err(unsupported("assignment pattern type key"));
                        };
                        default = Some(value);
                    }
                }
            }
            if let Some((name, _)) = named
                .iter()
                .find(|(name, _)| !members.iter().any(|member| member.name() == name))
            {
                return Err(unsupported(format!(
                    "assignment pattern key `{name}` that is not a structure member"
                )));
            }
            members
                .iter()
                .map(|member| {
                    named
                        .iter()
                        .find(|(name, _)| name == member.name())
                        .map(|(_, value)| *value)
                        .or(default)
                        .ok_or_else(|| {
                            unsupported(format!(
                                "assignment pattern without a value for member `{}`",
                                member.name()
                            ))
                        })
                })
                .collect::<Converted<Vec<_>>>()?
        }
        _ => return Err(unsupported("structure assignment pattern form")),
    };
    let parts = members
        .iter()
        .zip(values)
        .map(|(member, value)| {
            let member_shape = procedural::dimensions_from_type(member.r#type());
            element(value, &member_shape, tree, dims)
        })
        .collect::<Converted<Vec<_>>>()?;
    Ok(match <[Expr; 1]>::try_from(parts) {
        Ok([part]) => part,
        Err(parts) => Expr::Concat(parts),
    })
}
