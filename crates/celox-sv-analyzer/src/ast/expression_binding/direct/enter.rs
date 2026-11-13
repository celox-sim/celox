//! Entering an expression: the checks of the value its destination receives,
//! the forms that are bound whole, and the operands of every other kind.
use super::*;

/// Enter native expression `id`, the value a destination of shape `target`
/// receives: push its value, or the jobs that bind its operands and finish
/// it.
pub(super) fn enter(
    id: ExprId,
    target: Option<Rc<VariableDimensions>>,
    scope: &Option<Rc<PackedDimensions>>,
    root: ExprId,
    permits: &mut chandles::Permits,
    jobs: &mut Vec<Job>,
    values: &mut Vec<Expr>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    // The one check of every value the binder reads: a chandle
    // is only where 6.14 allows one.
    let permitted = permits.check(session, id, target.as_deref(), tree, native, dimensions)?;
    if let Some(value) = dimensions.effect_values.get(&id) {
        values.push(value.clone());
        return Ok(());
    }
    unpacked_structs::check_operands(id, native, tree, session, dimensions)?;
    check_received(id, root, &target, native, tree, session, dimensions)?;
    // A container formal passed by value receives a copy of a
    // container value, as an assignment does (13.5.1, 7.6). A
    // conditional is bound below, its operands receiving the
    // container in turn.
    if let Some(container) = target
        .as_deref()
        .filter(|shape| shape.unpacked.is_empty())
        .and_then(|shape| shape.container.as_deref())
        .filter(|_| {
            !matches!(
                native.expression(ungroup(id, native)).kind,
                ExprKind::Conditional { .. }
            )
        })
    {
        if containers::dynamic_array_new(id, native).is_some() {
            return Err(AnalyzerError::Parse(
                "`new[]` is not an argument; it only initializes or is assigned \
                 to a dynamic array (IEEE 1800-2023 7.5.1)"
                    .into(),
            ));
        }
        values.push(containers::assignment(
            id, tree, session, dimensions, container,
        )?);
        return Ok(());
    }
    // A fixed-size unpacked array takes the elements of a
    // dynamically sized one (7.6). A conditional is bound below,
    // its operands receiving the array in turn.
    if let Some(shape) = target
        .as_deref()
        .filter(|shape| {
            !shape.unpacked.is_empty()
                && !containers::holds_container(shape, tree, session, dimensions)
        })
        .filter(|_| {
            !matches!(
                native.expression(ungroup(id, native)).kind,
                ExprKind::Conditional { .. }
            )
        })
        && let Some(source) = containers::value_container(id, native, tree, session, dimensions)
    {
        let fixed = shape
            .type_id(&mut session.types, &tree.functions, &dimensions.const_env)
            .ok_or_else(|| unsupported("type of a fixed-size unpacked array"))?;
        if session.types.assignable_array(source, fixed).is_err() {
            return Err(AnalyzerError::IncompatibleUnpackedArray {
                context: "assignment".into(),
                actual: session.types.spell(source),
                target: session.types.spell(fixed),
            });
        }
        values.push(streams::assign_to_fixed(
            id, shape, native, tree, session, dimensions,
        )?);
        return Ok(());
    }
    if let Some(value) = locators::index(id, native, tree, session, dimensions)? {
        values.push(value);
        return Ok(());
    }
    if let Some(value) = locators::bind(id, native, tree, session, dimensions)? {
        values.push(value);
        return Ok(());
    }
    let enum_method = match &native.expression(id).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => enums::method(tree, session, *expression, native, dimensions)?
            .map(|method| (method, arguments.as_slice())),
        ExprKind::Member { .. } => {
            enums::method(tree, session, id, native, dimensions)?.map(|method| (method, &[][..]))
        }
        _ => None,
    };
    if let Some(((method, receiver, domain), arguments)) = enum_method {
        // `v.next()` lists one empty argument.
        let arguments = match arguments {
            [argument] if argument.name.is_none() && argument.value.is_none() => &[],
            arguments => arguments,
        };
        let counts = arguments
            .iter()
            .map(|argument| argument.value.filter(|_| argument.name.is_none()))
            .collect::<Option<Vec<_>>>()
            .filter(|counts| counts.len() <= method.formals())
            .ok_or_else(|| {
                unsupported(format!(
                    "arguments of enumerated type method `{}`",
                    native.token_text(native.expression(id).tokens).trim()
                ))
            })?;
        let mut children = vec![receiver];
        children.extend(&counts);
        jobs.push(Job::Finish(
            Finish::EnumMethod {
                method,
                domain,
                count: !counts.is_empty(),
            },
            children.len(),
            scope.clone(),
        ));
        jobs.extend(
            children
                .into_iter()
                .rev()
                .map(|id| Job::Enter(id, None, scope.clone())),
        );
        return Ok(());
    }
    if let Some(call) = scheduler_call(tree, session, id, native, dimensions)? {
        values.push(*call);
        return Ok(());
    }
    if let Some((value, _)) = array_reductions::bind(id, native, tree, session, dimensions)? {
        values.push(value);
        return Ok(());
    }
    if let Some(value) = containers::value(id, native, tree, session, dimensions, false)? {
        values.push(value);
        return Ok(());
    }
    if let Some(call) = objects::constraint_mode_call(tree, session, id, native, dimensions, false)?
    {
        values.push(call);
        return Ok(());
    }
    if let Some(call) = objects::rand_mode_call(tree, session, id, native, dimensions, false)? {
        values.push(call);
        return Ok(());
    }
    if timescales::is_query(id, native) {
        values.push(Expr::Literal(parameters::format_typed_parameter_literal(
            i128::from(timescales::query(id, tree)?),
            32,
            true,
        )));
        return Ok(());
    }
    let time_call = match &native.expression(id).kind {
        ExprKind::Name(name)
            if matches!(name.spelling.as_str(), "$time" | "$stime" | "$realtime") =>
        {
            Some((name.spelling.clone(), &[][..]))
        }
        ExprKind::Call {
            expression,
            arguments,
        } => call_name(tree, *expression, native)
            .filter(|name| matches!(name.as_str(), "$time" | "$stime" | "$realtime"))
            .map(|name| (name, arguments.as_slice())),
        _ => None,
    };
    if let Some((name, arguments)) = time_call {
        let presence = arguments
            .iter()
            .map(|argument| {
                if argument.value.is_some() {
                    system_functions::Arg::Given
                } else {
                    system_functions::Arg::Omitted
                }
            })
            .collect::<Vec<_>>();
        let function = system_functions::check_function_call(
            &name,
            (!arguments.iter().any(|argument| argument.name.is_some()))
                .then_some(presence.as_slice()),
        )?;
        values.push(Expr::Call {
            callee: Callee::System(function),
            args: vec![Some(Expr::Literal(format_typed_parameter_literal(
                i128::from(timescales::time_divisor(id, tree)?),
                64,
                false,
            )))],
            arg_order: Vec::new(),
        });
        return Ok(());
    }
    enter_kind(
        id, target, scope, permitted, permits, jobs, values, native, tree, session, dimensions,
    )
}

