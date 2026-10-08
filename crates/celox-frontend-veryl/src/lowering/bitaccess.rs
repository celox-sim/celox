use celox_design::BitAccess;
use num_bigint::BigUint;
use num_traits::{ToPrimitive as _, Zero};
use veryl_analyzer::ir::{
    AssignDestination, Comptime, Expression, Factor, Module, Op, Shape, Type, TypeKind,
    ValueVariant, VarId, VarIndex, VarSelect, VarSelectOp,
};
use veryl_analyzer::value::Value;
use veryl_parser::token_range::TokenRange;

use crate::ParserError;

use super::types::extend_resolved_dims;

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
    let width = val.width();
    let celox_value = &payload ^ &mask_xz;

    if comptime.r#type.is_2state() {
        let width_mask = if width == 0 {
            // Unsized fill literals use bit 0 as the value to replicate once
            // an enclosing context supplies their width.
            BigUint::from(1u8)
        } else {
            (BigUint::from(1u8) << width) - BigUint::from(1u8)
        };
        let defined = &width_mask ^ (&mask_xz & &width_mask);
        Some((
            celox_value & defined,
            BigUint::from(0u8),
            width,
            val.signed(),
        ))
    } else {
        Some((celox_value, mask_xz, width, val.signed()))
    }
}

