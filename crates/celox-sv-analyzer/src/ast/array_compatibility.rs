//! Assignment compatibility of unpacked arrays (IEEE 1800-2023 7.6).
//!
//! An unpacked array value is copied element by element, so it can only be
//! assigned to an array whose element type is equivalent (6.22.2) and whose
//! dimensions have the same element counts. Every assignment-like context
//! (10.8) follows this rule, including subroutine arguments of any direction
//! and the items of an assignment pattern.

use super::*;

use typecheck::UnpackedArrayType;

/// The identity of the element type of `shape` when it is not described by
/// its width, state count and signedness alone (IEEE 1800-2023 6.22.2): a
/// named type is equivalent only to itself.
fn element_identity(shape: &VariableDimensions) -> Option<String> {
    if let Some(class) = &shape.class {
        return Some(format!("class {class}"));
    }
    if shape.string {
        return Some("string".into());
    }
    if shape.real {
        return Some("real".into());
    }
    // The modport is part of the type (25.9).
    if let Some(interface) = &shape.virtual_interface {
        return Some(interface.spelling());
    }
    if let Some(aggregate) = &shape.unpacked_struct {
        return Some(format!("struct {}", aggregate.identity()));
    }
    let domain = shape.enum_domain.as_ref()?;
    // A named enumeration is its typedef; an anonymous one is told apart by
    // its members.
    Some(match type_names::declaration_of(shape) {
        Some(declaration) => format!("enum {declaration}"),
        None => format!(
            "enum {{{}}}",
            domain
                .members
                .iter()
                .map(|member| format!("{}={}", member.name, member.value))
                .collect::<Vec<_>>()
                .join(",")
        ),
    })
}

/// The type of an unpacked array of `shape`, or `None` when `shape` is not an
/// unpacked array or its type cannot be resolved.
pub(super) fn unpacked_array_type(
    state: &AnalysisState,
    shape: &VariableDimensions,
    dims: &PackedDimensions,
) -> Option<UnpackedArrayType> {
    if shape.unpacked.is_empty() {
        return None;
    }
    let count = |width: &ConstExpr| {
        eval_ast_const_expr(state, width, &dims.const_env)
            .and_then(|width| usize::try_from(width).ok())
    };
    Some(UnpackedArrayType {
        dims: shape
            .unpacked
            .iter()
            .map(|dimension| count(&dimension.width))
            .collect::<Option<_>>()?,
        element_width: shape.packed.iter().try_fold(1usize, |width, dimension| {
            width.checked_mul(count(&dimension.width)?)
        })?,
        signed: shape.signed,
        four_state: !shape.is_2state,
        element: element_identity(shape),
    })
}

/// How fixed-size unpacked arrays of types `actual` and `target` are
/// spelled if the first may not be assigned to the second (IEEE 1800-2023
/// 7.6): their elements are not equivalent types or they have another
/// number of elements.
fn incompatible_types(
    table: &semantic_types::TyTable,
    actual: semantic_types::TyId,
    target: semantic_types::TyId,
) -> Option<(String, String)> {
    let fixed = |id| {
        matches!(
            table.data(id),
            semantic_types::TyData::Array {
                kind: semantic_types::ArrayKind::Fixed(_),
                ..
            }
        )
    };
    (fixed(actual) && fixed(target) && table.assignable_array(actual, target).is_err())
        .then(|| (table.spell(actual), table.spell(target)))
}

/// How fixed-size unpacked arrays of `actual` and `target` are spelled if
/// the first may not be assigned to the second (see [`incompatible_types`]).
pub(super) fn incompatible(
    state: &AnalysisState,
    actual: &VariableDimensions,
    target: &VariableDimensions,
    dims: &PackedDimensions,
) -> Option<(String, String)> {
    let of_shape = |shape| semantic_types::of_shape(&state.types, state, shape, &dims.const_env);
    incompatible_types(&state.types, of_shape(actual)?, of_shape(target)?)
}

/// The shape of a variable `name` after `indices` index selects, when that
/// is still an unpacked array.
pub(super) fn selected_unpacked_shape(
    name: &str,
    indices: usize,
    dims: &PackedDimensions,
) -> Option<VariableDimensions> {
    let shape = dims.get(name)?;
    let unpacked = shape
        .unpacked
        .get(indices..)
        .filter(|rest| !rest.is_empty())?;
    // A subarray has the element type of the whole array; `$typename`
    // compares the declared dimensions before it uses the declared type.
    Some(VariableDimensions {
        container: None,
        unpacked: unpacked.to_vec(),
        ..shape.clone()
    })
}

