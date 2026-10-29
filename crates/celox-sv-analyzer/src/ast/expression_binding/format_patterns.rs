//! `%p`, the assignment pattern format (IEEE 1800-2023 21.2.1.6).
//!
//! The analyzer rewrites each `%p` of a literal format into a conversion the
//! formatter has: an integral value prints as `%0d`, a real as an unformatted
//! real, and a string, class handle or container as the `%s` of its pattern
//! text. Types whose patterns are not supported yet are rejected.

use super::*;
use crate::system_functions::ContainerLayout;

/// How `%p` prints a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ast) enum Pattern {
    Integral,
    String,
    /// A class handle, or with its layout a container.
    Handle(Option<ContainerLayout>),
}

/// The conversions of a format that take an argument (21.2.1.2).
const CONVERSIONS: &[u8] = b"bBoOhHxXdDiIcCsStTeEfFgGpP";

/// Whether `template` has a `%p` conversion.
pub(in crate::ast) fn has_pattern(template: &[u8]) -> bool {
    let mut found = false;
    let _ = rewrite_template(template, |_| {
        found = true;
        Ok(&b""[..])
    });
    found
}

/// `template` with each `%p` conversion, flags and width included, replaced
/// by the conversion `replace` gives for the index of its argument among the
/// arguments the template converts.
pub(in crate::ast) fn rewrite_template(
    template: &[u8],
    mut replace: impl FnMut(usize) -> Converted<&'static [u8]>,
) -> Converted<Vec<u8>> {
    let mut out = Vec::with_capacity(template.len());
    let mut position = 0;
    let mut index = 0;
    while position < template.len() {
        let byte = template[position];
        if byte != b'%' {
            out.push(byte);
            position += 1;
            continue;
        }
        let start = position;
        position += 1;
        while template
            .get(position)
            .is_some_and(|byte| b"-+ #0".contains(byte))
        {
            position += 1;
        }
        while template.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if template.get(position) == Some(&b'.') {
            position += 1;
            while template.get(position).is_some_and(u8::is_ascii_digit) {
                position += 1;
            }
        }
        let Some(&spec) = template.get(position) else {
            out.extend_from_slice(&template[start..]);
            break;
        };
        position += 1;
        if matches!(spec, b'p' | b'P') {
            out.extend_from_slice(replace(index)?);
        } else {
            out.extend_from_slice(&template[start..position]);
        }
        if CONVERSIONS.contains(&spec) {
            index += 1;
        }
    }
    Ok(out)
}

