//! Packed structure member paths and typed vector selections.

use super::*;

/// Collapse an interface handle/member prefix using its bound symbols. Any
/// remaining path selects a packed-structure member of the interface signal.
pub(in crate::ast) fn bind_interface_path(
    tree: &AnalysisContext<'_>,
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
    let imported = scope::imported(tree);
    for length in (2..=path.len()).rev() {
        if let Some(name) = imported.interface_members.get(&path[..length].join(".")) {
            path.splice(..length, [name.clone()]);
            return true;
        }
    }
    false
}

/// A path that starts at no variable or interface of the scope is a
/// hierarchical name of another scope (IEEE 1800-2023 23.6).
fn unknown_member(
    path: &[String],
    bound_interface: bool,
    dimensions: &PackedDimensions,
) -> AnalyzerError {
    if !bound_interface && dimensions.get(&path[0]).is_none() {
        return AnalyzerError::HierarchicalReference {
            path: path.join("."),
        };
    }
    unsupported(format!("structure member `{}`", path.join(".")))
}

fn selected_member<'a>(
    path: &[String],
    dimensions: &'a PackedDimensions,
) -> Option<(usize, &'a Type)> {
    let root = dimensions.get(path.first()?)?;
    if !root.unpacked.is_empty() || root.packed.len() != 1 {
        return None;
    }
    member_in(root, &path[1..])
}

