//! Lvalue parsing and flattening packed/unpacked selections.

use super::*;

pub(super) fn net_lvalue_from_node(
    node: &sv_parser::NetLvalue,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<LValue> {
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
            )
            .ok_or_else(|| unsupported("net assignment target name"))?;
            lvalue_from_constant_select(
                name,
                &identifier.nodes.1,
                syntax_tree,
                packed_dimensions,
                false,
            )
        }
        sv_parser::NetLvalue::Lvalue(_) => Err(unsupported("concatenated net assignment target")),
        sv_parser::NetLvalue::Pattern(_) => Err(unsupported("assignment pattern net target")),
    }
}

pub(super) fn variable_lvalue_from_node(
    node: &sv_parser::VariableLvalue,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<LValue> {
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
            )
            .ok_or_else(|| unsupported("variable assignment target name"))?;
            lvalue_from_select(
                name,
                &identifier.nodes.2,
                syntax_tree,
                packed_dimensions,
                false,
            )
        }
        sv_parser::VariableLvalue::Lvalue(_) => {
            Err(unsupported("concatenated variable assignment target"))
        }
        sv_parser::VariableLvalue::Pattern(_) => {
            Err(unsupported("assignment pattern variable target"))
        }
        sv_parser::VariableLvalue::StreamingConcatenation(_) => {
            Err(unsupported("streaming concatenation assignment target"))
        }
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

fn has_indexed_selection(node: RefNode<'_>) -> bool {
    node.into_iter().any(|node| {
        matches!(
            node,
            RefNode::IndexedRange(_) | RefNode::ConstantIndexedRange(_)
        )
    })
}

/// Why a select of `name` with constant `indices` and, for a part-select,
/// `bounds` has no representation.
fn select_error(
    name: &str,
    indices: &[ConstExpr],
    bounds: Option<(&ConstExpr, &ConstExpr)>,
    dimensions: &PackedDimensions,
) -> AnalyzerError {
    let what = if bounds.is_some() {
        "part-select"
    } else {
        "select"
    };
    let Some(variable) = dimensions.get(name) else {
        return unsupported(format!("{what} of `{name}`"));
    };
    let const_env = &dimensions.const_env;
    let ranges = variable
        .unpacked
        .iter()
        .map(|range| (&range.left, &range.right))
        .chain(
            variable
                .packed
                .iter()
                .map(|range| (&range.left, &range.right)),
        );
    for (position, (index, (left, right))) in indices.iter().zip(ranges.clone()).enumerate() {
        if const_expr_is_out_of_range(index, left, right, const_env) {
            return unsupported(format!(
                "index {} of `{name}` outside its declared range",
                position + 1
            ));
        }
    }
    if let Some((msb, lsb)) = bounds
        && let Some((left, right)) = ranges.clone().nth(indices.len())
        && (const_expr_is_out_of_range(msb, left, right, const_env)
            || const_expr_is_out_of_range(lsb, left, right, const_env))
    {
        return unsupported(format!(
            "part-select of `{name}` outside its declared range"
        ));
    }
    unsupported(format!("{what} of `{name}`"))
}

pub(super) fn bit_select_index(
    expression: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    dimensions: &PackedDimensions,
) -> Converted<ConstExpr> {
    if dimensions.constant_indexed_base || has_indexed_selection(RefNode::Expression(expression)) {
        indexed_select_base(RefNode::Expression(expression), syntax_tree, dimensions)
    } else {
        const_expr_from_expr(expression, syntax_tree)?.ok_or_else(|| {
            unsupported(format!(
                "select index `{}`",
                node_source_text(RefNode::Expression(expression), syntax_tree).unwrap_or_default()
            ))
        })
    }
}