/// Check native value `id` against the destination of shape `target` that
/// receives it: an enumeration, an unpacked structure, a virtual interface
/// and a tagged union take the values of their own types.
fn check_received(
    id: ExprId,
    root: ExprId,
    target: &Option<Rc<VariableDimensions>>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    // A value received by an enumerated type is of that type;
    // any other value needs a cast (IEEE 1800-2023 6.19.3,
    // 6.19.4). Only the received value itself is judged, not a
    // cast's operand, which takes the target as its context.
    if id == root
        && let Some(enumeration) = target
            .as_deref()
            .filter(|shape| {
                shape.unpacked.is_empty()
                    && !containers::holds_container(shape, tree, session, dimensions)
            })
            .and_then(|shape| {
                shape.type_id(&mut session.types, &tree.functions, &dimensions.const_env)
            })
            .filter(|ty| session.types.is_enum(*ty))
    {
        assignments::check_assign(tree, session, id, native, dimensions, enumeration, || {
            "assignment".into()
        })?;
    }
    // An argument, result or other value received as an unpacked
    // structure is a pattern or a value of its type (6.22.3).
    if let Some(structure) = target
        .as_deref()
        .filter(|shape| shape.unpacked.is_empty())
        .and_then(|shape| unpacked_structs::shape_struct(shape, tree, session, dimensions))
    {
        unpacked_structs::check_value(structure, id, native, tree, session, dimensions)?;
    } else if target.as_deref().is_some_and(|shape| {
        shape.unpacked.is_empty() && !containers::holds_container(shape, tree, session, dimensions)
    }) && unpacked_structs::value_struct(id, native, tree, session, dimensions).is_some()
    {
        return Err(unsupported(
            "unpacked structure value received by a destination that is not of its type",
        ));
    }
    // A virtual interface, or a fixed-size array of them, takes the values
    // of its own type (25.9).
    let target_type = target.as_deref().and_then(|shape| {
        shape.type_id(&mut session.types, &tree.functions, &dimensions.const_env)
    });
    if let Some(ty) = target_type.and_then(|ty| virtual_interfaces::of_type(&session.types, ty)) {
        virtual_interfaces::check_assignment(tree, session, id, native, dimensions, &ty)?;
    }
    if let Some(ty) =
        target_type.and_then(|ty| virtual_interfaces::element_of_array(&session.types, ty))
    {
        virtual_interfaces::check_array_assignment(tree, session, id, native, dimensions, &ty)?;
    }
    if let Some(shape) = target.as_deref()
        && shape
            .members
            .first()
            .is_some_and(packed_structs::PackedMember::is_tagged)
        && !matches!(
            native.expression(id).kind,
            ExprKind::Group(_) | ExprKind::Conditional { .. } | ExprKind::Tagged { .. }
        )
    {
        let actual = if let ExprKind::Cast { target, .. } = native.expression(id).kind {
            cast_shape(target, native, tree, session, dimensions)?
                .and_then(|shape| shape.pattern_type(&tree.functions, dimensions))
        } else {
            bound_patterns::native_value_type(id, tree, session, dimensions)?
        };
        if !actual.is_some_and(|ty| {
            ty.members
                .first()
                .is_some_and(|member| member.same_owner(&shape.members[0]))
                && ty.packed_ranges.len() == 1
                && ty.unpacked_ranges.is_empty()
        }) {
            return Err(unsupported(
                "assignment to a tagged union requires the same union type or a tagged expression",
            ));
        }
    }
    Ok(())
}