/// The offset and type of member path `path` in a packed structure of
/// `root`'s element type.
fn member_in<'a>(root: &'a VariableDimensions, path: &[String]) -> Option<(usize, &'a Type)> {
    let mut members = root.members.as_slice();
    let mut offset = 0usize;
    let mut selected = None;
    for name in path {
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
        VariableDimensions::new(VariableShape {
            declared: type_names::Declared::default(),
            container: None,
            chandle: false,
            randomizable: true,
            enum_domain: None,
            packed_enum: false,
            real: false,
            string: false,
            class: None,
            virtual_interface: None,
            packed: function_packed_dimension_widths(&r#type.packed_ranges),
            unpacked: dimensions::unpacked_dimension_widths(&r#type.unpacked_ranges),
            signed: r#type.is_signed,
            is_2state: r#type.kind == TypeKind::Bit,
            members: r#type.members.clone(),
            signed_element_depth: r#type.signed_element_depth,
            unpacked_struct: None,
        }),
    );
    result
}

fn finish_lvalue(
    tree: &AnalysisContext<'_>,
    path: &[String],
    offset: usize,
    r#type: &Type,
    relative: LValue,
    dimensions: &PackedDimensions,
) -> Option<LValue> {
    let functions = &tree.functions;
    let width = expr_type_from_type(&tree.functions, r#type, &dimensions.const_env)?.width;
    let (msb, lsb, signed) = match relative {
        LValue::Ident(_) => (width.checked_sub(1)? as i128, 0, r#type.is_signed),
        LValue::Select {
            msb, lsb, signed, ..
        } => (
            eval_ast_const_expr(functions, &msb, &dimensions.const_env)?,
            eval_ast_const_expr(functions, &lsb, &dimensions.const_env)?,
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
    tree: &AnalysisContext<'_>,
    mut path: Vec<String>,
    base: Option<String>,
    dimensions: &PackedDimensions,
) -> Converted<LValue> {
    let bound_interface = bind_interface_path(tree, &mut path, dimensions);
    if !bound_interface && let Some(base) = base {
        path[0] = base;
    }
    if bound_interface && path.len() == 1 {
        return Ok(LValue::Ident(path[0].clone()));
    }
    let (offset, ty) = selected_member(&path, dimensions)
        .ok_or_else(|| unknown_member(&path, bound_interface, dimensions))?;
    finish_lvalue(
        tree,
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
    tree: &AnalysisContext<'_>,
    mut path: Vec<String>,
    base: Option<String>,
    dimensions: &PackedDimensions,
    select: impl FnOnce(String, &PackedDimensions, bool) -> Converted<LValue>,
) -> Converted<LValue> {
    let bound_interface = bind_interface_path(tree, &mut path, dimensions);
    if !bound_interface && let Some(base) = base {
        path[0] = base;
    }
    if bound_interface && path.len() == 1 {
        return select(path[0].clone(), dimensions, false);
    }
    let (offset, ty) = selected_member(&path, dimensions)
        .ok_or_else(|| unknown_member(&path, bound_interface, dimensions))?;
    let relative = select(
        path[0].clone(),
        &member_dimensions(&path[0], ty, dimensions),
        true,
    )?;
    finish_lvalue(tree, &path, offset, ty, relative, dimensions).ok_or_else(|| {
        unsupported(format!(
            "select of structure member `{}` outside the member",
            path.join(".")
        ))
    })
}

/// Select member path `members` of the element of fixed array variable
/// `name` that `indices` select, such as `m[i].y` (IEEE 1800-2023 7.2.1):
/// the member's bits in that element, which an index outside the array's
/// range or with unknown bits reads as the default value and never writes,
/// like the element itself (7.4.5).
pub(in crate::ast) fn select_element_member(
    tree: &AnalysisContext<'_>,
    name: String,
    members: &[String],
    indices: Vec<ConstExpr>,
    dimensions: &PackedDimensions,
    select: impl FnOnce(String, &PackedDimensions, bool) -> Converted<LValue>,
) -> Converted<LValue> {
    let path = std::iter::once(name.clone())
        .chain(members.iter().cloned())
        .collect::<Vec<_>>();
    let (offset, ty) = element_member(&name, members, dimensions)?;
    let relative = select(
        name.clone(),
        &member_dimensions(&name, ty, dimensions),
        true,
    )?;
    if !ty.unpacked_ranges.is_empty() {
        return array_member_selection(tree, name, indices, (offset, ty), relative, dimensions);
    }
    let Some(LValue::Select {
        msb,
        lsb,
        signed,
        is_2state,
        ..
    }) = finish_lvalue(tree, &path, offset, ty, relative, dimensions)
    else {
        return Err(unsupported(format!(
            "select of structure member `{}` outside the member",
            path.join(".")
        )));
    };
    let mut lvalue =
        crate::ast::selects::lvalue_from_range(tree, name, indices, msb, lsb, false, dimensions)?;
    if let LValue::Select {
        signed: selected_signed,
        is_2state: selected_2state,
        ..
    } = &mut lvalue
    {
        *selected_signed = signed;
        *selected_2state = is_2state;
    }
    Ok(lvalue)
}

/// Flatten an integral array member into the cell that holds its structure.
/// Guard its own window before adding offsets, so an invalid inner index can
/// never reach the next member or the next structure element.
fn array_member_selection(
    tree: &AnalysisContext<'_>,
    name: String,
    indices: Vec<ConstExpr>,
    member: (usize, &Type),
    relative: LValue,
    dimensions: &PackedDimensions,
) -> Converted<LValue> {
    let (offset, ty) = member;
    let context = member_dimensions(&name, ty, dimensions);
    let member_width = lvalue_expr_type(&tree.functions, &LValue::Ident(name.clone()), &context)
        .ok_or_else(|| unsupported("structure array member width"))?
        .width;
    let width = lvalue_expr_type(&tree.functions, &relative, &context)
        .ok_or_else(|| unsupported("structure array member selection width"))?
        .width;
    let (msb, lsb, signed, slice_width, reversed) = match relative {
        LValue::Ident(_) => (
            ConstExpr::Literal((member_width - 1).to_string()),
            ConstExpr::Literal("0".into()),
            ty.is_signed,
            None,
            false,
        ),
        LValue::Select {
            msb,
            lsb,
            signed,
            array_slice_width,
            array_slice_reversed,
            ..
        } => (msb, lsb, signed, array_slice_width, array_slice_reversed),
    };
    let valid = ConstExpr::Binary {
        left: Box::new(ConstExpr::Binary {
            left: Box::new(lsb.clone()),
            op: BinaryOp::Ge,
            right: Box::new(ConstExpr::Literal("0".into())),
        }),
        op: BinaryOp::LogicAnd,
        right: Box::new(ConstExpr::Binary {
            left: Box::new(msb.clone()),
            op: BinaryOp::Lt,
            right: Box::new(ConstExpr::Literal(member_width.to_string())),
        }),
    };
    let known = eval_ast_const_expr(&tree.functions, &valid, &dimensions.const_env);
    let full_width = lvalue_expr_type(&tree.functions, &LValue::Ident(name.clone()), dimensions)
        .ok_or_else(|| unsupported("structure array storage width"))?
        .width;
    let invalid = ConstExpr::Literal(full_width.to_string());
    let lsb = if known == Some(0) {
        invalid
    } else {
        let offset = ConstExpr::Literal(offset.to_string());
        let target = crate::ast::selects::lvalue_from_range(
            tree,
            name.clone(),
            indices,
            add_expr(msb, offset.clone()),
            add_expr(lsb, offset),
            false,
            dimensions,
        )?;
        let LValue::Select { lsb, .. } = target else {
            return Err(unsupported("structure array member selection"));
        };
        if known.is_some() {
            lsb
        } else {
            ConstExpr::Mux {
                condition: Box::new(ConstExpr::Binary {
                    left: Box::new(valid),
                    op: BinaryOp::EqCase,
                    right: Box::new(ConstExpr::Literal("1'b1".into())),
                }),
                then_expr: Box::new(lsb),
                else_expr: Box::new(invalid),
            }
        }
    };
    Ok(LValue::Select {
        name,
        msb: add_expr(lsb.clone(), ConstExpr::Literal((width - 1).to_string())),
        lsb,
        signed,
        array_slice_width: slice_width,
        array_slice_reversed: reversed,
        is_2state: ty.kind == TypeKind::Bit,
    })
}

/// The offset and type of member path `members` in an element of a fixed
/// array variable `name`. A member of a tagged union is rejected: its tag is
/// checked only in a variable of its own (see `member_access`).
pub(in crate::ast) fn element_member<'a>(
    name: &str,
    members: &[String],
    dimensions: &'a PackedDimensions,
) -> Converted<(usize, &'a Type)> {
    let path = || format!("{name}.{}", members.join("."));
    let root = dimensions
        .get(name)
        .ok_or_else(|| unsupported(format!("structure member `{}`", path())))?;
    let mut level = root.members.as_slice();
    for member in members {
        let Some(member) = level.iter().find(|candidate| &candidate.name == member) else {
            break;
        };
        if member.is_tagged() {
            return Err(unsupported(format!(
                "member `{}` of a tagged union element of an unpacked array",
                path()
            )));
        }
        level = &member.r#type.members;
    }
    member_in(root, members).ok_or_else(|| unsupported(format!("structure member `{}`", path())))
}
