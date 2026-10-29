//! Streaming concatenations of fixed-size data (IEEE 1800-2023 11.4.14),
//! bound to [`Callee::Stream`] calls.
use super::*;
use celox_sv_syntax::{Dimension, DimensionKind, ExprId, ExprKind};
use system_functions::{Stream, StreamArg};

fn invalid(detail: impl Into<String>) -> AnalyzerError {
    AnalyzerError::InvalidStream(detail.into())
}

fn call(stream: Stream, args: Vec<Expr>) -> Expr {
    Expr::Call {
        callee: Callee::Stream(Box::new(stream)),
        args: args.into_iter().map(Some).collect(),
        arg_order: Vec::new(),
    }
}

/// Whether native expression `id` is a streaming concatenation.
pub(in crate::ast) fn is_stream(id: ExprId, native: &Declarations) -> bool {
    matches!(
        native.expression(ungroup(id, native)).kind,
        ExprKind::Streaming { .. }
    )
}

fn constant(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
    what: &str,
) -> Converted<i128> {
    constant_binding::bind(id, tree, dims)?
        .and_then(|value| eval_ast_const_expr(&tree.state, &value, &dims.const_env))
        .ok_or_else(|| {
            unsupported(format!(
                "streaming concatenation {what} that is not constant"
            ))
        })
}