pub(super) fn lvalue_from_select(
    name: String,
    select: &sv_parser::Select,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    require_constant_in_range: bool,
) -> Converted<LValue> {
    let bit_selects = select.nodes.1.nodes.0.as_slice();
    let indices = bit_selects
        .iter()
        .map(|bit_select| {
            if has_indexed_selection(RefNode::Expression(&bit_select.nodes.1)) {
                return bit_select_index(&bit_select.nodes.1, syntax_tree, packed_dimensions);
            }
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
            .ok_or_else(|| unsupported(format!("index of assignment target `{name}`")))
        })
        .collect::<Converted<Vec<_>>>()?;
    // Struct members cannot use the raw-vector fallback: an invalid index
    // would otherwise become a different bit or the entire member.
    if require_constant_in_range
        && !constant_packed_indices_in_range(&name, &indices, packed_dimensions)
    {
        return Err(unsupported(format!(
            "constant select of `{name}` outside its declared range"
        )));
    }

    if let Some(range) = &select.nodes.2 {
        if let Some((msb, lsb)) = dynamic_indexed_bounds(
            &name,
            &range.nodes.1,
            indices.is_empty(),
            syntax_tree,
            packed_dimensions,
        )? {
            return Ok(LValue::Select {
                name,
                msb,
                lsb,
                signed: false,
                array_slice_width: None,
                array_slice_reversed: false,
                is_2state: false,
            });
        }
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
        (msb, lsb) =
            flatten_select_range(&name, &indices, msb.clone(), lsb.clone(), packed_dimensions)
                .ok_or_else(|| {
                    select_error(&name, &indices, Some((&msb, &lsb)), packed_dimensions)
                })?;
        return Ok(LValue::Select {
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
                let signed = selects_signed_element(&name, packed_indices.len(), packed_dimensions);
                return Ok(LValue::Select {
                    name,
                    msb: add_expr(array_offset.clone(), msb),
                    lsb: add_expr(array_offset, lsb),
                    signed,
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
            return Ok(LValue::Select {
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
            return Ok(LValue::Select {
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
        return Err(select_error(&name, &indices, None, packed_dimensions));
    }
    if indices.len() == 1 {
        let signed = selects_signed_element(&name, 1, packed_dimensions);
        let bit = indices[0].clone();
        return Ok(LValue::Select {
            name,
            msb: bit.clone(),
            lsb: bit,
            signed,
            array_slice_width: None,
            array_slice_reversed: false,
            is_2state: false,
        });
    }

    Ok(LValue::Ident(name))
}

pub(super) fn lvalue_from_constant_select(
    name: String,
    select: &sv_parser::ConstantSelect,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    require_constant_in_range: bool,
) -> Converted<LValue> {
    let bit_selects = select.nodes.1.nodes.0.as_slice();
    let indices = bit_selects
        .iter()
        .map(|bit_select| {
            if packed_dimensions.constant_indexed_base
                || has_indexed_selection(RefNode::ConstantExpression(&bit_select.nodes.1))
            {
                indexed_select_base(
                    RefNode::ConstantExpression(&bit_select.nodes.1),
                    syntax_tree,
                    packed_dimensions,
                )
            } else {
                const_expr_from_ref_node(
                    RefNode::ConstantExpression(&bit_select.nodes.1),
                    syntax_tree,
                )?
                .ok_or_else(|| unsupported(format!("index of `{name}`")))
            }
        })
        .collect::<Converted<Vec<_>>>()?;
    // Struct members cannot use the raw-vector fallback: an invalid index
    // would otherwise become a different bit or the entire member.
    if require_constant_in_range
        && !constant_packed_indices_in_range(&name, &indices, packed_dimensions)
    {
        return Err(unsupported(format!(
            "constant select of `{name}` outside its declared range"
        )));
    }

    if let Some(range) = &select.nodes.2 {
        let bound = |expression| {
            if packed_dimensions.constant_indexed_base
                || has_indexed_selection(RefNode::ConstantExpression(expression))
            {
                indexed_select_base(
                    RefNode::ConstantExpression(expression),
                    syntax_tree,
                    packed_dimensions,
                )
            } else {
                const_expr_from_ref_node(RefNode::ConstantExpression(expression), syntax_tree)?
                    .ok_or_else(|| unsupported(format!("part-select bound of `{name}`")))
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
        (msb, lsb) =
            flatten_select_range(&name, &indices, msb.clone(), lsb.clone(), packed_dimensions)
                .ok_or_else(|| {
                    select_error(&name, &indices, Some((&msb, &lsb)), packed_dimensions)
                })?;
        return Ok(LValue::Select {
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
                let signed = selects_signed_element(&name, packed_indices.len(), packed_dimensions);
                return Ok(LValue::Select {
                    name,
                    msb: add_expr(array_offset.clone(), msb),
                    lsb: add_expr(array_offset, lsb),
                    signed,
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
            return Ok(LValue::Select {
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
            return Ok(LValue::Select {
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
        return Err(select_error(&name, &indices, None, packed_dimensions));
    }
    if indices.len() == 1 {
        let signed = selects_signed_element(&name, 1, packed_dimensions);
        let bit = indices[0].clone();
        return Ok(LValue::Select {
            name,
            msb: bit.clone(),
            lsb: bit,
            signed,
            array_slice_width: None,
            array_slice_reversed: false,
            is_2state: false,
        });
    }

    Ok(LValue::Ident(name))
}

pub(super) fn expr_select_from_select(
    base: Expr,
    select: &sv_parser::Select,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let bit_selects = select.nodes.1.nodes.0.as_slice();
    let indices = bit_selects
        .iter()
        .map(|bit_select| bit_select_index(&bit_select.nodes.1, syntax_tree, packed_dimensions))
        .collect::<Converted<Vec<_>>>()?;
    if let Some(range) = &select.nodes.2 {
        // A start index that is only known at run time keeps symbolic bounds in
        // declared index coordinates, like any other select; the frontend
        // lowers them.
        if let Expr::Ident(name) = &base
            && let Some((msb, lsb)) = dynamic_indexed_bounds(
                name,
                &range.nodes.1,
                indices.is_empty(),
                syntax_tree,
                packed_dimensions,
            )?
        {
            return Ok(Expr::Select {
                expr: Box::new(base),
                msb,
                lsb,
                signed: false,
            });
        }
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
            if !indices.is_empty() {
                return Err(unsupported("indexed select of an expression"));
            }
            return Ok(Expr::Select {
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
            return Ok(expr);
        }
        (msb, lsb) =
            flatten_select_range(name, &indices, msb.clone(), lsb.clone(), packed_dimensions)
                .ok_or_else(|| {
                    select_error(name, &indices, Some((&msb, &lsb)), packed_dimensions)
                })?;
        return Ok(Expr::Select {
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
                let signed = selects_signed_element(name, packed_indices.len(), packed_dimensions);
                return Ok(Expr::Select {
                    expr: Box::new(base),
                    msb: add_expr(array_offset.clone(), msb),
                    lsb: add_expr(array_offset, lsb),
                    signed,
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
                return Ok(Expr::Select {
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
                return Ok(Expr::Select {
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
            return Err(select_error(name, &indices, None, packed_dimensions));
        }
        let signed = matches!(&base, Expr::Ident(name)
            if selects_signed_element(name, 1, packed_dimensions));
        let bit = indices[0].clone();
        return Ok(Expr::Select {
            expr: Box::new(base),
            msb: bit.clone(),
            lsb: bit,
            signed,
        });
    }

    Err(match &base {
        Expr::Ident(name) => select_error(name, &indices, None, packed_dimensions),
        _ => unsupported("select of an expression"),
    })
}

/// Whether selecting `count` packed dimensions of `name` yields an element of
/// a signed named type, which keeps its signedness (IEEE 1800-2023 7.4.1).
fn selects_signed_element(name: &str, count: usize, packed_dimensions: &PackedDimensions) -> bool {
    packed_dimensions
        .get(name)
        .is_some_and(|dimensions| dimensions.signed_element_depth == Some(count))
}

/// The runtime start index, constant width, `+:` direction and the declared
/// packed range of a dynamic indexed part-select of `name`.
struct DynamicIndexed {
    start: ConstExpr,
    width: usize,
    plus: bool,
    ascending: bool,
}

/// The shape of an indexed part-select `[start +: width]` whose start is only
/// known at run time: `None` when the select is not one, and `Some(None)`
/// when it is but its target or width is not supported.
fn dynamic_indexed_shape(
    name: &str,
    range: &sv_parser::PartSelectRange,
    no_leading_indices: bool,
    syntax_tree: &SyntaxTree,
    dimensions: &PackedDimensions,
) -> Converted<Option<Option<DynamicIndexed>>> {
    let sv_parser::PartSelectRange::IndexedRange(range) = range else {
        return Ok(None);
    };
    let start = indexed_select_base(RefNode::Expression(&range.nodes.0), syntax_tree, dimensions)?;
    if eval_ast_const_expr(&start, &dimensions.const_env).is_some() {
        return Ok(None);
    }
    let shape = || {
        let width = indexed_select_base(
            RefNode::ConstantExpression(&range.nodes.2),
            syntax_tree,
            dimensions,
        )
        .ok()?;
        let width = usize::try_from(eval_ast_const_expr(&width, &dimensions.const_env)?).ok()?;
        if width == 0 || !no_leading_indices {
            return None;
        }
        let variable = dimensions.get(name)?;
        if !variable.unpacked.is_empty() || variable.packed.len() != 1 {
            return None;
        }
        let dimension = &variable.packed[0];
        let left = eval_ast_const_expr(&dimension.left, &dimensions.const_env)?;
        let right = eval_ast_const_expr(&dimension.right, &dimensions.const_env)?;
        Some(DynamicIndexed {
            start,
            width,
            plus: syntax_tree.get_str(&range.nodes.1.nodes.0)? == "+:",
            ascending: left < right,
        })
    };
    Ok(Some(shape()))
}

fn const_sub(left: ConstExpr, right: ConstExpr) -> ConstExpr {
    ConstExpr::Binary {
        left: Box::new(left),
        op: BinaryOp::Sub,
        right: Box::new(right),
    }
}

/// Symbolic `(msb, lsb)` of a dynamic indexed part-select used as a write
/// target, in declared index coordinates.
fn dynamic_indexed_bounds(
    name: &str,
    range: &sv_parser::PartSelectRange,
    no_leading_indices: bool,
    syntax_tree: &SyntaxTree,
    dimensions: &PackedDimensions,
) -> Converted<Option<(ConstExpr, ConstExpr)>> {
    let Some(Some(shape)) =
        dynamic_indexed_shape(name, range, no_leading_indices, syntax_tree, dimensions)?
    else {
        return Ok(None);
    };
    let tail = const_expr_from_i128(shape.width as i128 - 1);
    let add = |left: ConstExpr, right: ConstExpr| ConstExpr::Binary {
        left: Box::new(left),
        op: BinaryOp::Add,
        right: Box::new(right),
    };
    // Index range of the selection, most-significant end first.
    Ok(Some(match (shape.ascending, shape.plus) {
        (false, true) => (add(shape.start.clone(), tail), shape.start),
        (false, false) => (shape.start.clone(), const_sub(shape.start, tail)),
        (true, true) => (shape.start.clone(), add(shape.start, tail)),
        (true, false) => (const_sub(shape.start.clone(), tail), shape.start),
    }))
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

/// Whether `index` lies between the bounds of `dimension`.
fn packed_index_in_range(index: &ConstExpr, dimension: &PackedDimension) -> ConstExpr {
    let binary = |left: ConstExpr, op, right: ConstExpr| ConstExpr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    };
    let between = |low: &ConstExpr, high: &ConstExpr| {
        binary(
            binary(index.clone(), BinaryOp::Ge, low.clone()),
            BinaryOp::LogicAnd,
            binary(index.clone(), BinaryOp::Le, high.clone()),
        )
    };
    ConstExpr::Mux {
        condition: Box::new(binary(
            dimension.left.clone(),
            BinaryOp::Ge,
            dimension.right.clone(),
        )),
        then_expr: Box::new(between(&dimension.right, &dimension.left)),
        else_expr: Box::new(between(&dimension.left, &dimension.right)),
    }
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

    // A runtime index past its own dimension can still flatten to a position
    // in another element. Move the whole selection above the vector instead,
    // where a read gives X and a write is ignored (IEEE 1800-2023 11.5.1).
    let in_range = indices
        .iter()
        .zip(dimensions)
        .filter(|(index, _)| eval_ast_const_expr(index, &packed_dimensions.const_env).is_none())
        .map(|(index, dimension)| packed_index_in_range(index, dimension))
        .reduce(|left, right| ConstExpr::Binary {
            left: Box::new(left),
            op: BinaryOp::LogicAnd,
            right: Box::new(right),
        });
    if let Some(in_range) = in_range {
        let total_width = product_expr(
            &dimensions
                .iter()
                .map(|dimension| dimension.width.clone())
                .collect::<Vec<_>>(),
        );
        offset = ConstExpr::Mux {
            condition: Box::new(in_range),
            then_expr: Box::new(offset),
            else_expr: Box::new(total_width),
        };
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
) -> Converted<(ConstExpr, ConstExpr)> {
    let bound = |expression| {
        if dimensions.constant_indexed_base
            || has_indexed_selection(RefNode::ConstantExpression(expression))
        {
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
            )?
            .ok_or_else(|| unsupported("part-select bound"))
        }
    };
    match range {
        sv_parser::PartSelectRange::ConstantRange(range) => {
            Ok((bound(&range.nodes.0)?, bound(&range.nodes.2)?))
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
) -> Converted<(ConstExpr, ConstExpr)> {
    let (name, index_count) = selection;
    let width = indexed_select_base(RefNode::ConstantExpression(width), syntax_tree, dimensions)?;
    let width = eval_ast_const_expr(&width, &dimensions.const_env)
        .ok_or_else(|| unsupported("indexed part-select width that is not constant"))?;
    if width <= 0 {
        return Err(unsupported(format!("indexed part-select width {width}")));
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
    let plus = syntax_tree
        .get_str(&operator.nodes.0)
        .ok_or_else(|| unsupported("indexed part-select operator"))?
        == "+:";
    // The base is self-determined; endpoint arithmetic must not inherit its
    // unsigned type or truncate a carry across the base expression's width.
    let base = eval_ast_const_expr(&base, &dimensions.const_env)
        .ok_or_else(|| unsupported("indexed part-select start"))?;
    let overflow = || unsupported("indexed part-select bounds");
    let offset = width - 1;
    let other = if plus {
        base.checked_add(offset).ok_or_else(overflow)?
    } else {
        base.checked_sub(offset).ok_or_else(overflow)?
    };
    let base = const_expr_from_i128(base);
    let other = const_expr_from_i128(other);
    Ok(if plus == ascending {
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
) -> Converted<ConstExpr> {
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
            .ok_or_else(|| unsupported("index expression"))
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
            .ok_or_else(|| unsupported("constant index expression"))
        }
        _ => Err(unsupported("index expression")),
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
) -> Converted<ConstExpr> {
    let sv_parser::ConstantParamExpression::ConstantMintypmaxExpression(initializer) = initializer
    else {
        return Err(unsupported("parameter initializer"));
    };
    let sv_parser::ConstantMintypmaxExpression::Unary(expression) = &**initializer else {
        return Err(unsupported("min:typ:max parameter initializer"));
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
    let not_constant = || unsupported("parameter initializer that is not constant");
    let constant = constant_folding::constant_with_folded_selections(
        &expression,
        &dimensions.const_env,
        &integral_types,
    )
    .ok_or_else(not_constant)?;
    let expression_type = infer_const_expr_type(&constant, &types).ok_or_else(not_constant)?;
    let literal = typecheck::eval_generate_case_operand(
        &constant.into(),
        &dimensions.const_env,
        &integral_types,
        assignment_width.unwrap_or(0).max(expression_type.width),
        expression_type.signed,
    )
    .ok_or_else(not_constant)?;
    Ok(ConstExpr::Literal(
        typecheck::format_integral_literal_binary(&literal),
    ))
}

fn indexed_constant_expression(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let convert = |node| indexed_constant_expression(node, syntax_tree, dimensions);
    match node {
        RefNode::ConstantExpression(expression) => match expression {
            sv_parser::ConstantExpression::ConstantPrimary(primary) => {
                convert(RefNode::ConstantPrimary(primary))
            }
            sv_parser::ConstantExpression::Unary(unary) => {
                let symbol = &unary.nodes.0.nodes.0.nodes.0;
                unary_expr_from_symbol(
                    symbol,
                    convert(RefNode::ConstantPrimary(&unary.nodes.2))?,
                    syntax_tree,
                )
                .ok_or_else(|| {
                    unsupported(format!(
                        "unary operator `{}`",
                        syntax_tree.get_str(symbol).unwrap_or_default()
                    ))
                })
            }
            sv_parser::ConstantExpression::Binary(binary) => {
                let grouped = matches!(&binary.nodes.3, sv_parser::ConstantExpression::ConstantPrimary(primary)
                    if matches!(&**primary, sv_parser::ConstantPrimary::MintypmaxExpression(_)));
                let symbol = &binary.nodes.1.nodes.0.nodes.0;
                let expression = Expr::Binary {
                    left: Box::new(convert(RefNode::ConstantExpression(&binary.nodes.0))?),
                    op: binary_op_from_symbol(symbol, syntax_tree).ok_or_else(|| {
                        unsupported(format!(
                            "binary operator `{}`",
                            syntax_tree.get_str(symbol).unwrap_or_default()
                        ))
                    })?,
                    right: Box::new(convert(RefNode::ConstantExpression(&binary.nodes.3))?),
                };
                Ok(if grouped {
                    expression
                } else {
                    left_associate_expr_binary(expression)
                })
            }
            sv_parser::ConstantExpression::Ternary(ternary) => Ok(Expr::Mux {
                condition: Box::new(convert(RefNode::ConstantExpression(&ternary.nodes.0))?),
                then_expr: Box::new(convert(RefNode::ConstantExpression(&ternary.nodes.3))?),
                else_expr: Box::new(convert(RefNode::ConstantExpression(&ternary.nodes.5))?),
            }),
            sv_parser::ConstantExpression::Inside(_) => {
                Err(unsupported("inside expression in a constant index"))
            }
        },
        RefNode::ConstantPrimary(primary) => match primary {
            sv_parser::ConstantPrimary::PsParameter(parameter) => {
                // Preserve all selections and normalize against declared ranges.
                if parameter.nodes.1.nodes.0.is_some() {
                    return Err(unsupported("member select of a parameter"));
                }
                let name = identifier_text(
                    RefNode::PsParameterIdentifier(&parameter.nodes.0),
                    syntax_tree,
                )
                .ok_or_else(|| unsupported("parameter name"))?;
                let selected = lvalue_from_constant_select(
                    name,
                    &parameter.nodes.1,
                    syntax_tree,
                    dimensions,
                    false,
                )?;
                Ok(expr_from_lvalue(&selected, dimensions))
            }
            sv_parser::ConstantPrimary::MintypmaxExpression(grouped) => {
                match &grouped.nodes.0.nodes.1 {
                    sv_parser::ConstantMintypmaxExpression::Unary(expression) => {
                        convert(RefNode::ConstantExpression(expression))
                    }
                    sv_parser::ConstantMintypmaxExpression::Ternary(_) => {
                        Err(unsupported("min:typ:max expression"))
                    }
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
                    )?
                    .map(const_expr_to_expr)
                    .ok_or_else(|| unsupported("genvar reference"));
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
                .ok_or_else(|| unsupported("constant cast"))
            }
            sv_parser::ConstantPrimary::Concatenation(concat) => {
                let parts = concat
                    .nodes
                    .0
                    .nodes
                    .0
                    .nodes
                    .1
                    .contents()
                    .into_iter()
                    .map(|expression| convert(RefNode::ConstantExpression(expression)))
                    .collect::<Converted<Vec<_>>>()?;
                selected_constant_concatenation(
                    Expr::Concat(parts),
                    concat.nodes.1.as_ref().map(|range| &range.nodes.1),
                    syntax_tree,
                    dimensions,
                )
            }
            sv_parser::ConstantPrimary::MultipleConcatenation(concat) => {
                let (count, repeated) = &concat.nodes.0.nodes.0.nodes.1;
                let count = indexed_select_base(
                    RefNode::ConstantExpression(count),
                    syntax_tree,
                    dimensions,
                )?;
                let parts = repeated
                    .nodes
                    .0
                    .nodes
                    .1
                    .contents()
                    .into_iter()
                    .map(|expression| convert(RefNode::ConstantExpression(expression)))
                    .collect::<Converted<Vec<_>>>()?;
                selected_constant_concatenation(
                    Expr::RepeatConcat { count, parts },
                    concat.nodes.1.as_ref().map(|range| &range.nodes.1),
                    syntax_tree,
                    dimensions,
                )
            }
            _ => const_expr_from_ref_node_with_env(
                node,
                syntax_tree,
                &dimensions.const_env,
                &dimensions.type_aliases,
            )?
            .map(const_expr_to_expr)
            .ok_or_else(|| unsupported("constant expression")),
        },
        _ => Err(unsupported("constant expression")),
    }
}

fn selected_constant_concatenation(
    expression: Expr,
    selection: Option<&sv_parser::ConstantRangeExpression>,
    syntax_tree: &SyntaxTree,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let bound = |expression| {
        indexed_select_base(
            RefNode::ConstantExpression(expression),
            syntax_tree,
            dimensions,
        )
    };
    let (msb, lsb) = match selection {
        None => return Ok(expression),
        Some(sv_parser::ConstantRangeExpression::ConstantExpression(bit)) => {
            let bit = bound(bit)?;
            (bit.clone(), bit)
        }
        Some(sv_parser::ConstantRangeExpression::ConstantPartSelectRange(range)) => {
            match &**range {
                sv_parser::ConstantPartSelectRange::ConstantRange(range) => {
                    (bound(&range.nodes.0)?, bound(&range.nodes.2)?)
                }
                sv_parser::ConstantPartSelectRange::ConstantIndexedRange(range) => {
                    indexed_select_bounds(
                        bound(&range.nodes.0)?,
                        &range.nodes.1,
                        &range.nodes.2,
                        syntax_tree,
                        (None, 0),
                        dimensions,
                    )?
                }
            }
        }
    };
    Ok(Expr::Select {
        expr: Box::new(expression),
        msb,
        lsb,
        signed: false,
    })
}