/// Materialize a compile-time value in its expression context.
///
/// Veryl represents the context-dependent fill literals (`'0`, `'1`, `'x`,
/// and `'z`) with width zero and keeps their seed in bit 0. A zero-width
/// value must never reach SLT or SIR: in a sized context the seed fills that
/// width, while in a self-determined context the literal is one bit wide.
pub fn celox_value_from_comptime_in_context(
    comptime: &Comptime,
    context_width: Option<usize>,
) -> Option<(BigUint, BigUint, usize, bool)> {
    let (value, mask_xz, width, signed) = celox_value_from_comptime(comptime)?;
    if width != 0 {
        return Some((value, mask_xz, width, signed));
    }

    // A zero-sized destination is invalid independently of the literal. Use
    // the self-determined width here so the later destination-width check
    // reports that error without first constructing a zero-width IR value.
    let width = context_width.filter(|width| *width != 0).unwrap_or(1);
    let fill_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
    let value = if value.bit(0) {
        fill_mask.clone()
    } else {
        BigUint::from(0u8)
    };
    let mask_xz = if mask_xz.bit(0) {
        fill_mask
    } else {
        BigUint::from(0u8)
    };
    Some((value, mask_xz, width, signed))
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
    let width = var_type.width();

    let mut dims = Vec::with_capacity(var_type.array.as_slice().len() + width.as_slice().len() + 1);
    extend_resolved_dims(
        module,
        variable,
        var_type.array.as_slice(),
        "array",
        &mut dims,
    )?;
    // For enum-typed variables, the width Shape is empty but the actual
    // bit width is encoded in the TypeKind. Use kind.width() as the
    // base scalar width when the explicit width shape is absent.
    if width.is_empty() {
        if let Some(kind_width) = var_type.kind.width()
            && kind_width > 1
        {
            dims.push(kind_width);
        }
    } else {
        extend_resolved_dims(module, variable, width.as_slice(), "width", &mut dims)?;
    }
    Ok(dims)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartSelectGeometry {
    Colon { lsb: usize, elements: usize },
    PlusColon { elements: usize },
    MinusColon { elements: usize },
    Step { elements: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectGeometry {
    pub strides: Vec<usize>,
    pub total_width: usize,
    /// Number of aggregate indices before the optional part-select anchor.
    pub dimension_count: usize,
    pub part: Option<PartSelectGeometry>,
    pub selected_width: usize,
}

/// Veryl keeps an unpacked-array slice (`a[i+:2]`) as a range on the final
/// index of a reference. Celox addresses unpacked and packed dimensions as one
/// list, so the slice becomes a part select on that dimension. Unlike a packed
/// `[msb:lsb]`, an array `[first:last]` names its low element first.
pub fn fold_array_range(
    index: &VarIndex,
    select: &VarSelect,
) -> Result<Option<(VarIndex, VarSelect)>, ParserError> {
    let Some((op, bound)) = index.range.as_deref() else {
        return Ok(None);
    };
    if !select.0.is_empty() || select.1.is_some() {
        return Err(ParserError::illegal_context(
            "array slice",
            "a select after an unpacked-array slice is unsupported",
            Some(&bound.token_range()),
        ));
    }
    let mut indices = index.indices.clone();
    let part = if matches!(op, VarSelectOp::Colon) {
        let first = indices.pop().ok_or_else(|| {
            ParserError::illegal_context(
                "array slice",
                "array slice is missing its first element",
                Some(&bound.token_range()),
            )
        })?;
        indices.push(bound.clone());
        (VarSelectOp::Colon, first)
    } else {
        (op.clone(), bound.clone())
    };
    Ok(Some((VarIndex::default(), VarSelect(indices, Some(part)))))
}

/// Reject a runtime unpacked-array slice where a select is addressed directly.
/// It spans several elements from a runtime start, whereas SIR addresses an
/// unpacked array one element at a time; value lowering expands it first with
/// [`expand_runtime_array_slice`].
pub fn reject_runtime_array_slice(index: &VarIndex) -> Result<(), ParserError> {
    if index.is_range()
        && let Some(dynamic) = index.expressions().find(|e| eval_constexpr(e).is_none())
    {
        return Err(ParserError::illegal_context(
            "array slice",
            "an unpacked-array slice with a runtime index cannot be addressed directly",
            Some(&dynamic.token_range()),
        ));
    }
    Ok(())
}

/// Lower a runtime unpacked-array slice (`a[i+:k]`, `a[i-:k]`, `a[i step k]`).
///
/// The slice is read as one run of `k` consecutive elements starting at a
/// start clamped into the array, so the load never leaves the array. Shifting
/// that run by how far the clamp moved it then leaves exactly the elements
/// outside the array empty; those read X (0 when two-state) without shifting
/// their in-range neighbors (IEEE 1800-2023 7.4.6).
pub fn expand_runtime_array_slice(
    module: &Module,
    factor: &Factor,
) -> Result<Option<Expression>, ParserError> {
    let Factor::Variable(var_id, index, select, comptime) = factor else {
        return Ok(None);
    };
    let Some((op, bound)) = index.range.as_deref() else {
        return Ok(None);
    };
    if index.expressions().all(|e| eval_constexpr(e).is_some()) {
        return Ok(None);
    }
    let token = comptime.token;
    if !select.0.is_empty() || select.1.is_some() {
        return Err(ParserError::illegal_context(
            "array slice",
            "a select after an unpacked-array slice is unsupported",
            Some(&token),
        ));
    }
    let variable = &module.variables[var_id];
    let mut shape = Vec::new();
    extend_resolved_dims(
        module,
        variable,
        variable.r#type.array.as_slice(),
        "array",
        &mut shape,
    )?;
    let Some((first, prefix)) = index.indices.split_last() else {
        return Err(ParserError::illegal_context(
            "array slice",
            "array slice is missing its first element",
            Some(&token),
        ));
    };
    let dim = prefix.len();
    let count = eval_constexpr_usize(bound, "array slice width")?;
    // `[first:last]` has a compile-time count only when both bounds are, so a
    // runtime slice is always `+:`, `-:`, or `step`.
    let (Some(&dim_size), Some(count)) = (shape.get(dim), count) else {
        return Err(ParserError::illegal_context(
            "array slice",
            "a runtime array slice needs a compile-time width",
            Some(&token),
        ));
    };
    if count == 0 || count > dim_size || matches!(op, VarSelectOp::Colon) {
        return Err(ParserError::illegal_context(
            "array slice",
            format!("runtime array slice width {count} must be within dimension size {dim_size}"),
            Some(&token),
        ));
    }

    let unknown = || Box::new(Comptime::create_unknown(token));
    let literal = |value: u64| Expression::create_value(Value::new(value, 64, false), token);
    let binary = |lhs: Expression, op: Op, rhs: Expression| {
        Expression::Binary(Box::new(lhs), op, Box::new(rhs), unknown())
    };
    let ternary = |cond: Expression, then: Expression, otherwise: Expression| {
        Expression::Ternary(
            Box::new(cond),
            Box::new(then),
            Box::new(otherwise),
            unknown(),
        )
    };
    let or = |lhs: Option<Expression>, rhs: Expression| {
        Some(match lhs {
            Some(lhs) => binary(lhs, Op::LogicOr, rhs),
            None => rhs,
        })
    };

    // The run index must stay inside the array even for an X or Z
    // coordinate, so coordinates are read as two-state values first, as an
    // ordinary element select reads them.
    let two_state = |expr: &Expression| {
        let width = crate::context_width::get_expr_width(expr).unwrap_or(64);
        let mut target = Type::new(TypeKind::Bit);
        target.set_concrete_width(Shape::new(vec![Some(width)]));
        target.signed = crate::context_width::expression_signed(expr);
        let mut type_type = Type::new(TypeKind::Type);
        type_type.signed = target.signed;
        binary(
            expr.clone(),
            Op::As,
            Expression::Term(Box::new(Factor::Value(Comptime {
                value: ValueVariant::Type(target),
                r#type: type_type,
                is_const: true,
                is_global: true,
                token,
                ..Default::default()
            }))),
        )
    };
    let first = &two_state(first);
    let prefix = prefix
        .iter()
        .map(|expr| {
            if eval_constexpr(expr).is_some() {
                expr.clone()
            } else {
                two_state(expr)
            }
        })
        .collect::<Vec<_>>();

    // Work with `t = start + bias`, which is never negative, so that every
    // comparison and offset below is unsigned. A signed coordinate is biased
    // into an unsigned one by flipping its sign bit.
    let first_width = crate::context_width::get_expr_width(first).ok_or_else(|| {
        ParserError::illegal_context(
            "array slice",
            "the slice index has no known width",
            Some(&first.token_range()),
        )
    })?;
    let count_bits = (usize::BITS - count.leading_zeros()) as usize;
    if first_width + 2 * count_bits + 2 > 64 {
        return Err(ParserError::illegal_context(
            "array slice",
            format!("a {first_width}-bit runtime slice index is too wide to bounds-check"),
            Some(&first.token_range()),
        ));
    }
    let (unsigned_first, sign_bias) = if crate::context_width::expression_signed(first) {
        let sign = 1u64 << (first_width - 1);
        (
            binary(
                first.clone(),
                Op::BitXor,
                Expression::create_value(Value::new(sign, first_width, false), token),
            ),
            sign,
        )
    } else {
        (first.clone(), 0)
    };
    let count = count as u64;
    let (t, bias) = match op {
        VarSelectOp::PlusColon => (
            binary(unsigned_first, Op::Add, literal(count - 1)),
            sign_bias + count - 1,
        ),
        VarSelectOp::MinusColon => (
            binary(unsigned_first, Op::Add, literal(0)),
            sign_bias + count - 1,
        ),
        VarSelectOp::Step => (
            binary(
                binary(unsigned_first, Op::Mul, literal(count)),
                Op::Add,
                literal(count - 1),
            ),
            sign_bias * count + count - 1,
        ),
        VarSelectOp::Colon => unreachable!("rejected above"),
    };
    let dim_size = dim_size as u64;
    let highest = bias + dim_size - count;

    // Out-of-range prefix coordinates select no row at all.
    let mut invalid = None;
    let mut clamped_prefix = Vec::with_capacity(prefix.len());
    for (expr, &size) in prefix.iter().zip(&shape) {
        if eval_constexpr(expr).is_some() {
            clamped_prefix.push(expr.clone());
            continue;
        }
        let mut out_of_range = binary(expr.clone(), Op::GreaterEq, literal(size as u64));
        if crate::context_width::expression_signed(expr) {
            let width = crate::context_width::get_expr_width(expr).unwrap_or(64);
            let mut zero = Expression::create_value(Value::new(0, width, true), token);
            zero.comptime_mut().r#type.signed = true;
            out_of_range = binary(
                binary(expr.clone(), Op::Less, zero),
                Op::LogicOr,
                out_of_range,
            );
        }
        clamped_prefix.push(ternary(out_of_range.clone(), literal(0), expr.clone()));
        invalid = or(invalid, out_of_range);
    }
    // No element is in range when the slice starts at or after the end, or
    // ends before element 0.
    let invalid = or(
        invalid,
        binary(
            binary(t.clone(), Op::Less, literal(bias + 1 - count)),
            Op::LogicOr,
            binary(t.clone(), Op::GreaterEq, literal(bias + dim_size)),
        ),
    )
    .expect("the slice range check is always present");

    // The run starts at `clamped - bias`, inside `[0, dim_size - count]`.
    let clamped = ternary(
        binary(t.clone(), Op::Less, literal(bias)),
        literal(bias),
        ternary(
            binary(t.clone(), Op::Greater, literal(highest)),
            literal(highest),
            t.clone(),
        ),
    );
    let run_start = binary(clamped.clone(), Op::Sub, literal(bias));
    let mut run_index = clamped_prefix;
    run_index.push(run_start);
    let mut run_comptime = comptime.clone();
    run_comptime.is_const = false;
    let run = Expression::Term(Box::new(Factor::Variable(
        *var_id,
        VarIndex::default(),
        VarSelect(run_index, Some((VarSelectOp::PlusColon, literal(count)))),
        run_comptime,
    )));

    let inner = shape[dim + 1..].iter().product::<usize>() as u64;
    let element_width = variable.r#type.total_width().ok_or_else(|| {
        ParserError::illegal_context(
            "array slice",
            "array element width is unknown",
            Some(&token),
        )
    })? as u64;
    let stride = inner * element_width;
    let width = (count * stride) as usize;
    // Only one of the two shifts is nonzero; both are zero when nothing is in
    // range, which the final select replaces anyway.
    let left = ternary(
        binary(
            invalid.clone(),
            Op::LogicOr,
            binary(t.clone(), Op::GreaterEq, literal(bias)),
        ),
        literal(0),
        binary(
            binary(literal(bias), Op::Sub, t.clone()),
            Op::Mul,
            literal(stride),
        ),
    );
    let right = ternary(
        binary(
            invalid.clone(),
            Op::LogicOr,
            binary(t.clone(), Op::LessEq, literal(highest)),
        ),
        literal(0),
        binary(
            binary(t, Op::Sub, literal(highest)),
            Op::Mul,
            literal(stride),
        ),
    );
    let align = |value: Expression| {
        binary(
            binary(value, Op::LogicShiftL, left.clone()),
            Op::LogicShiftR,
            right.clone(),
        )
    };
    let all =
        |value: BigUint| Expression::create_value(Value::new_biguint(value, width, false), token);
    let ones = (BigUint::from(1u8) << width) - BigUint::from(1u8);
    let shifted = align(run);
    let (empty, value) = if variable.r#type.is_2state() {
        (all(BigUint::zero()), shifted)
    } else {
        // `create_value` types a literal as two-state `bit`, which drops X.
        let mut x = Expression::create_value(Value::new_x(width, false), token);
        x.comptime_mut().r#type.kind = TypeKind::Logic;
        let empty_positions = Expression::Unary(Op::BitNot, Box::new(align(all(ones))), unknown());
        (
            x.clone(),
            binary(shifted, Op::BitOr, binary(empty_positions, Op::BitAnd, x)),
        )
    };
    // A concatenation keeps the shifts self-determined at the slice width.
    let selected = ternary(invalid, empty, value);
    Ok(Some(Expression::Concatenation(
        vec![(selected, None)],
        Box::new(comptime.clone()),
    )))
}

