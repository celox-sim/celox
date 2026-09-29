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
            lvalue_from_constant_select(name, &identifier.nodes.1, syntax_tree, packed_dimensions)
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
            lvalue_from_select(name, &identifier.nodes.2, syntax_tree, packed_dimensions)
        }
        _ => None,
    }
}

pub(super) fn lvalue_from_select(
    name: String,
    select: &sv_parser::Select,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
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
    if let Some(range) = &select.nodes.2 {
        let sv_parser::PartSelectRange::ConstantRange(range) = &range.nodes.1 else {
            return None;
        };
        let mut msb =
            const_expr_from_ref_node(RefNode::ConstantExpression(&range.nodes.0), syntax_tree)?;
        let mut lsb =
            const_expr_from_ref_node(RefNode::ConstantExpression(&range.nodes.2), syntax_tree)?;
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
) -> Option<LValue> {
    let bit_selects = select.nodes.1.nodes.0.as_slice();
    let indices = bit_selects
        .iter()
        .map(|bit_select| {
            const_expr_from_ref_node(
                RefNode::ConstantExpression(&bit_select.nodes.1),
                syntax_tree,
            )
        })
        .collect::<Option<Vec<_>>>()?;
    if let Some(range) = &select.nodes.2 {
        let sv_parser::ConstantPartSelectRange::ConstantRange(range) = &range.nodes.1 else {
            return None;
        };
        let mut msb =
            const_expr_from_ref_node(RefNode::ConstantExpression(&range.nodes.0), syntax_tree)?;
        let mut lsb =
            const_expr_from_ref_node(RefNode::ConstantExpression(&range.nodes.2), syntax_tree)?;
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
        .map(|bit_select| const_expr_from_expr(&bit_select.nodes.1, syntax_tree))
        .collect::<Option<Vec<_>>>()?;
    if let Some(range) = &select.nodes.2 {
        let sv_parser::PartSelectRange::ConstantRange(range) = &range.nodes.1 else {
            return None;
        };
        let mut msb =
            const_expr_from_ref_node(RefNode::ConstantExpression(&range.nodes.0), syntax_tree)?;
        let mut lsb =
            const_expr_from_ref_node(RefNode::ConstantExpression(&range.nodes.2), syntax_tree)?;
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
