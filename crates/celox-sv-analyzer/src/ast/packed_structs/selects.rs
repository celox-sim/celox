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
/// The variable of a generate block of the scope that dotted name `path`
/// names, such as `gen_ecc.err_a` for `logic err_a` of `begin : gen_ecc`
/// (IEEE 1800-2023 27.6, 23.6): its first component names no variable, and
/// the scope declares the block's variable by the whole path.
fn generate_scope_signal(
    path: &[String],
    bound_interface: bool,
    dimensions: &PackedDimensions,
) -> Option<String> {
    if bound_interface || path.len() < 2 || dimensions.get(&path[0]).is_some() {
        return None;
    }
    let name = path.join(".");
    dimensions.get(&name).is_some().then_some(name)
}

fn unknown_member(
    path: &[String],
    bound_interface: bool,
    dimensions: &PackedDimensions,
) -> AnalyzerError {
    if !bound_interface && dimensions.get(&path[0]).is_none() {
        // A name that the scope binds to a parameter is no hierarchical
        // name, whatever follows it (23.7).
        if dimensions
            .const_env
            .contains_key(&parameters::unbounded_parameter_marker(&path[0]))
        {
            return unsupported(format!(
                "member `{}` of a parameter in this constant expression",
                path.join(".")
            ));
        }
        return AnalyzerError::HierarchicalReference {
            path: path.join("."),
        };
    }
    unsupported(format!("structure member `{}`", path.join(".")))
}

