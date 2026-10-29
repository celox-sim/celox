//! Streaming concatenations (IEEE 1800-2023 11.4.14). A stream of
//! fixed-size data binds to a [`Callee::Stream`] call, a permutation of the
//! bits of its operands. A stream that holds or fills dynamically sized data
//! (queues, dynamic and associative arrays, strings) is built in a heap
//! stream by [`StreamOp`] operations, so only these streams pay for it.
use super::*;
use celox_sv_syntax::{Dimension, DimensionKind, ExprId, ExprKind};
use system_functions::{
    ContainerKind, ContainerLayout, HandleProperty, Stream, StreamArg, StreamItem, StreamOp,
    StreamPack, StreamRange,
};

/// A heap stream operation.
fn stream_op(op: StreamOp, args: Vec<Expr>) -> Expr {
    classes::object_call(ObjectOp::Stream(op), args)
}

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

/// What receives a stream.
#[derive(Clone, Copy)]
pub(in crate::ast) enum Receiver {
    /// A fixed-size variable, which receives the stream aligned to its left.
    Fixed(StreamArg),
    /// A queue or dynamic array, resized to hold the stream.
    Container(ContainerLayout),
    String,
    /// The integral type of a bit-stream cast: a stream of fixed-size data
    /// keeps its own width for the cast to convert; a dynamically sized one
    /// shall have the type's width (6.24.3).
    Cast(usize),
}

/// The layout of a container that a stream can fill: a queue or dynamic
/// array of integral or string elements.
fn stream_container(container: &ContainerType) -> Converted<ContainerLayout> {
    let layout = container.layout;
    if matches!(layout.kind, ContainerKind::Associative { .. }) {
        // 6.24.3: an associative array is illegal as a destination type.
        return Err(invalid("an associative array cannot receive a stream"));
    }
    match layout.handle {
        Some(HandleProperty::Object) => Err(unsupported(
            "streaming concatenation of class handles or structures in a container",
        )),
        // How a stream divides into strings is not defined by its bits.
        Some(HandleProperty::String) => Err(unsupported(
            "streaming concatenation into a container of strings",
        )),
        None => Ok(layout),
    }
}

/// The receiver of a variable of `shape` of bit-stream type.
pub(in crate::ast) fn receiver(
    shape: &VariableDimensions,
    state: &AnalysisState,
    dims: &PackedDimensions,
) -> Converted<Receiver> {
    if let Some(container) = &shape.container {
        return Ok(Receiver::Container(stream_container(container)?));
    }
    if shape.string {
        return Ok(Receiver::String);
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
        return Ok(Receiver::Fixed(StreamArg {
            width: element * count,
            bits: None,
            element: Some(element),
        }));
    }
    patterns::shape_width(state, shape, dims)
        .map(|width| Receiver::Fixed(StreamArg::packed(width)))
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
    Receiver::Fixed(StreamArg {
        width,
        bits: None,
        element,
    })
}