/// How `%p` prints native expression `id`, bound as `value` (before its
/// names are renamed).
pub(in crate::ast) fn pattern(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    value: &Expr,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Pattern> {
    let expression = &native.expression(ungroup(id, native)).kind;
    // A conditional has the type of its operands (11.4.11).
    if let ExprKind::Conditional {
        then_expression,
        else_expression,
        ..
    } = expression
        && let Expr::Mux {
            then_expr,
            else_expr,
            ..
        } = value
    {
        // A string literal is integral unless the other operand is a string
        // (5.9, 6.16), and an enum operand is integral unless the other
        // operand has the same enumerated type (11.4.11).
        let enum_valued = enums::domain(tree, id, native, dimensions)?.is_some();
        let operand = |id, value| {
            if !enum_valued && enums::domain(tree, id, native, dimensions)?.is_some() {
                return Ok(Pattern::Integral);
            }
            Ok(match pattern(tree, id, value, native, dimensions)? {
                Pattern::String
                    if matches!(&native.expression(ungroup(id, native)).kind,
                        ExprKind::Literal(text) if text.starts_with('"')) =>
                {
                    Pattern::Integral
                }
                pattern => pattern,
            })
        };
        let then_pattern = operand(*then_expression, then_expr)?;
        let else_pattern = operand(*else_expression, else_expr)?;
        return Ok(match (then_pattern, else_pattern) {
            (Pattern::Handle(Some(layout)), _) | (_, Pattern::Handle(Some(layout))) => {
                Pattern::Handle(Some(layout))
            }
            (Pattern::Handle(None), _) | (_, Pattern::Handle(None)) => Pattern::Handle(None),
            (Pattern::String, _) | (_, Pattern::String) => Pattern::String,
            (Pattern::Integral, Pattern::Integral) => Pattern::Integral,
        });
    }
    // An enum value: a variable, constant, cast, method or call of an
    // enumerated type.
    if matches!(expression, ExprKind::Call { expression, .. }
        if direct::call_name(&tree.state, *expression, native)
            .is_some_and(|name| returns_enum(&name, dimensions)))
        || enums::domain(tree, id, native, dimensions)?.is_some()
    {
        return Err(unsupported("%p of an enum value"));
    }
    if strings::string_value(value, dimensions)
        || strings::native_string(tree, id, native, dimensions)
        || matches!(expression, ExprKind::Literal(text) if text.starts_with('"'))
    {
        return Ok(Pattern::String);
    }
    if matches!(expression, ExprKind::Literal(text) if text == "null") {
        return Ok(Pattern::Handle(None));
    }
    if let Some(container) = containers::native_type(id, native, tree, dimensions)? {
        let element = &container.element;
        if element.is_enum || element.enum_domain.is_some() || element.packed_enum {
            return Err(unsupported("%p of a container of enum values"));
        }
        if matches!(element.kind, TypeKind::Real | TypeKind::Shortreal) {
            return Err(unsupported("%p of a container of real values"));
        }
        if !element.members.is_empty()
            || element.unpacked_struct.is_some()
            || element.container.is_some()
            || !element.unpacked_ranges.is_empty()
        {
            return Err(unsupported("%p of a container of aggregate elements"));
        }
        if container
            .key
            .as_ref()
            .is_some_and(|key| key.is_enum || key.enum_domain.is_some() || key.packed_enum)
        {
            return Err(unsupported("%p of an associative array with enum keys"));
        }
        if container
            .key
            .as_ref()
            .is_some_and(|key| key.class.is_some())
        {
            return Err(unsupported(
                "%p of an associative array with class handle keys",
            ));
        }
        return Ok(Pattern::Handle(Some(container.layout)));
    }
    if objects::native_class(tree, id, native, dimensions).is_some()
        || handle_value(value, dimensions)
        || matches!(expression, ExprKind::Call { expression, .. }
            if direct::call_name(&tree.state, *expression, native)
                .is_some_and(|name| returns_handle(&name, dimensions)))
    {
        return Ok(Pattern::Handle(None));
    }
    // An element selected from or popped off an unpacked array or container
    // has its element type.
    let base = match expression {
        ExprKind::Select { expression, .. } => Some(*expression),
        ExprKind::Call { expression, .. } => match &native
            .expression(ungroup(*expression, native))
            .kind
        {
            ExprKind::Member {
                expression, name, ..
            } if matches!(name.spelling.as_str(), "pop_front" | "pop_back") => Some(*expression),
            _ => None,
        },
        _ => None,
    };
    if let Some(base) = base {
        if let Some(container) = containers::native_type(base, native, tree, dimensions)? {
            let element = &container.element;
            if element.is_enum || element.enum_domain.is_some() || element.packed_enum {
                return Err(unsupported("%p of an enum value"));
            }
            if element.class.is_some() {
                return Ok(Pattern::Handle(None));
            }
            if !element.members.is_empty()
                || element.unpacked_struct.is_some()
                || element.container.is_some()
                || !element.unpacked_ranges.is_empty()
            {
                return Err(unsupported("%p of a structure or unpacked array"));
            }
        } else if let Some((shape, indices)) = fixed_selection(id, native, dimensions)
            && !shape.unpacked.is_empty()
        {
            // A selection from a fixed-size unpacked array: an index of each
            // unpacked dimension selects an element, which further selects
            // take bits of; fewer indices, or a slice, leave an array.
            match indices {
                Some(indices) if indices > shape.unpacked.len() => {}
                Some(indices) if indices == shape.unpacked.len() => {
                    if shape.enum_domain.is_some() || shape.packed_enum {
                        return Err(unsupported("%p of an enum value"));
                    }
                    if !shape.members.is_empty() || shape.unpacked_struct.is_some() {
                        return Err(unsupported("%p of a structure or unpacked array"));
                    }
                }
                _ => return Err(unsupported("%p of a structure or unpacked array")),
            }
        }
    }
    // A cast to, or a call of a function that returns, a structure or an
    // unpacked array.
    let aggregate = |ty: &Type| {
        !ty.members.is_empty() || ty.unpacked_struct.is_some() || !ty.unpacked_ranges.is_empty()
    };
    let aggregate_value = match expression {
        ExprKind::Cast { target, .. } => {
            enums::cast_type(tree, *target, native, dimensions)?.is_some_and(|ty| aggregate(&ty))
        }
        ExprKind::Call { expression, .. } => direct::call_name(&tree.state, *expression, native)
            .is_some_and(|name| returns_aggregate(&name, dimensions, aggregate)),
        _ => false,
    };
    // A value of unpacked structure type: a variable, member or property.
    if aggregate_value
        || unpacked_structs::native_struct(id, native, tree, dimensions)?.is_some()
        || objects::aggregate_property(tree, id, native, dimensions)?.is_some()
    {
        return Err(unsupported("%p of a structure or unpacked array"));
    }
    let checked = |is_enum: bool, aggregate: bool| {
        if is_enum {
            Err(unsupported("%p of an enum value"))
        } else if aggregate {
            Err(unsupported("%p of a structure or unpacked array"))
        } else {
            Ok(Pattern::Integral)
        }
    };
    if let ExprKind::Name(name) = expression
        && let Some(shape) = dimensions.get(&instances::normalize_identifier(&name.spelling))
    {
        return checked(
            shape.enum_domain.is_some() || shape.packed_enum,
            !shape.members.is_empty()
                || shape.unpacked_struct.is_some()
                || !shape.unpacked.is_empty(),
        );
    }
    if let Some((property, _)) = objects::property(tree, id, native, dimensions)? {
        let ty = &property.r#type;
        return checked(
            ty.is_enum || ty.enum_domain.is_some() || ty.packed_enum,
            !ty.members.is_empty()
                || ty.unpacked_struct.is_some()
                || !ty.unpacked_ranges.is_empty(),
        );
    }
    Ok(Pattern::Integral)
}

/// Whether bound `value` is a class handle: a handle variable or property,
/// an element of a container of handles, a call of a function that returns
/// one, or a conditional with such an operand.
fn handle_value(value: &Expr, dimensions: &PackedDimensions) -> bool {
    match value {
        Expr::Ident(name) => dimensions
            .get(name)
            .is_some_and(|shape| shape.class.is_some() && shape.unpacked.is_empty()),
        Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => handle_value(then_expr, dimensions) || handle_value(else_expr, dimensions),
        Expr::Call {
            callee: Callee::Subroutine(name),
            ..
        } => returns_handle(name, dimensions),
        Expr::Call {
            callee: Callee::Object(op),
            ..
        } => match &**op {
            ObjectOp::Load(field) => {
                field.handle == Some(crate::system_functions::HandleProperty::Object)
            }
            ObjectOp::Container {
                op: ContainerOp::Get | ContainerOp::PopFront | ContainerOp::PopBack,
                layout,
            } => layout.handle == Some(crate::system_functions::HandleProperty::Object),
            _ => false,
        },
        _ => false,
    }
}

/// Whether function `name` returns an enumerated value.
fn returns_enum(name: &str, dimensions: &PackedDimensions) -> bool {
    dimensions
        .functions
        .get(name)
        .is_some_and(|function| function.return_enum_domain.is_some())
        || dimensions
            .function_return_types
            .get(name)
            .is_some_and(|metadata| metadata.enum_domain.is_some())
}

/// The shape of the variable that native selection `id` selects from, and
/// the number of indices that precede the first slice, all of them when
/// there is no slice, or `None` when an unpacked dimension is sliced.
fn fixed_selection<'a>(
    id: ExprId,
    native: &Declarations,
    dimensions: &'a PackedDimensions,
) -> Option<(&'a VariableDimensions, Option<usize>)> {
    let mut selects = Vec::new();
    let mut current = ungroup(id, native);
    let name = loop {
        match &native.expression(current).kind {
            ExprKind::Select {
                expression,
                dimension,
            } => {
                selects.push(matches!(dimension.kind, DimensionKind::Index(_)));
                current = ungroup(*expression, native);
            }
            ExprKind::Name(name) => break name,
            _ => return None,
        }
    };
    let shape = dimensions.get(&instances::normalize_identifier(&name.spelling))?;
    // Outermost select last: the first ones apply to the unpacked dimensions.
    selects.reverse();
    let indices = selects.iter().take_while(|index| **index).count();
    let sliced = indices < selects.len() && indices < shape.unpacked.len();
    Some((shape, (!sliced).then_some(selects.len())))
}

