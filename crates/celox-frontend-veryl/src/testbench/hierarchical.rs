//! Hierarchical references of a testbench and the bits they select.

use super::*;

pub(super) fn hierarchical_reference_name(reference: &HierVarRef) -> String {
    reference
        .inst_path
        .iter()
        .copied()
        .chain(reference.var_path.0.iter().copied())
        .map(source_name)
        .collect::<Vec<_>>()
        .join(".")
}

pub(super) fn invalid_hierarchical_reference(
    reference: &HierVarRef,
    detail: impl Into<String>,
) -> ParserError {
    ParserError::illegal_context(
        "hierarchical variable reference",
        format!(
            "`{}`: {}",
            hierarchical_reference_name(reference),
            detail.into()
        ),
        Some(&reference.comptime.token),
    )
}

pub(crate) fn hierarchical_destination_reference(
    destination: &HierAssignDestination,
) -> HierVarRef {
    HierVarRef {
        inst_path: destination.inst_path.clone(),
        var_path: destination.var_path.clone(),
        index: destination.index.clone(),
        select: destination.select.clone(),
        comptime: destination.comptime.clone(),
    }
}

#[cfg(test)]
pub(super) fn resolve_hierarchical_reference<'a>(
    lookup: &'a FrontendLookup,
    reference: &HierVarRef,
) -> Result<(StateAddr, &'a VariableInfo), ParserError> {
    let (root_instance, _) = lookup.root_instance_and_module().ok_or_else(|| {
        invalid_hierarchical_reference(reference, "the root instance was not found")
    })?;
    resolve_hierarchical_reference_from(lookup, root_instance, reference)
}

pub(crate) fn resolve_hierarchical_reference_from<'a>(
    lookup: &'a FrontendLookup,
    base_instance: InstanceId,
    reference: &HierVarRef,
) -> Result<(StateAddr, &'a VariableInfo), ParserError> {
    let mut resolved_path = lookup
        .instance_ids
        .iter()
        .find_map(|(path, instance)| (*instance == base_instance).then(|| path.0.clone()))
        .ok_or_else(|| {
            invalid_hierarchical_reference(reference, "the base instance path was not found")
        })?;
    resolved_path.reserve(reference.inst_path.len());
    for &segment in &reference.inst_path {
        let segment_name = source_name(segment);
        let candidates = lookup
            .instance_ids
            .keys()
            .filter(|candidate| {
                candidate.0.len() == resolved_path.len() + 1
                    && candidate.0.starts_with(&resolved_path)
                    && candidate.0[resolved_path.len()].0 == segment_name
            })
            .map(|candidate| candidate.0[resolved_path.len()].clone())
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [part] => resolved_path.push(part.clone()),
            [] => {
                return Err(invalid_hierarchical_reference(
                    reference,
                    format!("instance `{}` was not found", source_name(segment)),
                ));
            }
            _ => {
                return Err(invalid_hierarchical_reference(
                    reference,
                    format!(
                        "instance `{}` is ambiguous because it elaborates to multiple array elements",
                        source_name(segment)
                    ),
                ));
            }
        }
    }

    let instance_id = lookup
        .instance_ids
        .get(&InstancePath(resolved_path))
        .copied()
        .ok_or_else(|| {
            invalid_hierarchical_reference(reference, "the elaborated instance path was not found")
        })?;
    let module_id = lookup
        .instance_module
        .get(&instance_id)
        .copied()
        .ok_or_else(|| {
            invalid_hierarchical_reference(reference, "the target instance has no module")
        })?;
    let var_id = match lookup
        .module_var_path_index
        .get(&module_id)
        .and_then(|variables| variables.get(&source_path(&reference.var_path)))
    {
        Some(Some(var_id)) => *var_id,
        Some(None) => {
            return Err(invalid_hierarchical_reference(
                reference,
                format!("variable `{}` is ambiguous", reference.var_path),
            ));
        }
        None => {
            return Err(invalid_hierarchical_reference(
                reference,
                format!("variable `{}` was not found", reference.var_path),
            ));
        }
    };
    let info = lookup
        .module_variables
        .get(&module_id)
        .and_then(|variables| variables.get(&var_id))
        .ok_or_else(|| {
            invalid_hierarchical_reference(reference, "the target variable has no metadata")
        })?;
    let address = lookup
        .state_address(&SourceAddr {
            instance_id,
            var_id,
        })
        .ok_or_else(|| {
            invalid_hierarchical_reference(
                reference,
                "the target variable has no elaborated state address",
            )
        })?;
    Ok((address, info))
}