/// The first element and the number of elements that `with [range]` selects
/// in a queue or dynamic array, evaluated when the array is streamed
/// (11.4.14.4).
fn dynamic_range(
    range: &Dimension,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<(Expr, Expr, StreamRange)> {
    let value = |id| bind_value(id, native, tree, dims, None);
    Ok(match range.kind {
        DimensionKind::Index(id) => (value(id)?, Expr::Literal("32'sd1".into()), StreamRange::Up),
        DimensionKind::Range { left, right } => (value(left)?, value(right)?, StreamRange::Bounds),
        DimensionKind::Indexed {
            base,
            width,
            ref operator,
        } => (
            value(base)?,
            value(width)?,
            if operator == "+:" {
                StreamRange::Up
            } else {
                StreamRange::Down
            },
        ),
        _ => return Err(unsupported("streaming concatenation with range")),
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

/// One operand of a stream: fixed-size data, or dynamically sized data
/// with the arguments of its [`StreamItem`].
enum Item {
    Fixed(Expr, StreamArg),
    Dynamic(StreamItem, Vec<Expr>),
}

/// The items of one operand of a stream.
fn operand(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Vec<Item>> {
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
        return Ok(vec![match packed(id, native, tree, dims)? {
            Packed::Fixed(value, width) => Item::Fixed(value, StreamArg::packed(width)),
            Packed::Dynamic(stream) => Item::Dynamic(StreamItem::Stream, vec![stream]),
        }]);
    }
    let shape = dimensions::operand_shape(id, tree, dims)?;
    // A container variable, or any expression of container type, such as a
    // bit-stream cast to a queue type.
    let container = match containers::native_type(id, native, tree, dims)? {
        Some(container) => Some(container),
        None => shape.as_ref().and_then(|shape| shape.container.clone()),
    };
    if let Some(container) = container {
        let layout = container.layout;
        if layout.handle == Some(HandleProperty::Object) {
            return Err(unsupported(
                "streaming concatenation of class handles or structures in a container",
            ));
        }
        let value = bind_value(id, native, tree, dims, None)?;
        let Some(range) = range else {
            return Ok(vec![Item::Dynamic(
                StreamItem::Container {
                    layout,
                    range: None,
                },
                vec![value],
            )]);
        };
        if matches!(layout.kind, ContainerKind::Associative { .. }) {
            return Err(unsupported(
                "streaming concatenation with range on an associative array",
            ));
        }
        let (first, second, form) = dynamic_range(range, native, tree, dims)?;
        return Ok(vec![Item::Dynamic(
            StreamItem::Container {
                layout,
                range: Some(form),
            },
            vec![value, first, second],
        )]);
    }
    if let Some(shape) = &shape
        && !shape.string
    {
        check_fixed(shape)?;
    }
    let value = bind_value(id, native, tree, dims, None)?;
    if real_value(&value, dims) {
        return Err(invalid("a real value is not of bit-stream type"));
    }
    if shape.as_ref().is_some_and(|shape| shape.string) || strings::string_value(&value, dims) {
        if range.is_some() {
            return Err(invalid("with applies to a one-dimensional unpacked array"));
        }
        // 6.24.3: a string streams as a dynamic array of bytes.
        return Ok(vec![Item::Dynamic(StreamItem::String, vec![value])]);
    }
    let array = match &shape {
        Some(shape) => fixed_array(shape, &tree.state, dims)?.map(|array| (array, shape)),
        None => None,
    };
    if let Expr::Concat(leaves) = &value
        && shape
            .as_ref()
            .is_some_and(|shape| shape.unpacked_struct.is_some())
    {
        // Each member streams as fixed-size data.
        for leaf in leaves {
            let Expr::Ident(name) = leaf else {
                return Err(unsupported(
                    "streaming concatenation of an unpacked structure",
                ));
            };
            let leaf = dims
                .get(name)
                .ok_or_else(|| unsupported("streaming concatenation of an unpacked structure"))?;
            check_fixed(leaf)?;
            if !leaf.unpacked.is_empty() {
                return Err(unsupported(
                    "streaming concatenation of an unpacked structure with array members",
                ));
            }
        }
    }
    let Some(((element, count), shape)) = array else {
        if range.is_some() {
            return Err(invalid("with applies to a one-dimensional unpacked array"));
        }
        let width = expr_static_width(&tree.state, &value, dims)
            .filter(|width| *width > 0)
            .ok_or_else(|| unsupported("streaming concatenation operand of unknown width"))?;
        return Ok(vec![Item::Fixed(value, StreamArg::packed(width))]);
    };
    let width = element * count;
    let Some(range) = range else {
        return Ok(vec![Item::Fixed(
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
            parts.push(Item::Fixed(
                value.clone(),
                StreamArg {
                    width,
                    bits: Some((position as usize * element, size)),
                    element: Some(element),
                },
            ));
            position = end + 1;
        } else {
            parts.push(Item::Fixed(missing.clone(), StreamArg::packed(element)));
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

/// A stream: the [`Stream`] call of fixed-size data and its width, or the
/// heap stream of dynamically sized data.
enum Packed {
    Fixed(Expr, usize),
    Dynamic(Expr),
}

/// The heap stream of `items`, re-ordered by `slice` or, to `unorder`, put
/// back in the order that re-ordering would undo, after keeping its first
/// `limit` bits.
fn heap_stream(
    items: Vec<Item>,
    slice: Option<usize>,
    unorder: bool,
    limit: Option<usize>,
) -> Expr {
    let mut kinds = Vec::with_capacity(items.len());
    let mut args = Vec::new();
    for item in items {
        match item {
            Item::Fixed(value, arg) => {
                kinds.push(StreamItem::Bits(arg));
                args.push(value);
            }
            Item::Dynamic(kind, values) => {
                kinds.push(kind);
                args.extend(values);
            }
        }
    }
    stream_op(
        StreamOp::Pack(Box::new(StreamPack {
            items: kinds,
            slice,
            unorder,
            limit,
        })),
        args,
    )
}

/// The stream of streaming concatenation `id`.
fn packed(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Packed> {
    let (slice, expressions) = parts(id, native, tree, dims)?;
    let mut items = Vec::new();
    for expression in expressions {
        items.extend(operand(expression, native, tree, dims)?);
    }
    if items.iter().any(|item| matches!(item, Item::Dynamic(..))) {
        return Ok(Packed::Dynamic(heap_stream(items, slice, false, None)));
    }
    let (values, args): (Vec<_>, Vec<_>) = items
        .into_iter()
        .map(|item| match item {
            Item::Fixed(value, arg) => (value, arg),
            Item::Dynamic(..) => unreachable!("checked above"),
        })
        .unzip();
    let width = args.iter().map(StreamArg::stream_width).sum::<usize>();
    Ok(Packed::Fixed(
        call(
            Stream {
                args,
                slice,
                unpack: false,
                targets: vec![StreamArg::packed(width)],
            },
            values,
        ),
        width,
    ))
}

/// The value of stream `id` that `receiver` receives, or its own stream
/// when it has none (a stream of fixed-size data in a bit-stream cast).
pub(in crate::ast) fn pack(
    id: ExprId,
    receiver: Option<Receiver>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Expr> {
    Ok(match (packed(id, native, tree, dims)?, receiver) {
        (Packed::Fixed(value, _), None | Some(Receiver::Cast(_))) => value,
        (Packed::Fixed(_, width), Some(Receiver::Fixed(target))) if target.width < width => {
            return Err(invalid(format!(
                "the {width}-bit stream is wider than its {}-bit target",
                target.width
            )));
        }
        (Packed::Fixed(mut value, _), Some(Receiver::Fixed(target))) => {
            if let Expr::Call {
                callee: Callee::Stream(stream),
                ..
            } = &mut value
            {
                stream.targets = vec![target];
            }
            value
        }
        (Packed::Fixed(value, width), Some(receiver)) => convert(
            heap_stream(
                vec![Item::Fixed(value, StreamArg::packed(width))],
                None,
                false,
                None,
            ),
            receiver,
        ),
        (Packed::Dynamic(_), None) => {
            return Err(unsupported(
                "dynamically sized streaming concatenation without a target type",
            ));
        }
        (Packed::Dynamic(stream), Some(receiver)) => convert(stream, receiver),
    })
}

/// Heap stream `stream` converted to `receiver` (11.4.14).
fn convert(stream: Expr, receiver: Receiver) -> Expr {
    match receiver {
        Receiver::Fixed(target) => {
            let bits = stream_op(
                StreamOp::ToBits {
                    width: target.width,
                    exact: false,
                },
                vec![stream],
            );
            if target.element.is_none() {
                return bits;
            }
            // An unpacked array receives the stream in its layout.
            call(
                Stream {
                    args: vec![StreamArg::packed(target.width)],
                    slice: None,
                    unpack: false,
                    targets: vec![target],
                },
                vec![bits],
            )
        }
        Receiver::Cast(width) => stream_op(StreamOp::ToBits { width, exact: true }, vec![stream]),
        Receiver::Container(layout) => stream_op(StreamOp::ToContainer(layout), vec![stream]),
        Receiver::String => stream_op(StreamOp::ToString, vec![stream]),
    }
}

/// How one variable of an unpack target takes its bits from the stream.
pub(in crate::ast) enum Take {
    /// A fixed-size variable.
    Fixed(StreamArg),
    /// A queue or dynamic array resized to hold the unread bits but the
    /// last `reserve`, which the fixed-size variables after it take
    /// (11.4.14.4); `usize::MAX` leaves it empty.
    Rest {
        layout: ContainerLayout,
        reserve: usize,
    },
    /// The elements of a queue or dynamic array that `with` selects: its
    /// value, the first element and the number of elements.
    Range {
        layout: ContainerLayout,
        form: StreamRange,
        operands: Box<[Expr; 3]>,
    },
    /// A string, like [`Take::Rest`].
    String { reserve: usize },
}

/// An assignment to a streaming concatenation (11.4.14.3).
pub(in crate::ast) enum Unpack {
    /// Fixed-size variables, most significant first, and the value they
    /// receive in that layout.
    Fixed(Vec<LValue>, Expr),
    /// Variables some of which are dynamically sized: each takes its bits
    /// in turn from the heap stream `source`, the source put back in the
    /// order that packing the variables gives.
    Dynamic {
        source: Expr,
        targets: Vec<(LValue, Take)>,
    },
}

/// The value variable `take` receives from heap stream `stream`, and the
/// operation that reads its bits after it, if any.
pub(in crate::ast) fn take(stream: &Expr, take: Take) -> (Expr, Option<Expr>) {
    match take {
        Take::Fixed(target) => {
            let bits = stream_op(
                StreamOp::Peek(target.width),
                vec![stream.clone(), Expr::Literal("64'd0".into())],
            );
            let value = if target.element.is_some() {
                call(
                    Stream {
                        args: vec![StreamArg::packed(target.width)],
                        slice: None,
                        unpack: false,
                        targets: vec![target],
                    },
                    vec![bits],
                )
            } else {
                bits
            };
            (
                value,
                Some(stream_op(
                    StreamOp::Skip(target.width),
                    vec![stream.clone()],
                )),
            )
        }
        Take::Rest { layout, reserve } => (
            stream_op(StreamOp::TakeRest { layout, reserve }, vec![stream.clone()]),
            None,
        ),
        Take::Range {
            layout,
            form,
            operands,
        } => {
            let [current, first, second] = *operands;
            (
                stream_op(
                    StreamOp::TakeRange(layout, form),
                    vec![stream.clone(), current, first, second],
                ),
                None,
            )
        }
        Take::String { reserve } => (
            stream_op(StreamOp::TakeString { reserve }, vec![stream.clone()]),
            None,
        ),
    }
}

/// The free of heap stream `stream`.
pub(in crate::ast) fn free(stream: Expr) -> Expr {
    stream_op(StreamOp::Free, vec![stream])
}

/// An assignment to stream `left` from `right` (11.4.14.3), or `None` when
/// `left` is no stream.
pub(in crate::ast) fn unpack(
    left: ExprId,
    right: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Unpack>> {
    if !is_stream(left, native) {
        return Ok(None);
    }
    let (slice, expressions) = parts(left, native, tree, dims)?;
    let mut lvalues = Vec::new();
    let mut takes = Vec::new();
    let mut greedy = false;
    let mut pending = expressions.into_iter().rev().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
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
        let dynamic = shape.container.is_some() || shape.string;
        if let Some(range) = range {
            let Some(container) = &shape.container else {
                return Err(if shape.unpacked.len() == 1 {
                    unsupported("streaming concatenation with range on a fixed-size unpack target")
                } else {
                    invalid("with applies to a one-dimensional unpacked array")
                });
            };
            let layout = stream_container(container)?;
            let (first, second, form) = dynamic_range(range, native, tree, dims)?;
            takes.push(Take::Range {
                layout,
                form,
                operands: Box::new([bind_value(id, native, tree, dims, None)?, first, second]),
            });
            lvalues.push(variable_lvalue(id, native, tree, dims)?);
            continue;
        }
        if dynamic {
            // The first dynamically sized variable takes all the data the
            // fixed-size variables after it leave; the others stay empty.
            let reserve = if greedy { usize::MAX } else { 0 };
            greedy = true;
            takes.push(match &shape.container {
                Some(container) => Take::Rest {
                    layout: stream_container(container)?,
                    reserve,
                },
                None => Take::String { reserve },
            });
            lvalues.push(variable_lvalue(id, native, tree, dims)?);
            continue;
        }
        check_fixed(&shape)?;
        if let ExprKind::Name(name) = &native.expression(ungroup(id, native)).kind
            && let Some(leaves) =
                unpacked_structs::targets(&instances::normalize_identifier(&name.spelling), dims)
        {
            for leaf in leaves {
                let leaf_shape = dims
                    .get(leaf.name())
                    .ok_or_else(|| unsupported("streaming concatenation unpack target"))?;
                check_fixed(leaf_shape)?;
                if !leaf_shape.unpacked.is_empty() {
                    return Err(unsupported(
                        "streaming concatenation into an unpacked structure with array members",
                    ));
                }
                let width = dims
                    .get(leaf.name())
                    .and_then(|shape| patterns::shape_width(&tree.state, shape, dims))
                    .ok_or_else(|| unsupported("streaming concatenation unpack target"))?;
                takes.push(Take::Fixed(StreamArg::packed(width)));
                lvalues.push(leaf);
            }
            continue;
        }
        if shape.unpacked_struct.is_some() {
            return Err(unsupported(
                "streaming concatenation into an unpacked structure member",
            ));
        }
        let Receiver::Fixed(target) = receiver(&shape, &tree.state, dims)? else {
            unreachable!("a fixed-size variable receives fixed-size data");
        };
        takes.push(Take::Fixed(target));
        lvalues.push(variable_lvalue(id, native, tree, dims)?);
    }
    let source = if is_stream(right, native) {
        vec![match packed(right, native, tree, dims)? {
            Packed::Fixed(value, width) => Item::Fixed(value, StreamArg::packed(width)),
            Packed::Dynamic(stream) => Item::Dynamic(StreamItem::Stream, vec![stream]),
        }]
    } else {
        operand(right, native, tree, dims)?
    };
    let fixed = takes.iter().all(|take| matches!(take, Take::Fixed(_)));
    if !fixed || source.iter().any(|item| matches!(item, Item::Dynamic(..))) {
        // A greedy variable leaves the bits of the fixed-size ones after it.
        let mut after = 0usize;
        for take in takes.iter_mut().rev() {
            match take {
                Take::Fixed(target) => after += target.width,
                Take::Rest { reserve, .. } | Take::String { reserve } if *reserve == 0 => {
                    *reserve = after;
                }
                _ => {}
            }
        }
        return Ok(Some(Unpack::Dynamic {
            // Fixed-size variables take the source's leading bits, which
            // alone are put back in order (11.4.14.3).
            source: heap_stream(source, slice, true, fixed.then_some(after)),
            targets: lvalues.into_iter().zip(takes).collect(),
        }));
    }
    let targets = takes
        .into_iter()
        .map(|take| match take {
            Take::Fixed(target) => target,
            _ => unreachable!("checked above"),
        })
        .collect::<Vec<_>>();
    let width = targets.iter().map(|target| target.width).sum::<usize>();
    let Some(Item::Fixed(value, arg)) = source.into_iter().next() else {
        unreachable!("a fixed-size source is one item");
    };
    // The source is of bit-stream type: an unpacked array streams its
    // elements in order.
    let (source, arg) = match arg.element {
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
    };
    if arg.width < width {
        return Err(invalid(format!(
            "the {}-bit source has fewer bits than the {width}-bit stream",
            arg.width
        )));
    }
    Ok(Some(Unpack::Fixed(
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
