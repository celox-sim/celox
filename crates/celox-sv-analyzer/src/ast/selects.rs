//! Lvalue parsing and flattening packed/unpacked selections.

use super::*;

pub(super) fn constant_packed_indices_in_range(
    state: &AnalysisState,
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
        let Some(index) = eval_ast_const_expr(state, index, &dimensions.const_env) else {
            return false;
        };
        let Some(dimension) = variable.packed.get(position) else {
            // A scalar integral member can only select bit zero.
            return index == 0;
        };
        let (Some(left), Some(right)) = (
            eval_ast_const_expr(state, &dimension.left, &dimensions.const_env),
            eval_ast_const_expr(state, &dimension.right, &dimensions.const_env),
        ) else {
            return false;
        };
        (left.min(right)..=left.max(right)).contains(&index)
    })
}

/// Why a select of `name` with constant `indices` and, for a part-select,
/// `bounds` has no representation.
fn select_error(
    state: &AnalysisState,
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
        if const_expr_is_out_of_range(state, index, left, right, const_env) {
            return unsupported(format!(
                "index {} of `{name}` outside its declared range",
                position + 1
            ));
        }
    }
    if let Some((msb, lsb)) = bounds
        && let Some((left, right)) = ranges.clone().nth(indices.len())
        && (const_expr_is_out_of_range(state, msb, left, right, const_env)
            || const_expr_is_out_of_range(state, lsb, left, right, const_env))
    {
        return unsupported(format!(
            "part-select of `{name}` outside its declared range"
        ));
    }
    unsupported(format!("{what} of `{name}`"))
}

