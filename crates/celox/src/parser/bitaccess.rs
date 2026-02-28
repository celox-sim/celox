use crate::BigUint;
use num_traits::Zero;
use veryl_analyzer::ir::{Expression, Factor, Module, VarId, VarIndex, VarSelect, VarSelectOp};

use crate::ir::BitAccess;

// TODO: I feel this is definitely not enough
pub fn eval_constexpr(expr: &Expression) -> Option<BigUint> {
    match expr {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Variable(_var_id, _var_index, _var_select, comptime, _token_range) => {
                comptime.get_value().ok().map(|e| e.payload().into_owned())
            }
            Factor::Value(comptime, _token_range) => {
                comptime.get_value().ok().map(|e| e.payload().into_owned())
            }
            // TODO: There are cases where constant folding can be properly performed
            _ => None,
        },
        // TODO: There are cases where constant folding can be properly performed
        _ => None,
    }
}
pub fn eval_var_select(
    module: &Module,
    var_id: VarId,
    index: &VarIndex,
    select: &VarSelect,
) -> BitAccess {
    let variable = &module.variables[&var_id];
    let var_type = &variable.r#type;

    let mut dims = Vec::new();
    for dim in var_type.array.as_slice() {
        dims.push(dim.expect("Array dimension must be known"));
    }
    // For enum-typed variables, the width Shape is empty but the actual
    // bit width is encoded in the TypeKind. Use kind.width() as the
    // base scalar width when the explicit width shape is absent.
    if var_type.width.is_empty() {
        if let Some(kind_width) = var_type.kind.width()
            && kind_width > 1
        {
            dims.push(kind_width);
        }
    } else {
        for dim in var_type.width.as_slice() {
            dims.push(dim.expect("Vector width must be known"));
        }
    }

    let mut strides = vec![1; dims.len()];
    let mut current_stride = 1;
    for i in (0..dims.len()).rev() {
        strides[i] = current_stride;
        current_stride *= dims[i];
    }

    let total_width = current_stride;

    // Helper: Calculates the "full slice range" at that point
    // i: Index of the failed dimension
    let get_slice_fallback = |base: usize, i: usize| -> BitAccess {
        let width = if i == 0 { total_width } else { strides[i - 1] };
        BitAccess::new(base, base + width - 1)
    };

    let to_u = |e: &Expression| -> Option<usize> {
        eval_constexpr(e).map(|v| {
            if v.is_zero() {
                0
            } else {
                v.to_u64_digits().first().copied().unwrap_or(0) as usize
            }
        })
    };

    let mut all_indices = index.0.clone();
    all_indices.extend(select.0.iter().cloned());

    let mut base_offset = 0;
    let mut processed_count = 0;

    let limit = if select.1.is_some() {
        all_indices.len().saturating_sub(1)
    } else {
        all_indices.len()
    };

    for i in 0..limit {
        if let Some(idx) = to_u(&all_indices[i]) {
            if let Some(&stride) = strides.get(i) {
                base_offset += idx * stride;
                processed_count += 1;
            }
        } else {
            // Encountered dynamic index: return the entire range of this level based on current base_offset
            return get_slice_fallback(base_offset, i);
        }
    }

    if let Some((op, range_expr)) = &select.1 {
        let anchor = to_u(all_indices.last().unwrap()).unwrap_or(0);
        let val = if let Some(v) = to_u(range_expr) {
            v
        } else {
            // If range width is dynamic, also return the entire level range
            return get_slice_fallback(base_offset, processed_count);
        };

        let weight = strides[processed_count];

        let (lsb_rel, msb_rel) = match op {
            VarSelectOp::Colon => (val * weight, anchor * weight + (weight - 1)),
            VarSelectOp::PlusColon => (anchor * weight, (anchor + val) * weight - 1),
            VarSelectOp::MinusColon => {
                let msb = anchor * weight + (weight - 1);
                (msb.saturating_sub(val * weight) + 1, msb)
            }
            VarSelectOp::Step => {
                let actual_lsb = anchor * val;
                let actual_msb = actual_lsb + val - 1;
                (actual_lsb * weight, (actual_msb + 1) * weight - 1)
            }
        };
        BitAccess::new(base_offset + lsb_rel, base_offset + msb_rel)
    } else {
        let width = if processed_count == 0 {
            total_width
        } else {
            strides[processed_count - 1]
        };
        BitAccess::new(base_offset, base_offset + width - 1)
    }
}
pub fn is_static_access(index: &VarIndex, select: &VarSelect) -> bool {
    for expr in &index.0 {
        if eval_constexpr(expr).is_none() {
            return false;
        }
    }

    for expr in &select.0 {
        if eval_constexpr(expr).is_none() {
            return false;
        }
    }

    if let Some((_, range_expr)) = &select.1
        && eval_constexpr(range_expr).is_none()
    {
        return false;
    }

    true
}

pub fn get_dimensions_and_strides(
    module: &Module,
    var_id: VarId,
) -> (Vec<usize>, Vec<usize>, usize) {
    let variable = &module.variables[&var_id];
    let var_type = &variable.r#type;

    let mut dims = Vec::new();
    for dim in var_type.array.as_slice() {
        dims.push(dim.expect("Array dimension must be known"));
    }
    if var_type.width.is_empty() {
        if let Some(kind_width) = var_type.kind.width()
            && kind_width > 1
        {
            dims.push(kind_width);
        }
    } else {
        for dim in var_type.width.as_slice() {
            dims.push(dim.expect("Vector width must be known"));
        }
    }

    let mut strides = vec![1; dims.len()];
    let mut current_stride = 1;
    for i in (0..dims.len()).rev() {
        strides[i] = current_stride;
        current_stride *= dims[i];
    }
    (dims, strides, current_stride)
}

pub fn get_access_width(
    module: &Module,
    var_id: VarId,
    index: &VarIndex,
    select: &VarSelect,
) -> usize {
    let (dims, strides, total_width) = get_dimensions_and_strides(module, var_id);
    let total_indices = index.0.len() + select.0.len();

    let to_u = |e: &Expression| -> Option<usize> {
        eval_constexpr(e).map(|v| {
            if v.is_zero() {
                0
            } else {
                v.to_u64_digits().first().copied().unwrap_or(0) as usize
            }
        })
    };

    // Part select handling
    if let Some((op, range_expr)) = &select.1 {
        // When there's a part select (+: / -:), the last element of select.0
        // is the anchor/base expression, not a dimension-consuming index.
        // This matches eval_var_select which uses limit = all_indices.len() - 1.
        let effective_idx = total_indices.saturating_sub(1);
        let stride = if effective_idx < strides.len() {
            strides[effective_idx]
        } else {
            1
        };

        let anchor = select.0.last().and_then(to_u);
        let rhs = to_u(range_expr);

        if let (Some(anchor), Some(rhs)) = (anchor, rhs) {
            let elem_width = match op {
                VarSelectOp::Colon => {
                    if anchor >= rhs {
                        anchor - rhs + 1
                    } else {
                        rhs - anchor + 1
                    }
                }
                VarSelectOp::PlusColon | VarSelectOp::MinusColon | VarSelectOp::Step => rhs,
            };
            elem_width * stride
        } else {
            // Fallback: return full width of the current dimension if width is dynamic (should not happen for +: / -:)
            if effective_idx == 0 {
                total_width
            } else {
                strides[effective_idx - 1]
            }
        }
    } else {
        // Simple index access
        if total_indices == 0 {
            total_width
        } else if total_indices <= dims.len() {
            strides[total_indices - 1]
        } else {
            1 // Should not happen if index count matches dimensions
        }
    }
}
