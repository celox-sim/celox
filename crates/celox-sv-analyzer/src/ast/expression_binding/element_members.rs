//! Member selects of elements of unpacked arrays, such as `m[i].y`.

use super::*;
use semantic_types::{ArrayKind, Packed, PackedElement, TyData};

/// A member select of an element of an unpacked array variable: the
/// variable, the index of each of its unpacked dimensions, outermost first,
/// and the member path in the element.
pub(in crate::ast) struct ElementMember {
    pub name: String,
    pub indices: Vec<ExprId>,
    pub members: Vec<String>,
}

/// The member select `root` of an element of a fixed-size unpacked array
/// variable of a packed structure or union type, such as `m[i].y`, if it is
/// one. The index selects take elements of the variable (IEEE 1800-2023
/// 7.4.5) and the member selects then select from the element (7.2.1, 7.3.1),
/// so the indices need not be constant, unlike those of a hierarchical name
/// (23.6).
pub(in crate::ast) fn element_member(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Option<ElementMember> {
    let mut id = ungroup(root, native);
    let mut members = Vec::new();
    while let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(id).kind
    {
        if separator != "." {
            return None;
        }
        members.push(instances::normalize_identifier(&name.spelling));
        id = ungroup(*expression, native);
    }
    let mut indices = Vec::new();
    while let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(id).kind
    {
        let DimensionKind::Index(index) = dimension.kind else {
            return None;
        };
        indices.push(index);
        id = ungroup(*expression, native);
    }
    if members.is_empty() || indices.is_empty() {
        return None;
    }
    // The variable, or a parameter table of a package, such as `p::T`.
    let name = direct::call_name(tree, id, native)?;
    let table = &mut session.types;
    let mut ty = semantic_types::of_shape(
        table,
        &tree.functions,
        dimensions.get(&name)?,
        &dimensions.const_env,
    )?;
    for _ in &indices {
        let TyData::Array {
            kind: ArrayKind::Fixed(_),
            element,
        } = table.data(ty)
        else {
            return None;
        };
        ty = *element;
    }
    let TyData::Packed(Packed {
        element: PackedElement::Aggregate(_),
        dims,
        ..
    }) = table.data(ty)
    else {
        return None;
    };
    if dims.len() != 1 {
        return None;
    }
    members.reverse();
    indices.reverse();
    Some(ElementMember {
        name,
        indices,
        members,
    })
}
