//! Packed structure member paths and typed vector selections.

use super::*;

/// Collapse an interface handle/member prefix using its bound symbols. Any
/// remaining path selects a packed-structure member of the interface signal.
pub(super) fn bind_interface_path(
    state: &AnalysisState,
    path: &mut Vec<String>,
    dimensions: &PackedDimensions,
) -> bool {
    // A lexical variable wins over an enclosing interface handle, even if its
    // member path is invalid. Never fall back to the interface in that case.
    if path
        .first()
        .is_some_and(|root| dimensions.get(root).is_some())
    {
        return false;
    }
    let imported = scope::imported(state);
    for length in (2..=path.len()).rev() {
        if let Some(name) = imported.interface_members.get(&path[..length].join(".")) {
            path.splice(..length, [name.clone()]);
            return true;
        }
    }
    false
}

fn selected_member<'a>(
    path: &[String],
    dimensions: &'a PackedDimensions,
) -> Option<(usize, &'a Type)> {
    let root = dimensions.get(path.first()?)?;
    if !root.unpacked.is_empty() || root.packed.len() != 1 {
        return None;
    }
    let mut members = root.members.as_slice();
    let mut offset = 0usize;
    let mut selected = None;
    for name in &path[1..] {
        let member = members.iter().find(|member| &member.name == name)?;
        if member.is_void {
            return None;
        }
        offset = offset.checked_add(member.offset)?;
        selected = Some(&member.r#type);
        members = if member.r#type.packed_ranges.len() == 1 {
            &member.r#type.members
        } else {
            &[]
        };
    }
    Some((offset, selected?))
}

fn member_dimensions(name: &str, r#type: &Type, dimensions: &PackedDimensions) -> PackedDimensions {
    let mut result = dimensions.clone();
    result.insert(
        name.to_string(),
        VariableDimensions {
            randomizable: true,
            enum_values: None,
            real: false,
            string: false,
            class: None,
            packed: function_packed_dimension_widths(&r#type.packed_ranges),
            unpacked: Vec::new(),
            signed: r#type.is_signed,
            is_2state: r#type.kind == TypeKind::Bit,
            members: r#type.members.clone(),
            signed_element_depth: r#type.signed_element_depth,
        },
    );
    result
}

fn finish_lvalue(
    state: &AnalysisState,
    path: &[String],
    offset: usize,
    r#type: &Type,
    relative: LValue,
    dimensions: &PackedDimensions,
) -> Option<LValue> {
    let width = expr_type_from_type(state, r#type, &dimensions.const_env)?.width;
    let (msb, lsb, signed) = match relative {
        LValue::Ident(_) => (width.checked_sub(1)? as i128, 0, r#type.is_signed),
        LValue::Select {
            msb, lsb, signed, ..
        } => (
            eval_ast_const_expr(state, &msb, &dimensions.const_env)?,
            eval_ast_const_expr(state, &lsb, &dimensions.const_env)?,
            signed,
        ),
    };
    // Constant member selects must stay within the member, never hit a neighbor.
    if lsb < 0 || msb < lsb || usize::try_from(msb).ok()? >= width {
        return None;
    }
    Some(LValue::Select {
        name: path[0].clone(),
        msb: ConstExpr::Literal(offset.checked_add(msb as usize)?.to_string()),
        lsb: ConstExpr::Literal(offset.checked_add(lsb as usize)?.to_string()),
        signed,
        array_slice_width: None,
        array_slice_reversed: false,
        is_2state: r#type.kind == TypeKind::Bit,
    })
}

/// A whole native member path uses the same interface shadowing, type/offset
/// lookup and bounds kernel as the remaining authored-select adapters.
pub(in crate::ast) fn whole_member(
    state: &AnalysisState,
    mut path: Vec<String>,
    base: Option<String>,
    dimensions: &PackedDimensions,
) -> Converted<LValue> {
    let bound_interface = bind_interface_path(state, &mut path, dimensions);
    if !bound_interface && let Some(base) = base {
        path[0] = base;
    }
    if bound_interface && path.len() == 1 {
        return Ok(LValue::Ident(path[0].clone()));
    }
    let (offset, ty) = selected_member(&path, dimensions)
        .ok_or_else(|| unsupported(format!("structure member `{}`", path.join("."))))?;
    finish_lvalue(
        state,
        &path,
        offset,
        ty,
        LValue::Ident(path[0].clone()),
        dimensions,
    )
    .ok_or_else(|| {
        unsupported(format!(
            "select of structure member `{}` outside the member",
            path.join(".")
        ))
    })
}

/// Select already-bound native member coordinates in the member's own type
/// context. Interface members retain ordinary target coordinate rules.
pub(in crate::ast) fn select_member(
    state: &AnalysisState,
    mut path: Vec<String>,
    base: Option<String>,
    dimensions: &PackedDimensions,
    select: impl FnOnce(String, &PackedDimensions, bool) -> Converted<LValue>,
) -> Converted<LValue> {
    let bound_interface = bind_interface_path(state, &mut path, dimensions);
    if !bound_interface && let Some(base) = base {
        path[0] = base;
    }
    if bound_interface && path.len() == 1 {
        return select(path[0].clone(), dimensions, false);
    }
    let (offset, ty) = selected_member(&path, dimensions)
        .ok_or_else(|| unsupported(format!("structure member `{}`", path.join("."))))?;
    let relative = select(
        path[0].clone(),
        &member_dimensions(&path[0], ty, dimensions),
        true,
    )?;
    finish_lvalue(state, &path, offset, ty, relative, dimensions).ok_or_else(|| {
        unsupported(format!(
            "select of structure member `{}` outside the member",
            path.join(".")
        ))
    })
}
