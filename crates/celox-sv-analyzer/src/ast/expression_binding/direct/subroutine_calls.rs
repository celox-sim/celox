//! Calls of methods, system functions and subroutines: the operands they
//! enter and the calls that finish them.
use super::*;

/// Enter a call of a class method with actuals `args`.
pub(super) fn enter_method_call(
    id: ExprId,
    args: &[celox_sv_syntax::Argument],
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Operands> {
    let (name, formals, receiver) =
        objects::call_method(tree, session, id, native, dimensions)?.expect("checked by the guard");
    let selection = name
        .ends_with("::randomize")
        .then(|| objects::random_selection(tree, session, dimensions, &name, args, native, false))
        .transpose()?;
    if name.ends_with("::srandom") && (args.len() != 1 || args[0].name.is_some()) {
        return Err(unsupported(format!("arguments of method `{name}`")));
    }
    let (actuals, order) = match selection {
        Some(_) => (Vec::new(), Vec::new()),
        None => formal_arguments(
            name.strip_suffix("$virtual").unwrap_or(&name),
            args,
            &formals,
        )?,
    };
    let mut children = Vec::new();
    if let objects::Receiver::Handle(handle) = receiver {
        children.push((handle, None));
    }
    // The shapes of the formals, after `this` of a method
    // that is not static, receive assignment patterns.
    let first = usize::from(receiver != objects::Receiver::Static);
    let shapes = dimensions.subroutine_param_shapes.get(&name);
    for (position, value) in actuals.iter().enumerate() {
        let Some(value) = *value else { continue };
        references::check_actual(
            &name,
            first + position,
            value,
            native,
            tree,
            session,
            dimensions,
        )?;
        check_argument(
            &name,
            first + position,
            position + 1,
            value,
            native,
            tree,
            dimensions,
            session,
        )?;
        check_written_container(&name, first + position, value, tree, session, dimensions)?;
        let shape = shapes.and_then(|shapes| shapes.get(first + position));
        let copied_in = copied_in(dimensions, &name, first + position);
        // The formal is copied out to the actual.
        if let Some(formal) = shape
            .filter(|_| !copied_in)
            .and_then(|shape| {
                shape.type_id(&mut session.types, &tree.functions, &dimensions.const_env)
            })
            .and_then(|ty| virtual_interfaces::of_type(&session.types, ty))
        {
            virtual_interfaces::check_copy_out(tree, session, value, native, dimensions, &formal)?;
        }
        let shape = shape
            .filter(|shape| {
                unpacked_structs::holds_struct(shape, tree, session, dimensions)
                    || chandles::receives_chandle(shape)
                    || (containers::holds_container(shape, tree, session, dimensions)
                        || virtual_interfaces::holds_virtual_interface(
                            shape, tree, session, dimensions,
                        ))
                        && copied_in
            })
            .cloned()
            .map(Rc::new);
        children.push((value, shape));
    }
    Ok((
        Finish::MethodCall(Box::new(MethodCall {
            name,
            given: actuals.iter().map(Option::is_some).collect(),
            order,
            formals: formals.len(),
            receiver,
            selection,
        })),
        children,
    ))
}

/// Enter a call of a string method that is a value.
pub(super) fn enter_string_method(
    expression: &ExprId,
    args: &[celox_sv_syntax::Argument],
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Operands> {
    let (op, base) = strings::method(tree, session, *expression, native, dimensions)
        .expect("checked by the guard");
    if op.updates_receiver() {
        return Err(unsupported(format!("string method `{op}` used as a value")));
    }
    let (_, count) = StringOp::method(&op.to_string()).expect("a method name");
    let values = args
        .iter()
        .map(|arg| arg.value.filter(|_| arg.name.is_none()))
        .collect::<Option<Vec<_>>>()
        .filter(|values| values.len() == count)
        .ok_or_else(|| unsupported(format!("arguments of string method `{op}`")))?;
    Ok((
        Finish::StringCall(op),
        std::iter::once(base)
            .chain(values)
            .map(|id| (id, None))
            .collect(),
    ))
}

/// Enter a call of `expression` with actuals `args` that is none of a class
/// or string method: `None` when its value or its jobs are pushed.
pub(super) fn enter_call(
    id: ExprId,
    expression: &ExprId,
    args: &[celox_sv_syntax::Argument],
    scope: &Option<Rc<PackedDimensions>>,
    jobs: &mut Vec<Job>,
    values: &mut Vec<Expr>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Option<Operands>> {
    if matches!(
        call_name(tree, *expression, native).as_deref(),
        Some("randomize" | "std::randomize")
    ) {
        if args
            .iter()
            .any(|arg| arg.name.is_some() || arg.value.is_none())
        {
            return Err(unsupported(
                "scope randomize requires positional variable identifiers",
            ));
        }
        let actuals = args.iter().filter_map(|arg| arg.value).collect::<Vec<_>>();
        let mut domains = Vec::with_capacity(actuals.len());
        for &actual in &actuals {
            let ExprKind::Name(name) = &native.expression(actual).kind else {
                return Err(unsupported(
                    "scope randomize requires whole packed variable identifiers",
                ));
            };
            let name = instances::normalize_identifier(&name.spelling);
            let shape = dimensions
                .get(&name)
                .ok_or_else(|| unsupported("scope randomize variable"))?;
            if !shape.unpacked.is_empty()
                || !shape.randomizable
                || shape.real
                || shape.string
                || objects::shape_class(shape, tree, session, dimensions).is_some()
            {
                return Err(unsupported(
                    "scope randomize requires a packed variable with a supported enum domain",
                ));
            }
            domains.push(shape.enum_domain.clone());
            if !expressions::query_actual_is_writable(&Expr::Ident(name), dimensions) {
                return Err(unsupported("scope randomize argument is not writable"));
            }
        }
        jobs.push(Job::Finish(
            Finish::ScopeRandomize(domains),
            actuals.len(),
            scope.clone(),
        ));
        jobs.extend(
            actuals
                .into_iter()
                .rev()
                .map(|id| Job::Enter(id, None, scope.clone())),
        );
        return Ok(None);
    }
    let name = bound_callable_name(tree, session, *expression, native, dimensions)?
        .ok_or_else(|| unsupported("method or randomize call"))?;
    let cast = match name.as_str() {
        "$cast" => objects::cast(tree, session, args, native, dimensions)?,
        _ => None,
    };
    Ok(Some(if let Some(cast) = cast {
        let objects::Cast::Call {
            name,
            destination,
            source,
        } = cast
        else {
            values.push(Expr::Literal("0".into()));
            return Ok(None);
        };
        (
            Finish::Call {
                name,
                args: vec![Some(destination), Some(source)],
                order: Vec::new(),
                system: None,
            },
            vec![(destination, None), (source, None)],
        )
    } else if name == "$sformatf" {
        let actuals = format_arguments(&name, args)?;
        (
            Finish::Format {
                ticks_per_unit: timescales::time_divisor(id, tree)?,
                ids: actuals.clone(),
            },
            actuals.into_iter().map(|id| (id, None)).collect(),
        )
    } else if name.starts_with('$') && !name.contains("::") {
        return enter_system_call(
            id, name, args, scope, jobs, values, native, tree, session, dimensions,
        );
    } else {
        enter_subroutine_call(name, args, native, tree, session, dimensions)?
    }))
}

/// Enter a call of system function `name`: `None` when its value or its
/// jobs are pushed.
fn enter_system_call(
    id: ExprId,
    name: String,
    args: &[celox_sv_syntax::Argument],
    scope: &Option<Rc<PackedDimensions>>,
    jobs: &mut Vec<Job>,
    values: &mut Vec<Expr>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Option<Operands>> {
    let presence = args
        .iter()
        .map(|arg| {
            if arg.value.is_some() {
                system_functions::Arg::Given
            } else {
                system_functions::Arg::Omitted
            }
        })
        .collect::<Vec<_>>();
    let function = system_functions::check_function_call(
        &name,
        (!args.iter().any(|arg| arg.name.is_some())).then_some(presence.as_slice()),
    )?;
    if function == SystemFunction::Typename {
        let argument = args[0]
            .value
            .ok_or_else(|| unsupported(format!("operand of `{name}`")))?;
        values.push(type_names::query_value(
            argument,
            tree,
            session,
            scope.as_deref().unwrap_or(dimensions),
        )?);
        return Ok(None);
    }
    if function == SystemFunction::Bits
        && let Some(argument) = args.first().and_then(|a| a.value)
    {
        let argument = ungroup(argument, native);
        let ordinary = matches!(
            native.expression(argument).kind,
            ExprKind::Unary { .. }
                | ExprKind::Binary { .. }
                | ExprKind::Conditional { .. }
                | ExprKind::Concatenation(_)
                | ExprKind::Replication { .. }
                | ExprKind::Cast { .. }
                | ExprKind::Call { .. }
        );
        if ordinary {
            let context = match &scope {
                Some(scope) => scope.clone(),
                None => Rc::new(dimensions::query_context_for_operand(
                    argument, tree, session, dimensions,
                )?),
            };
            if scope.is_none() {
                dimensions::validate_data_operand(argument, function, tree, &context)?;
            }
            jobs.push(Job::Finish(Finish::Bits, 1, Some(context.clone())));
            jobs.push(Job::Enter(argument, None, Some(context)));
            return Ok(None);
        }
    }
    if matches!(
        function,
        SystemFunction::Bits
            | SystemFunction::Size
            | SystemFunction::Dimensions
            | SystemFunction::UnpackedDimensions
            | SystemFunction::ArrayQuery(_)
    ) {
        values.push(
            dimensions::query_value(id, tree, session, dimensions)?
                .ok_or_else(|| unsupported(format!("operand of `{name}`")))?,
        );
        return Ok(None);
    }
    if function == SystemFunction::Isunbounded {
        let arg = args
            .first()
            .and_then(|arg| arg.value)
            .and_then(|id| call_name(tree, id, native));
        let Some(arg) = arg.filter(|name| {
            dimensions
                .const_env
                .contains_key(&parameters::unbounded_parameter_marker(name))
        }) else {
            return Err(AnalyzerError::InvalidSystemTfCall {
                name,
                detail: "argument must name a value parameter".into(),
            });
        };
        values.push(Expr::Literal(format!(
            "1'b{}",
            usize::from(
                dimensions
                    .const_env
                    .get(&parameters::unbounded_parameter_marker(&arg))
                    == Some(&1)
            )
        )));
        return Ok(None);
    }
    if function.is_coverage() {
        // A constant function calls constant
        // system functions only (13.4.3), and a
        // coverage function is none (11.2.1).
        if dimensions.constant_function_bodies {
            return Err(AnalyzerError::InvalidSystemTfCall {
                name,
                detail: "a coverage function is not a constant system function".into(),
            });
        }
        // Celox collects no code coverage, so the
        // Clause 40 functions fold to their
        // no-coverage results (40.3.2): `SV_COV_NOCOV
        // (0), `SV_COV_ERROR (-1) for bad arguments,
        // and `SV_COV_OK (1) for operations with
        // nothing to do. $get_coverage is 100.0 on a
        // design without covergroup instances
        // (19.11), which is every design Celox
        // elaborates. The scope and name arguments
        // name a hierarchy or a database, not a
        // value, so they are not bound; the
        // selectors before them are constant
        // expressions.
        let mut selectors = [None::<i128>; 3];
        let count = match function {
            SystemFunction::CoverageControl => 3,
            SystemFunction::CoverageGetMax | SystemFunction::CoverageGet => 2,
            SystemFunction::CoverageMerge | SystemFunction::CoverageSave => 1,
            _ => 0,
        };
        for (position, selector) in selectors.iter_mut().enumerate().take(count) {
            let Some(id) = args.get(position).and_then(|arg| arg.value) else {
                continue;
            };
            let value = match crate::ast::constant_binding::bind_in(
                id, native, tree, session, dimensions,
            ) {
                Ok(value) => value,
                Err(AnalyzerError::Unsupported(_)) => None,
                Err(error) => return Err(error),
            };
            *selector = value.and_then(|value| {
                eval_ast_const_expr(&tree.functions, &value, &dimensions.const_env)
            });
        }
        let constant = |position: usize| selectors[position];
        let valid_type = |v: i128| (20..=23).contains(&v);
        let valid_scope = |v: i128| v == 10 || v == 11;
        let result: Option<i128> = match function {
            SystemFunction::CoverageControl => {
                match (constant(0), constant(1), constant(2)) {
                    (Some(control), Some(ty), Some(scope))
                        if valid_type(ty) && valid_scope(scope) =>
                    {
                        Some(match control {
                            // START and CHECK find no
                            // coverage; STOP and RESET
                            // have nothing to do.
                            0 | 3 => 0,
                            1 | 2 => 1,
                            _ => -1,
                        })
                    }
                    (Some(_), Some(_), Some(_)) => Some(-1),
                    _ => None,
                }
            }
            SystemFunction::CoverageGetMax | SystemFunction::CoverageGet => {
                match (constant(0), constant(1)) {
                    (Some(ty), Some(scope)) if valid_type(ty) && valid_scope(scope) => Some(0),
                    (Some(_), Some(_)) => Some(-1),
                    _ => None,
                }
            }
            SystemFunction::CoverageMerge => {
                // No database was ever saved.
                constant(0).map(|_| -1)
            }
            // Without coverage nothing is saved
            // (40.3.2.5).
            SystemFunction::CoverageSave => match constant(0) {
                Some(ty) if valid_type(ty) => Some(0),
                Some(_) => Some(-1),
                None => None,
            },
            _ => None,
        };
        // The scope and database name arguments are
        // not evaluated, so one that could do more
        // than name something is not dropped.
        if let Some(id) = args
            .iter()
            .skip(count)
            .filter_map(|arg| arg.value)
            .find(|id| {
                !matches!(
                    native.expression(ungroup(*id, native)).kind,
                    ExprKind::Literal(_) | ExprKind::Name(_) | ExprKind::Member { .. }
                )
            })
        {
            return Err(unsupported(format!(
                "argument `{}` of `{name}`, which is not a literal or a name",
                native.token_text(native.expression(id).tokens).trim()
            )));
        }
        if function == SystemFunction::GetCoverage {
            // The covergroups of a design are not
            // elaborated, so its coverage is known
            // only without any (19.11).
            if native.tokens.iter().any(|token| token.text == "covergroup") {
                return Err(unsupported(
                    "`$get_coverage` in a source that declares a covergroup",
                ));
            }
            values.push(Expr::Literal("100.0".into()));
            return Ok(None);
        }
        let Some(result) = result else {
            return Err(AnalyzerError::InvalidSystemTfCall {
                name,
                detail: "coverage selector arguments must be constant".into(),
            });
        };
        values.push(Expr::Literal(match result {
            -1 => "32'shffffffff".into(),
            0 => "32'sh0".into(),
            _ => "32'sh1".into(),
        }));
        return Ok(None);
    }
    if args.iter().filter_map(|arg| arg.value).any(|id| {
        matches!(
            native.expression(ungroup(id, native)).kind,
            ExprKind::Type(_)
        )
    }) {
        return Err(unsupported(format!("operand of `{name}`")));
    }
    if function.is_effectful() && args.iter().filter_map(|arg| arg.value).any(|id| {
    matches!(&native.expression(ungroup(id, native)).kind, ExprKind::Literal(text)
        if typecheck::integral_literal_type_metadata(text).is_none() && packed_string_literal(text).is_none())
}) {
    return Err(unsupported(format!("integral argument of `{name}`")));
}
    let actuals = args.iter().map(|arg| arg.value).collect::<Vec<_>>();
    for (position, id) in actuals.iter().enumerate() {
        if function.writes_argument(position)
            && let Some(id) = id
        {
            // Such as `$fread` into a memory.
            chandles::check_memory(*id, tree, dimensions, session)?;
            check_tagged_copyout(*id, None, tree, session, dimensions)?;
        }
    }
    Ok(Some((
        Finish::Call {
            name,
            args: actuals.clone(),
            order: Vec::new(),
            system: Some(function),
        },
        actuals.into_iter().flatten().map(|id| (id, None)).collect(),
    )))
}

/// Enter a call of subroutine `name`.
fn enter_subroutine_call(
    name: String,
    args: &[celox_sv_syntax::Argument],
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Operands> {
    let (actuals, order) = arguments(&name, args, dimensions)?;
    for (position, id) in actuals.iter().enumerate() {
        if let Some(id) = id {
            references::check_actual(&name, position, *id, native, tree, session, dimensions)?;
            check_argument(
                &name,
                position,
                position + 1,
                *id,
                native,
                tree,
                dimensions,
                session,
            )?;
            check_written_container(&name, position, *id, tree, session, dimensions)?;
        }
        if let Some(id) = id
            && let Some(shape) = dimensions
                .subroutine_param_shapes
                .get(&name)
                .and_then(|shapes| shapes.get(position))
        {
            if dimensions
                .subroutine_directions
                .get(&name)
                .and_then(|directions| directions.get(position))
                .is_some_and(|direction| direction.is_written())
            {
                check_tagged_copyout(*id, Some(shape), tree, session, dimensions)?;
                // The formal is copied out to the actual.
                if let Some(formal) = shape
                    .type_id(&mut session.types, &tree.functions, &dimensions.const_env)
                    .and_then(|ty| virtual_interfaces::of_type(&session.types, ty))
                {
                    virtual_interfaces::check_copy_out(
                        tree, session, *id, native, dimensions, &formal,
                    )?;
                }
            }
        }
    }
    let children = actuals
        .iter()
        .enumerate()
        .filter_map(|(position, id)| {
            id.map(|id| {
                let output = dimensions
                    .subroutine_directions
                    .get(&name)
                    .and_then(|directions| directions.get(position))
                    .is_some_and(|direction| matches!(direction, ParamDirection::Output));
                (
                    id,
                    dimensions
                        .subroutine_param_shapes
                        .get(&name)
                        .and_then(|shapes| shapes.get(position))
                        .filter(|shape| {
                            !containers::holds_container(shape, tree, session, dimensions)
                                || copied_in(dimensions, &name, position)
                        })
                        .cloned()
                        // An output actual is not a value
                        // assigned to the formal.
                        .map(|mut shape| {
                            if output {
                                shape.virtual_interface = None;
                            }
                            shape
                        })
                        .map(Rc::new),
                )
            })
        })
        .collect();
    Ok((
        Finish::Call {
            name,
            args: actuals,
            order,
            system: None,
        },
        children,
    ))
}

/// The call of a class method, of its bound receiver and actuals.
pub(super) fn finish_method_call(call: Box<MethodCall>, operands: Vec<Expr>) -> Converted<Expr> {
    let MethodCall {
        name,
        given,
        order,
        formals,
        receiver,
        selection,
    } = *call;
    let mut args: Vec<Option<Expr>> = Vec::with_capacity(formals + 1);
    let mut operands = operands.into_iter();
    if receiver != objects::Receiver::Static {
        args.push(Some(match receiver {
            objects::Receiver::This => objects::this(),
            _ => operands.next().ok_or_else(disagreement)?,
        }));
    }
    let handles = args.len();
    for given in given {
        args.push(if given {
            Some(operands.next().ok_or_else(disagreement)?)
        } else {
            None
        });
    }
    args.extend(selection.map(Some));
    // Omitted arguments take their defaults (13.5.3).
    args.resize(formals + handles, None);
    // The receiver is evaluated before the actuals.
    let arg_order = if order.iter().enumerate().all(|(at, index)| at == *index) {
        Vec::new()
    } else {
        (0..handles)
            .chain(order.into_iter().map(|index| index + handles))
            .collect()
    };
    Ok(Expr::Call {
        callee: Callee::Subroutine(name),
        args,
        arg_order,
    })
}

/// The call of system function `function`, named `name`, of its bound
/// actuals, whose native expressions are `args`.
pub(super) fn finish_system_call(
    function: SystemFunction,
    name: String,
    args: Vec<Option<ExprId>>,
    operands: Vec<Expr>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let mut operands = operands;
    // Bind selected string outputs as targets: the read path
    // wraps handles and folds invalid indices to empty strings.
    let output = usize::from(function == SystemFunction::Ferror);
    if matches!(function, SystemFunction::Fgets | SystemFunction::Ferror)
        && let Some(Some(destination)) = args.get(output)
        && strings::fixed_array_element(tree, session, *destination, native, dimensions)
    {
        operands[output] =
            match lvalues::variable_lvalue(*destination, native, tree, session, dimensions, false)?
            {
                // An element of a fixed array is a variable.
                LValue::Object { .. } => {
                    return Err(unsupported(
                        "string output to a class property or container element",
                    ));
                }
                LValue::Ident(name) => Expr::Ident(name),
                LValue::Select {
                    name,
                    msb,
                    lsb,
                    signed,
                    ..
                } => Expr::Select {
                    expr: Box::new(Expr::Ident(name)),
                    msb,
                    lsb,
                    signed,
                },
            };
    }
    Ok(if function == SystemFunction::Fread {
        let mut operands = operands.into_iter();
        let mut values = Vec::new();
        for (position, id) in args.into_iter().enumerate() {
            let value = if id.is_some() {
                Some(operands.next().ok_or_else(disagreement)?)
            } else {
                None
            };
            if position < 2 && value.is_none() {
                return Err(unsupported("omitted $fread destination or descriptor"));
            }
            if let Some(value) = &value {
                if position == 0 {
                    if !expressions::query_actual_is_writable(value, dimensions) {
                        return Err(unsupported("writable $fread destination"));
                    }
                } else if expr_static_width(tree, value, dimensions).is_none() {
                    return Err(unsupported("integral $fread argument"));
                }
            }
            values.push(value);
        }
        Expr::Call {
            callee: Callee::System(function),
            args: values,
            arg_order: Vec::new(),
        }
    } else if matches!(
        function,
        SystemFunction::Urandom
            | SystemFunction::Random
            | SystemFunction::DistUniform
            | SystemFunction::DistNormal
            | SystemFunction::DistExponential
            | SystemFunction::DistPoisson
            | SystemFunction::DistChiSquare
            | SystemFunction::DistT
            | SystemFunction::DistErlang
            | SystemFunction::TestPlusargs
            | SystemFunction::ValuePlusargs
            | SystemFunction::System
            | SystemFunction::Fopen
            | SystemFunction::Fgets
            | SystemFunction::Ferror
            | SystemFunction::Ftell
            | SystemFunction::Fseek
            | SystemFunction::Rewind
            | SystemFunction::Fgetc
            | SystemFunction::Ungetc
            | SystemFunction::Feof
            | SystemFunction::Sscanf
            | SystemFunction::Fscanf
            | SystemFunction::UrandomRange
            | SystemFunction::ScopeRandomize
    ) {
        let mut places = Vec::new();
        for (position, arg) in operands.iter().enumerate() {
            let real_output = matches!(function, SystemFunction::Sscanf | SystemFunction::Fscanf)
                && position >= 2
                && real_value(arg, dimensions);
            if (!real_output && expr_static_width(tree, arg, dimensions).is_none())
                || args
                    .get(position)
                    .copied()
                    .flatten()
                    .and_then(|id| {
                        array_compatibility::native_unpacked_shape(id, tree, session, dimensions)
                    })
                    .is_some()
            {
                return Err(unsupported(format!("integral argument of `{name}`")));
            }
            // An output of a scan may be a class property or
            // a container element, a place that the scan
            // writes as an assignment does (21.3.4.3).
            // The scan writes it through the place, with the
            // place's checks (see `place_output`).
            let place = match args.get(position).copied().flatten() {
                Some(id)
                    if matches!(function, SystemFunction::Sscanf | SystemFunction::Fscanf)
                        && position >= 2 =>
                {
                    object_place(id, native, tree, session, dimensions)?
                }
                _ => None,
            };
            if let Some(place) = &place {
                places.push((position, place_output(place, tree, dimensions)?));
            }
            let place = place.is_some();
            // A member, or a select of one, such as `v.low[3:0]`.
            let member = args.get(position).copied().flatten().is_some_and(|id| {
                let mut id = ungroup(id, native);
                while let celox_sv_syntax::ExprKind::Select { expression, .. } =
                    native.expression(id).kind
                {
                    id = ungroup(expression, native);
                }
                matches!(
                    native.expression(id).kind,
                    celox_sv_syntax::ExprKind::Member { .. }
                )
            });
            if function.writes_argument(position)
                && !place
                && !(if member {
                    expressions::member_actual_is_writable(arg, dimensions)
                } else {
                    expressions::query_actual_is_writable(arg, dimensions)
                })
            {
                return Err(AnalyzerError::InvalidSystemTfCall {
                    name: name.clone(),
                    detail: if function == SystemFunction::Ferror {
                        "output argument must be a writable text variable".into()
                    } else if function == SystemFunction::Fgets {
                        "output argument must be a writable string variable".into()
                    } else if matches!(function, SystemFunction::Sscanf | SystemFunction::Fscanf) {
                        "output argument must be a writable integral, real or shortreal variable"
                            .into()
                    } else {
                        "output/inout argument must be an integral variable".into()
                    },
                });
            }
        }
        let mut operands = operands;
        for (position, output) in places {
            operands[position] = output;
        }
        Expr::Call {
            callee: Callee::System(function),
            args: operands.into_iter().map(Some).collect(),
            arg_order: Vec::new(),
        }
    } else {
        let mut operands = operands;
        // An omitted number of ticks of `$past` is
        // one (IEEE 1800-2023 16.9.3); the gate stays
        // the third argument.
        if function == SystemFunction::Past && args.get(1).is_some_and(Option::is_none) {
            operands.insert(1, Expr::Literal("1".into()));
        }
        expressions::numeric_system_function(tree, session, function, operands, dimensions)?
    })
}

/// The call of subroutine `name` of its bound actuals, whose native
/// expressions are `args`, evaluated in `order`.
pub(super) fn finish_subroutine_call(
    name: String,
    args: Vec<Option<ExprId>>,
    order: Vec<usize>,
    operands: Vec<Expr>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let mut operands = operands.into_iter();
    let mut values = Vec::new();
    for (position, id) in args.into_iter().enumerate() {
        let mut value = if id.is_some() {
            Some(operands.next().ok_or_else(disagreement)?)
        } else {
            None
        };
        // An output or inout actual is the target of the
        // assignment the return makes (IEEE 1800-2023
        // 13.5): a class property, a select or member of
        // one, or a container element is bound as the
        // place an assignment to it writes (see
        // `place_output`).
        if let Some(id) = id
            && dimensions
                .subroutine_directions
                .get(&name)
                .and_then(|directions| directions.get(position))
                .is_some_and(|direction| {
                    matches!(direction, ParamDirection::Output | ParamDirection::Inout)
                })
            && let Some(place) = object_place(id, native, tree, session, dimensions)?
        {
            value = Some(place_output(&place, tree, dimensions)?);
        }
        if let Some(value) = &value {
            expressions::validate_call_argument(&name, position, value, dimensions)?;
        }
        values.push(value);
    }
    Ok(Expr::Call {
        callee: Callee::Subroutine(name),
        args: values,
        arg_order: order,
    })
}