/// Syntax-independent index-only assignment target flattening.
pub(super) fn lvalue_from_indices(
    state: &AnalysisState,
    name: String,
    indices: Vec<ConstExpr>,
    packed_dimensions: &PackedDimensions,
) -> Converted<LValue> {
    if let Some((array_offset, packed_indices)) =
        flatten_variable_select(state, &name, &indices, packed_dimensions)
    {
        if !packed_indices.is_empty() {
            if let Some((msb, lsb)) =
                flatten_packed_select(state, &name, &packed_indices, packed_dimensions)
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
        return Err(select_error(
            state,
            &name,
            &indices,
            None,
            packed_dimensions,
        ));
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

/// Shared syntax-independent assignment range flattening and array metadata.
pub(super) fn lvalue_from_range(
    state: &AnalysisState,
    name: String,
    indices: Vec<ConstExpr>,
    mut msb: ConstExpr,
    mut lsb: ConstExpr,
    dynamic: bool,
    packed_dimensions: &PackedDimensions,
) -> Converted<LValue> {
    if dynamic {
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
    let (array_slice_width, array_slice_reversed) =
        unpacked_select_range_metadata(state, &name, &indices, &msb, &lsb, packed_dimensions)
            .map_or((None, false), |(width, reversed)| (Some(width), reversed));
    (msb, lsb) = flatten_select_range(
        state,
        &name,
        &indices,
        msb.clone(),
        lsb.clone(),
        packed_dimensions,
    )
    .ok_or_else(|| {
        select_error(
            state,
            &name,
            &indices,
            Some((&msb, &lsb)),
            packed_dimensions,
        )
    })?;
    Ok(LValue::Select {
        name,
        msb,
        lsb,
        signed: false,
        array_slice_width,
        array_slice_reversed,
        is_2state: false,
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

/// Bind the declared packed range of a runtime indexed part-select.
fn dynamic_indexed_shape_values(
    state: &AnalysisState,
    name: &str,
    start: ConstExpr,
    width: usize,
    plus: bool,
    no_leading_indices: bool,
    dimensions: &PackedDimensions,
) -> Option<DynamicIndexed> {
    if width == 0 || !no_leading_indices {
        return None;
    }
    let variable = dimensions.get(name)?;
    if !variable.unpacked.is_empty() || variable.packed.len() != 1 {
        return None;
    }
    let dimension = &variable.packed[0];
    let left = eval_ast_const_expr(state, &dimension.left, &dimensions.const_env)?;
    let right = eval_ast_const_expr(state, &dimension.right, &dimensions.const_env)?;
    Some(DynamicIndexed {
        start,
        width,
        plus,
        ascending: left < right,
    })
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
fn dynamic_bounds_from_shape(shape: DynamicIndexed) -> (ConstExpr, ConstExpr) {
    let tail = const_expr_from_i128(shape.width as i128 - 1);
    let add = |left: ConstExpr, right: ConstExpr| ConstExpr::Binary {
        left: Box::new(left),
        op: BinaryOp::Add,
        right: Box::new(right),
    };
    // Index range of the selection, most-significant end first.
    match (shape.ascending, shape.plus) {
        (false, true) => (add(shape.start.clone(), tail), shape.start),
        (false, false) => (shape.start.clone(), const_sub(shape.start, tail)),
        (true, true) => (shape.start.clone(), add(shape.start, tail)),
        (true, false) => (const_sub(shape.start.clone(), tail), shape.start),
    }
}

/// Syntax-independent static or supported dynamic indexed range endpoints.
pub(super) fn indexed_range_values(
    state: &AnalysisState,
    name: &str,
    base: ConstExpr,
    width: ConstExpr,
    plus: bool,
    index_count: usize,
    dimensions: &PackedDimensions,
) -> Converted<((ConstExpr, ConstExpr), bool)> {
    let width = eval_ast_const_expr(state, &width, &dimensions.const_env)
        .ok_or_else(|| unsupported("indexed part-select width that is not constant"))?;
    let start = self_determined_index(state, base.clone(), dimensions);
    if eval_ast_const_expr(state, &start, &dimensions.const_env).is_none()
        && let Ok(width) = usize::try_from(width)
        && let Some(shape) = dynamic_indexed_shape_values(
            state,
            name,
            start,
            width,
            plus,
            index_count == 0,
            dimensions,
        )
    {
        return Ok((dynamic_bounds_from_shape(shape), true));
    }
    indexed_select_bounds_values(
        state,
        base,
        plus,
        width,
        (Some(name), index_count),
        dimensions,
    )
    .map(|bounds| (bounds, false))
}

fn flatten_variable_select(
    state: &AnalysisState,
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
            state,
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

fn unpacked_select_range_metadata(
    state: &AnalysisState,
    name: &str,
    indices: &[ConstExpr],
    msb: &ConstExpr,
    lsb: &ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> Option<(ConstExpr, bool)> {
    let dimensions = packed_dimensions.get(name)?;
    let dimension = dimensions.unpacked.get(indices.len())?;
    if const_expr_is_out_of_range(
        state,
        msb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) || const_expr_is_out_of_range(
        state,
        lsb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) {
        return None;
    }
    let (_, packed_indices) = flatten_variable_select(state, name, indices, packed_dimensions)?;
    if !packed_indices.is_empty() {
        return None;
    }
    let msb_offset = eval_ast_const_expr(
        state,
        &unpacked_index_offset(dimension, msb.clone()),
        &packed_dimensions.const_env,
    )?;
    let lsb_offset = eval_ast_const_expr(
        state,
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
    state: &AnalysisState,
    name: &str,
    indices: &[ConstExpr],
    msb: ConstExpr,
    lsb: ConstExpr,
    dimensions: &VariableDimensions,
    packed_dimensions: &PackedDimensions,
) -> Option<(ConstExpr, ConstExpr)> {
    let dimension = dimensions.unpacked.get(indices.len())?;
    if const_expr_is_out_of_range(
        state,
        &msb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) || const_expr_is_out_of_range(
        state,
        &lsb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) {
        return None;
    }
    let (array_offset, packed_indices) =
        flatten_variable_select(state, name, indices, packed_dimensions)?;
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
    state: &AnalysisState,
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
            state,
            name,
            indices,
            msb,
            lsb,
            dimensions,
            packed_dimensions,
        );
    }
    let (array_offset, packed_indices) =
        flatten_variable_select(state, name, indices, packed_dimensions)?;
    let dimension = dimensions.packed.get(packed_indices.len())?;
    if const_expr_is_out_of_range(
        state,
        &msb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) || const_expr_is_out_of_range(
        state,
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
        let (_, offset) = flatten_packed_select(state, name, &packed_indices, packed_dimensions)?;
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
    state: &AnalysisState,
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
        if eval_ast_const_expr(state, &indices[0], &packed_dimensions.const_env) != Some(0) {
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
            state,
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
    // in another element. Move the whole selection above the variable
    // instead, past every unpacked element the selection may be added to,
    // where a read gives X and a write is ignored (IEEE 1800-2023 11.5.1).
    let in_range = indices
        .iter()
        .zip(dimensions)
        .filter(|(index, _)| {
            eval_ast_const_expr(state, index, &packed_dimensions.const_env).is_none()
        })
        .map(|(index, dimension)| packed_index_in_range(index, dimension))
        .reduce(|left, right| ConstExpr::Binary {
            left: Box::new(left),
            op: BinaryOp::LogicAnd,
            right: Box::new(right),
        });
    if let Some(in_range) = in_range {
        let total_width = product_expr(
            &variable_dimensions
                .unpacked
                .iter()
                .map(|dimension| dimension.width.clone())
                .chain(dimensions.iter().map(|dimension| dimension.width.clone()))
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

pub(in crate::ast) fn const_index_has_unknown_value(
    index: &ConstExpr,
    dimensions: &PackedDimensions,
) -> bool {
    // Evaluate the index's mask rather than looking for X/Z syntax: bitwise
    // operations and case equality can make such an expression fully known.
    typecheck::eval_const_integral_literal_with_lookup(
        &index.clone().into(),
        &dimensions.const_env,
        &|name| {
            parameter_type_from_const_env(&dimensions.const_env, name)
                .map(|ty| (ty.width, ty.signed))
        },
    )
    .is_some_and(|value| value.mask != Default::default())
}

fn const_expr_is_out_of_range(
    state: &AnalysisState,
    index: &ConstExpr,
    left: &ConstExpr,
    right: &ConstExpr,
    const_env: &HashMap<String, i128>,
) -> bool {
    let (Some(index), Some(left), Some(right)) = (
        eval_ast_const_expr(state, index, const_env),
        eval_ast_const_expr(state, left, const_env),
        eval_ast_const_expr(state, right, const_env),
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

/// Shared indexed endpoint arithmetic in declared coordinates.
pub(super) fn indexed_select_bounds_values(
    state: &AnalysisState,
    base: ConstExpr,
    plus: bool,
    width: i128,
    selection: (Option<&str>, usize),
    dimensions: &PackedDimensions,
) -> Converted<(ConstExpr, ConstExpr)> {
    if width <= 0 {
        return Err(unsupported(format!("indexed part-select width {width}")));
    }
    let (name, index_count) = selection;
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
                eval_ast_const_expr(state, left, &dimensions.const_env)?
                    < eval_ast_const_expr(state, right, &dimensions.const_env)?,
            )
        })
        .unwrap_or(false);
    // The base is self-determined; endpoint arithmetic must not inherit its
    // unsigned type or truncate a carry across the base expression's width.
    let Some(base_value) = eval_ast_const_expr(state, &base, &dimensions.const_env) else {
        // A procedural loop index is not constant while its body is bound.
        // Keep the endpoints in declared coordinates so normal range flattening
        // retains preceding unpacked and packed indices. Lowering can then unroll
        // the loop before requiring a constant selection inside the selected word.
        let selected_word = name.is_some_and(|name| {
            if let Some(shape) = dimensions.get(name) {
                index_count >= shape.unpacked.len()
                    && index_count + 1 == shape.unpacked.len() + shape.packed.len()
            } else {
                index_count == 0
                    && dimensions::parameter_type_from_const_env(&dimensions.const_env, name)
                        .is_some_and(|ty| ty.width > 1)
            }
        });
        let ty = self_determined_type(state, &base, dimensions).filter(|ty| ty.0 <= 32);
        let Some(ty) = ty.filter(|_| selected_word) else {
            return Err(unsupported("indexed part-select start"));
        };
        let base = ConstExpr::Resize {
            expr: Box::new(self_determined_index(state, base, dimensions)),
            width: 64,
            signed: ty.1,
        };
        // Zero-extend unsigned indices before using signed endpoint arithmetic.
        let base = ConstExpr::Resize {
            expr: Box::new(base),
            width: 64,
            signed: true,
        };
        let other = ConstExpr::Binary {
            left: Box::new(base.clone()),
            op: if plus { BinaryOp::Add } else { BinaryOp::Sub },
            right: Box::new(ConstExpr::Literal(format!("64'sd{}", width - 1))),
        };
        return Ok(if plus == ascending {
            (base, other)
        } else {
            (other, base)
        });
    };
    let base = base_value;
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

/// Bound expressions keep both mux arms until typed folding establishes width.
pub(super) fn indexed_expression_value(
    state: &AnalysisState,
    expression: Expr,
    dimensions: &PackedDimensions,
) -> Option<ConstExpr> {
    let expression = substitute_indexed_parameter_values(state, expression, dimensions);
    expr_to_const(
        state,
        fold_const_integral_expr_preserving_mask(state, expression, &dimensions.const_env),
    )
}

fn substitute_indexed_parameter_values(
    state: &AnalysisState,
    expression: Expr,
    dimensions: &PackedDimensions,
) -> Expr {
    // Unpacked parameter values use the declared array layout, while selections
    // have already been translated to bit offsets in that layout.
    let expression = substitute_expr_idents(expression, &dimensions.array_parameter_values);
    // Known values come from the current environment, which may include a
    // generate index shadowing a parameter. Supply expressions only for values
    // absent from that numeric environment, including X/Z parameters.
    let values = dimensions
        .parameter_values
        .iter()
        .filter(|(name, _)| !dimensions.const_env.contains_key(*name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    substitute_expr_constants_with_parameter_literals(
        state,
        expression,
        &dimensions.const_env,
        &values,
    )
}

// Parameter arithmetic receives the assignment width after self-determined
// selection operands have been folded, rather than the index expression context.
pub(super) fn selected_parameter_initializer(
    native: celox_sv_syntax::ExprId,
    syntax_tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    assignment_width: Option<usize>,
) -> Converted<ConstExpr> {
    let mut dimensions = dimensions.clone();
    dimensions.constant_indexed_base = true;
    let expression = constant_binding::indexed_expression(
        native,
        syntax_tree.declarations(),
        syntax_tree,
        &dimensions,
    )?;
    fold_parameter_selection(
        &syntax_tree.state,
        expression,
        &dimensions,
        assignment_width,
    )
}

fn fold_parameter_selection(
    state: &AnalysisState,
    expression: Expr,
    dimensions: &PackedDimensions,
    assignment_width: Option<usize>,
) -> Converted<ConstExpr> {
    let expression = substitute_indexed_parameter_values(state, expression, dimensions);
    if let Expr::Literal(text) = &expression
        && text.contains(['.', 'e', 'E'])
        && text.parse::<f64>().is_ok()
    {
        return Ok(ConstExpr::Literal(text.clone()));
    }

    let types = parameter_types_from_const_env(&dimensions.const_env);
    let integral_types = types
        .iter()
        .map(|(name, ty)| (name.clone(), (ty.width, ty.signed)))
        .collect();
    let not_constant = || unsupported("parameter initializer that is not constant");
    let constant = constant_folding::constant_with_folded_selections(
        state,
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

fn unknown_indexed_range_literal(
    state: &AnalysisState,
    width: i128,
    stride: &ConstExpr,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let width = eval_ast_const_expr(state, stride, &dimensions.const_env)
        .and_then(|stride| width.checked_mul(stride))
        .and_then(|width| usize::try_from(width).ok())
        .filter(|width| *width > 0 && *width <= constant_folding::MAX_CONSTANT_CONCAT_BITS)
        .ok_or_else(|| unsupported("constant indexed part-select materialization width"))?;
    // IEEE 1800-2023 11.5.1: an X/Z part-select base reads as X,
    // including for two-state sources. Part-select results are unsigned.
    Ok(Expr::Literal(format!("{width}'bx")))
}

/// Explicit remaining typed-primary kernel. Native operator/control binding
/// must call this only for its declared leaf classes, never as an error retry.
pub(super) fn indexed_parameter_indices(
    state: &AnalysisState,
    name: String,
    indices: Vec<ConstExpr>,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    if !indices.is_empty()
        && let Some(variable) = dimensions.get(&name)
        && variable.unpacked.is_empty()
        && indices.len() <= variable.packed.len()
    {
        if indices.iter().zip(&variable.packed).any(|(index, range)| {
            const_expr_is_out_of_range(
                state,
                index,
                &range.left,
                &range.right,
                &dimensions.const_env,
            ) || const_index_has_unknown_value(index, dimensions)
        }) {
            let width = product_expr(
                &variable.packed[indices.len()..]
                    .iter()
                    .map(|range| range.width.clone())
                    .collect::<Vec<_>>(),
            );
            let width = eval_ast_const_expr(state, &width, &dimensions.const_env)
                .and_then(|width| usize::try_from(width).ok())
                .ok_or_else(|| unsupported("constant parameter element width"))?;
            let signing = if selects_signed_element(&name, indices.len(), dimensions) {
                "s"
            } else {
                ""
            };
            let digit = if variable.is_2state { "0" } else { "x" };
            return Ok(Expr::Literal(format!(
                "{width}'{signing}b{}",
                digit.repeat(width)
            )));
        }
    }
    let indices_were_empty = indices.is_empty();
    let selected = lvalue_from_indices(
        state,
        name.clone(),
        indices
            .into_iter()
            .map(|index| self_determined_index(state, index, dimensions))
            .collect(),
        dimensions,
    )?;
    if matches!(selected, LValue::Ident(_)) && !indices_were_empty {
        return Err(unsupported(format!("constant select of `{name}`")));
    }
    Ok(expr_from_lvalue(state, &selected, dimensions))
}

/// Already-bound range coordinates; syntax/source ownership remains outside
/// declared-dimension clipping, stride and unknown-base value semantics.
pub(super) enum ParameterRange {
    Bounds(ConstExpr, ConstExpr),
    Indexed {
        base: ConstExpr,
        width: ConstExpr,
        plus: bool,
    },
}
fn parameter_range_bounds(
    state: &AnalysisState,
    range: ParameterRange,
    selection: (Option<&str>, usize),
    dimensions: &PackedDimensions,
) -> Converted<(ConstExpr, ConstExpr)> {
    match range {
        ParameterRange::Bounds(left, right) => Ok((left, right)),
        ParameterRange::Indexed { base, width, plus } => {
            let width = eval_ast_const_expr(state, &width, &dimensions.const_env)
                .ok_or_else(|| unsupported("indexed part-select width that is not constant"))?;
            indexed_select_bounds_values(state, base, plus, width, selection, dimensions)
        }
    }
}
pub(super) fn indexed_parameter_range(
    state: &AnalysisState,
    name: String,
    indices: Vec<ConstExpr>,
    range: ParameterRange,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let index_count = indices.len();
    if let Some(variable) = dimensions.get(&name)
        && variable.unpacked.is_empty()
        && !variable.packed.is_empty()
    {
        let dimension = variable.packed.get(index_count).ok_or_else(|| {
            unsupported(format!("part-select of `{name}` with invalid packed rank"))
        })?;
        // Clip to the selected element before its final range, without cloning
        // or altering original syntax to manufacture an element view.
        let element = indexed_parameter_indices(state, name.clone(), indices, dimensions)?;
        let stride = product_expr(
            &variable.packed[index_count + 1..]
                .iter()
                .map(|dimension| dimension.width.clone())
                .collect::<Vec<_>>(),
        );
        if let ParameterRange::Indexed { base, width, .. } = &range
            && const_index_has_unknown_value(base, dimensions)
        {
            let width = eval_ast_const_expr(state, width, &dimensions.const_env)
                .ok_or_else(|| unsupported("indexed part-select width that is not constant"))?;
            if width <= 0 {
                return Err(unsupported(format!("indexed part-select width {width}")));
            }
            return unknown_indexed_range_literal(state, width, &stride, dimensions);
        }
        let (msb, lsb) =
            parameter_range_bounds(state, range, (Some(&name), index_count), dimensions)?;
        let mut msb = packed_index_offset(dimension, msb);
        let mut lsb = packed_index_offset(dimension, lsb);
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
                    right: Box::new(ConstExpr::Literal("1".into())),
                },
            );
            lsb = ConstExpr::Binary {
                left: Box::new(lsb),
                op: BinaryOp::Mul,
                right: Box::new(stride),
            };
        }
        return Ok(Expr::Select {
            expr: Box::new(element),
            msb,
            lsb,
            signed: false,
        });
    }
    let (msb, lsb) = parameter_range_bounds(state, range, (Some(&name), index_count), dimensions)?;
    let selected = lvalue_from_range(state, name, indices, msb, lsb, false, dimensions)?;
    Ok(expr_from_lvalue(state, &selected, dimensions))
}

pub(super) enum AggregateSelection {
    Index(ConstExpr),
    Range(ParameterRange),
}
/// Select a self-determined concatenated value in descending bit coordinates.
/// Syntax adapters supply values; unknown indexed bases retain four-state X.
pub(super) fn aggregate_selection_value(
    state: &AnalysisState,
    expression: Expr,
    selection: AggregateSelection,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let (msb, lsb) = match selection {
        AggregateSelection::Index(bit) => (bit.clone(), bit),
        AggregateSelection::Range(range) => {
            if let ParameterRange::Indexed { base, width, .. } = &range
                && const_index_has_unknown_value(base, dimensions)
            {
                let width = eval_ast_const_expr(state, width, &dimensions.const_env)
                    .ok_or_else(|| unsupported("indexed part-select width that is not constant"))?;
                if width <= 0 {
                    return Err(unsupported(format!("indexed part-select width {width}")));
                }
                return unknown_indexed_range_literal(
                    state,
                    width,
                    &ConstExpr::Literal("1".into()),
                    dimensions,
                );
            }
            parameter_range_bounds(state, range, (None, 0), dimensions)?
        }
    };
    Ok(Expr::Select {
        expr: Box::new(expression),
        msb,
        lsb,
        signed: false,
    })
}

fn self_determined_type(
    state: &AnalysisState,
    expr: &ConstExpr,
    dimensions: &PackedDimensions,
) -> Option<(usize, bool)> {
    let const_env = &dimensions.const_env;
    match expr {
        ConstExpr::Literal(value) => {
            let literal = typecheck::parse_integral_literal(value)?;
            Some((literal.width, literal.signed))
        }
        ConstExpr::Ident(name) => {
            let Some(variable) = dimensions.get(name) else {
                return dimensions::parameter_type_from_const_env(const_env, name)
                    .map(|ty| (ty.width, ty.signed))
                    .or_else(|| const_env.contains_key(name).then_some((32, true)));
            };
            if !variable.unpacked.is_empty() {
                return None;
            }
            let mut width = 1usize;
            for dimension in &variable.packed {
                let size =
                    usize::try_from(eval_ast_const_expr(state, &dimension.width, const_env)?)
                        .ok()?;
                width = width.checked_mul(size)?;
            }
            Some((width, variable.signed))
        }
        ConstExpr::SelectRange {
            msb, lsb, signed, ..
        } => Some((
            typecheck::select_range_width(
                &(**msb).clone().into(),
                &(**lsb).clone().into(),
                const_env,
            )?,
            *signed,
        )),
        ConstExpr::Resize { width, signed, .. } => Some((*width, *signed)),
        ConstExpr::Select { .. } => Some((1, false)),
        ConstExpr::Function { .. } => None,
        ConstExpr::Unary { op, expr } => match op {
            UnaryOp::Plus | UnaryOp::Minus | UnaryOp::BitNot | UnaryOp::ToTwoState => {
                self_determined_type(state, expr, dimensions)
            }
            UnaryOp::LogicNot | UnaryOp::RedAnd | UnaryOp::RedOr | UnaryOp::RedXor => {
                Some((1, false))
            }
        },
        ConstExpr::Binary { left, op, right } => match op {
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Mod
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor => {
                let (left_width, left_signed) = self_determined_type(state, left, dimensions)?;
                let (right_width, right_signed) = self_determined_type(state, right, dimensions)?;
                Some((left_width.max(right_width), left_signed && right_signed))
            }
            BinaryOp::Pow | BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
                self_determined_type(state, left, dimensions)
            }
            _ => Some((1, false)),
        },
        ConstExpr::Mux {
            then_expr,
            else_expr,
            ..
        } => {
            let (then_width, then_signed) = self_determined_type(state, then_expr, dimensions)?;
            let (else_width, else_signed) = self_determined_type(state, else_expr, dimensions)?;
            Some((then_width.max(else_width), then_signed && else_signed))
        }
    }
}

/// A select index as the value it has in its self-determined context
/// (IEEE 1800-2023 11.5.1): the flattening adds it to offsets at the width of
/// unsized literals, where `j + 2'd1` would not wrap.
pub(super) fn self_determined_index(
    state: &AnalysisState,
    index: ConstExpr,
    dimensions: &PackedDimensions,
) -> ConstExpr {
    if matches!(
        index,
        ConstExpr::Literal(_) | ConstExpr::Ident(_) | ConstExpr::Select { .. }
    ) {
        return index;
    }
    let Some((width, signed)) = self_determined_type(state, &index, dimensions) else {
        return index;
    };
    if width == 0 || width >= 32 {
        return index;
    }
    let literal = |value: i128| ConstExpr::Literal(value.to_string());
    let binary = |left: ConstExpr, op, right: ConstExpr| ConstExpr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    };
    if !signed {
        return binary(index, BinaryOp::BitAnd, literal((1 << width) - 1));
    }
    // A signed index is computed with its operands sign-extended to the
    // offsets' width, which leaves its low bits as they are except under a
    // logical right shift: shift in zeros from the operand's own width.
    let index = zero_extend_logical_shifts(state, index, dimensions);
    let masked = binary(index, BinaryOp::BitAnd, literal((1 << width) - 1));
    // Sign-extend from the top bit: (x ^ s) - s.
    let sign = literal(1 << (width - 1));
    binary(
        binary(masked, BinaryOp::BitXor, sign.clone()),
        BinaryOp::Sub,
        sign,
    )
}

/// `expr` with the left operand of each logical right shift reduced to its own
/// width, so that a wider signed context does not shift its sign bits in.
fn zero_extend_logical_shifts(
    state: &AnalysisState,
    expr: ConstExpr,
    dimensions: &PackedDimensions,
) -> ConstExpr {
    let go = |expr: Box<ConstExpr>| Box::new(zero_extend_logical_shifts(state, *expr, dimensions));
    match expr {
        ConstExpr::Binary {
            left,
            op: BinaryOp::Shr,
            right,
        } => {
            let left = zero_extend_logical_shifts(state, *left, dimensions);
            let left = match self_determined_type(state, &left, dimensions) {
                Some((width, true)) if width < 32 => ConstExpr::Binary {
                    left: Box::new(left),
                    op: BinaryOp::BitAnd,
                    right: Box::new(ConstExpr::Literal(((1i128 << width) - 1).to_string())),
                },
                _ => left,
            };
            ConstExpr::Binary {
                left: Box::new(left),
                op: BinaryOp::Shr,
                right,
            }
        }
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: go(left),
            op,
            right: go(right),
        },
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => ConstExpr::Resize {
            expr: go(expr),
            width,
            signed,
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary { op, expr: go(expr) },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition,
            then_expr: go(then_expr),
            else_expr: go(else_expr),
        },
        expr => expr,
    }
}
