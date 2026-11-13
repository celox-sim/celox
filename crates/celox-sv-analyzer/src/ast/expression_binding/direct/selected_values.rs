//! Selected values and places: the paths, selects and members of variables,
//! class properties and container elements.
use super::*;

pub(super) fn index_value(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<ConstExpr> {
    let value = bind_value(id, native, tree, session, dimensions, None)?;
    if real_value(&value, dimensions) {
        return Err(unsupported("real-valued select index"));
    }
    let value = expand_expr_calls(
        value,
        &dimensions.functions,
        &dimensions.expression_signedness,
        0,
        true,
    );
    if let Some(folded) =
        selects::indexed_expression_value(tree, session, value.clone(), dimensions)
        && (matches!(folded, ConstExpr::Literal(_))
            || eval_ast_const_expr(&tree.functions, &folded, &dimensions.const_env).is_some())
    {
        if matches!(&folded, ConstExpr::Literal(text) if real_value(&Expr::Literal(text.clone()), dimensions))
        {
            return Err(unsupported("real-valued select index"));
        }
        return Ok(folded);
    }
    (if dimensions.constant_indexed_base {
        selects::indexed_expression_value(tree, session, value, dimensions)
    } else {
        expr_to_index_const(tree, session, value, dimensions)
    })
    .ok_or_else(|| unsupported("index expression"))
}

pub(super) fn read_index_value(
    id: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<ConstExpr> {
    index_value(id, native, tree, session, dimensions)
}

/// Whether native expression `id` is the element of a dynamic container
/// whose select is being bound against a local.
pub(in crate::ast::expression_binding) fn path(
    mut root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<(Vec<String>, Option<String>)> {
    let authored = root;
    // A member through a virtual interface is selected in its template,
    // which the caller applies to the member of each instance.
    if let Some((_, ty, member)) =
        virtual_interfaces::native_member(tree, session, root, native, dimensions)
    {
        virtual_interfaces::member(session, &ty, &member)?;
        return Ok((
            vec![virtual_interfaces::template(&ty.interface, &member)],
            None,
        ));
    }
    // A static class property is a variable of the module.
    if let Some(variable) = objects::static_property(tree, session, root, native, dimensions)? {
        return Ok((vec![variable], None));
    }
    let mut parts = Vec::new();
    let mut qualified = None;
    loop {
        match &native.expression(root).kind {
            ExprKind::Name(name) => {
                parts.push(instances::normalize_identifier(&name.spelling));
                break;
            }
            ExprKind::Member {
                expression,
                separator,
                name,
            } => {
                parts.push(instances::normalize_identifier(&name.spelling));
                if separator == "::" {
                    qualified = Some(parts.len());
                }
                root = *expression;
            }
            ExprKind::Select {
                expression,
                dimension,
            } if matches!(dimension.kind, DimensionKind::Index(_)) => {
                let DimensionKind::Index(index) = dimension.kind else {
                    unreachable!()
                };
                let value = index_value(index, native, tree, session, dimensions)?;
                let value = eval_ast_const_expr(&tree.functions, &value, &dimensions.const_env)
                    .ok_or_else(|| unsupported("hierarchical index"))?;
                let (mut prefix, base) = path(*expression, native, tree, session, dimensions)?;
                let last = prefix.last_mut().ok_or_else(disagreement)?;
                last.push_str(&format!("[{value}]"));
                prefix.extend(parts.into_iter().rev());
                if base.is_none()
                    && let Some(name) = interface_binding::unindexed_parameter(
                        &prefix, tree, session, dimensions, authored,
                    )
                {
                    return Ok((vec![name], None));
                }
                return Ok((prefix, base));
            }
            _ => return Err(unsupported("member receiver")),
        }
    }
    parts.reverse();
    if qualified.is_none() {
        unpacked_structs::collapse(&mut parts, dimensions);
    }
    if qualified.is_none()
        && let Some(name) =
            interface_binding::unindexed_parameter(&parts, tree, session, dimensions, authored)
    {
        return Ok((vec![name], None));
    }

    if let Some(length) = qualified {
        if parts.len() == length + 1 {
            // Inside its package, the item itself.
            let base = scope::qualified_name(tree, &parts[0], &parts[1]);
            parts.drain(..1);
            qualified = None;
            let _ = qualified;
            return Ok((parts, Some(base)));
        }
    }
    Ok((parts, None))
}

pub(super) fn selected_value(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    // A member of a class property of packed structure type, such as
    // `h.p.m` or `h.p.m[1:0]`, selects the property's value.
    if let Some((base, property, receiver)) =
        property_member_base(root, native, tree, session, dimensions)?
    {
        let steps = value_selects::steps(root, base, native).ok_or_else(disagreement)?;
        let handle = objects::receiver(tree, session, receiver, native, dimensions)?;
        return value_selects::read(
            objects::load(&property, handle),
            &property.r#type,
            &steps,
            native,
            tree,
            session,
            dimensions,
        );
    }
    // A select of an element of a container, or of a member of one, such
    // as `d[i][8*j+:8]` or `q[i].m[k]`, selects the element's value.
    if let Some((element, r#type)) =
        selected_container_element(root, native, tree, session, dimensions)?
    {
        let steps = value_selects::steps(root, element, native).ok_or_else(disagreement)?;
        let value = bind_value(element, native, tree, session, dimensions, None)?;
        return value_selects::read(value, &r#type, &steps, native, tree, session, dimensions);
    }
    let mut base = root;
    let mut suffix = Vec::new();
    while let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(base).kind
    {
        suffix.push(&dimension.kind);
        base = *expression;
    }
    suffix.reverse();
    if let ExprKind::Name(name) = &native.expression(base).kind
        && dimensions
            .get(&instances::normalize_identifier(&name.spelling))
            .is_some_and(|ty| ty.real && suffix.len() > ty.unpacked.len())
    {
        return Err(unsupported("bit-select or part-select of a real variable"));
    }
    // A select of a class property selects its value, read through its
    // handle, as a select of a variable of its type (IEEE 1800-2023 11.5.1).
    if !suffix.is_empty()
        && let Some((property, receiver)) =
            objects::property(tree, session, base, native, dimensions)?
        && property.field.handle.is_none()
        && property.r#type.container.is_none()
        && property.r#type.unpacked_struct().is_none()
    {
        // The value has the positions of a `[N-1:0]` vector.
        if !zero_based_vector(&property.r#type, tree, dimensions) {
            return Err(unsupported(
                "read of a select of a class property that is not declared [N-1:0]",
            ));
        }
        let steps = value_selects::steps(root, base, native).ok_or_else(disagreement)?;
        let handle = objects::receiver(tree, session, receiver, native, dimensions)?;
        return value_selects::read(
            objects::load(&property, handle),
            &property.r#type,
            &steps,
            native,
            tree,
            session,
            dimensions,
        );
    }
    // A select of the iterator of a locator over elements kept in records,
    // such as `item[31:0]`, selects the element's value.
    if !suffix.is_empty()
        && let ExprKind::Name(name) = &native.expression(base).kind
        && dimensions
            .get(&instances::normalize_identifier(&name.spelling))
            .is_none()
        && let Some((container, handle)) = locators::record_iterator(base, native, dimensions)
        && container.element.unpacked_struct().is_none()
    {
        if !zero_based_vector(&container.element, tree, dimensions) {
            return Err(unsupported(
                "select of an element wider than 64 bits that is not declared [N-1:0]",
            ));
        }
        let steps = value_selects::steps(root, base, native).ok_or_else(disagreement)?;
        return value_selects::read(
            containers::element_read(&container, handle),
            &container.element,
            &steps,
            native,
            tree,
            session,
            dimensions,
        );
    }
    // An element wider than 64 bits is held in a record, whose packed
    // selects, such as `q[0][129]`, are not bound yet.
    if let ExprKind::Name(name) = &native.expression(base).kind
        && let Some(shape) = dimensions.get(&instances::normalize_identifier(&name.spelling))
        && let Some(mut container) = shape.container.as_deref()
    {
        let mut levels = shape.unpacked.len() + 1;
        while let Some(inner) = container.element.container.as_deref() {
            container = inner;
            levels += 1;
        }
        if suffix.len() > levels
            && container.record.is_some()
            && container.element.unpacked_struct().is_none()
        {
            return Err(unsupported(
                "bit-select or part-select of a container element wider than 64 bits",
            ));
        }
    }
    if let ExprKind::Name(name) = &native.expression(base).kind
        && dimensions
            .get(&instances::normalize_identifier(&name.spelling))
            .is_some_and(|ty| ty.chandle && suffix.len() > ty.unpacked.len())
    {
        return Err(chandles::packed_select());
    }
    // Nor does a chandle member, property or call result.
    if !suffix.is_empty()
        && !matches!(native.expression(base).kind, ExprKind::Name(_))
        && chandles::native_chandle(tree, base, native, dimensions, session)
    {
        return Err(chandles::packed_select());
    }
    // A select of a member of a container element reads the member first.
    let element_member = if suffix.is_empty() {
        None
    } else {
        containers::element_member(base, native, tree, session, dimensions)?
    };
    if element_member.is_some()
        || matches!(
            native.expression(base).kind,
            ExprKind::Concatenation(_) | ExprKind::Replication { .. }
        )
    {
        let expr = match element_member {
            Some((ty, value)) => {
                // The select indices are offsets from bit 0, which only a
                // `[N-1:0]` range declares.
                let zero_based = match ty.packed_ranges() {
                    [range] => {
                        eval_ast_const_expr(&tree.functions, range.right(), &dimensions.const_env)
                            == Some(0)
                            && eval_ast_const_expr(
                                &tree.functions,
                                range.left(),
                                &dimensions.const_env,
                            )
                            .is_some_and(|left| left >= 0)
                    }
                    _ => false,
                };
                if !zero_based || ty.unpacked_struct().is_some() || !ty.members.is_empty() {
                    return Err(unsupported(
                        "select of a container element member that is not declared [N-1:0]",
                    ));
                }
                value
            }
            None => bind_value(base, native, tree, session, dimensions, None)?,
        };
        if suffix.len() != 1 {
            return Err(unsupported("concatenation select"));
        }
        let selection = match suffix[0] {
            DimensionKind::Range { left, right } => {
                selects::AggregateSelection::Range(selects::ParameterRange::Bounds(
                    read_index_value(*left, native, tree, session, dimensions)?,
                    read_index_value(*right, native, tree, session, dimensions)?,
                ))
            }
            DimensionKind::Index(id) => selects::AggregateSelection::Index(read_index_value(
                *id, native, tree, session, dimensions,
            )?),
            DimensionKind::Indexed {
                base,
                operator,
                width,
            } => selects::AggregateSelection::Range(selects::ParameterRange::Indexed {
                base: read_index_value(*base, native, tree, session, dimensions)?,
                width: read_index_value(*width, native, tree, session, dimensions)?,
                plus: operator == "+:",
            }),
            _ => return Err(unsupported("concatenation select")),
        };
        return selects::aggregate_selection_value(tree, expr, selection, dimensions);
    }
    let (parts, qualified) =
        match element_members::element_member(base, native, tree, session, dimensions) {
            Some(_) => (Vec::new(), None),
            None => path(base, native, tree, session, dimensions)?,
        };
    if dimensions.constant_indexed_base && parts.len() == 1 {
        let name = qualified.clone().unwrap_or_else(|| parts[0].clone());
        let mut indices = Vec::new();
        let mut range = None;
        let mut ranged = false;
        for dimension in &suffix {
            if ranged {
                return Err(unsupported("select following a part-select"));
            }
            ranged = !matches!(dimension, DimensionKind::Index(_));
            match dimension {
                DimensionKind::Index(id) => {
                    indices.push(read_index_value(*id, native, tree, session, dimensions)?)
                }
                DimensionKind::Range { left, right } => {
                    range = Some(selects::ParameterRange::Bounds(
                        read_index_value(*left, native, tree, session, dimensions)?,
                        read_index_value(*right, native, tree, session, dimensions)?,
                    ))
                }
                DimensionKind::Indexed {
                    base,
                    operator,
                    width,
                } => {
                    range = Some(selects::ParameterRange::Indexed {
                        base: read_index_value(*base, native, tree, session, dimensions)?,
                        width: read_index_value(*width, native, tree, session, dimensions)?,
                        plus: operator == "+:",
                    })
                }
                _ => return Err(unsupported("select dimension")),
            }
        }
        return if let Some(range) = range {
            selects::indexed_parameter_range(tree, name, indices, range, dimensions)
        } else {
            selects::indexed_parameter_indices(tree, name, indices, dimensions)
        };
    }
    for dimension in &suffix {
        if let DimensionKind::Index(id) = dimension
            && matches!(
                native.expression(ungroup(*id, native)).kind,
                ExprKind::Conditional { .. }
            )
        {
            read_index_value(*id, native, tree, session, dimensions)?;
        }
    }
    let lvalue = selected_lvalue(root, native, tree, session, dimensions, false)?;
    let value = expr_from_lvalue(tree, &lvalue, dimensions);
    if let Expr::Ident(name) = &value
        && let Some(value) = unpacked_structs::whole(name, dimensions)
    {
        return Ok(value);
    }
    let guards = member_context(root, native, tree, session, dimensions)?
        .map(|(guards, _)| guards)
        .unwrap_or_default();
    packed_structs::checked_value(guards, value, tree, dimensions)
}

/// The output actual of a system function that writes place `place`, an
/// [`LValue::Object`]: the bits of the value at the place that its
/// selection names, under the place's checks of the tags its selection
/// crosses, which read the place's value as [`crate::ir::PLACE_VALUE`]: the
/// write captures the place and applies them to the value at the place it
/// captured (IEEE 1800-2023 7.3.2).
pub(super) fn place_output(
    place: &LValue,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let LValue::Object {
        place: object,
        select,
        guards,
        ..
    } = place
    else {
        unreachable!("a place is an object place");
    };
    let load = object.load();
    // The bits the place's selection names, as a variable's are named.
    let value = match &**select {
        LValue::Ident(_) => load,
        // A member keeps its signedness, as a select of a variable does.
        LValue::Select {
            msb,
            lsb,
            signed,
            array_slice_width: None,
            ..
        } => Expr::Select {
            expr: Box::new(load),
            msb: msb.clone(),
            lsb: lsb.clone(),
            signed: *signed,
        },
        _ => return Err(unsupported("output actual of a slice of a place")),
    };
    packed_structs::checked_value(guards.clone(), value, tree, dimensions)
}

/// The place of property `property` of the object that native expression
/// `receiver` denotes, or of `this` without one. Every property place is
/// made here, and every write of a property's place is a write of the
/// property, so a property that is not writable here has none (IEEE
/// 1800-2023 8.19).
pub(super) fn property_place(
    property: classes::Property,
    receiver: Option<ExprId>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<ObjectPlace> {
    objects::check_writable(&property, dimensions)?;
    let handle = objects::receiver(tree, session, receiver, native, dimensions)?;
    Ok(ObjectPlace::Property {
        field: property.field,
        handle,
        r#type: property.r#type,
    })
}

/// The target that native expression `root` assigns when it is a class
/// property of integral type, a select of one, or a select of an element
/// that a container holds by value: an [`LValue::Object`], whose selection
/// is that of a variable of the value's type (see `value_selects`).
pub(in crate::ast) fn object_place(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Option<LValue>> {
    // A place is found in the declarations of the scope.
    if !std::ptr::eq(native, tree.declarations()) {
        return Ok(None);
    }
    let mut base = ungroup(root, native);
    while let ExprKind::Select { expression, .. } = &native.expression(base).kind {
        base = ungroup(*expression, native);
    }
    // A member of a property of packed structure type, `h.p.m[1:0]`.
    let member = property_member_base(root, native, tree, session, dimensions)?;
    let (place, base) = if let Some((base, property, receiver)) = member {
        let place = property_place(property, receiver, native, tree, session, dimensions)?;
        (place, base)
    } else if let Some((property, receiver)) =
        objects::property(tree, session, base, native, dimensions)?
        && property.field.handle.is_none()
        && property.r#type.container.is_none()
        && property.r#type.unpacked_struct().is_none()
    {
        let place = property_place(property, receiver, native, tree, session, dimensions)?;
        (place, base)
    } else if let Some((element, target, container, index)) =
        containers::selected_element(root, tree, session, dimensions)?
    {
        (
            ObjectPlace::Element {
                container,
                index,
                layout: target.layout,
                element: target.element.clone(),
            },
            element,
        )
    } else {
        return Ok(None);
    };
    let steps = value_selects::steps(root, base, native).ok_or_else(disagreement)?;
    let selection = value_selects::selection(
        OBJECT_VALUE,
        Expr::Ident(OBJECT_VALUE.into()),
        place.value_type(),
        &steps,
        native,
        tree,
        session,
        dimensions,
    )?;
    Ok(Some(LValue::Object {
        place: Box::new(place),
        select: Box::new(selection.lvalue),
        guards: selection.guards,
        whole: base,
    }))
}

pub(in crate::ast) fn selected_lvalue(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
    net: bool,
) -> Converted<LValue> {
    let mut base = root;
    let mut suffix = Vec::new();
    while let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(base).kind
    {
        suffix.push(&dimension.kind);
        base = *expression;
    }
    suffix.reverse();
    let element = match element_members::element_member(base, native, tree, session, dimensions) {
        Some(element) => {
            let indices = element
                .indices
                .iter()
                .map(|id| {
                    Ok(selects::self_determined_index(
                        tree,
                        index_value(*id, native, tree, session, dimensions)?,
                        dimensions,
                    ))
                })
                .collect::<Converted<Vec<_>>>()?;
            Some((element, indices))
        }
        None => None,
    };
    let (parts, qualified) = match element {
        Some(_) => (Vec::new(), None),
        None => path(base, native, tree, session, dimensions)?,
    };
    if suffix.is_empty() && parts.len() > 1 {
        return packed_structs::whole_member(tree, parts, qualified, dimensions);
    }
    let mut finish = |name: String, context: &PackedDimensions, strict: bool| {
        select_suffix(
            name, &suffix, native, tree, session, context, dimensions, strict, net,
        )
    };
    let lvalue = if let Some((element, indices)) = element {
        packed_structs::select_element_member(
            tree,
            element.name,
            &element.members,
            indices,
            dimensions,
            finish,
        )?
    } else if parts.len() > 1 {
        packed_structs::select_member(tree, parts, qualified, dimensions, finish)?
    } else {
        finish(
            qualified.unwrap_or_else(|| parts[0].clone()),
            dimensions,
            false,
        )?
    };
    Ok(lvalue)
}

/// The selection that `suffix`, the selects of a native expression, make of
/// variable `name`, whose layout `context` holds; `strict` rejects constant
/// indices outside it, and `net` selects of a net. The indices and bounds
/// are expressions of the scope `scope`, where a member's `context` does not
/// stand for the variable that holds the member.
#[allow(clippy::too_many_arguments)]
pub(in crate::ast::expression_binding) fn select_suffix(
    name: String,
    suffix: &[&DimensionKind],
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
    scope: &PackedDimensions,
    strict: bool,
    net: bool,
) -> Converted<LValue> {
    if context.get(&name).is_some_and(|ty| {
        ty.members
            .first()
            .is_some_and(packed_structs::PackedMember::is_tagged)
            && suffix.len() >= ty.unpacked.len() + ty.packed.len()
    }) {
        return Err(unsupported("bit-select or part-select of a tagged union"));
    }
    if context
        .get(&name)
        .is_some_and(|ty| ty.chandle && suffix.len() > ty.unpacked.len())
    {
        return Err(chandles::packed_select());
    }
    if context
        .get(&name)
        .is_some_and(|ty| ty.real && suffix.len() > ty.unpacked.len())
    {
        return Err(unsupported("bit-select or part-select of a real variable"));
    }
    let mut indices = Vec::new();
    let mut range = None;
    let mut ranged = false;
    for dimension in suffix {
        if ranged {
            return Err(unsupported("select following a part-select"));
        }
        ranged = !matches!(dimension, DimensionKind::Index(_));
        match dimension {
            DimensionKind::Index(id) => {
                let index = index_value(*id, native, tree, session, scope)?;
                indices.push(selects::self_determined_index(tree, index, scope))
            }
            DimensionKind::Range { left, right } => {
                range = Some((
                    index_value(*left, native, tree, session, scope)?,
                    index_value(*right, native, tree, session, scope)?,
                    false,
                ))
            }
            DimensionKind::Indexed {
                base,
                operator,
                width,
            } => {
                let start = index_value(*base, native, tree, session, scope)?;
                let width = index_value(*width, native, tree, session, scope)?;
                let ((msb, lsb), dynamic) = if net {
                    let width = eval_ast_const_expr(&tree.functions, &width, &context.const_env)
                        .ok_or_else(|| {
                            unsupported("indexed part-select width that is not constant")
                        })?;
                    (
                        selects::indexed_select_bounds_values(
                            tree,
                            start,
                            operator == "+:",
                            width,
                            (Some(&name), indices.len()),
                            context,
                        )?,
                        false,
                    )
                } else {
                    selects::indexed_range_values(
                        tree,
                        &name,
                        start,
                        width,
                        operator == "+:",
                        indices.len(),
                        context,
                    )?
                };
                range = Some((msb, lsb, dynamic));
            }
            _ => return Err(unsupported("select dimension")),
        }
    }
    if strict
        && !selects::constant_packed_indices_in_range(&tree.functions, &name, &indices, context)
    {
        return Err(unsupported(format!(
            "constant select of `{name}` outside its declared range"
        )));
    }
    if let Some((msb, lsb, dynamic)) = range {
        selects::lvalue_from_range(tree, name, indices, msb, lsb, dynamic, context)
    } else {
        selects::lvalue_from_indices(tree, name, indices, context)
    }
}