/// Whether function `name` returns a value of a type that `aggregate` holds
/// for.
fn returns_aggregate(
    name: &str,
    dimensions: &PackedDimensions,
    aggregate: impl Fn(&Type) -> bool,
) -> bool {
    dimensions
        .functions
        .get(name)
        .and_then(|function| function.return_type.as_ref())
        .or_else(|| {
            dimensions
                .function_return_types
                .get(name)
                .and_then(|metadata| metadata.pattern_type.as_ref())
        })
        .is_some_and(aggregate)
}

/// Whether function `name` returns a class handle.
fn returns_handle(name: &str, dimensions: &PackedDimensions) -> bool {
    dimensions
        .functions
        .get(name)
        .is_some_and(|function| function.return_handle)
}

/// The conversion that prints `value` as `%p` does, and the argument it
/// converts.
pub(in crate::ast) fn pattern_argument(
    pattern: Pattern,
    value: Expr,
    dimensions: &PackedDimensions,
) -> (&'static [u8], Expr) {
    let quote = || Expr::Literal("8'h22".into());
    match pattern {
        // A real prints as an unformatted `$display` prints it.
        Pattern::Integral if real_value(&value, dimensions) => (b"%#g", value),
        Pattern::Integral => (b"%0d", value),
        Pattern::String => (
            b"%s",
            strings::call(
                StringOp::Concat,
                strings::operands(StringOp::Concat, vec![quote(), value, quote()], dimensions),
            ),
        ),
        Pattern::Handle(layout) => (b"%s", strings::call(StringOp::Pattern(layout), vec![value])),
    }
}