/// Enter native expression `id` by its kind: push its value, or the jobs
/// that bind its operands and finish it.
fn enter_kind(
    id: ExprId,
    target: Option<Rc<VariableDimensions>>,
    scope: &Option<Rc<PackedDimensions>>,
    permitted: bool,
    permits: &mut chandles::Permits,
    jobs: &mut Vec<Job>,
    values: &mut Vec<Expr>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    let method_call = objects::method_call(tree, session, id, native, dimensions)?;
    let (finish, children) = match &native.expression(id).kind {
        ExprKind::Group(inner) => {
            permits.grant_if(permitted, *inner);
            jobs.push(Job::Enter(*inner, target, scope.clone()));
            return Ok(());
        }
        ExprKind::Literal(text) if text == "null" => {
            // A handle of no object (IEEE 1800-2023 8.4).
            values.push(Expr::Literal("32'h0".into()));
            return Ok(());
        }
        ExprKind::Literal(text) => {
            if let Some(packed) = packed_string_literal(text) {
                values.push(Expr::Literal(packed));
            } else if typecheck::integral_literal_type_metadata(text).is_none()
                && let Some(real) = timescales::real_literal(id, text, tree)?
            {
                values.push(Expr::Literal(real));
            } else {
                if typecheck::integral_literal_type_metadata(text).is_none()
                    && !(text.contains(['.', 'e', 'E'])
                        && text.replace('_', "").parse::<f64>().is_ok())
                {
                    return Err(unsupported("primary literal"));
                }
                values.push(Expr::Literal(text.clone()));
            }
            return Ok(());
        }
        ExprKind::Name(name) if !name.spelling.starts_with('$') => {
            enter_name(id, name, values, native, tree, session, dimensions)?;
            return Ok(());
        }
        ExprKind::Unary {
            operator,
            expression,
            postfix: false,
        } if !matches!(operator.as_str(), "++" | "--") => {
            // A Boolean test (6.14).
            permits.grant_if(operator == "!", *expression);
            (Finish::Unary(operator.clone()), vec![(*expression, None)])
        }
        // A comparison of type references is a constant
        // (IEEE 1800-2023 6.23).
        ExprKind::Binary { .. } if type_references::compares(id, native) => {
            values.push(type_references::comparison(
                tree, session, id, native, dimensions,
            )?);
            return Ok(());
        }
        ExprKind::Binary {
            left,
            operator,
            right,
        } if operator != "inside" => {
            if matches!(operator.as_str(), "==" | "!=" | "===" | "!==") {
                virtual_interfaces::check_equality(
                    tree, session, *left, *right, native, dimensions,
                )?;
            }
            permits.binary(session, operator, *left, *right, tree, native, dimensions)?;
            (
                Finish::Binary(operator.clone()),
                vec![(*left, None), (*right, None)],
            )
        }
        ExprKind::Conditional {
            condition,
            then_expression,
            else_expression,
        } => enter_conditional(
            id,
            condition,
            then_expression,
            else_expression,
            &target,
            permitted,
            permits,
            native,
            tree,
            session,
            dimensions,
        )?,
        ExprKind::Concatenation(parts) if !parts.is_empty() => {
            (Finish::Concat, parts.iter().map(|id| (*id, None)).collect())
        }
        ExprKind::Replication { count, expressions } if !expressions.is_empty() => {
            let literals = expressions.iter().all(|&id| {
                matches!(
                    &native.expression(ungroup(id, native)).kind,
                    ExprKind::Literal(text) if text.starts_with('"')
                )
            });
            let mut children = vec![(*count, None)];
            children.extend(expressions.iter().map(|id| (*id, None)));
            (Finish::Repeat { literals }, children)
        }
        ExprKind::Cast { target, expression }
            if class_cast(tree, session, *target, *expression, native, dimensions)? =>
        {
            // An upcast is the assignment of the handle (6.24.1).
            values.push(bind_value(
                *expression,
                native,
                tree,
                session,
                dimensions,
                None,
            )?);
            return Ok(());
        }
        ExprKind::Cast { target, expression }
            if streams::is_stream(*expression, native)
                || bit_stream_cast(tree, session, *target, *expression, native, dimensions) =>
        {
            enter_stream_cast(
                target, expression, scope, jobs, values, native, tree, session, dimensions,
            )?;
            return Ok(());
        }
        ExprKind::Cast { target, expression } => (
            Finish::Cast(cast_target_native(
                *target,
                *expression,
                native,
                tree,
                session,
                dimensions,
            )?),
            vec![(
                *expression,
                cast_shape(*target, native, tree, session, dimensions)?.map(Rc::new),
            )],
        ),
        _ if let Some((_, args)) = method_call => {
            enter_method_call(id, args, native, tree, session, dimensions)?
        }
        ExprKind::Call {
            expression,
            arguments: args,
        } if strings::method(tree, session, *expression, native, dimensions).is_some() => {
            enter_string_method(expression, args, native, tree, session, dimensions)?
        }
        ExprKind::Call {
            expression,
            arguments: args,
        } => {
            let Some(operands) = enter_call(
                id, expression, args, scope, jobs, values, native, tree, session, dimensions,
            )?
            else {
                return Ok(());
            };
            operands
        }
        ExprKind::Name(name) if name.spelling.starts_with('$') => {
            let function = system_functions::check_function_call(&name.spelling, Some(&[]))?;
            (
                Finish::Call {
                    name: name.spelling.clone(),
                    args: Vec::new(),
                    order: Vec::new(),
                    system: Some(function),
                },
                Vec::new(),
            )
        }
        ExprKind::Select {
            expression,
            dimension,
        } if strings::native_string(tree, session, *expression, native, dimensions) => {
            let celox_sv_syntax::DimensionKind::Index(index) = dimension.kind else {
                return Err(unsupported("part-select of a string"));
            };
            (
                Finish::StringCall(StringOp::Getc),
                vec![(*expression, None), (index, None)],
            )
        }
        ExprKind::Member { .. }
            if objects::class_parameter(tree, session, id, native, dimensions)?.is_some() =>
        {
            values.push(
                objects::class_parameter(tree, session, id, native, dimensions)?
                    .expect("checked by the guard"),
            );
            return Ok(());
        }
        ExprKind::Member { .. }
            if virtual_interfaces::native_member(tree, session, id, native, dimensions)
                .is_some() =>
        {
            let (handle, ty, member) =
                virtual_interfaces::native_member(tree, session, id, native, dimensions)
                    .expect("checked by the guard");
            (
                Finish::VirtualMember(Box::new((ty.as_ref().clone(), member))),
                vec![(handle, None)],
            )
        }
        ExprKind::Member { .. }
            if containers::element_member(id, native, tree, session, dimensions)?.is_some() =>
        {
            let (_, value) = containers::element_member(id, native, tree, session, dimensions)?
                .expect("checked by the guard");
            values.push(value);
            return Ok(());
        }
        ExprKind::Member { .. }
            if objects::aggregate_property(tree, session, id, native, dimensions)?.is_some() =>
        {
            let (aggregate, class, receiver) =
                objects::aggregate_property(tree, session, id, native, dimensions)?
                    .expect("checked by the guard");
            let handle = objects::receiver(tree, session, receiver, native, dimensions)?;
            values.push(objects::aggregate_load(&aggregate, &class, &handle)?);
            return Ok(());
        }
        ExprKind::Member { .. }
            if objects::property(tree, session, id, native, dimensions)?.is_some() =>
        {
            let (property, receiver) = objects::property(tree, session, id, native, dimensions)?
                .expect("checked by the guard");
            match receiver {
                Some(receiver) => (
                    Finish::ObjectLoad(Box::new(property)),
                    vec![(receiver, None)],
                ),
                None => {
                    values.push(objects::load(&property, objects::this()));
                    return Ok(());
                }
            }
        }
        ExprKind::Member { .. } | ExprKind::Select { .. }
            if virtual_interfaces::native_identity(tree, session, id, dimensions).is_some() =>
        {
            values.push(
                virtual_interfaces::native_identity(tree, session, id, dimensions)
                    .expect("checked by the guard"),
            );
            return Ok(());
        }
        ExprKind::Select { .. }
            if let Some((handle, ty, member)) = virtual_interfaces::native_selected_member(
                tree, session, id, native, dimensions,
            ) =>
        {
            let selected = selected_value(id, native, tree, session, dimensions)?;
            let handle = bind_value(handle, native, tree, session, dimensions, None)?;
            values.push(virtual_interfaces::selected_read(
                tree, session, handle, &ty, &member, selected, dimensions,
            )?);
            return Ok(());
        }
        ExprKind::Member { .. } | ExprKind::Select { .. } => {
            let value = selected_value(id, native, tree, session, dimensions)?;
            values.push(
                if strings::fixed_array_element(tree, session, id, native, dimensions) {
                    strings::call(StringOp::FromHandle, vec![value])
                } else {
                    value
                },
            );
            return Ok(());
        }
        ExprKind::Pattern { .. } => {
            values.push(patterns::bind_native(
                id,
                native,
                tree,
                session,
                dimensions,
                target.as_deref(),
            )?);
            return Ok(());
        }
        ExprKind::Binary {
            left,
            operator,
            right,
        } if operator == "inside" => enter_inside(left, right, native)?,
        ExprKind::Unary { .. } => {
            return Err(unsupported("increment or decrement expression"));
        }
        ExprKind::Assignment { .. } => {
            return Err(unsupported("assignment used as an expression"));
        }
        ExprKind::Tagged { member, value } => {
            let shape = target
                .as_deref()
                .ok_or_else(|| unsupported("tagged union expression without a type context"))?;
            let plan = packed_structs::construction_member(
                shape,
                &instances::normalize_identifier(&member.spelling),
            )?;
            if value.is_some() != plan.shape.is_some() {
                return Err(unsupported(
                    "tagged member payload does not match void/non-void member",
                ));
            }
            let children = value
                .iter()
                .map(|id| (*id, plan.shape.clone().map(Rc::new)))
                .collect();
            (Finish::Tagged(plan), children)
        }
        // A stream is the source of an assignment to a target of
        // the shape the context gives (IEEE 1800-2023 11.4.14).
        ExprKind::Streaming { .. } => {
            let Some(shape) = target.as_deref() else {
                return Err(AnalyzerError::InvalidStream(
                    "a streaming concatenation operand shall first be cast to a bit-stream type"
                        .into(),
                ));
            };
            let receiver = streams::receiver(shape, &tree.functions, dimensions)?;
            values.push(streams::pack(
                id,
                Some(receiver),
                native,
                tree,
                session,
                dimensions,
            )?);
            return Ok(());
        }
        ExprKind::Concatenation(_) => {
            return Err(unsupported("empty unpacked array concatenation"));
        }
        ExprKind::RandomizeWith {
            expression,
            constraints,
        } => {
            values.push(objects::randomize_with(
                tree,
                session,
                *expression,
                constraints.as_deref(),
                native,
                dimensions,
            )?);
            return Ok(());
        }
        ExprKind::ArrayMethodWith { .. } => {
            return Err(unsupported("array method with expression"));
        }
        ExprKind::MinTypMax { .. } => return Err(unsupported("mintypmax expression")),
        _ => return Err(unsupported("primary expression")),
    };
    jobs.push(Job::Finish(finish, children.len(), scope.clone()));
    jobs.extend(
        children
            .into_iter()
            .rev()
            .map(|(id, shape)| Job::Enter(id, shape, scope.clone())),
    );
    Ok(())
}