fn selected_member<'a>(
    tree: &AnalysisContext<'_>,
    path: &[String],
    dimensions: &'a PackedDimensions,
) -> Option<(usize, Cow<'a, Type>)> {
    let root = dimensions.get(path.first()?)?;
    if !root.unpacked.is_empty() || root.packed.len() != 1 {
        return None;
    }
    if let Some((offset, r#type)) = member_in(root, &path[1..]) {
        return Some((offset, Cow::Borrowed(r#type)));
    }
    indexed_member_in(tree, root, &path[1..], dimensions)
        .map(|(offset, r#type)| (offset, Cow::Owned(r#type)))
}

/// The offset and type of member path `path` in a packed structure of
/// `root`'s element type, whose components may select elements of packed
/// array members, such as `arr[0]` of `s.arr[0].f` (IEEE 1800-2023 7.2.1,
/// 7.4.1). An index outside a member's range selects no member.
fn indexed_member_in(
    tree: &AnalysisContext<'_>,
    root: &VariableDimensions,
    path: &[String],
    dimensions: &PackedDimensions,
) -> Option<(usize, Type)> {
    let constant =
        |expr: &ConstExpr| eval_ast_const_expr(&tree.functions, expr, &dimensions.const_env);
    let mut members = root.members.as_slice();
    let mut offset = 0usize;
    let mut selected = None;
    for component in path {
        let (name, indices) = indexed_component(component)?;
        let member = members.iter().find(|member| member.name == name)?;
        if member.is_void || member.is_tagged() {
            return None;
        }
        offset = offset.checked_add(member.offset)?;
        let mut r#type = member.r#type.clone();
        for index in indices {
            // An element of the outermost packed dimension; an index of the
            // last one would select a bit, which has no members.
            if r#type.packed_ranges.len() < 2 {
                return None;
            }
            let range = r#type.packed_ranges.remove(0);
            let (left, right) = (constant(&range.left)?, constant(&range.right)?);
            let position = if left >= right {
                index.checked_sub(right)?
            } else {
                right.checked_sub(index)?
            };
            if position < 0 || position > left.abs_diff(right) as i128 {
                return None;
            }
            // An element of a named type keeps its signedness; one of a
            // packed array otherwise is unsigned (7.4.1).
            r#type.is_signed = r#type.signed_element_depth == Some(1);
            r#type.signed_element_depth = r#type
                .signed_element_depth
                .and_then(|depth| depth.checked_sub(1))
                .filter(|depth| *depth > 0);
            let width = expr_type_from_type(&tree.functions, &r#type, &dimensions.const_env)?.width;
            offset = offset.checked_add(usize::try_from(position).ok()?.checked_mul(width)?)?;
        }
        members = if r#type.packed_ranges.len() == 1 {
            &member.r#type.members
        } else {
            &[]
        };
        selected = Some(r#type);
    }
    selected.map(|r#type| (offset, r#type))
}

/// The member name and constant indices of a member path component, such
/// as `arr` and `[0, 1]` of `arr[0][1]`, or `f` and none of `f`.
fn indexed_component(component: &str) -> Option<(&str, Vec<i128>)> {
    let Some((name, mut rest)) = component.split_once('[') else {
        return Some((component, Vec::new()));
    };
    let mut indices = Vec::new();
    loop {
        let (index, after) = rest.split_once(']')?;
        indices.push(index.parse().ok()?);
        match after.strip_prefix('[') {
            Some(next) => rest = next,
            None if after.is_empty() => return Some((name, indices)),
            None => return None,
        }
    }
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
            is_2state: matches!(r#type.kind, TypeKind::Bit | TypeKind::Chandle),
            members: r#type.members.clone(),
            signed_element_depth: r#type.signed_element_depth,
            unpacked_struct: None,
        }),
    );
    result
}

/// Whether a select of a member may have run-time bounds, and where a bit
/// outside the member goes.
#[derive(Clone, Copy)]
enum RunTime {
    /// Only constant bounds.
    Rejected,
    /// A member of the structure variable `path[0]`: past the variable.
    Structure,
    /// A member of an element of array variable `path[0]`, whose element
    /// the select is then placed in: bit `beyond` of the element, past the
    /// whole variable wherever the element is. The window of a part-select
    /// counts from the element's bit 0.
    Element { beyond: usize },
}

fn finish_lvalue(
    tree: &AnalysisContext<'_>,
    path: &[String],
    offset: usize,
    r#type: &Type,
    relative: LValue,
    run_time: RunTime,
    dimensions: &PackedDimensions,
) -> Option<LValue> {
    let functions = &tree.functions;
    let width = expr_type_from_type(&tree.functions, r#type, &dimensions.const_env)?.width;
    let (msb, lsb, signed) = match relative {
        // A selection within a member is of a variable.
        LValue::Object { .. } => lowered_object(),
        LValue::Ident(_) => (width.checked_sub(1)? as i128, 0, r#type.is_signed),
        LValue::Select {
            msb, lsb, signed, ..
        } => match (
            eval_ast_const_expr(functions, &msb, &dimensions.const_env),
            eval_ast_const_expr(functions, &lsb, &dimensions.const_env),
        ) {
            (Some(msb), Some(lsb)) => (msb, lsb, signed),
            // A run-time select of a vector member. One that leaves the
            // member is moved above the whole structure, where a read gives
            // X and a write is ignored (IEEE 1800-2023 11.5.1), so that it
            // never reaches a neighboring member.
            _ if !matches!(run_time, RunTime::Rejected) && !r#type.packed_ranges.is_empty() => {
                return dynamic_member_select(
                    tree, path, offset, r#type, width, msb, lsb, signed, run_time, dimensions,
                );
            }
            _ => return None,
        },
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
        window: None,
        is_2state: matches!(r#type.kind, TypeKind::Bit | TypeKind::Chandle),
    })
}

/// The select of structure variable `path[0]` that a run-time select with
/// bounds `msb` and `lsb` makes of its member of `width` bits at `offset`.
///
/// A bit-select arrives as a position in the member, which is the member's
/// width when the index is outside its range; such a select is moved above
/// the whole structure, where a read gives X and a write is ignored.
///
/// A select of the elements of a member of several packed dimensions
/// arrives in positions of the member's bits too, those of an index outside
/// its range at the member's width. It becomes a select at those positions
/// in the structure, confined to the member's bits by a
/// [`crate::ast::SelectWindow`].
///
/// An indexed part-select arrives in the index coordinates of the member's
/// declared range, its width a constant. It becomes a select at the
/// positions of those indices in the structure, confined to the member's
/// bits by a [`crate::ast::SelectWindow`]: the bits it selects outside the
/// member read X and are not written, as those outside a vector are (IEEE
/// 1800-2023 11.5.1), and never those of a neighboring member.
#[allow(clippy::too_many_arguments)]
fn dynamic_member_select(
    tree: &AnalysisContext<'_>,
    path: &[String],
    offset: usize,
    r#type: &Type,
    width: usize,
    msb: ConstExpr,
    lsb: ConstExpr,
    signed: bool,
    run_time: RunTime,
    dimensions: &PackedDimensions,
) -> Option<LValue> {
    let functions = &tree.functions;
    let literal = |value: usize| ConstExpr::Literal(value.to_string());
    let select = |msb, lsb, window| LValue::Select {
        name: path[0].clone(),
        msb,
        lsb,
        signed,
        array_slice_width: None,
        array_slice_reversed: false,
        window,
        is_2state: matches!(r#type.kind, TypeKind::Bit | TypeKind::Chandle),
    };
    if msb == lsb {
        let structure = match run_time {
            RunTime::Element { beyond } => beyond,
            _ => lvalue_expr_type(functions, &LValue::Ident(path[0].clone()), dimensions)?.width,
        };
        let position = ConstExpr::Mux {
            condition: Box::new(ConstExpr::Binary {
                left: Box::new(lsb.clone()),
                op: BinaryOp::Lt,
                right: Box::new(literal(width)),
            }),
            then_expr: Box::new(add_expr(literal(offset), lsb)),
            else_expr: Box::new(literal(structure)),
        };
        return Some(select(position.clone(), position, None));
    }
    let window = Some(crate::ast::SelectWindow::bits(
        u32::try_from(offset).ok()?,
        u32::try_from(offset.checked_add(width.checked_sub(1)?)?).ok()?,
    ));
    if r#type.packed_ranges.len() > 1 {
        return Some(select(
            add_expr(literal(offset), msb),
            add_expr(literal(offset), lsb),
            window,
        ));
    }
    let [range] = r#type.packed_ranges.as_slice() else {
        return None;
    };
    let left = eval_ast_const_expr(functions, &range.left, &dimensions.const_env)?;
    let right = eval_ast_const_expr(functions, &range.right, &dimensions.const_env)?;
    // The bounds are `start` and `start` plus or minus a constant, the
    // more significant end first: `(start + n, start)` or `(start, start -
    // n)` in a descending range, `(start, start + n)` or `(start - n,
    // start)` in an ascending one.
    let constant = |expr: &ConstExpr| eval_ast_const_expr(functions, expr, &dimensions.const_env);
    let offset_from = |bound: &ConstExpr, start: &ConstExpr| match bound {
        ConstExpr::Binary {
            left,
            op: BinaryOp::Add,
            right,
        } if **left == *start => constant(right),
        ConstExpr::Binary {
            left,
            op: BinaryOp::Sub,
            right,
        } if **left == *start => constant(right).and_then(i128::checked_neg),
        _ => None,
    };
    let (start, msb_index, lsb_index) = match (offset_from(&msb, &lsb), offset_from(&lsb, &msb)) {
        (Some(from_lsb), _) => (&lsb, from_lsb, 0),
        (None, Some(from_msb)) => (&msb, 0, from_msb),
        (None, None) => return None,
    };
    // Index `i` is at position `i - right` of a descending member and at
    // `right - i` of an ascending one, above the member's offset.
    let offset = i128::try_from(offset).ok()?;
    let (base, msb, lsb) = if left >= right {
        (
            start.clone(),
            offset.checked_add(msb_index)?.checked_sub(right)?,
            offset.checked_add(lsb_index)?.checked_sub(right)?,
        )
    } else {
        (
            ConstExpr::Unary {
                op: UnaryOp::Minus,
                expr: Box::new(start.clone()),
            },
            offset.checked_add(right)?.checked_sub(msb_index)?,
            offset.checked_add(right)?.checked_sub(lsb_index)?,
        )
    };
    let at = |constant: i128| ConstExpr::Binary {
        left: Box::new(base.clone()),
        op: BinaryOp::Add,
        right: Box::new(const_expr_from_i128(constant)),
    };
    Some(select(at(msb), at(lsb), window))
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
    if let Some(name) = generate_scope_signal(&path, bound_interface, dimensions) {
        return Ok(LValue::Ident(name));
    }
    let (offset, ty) = selected_member(tree, &path, dimensions)
        .ok_or_else(|| unknown_member(&path, bound_interface, dimensions))?;
    finish_lvalue(
        tree,
        &path,
        offset,
        &ty,
        LValue::Ident(path[0].clone()),
        RunTime::Rejected,
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
    if let Some(name) = generate_scope_signal(&path, bound_interface, dimensions) {
        return select(name, dimensions, false);
    }
    let (offset, ty) = selected_member(tree, &path, dimensions)
        .ok_or_else(|| unknown_member(&path, bound_interface, dimensions))?;
    let relative = select(
        path[0].clone(),
        &member_dimensions(&path[0], &ty, dimensions),
        true,
    )?;
    finish_lvalue(
        tree,
        &path,
        offset,
        &ty,
        relative,
        RunTime::Structure,
        dimensions,
    )
    .ok_or_else(|| {
        unsupported(format!(
            "select of structure member `{}` outside the member",
            path.join(".")
        ))
    })
}

/// An upper bound of the bits of variable `name`: a bit past it is in no
/// element of the variable.
fn variable_bits_bound(
    tree: &AnalysisContext<'_>,
    name: &str,
    dimensions: &PackedDimensions,
) -> Option<usize> {
    let variable = dimensions.get(name)?;
    let mut bits = lvalue_expr_type(
        &tree.functions,
        &LValue::Ident(name.to_string()),
        dimensions,
    )?
    .width;
    for width in variable
        .unpacked
        .iter()
        .map(|dimension| &dimension.width)
        .chain(variable.packed.iter().map(|dimension| &dimension.width))
    {
        let width = eval_ast_const_expr(&tree.functions, width, &dimensions.const_env)?;
        bits = bits.checked_mul(usize::try_from(width).ok()?)?;
    }
    Some(bits)
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
    let beyond = variable_bits_bound(tree, &name, dimensions);
    let run_time = beyond.map_or(RunTime::Rejected, |beyond| RunTime::Element { beyond });
    let Some(LValue::Select {
        msb,
        lsb,
        signed,
        is_2state,
        window,
        ..
    }) = finish_lvalue(tree, &path, offset, ty, relative, run_time, dimensions)
    else {
        return Err(unsupported(format!(
            "select of structure member `{}` outside the member",
            path.join(".")
        )));
    };
    // A window counts from the bit 0 of the element, which is at a run-time
    // position for a run-time index.
    let window = match window {
        Some(window) => {
            let zero = || ConstExpr::Literal("0".into());
            let LValue::Select { lsb: element, .. } = crate::ast::selects::lvalue_from_range(
                tree,
                name.clone(),
                indices.clone(),
                zero(),
                zero(),
                false,
                dimensions,
            )?
            else {
                return Err(unsupported("element of a structure array"));
            };
            Some(
                match eval_ast_const_expr(&tree.functions, &element, &dimensions.const_env)
                    .and_then(|element| u32::try_from(element).ok())
                {
                    Some(element) => crate::ast::SelectWindow::bits(
                        element
                            .checked_add(window.lsb)
                            .ok_or_else(|| unsupported("window"))?,
                        element
                            .checked_add(window.msb)
                            .ok_or_else(|| unsupported("window"))?,
                    ),
                    None => crate::ast::SelectWindow {
                        at: Some(Box::new(element)),
                        ..window
                    },
                },
            )
        }
        None => None,
    };
    let mut lvalue =
        crate::ast::selects::lvalue_from_range(tree, name, indices, msb, lsb, false, dimensions)?;
    if let LValue::Select {
        signed: selected_signed,
        is_2state: selected_2state,
        window: selected_window,
        ..
    } = &mut lvalue
    {
        *selected_signed = signed;
        *selected_2state = is_2state;
        *selected_window = window;
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
        // A selection within a member is of a variable.
        LValue::Object { .. } => lowered_object(),
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
        window: None,
        is_2state: matches!(ty.kind, TypeKind::Bit | TypeKind::Chandle),
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
