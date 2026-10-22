//! Assignment compatibility of unpacked arrays (IEEE 1800-2023 7.6).
//!
//! An unpacked array value is copied element by element, so it can only be
//! assigned to an array whose element type is equivalent (6.22.2) and whose
//! dimensions have the same element counts. Every assignment-like context
//! (10.8) follows this rule, including subroutine arguments of any direction
//! and the items of an assignment pattern.

use super::*;

use typecheck::UnpackedArrayType;

/// The type of an unpacked array of `shape`, or `None` when `shape` is not an
/// unpacked array or its type cannot be resolved.
fn unpacked_array_type(
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
    })
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
    Some(VariableDimensions {
        real: false,
        string: false,
        unpacked: unpacked.to_vec(),
        ..shape.clone()
    })
}

/// The shape of the unpacked array an expression names: a variable, or one
/// of its subarrays selected by indices. Other expressions (slices, member
/// selects, calls, concatenations) are not resolved.
pub(super) fn native_unpacked_shape(
    mut id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dims: &PackedDimensions,
) -> Option<VariableDimensions> {
    use celox_sv_syntax::{DimensionKind, ExprKind};
    let mut indices = 0;
    loop {
        match &native.expression(id).kind {
            ExprKind::Group(inner) => id = *inner,
            ExprKind::Select {
                expression,
                dimension,
            } if matches!(dimension.kind, DimensionKind::Index(_)) => {
                indices += 1;
                id = *expression;
            }
            ExprKind::Name(name) => {
                return selected_unpacked_shape(
                    &instances::normalize_identifier(&name.spelling),
                    indices,
                    dims,
                );
            }
            _ => return None,
        }
    }
}
pub(super) fn check_native_unpacked_assignment(
    state: &AnalysisState,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    target: &VariableDimensions,
    context: impl FnOnce() -> String,
    dims: &PackedDimensions,
) -> Converted<()> {
    let Some(target) = unpacked_array_type(state, target, dims) else {
        return Ok(());
    };
    let Some(actual) = native_unpacked_shape(id, native, dims)
        .and_then(|shape| unpacked_array_type(state, &shape, dims))
    else {
        return Ok(());
    };
    if actual.is_assignment_compatible_with(&target) {
        return Ok(());
    }
    Err(AnalyzerError::IncompatibleUnpackedArray {
        context: context(),
        actual,
        target,
    })
}