/// Resolve the flattened state bits read by a hierarchical reference.
///
/// Static unpacked and packed indices retain their precise range. A dynamic
/// index conservatively covers the remaining slice at that dimension.
pub(crate) fn hierarchical_reference_bits(
    info: &VariableInfo,
    reference: &HierVarRef,
) -> Result<BitAccess, ParserError> {
    let invalid = |detail| invalid_hierarchical_reference(reference, detail);
    let array_total = info
        .array_dims
        .iter()
        .try_fold(1usize, |total, dimension| total.checked_mul(*dimension))
        .ok_or_else(|| invalid("array dimensions overflow usize"))?;
    if array_total == 0 || info.width == 0 || !info.width.is_multiple_of(array_total) {
        return Err(invalid(
            "the target variable has invalid flattened dimensions",
        ));
    }
    let element_width = info.width / array_total;
    let mut strides = vec![element_width; info.array_dims.len()];
    let mut stride = element_width;
    for (dimension, size) in info.array_dims.iter().enumerate().rev() {
        strides[dimension] = stride;
        stride = stride
            .checked_mul(*size)
            .ok_or_else(|| invalid("array stride overflows usize"))?;
    }

    let mut base = 0usize;
    let mut consumed = 0usize;
    for (dimension, expression) in reference.index.0.iter().enumerate() {
        let Some(&dimension_stride) = strides.get(dimension) else {
            return Err(invalid("too many unpacked array indices"));
        };
        let Some(index) = eval_constexpr(expression).and_then(|value| value.to_usize()) else {
            let width = if dimension == 0 {
                info.width
            } else {
                strides[dimension - 1]
            };
            return checked_reference_bits(reference, base, width, info.width);
        };
        if index >= info.array_dims[dimension] {
            return Err(invalid_hierarchical_reference(
                reference,
                format!(
                    "array index {index} is outside dimension width {}",
                    info.array_dims[dimension]
                ),
            ));
        }
        base = base
            .checked_add(
                index
                    .checked_mul(dimension_stride)
                    .ok_or_else(|| invalid("array index offset overflows usize"))?,
            )
            .ok_or_else(|| invalid("array index offset overflows usize"))?;
        consumed += 1;
    }

    let accessed_width = if consumed == 0 {
        info.width
    } else if consumed == info.array_dims.len() {
        element_width
    } else {
        strides[consumed - 1]
    };
    if reference.select.0.is_empty() && reference.select.1.is_none() {
        return checked_reference_bits(reference, base, accessed_width, info.width);
    }

    let packed_dimensions = if info.packed_dims.is_empty() {
        vec![element_width]
    } else {
        info.packed_dims.clone()
    };
    let mut packed_strides = vec![1usize; packed_dimensions.len()];
    let mut packed_stride = 1usize;
    for (dimension, size) in packed_dimensions.iter().enumerate().rev() {
        packed_strides[dimension] = packed_stride;
        packed_stride = packed_stride
            .checked_mul(*size)
            .ok_or_else(|| invalid("packed stride overflows usize"))?;
    }

    let prefix_count = if reference.select.1.is_some() {
        reference.select.0.len().saturating_sub(1)
    } else {
        reference.select.0.len()
    };
    for (dimension, expression) in reference.select.0[..prefix_count].iter().enumerate() {
        let Some(&scale) = packed_strides.get(dimension) else {
            return Err(invalid("too many packed indices"));
        };
        let Some(index) = eval_constexpr(expression).and_then(|value| value.to_usize()) else {
            let width = if dimension == 0 {
                accessed_width
            } else {
                packed_strides[dimension - 1]
            };
            return checked_reference_bits(reference, base, width, info.width);
        };
        base = base
            .checked_add(
                index
                    .checked_mul(scale)
                    .ok_or_else(|| invalid("packed index offset overflows usize"))?,
            )
            .ok_or_else(|| invalid("packed index offset overflows usize"))?;
    }

    let selected_width = if let Some((op, range_expression)) = &reference.select.1 {
        let anchor_expression = reference
            .select
            .0
            .last()
            .ok_or_else(|| invalid("part select is missing its anchor"))?;
        let current_width = if prefix_count == 0 {
            accessed_width
        } else {
            packed_strides[prefix_count - 1]
        };
        let Some(anchor) = eval_constexpr(anchor_expression).and_then(|value| value.to_usize())
        else {
            return checked_reference_bits(reference, base, current_width, info.width);
        };
        let Some(range) = eval_constexpr(range_expression).and_then(|value| value.to_usize())
        else {
            return checked_reference_bits(reference, base, current_width, info.width);
        };
        let scale = *packed_strides
            .get(prefix_count)
            .ok_or_else(|| invalid("part select exceeds packed dimensions"))?;
        let (relative_lsb, elements) = match op {
            veryl_analyzer::ir::VarSelectOp::Colon => {
                let elements = anchor
                    .checked_sub(range)
                    .and_then(|width| width.checked_add(1))
                    .ok_or_else(|| invalid("part-select range underflows"))?;
                (range, elements)
            }
            veryl_analyzer::ir::VarSelectOp::PlusColon => (anchor, range),
            veryl_analyzer::ir::VarSelectOp::MinusColon => {
                let lsb = anchor
                    .checked_add(1)
                    .and_then(|end| end.checked_sub(range))
                    .ok_or_else(|| invalid("part-select range underflows"))?;
                (lsb, range)
            }
            veryl_analyzer::ir::VarSelectOp::Step => {
                let lsb = anchor
                    .checked_mul(range)
                    .ok_or_else(|| invalid("part-select offset overflows usize"))?;
                (lsb, range)
            }
        };
        base = base
            .checked_add(
                relative_lsb
                    .checked_mul(scale)
                    .ok_or_else(|| invalid("part-select offset overflows usize"))?,
            )
            .ok_or_else(|| invalid("part-select offset overflows usize"))?;
        elements
            .checked_mul(scale)
            .ok_or_else(|| invalid("part-select width overflows usize"))?
    } else {
        *packed_strides
            .get(reference.select.0.len() - 1)
            .ok_or_else(|| invalid("packed select exceeds variable dimensions"))?
    };
    checked_reference_bits(reference, base, selected_width, info.width)
}