/// Validate and normalize a variable select before any consumer constructs IR.
///
/// In particular, a part-select anchor never consumes another aggregate
/// dimension.  `+:`, `-:`, and `step` require a static, nonzero width, while
/// both bounds of `:` must be static because its result width is otherwise not
/// representable in the typed IR.
pub fn select_geometry(
    module: &Module,
    var_id: VarId,
    index: &VarIndex,
    select: &VarSelect,
) -> Result<SelectGeometry, ParserError> {
    let folded = fold_array_range(index, select)?;
    let (index, select) = folded.as_ref().map_or((index, select), |(i, s)| (i, s));
    let mut strides = collect_dims(module, var_id)?;
    let dimensions_len = strides.len();
    let total_width = dimensions_to_strides(&mut strides)?;
    let total_indices = index
        .indices
        .len()
        .checked_add(select.0.len())
        .ok_or_else(|| {
            ParserError::illegal_context("variable select", "index count overflows usize", None)
        })?;

    let Some((op, range_expr)) = &select.1 else {
        if total_indices > dimensions_len {
            return Err(ParserError::illegal_context(
                "variable select",
                format!(
                    "{total_indices} indices exceed the variable's {} dimensions",
                    dimensions_len
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
    let dimension_width = if dimension_count >= dimensions_len {
        return Err(ParserError::illegal_context(
            "variable part select",
            format!(
                "part-select dimension {dimension_count} is outside the {dimensions_len}-dimension variable"
            ),
            Some(&range_expr.token_range()),
        ));
    } else if dimension_count == 0 {
        total_width / strides[0]
    } else {
        strides[dimension_count - 1] / strides[dimension_count]
    };
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
    eval_var_select_with_geometry(index, select, &geometry)
}

pub(crate) fn eval_var_select_with_geometry(
    index: &VarIndex,
    select: &VarSelect,
    geometry: &SelectGeometry,
) -> Result<BitAccess, ParserError> {
    let folded = fold_array_range(index, select)?;
    let (index, select) = folded.as_ref().map_or((index, select), |(i, s)| (i, s));
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

    let mut base_offset = 0usize;
    let mut processed_count = 0;

    for (i, index_val) in index
        .expressions()
        .chain(&select.0)
        .take(geometry.dimension_count)
        .enumerate()
    {
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
        let Some(anchor_expr) = select.0.last() else {
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
    for expr in index.expressions() {
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

fn dimensions_to_strides(strides: &mut [usize]) -> Result<usize, ParserError> {
    let mut current_stride = 1usize;
    for i in (0..strides.len()).rev() {
        let dimension = strides[i];
        if dimension == 0 {
            return Err(ParserError::illegal_context(
                "variable dimensions",
                format!("dimension {i} has zero width"),
                None,
            ));
        }
        strides[i] = current_stride;
        current_stride = current_stride.checked_mul(dimension).ok_or_else(|| {
            ParserError::illegal_context(
                "variable dimensions",
                format!(
                    "dimension {dimension} times accumulated stride {current_stride} overflows usize"
                ),
                None,
            )
        })?;
    }
    Ok(current_stride)
}

pub fn get_dimensions_and_strides(
    module: &Module,
    var_id: VarId,
) -> Result<(Vec<usize>, Vec<usize>, usize), ParserError> {
    let dims = collect_dims(module, var_id)?;
    let mut strides = dims.clone();
    let total_width = dimensions_to_strides(&mut strides)?;
    Ok((dims, strides, total_width))
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

/// Bit offset of a variable select as an expression over its index
/// expressions, together with the select's geometry. Static selects yield a
/// literal offset.
pub fn select_offset_expr(
    module: &Module,
    var_id: VarId,
    index: &VarIndex,
    select: &VarSelect,
) -> Result<(Expression, SelectGeometry), ParserError> {
    let folded = fold_array_range(index, select)?;
    let (index, select) = folded.as_ref().map_or((index, select), |(i, s)| (i, s));
    let geometry = select_geometry(module, var_id, index, select)?;
    let mut indices = index.indices.clone();
    indices.extend(select.0.iter().cloned());
    let token = TokenRange::default();
    let ct = || Box::new(Comptime::create_unknown(token));
    let offset_literal =
        |value: usize| Expression::create_value(Value::new(value as u64, 64, false), token);
    let scaled = |expr: Expression, stride: usize| {
        if stride == 1 {
            expr
        } else {
            Expression::Binary(
                Box::new(expr),
                Op::Mul,
                Box::new(offset_literal(stride)),
                ct(),
            )
        }
    };

    let mut offset = None;
    for (dimension, expr) in indices
        .iter()
        .take(geometry.dimension_count)
        .cloned()
        .enumerate()
    {
        let term = scaled(expr, geometry.strides[dimension]);
        offset = Some(match offset {
            Some(offset) => Expression::Binary(Box::new(offset), Op::Add, Box::new(term), ct()),
            None => term,
        });
    }
    if let Some((op, range)) = &select.1 {
        let anchor = indices
            .get(geometry.dimension_count)
            .expect("validated part-select anchor is present");
        let (_, lsb) = op.eval_expr(anchor, range);
        let term = scaled(lsb, geometry.strides[geometry.dimension_count]);
        offset = Some(match offset {
            Some(offset) => Expression::Binary(Box::new(offset), Op::Add, Box::new(term), ct()),
            None => term,
        });
    }
    Ok((offset.unwrap_or_else(|| offset_literal(0)), geometry))
}

/// Build a read-modify-write expression for a partial assignment whose
/// destination contains a dynamic index or select.
pub fn build_dynamic_partial_assign_expr(
    module: &Module,
    dst: &AssignDestination,
    rhs: Expression,
    old_value: Expression,
) -> Result<Expression, ParserError> {
    let (offset, geometry) = select_offset_expr(module, dst.id, &dst.index, &dst.select)?;
    let token = TokenRange::default();
    let ct = || Box::new(Comptime::create_unknown(token));

    let mask_big = (BigUint::from(1u8) << geometry.selected_width) - BigUint::from(1u8);
    let mask = Expression::create_value(
        Value::new_biguint(mask_big, geometry.total_width, false),
        token,
    );
    let shifted_mask = Expression::Binary(
        Box::new(mask.clone()),
        Op::LogicShiftL,
        Box::new(offset.clone()),
        ct(),
    );
    let cleared = Expression::Binary(
        Box::new(old_value),
        Op::BitAnd,
        Box::new(Expression::Unary(Op::BitNot, Box::new(shifted_mask), ct())),
        ct(),
    );
    let masked_rhs = Expression::Binary(Box::new(rhs), Op::BitAnd, Box::new(mask), ct());
    let shifted_rhs = Expression::Binary(
        Box::new(masked_rhs),
        Op::LogicShiftL,
        Box::new(offset),
        ct(),
    );
    Ok(Expression::Binary(
        Box::new(cleared),
        Op::BitOr,
        Box::new(shifted_rhs),
        ct(),
    ))
}