/// The variable an expression selects from, as the dotted path of its
/// name, with how many index selects it makes and the element count of a
/// slice it ends with: a variable, one of its subarrays selected by
/// indices, or a slice of either with a constant element count. Other
/// expressions (member selects, calls, concatenations) are not resolved.
fn native_selection(
    mut id: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Option<(String, usize, Option<u128>)> {
    use celox_sv_syntax::{DimensionKind, ExprKind};
    let native = tree.declarations();
    let constant = |id| {
        constant_binding::bind(id, tree, dims)
            .ok()
            .flatten()
            .and_then(|value| eval_ast_const_expr(&tree.state, &value, &dims.const_env))
    };
    let mut indices = 0;
    // A slice selects from the dimension that the indices inside it leave
    // first; only the last select of an operand can be one (7.4.6).
    let mut slice = None;
    let mut outermost = true;
    loop {
        match &native.expression(id).kind {
            ExprKind::Group(inner) => id = *inner,
            ExprKind::Select {
                expression,
                dimension,
            } => {
                match &dimension.kind {
                    DimensionKind::Index(_) => indices += 1,
                    DimensionKind::Range { left, right } if outermost => {
                        let count = constant(*left)?
                            .abs_diff(constant(*right)?)
                            .checked_add(1)?;
                        slice = Some(count);
                    }
                    DimensionKind::Indexed { width, .. } if outermost => {
                        slice = Some(u128::try_from(constant(*width)?).ok()?);
                    }
                    _ => return None,
                }
                outermost = false;
                id = *expression;
            }
            // A hierarchical name is known by its dotted path.
            ExprKind::Name(_) | ExprKind::Member { .. } => {
                return Some((dotted_path(id, native)?, indices, slice));
            }
            _ => return None,
        }
    }
}

/// The shape of the unpacked array an expression names (see
/// [`native_selection`]).
pub(super) fn native_unpacked_shape(
    id: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Option<VariableDimensions> {
    let (path, indices, slice) = native_selection(id, tree, dims)?;
    let mut shape = selected_unpacked_shape(&path, indices, dims)?;
    if let Some(count) = slice {
        let count = ConstExpr::Literal(count.to_string());
        shape.unpacked[0] = UnpackedDimension {
            left: ConstExpr::Literal("0".into()),
            right: ConstExpr::Binary {
                left: Box::new(count.clone()),
                op: BinaryOp::Sub,
                right: Box::new(ConstExpr::Literal("1".into())),
            },
            width: count,
        };
    }
    Some(shape)
}

/// The type of the unpacked array an expression names (see
/// [`native_selection`]): each index select takes an element of the array,
/// and a slice is a fixed-size array of its element count (7.4.6).
fn native_unpacked_type(
    id: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Option<semantic_types::TyId> {
    use semantic_types::{ArrayKind, Range, TyData};
    let (path, indices, slice) = native_selection(id, tree, dims)?;
    let table = &tree.state.types;
    let mut ty = semantic_types::of_shape(table, &tree.state, dims.get(&path)?, &dims.const_env)?;
    for _ in 0..indices {
        let TyData::Array { element, .. } = table.data(ty) else {
            return None;
        };
        ty = element;
    }
    if let Some(count) = slice {
        let TyData::Array { element, .. } = table.data(ty) else {
            return None;
        };
        ty = table.intern(TyData::Array {
            kind: ArrayKind::Fixed(Range {
                left: 0,
                right: i128::try_from(count).ok()?.checked_sub(1)?,
            }),
            element,
        });
    }
    Some(ty)
}

/// The dotted path of a name or a hierarchical name `a.b.c`.
fn dotted_path(
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
) -> Option<String> {
    use celox_sv_syntax::ExprKind;
    match &native.expression(id).kind {
        ExprKind::Name(name) => Some(instances::normalize_identifier(&name.spelling)),
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "." => Some(format!(
            "{}.{}",
            dotted_path(*expression, native)?,
            instances::normalize_identifier(&name.spelling)
        )),
        _ => None,
    }
}

/// Check that the unpacked array value of native expression `id` may be
/// assigned to an array of `target` (IEEE 1800-2023 7.6). Each operand of a
/// conditional operator that selects the value is checked (11.4.11).
pub(super) fn check_native_unpacked_assignment(
    tree: &AnalysisContext<'_>,
    id: celox_sv_syntax::ExprId,
    target: &VariableDimensions,
    context: impl Fn() -> String,
    dims: &PackedDimensions,
) -> Converted<()> {
    check_assignment(tree, id, target, &context, dims)
}
fn check_assignment(
    tree: &AnalysisContext<'_>,
    id: celox_sv_syntax::ExprId,
    target: &VariableDimensions,
    context: &dyn Fn() -> String,
    dims: &PackedDimensions,
) -> Converted<()> {
    let native = tree.declarations();
    if let celox_sv_syntax::ExprKind::Conditional {
        then_expression,
        else_expression,
        ..
    } = &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    {
        check_assignment(tree, *then_expression, target, context, dims)?;
        return check_assignment(tree, *else_expression, target, context, dims);
    }
    let table = &tree.state.types;
    let Some((actual, target)) =
        semantic_types::of_shape(table, &tree.state, target, &dims.const_env)
            .zip(native_unpacked_type(id, tree, dims))
            .and_then(|(target, actual)| incompatible_types(table, actual, target))
    else {
        return Ok(());
    };
    Err(AnalyzerError::IncompatibleUnpackedArray {
        context: context(),
        actual,
        target,
    })
}
