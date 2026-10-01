//! Lvalue parsing and flattening packed/unpacked selections.

use super::*;

pub(super) fn net_lvalue_from_node(
    node: &sv_parser::NetLvalue,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<LValue> {
    match node {
        sv_parser::NetLvalue::Identifier(identifier) => {
            let node = RefNode::PsOrHierarchicalNetIdentifier(&identifier.nodes.0);
            if packed_structs::has_member_access(
                node.clone(),
                RefNode::ConstantSelect(&identifier.nodes.1),
            ) {
                return packed_structs::net_member(
                    node,
                    &identifier.nodes.1,
                    syntax_tree,
                    packed_dimensions,
                );
            }
            let name = identifier_text(
                RefNode::PsOrHierarchicalNetIdentifier(&identifier.nodes.0),
                syntax_tree,
            )?;
            lvalue_from_constant_select(
                name,
                &identifier.nodes.1,
                syntax_tree,
                packed_dimensions,
                false,
            )
        }
        _ => None,
    }
}

pub(super) fn variable_lvalue_from_node(
    node: &sv_parser::VariableLvalue,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<LValue> {
    match node {
        sv_parser::VariableLvalue::Identifier(identifier) => {
            let node = RefNode::HierarchicalVariableIdentifier(&identifier.nodes.1);
            if packed_structs::has_member_access(node.clone(), RefNode::Select(&identifier.nodes.2))
            {
                return packed_structs::variable_member(
                    node,
                    &identifier.nodes.2,
                    syntax_tree,
                    packed_dimensions,
                );
            }
            let name = identifier_text(
                RefNode::HierarchicalVariableIdentifier(&identifier.nodes.1),
                syntax_tree,
            )?;
            lvalue_from_select(
                name,
                &identifier.nodes.2,
                syntax_tree,
                packed_dimensions,
                false,
            )
        }
        _ => None,
    }
}

fn constant_packed_indices_in_range(
    name: &str,
    indices: &[ConstExpr],
    dimensions: &PackedDimensions,
) -> bool {
    let Some(variable) = dimensions.get(name) else {
        return false;
    };
    if indices.len() > variable.packed.len().max(1) {
        return false;
    }
    indices.iter().enumerate().all(|(position, index)| {
        let Some(index) = eval_ast_const_expr(index, &dimensions.const_env) else {
            return false;
        };
        let Some(dimension) = variable.packed.get(position) else {
            // A scalar integral member can only select bit zero.
            return index == 0;
        };
        let (Some(left), Some(right)) = (
            eval_ast_const_expr(&dimension.left, &dimensions.const_env),
            eval_ast_const_expr(&dimension.right, &dimensions.const_env),
        ) else {
            return false;
        };
        (left.min(right)..=left.max(right)).contains(&index)
    })
}

pub(super) fn lvalue_from_select(
    name: String,
    select: &sv_parser::Select,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    require_constant_in_range: bool,
) -> Option<LValue> {
    let bit_selects = select.nodes.1.nodes.0.as_slice();
    let indices = bit_selects
        .iter()
        .map(|bit_select| {
            let expr = expr_from_expression_with_types(
                &bit_select.nodes.1,
                syntax_tree,
                packed_dimensions,
            )?;
            expr_to_lvalue_const(expand_expr_calls(
                expr,
                &packed_dimensions.functions,
                &packed_dimensions.expression_signedness,
                0,
                true,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    // Struct members cannot use the raw-vector fallback: an invalid index
    // would otherwise become a different bit or the entire member.
    if require_constant_in_range
        && !constant_packed_indices_in_range(&name, &indices, packed_dimensions)
    {
        return None;
    }

    if let Some(range) = &select.nodes.2 {
        let (mut msb, mut lsb) = part_select_bounds(
            &range.nodes.1,
            syntax_tree,
            Some(&name),
            indices.len(),
            packed_dimensions,
        )?;
        let (array_slice_width, array_slice_reversed) =
            unpacked_select_range_metadata(&name, &indices, &msb, &lsb, packed_dimensions)
                .map_or((None, false), |(width, reversed)| (Some(width), reversed));
        (msb, lsb) = flatten_select_range(&name, &indices, msb, lsb, packed_dimensions)?;
        return Some(LValue::Select {
            name,
            msb,
            lsb,
            signed: false,
            array_slice_width,
            array_slice_reversed,
            is_2state: false,
        });
    }

    if let Some((array_offset, packed_indices)) =
        flatten_variable_select(&name, &indices, packed_dimensions)
    {
        if !packed_indices.is_empty() {
            if let Some((msb, lsb)) =
                flatten_packed_select(&name, &packed_indices, packed_dimensions)
            {
                return Some(LValue::Select {
                    name,
                    msb: add_expr(array_offset.clone(), msb),
                    lsb: add_expr(array_offset, lsb),
                    signed: false,
                    array_slice_width: None,
                    array_slice_reversed: false,
                    is_2state: false,
                });
            }
        } else if let Some(dimensions) = packed_dimensions.get(&name)
            && !dimensions.unpacked.is_empty()
            && indices.len() == dimensions.unpacked.len()
        {
            let width = product_expr(
                &dimensions
                    .packed
                    .iter()
                    .map(|dimension| dimension.width.clone())
                    .collect::<Vec<_>>(),
            );
            return Some(LValue::Select {
                name,
                msb: add_expr(
                    array_offset.clone(),
                    ConstExpr::Binary {
                        left: Box::new(width),
                        op: BinaryOp::Sub,
                        right: Box::new(ConstExpr::Literal("1".to_string())),
                    },
                ),
                lsb: array_offset,
                signed: dimensions.signed,
                array_slice_width: None,
                array_slice_reversed: false,
                is_2state: false,
            });
        } else if let Some(dimensions) = packed_dimensions.get(&name)
            && !dimensions.unpacked.is_empty()
            && !indices.is_empty()
            && indices.len() < dimensions.unpacked.len()
        {
            let width = remaining_unpacked_selection_width(dimensions, indices.len());
            return Some(LValue::Select {
                name,
                msb: add_expr(
                    array_offset.clone(),
                    ConstExpr::Binary {
                        left: Box::new(width),
                        op: BinaryOp::Sub,
                        right: Box::new(ConstExpr::Literal("1".to_string())),
                    },
                ),
                lsb: array_offset,
                signed: dimensions.signed,
                array_slice_width: None,
                array_slice_reversed: false,
                is_2state: false,
            });
        }
    }
    if !indices.is_empty()
        && packed_dimensions
            .get(&name)
            .is_some_and(|dimensions| !dimensions.unpacked.is_empty())
    {
        return None;
    }
    if indices.len() == 1 {
        let bit = indices[0].clone();
        return Some(LValue::Select {
            name,
            msb: bit.clone(),
            lsb: bit,
            signed: false,
            array_slice_width: None,
            array_slice_reversed: false,
            is_2state: false,
        });
    }

    Some(LValue::Ident(name))
}

pub(super) fn lvalue_from_constant_select(
    name: String,
    select: &sv_parser::ConstantSelect,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    require_constant_in_range: bool,
) -> Option<LValue> {
    let bit_selects = select.nodes.1.nodes.0.as_slice();
    let indices = bit_selects
        .iter()
        .map(|bit_select| {
            if packed_dimensions.constant_indexed_base {
                indexed_select_base(
                    RefNode::ConstantExpression(&bit_select.nodes.1),
                    syntax_tree,
                    packed_dimensions,
                )
            } else {
                const_expr_from_ref_node(
                    RefNode::ConstantExpression(&bit_select.nodes.1),
                    syntax_tree,
                )
            }
        })
        .collect::<Option<Vec<_>>>()?;
    // Struct members cannot use the raw-vector fallback: an invalid index
    // would otherwise become a different bit or the entire member.
    if require_constant_in_range
        && !constant_packed_indices_in_range(&name, &indices, packed_dimensions)
    {
        return None;
    }

    if let Some(range) = &select.nodes.2 {
        let bound = |expression| {
            if packed_dimensions.constant_indexed_base {
                indexed_select_base(
                    RefNode::ConstantExpression(expression),
                    syntax_tree,
                    packed_dimensions,
                )
            } else {
                const_expr_from_ref_node(RefNode::ConstantExpression(expression), syntax_tree)
            }
        };
        let (mut msb, mut lsb) = match &range.nodes.1 {
            sv_parser::ConstantPartSelectRange::ConstantRange(range) => {
                (bound(&range.nodes.0)?, bound(&range.nodes.2)?)
            }
            sv_parser::ConstantPartSelectRange::ConstantIndexedRange(range) => {
                indexed_select_bounds(
                    indexed_select_base(
                        RefNode::ConstantExpression(&range.nodes.0),
                        syntax_tree,
                        packed_dimensions,
                    )?,
                    &range.nodes.1,
                    &range.nodes.2,
                    syntax_tree,
                    (Some(&name), indices.len()),
                    packed_dimensions,
                )?
            }
        };
        let (array_slice_width, array_slice_reversed) =
            unpacked_select_range_metadata(&name, &indices, &msb, &lsb, packed_dimensions)
                .map_or((None, false), |(width, reversed)| (Some(width), reversed));
        (msb, lsb) = flatten_select_range(&name, &indices, msb, lsb, packed_dimensions)?;
        return Some(LValue::Select {
            name,
            msb,
            lsb,
            signed: false,
            array_slice_width,
            array_slice_reversed,
            is_2state: false,
        });
    }

    if let Some((array_offset, packed_indices)) =
        flatten_variable_select(&name, &indices, packed_dimensions)
    {
        if !packed_indices.is_empty() {
            if let Some((msb, lsb)) =
                flatten_packed_select(&name, &packed_indices, packed_dimensions)
            {
                return Some(LValue::Select {
                    name,
                    msb: add_expr(array_offset.clone(), msb),
                    lsb: add_expr(array_offset, lsb),
                    signed: false,
                    array_slice_width: None,
                    array_slice_reversed: false,
                    is_2state: false,
                });
            }
        } else if let Some(dimensions) = packed_dimensions.get(&name)
            && !dimensions.unpacked.is_empty()
            && indices.len() == dimensions.unpacked.len()
        {
            let width = product_expr(
                &dimensions
                    .packed
                    .iter()
                    .map(|dimension| dimension.width.clone())
                    .collect::<Vec<_>>(),
            );
            return Some(LValue::Select {
                name,
                msb: add_expr(
                    array_offset.clone(),
                    ConstExpr::Binary {
                        left: Box::new(width),
                        op: BinaryOp::Sub,
                        right: Box::new(ConstExpr::Literal("1".to_string())),
                    },
                ),
                lsb: array_offset,
                signed: dimensions.signed,
                array_slice_width: None,
                array_slice_reversed: false,
                is_2state: false,
            });
        } else if let Some(dimensions) = packed_dimensions.get(&name)
            && !dimensions.unpacked.is_empty()
            && !indices.is_empty()
            && indices.len() < dimensions.unpacked.len()
        {
            let width = remaining_unpacked_selection_width(dimensions, indices.len());
            return Some(LValue::Select {
                name,
                msb: add_expr(
                    array_offset.clone(),
                    ConstExpr::Binary {
                        left: Box::new(width),
                        op: BinaryOp::Sub,
                        right: Box::new(ConstExpr::Literal("1".to_string())),
                    },
                ),
                lsb: array_offset,
                signed: dimensions.signed,
                array_slice_width: None,
                array_slice_reversed: false,
                is_2state: false,
            });
        }
    }
    if !indices.is_empty()
        && packed_dimensions
            .get(&name)
            .is_some_and(|dimensions| !dimensions.unpacked.is_empty())
    {
        return None;
    }
    if indices.len() == 1 {
        let bit = indices[0].clone();
        return Some(LValue::Select {
            name,
            msb: bit.clone(),
            lsb: bit,
            signed: false,
            array_slice_width: None,
            array_slice_reversed: false,
            is_2state: false,
        });
    }

    Some(LValue::Ident(name))
}

pub(super) fn expr_select_from_select(
    base: Expr,
    select: &sv_parser::Select,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    let bit_selects = select.nodes.1.nodes.0.as_slice();
    let indices = bit_selects
        .iter()
        .map(|bit_select| {
            if packed_dimensions.constant_indexed_base {
                indexed_select_base(
                    RefNode::Expression(&bit_select.nodes.1),
                    syntax_tree,
                    packed_dimensions,
                )
            } else {
                const_expr_from_expr(&bit_select.nodes.1, syntax_tree)
            }
        })
        .collect::<Option<Vec<_>>>()?;
    if let Some(range) = &select.nodes.2 {
        let name = if let Expr::Ident(name) = &base {
            Some(name.as_str())
        } else {
            None
        };
        let (mut msb, mut lsb) = part_select_bounds(
            &range.nodes.1,
            syntax_tree,
            name,
            indices.len(),
            packed_dimensions,
        )?;
        let Expr::Ident(name) = &base else {
            return indices.is_empty().then_some(Expr::Select {
                expr: Box::new(base),
                msb,
                lsb,
                signed: false,
            });
        };
        if let Some(expr) = expr_from_unpacked_select_range(
            name,
            &indices,
            msb.clone(),
            lsb.clone(),
            packed_dimensions,
        ) {
            return Some(expr);
        }
        (msb, lsb) = flatten_select_range(name, &indices, msb, lsb, packed_dimensions)?;
        return Some(Expr::Select {
            expr: Box::new(base),
            msb,
            lsb,
            signed: false,
        });
    }

    if let Expr::Ident(name) = &base {
        if let Some((array_offset, packed_indices)) =
            flatten_variable_select(name, &indices, packed_dimensions)
        {
            if !packed_indices.is_empty()
                && let Some((msb, lsb)) =
                    flatten_packed_select(name, &packed_indices, packed_dimensions)
            {
                return Some(Expr::Select {
                    expr: Box::new(base),
                    msb: add_expr(array_offset.clone(), msb),
                    lsb: add_expr(array_offset, lsb),
                    signed: false,
                });
            }
            if let Some(dimensions) = packed_dimensions.get(name)
                && !dimensions.unpacked.is_empty()
                && indices.len() == dimensions.unpacked.len()
            {
                let width = product_expr(
                    &dimensions
                        .packed
                        .iter()
                        .map(|dimension| dimension.width.clone())
                        .collect::<Vec<_>>(),
                );
                return Some(Expr::Select {
                    expr: Box::new(base),
                    msb: add_expr(
                        array_offset.clone(),
                        ConstExpr::Binary {
                            left: Box::new(width),
                            op: BinaryOp::Sub,
                            right: Box::new(ConstExpr::Literal("1".to_string())),
                        },
                    ),
                    lsb: array_offset,
                    signed: dimensions.signed,
                });
            }
            if let Some(dimensions) = packed_dimensions.get(name)
                && !dimensions.unpacked.is_empty()
                && packed_indices.is_empty()
                && indices.len() < dimensions.unpacked.len()
            {
                let width = remaining_unpacked_selection_width(dimensions, indices.len());
                return Some(Expr::Select {
                    expr: Box::new(base),
                    msb: add_expr(
                        array_offset.clone(),
                        ConstExpr::Binary {
                            left: Box::new(width),
                            op: BinaryOp::Sub,
                            right: Box::new(ConstExpr::Literal("1".to_string())),
                        },
                    ),
                    lsb: array_offset,
                    signed: dimensions.signed,
                });
            }
        }
    }
    if indices.len() == 1 {
        if let Expr::Ident(name) = &base
            && packed_dimensions
                .get(name)
                .is_some_and(|dimensions| !dimensions.unpacked.is_empty())
        {
            return None;
        }
        let bit = indices[0].clone();
        return Some(Expr::Select {
            expr: Box::new(base),
            msb: bit.clone(),
            lsb: bit,
            signed: false,
        });
    }

    None
}

fn flatten_variable_select(
    name: &str,
    indices: &[ConstExpr],
    packed_dimensions: &PackedDimensions,
) -> Option<(ConstExpr, Vec<ConstExpr>)> {
    let dimensions = packed_dimensions.get(name)?;
    let unpacked_count = dimensions.unpacked.len();
    let selected_unpacked_count = indices.len().min(unpacked_count);

    let mut offset = ConstExpr::Literal("0".to_string());
    let mut valid = None;
    for (index, value) in indices[..selected_unpacked_count].iter().enumerate() {
        if const_expr_is_out_of_range(
            value,
            &dimensions.unpacked[index].left,
            &dimensions.unpacked[index].right,
            &packed_dimensions.const_env,
        ) {
            return None;
        }
        let dimension = &dimensions.unpacked[index];
        let descending = ConstExpr::Binary {
            left: Box::new(dimension.left.clone()),
            op: BinaryOp::Ge,
            right: Box::new(dimension.right.clone()),
        };
        let descending_valid = ConstExpr::Binary {
            left: Box::new(ConstExpr::Binary {
                left: Box::new(value.clone()),
                op: BinaryOp::Ge,
                right: Box::new(dimension.right.clone()),
            }),
            op: BinaryOp::LogicAnd,
            right: Box::new(ConstExpr::Binary {
                left: Box::new(value.clone()),
                op: BinaryOp::Le,
                right: Box::new(dimension.left.clone()),
            }),
        };
        let ascending_valid = ConstExpr::Binary {
            left: Box::new(ConstExpr::Binary {
                left: Box::new(value.clone()),
                op: BinaryOp::Ge,
                right: Box::new(dimension.left.clone()),
            }),
            op: BinaryOp::LogicAnd,
            right: Box::new(ConstExpr::Binary {
                left: Box::new(value.clone()),
                op: BinaryOp::Le,
                right: Box::new(dimension.right.clone()),
            }),
        };
        let dimension_valid = ConstExpr::Mux {
            condition: Box::new(descending),
            then_expr: Box::new(descending_valid),
            else_expr: Box::new(ascending_valid),
        };
        valid = Some(match valid {
            Some(previous) => ConstExpr::Binary {
                left: Box::new(previous),
                op: BinaryOp::LogicAnd,
                right: Box::new(dimension_valid),
            },
            None => dimension_valid,
        });
        let mut stride_parts = dimensions.unpacked[index + 1..]
            .iter()
            .map(|dimension| dimension.width.clone())
            .collect::<Vec<_>>();
        stride_parts.extend(
            dimensions
                .packed
                .iter()
                .map(|dimension| dimension.width.clone()),
        );
        let stride = product_expr(&stride_parts);
        let index = unpacked_index_offset(&dimensions.unpacked[index], value.clone());
        let term = if is_one(&stride) {
            index
        } else {
            ConstExpr::Binary {
                left: Box::new(index),
                op: BinaryOp::Mul,
                right: Box::new(stride),
            }
        };
        offset = add_expr(offset, term);
    }
    if let Some(valid) = valid {
        let mut total_width_parts = dimensions
            .unpacked
            .iter()
            .map(|dimension| dimension.width.clone())
            .collect::<Vec<_>>();
        total_width_parts.extend(
            dimensions
                .packed
                .iter()
                .map(|dimension| dimension.width.clone()),
        );
        offset = ConstExpr::Mux {
            condition: Box::new(valid),
            then_expr: Box::new(offset),
            else_expr: Box::new(product_expr(&total_width_parts)),
        };
    }
    Some((offset, indices[selected_unpacked_count..].to_vec()))
}

fn expr_from_unpacked_select_range(
    name: &str,
    indices: &[ConstExpr],
    msb: ConstExpr,
    lsb: ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    let dimensions = packed_dimensions.get(name)?;
    let dimension = dimensions.unpacked.get(indices.len())?;
    if const_expr_is_out_of_range(
        &msb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) || const_expr_is_out_of_range(
        &lsb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) {
        return None;
    }
    let (array_offset, packed_indices) = flatten_variable_select(name, indices, packed_dimensions)?;
    if !packed_indices.is_empty() {
        return None;
    }
    let msb_value = eval_ast_const_expr(&msb, &packed_dimensions.const_env)?;
    let lsb_value = eval_ast_const_expr(&lsb, &packed_dimensions.const_env)?;
    let stride = product_expr(
        &dimensions.unpacked[indices.len() + 1..]
            .iter()
            .map(|dimension| dimension.width.clone())
            .chain(
                dimensions
                    .packed
                    .iter()
                    .map(|dimension| dimension.width.clone()),
            )
            .collect::<Vec<_>>(),
    );
    let stride_minus_one = ConstExpr::Binary {
        left: Box::new(stride.clone()),
        op: BinaryOp::Sub,
        right: Box::new(ConstExpr::Literal("1".to_string())),
    };
    // Concat parts are ordered from most-significant to least-significant.
    // Walking from the slice lsb toward its msb preserves the array element
    // order when the slice direction is reversed.
    let mut parts = Vec::new();
    let mut value = lsb_value;
    let step = if value <= msb_value { 1 } else { -1 };
    loop {
        let offset = unpacked_index_offset(dimension, ConstExpr::Literal(value.to_string()));
        let offset = if is_one(&stride) {
            offset
        } else {
            ConstExpr::Binary {
                left: Box::new(offset),
                op: BinaryOp::Mul,
                right: Box::new(stride.clone()),
            }
        };
        let msb = add_expr(
            array_offset.clone(),
            add_expr(offset.clone(), stride_minus_one.clone()),
        );
        let lsb = add_expr(array_offset.clone(), offset);
        parts.push(Expr::Select {
            expr: Box::new(Expr::Ident(name.to_string())),
            msb,
            lsb,
            signed: dimensions.signed,
        });
        if value == msb_value {
            break;
        }
        value = value.checked_add(step)?;
    }
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Expr::Concat(parts)),
    }
}

fn unpacked_select_range_metadata(
    name: &str,
    indices: &[ConstExpr],
    msb: &ConstExpr,
    lsb: &ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> Option<(ConstExpr, bool)> {
    let dimensions = packed_dimensions.get(name)?;
    let dimension = dimensions.unpacked.get(indices.len())?;
    if const_expr_is_out_of_range(
        msb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) || const_expr_is_out_of_range(
        lsb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) {
        return None;
    }
    let (_, packed_indices) = flatten_variable_select(name, indices, packed_dimensions)?;
    if !packed_indices.is_empty() {
        return None;
    }
    let msb_offset = eval_ast_const_expr(
        &unpacked_index_offset(dimension, msb.clone()),
        &packed_dimensions.const_env,
    )?;
    let lsb_offset = eval_ast_const_expr(
        &unpacked_index_offset(dimension, lsb.clone()),
        &packed_dimensions.const_env,
    )?;
    let stride = product_expr(
        &dimensions.unpacked[indices.len() + 1..]
            .iter()
            .map(|dimension| dimension.width.clone())
            .chain(
                dimensions
                    .packed
                    .iter()
                    .map(|dimension| dimension.width.clone()),
            )
            .collect::<Vec<_>>(),
    );
    Some((stride, msb_offset > lsb_offset))
}

fn remaining_unpacked_selection_width(
    dimensions: &VariableDimensions,
    selected_unpacked_count: usize,
) -> ConstExpr {
    let mut parts = dimensions.unpacked[selected_unpacked_count..]
        .iter()
        .map(|dimension| dimension.width.clone())
        .collect::<Vec<_>>();
    parts.extend(
        dimensions
            .packed
            .iter()
            .map(|dimension| dimension.width.clone()),
    );
    product_expr(&parts)
}

fn flatten_unpacked_select_range(
    name: &str,
    indices: &[ConstExpr],
    msb: ConstExpr,
    lsb: ConstExpr,
    dimensions: &VariableDimensions,
    packed_dimensions: &PackedDimensions,
) -> Option<(ConstExpr, ConstExpr)> {
    let dimension = dimensions.unpacked.get(indices.len())?;
    if const_expr_is_out_of_range(
        &msb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) || const_expr_is_out_of_range(
        &lsb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) {
        return None;
    }
    let (array_offset, packed_indices) = flatten_variable_select(name, indices, packed_dimensions)?;
    if !packed_indices.is_empty() {
        return None;
    }
    let msb_offset = unpacked_index_offset(dimension, msb);
    let lsb_offset = unpacked_index_offset(dimension, lsb);
    let stride = product_expr(
        &dimensions.unpacked[indices.len() + 1..]
            .iter()
            .map(|dimension| dimension.width.clone())
            .chain(
                dimensions
                    .packed
                    .iter()
                    .map(|dimension| dimension.width.clone()),
            )
            .collect::<Vec<_>>(),
    );
    let scale = |offset: ConstExpr| {
        if is_one(&stride) {
            offset
        } else {
            ConstExpr::Binary {
                left: Box::new(offset),
                op: BinaryOp::Mul,
                right: Box::new(stride.clone()),
            }
        }
    };
    let stride_minus_one = ConstExpr::Binary {
        left: Box::new(stride.clone()),
        op: BinaryOp::Sub,
        right: Box::new(ConstExpr::Literal("1".to_string())),
    };
    let descending = ConstExpr::Binary {
        left: Box::new(msb_offset.clone()),
        op: BinaryOp::Ge,
        right: Box::new(lsb_offset.clone()),
    };
    let high = ConstExpr::Mux {
        condition: Box::new(descending.clone()),
        then_expr: Box::new(add_expr(
            scale(msb_offset.clone()),
            stride_minus_one.clone(),
        )),
        else_expr: Box::new(add_expr(scale(lsb_offset.clone()), stride_minus_one)),
    };
    let low = ConstExpr::Mux {
        condition: Box::new(descending),
        then_expr: Box::new(scale(lsb_offset)),
        else_expr: Box::new(scale(msb_offset)),
    };
    Some((
        add_expr(array_offset.clone(), high),
        add_expr(array_offset, low),
    ))
}

fn flatten_select_range(
    name: &str,
    indices: &[ConstExpr],
    mut msb: ConstExpr,
    mut lsb: ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> Option<(ConstExpr, ConstExpr)> {
    let Some(dimensions) = packed_dimensions.get(name) else {
        return indices.is_empty().then_some((msb, lsb));
    };
    if indices.is_empty()
        && dimensions.unpacked.is_empty()
        && dimensions.packed.len() == 1
        && !dimensions.packed[0].normalize_single
    {
        return Some((msb, lsb));
    }
    if indices.len() < dimensions.unpacked.len() {
        return flatten_unpacked_select_range(
            name,
            indices,
            msb,
            lsb,
            dimensions,
            packed_dimensions,
        );
    }
    let (array_offset, packed_indices) = flatten_variable_select(name, indices, packed_dimensions)?;
    let dimension = dimensions.packed.get(packed_indices.len())?;
    if const_expr_is_out_of_range(
        &msb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) || const_expr_is_out_of_range(
        &lsb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) {
        return None;
    }
    msb = packed_index_offset(dimension, msb);
    lsb = packed_index_offset(dimension, lsb);

    let stride = product_expr(
        &dimensions.packed[packed_indices.len() + 1..]
            .iter()
            .map(|dimension| dimension.width.clone())
            .collect::<Vec<_>>(),
    );
    if !is_one(&stride) {
        msb = add_expr(
            ConstExpr::Binary {
                left: Box::new(msb),
                op: BinaryOp::Mul,
                right: Box::new(stride.clone()),
            },
            ConstExpr::Binary {
                left: Box::new(stride.clone()),
                op: BinaryOp::Sub,
                right: Box::new(ConstExpr::Literal("1".to_string())),
            },
        );
        lsb = ConstExpr::Binary {
            left: Box::new(lsb),
            op: BinaryOp::Mul,
            right: Box::new(stride),
        };
    }

    let prefix_offset = if packed_indices.is_empty() {
        ConstExpr::Literal("0".to_string())
    } else {
        let (_, offset) = flatten_packed_select(name, &packed_indices, packed_dimensions)?;
        offset
    };
    let offset = add_expr(array_offset, prefix_offset);
    Some((add_expr(offset.clone(), msb), add_expr(offset, lsb)))
}

fn flatten_packed_select(
    name: &str,
    indices: &[ConstExpr],
    packed_dimensions: &PackedDimensions,
) -> Option<(ConstExpr, ConstExpr)> {
    let variable_dimensions = packed_dimensions.get(name)?;
    let dimensions = &variable_dimensions.packed;
    if dimensions.is_empty() {
        if variable_dimensions.unpacked.is_empty() || indices.len() != 1 {
            return None;
        }
        let zero = ConstExpr::Literal("0".to_string());
        if eval_ast_const_expr(&indices[0], &packed_dimensions.const_env) != Some(0) {
            return None;
        }
        return Some((zero.clone(), zero));
    }
    if indices.is_empty()
        || indices.len() > dimensions.len()
        || (variable_dimensions.unpacked.is_empty()
            && dimensions.len() == 1
            && !dimensions[0].normalize_single)
    {
        return None;
    }

    let mut offset = ConstExpr::Literal("0".to_string());
    for (idx, index) in indices.iter().enumerate() {
        let dimension = &dimensions[idx];
        if const_expr_is_out_of_range(
            index,
            &dimension.left,
            &dimension.right,
            &packed_dimensions.const_env,
        ) {
            return None;
        }
        let stride = product_expr(
            &dimensions[idx + 1..]
                .iter()
                .map(|dimension| dimension.width.clone())
                .collect::<Vec<_>>(),
        );
        let index = packed_index_offset(dimension, index.clone());
        let term = if is_one(&stride) {
            index
        } else {
            ConstExpr::Binary {
                left: Box::new(index),
                op: BinaryOp::Mul,
                right: Box::new(stride),
            }
        };
        offset = add_expr(offset, term);
    }

    let remaining_width = product_expr(
        &dimensions[indices.len()..]
            .iter()
            .map(|dimension| dimension.width.clone())
            .collect::<Vec<_>>(),
    );
    if is_one(&remaining_width) {
        return Some((offset.clone(), offset));
    }
    let msb = ConstExpr::Binary {
        left: Box::new(offset.clone()),
        op: BinaryOp::Add,
        right: Box::new(ConstExpr::Binary {
            left: Box::new(remaining_width),
            op: BinaryOp::Sub,
            right: Box::new(ConstExpr::Literal("1".to_string())),
        }),
    };
    Some((msb, offset))
}

pub(super) fn packed_index_offset(dimension: &PackedDimension, index: ConstExpr) -> ConstExpr {
    ConstExpr::Mux {
        condition: Box::new(ConstExpr::Binary {
            left: Box::new(dimension.left.clone()),
            op: BinaryOp::Ge,
            right: Box::new(dimension.right.clone()),
        }),
        then_expr: Box::new(ConstExpr::Binary {
            left: Box::new(index.clone()),
            op: BinaryOp::Sub,
            right: Box::new(dimension.right.clone()),
        }),
        else_expr: Box::new(ConstExpr::Binary {
            left: Box::new(dimension.right.clone()),
            op: BinaryOp::Sub,
            right: Box::new(index),
        }),
    }
}

fn unpacked_index_offset(dimension: &UnpackedDimension, index: ConstExpr) -> ConstExpr {
    ConstExpr::Mux {
        condition: Box::new(ConstExpr::Binary {
            left: Box::new(dimension.left.clone()),
            op: BinaryOp::Ge,
            right: Box::new(dimension.right.clone()),
        }),
        then_expr: Box::new(ConstExpr::Binary {
            left: Box::new(dimension.left.clone()),
            op: BinaryOp::Sub,
            right: Box::new(index.clone()),
        }),
        else_expr: Box::new(ConstExpr::Binary {
            left: Box::new(index),
            op: BinaryOp::Sub,
            right: Box::new(dimension.left.clone()),
        }),
    }
}

fn const_expr_is_out_of_range(
    index: &ConstExpr,
    left: &ConstExpr,
    right: &ConstExpr,
    const_env: &HashMap<String, i128>,
) -> bool {
    let (Some(index), Some(left), Some(right)) = (
        eval_ast_const_expr(index, const_env),
        eval_ast_const_expr(left, const_env),
        eval_ast_const_expr(right, const_env),
    ) else {
        return false;
    };
    index < left.min(right) || index > left.max(right)
}

pub(super) fn product_expr(parts: &[ConstExpr]) -> ConstExpr {
    parts
        .iter()
        .cloned()
        .reduce(|left, right| ConstExpr::Binary {
            left: Box::new(left),
            op: BinaryOp::Mul,
            right: Box::new(right),
        })
        .unwrap_or_else(|| ConstExpr::Literal("1".to_string()))
}

pub(super) fn add_expr(left: ConstExpr, right: ConstExpr) -> ConstExpr {
    if is_zero(&left) {
        right
    } else if is_zero(&right) {
        left
    } else {
        ConstExpr::Binary {
            left: Box::new(left),
            op: BinaryOp::Add,
            right: Box::new(right),
        }
    }
}

fn is_zero(expr: &ConstExpr) -> bool {
    matches!(expr, ConstExpr::Literal(value) if value == "0")
}

fn is_one(expr: &ConstExpr) -> bool {
    matches!(expr, ConstExpr::Literal(value) if value == "1")
}

// IEEE 1800-2023 11.5.1: +:/-: specify an index direction, while the
// declaration determines which endpoint is more significant. Reuse ordinary
// range flattening after recovering those endpoints.
pub(super) fn part_select_bounds(
    range: &sv_parser::PartSelectRange,
    syntax_tree: &SyntaxTree,
    name: Option<&str>,
    index_count: usize,
    dimensions: &PackedDimensions,
) -> Option<(ConstExpr, ConstExpr)> {
    let bound = |expression| {
        if dimensions.constant_indexed_base {
            indexed_select_base(
                RefNode::ConstantExpression(expression),
                syntax_tree,
                dimensions,
            )
        } else {
            const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(expression),
                syntax_tree,
                &dimensions.const_env,
                &dimensions.type_aliases,
            )
        }
    };
    match range {
        sv_parser::PartSelectRange::ConstantRange(range) => {
            Some((bound(&range.nodes.0)?, bound(&range.nodes.2)?))
        }
        sv_parser::PartSelectRange::IndexedRange(range) => indexed_select_bounds(
            indexed_select_base(RefNode::Expression(&range.nodes.0), syntax_tree, dimensions)?,
            &range.nodes.1,
            &range.nodes.2,
            syntax_tree,
            (name, index_count),
            dimensions,
        ),
    }
}

fn indexed_select_bounds(
    base: ConstExpr,
    operator: &sv_parser::Symbol,
    width: &sv_parser::ConstantExpression,
    syntax_tree: &SyntaxTree,
    selection: (Option<&str>, usize),
    dimensions: &PackedDimensions,
) -> Option<(ConstExpr, ConstExpr)> {
    let (name, index_count) = selection;
    let width = indexed_select_base(RefNode::ConstantExpression(width), syntax_tree, dimensions)?;
    let width = eval_ast_const_expr(&width, &dimensions.const_env)?;
    if width <= 0 {
        return None;
    }
    let ascending = name
        .and_then(|name| dimensions.get(name))
        .and_then(|dims| {
            let (left, right) = if let Some(dim) = dims.unpacked.get(index_count) {
                (&dim.left, &dim.right)
            } else {
                let dim = dims
                    .packed
                    .get(index_count.checked_sub(dims.unpacked.len())?)?;
                (&dim.left, &dim.right)
            };
            Some(
                eval_ast_const_expr(left, &dimensions.const_env)?
                    < eval_ast_const_expr(right, &dimensions.const_env)?,
            )
        })
        .unwrap_or(false);
    let plus = syntax_tree.get_str(&operator.nodes.0)? == "+:";
    // The base is self-determined; endpoint arithmetic must not inherit its
    // unsigned type or truncate a carry across the base expression's width.
    let base = eval_ast_const_expr(&base, &dimensions.const_env)?;
    let offset = width.checked_sub(1)?;
    let other = if plus {
        base.checked_add(offset)?
    } else {
        base.checked_sub(offset)?
    };
    let base = const_expr_from_i128(base);
    let other = const_expr_from_i128(other);
    Some(if plus == ascending {
        (base, other)
    } else {
        (other, base)
    })
}

// Use the complete typed expression path: the lightweight constant-expression
// parser can discard selections, and cannot resolve typedef casts.
pub(super) fn indexed_select_base(
    base: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    dimensions: &PackedDimensions,
) -> Option<ConstExpr> {
    let mut dimensions = dimensions.clone();
    dimensions.constant_indexed_base = true;
    match base {
        RefNode::Expression(base) => {
            let expression = expr_from_expression_with_types(base, syntax_tree, &dimensions)?;
            // Keep both mux arms until typed evaluation determines their common width.
            let expression = substitute_indexed_parameter_values(expression, &dimensions);
            expr_to_const(fold_const_integral_expr_preserving_mask(
                expression,
                &dimensions.const_env,
            ))
        }
        RefNode::ConstantExpression(base) => {
            let expression = indexed_constant_expression(
                RefNode::ConstantExpression(base),
                syntax_tree,
                &dimensions,
            )?;
            let expression = substitute_indexed_parameter_values(expression, &dimensions);
            expr_to_const(fold_const_integral_expr_preserving_mask(
                expression,
                &dimensions.const_env,
            ))
        }
        _ => None,
    }
}

fn substitute_indexed_parameter_values(expression: Expr, dimensions: &PackedDimensions) -> Expr {
    // Known values come from the current environment, which may include a
    // generate index shadowing a parameter. Supply expressions only for values
    // absent from that numeric environment, including X/Z parameters.
    let values = dimensions
        .parameter_values
        .iter()
        .filter(|(name, _)| !dimensions.const_env.contains_key(*name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    substitute_expr_constants_with_parameter_literals(expression, &dimensions.const_env, &values)
}

// Parameter arithmetic receives the assignment width after self-determined
// selection operands have been folded, rather than the index expression context.
pub(super) fn indexed_parameter_initializer(
    initializer: &sv_parser::ConstantParamExpression,
    syntax_tree: &SyntaxTree,
    dimensions: &PackedDimensions,
    assignment_width: Option<usize>,
) -> Option<ConstExpr> {
    let sv_parser::ConstantParamExpression::ConstantMintypmaxExpression(initializer) = initializer
    else {
        return None;
    };
    let sv_parser::ConstantMintypmaxExpression::Unary(expression) = &**initializer else {
        return None;
    };
    let mut dimensions = dimensions.clone();
    dimensions.constant_indexed_base = true;
    let expression = indexed_constant_expression(
        RefNode::ConstantExpression(expression),
        syntax_tree,
        &dimensions,
    )?;
    let expression = substitute_indexed_parameter_values(expression, &dimensions);
    let types = parameter_types_from_const_env(&dimensions.const_env);
    let integral_types = types
        .iter()
        .map(|(name, ty)| (name.clone(), (ty.width, ty.signed)))
        .collect();
    let constant = constant_folding::constant_with_folded_selections(
        &expression,
        &dimensions.const_env,
        &integral_types,
    )?;
    let expression_type = infer_const_expr_type(&constant, &types)?;
    let literal = typecheck::eval_generate_case_operand(
        &constant.into(),
        &dimensions.const_env,
        &integral_types,
        assignment_width.unwrap_or(0).max(expression_type.width),
        expression_type.signed,
    )?;
    Some(ConstExpr::Literal(
        typecheck::format_integral_literal_binary(&literal),
    ))
}

fn indexed_constant_expression(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    dimensions: &PackedDimensions,
) -> Option<Expr> {
    let convert = |node| indexed_constant_expression(node, syntax_tree, dimensions);
    match node {
        RefNode::ConstantExpression(expression) => match expression {
            sv_parser::ConstantExpression::ConstantPrimary(primary) => {
                convert(RefNode::ConstantPrimary(primary))
            }
            sv_parser::ConstantExpression::Unary(unary) => unary_expr_from_symbol(
                &unary.nodes.0.nodes.0.nodes.0,
                convert(RefNode::ConstantPrimary(&unary.nodes.2))?,
                syntax_tree,
            ),
            sv_parser::ConstantExpression::Binary(binary) => {
                let grouped = matches!(&binary.nodes.3, sv_parser::ConstantExpression::ConstantPrimary(primary)
                    if matches!(&**primary, sv_parser::ConstantPrimary::MintypmaxExpression(_)));
                let expression = Expr::Binary {
                    left: Box::new(convert(RefNode::ConstantExpression(&binary.nodes.0))?),
                    op: binary_op_from_symbol(&binary.nodes.1.nodes.0.nodes.0, syntax_tree)?,
                    right: Box::new(convert(RefNode::ConstantExpression(&binary.nodes.3))?),
                };
                Some(if grouped {
                    expression
                } else {
                    left_associate_expr_binary(expression)
                })
            }
            sv_parser::ConstantExpression::Ternary(ternary) => Some(Expr::Mux {
                condition: Box::new(convert(RefNode::ConstantExpression(&ternary.nodes.0))?),
                then_expr: Box::new(convert(RefNode::ConstantExpression(&ternary.nodes.3))?),
                else_expr: Box::new(convert(RefNode::ConstantExpression(&ternary.nodes.5))?),
            }),
            _ => None,
        },
        RefNode::ConstantPrimary(primary) => match primary {
            sv_parser::ConstantPrimary::PsParameter(parameter) => {
                // Preserve all selections and normalize against declared ranges.
                if parameter.nodes.1.nodes.0.is_some() {
                    return None;
                }
                let name = identifier_text(
                    RefNode::PsParameterIdentifier(&parameter.nodes.0),
                    syntax_tree,
                )?;
                let selected = lvalue_from_constant_select(
                    name,
                    &parameter.nodes.1,
                    syntax_tree,
                    dimensions,
                    false,
                )?;
                Some(expr_from_lvalue(&selected, dimensions))
            }
            sv_parser::ConstantPrimary::MintypmaxExpression(grouped) => {
                match &grouped.nodes.0.nodes.1 {
                    sv_parser::ConstantMintypmaxExpression::Unary(expression) => {
                        convert(RefNode::ConstantExpression(expression))
                    }
                    _ => None,
                }
            }
            sv_parser::ConstantPrimary::ConstantFunctionCall(call) => {
                // The parser can represent a bare genvar as a no-argument call.
                if matches!(&call.nodes.0.nodes.0, sv_parser::SubroutineCall::TfCall(call)
                    if call.nodes.2.is_none())
                {
                    return const_expr_from_ref_node_with_env(
                        node,
                        syntax_tree,
                        &dimensions.const_env,
                        &dimensions.type_aliases,
                    )
                    .map(const_expr_to_expr);
                }
                if let Some(ty) = size_system_function_expr_type(
                    primary,
                    syntax_tree,
                    &dimensions.const_env,
                    &dimensions.type_aliases,
                ) {
                    return Some(Expr::Literal(ty.width.to_string()));
                }
                // Function arguments need the same typed selection lowering as bases.
                expr_from_function_subroutine_call(&call.nodes.0, syntax_tree, dimensions)
            }
            sv_parser::ConstantPrimary::ConstantCast(cast) => {
                let operand = indexed_select_base(
                    RefNode::ConstantExpression(&cast.nodes.2.nodes.1),
                    syntax_tree,
                    dimensions,
                )?;
                casts::cast_constant_operand(
                    operand,
                    &cast.nodes.0,
                    syntax_tree,
                    &dimensions.const_env,
                    &dimensions.type_aliases,
                )
                .map(const_expr_to_expr)
            }
            sv_parser::ConstantPrimary::Concatenation(concat) if concat.nodes.1.is_none() => {
                Some(Expr::Concat(
                    concat
                        .nodes
                        .0
                        .nodes
                        .0
                        .nodes
                        .1
                        .contents()
                        .into_iter()
                        .map(|expression| convert(RefNode::ConstantExpression(expression)))
                        .collect::<Option<Vec<_>>>()?,
                ))
            }
            _ => const_expr_from_ref_node_with_env(
                node,
                syntax_tree,
                &dimensions.const_env,
                &dimensions.type_aliases,
            )
            .map(const_expr_to_expr),
        },
        _ => None,
    }
}
