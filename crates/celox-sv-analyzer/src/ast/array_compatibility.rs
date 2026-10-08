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
    shape: &VariableDimensions,
    dims: &PackedDimensions,
) -> Option<UnpackedArrayType> {
    if shape.unpacked.is_empty() {
        return None;
    }
    let count = |width: &ConstExpr| {
        eval_ast_const_expr(width, &dims.const_env).and_then(|width| usize::try_from(width).ok())
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
fn selected_unpacked_shape(
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
        unpacked: unpacked.to_vec(),
        ..shape.clone()
    })
}

/// The shape of the unpacked array an expression names: a variable, or one
/// of its subarrays selected by indices. Other expressions (slices, member
/// selects, calls, concatenations) are not resolved.
fn expression_unpacked_shape(
    expr: &sv_parser::Expression,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Option<VariableDimensions> {
    let sv_parser::Expression::Primary(primary) = expr else {
        return None;
    };
    match &**primary {
        sv_parser::Primary::MintypmaxExpression(grouped) => {
            let sv_parser::MintypmaxExpression::Expression(inner) = &grouped.nodes.0.nodes.1 else {
                return None;
            };
            expression_unpacked_shape(inner, tree, dims)
        }
        sv_parser::Primary::Hierarchical(hierarchical) => {
            let select = &hierarchical.nodes.2;
            if select.nodes.0.is_some() || select.nodes.2.is_some() {
                return None;
            }
            let name =
                identifier_text(RefNode::HierarchicalIdentifier(&hierarchical.nodes.1), tree)?;
            selected_unpacked_shape(&name, select.nodes.1.nodes.0.len(), dims)
        }
        _ => None,
    }
}

/// The shape of an assignment target when it is an unpacked array.
pub(super) fn variable_lvalue_unpacked_shape(
    lvalue: &sv_parser::VariableLvalue,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Option<VariableDimensions> {
    let sv_parser::VariableLvalue::Identifier(identifier) = lvalue else {
        return None;
    };
    let select = &identifier.nodes.2;
    if identifier.nodes.0.is_some() || select.nodes.0.is_some() || select.nodes.2.is_some() {
        return None;
    }
    let name = identifier_text(
        RefNode::HierarchicalVariableIdentifier(&identifier.nodes.1),
        tree,
    )?;
    selected_unpacked_shape(&name, select.nodes.1.nodes.0.len(), dims)
}

/// The shape of a net assignment target when it is an unpacked array.
pub(super) fn net_lvalue_unpacked_shape(
    lvalue: &sv_parser::NetLvalue,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Option<VariableDimensions> {
    let sv_parser::NetLvalue::Identifier(identifier) = lvalue else {
        return None;
    };
    let select = &identifier.nodes.1;
    if select.nodes.0.is_some() || select.nodes.2.is_some() {
        return None;
    }
    let name = identifier_text(
        RefNode::PsOrHierarchicalNetIdentifier(&identifier.nodes.0),
        tree,
    )?;
    selected_unpacked_shape(&name, select.nodes.1.nodes.0.len(), dims)
}

/// Rejects `value` in an assignment-like context with an unpacked array of
/// `target` shape when `value` is an unpacked array of an incompatible type.
/// `context` names the assignment-like context for the diagnostic.
pub(super) fn check_unpacked_array_assignment(
    value: &sv_parser::Expression,
    target: &VariableDimensions,
    context: impl FnOnce() -> String,
    tree: &SyntaxTree,
    dims: &PackedDimensions,
) -> Converted<()> {
    let Some(target) = unpacked_array_type(target, dims) else {
        return Ok(());
    };
    let Some(actual) = expression_unpacked_shape(value, tree, dims)
        .and_then(|shape| unpacked_array_type(&shape, dims))
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
