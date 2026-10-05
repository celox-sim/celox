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
) -> Option<Expr> {
    let r#type = match pattern.nodes.0.as_ref()? {
        sv_parser::AssignmentPatternExpressionType::PsTypeIdentifier(identifier) => {
            let name = identifier_text(RefNode::PsTypeIdentifier(identifier), tree)?;
            dims.type_aliases.get(&name)?.clone()
        }
        sv_parser::AssignmentPatternExpressionType::IntegerAtomType(atom) => {
            let atom = integer_atom_expr_type(RefNode::IntegerAtomType(atom))?;
            let mut r#type = Type::new(TypeKind::Bit);
            r#type.is_signed = atom.signed;
            r#type.packed_ranges = vec![PackedRange::new(
                ConstExpr::Literal((atom.width - 1).to_string()),
                ConstExpr::Literal("0".to_string()),
            )];
            r#type
        }
        _ => return None,
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
) -> Option<Expr> {
    if let Some(pattern) = pattern_expression(expr) {
        return if pattern.nodes.0.is_some() {
            typed_pattern(pattern, tree, dims)
        } else {
            expr_from_pattern(&pattern.nodes.1, shape, tree, dims)
        };
    }
    let value = expr_from_expression_with_types(expr, tree, dims)?;
    let width = shape_width(shape, dims)?;
    let signed =
        expr_signedness(&value, &dims.expression_signedness, &dims.functions).unwrap_or(false);
    Some(Expr::Resize {
        expr: Box::new(value),
        width,
        signed,
    })
}

/// The items of a pattern for `count` positions, keyed by position: list
/// items in order, `n{...}` repetitions, index keys, and a `default`.
fn positional_items<'p>(
    pattern: &'p sv_parser::AssignmentPattern,
    count: usize,
    index_offset: impl Fn(i128) -> Option<usize>,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Option<Vec<&'p sv_parser::Expression>> {
    match pattern {
        sv_parser::AssignmentPattern::List(list) => {
            let items = list.nodes.0.nodes.1.contents();
            (items.len() == count).then_some(items)
        }
        sv_parser::AssignmentPattern::Repeat(repeat) => {
            let (times, items) = &repeat.nodes.0.nodes.1;
            let times = const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(times),
                tree,
                &dims.const_env,
                &dims.type_aliases,
            )
            .and_then(|times| eval(&times, dims))?;
            let items = items.nodes.1.contents();
            let repeated: Vec<_> = (0..usize::try_from(times).ok()?)
                .flat_map(|_| items.iter().copied())
                .collect();
            (repeated.len() == count).then_some(repeated)
        }
        sv_parser::AssignmentPattern::Array(array) => {
            let mut slots: Vec<Option<&sv_parser::Expression>> = vec![None; count];
            let mut default = None;
            for (key, _, value) in array.nodes.0.nodes.1.contents() {
                match key {
                    sv_parser::ArrayPatternKey::ConstantExpression(index) => {
                        let index = const_expr_from_ref_node_with_env(
                            RefNode::ConstantExpression(index),
                            tree,
                            &dims.const_env,
                            &dims.type_aliases,
                        )
                        .and_then(|index| eval(&index, dims))?;
                        *slots.get_mut(index_offset(index)?)? = Some(value);
                    }
                    sv_parser::ArrayPatternKey::AssignmentPatternKey(key) => match &**key {
                        sv_parser::AssignmentPatternKey::Default(_) => default = Some(value),
                        sv_parser::AssignmentPatternKey::SimpleType(_) => return None,
                    },
                }
            }
            slots.into_iter().map(|slot| slot.or(default)).collect()
        }
        sv_parser::AssignmentPattern::Structure(structure) => {
            // Only `default:` applies to an array.
            let mut default = None;
            for (key, _, value) in structure.nodes.0.nodes.1.contents() {
                let sv_parser::StructurePatternKey::AssignmentPatternKey(key) = key else {
                    return None;
                };
                let sv_parser::AssignmentPatternKey::Default(_) = &**key else {
                    return None;
                };
                default = Some(value);
            }
            let default = default?;
            Some(vec![default; count])
        }
    }
}

/// The value of an assignment pattern for a target of `shape`.
pub(super) fn expr_from_pattern(
    pattern: &sv_parser::AssignmentPattern,
    shape: &VariableDimensions,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Option<Expr> {
    if let Some((dimension, rest)) = shape.unpacked.split_first() {
        // An unpacked array: element 0 is the left bound, in the low bits.
        let left = eval(&dimension.left, dims)?;
        let right = eval(&dimension.right, dims)?;
        let count = usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?;
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
            .map(|item| element(item, &element_shape, tree, dims))
            .collect::<Option<Vec<_>>>()?;
        parts.reverse();
        return Some(match <[Expr; 1]>::try_from(parts) {
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
        let left = eval(&dimension.left, dims)?;
        let right = eval(&dimension.right, dims)?;
        let count = usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?;
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
            .map(|item| element(item, &element_shape, tree, dims))
            .collect::<Option<Vec<_>>>()?;
        return Some(Expr::Concat(parts));
    }
    // A vector: `'{default: v}` gives every bit the value `v`.
    let width = shape_width(shape, dims)?;
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
        .map(|item| element(item, &bit, tree, dims))
        .collect::<Option<Vec<_>>>()?;
    Some(match <[Expr; 1]>::try_from(parts) {
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
) -> Option<Expr> {
    let members = &shape.members;
    let values: Vec<&sv_parser::Expression> = match pattern {
        sv_parser::AssignmentPattern::List(list) => {
            let values = list.nodes.0.nodes.1.contents();
            if values.len() != members.len() {
                return None;
            }
            values
        }
        sv_parser::AssignmentPattern::Structure(structure) => {
            let mut default = None;
            let mut named: Vec<(String, &sv_parser::Expression)> = Vec::new();
            for (key, _, value) in structure.nodes.0.nodes.1.contents() {
                match key {
                    sv_parser::StructurePatternKey::MemberIdentifier(member) => named.push((
                        identifier_text(RefNode::MemberIdentifier(member), tree)?,
                        value,
                    )),
                    sv_parser::StructurePatternKey::AssignmentPatternKey(key) => {
                        let sv_parser::AssignmentPatternKey::Default(_) = &**key else {
                            return None;
                        };
                        default = Some(value);
                    }
                }
            }
            if named
                .iter()
                .any(|(name, _)| !members.iter().any(|member| member.name() == name))
            {
                return None;
            }
            members
                .iter()
                .map(|member| {
                    named
                        .iter()
                        .find(|(name, _)| name == member.name())
                        .map(|(_, value)| *value)
                        .or(default)
                })
                .collect::<Option<Vec<_>>>()?
        }
        _ => return None,
    };
    let parts = members
        .iter()
        .zip(values)
        .map(|(member, value)| {
            let member_shape = procedural::dimensions_from_type(member.r#type());
            element(value, &member_shape, tree, dims)
        })
        .collect::<Option<Vec<_>>>()?;
    Some(match <[Expr; 1]>::try_from(parts) {
        Ok([part]) => part,
        Err(parts) => Expr::Concat(parts),
    })
}