pub(super) fn checked_reference_bits(
    reference: &HierVarRef,
    lsb: usize,
    width: usize,
    total_width: usize,
) -> Result<BitAccess, ParserError> {
    let msb = lsb
        .checked_add(width.checked_sub(1).ok_or_else(|| {
            invalid_hierarchical_reference(reference, "selected width must be nonzero")
        })?)
        .ok_or_else(|| invalid_hierarchical_reference(reference, "bit range overflows usize"))?;
    if msb >= total_width {
        return Err(invalid_hierarchical_reference(
            reference,
            "selected bit range is outside the target variable",
        ));
    }
    Ok(BitAccess::new(lsb, msb))
}

pub(super) fn variable_access_width(
    info: &VariableInfo,
    index: &VarIndex,
    select: &VarSelect,
) -> Option<usize> {
    let array_total = info
        .array_dims
        .iter()
        .try_fold(1usize, |total, dimension| total.checked_mul(*dimension))?;
    if array_total == 0 || !info.width.is_multiple_of(array_total) {
        return None;
    }
    let consumed = index.0.len();
    if consumed > info.array_dims.len() {
        return None;
    }
    let element_width = info.width / array_total;
    let accessed_width = info.array_dims[consumed..]
        .iter()
        .try_fold(element_width, |width, dimension| {
            width.checked_mul(*dimension)
        })?;

    let packed_dimensions = if info.packed_dims.is_empty() {
        vec![element_width]
    } else {
        info.packed_dims.clone()
    };
    let mut packed_strides = vec![1usize; packed_dimensions.len()];
    let mut stride = 1usize;
    for (dimension, size) in packed_dimensions.iter().enumerate().rev() {
        packed_strides[dimension] = stride;
        stride = stride.checked_mul(*size)?;
    }

    if let Some((op, range_expression)) = &select.1 {
        let range = eval_constexpr(range_expression)?.to_usize()?;
        let dimension = select.0.len().checked_sub(1)?;
        let stride = *packed_strides.get(dimension)?;
        let elements = match op {
            VarSelectOp::Colon => {
                let anchor = eval_constexpr(select.0.last()?)?.to_usize()?;
                anchor.checked_sub(range)?.checked_add(1)
            }
            VarSelectOp::PlusColon | VarSelectOp::MinusColon | VarSelectOp::Step => Some(range),
        }?;
        return elements.checked_mul(stride);
    }
    if select.0.is_empty() {
        Some(accessed_width)
    } else {
        packed_strides.get(select.0.len() - 1).copied()
    }
}

pub(super) enum TestbenchRead {
    Root(VarId),
    Hierarchical(Box<HierVarRef>),
}
