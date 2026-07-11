use crate::BigUint;
use crate::parser::{ParserError, resolve_dims};
use num_traits::{ToPrimitive as _, Zero};
use veryl_analyzer::ir::{
    AssignDestination, Comptime, Expression, Factor, Module, Op, VarId, VarIndex, VarSelect,
    VarSelectOp,
};
use veryl_analyzer::value::Value;
use veryl_parser::token_range::TokenRange;

use crate::ir::BitAccess;

/// Extract a compile-time constant value for Celox 4-state encoding.
///
/// Veryl encoding: X=(payload=0, mask=1), Z=(payload=1, mask=1)
/// Celox encoding: X=(v=1, m=1), Z=(v=0, m=1)
///
/// Conversion: v = payload ^ mask_xz, m = mask_xz
pub fn celox_value_from_comptime(comptime: &Comptime) -> Option<(BigUint, BigUint, usize, bool)> {
    let val = comptime.get_value().ok()?;
    let mask_xz = val.mask_xz().into_owned();
    let payload = val.payload().into_owned();
    Some((&payload ^ &mask_xz, mask_xz, val.width(), val.signed()))
}

pub fn eval_constexpr(expr: &Expression) -> Option<BigUint> {
    let comptime = expr.comptime();
    // `evaluated` is only trusted for Factor::Value (literals); for variables and
    // compound expressions we require `is_const` (true compile-time constants).
    let is_value = matches!(expr, Expression::Term(f) if matches!(f.as_ref(), Factor::Value(_)));
    if comptime.is_const || (is_value && comptime.evaluated) {
        if let Ok(v) = comptime.get_value() {
            return Some(v.payload().into_owned());
        }
        // The analyzer may set is_const=true on compound expressions without
        // computing the value (value=Unknown).  Fall through to recursive
        // evaluation from sub-expressions.
    } else {
        return None;
    }

    // Recursive evaluation for is_const expressions whose top-level value is Unknown.
    match expr {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Variable(_, _, _, ct) | Factor::Value(ct) => {
                ct.get_value().ok().map(|v| v.payload().into_owned())
            }
            _ => None,
        },
        Expression::Binary(lhs, op, rhs, _) => {
            let l = eval_constexpr(lhs)?;
            let r = eval_constexpr(rhs)?;
            match op {
                Op::Add => Some(l + r),
                Op::Sub => {
                    if l >= r {
                        Some(l - r)
                    } else {
                        // Wrap around for unsigned subtraction (2^width)
                        let width = expr.comptime().r#type.total_width().unwrap_or(64);
                        let modulus = BigUint::from(1u8) << width;
                        Some(modulus - (r - l))
                    }
                }
                Op::Mul => Some(l * r),
                Op::Div => {
                    if r.is_zero() {
                        None
                    } else {
                        Some(l / r)
                    }
                }
                Op::Rem => {
                    if r.is_zero() {
                        None
                    } else {
                        Some(l % r)
                    }
                }
                Op::BitAnd => Some(l & r),
                Op::BitOr => Some(l | r),
                Op::BitXor => Some(l ^ r),
                Op::LogicShiftL | Op::ArithShiftL => {
                    use num_traits::ToPrimitive;
                    Some(l << r.to_usize()?)
                }
                Op::LogicShiftR | Op::ArithShiftR => {
                    use num_traits::ToPrimitive;
                    Some(l >> r.to_usize()?)
                }
                Op::Eq => Some(BigUint::from(u64::from(l == r))),
                Op::Ne => Some(BigUint::from(u64::from(l != r))),
                Op::Less => Some(BigUint::from(u64::from(l < r))),
                Op::LessEq => Some(BigUint::from(u64::from(l <= r))),
                Op::Greater => Some(BigUint::from(u64::from(l > r))),
                Op::GreaterEq => Some(BigUint::from(u64::from(l >= r))),
                _ => None,
            }
        }
        Expression::Unary(op, inner, _) => {
            let v = eval_constexpr(inner)?;
            match op {
                Op::Add => Some(v),
                Op::Sub => {
                    let width = expr.comptime().r#type.total_width().unwrap_or(64);
                    let modulus = BigUint::from(1u8) << width;
                    if v.is_zero() {
                        Some(v)
                    } else {
                        Some(modulus - v)
                    }
                }
                Op::BitNot => {
                    let width = expr.comptime().r#type.total_width().unwrap_or(64);
                    let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
                    Some(v ^ mask)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn eval_constexpr_usize(
    expr: &Expression,
    feature: &'static str,
) -> Result<Option<usize>, ParserError> {
    let Some(value) = eval_constexpr(expr) else {
        return Ok(None);
    };
    value.to_usize().map(Some).ok_or_else(|| {
        ParserError::illegal_context(
            feature,
            "compile-time value cannot be represented as usize",
            Some(&expr.token_range()),
        )
    })
}

fn checked_bit_access(
    base: usize,
    width: usize,
    feature: &'static str,
    token: Option<&TokenRange>,
) -> Result<BitAccess, ParserError> {
    if width == 0 {
        return Err(ParserError::illegal_context(
            feature,
            "selected width must be nonzero",
            token,
        ));
    }
    let msb = base
        .checked_add(width - 1)
        .ok_or_else(|| ParserError::illegal_context(feature, "bit range overflows usize", token))?;
    Ok(BitAccess::new(base, msb))
}

fn collect_dims(module: &Module, var_id: VarId) -> Result<Vec<usize>, ParserError> {
    let variable = &module.variables[&var_id];
    let var_type = &variable.r#type;

    let mut dims = resolve_dims(module, variable, var_type.array.as_slice(), "array")?;
    // For enum-typed variables, the width Shape is empty but the actual
    // bit width is encoded in the TypeKind. Use kind.width() as the
    // base scalar width when the explicit width shape is absent.
    if var_type.width().is_empty() {
        if let Some(kind_width) = var_type.kind.width()
            && kind_width > 1
        {
            dims.push(kind_width);
        }
    } else {
        dims.extend(resolve_dims(
            module,
            variable,
            var_type.width().as_slice(),
            "width",
        )?);
    }
    Ok(dims)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PartSelectGeometry {
    Colon { lsb: usize, elements: usize },
    PlusColon { elements: usize },
    MinusColon { elements: usize },
    Step { elements: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectGeometry {
    pub dimensions: Vec<usize>,
    pub strides: Vec<usize>,
    pub total_width: usize,
    /// Number of aggregate indices before the optional part-select anchor.
    pub dimension_count: usize,
    pub part: Option<PartSelectGeometry>,
    pub selected_width: usize,
}

/// Validate and normalize a variable select before any consumer constructs IR.
///
/// In particular, a part-select anchor never consumes another aggregate
/// dimension.  `+:`, `-:`, and `step` require a static, nonzero width, while
/// both bounds of `:` must be static because its result width is otherwise not
/// representable in the typed IR.
pub(crate) fn select_geometry(
    module: &Module,
    var_id: VarId,
    index: &VarIndex,
    select: &VarSelect,
) -> Result<SelectGeometry, ParserError> {
    let (dimensions, strides, total_width) = get_dimensions_and_strides(module, var_id)?;
    let total_indices = index.0.len().checked_add(select.0.len()).ok_or_else(|| {
        ParserError::illegal_context("variable select", "index count overflows usize", None)
    })?;

    let Some((op, range_expr)) = &select.1 else {
        if total_indices > dimensions.len() {
            return Err(ParserError::illegal_context(
                "variable select",
                format!(
                    "{total_indices} indices exceed the variable's {} dimensions",
                    dimensions.len()
                ),
                None,
            ));
        }
        let selected_width = if total_indices == 0 {
            total_width
        } else {
            *strides.get(total_indices - 1).ok_or_else(|| {
                ParserError::illegal_context(
                    "variable select",
                    "selected dimension is absent from the stride table",
                    None,
                )
            })?
        };
        return Ok(SelectGeometry {
            dimensions,
            strides,
            total_width,
            dimension_count: total_indices,
            part: None,
            selected_width,
        });
    };

    let anchor_expr = select.0.last().ok_or_else(|| {
        ParserError::illegal_context(
            "variable part select",
            "part select is missing its anchor expression",
            Some(&range_expr.token_range()),
        )
    })?;
    let dimension_count = total_indices.checked_sub(1).ok_or_else(|| {
        ParserError::illegal_context(
            "variable part select",
            "part select is missing its anchor expression",
            Some(&range_expr.token_range()),
        )
    })?;
    let dimension_width = *dimensions.get(dimension_count).ok_or_else(|| {
        ParserError::illegal_context(
            "variable part select",
            format!(
                "part-select dimension {dimension_count} is outside the {}-dimension variable",
                dimensions.len()
            ),
            Some(&range_expr.token_range()),
        )
    })?;
    let stride = *strides.get(dimension_count).ok_or_else(|| {
        ParserError::illegal_context(
            "variable part select",
            format!(
                "part-select dimension {dimension_count} is outside the {}-entry stride table",
                strides.len()
            ),
            Some(&range_expr.token_range()),
        )
    })?;
    let range =
        eval_constexpr_usize(range_expr, "variable part-select range")?.ok_or_else(|| {
            ParserError::illegal_context(
                "variable part select",
                "part-select range must be a compile-time value",
                Some(&range_expr.token_range()),
            )
        })?;
    let anchor = eval_constexpr_usize(anchor_expr, "variable part-select anchor")?;

    let part = match op {
        VarSelectOp::Colon => {
            let anchor = anchor.ok_or_else(|| {
                ParserError::illegal_context(
                    "variable part select",
                    "colon-select bounds must both be compile-time values",
                    Some(&anchor_expr.token_range()),
                )
            })?;
            if anchor < range || anchor >= dimension_width {
                return Err(ParserError::illegal_context(
                    "variable part select",
                    format!(
                        "colon-select [{anchor}:{range}] is outside dimension width {dimension_width}"
                    ),
                    Some(&anchor_expr.token_range()),
                ));
            }
            PartSelectGeometry::Colon {
                lsb: range,
                elements: anchor - range + 1,
            }
        }
        VarSelectOp::PlusColon | VarSelectOp::MinusColon | VarSelectOp::Step => {
            if range == 0 {
                return Err(ParserError::illegal_context(
                    "variable part select",
                    "part-select width must be nonzero",
                    Some(&range_expr.token_range()),
                ));
            }
            if range > dimension_width {
                return Err(ParserError::illegal_context(
                    "variable part select",
                    format!("part-select width {range} exceeds dimension width {dimension_width}"),
                    Some(&range_expr.token_range()),
                ));
            }
            if let Some(anchor) = anchor {
                let valid = match op {
                    VarSelectOp::PlusColon => anchor
                        .checked_add(range)
                        .is_some_and(|end| end <= dimension_width),
                    VarSelectOp::MinusColon => {
                        anchor < dimension_width
                            && anchor.checked_add(1).is_some_and(|n| n >= range)
                    }
                    VarSelectOp::Step => anchor
                        .checked_mul(range)
                        .and_then(|start| start.checked_add(range))
                        .is_some_and(|end| end <= dimension_width),
                    VarSelectOp::Colon => false,
                };
                if !valid {
                    return Err(ParserError::illegal_context(
                        "variable part select",
                        format!(
                            "part-select anchor {anchor} and width {range} are outside dimension width {dimension_width}"
                        ),
                        Some(&anchor_expr.token_range()),
                    ));
                }
            }
            match op {
                VarSelectOp::PlusColon => PartSelectGeometry::PlusColon { elements: range },
                VarSelectOp::MinusColon => PartSelectGeometry::MinusColon { elements: range },
                VarSelectOp::Step => PartSelectGeometry::Step { elements: range },
                VarSelectOp::Colon => {
                    return Err(ParserError::illegal_context(
                        "variable part select",
                        "inconsistent colon-select geometry",
                        Some(&range_expr.token_range()),
                    ));
                }
            }
        }
    };
    let elements = match part {
        PartSelectGeometry::Colon { elements, .. }
        | PartSelectGeometry::PlusColon { elements }
        | PartSelectGeometry::MinusColon { elements }
        | PartSelectGeometry::Step { elements } => elements,
    };
    let selected_width = elements.checked_mul(stride).ok_or_else(|| {
        ParserError::illegal_context(
            "variable part select",
            format!("select width {elements} times stride {stride} overflows usize"),
            Some(&range_expr.token_range()),
        )
    })?;

    Ok(SelectGeometry {
        dimensions,
        strides,
        total_width,
        dimension_count,
        part: Some(part),
        selected_width,
    })
}

pub fn eval_var_select(
    module: &Module,
    var_id: VarId,
    index: &VarIndex,
    select: &VarSelect,
) -> Result<BitAccess, ParserError> {
    let geometry = select_geometry(module, var_id, index, select)?;
    let strides = &geometry.strides;
    let total_width = geometry.total_width;

    // Helper: Calculates the "full slice range" at that point
    // i: Index of the failed dimension
    let get_slice_fallback = |base: usize, i: usize| -> Result<BitAccess, ParserError> {
        let width = if i == 0 {
            total_width
        } else {
            *strides.get(i - 1).ok_or_else(|| {
                ParserError::illegal_context(
                    "variable select",
                    format!("fallback dimension {i} is outside the stride table"),
                    None,
                )
            })?
        };
        let access = checked_bit_access(base, width, "variable select", None)?;
        if access.msb >= total_width {
            return Err(ParserError::illegal_context(
                "variable select",
                format!(
                    "dynamic fallback range [{}:{}] is outside width {total_width}",
                    access.msb, access.lsb
                ),
                None,
            ));
        }
        Ok(access)
    };

    let mut all_indices = index.0.clone();
    all_indices.extend(select.0.iter().cloned());

    let mut base_offset = 0usize;
    let mut processed_count = 0;

    let limit = if select.1.is_some() {
        all_indices.len().saturating_sub(1)
    } else {
        all_indices.len()
    };

    for (i, index_val) in all_indices[..limit].iter().enumerate() {
        if let Some(idx) = eval_constexpr_usize(index_val, "variable select index")? {
            if let Some(&stride) = strides.get(i) {
                let term = idx.checked_mul(stride).ok_or_else(|| {
                    ParserError::illegal_context(
                        "variable select",
                        format!("index {idx} times stride {stride} overflows usize"),
                        Some(&index_val.token_range()),
                    )
                })?;
                base_offset = base_offset.checked_add(term).ok_or_else(|| {
                    ParserError::illegal_context(
                        "variable select",
                        "selected bit offset overflows usize",
                        Some(&index_val.token_range()),
                    )
                })?;
                processed_count += 1;
            }
        } else {
            // Encountered dynamic index: return the entire range of this level based on current base_offset
            return get_slice_fallback(base_offset, i);
        }
    }

    if let Some((op, range_expr)) = &select.1 {
        let Some(anchor_expr) = all_indices.last() else {
            return Err(ParserError::illegal_context(
                "variable select",
                "part select is missing its anchor expression",
                Some(&range_expr.token_range()),
            ));
        };
        let Some(anchor) = eval_constexpr_usize(anchor_expr, "variable select anchor")? else {
            // Dynamic part-select anchor: the longest static prefix stops
            // before this select, so conservatively use the whole current level.
            return get_slice_fallback(base_offset, processed_count);
        };
        let val = if let Some(v) = eval_constexpr_usize(range_expr, "variable select range")? {
            v
        } else {
            // If range width is dynamic, also return the entire level range
            return get_slice_fallback(base_offset, processed_count);
        };

        let Some(&weight) = strides.get(processed_count) else {
            return Err(ParserError::illegal_context(
                "variable select",
                format!(
                    "part-select dimension {processed_count} is outside the {}-entry stride table",
                    strides.len()
                ),
                Some(&anchor_expr.token_range()),
            ));
        };
        if weight == 0 {
            return Err(ParserError::illegal_context(
                "variable select",
                "part-select stride is zero",
                Some(&anchor_expr.token_range()),
            ));
        }

        let (lsb_rel, msb_rel) = match op {
            VarSelectOp::Colon => {
                let lsb = val.checked_mul(weight);
                let msb = anchor
                    .checked_mul(weight)
                    .and_then(|base| base.checked_add(weight - 1));
                (lsb, msb)
            }
            VarSelectOp::PlusColon => {
                let lsb = anchor.checked_mul(weight);
                let msb = anchor
                    .checked_add(val)
                    .and_then(|end| end.checked_mul(weight))
                    .and_then(|end| end.checked_sub(1));
                (lsb, msb)
            }
            VarSelectOp::MinusColon => {
                let msb = anchor
                    .checked_mul(weight)
                    .and_then(|base| base.checked_add(weight - 1));
                let span = val.checked_mul(weight);
                let lsb = msb
                    .zip(span)
                    .and_then(|(msb, span)| msb.checked_add(1)?.checked_sub(span));
                (lsb, msb)
            }
            VarSelectOp::Step => {
                let actual_lsb = anchor.checked_mul(val);
                let lsb = actual_lsb.and_then(|lsb| lsb.checked_mul(weight));
                let msb = actual_lsb
                    .and_then(|lsb| lsb.checked_add(val))
                    .and_then(|end| end.checked_mul(weight))
                    .and_then(|end| end.checked_sub(1));
                (lsb, msb)
            }
        };
        let (Some(lsb_rel), Some(msb_rel)) = (lsb_rel, msb_rel) else {
            return Err(ParserError::illegal_context(
                "variable select",
                "part-select range overflows or underflows usize",
                Some(&anchor_expr.token_range()),
            ));
        };
        let lsb = base_offset.checked_add(lsb_rel).ok_or_else(|| {
            ParserError::illegal_context(
                "variable select",
                "part-select LSB overflows usize",
                Some(&anchor_expr.token_range()),
            )
        })?;
        let msb = base_offset.checked_add(msb_rel).ok_or_else(|| {
            ParserError::illegal_context(
                "variable select",
                "part-select MSB overflows usize",
                Some(&anchor_expr.token_range()),
            )
        })?;
        if lsb > msb || msb >= total_width {
            return Err(ParserError::illegal_context(
                "variable select",
                format!("selected range [{msb}:{lsb}] is outside width {total_width}"),
                Some(&anchor_expr.token_range()),
            ));
        }
        Ok(BitAccess::new(lsb, msb))
    } else {
        let width = if processed_count == 0 {
            total_width
        } else {
            *strides.get(processed_count - 1).ok_or_else(|| {
                ParserError::illegal_context(
                    "variable select",
                    "selected dimension is outside the stride table",
                    None,
                )
            })?
        };
        let access = checked_bit_access(base_offset, width, "variable select", None)?;
        if access.msb >= total_width {
            return Err(ParserError::illegal_context(
                "variable select",
                format!(
                    "selected range [{}:{}] is outside width {total_width}",
                    access.msb, access.lsb
                ),
                None,
            ));
        }
        Ok(access)
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
) -> Result<(Vec<usize>, Vec<usize>, usize), ParserError> {
    let dims = collect_dims(module, var_id)?;

    let mut strides = vec![1; dims.len()];
    let mut current_stride = 1usize;
    for i in (0..dims.len()).rev() {
        if dims[i] == 0 {
            return Err(ParserError::illegal_context(
                "variable dimensions",
                format!("dimension {i} has zero width"),
                None,
            ));
        }
        strides[i] = current_stride;
        current_stride = current_stride.checked_mul(dims[i]).ok_or_else(|| {
            ParserError::illegal_context(
                "variable dimensions",
                format!(
                    "dimension {} times accumulated stride {current_stride} overflows usize",
                    dims[i]
                ),
                None,
            )
        })?;
    }
    Ok((dims, strides, current_stride))
}

pub fn get_access_width(
    module: &Module,
    var_id: VarId,
    index: &VarIndex,
    select: &VarSelect,
) -> Result<usize, ParserError> {
    Ok(select_geometry(module, var_id, index, select)?.selected_width)
}

/// Build a read-modify-write expression for a static partial assignment.
///
/// For `dst[lsb..=msb] = rhs`, produces:
///   `(old_value & ~(mask << lsb)) | (rhs << lsb)`
/// where `mask = (1 << access_width) - 1`.
///
/// `old_value` is the current whole-variable expression from the symbolic state.
pub fn build_partial_assign_expr(
    module: &Module,
    dst: &AssignDestination,
    rhs: Expression,
    old_value: Expression,
) -> Result<Expression, ParserError> {
    let bit_access = eval_var_select(module, dst.id, &dst.index, &dst.select)?;
    let (_, _, total_width) = get_dimensions_and_strides(module, dst.id)?;

    let lsb = bit_access.lsb;
    let access_width = bit_access.msb - bit_access.lsb + 1;

    // If the partial assignment covers the entire variable, just return rhs directly.
    if lsb == 0 && access_width == total_width {
        return Ok(rhs);
    }

    let token = TokenRange::default();

    // mask = (1 << access_width) - 1  (all-ones of access_width bits)
    let mask_big = (BigUint::from(1u64) << access_width) - BigUint::from(1u64);
    let mask_expr =
        Expression::create_value(Value::new_biguint(mask_big, total_width, false), token);

    let ct = || Box::new(Comptime::create_unknown(token));

    // Build: shifted_mask = mask << lsb  (skip shift when lsb == 0)
    let shifted_mask = if lsb == 0 {
        mask_expr
    } else {
        let lsb_expr = Expression::create_value(Value::new(lsb as u64, total_width, false), token);
        Expression::Binary(
            Box::new(mask_expr),
            Op::LogicShiftL,
            Box::new(lsb_expr),
            ct(),
        )
    };

    // Build: ~shifted_mask
    let inv_mask = Expression::Unary(Op::BitNot, Box::new(shifted_mask), ct());

    // Build: old_value & ~shifted_mask  (clear the target bits)
    let cleared = Expression::Binary(Box::new(old_value), Op::BitAnd, Box::new(inv_mask), ct());

    // Build: rhs << lsb  (skip shift when lsb == 0)
    let shifted_rhs = if lsb == 0 {
        rhs
    } else {
        let lsb_expr = Expression::create_value(Value::new(lsb as u64, total_width, false), token);
        Expression::Binary(Box::new(rhs), Op::LogicShiftL, Box::new(lsb_expr), ct())
    };

    // Build: (old_value & ~shifted_mask) | (rhs << lsb)
    Ok(Expression::Binary(
        Box::new(cleared),
        Op::BitOr,
        Box::new(shifted_rhs),
        ct(),
    ))
}