/// Enter the name of a variable, a property or `null`.
fn enter_name(
    id: ExprId,
    name: &celox_sv_syntax::Name,
    values: &mut Vec<Expr>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    let name = instances::normalize_identifier(&name.spelling);
    if name == "null" {
        values.push(Expr::Literal("32'h0".into()));
        return Ok(());
    }
    if matches!(name.as_str(), "this" | "super") && dimensions.get(&name).is_none() {
        return Err(unsupported(format!("`{name}`")));
    }
    if let Some(value) = unpacked_structs::whole(&name, dimensions) {
        values.push(value);
        return Ok(());
    }
    if let Some((aggregate, class, _)) =
        objects::aggregate_property(tree, session, id, native, dimensions)?
    {
        values.push(objects::aggregate_load(
            &aggregate,
            &class,
            &objects::this(),
        )?);
        return Ok(());
    }
    if let Some(value) = objects::unqualified_property(tree, session, dimensions, &name)? {
        values.push(value);
        return Ok(());
    }
    if let Some(value) = virtual_interfaces::identity(tree, session, &name, dimensions)? {
        values.push(value);
        return Ok(());
    }
    values.push(expressions::identifier_expression(tree, name, dimensions)?);
    Ok(())
}

/// Enter a conditional: its condition, and its branches as values of
/// what receives the conditional.
fn enter_conditional(
    id: ExprId,
    condition: &ExprId,
    then_expression: &ExprId,
    else_expression: &ExprId,
    target: &Option<Rc<VariableDimensions>>,
    permitted: bool,
    permits: &mut chandles::Permits,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Operands> {
    // The condition is a Boolean test; the branches are
    // values where the conditional is one.
    permits.grant(*condition);
    permits.conditional(
        session,
        permitted,
        [*then_expression, *else_expression],
        tree,
        native,
        dimensions,
    )?;
    // Without a target, a conditional of containers has
    // the type of its operands (IEEE 1800-2023 11.4.11),
    // which receives them as a target would.
    let target = match target.clone() {
        Some(target) => Some(target),
        None => containers::native_type(id, native, tree, session, dimensions)?
            .map(|container| Rc::new(containers::shape(&container))),
    };
    Ok((
        Finish::Mux(containers::conditional(
            *condition,
            target.as_deref(),
            tree,
            session,
            native,
            dimensions,
        )),
        vec![
            (*condition, None),
            (*then_expression, target.clone()),
            (*else_expression, target),
        ],
    ))
}

/// Enter a cast of a streaming concatenation, or a bit-stream cast
/// (IEEE 1800-2023 6.24.3, 11.4.14).
fn enter_stream_cast(
    target: &ExprId,
    expression: &ExprId,
    scope: &Option<Rc<PackedDimensions>>,
    jobs: &mut Vec<Job>,
    values: &mut Vec<Expr>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    // Both sides convert by their layouts in a stream
    // (6.24.3), which also tells which the lowering
    // cannot give yet.
    let target_layout = stream_layouts(tree, session, *target, *expression, native, dimensions)?;
    // A cast to a type that is not integral receives the
    // stream as an assignment does.
    let ty = match call_name(tree, *target, native) {
        Some(name) => dimensions.type_aliases.get(&name).cloned(),
        None => match native.expression(ungroup(*target, native)).kind {
            ExprKind::Type(ty) => {
                type_binding::binding::bind_in(ty, native, tree, session, dimensions)?
            }
            _ => None,
        },
    };
    if let Some(shape) = ty
        .as_ref()
        .map(procedural::dimensions_from_type)
        .filter(|shape| match target_layout {
            Some(semantic_types::StreamLayout::Bits { element: None, .. }) => false,
            Some(_) => true,
            None => shape.container.is_some() || shape.string || !shape.unpacked.is_empty(),
        })
    {
        let receiver = streams::cast_receiver(&shape, &tree.functions, dimensions)?;
        values.push(streams::pack(
            *expression,
            Some(receiver),
            native,
            tree,
            session,
            dimensions,
        )?);
        return Ok(());
    }
    let cast = cast_target_native(*target, *expression, native, tree, session, dimensions)?;
    let receiver = match &cast {
        casts::RuntimeCastTarget::String => {
            values.push(streams::pack(
                *expression,
                Some(streams::Receiver::String),
                native,
                tree,
                session,
                dimensions,
            )?);
            return Ok(());
        }
        casts::RuntimeCastTarget::Integral { target, .. }
        | casts::RuntimeCastTarget::Zero(target) => Some(streams::Receiver::Cast(target.width)),
        _ => None,
    };
    jobs.push(Job::Finish(Finish::Cast(cast), 1, scope.clone()));
    jobs.push(Job::Stream(*expression, receiver, scope.clone()));
    Ok(())
}

/// Enter a set membership test, `left inside {right}`.
fn enter_inside(left: &ExprId, right: &ExprId, native: &Declarations) -> Converted<Operands> {
    let ExprKind::Set(items) = &native.expression(*right).kind else {
        return Err(unsupported("inside expression"));
    };
    let mut children = vec![(*left, None)];
    let mut ranges = Vec::new();
    for item in items {
        match item.kind {
            DimensionKind::Index(id) => {
                children.push((id, None));
                ranges.push(false);
            }
            DimensionKind::Range { left, right } => {
                children.extend([(left, None), (right, None)]);
                ranges.push(true);
            }
            _ => return Err(unsupported("inside range")),
        }
    }
    Ok((Finish::Inside(ranges), children))
}