/// The block size of `slice` (11.4.14.2): a type's width, or a positive
/// constant.
fn slice_size(
    slice: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<usize> {
    let ty = match native.expression(slice).kind {
        ExprKind::Type(ty) => type_binding::bind(ty, tree, dims)?,
        _ => type_binding::argument_alias(slice, native)
            .and_then(|(name, dimensions)| dimensions.is_empty().then_some(name))
            .and_then(|name| dims.type_aliases.get(&name).cloned()),
    };
    if let Some(ty) = ty {
        return casts::expr_type_from_type(&tree.state, &ty, &dims.const_env)
            .map(|ty| ty.width)
            .filter(|width| *width > 0)
            .ok_or_else(|| unsupported("streaming concatenation slice type of unknown width"));
    }
    let size = constant(slice, tree, dims, "slice size")?;
    usize::try_from(size)
        .ok()
        .filter(|size| *size > 0)
        .ok_or_else(|| invalid(format!("the slice size {size} is not positive")))
}

/// The shape of a fixed-size unpacked array: the width of its elements and
/// their number.
fn fixed_array(
    shape: &VariableDimensions,
    state: &AnalysisState,
    dims: &PackedDimensions,
) -> Converted<Option<(usize, usize)>> {
    if shape.unpacked.is_empty() {
        return Ok(None);
    }
    let array = array_compatibility::unpacked_array_type(state, shape, dims)
        .ok_or_else(|| unsupported("streaming concatenation of an array of unknown size"))?;
    let count = array
        .dims
        .iter()
        .try_fold(1usize, |count, size| count.checked_mul(*size))
        .ok_or_else(|| unsupported("streaming concatenation of an array that is too large"))?;
    Ok(Some((array.element_width, count)))
}

/// Reject the kinds of data a fixed-size stream cannot hold.
fn check_fixed(shape: &VariableDimensions) -> Converted<()> {
    if shape.container.is_some() {
        return Err(unsupported(
            "streaming concatenation of a dynamically sized array",
        ));
    }
    if shape.string {
        return Err(unsupported("streaming concatenation of a string"));
    }
    if shape.real {
        // 6.24.3: real is not a bit-stream type.
        return Err(invalid("a real value is not of bit-stream type"));
    }
    if shape.class.is_some() {
        return Err(unsupported("streaming concatenation of a class handle"));
    }
    Ok(())
}

/// What receives a packed stream.
#[derive(Clone, Copy)]
pub(in crate::ast) struct Receiver(StreamArg);

/// The receiver of a variable of `shape`: a fixed-size variable of
/// bit-stream type.
pub(in crate::ast) fn receiver(
    shape: &VariableDimensions,
    state: &AnalysisState,
    dims: &PackedDimensions,
) -> Converted<Receiver> {
    if shape.container.is_some() || shape.string {
        return Err(unsupported(
            "streaming concatenation into a dynamically sized variable",
        ));
    }
    if shape.real {
        return Err(invalid(
            "the target of a stream shall be of bit-stream type",
        ));
    }
    if shape.class.is_some() {
        return Err(unsupported("streaming concatenation into a class handle"));
    }
    if shape.unpacked_struct.is_some() {
        return Err(unsupported(
            "streaming concatenation into an unpacked structure",
        ));
    }
    if let Some((element, count)) = fixed_array(shape, state, dims)? {
        return Ok(Receiver(StreamArg {
            width: element * count,
            bits: None,
            element: Some(element),
        }));
    }
    patterns::shape_width(state, shape, dims)
        .map(|width| Receiver(StreamArg::packed(width)))
        .ok_or_else(|| unsupported("streaming concatenation into a variable of unknown width"))
}

/// The receiver of selection `lhs` of `width` bits. A selection of whole
/// elements of an unpacked array, such as a subarray, keeps the array's
/// layout: its first element is in the least significant bits.
pub(in crate::ast) fn selection_receiver(
    lhs: &LValue,
    width: usize,
    state: &AnalysisState,
    dims: &PackedDimensions,
) -> Receiver {
    let element = match lhs {
        LValue::Select { name, .. } => dims
            .get(name)
            .filter(|shape| !shape.unpacked.is_empty())
            .and_then(|shape| {
                patterns::shape_width(
                    state,
                    &VariableDimensions {
                        unpacked: Vec::new(),
                        ..shape.clone()
                    },
                    dims,
                )
            })
            .filter(|element| width > *element && width.is_multiple_of(*element)),
        LValue::Ident(_) => None,
    };
    Receiver(StreamArg {
        width,
        bits: None,
        element,
    })
}

/// The positions `first..=last` of the elements that `with [range]`
/// selects in a fixed-size array of `count` elements whose left bound is
/// `left` and whose right bound is `right`, in streaming order. Positions
/// outside `0..count` select no element.
fn with_positions(
    range: &Dimension,
    (left, right): (i128, i128),
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<(i128, i128)> {
    let index = |id| constant(id, tree, dims, "with range");
    let (low, high) = match range.kind {
        DimensionKind::Index(id) => {
            let index = index(id)?;
            (index, index)
        }
        DimensionKind::Range { left, right } => {
            let (left, right) = (index(left)?, index(right)?);
            (left.min(right), left.max(right))
        }
        DimensionKind::Indexed {
            base,
            width,
            ref operator,
        } => {
            let (base, width) = (index(base)?, index(width)?);
            if width <= 0 {
                return Err(invalid(format!(
                    "the with range width {width} is not positive"
                )));
            }
            if operator == "+:" {
                (base, base + width - 1)
            } else {
                (base - width + 1, base)
            }
        }
        _ => return Err(unsupported("streaming concatenation with range")),
    };
    // A descending array streams from its left bound downwards; an index
    // past either bound has a position outside the array.
    Ok(if left <= right {
        (low - left, high - left)
    } else {
        (left - high, left - low)
    })
}

/// The value and argument of one operand of a stream.
fn operand(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Vec<(Expr, StreamArg)>> {
    let (id, range) = match &native.expression(ungroup(id, native)).kind {
        ExprKind::StreamSelect {
            expression,
            dimension,
        } => (*expression, Some(&**dimension)),
        _ => (id, None),
    };
    if is_stream(id, native) {
        if range.is_some() {
            return Err(invalid("with applies to a one-dimensional unpacked array"));
        }
        let value = pack(id, None, native, tree, dims)?;
        let Expr::Call {
            callee: Callee::Stream(stream),
            ..
        } = &value
        else {
            unreachable!("a stream binds to a stream call");
        };
        let width = stream.width();
        return Ok(vec![(value, StreamArg::packed(width))]);
    }
    let shape = dimensions::operand_shape(id, tree, dims)?;
    if let Some(shape) = &shape {
        check_fixed(shape)?;
    }
    let value = bind_value(id, native, tree, dims, None)?;
    if real_value(&value, dims) {
        return Err(invalid("a real value is not of bit-stream type"));
    }
    if strings::string_value(&value, dims) {
        return Err(unsupported("streaming concatenation of a string"));
    }
    let array = match &shape {
        Some(shape) => fixed_array(shape, &tree.state, dims)?.map(|array| (array, shape)),
        None => None,
    };
    if let Expr::Concat(leaves) = &value
        && shape.as_ref().is_some_and(|shape| shape.unpacked_struct.is_some())
        && leaves.iter().any(|leaf| {
            !matches!(leaf, Expr::Ident(name) if dims.get(name).is_some_and(|leaf| leaf.unpacked.is_empty()))
        })
    {
        return Err(unsupported(
            "streaming concatenation of an unpacked structure with array members",
        ));
    }
    let Some(((element, count), shape)) = array else {
        if range.is_some() {
            return Err(invalid("with applies to a one-dimensional unpacked array"));
        }
        let width = expr_static_width(&tree.state, &value, dims)
            .filter(|width| *width > 0)
            .ok_or_else(|| unsupported("streaming concatenation operand of unknown width"))?;
        return Ok(vec![(value, StreamArg::packed(width))]);
    };
    let width = element * count;
    let Some(range) = range else {
        return Ok(vec![(
            value,
            StreamArg {
                width,
                bits: None,
                element: Some(element),
            },
        )]);
    };
    let [dimension] = shape.unpacked.as_slice() else {
        return Err(invalid("with applies to a one-dimensional unpacked array"));
    };
    let bound = |expr| {
        eval_ast_const_expr(&tree.state, expr, &dims.const_env)
            .ok_or_else(|| unsupported("streaming concatenation with an array of unknown bounds"))
    };
    let bounds = (bound(&dimension.left)?, bound(&dimension.right)?);
    let (first, last) = with_positions(range, bounds, tree, dims)?;
    // A pack streams the selected elements, and the value of a nonexistent
    // entry (Table 7-1) for each position past the array (11.4.14.4).
    let missing = Expr::Literal(if shape.is_2state {
        format!("{element}'b0")
    } else {
        format!("{element}'bx")
    });
    let mut parts = Vec::new();
    let mut position = first;
    while position <= last {
        if (0..count as i128).contains(&position) {
            let end = last.min(count as i128 - 1);
            let size = (end - position + 1) as usize * element;
            parts.push((
                value.clone(),
                StreamArg {
                    width,
                    bits: Some((position as usize * element, size)),
                    element: Some(element),
                },
            ));
            position = end + 1;
        } else {
            parts.push((missing.clone(), StreamArg::packed(element)));
            position += 1;
        }
    }
    Ok(parts)
}

/// The operator, slice size and stream expressions of stream `id`.
fn parts(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<(Option<usize>, Vec<ExprId>)> {
    let ExprKind::Streaming {
        operator,
        slice,
        expressions,
    } = &native.expression(ungroup(id, native)).kind
    else {
        unreachable!("checked by the caller");
    };
    let size = slice
        .map(|slice| slice_size(slice, native, tree, dims))
        .transpose()?;
    // `>>` streams the blocks left to right, ignoring the slice size.
    Ok((
        if operator == "<<" {
            Some(size.unwrap_or(1))
        } else {
            None
        },
        expressions.clone(),
    ))
}

/// The value of stream `id` that `receiver` receives, or its own stream
/// when it has none (a bit-stream cast, or a stream in a stream).
pub(in crate::ast) fn pack(
    id: ExprId,
    receiver: Option<Receiver>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Expr> {
    let (slice, expressions) = parts(id, native, tree, dims)?;
    let mut values = Vec::new();
    let mut args = Vec::new();
    for expression in expressions {
        for (value, arg) in operand(expression, native, tree, dims)? {
            values.push(value);
            args.push(arg);
        }
    }
    let width = args.iter().map(StreamArg::stream_width).sum::<usize>();
    let target = match receiver {
        Some(Receiver(target)) if target.width < width => {
            return Err(invalid(format!(
                "the {width}-bit stream is wider than its {}-bit target",
                target.width
            )));
        }
        Some(Receiver(target)) => target,
        None => StreamArg::packed(width),
    };
    Ok(call(
        Stream {
            args,
            slice,
            unpack: false,
            targets: vec![target],
        },
        values,
    ))
}

/// The targets and source of an assignment to stream `left` from `right`
/// (11.4.14.3): the variables in the stream, most significant first, and the
/// value they receive in that layout. `None` when `left` is no stream.
pub(in crate::ast) fn unpack(
    left: ExprId,
    right: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<(Vec<LValue>, Expr)>> {
    if !is_stream(left, native) {
        return Ok(None);
    }
    let (slice, expressions) = parts(left, native, tree, dims)?;
    let mut lvalues = Vec::new();
    let mut targets = Vec::new();
    let mut pending = expressions.into_iter().rev().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        if let ExprKind::StreamSelect { .. } = native.expression(ungroup(id, native)).kind {
            return Err(unsupported(
                "streaming concatenation with range on an unpack target",
            ));
        }
        if is_stream(id, native) {
            // A stream that keeps its order unpacks as its operands do.
            let (slice, inner) = parts(id, native, tree, dims)?;
            if slice.is_some() {
                return Err(unsupported(
                    "re-ordering streaming concatenation in an unpack target",
                ));
            }
            pending.extend(inner.into_iter().rev());
            continue;
        }
        let shape = dimensions::operand_shape(id, tree, dims)?
            .ok_or_else(|| unsupported("streaming concatenation unpack target"))?;
        check_fixed(&shape)?;
        if let ExprKind::Name(name) = &native.expression(ungroup(id, native)).kind
            && let Some(leaves) =
                unpacked_structs::targets(&instances::normalize_identifier(&name.spelling), dims)
        {
            for leaf in leaves {
                let width = dims
                    .get(leaf.name())
                    .and_then(|shape| patterns::shape_width(&tree.state, shape, dims))
                    .ok_or_else(|| unsupported("streaming concatenation unpack target"))?;
                targets.push(StreamArg::packed(width));
                lvalues.push(leaf);
            }
            continue;
        }
        if shape.unpacked_struct.is_some() {
            return Err(unsupported(
                "streaming concatenation into an unpacked structure member",
            ));
        }
        let Receiver(target) = receiver(&shape, &tree.state, dims)?;
        targets.push(target);
        lvalues.push(variable_lvalue(id, native, tree, dims)?);
    }
    let width = targets.iter().map(|target| target.width).sum::<usize>();
    let (source, arg) = if is_stream(right, native) {
        let value = pack(right, None, native, tree, dims)?;
        let width = expr_static_width(&tree.state, &value, dims).expect("a stream has a width");
        (value, StreamArg::packed(width))
    } else {
        // The source is of bit-stream type: an unpacked array streams its
        // elements in order.
        let mut operands = operand(right, native, tree, dims)?;
        let (value, arg) = operands
            .pop()
            .expect("an operand without a range is one value");
        match arg.element {
            Some(_) => {
                let width = arg.width;
                (
                    call(
                        Stream {
                            args: vec![arg],
                            slice: None,
                            unpack: false,
                            targets: vec![StreamArg::packed(width)],
                        },
                        vec![value],
                    ),
                    StreamArg::packed(width),
                )
            }
            None => (value, arg),
        }
    };
    if arg.width < width {
        return Err(invalid(format!(
            "the {}-bit source has fewer bits than the {width}-bit stream",
            arg.width
        )));
    }
    Ok(Some((
        lvalues,
        call(
            Stream {
                args: vec![arg],
                slice,
                unpack: true,
                targets,
            },
            vec![source],
        ),
    )))
}