/// A literal format and the values it converts, with each `%p` rewritten.
/// `ids` are the native values, `values` the bound ones.
pub(in crate::ast) fn rewrite_format(
    template: &[u8],
    ids: &[ExprId],
    values: &mut [Expr],
    tree: &AnalysisContext<'_>,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Vec<u8>> {
    rewrite_template(template, |index| {
        let (Some(&id), Some(value)) = (ids.get(index), values.get_mut(index)) else {
            // A conversion without an argument prints nothing.
            return Ok(&b"%0d"[..]);
        };
        let pattern = pattern(tree, id, value, native, dimensions)?;
        let (conversion, argument) = pattern_argument(
            pattern,
            std::mem::replace(value, Expr::Literal("0".into())),
            dimensions,
        );
        *value = argument;
        Ok(conversion)
    })
}

/// The packed literal of `bytes`, most significant first, as a string
/// literal is (5.9).
pub(in crate::ast) fn bytes_literal(bytes: &[u8]) -> Expr {
    Expr::Literal(format!(
        "{}'h{}",
        (bytes.len() * 8).max(8),
        num_bigint::BigUint::from_bytes_be(bytes).to_str_radix(16)
    ))
}

/// `$sformatf` of the bound `operands`, a format and its values, whose native
/// expressions are `ids` (21.3.3). A literal format has its `%p` conversions
/// rewritten.
pub(in crate::ast) fn format(
    ticks_per_unit: u64,
    ids: &[ExprId],
    mut operands: Vec<Expr>,
    tree: &AnalysisContext<'_>,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    if let Some((&format, values)) = ids.split_first()
        && let ExprKind::Literal(text) = &native.expression(ungroup(format, native)).kind
        && let Some(bytes) = string_literal_bytes(text)
        && has_pattern(&bytes)
        && let Some((first, rest)) = operands.split_first_mut()
    {
        let template = rewrite_format(&bytes, values, rest, tree, native, dimensions)?;
        *first = bytes_literal(&template);
    }
    Ok(strings::call(StringOp::Format { ticks_per_unit }, operands))
}
