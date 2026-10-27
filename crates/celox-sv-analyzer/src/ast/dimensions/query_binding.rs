//! Size and array queries consume native operands and semantic shape metadata.
use super::*;
use celox_sv_syntax::{DimensionKind, ExprId, ExprKind};

fn integer(value: i128) -> Expr {
    Expr::Literal(value.to_string())
}

fn query_context(context: &PackedDimensions) -> PackedDimensions {
    let mut query = context.clone();
    if !context.scope_types_complete {
        extend_variable_dimensions_from_const_env(&mut query, &context.const_env);
    }
    let signedness = context
        .iter()
        .map(|(name, shape)| (name.clone(), shape.signed))
        .collect::<Vec<_>>();
    query.expression_signedness.extend(signedness);
    query.functions = Arc::default();
    query.type_query = true;
    query
}

/// These operands carry all type information in their literal leaves. Walking
/// declaration scopes cannot change their shape or signedness. Keep calls,
/// casts and names on the discovery path, including names in unevaluated arms.
fn literal_operand(root: ExprId, native: &celox_sv_syntax::Declarations) -> bool {
    let root = expression_binding::ungroup(root, native);
    if matches!(native.expression(root).kind, ExprKind::Literal(_)) {
        return true;
    }
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        match &native.expression(id).kind {
            ExprKind::Literal(_) => {}
            ExprKind::Group(inner)
            | ExprKind::Unary {
                expression: inner, ..
            } => {
                pending.push(*inner);
            }
            ExprKind::Binary { left, right, .. } => pending.extend([*left, *right]),
            ExprKind::Conditional {
                condition,
                then_expression,
                else_expression,
            } => {
                pending.extend([*condition, *then_expression, *else_expression]);
            }
            _ => return false,
        }
    }
    true
}

pub(in crate::ast) fn query_context_for_operand(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<PackedDimensions> {
    // Aggregate port defaults commonly repeat literal arithmetic. Rediscovering
    // all declarations for each item repeatedly snapshots the growing prefix.
    if context.scope_types_complete || literal_operand(root, tree.declarations()) {
        return Ok(query_context(context));
    }
    Ok(query_context(&query_operand_dimensions_id(
        root, tree, context,
    )?))
}

pub(in crate::ast) fn validate_data_operand(
    root: ExprId,
    function: SystemFunction,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<()> {
    reject_unbounded(root, function, tree, context)
}

pub(in crate::ast) fn query_value(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Option<Expr>> {
    let native = tree.declarations();
    let ExprKind::Call {
        expression,
        arguments,
    } = &native.expression(root).kind
    else {
        return Ok(None);
    };
    let ExprKind::Name(name) = &native.expression(*expression).kind else {
        return Ok(None);
    };
    let Some(
        function @ (SystemFunction::Bits
        | SystemFunction::Size
        | SystemFunction::Dimensions
        | SystemFunction::UnpackedDimensions
        | SystemFunction::ArrayQuery(_)),
    ) = SystemFunction::from_name(&name.spelling)
    else {
        return Ok(None);
    };
    let maximum = if matches!(
        function,
        SystemFunction::Size | SystemFunction::ArrayQuery(_)
    ) {
        2
    } else {
        1
    };
    if arguments.is_empty()
        || arguments.len() > maximum
        || arguments
            .iter()
            .any(|a| a.name.is_some() || a.value.is_none())
    {
        return Err(unsupported(format!("arguments of `{}`", name.spelling)));
    }
    let argument = arguments[0].value.unwrap();
    let context = query_context_for_operand(argument, tree, context)?;
    reject_unbounded(argument, function, tree, &context)?;
    if function == SystemFunction::Size
        && let Some(value) = interface_array_size(
            root,
            argument,
            arguments.get(1).and_then(|a| a.value),
            tree,
            &context,
        )?
    {
        return Ok(Some(value));
    }
    let shape = shape(argument, tree, &context)?;
    let dimension = if let Some(argument) = arguments.get(1) {
        Some(expression_binding::bind(
            argument.value.unwrap(),
            tree,
            &context,
        )?)
    } else {
        None
    };
    let value = match function {
        SystemFunction::Bits => {
            let width = if let Some(shape) = &shape {
                shape
                    .unpacked
                    .iter()
                    .map(|d| &d.width)
                    .chain(shape.packed.iter().map(|d| &d.width))
                    .try_fold(1usize, |value, width| {
                        value.checked_mul(
                            usize::try_from(eval_ast_const_expr(
                                &tree.state,
                                width,
                                &context.const_env,
                            )?)
                            .ok()?,
                        )
                    })
            } else {
                type_binding::expression_type(argument, tree, &context)?.map(|ty| ty.width)
            };
            width.map(|width| integer(width as i128))
        }
        SystemFunction::Dimensions | SystemFunction::UnpackedDimensions => {
            let declared_return = if function == SystemFunction::Dimensions {
                function_dimension_count(argument, tree, &context)
            } else {
                None
            };
            let declared_parameter = parameter_dimension_count(argument, tree, &context, function);
            let count = if let Some(count) = declared_return.or(declared_parameter) {
                Some(count)
            } else if let Some(shape) = &shape {
                Some(if function == SystemFunction::Dimensions {
                    shape.unpacked.len() + if shape.real { 0 } else { shape.packed.len() }
                } else {
                    shape.unpacked.len()
                })
            } else {
                parameter_dimension_count(argument, tree, &context, function)
                    .or_else(|| special_dimension_count(argument, tree, function))
            };
            count.map(|count| integer(count as i128))
        }
        SystemFunction::Size => {
            let index = dimension
                .map(|value| expr_to_const(&tree.state, value))
                .unwrap_or_else(|| Some(ConstExpr::Literal("1".into())))
                .and_then(|value| eval_ast_const_expr(&tree.state, &value, &context.const_env));
            let width = if let (Some(shape), Some(index)) = (&shape, index) {
                let index = usize::try_from(index).ok().and_then(|i| i.checked_sub(1));
                if index == Some(0)
                    && let Some(width) = function_first_width(argument, tree, &context)
                {
                    Some(width as i128)
                } else {
                    index
                        .and_then(|index| {
                            shape
                                .unpacked
                                .iter()
                                .map(|d| &d.width)
                                .chain(shape.packed.iter().map(|d| &d.width))
                                .nth(index)
                        })
                        .and_then(|width| {
                            eval_ast_const_expr(&tree.state, width, &context.const_env)
                        })
                }
                .or_else(|| {
                    (shape.unpacked.is_empty() && shape.packed.is_empty() && index == Some(0))
                        .then_some(1)
                })
            } else {
                None
            };
            width
                .map(integer)
                .or_else(|| shape.as_ref().map(|_| Expr::Literal("32'bx".into())))
        }
        SystemFunction::ArrayQuery(query) => {
            let bounds = shape.as_ref().and_then(|shape| {
                if part_selected(argument, tree) {
                    let width = shape
                        .unpacked
                        .iter()
                        .map(|d| &d.width)
                        .chain(shape.packed.iter().map(|d| &d.width))
                        .next()
                        .and_then(|width| {
                            eval_ast_const_expr(&tree.state, width, &context.const_env)
                        })?;
                    return Some(vec![(width - 1, 0)]);
                }
                shape
                    .unpacked
                    .iter()
                    .map(|d| (&d.left, &d.right))
                    .chain(shape.packed.iter().map(|d| (&d.left, &d.right)))
                    .map(|(l, r)| {
                        Some((
                            eval_ast_const_expr(&tree.state, l, &context.const_env)?,
                            eval_ast_const_expr(&tree.state, r, &context.const_env)?,
                        ))
                    })
                    .collect::<Option<Vec<_>>>()
            });
            bounds.and_then(|bounds| {
                super::queries::bound_query(&tree.state, query, &bounds, dimension, &context)
            })
        }
        _ => unreachable!(),
    };
    value
        .map(Some)
        .ok_or_else(|| unsupported(format!("operand of `{}`", name.spelling)))
}

/// Query interface array metadata without admitting interface handles as data
/// expressions. This extension supports `$size` only; it adds no packed rank.
fn interface_array_size(
    call: ExprId,
    mut argument: ExprId,
    dimension: Option<ExprId>,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Option<Expr>> {
    let native = tree.declarations();
    let mut selected = Vec::new();
    loop {
        argument = expression_binding::ungroup(argument, native);
        match &native.expression(argument).kind {
            ExprKind::Select {
                expression,
                dimension,
            } => {
                let DimensionKind::Index(index) = dimension.kind else {
                    return Ok(None);
                };
                selected.push(index);
                argument = *expression;
            }
            _ => break,
        }
    }
    let Ok(handle) = interface_binding::actual_handle_key(
        argument,
        tree,
        &context.const_env,
        &context.type_aliases,
    ) else {
        return Ok(None);
    };
    let receiver = handle.split(['.', '[']).next().unwrap_or(&handle);
    if context.get(receiver).is_some()
        || context.const_env.contains_key(receiver)
        || context.parameter_values.contains_key(receiver)
    {
        return Ok(None);
    }
    let imported = scope::imported(&tree.state);
    let Some(array) = imported.interface_arrays.get(&handle) else {
        return Ok(None);
    };
    let Some(shape) = array
        .shape
        .get(selected.len()..)
        .filter(|shape| !shape.is_empty())
    else {
        return Ok(None);
    };
    for index in selected {
        validate_index(index, tree, context)?;
    }
    let dimension = match dimension {
        Some(id) => constant_binding::bind(id, tree, context)?
            .and_then(|value| eval_ast_const_expr(&tree.state, &value, &context.const_env))
            .ok_or_else(|| unsupported("nonconstant interface array query dimension"))?,
        None => 1,
    };
    let value = usize::try_from(dimension)
        .ok()
        .and_then(|d| d.checked_sub(1))
        .and_then(|index| shape.get(index))
        .map(|size| integer(*size as i128))
        .unwrap_or_else(|| Expr::Literal("32'sbx".into()));
    warnings::append(
        &mut tree.state.diagnostics.borrow_mut(),
        [warnings::AnalyzerWarning::InterfaceArrayQuery {
            site: procedural::source_span(native.expression(call).tokens, tree),
        }],
    );
    Ok(Some(value))
}

fn parameter_dimension_count(
    mut root: ExprId,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
    function: SystemFunction,
) -> Option<usize> {
    let native = tree.declarations();
    while let ExprKind::Group(inner) = native.expression(root).kind {
        root = inner;
    }
    let (name, dimensions) = type_binding::argument_alias(root, native)?;
    if !dimensions.is_empty() {
        return None;
    }
    let rank = usize::try_from(*context.const_env.get(&parameter_rank_marker(&name))?).ok()?;
    Some(if function == SystemFunction::Dimensions {
        rank
    } else {
        0
    })
}

fn function_first_width(
    mut root: ExprId,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Option<usize> {
    let native = tree.declarations();
    while let ExprKind::Group(inner) = native.expression(root).kind {
        root = inner;
    }
    let callee = match native.expression(root).kind {
        ExprKind::Call { expression, .. } => expression,
        _ => root,
    };
    let (name, dimensions) = type_binding::argument_alias(callee, native)?;
    if !dimensions.is_empty()
        || context.get(&name).is_some()
        || context.const_env.contains_key(&name)
    {
        return None;
    }
    context
        .function_return_types
        .get(&name)?
        .first_packed_dimension_width
}

fn reject_unbounded(
    root: ExprId,
    function: SystemFunction,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<()> {
    let native = tree.declarations();
    if matches!(native.expression(root).kind, ExprKind::Type(_)) {
        return Ok(());
    }
    let mut jobs = vec![root];
    let mut seen = HashSet::default();
    while let Some(id) = jobs.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some((name, dimensions)) = type_binding::argument_alias(id, native)
            && dimensions.is_empty()
            && (name == "$"
                || context
                    .const_env
                    .get(&parameters::unbounded_parameter_marker(&name))
                    == Some(&1))
        {
            return Err(AnalyzerError::InvalidSystemTfCall {
                name: function.name().into(),
                detail: "unbounded parameter is not valid in this data query".into(),
            });
        }
        match &native.expression(id).kind {
            ExprKind::Literal(text) if text == "$" => {
                return Err(AnalyzerError::InvalidSystemTfCall {
                    name: function.name().into(),
                    detail: "unbounded parameter is not valid in this data query".into(),
                });
            }
            ExprKind::Call {
                expression,
                arguments,
            }
            | ExprKind::Parameterized {
                expression,
                arguments,
            } => {
                if matches!(&native.expression(*expression).kind,ExprKind::Name(name) if name.spelling=="$isunbounded")
                {
                    continue;
                }
                jobs.extend(arguments.iter().filter_map(|a| a.value));
            }
            ExprKind::Group(inner)
            | ExprKind::Unary {
                expression: inner, ..
            }
            | ExprKind::Member {
                expression: inner, ..
            } => jobs.push(*inner),
            ExprKind::Binary { left, right, .. }
            | ExprKind::Assignment { left, right, .. }
            | ExprKind::Cast {
                target: left,
                expression: right,
            } => jobs.extend([*left, *right]),
            ExprKind::Conditional {
                condition,
                then_expression,
                else_expression,
            } => jobs.extend([*condition, *then_expression, *else_expression]),
            ExprKind::Concatenation(parts) => jobs.extend(parts),
            ExprKind::Replication { count, expressions } => {
                jobs.push(*count);
                jobs.extend(expressions);
            }
            ExprKind::Select {
                expression,
                dimension,
            }
            | ExprKind::StreamSelect {
                expression,
                dimension,
            } => {
                jobs.push(*expression);
                dimension_jobs(&dimension.kind, &mut jobs);
            }
            ExprKind::Pattern { target, items } => {
                jobs.extend(target);
                for item in items {
                    jobs.extend(item.key);
                    jobs.push(item.value);
                }
            }
            ExprKind::Set(parts) => {
                for part in parts {
                    dimension_jobs(&part.kind, &mut jobs);
                }
            }
            ExprKind::Streaming {
                slice, expressions, ..
            } => {
                jobs.extend(slice);
                jobs.extend(expressions);
            }
            ExprKind::MinTypMax {
                minimum,
                typical,
                maximum,
            } => jobs.extend([*minimum, *typical, *maximum]),
            ExprKind::Tagged { value, .. } => jobs.extend(value),
            _ => {}
        }
    }
    Ok(())
}
fn dimension_jobs(dimension: &DimensionKind, jobs: &mut Vec<ExprId>) {
    match dimension {
        DimensionKind::Index(id) => jobs.push(*id),
        DimensionKind::Range { left, right } => jobs.extend([*left, *right]),
        DimensionKind::Indexed { base, width, .. } => jobs.extend([*base, *width]),
        DimensionKind::Queue(value) => jobs.extend(value),
        _ => {}
    }
}

/// Unevaluated array queries still require every selection operand to be
/// well typed (20.7). A comparison's one-bit result cannot hide an unknown name.
fn validate_index(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<()> {
    let native = tree.declarations();
    let value = expression_binding::bind(root, tree, context)?;
    if expr_static_width(&tree.state, &value, context).is_none() {
        return Err(unsupported("query index type"));
    }
    let mut jobs = vec![root];
    while let Some(id) = jobs.pop() {
        match &native.expression(id).kind {
            ExprKind::Name(_) | ExprKind::Literal(_) => {
                if type_binding::expression_type(id, tree, context)?.is_none() {
                    return Err(unsupported("query index type"));
                }
            }
            ExprKind::Member { .. } | ExprKind::Select { .. } => {
                if type_binding::expression_type(id, tree, context)?.is_none() {
                    return Err(unsupported("query index type"));
                }
                let mut base = id;
                loop {
                    match &native.expression(base).kind {
                        ExprKind::Member { expression, .. } => base = *expression,
                        ExprKind::Select {
                            expression,
                            dimension,
                        } => {
                            dimension_jobs(&dimension.kind, &mut jobs);
                            base = *expression;
                        }
                        _ => break,
                    }
                }
            }
            ExprKind::Group(inner)
            | ExprKind::Unary {
                expression: inner, ..
            } => jobs.push(*inner),
            ExprKind::Binary { left, right, .. } | ExprKind::Assignment { left, right, .. } => {
                jobs.extend([*left, *right])
            }
            ExprKind::Conditional {
                condition,
                then_expression,
                else_expression,
            } => jobs.extend([*condition, *then_expression, *else_expression]),
            ExprKind::Cast { expression, .. } => jobs.push(*expression),
            ExprKind::Call {
                expression,
                arguments,
            } => {
                if expression_binding::native_name(&tree.state, *expression, native)
                    .as_deref()
                    .and_then(SystemFunction::from_name)
                    .is_some_and(SystemFunction::is_data_query)
                {
                    continue;
                }
                jobs.extend(arguments.iter().filter_map(|argument| argument.value));
            }
            ExprKind::Concatenation(parts) => jobs.extend(parts),
            ExprKind::Replication { count, expressions } => {
                jobs.push(*count);
                jobs.extend(expressions);
            }
            _ => {}
        }
    }
    Ok(())
}

fn part_selected(mut root: ExprId, tree: &AnalysisContext<'_>) -> bool {
    loop {
        match &tree.declarations().expression(root).kind {
            ExprKind::Group(inner) => root = *inner,
            ExprKind::Select { dimension, .. } => {
                return matches!(
                    dimension.kind,
                    DimensionKind::Range { .. } | DimensionKind::Indexed { .. }
                );
            }
            _ => return false,
        }
    }
}

fn function_dimension_count(
    mut root: ExprId,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Option<usize> {
    let native = tree.declarations();
    loop {
        match &native.expression(root).kind {
            ExprKind::Group(inner) => root = *inner,
            ExprKind::Unary {
                operator,
                expression,
                ..
            } if matches!(operator.as_str(), "+" | "-" | "~") => root = *expression,
            ExprKind::Binary { operator, left, .. }
                if matches!(operator.as_str(), "<<" | ">>" | "<<<" | ">>>") =>
            {
                root = *left
            }
            _ => break,
        }
    }
    let callee = match native.expression(root).kind {
        ExprKind::Call { expression, .. } => expression,
        _ => root,
    };
    let (name, dimensions) = type_binding::argument_alias(callee, native)?;
    if !dimensions.is_empty()
        || context.get(&name).is_some()
        || context.const_env.contains_key(&name)
    {
        return None;
    }
    context.function_return_types.get(&name)?.dimensions
}

fn special_dimension_count(
    mut root: ExprId,
    tree: &AnalysisContext<'_>,
    function: SystemFunction,
) -> Option<usize> {
    loop {
        root = match &tree.declarations().expression(root).kind {
            ExprKind::Group(inner) => *inner,
            ExprKind::Unary {
                operator,
                expression,
                ..
            } if matches!(operator.as_str(), "+" | "-" | "~") => *expression,
            ExprKind::Binary { operator, left, .. }
                if matches!(operator.as_str(), "<<" | ">>" | "<<<" | ">>>") =>
            {
                *left
            }
            _ => break,
        };
    }
    match &tree.declarations().expression(root).kind {
        ExprKind::Type(id) => match &tree.declarations().data_type(*id).kind {
            celox_sv_syntax::DataTypeKind::Builtin(name) if name == "string" => {
                Some(usize::from(function == SystemFunction::Dimensions))
            }
            celox_sv_syntax::DataTypeKind::Builtin(name)
                if matches!(
                    name.as_str(),
                    "real" | "shortreal" | "realtime" | "chandle" | "event"
                ) =>
            {
                Some(0)
            }
            _ => None,
        },
        ExprKind::Literal(text) if text.starts_with('"') => {
            Some(usize::from(function == SystemFunction::Dimensions))
        }
        ExprKind::Literal(text)
            if !text.contains('\'')
                && text.replace('_', "").parse::<f64>().is_ok()
                && text.contains(['.', 'e', 'E']) =>
        {
            Some(0)
        }
        _ => None,
    }
}

fn scalar(width: usize, signed: bool, packed: bool) -> VariableDimensions {
    let mut ty = Type::new(TypeKind::Logic);
    ty.is_signed = signed;
    if packed {
        ty.packed_ranges.push(PackedRange::new(
            ConstExpr::Literal(width.saturating_sub(1).to_string()),
            ConstExpr::Literal("0".into()),
        ));
    }
    procedural::dimensions_from_type(&ty)
}

pub(in crate::ast) fn shape(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Option<VariableDimensions>> {
    enum Job {
        Enter(ExprId),
        Index(ExprId),
        Range(ExprId, ExprId, bool),
        Member(String),
    }
    let native = tree.declarations();
    let mut jobs = vec![Job::Enter(root)];
    let mut values = Vec::<VariableDimensions>::new();
    while let Some(job) = jobs.pop() {
        match job {
            Job::Enter(id) => {
                if let Some((_, shape)) = array_reductions::bind(id, native, tree, context)? {
                    values.push(shape);
                    continue;
                }
                let callee = match native.expression(id).kind {
                    ExprKind::Call { expression, .. } => expression,
                    _ => id,
                };
                if let ExprKind::Member {
                    expression, name, ..
                } = &native.expression(callee).kind
                    && let Some(container) = containers::native_type(*expression, native, context)
                {
                    // Validate the call without executing it. Pop returns the
                    // declared element type, including packed dimensions and
                    // authored bounds, rather than a flattened bit vector.
                    let value = expression_binding::bind(id, tree, context)?;
                    let shape = if matches!(name.spelling.as_str(), "pop_front" | "pop_back") {
                        procedural::dimensions_from_type(&container.element)
                    } else {
                        let ty = bound_bits_type(&tree.state, &value, context)
                            .ok_or_else(|| unsupported("container method result type"))?;
                        scalar(ty.width, ty.signed, ty.width > 1)
                    };
                    values.push(shape);
                    continue;
                }
                if let Some((name, dimensions)) = type_binding::argument_alias(id, native)
                    && dimensions.is_empty()
                {
                    let imported = scope::imported(&tree.state);
                    let receiver = name.split('.').next().unwrap_or(&name);
                    let local_receiver = context.get(receiver).is_some()
                        || context.const_env.contains_key(receiver)
                        || context.parameter_values.contains_key(receiver);
                    let binding = if local_receiver {
                        &name
                    } else {
                        imported.interface_members.get(&name).unwrap_or(&name)
                    };
                    if let Some(shape) = context.get(binding) {
                        values.push(shape.clone());
                        continue;
                    }
                    if !context.const_env.contains_key(&name)
                        && !context.parameter_values.contains_key(&name)
                        && let Some(ty) = context.type_aliases.get(&name)
                    {
                        values.push(procedural::dimensions_from_type(ty));
                        continue;
                    }
                    if let Some(ty) = parameter_type_from_const_env(&context.const_env, &name) {
                        let count = context
                            .const_env
                            .get(&parameter_dimensions_marker(&name))
                            .and_then(|n| usize::try_from(*n).ok());
                        let rank = context
                            .const_env
                            .get(&parameter_rank_marker(&name))
                            .copied();
                        let mut shape = scalar(
                            ty.width,
                            ty.signed,
                            ty.width > 1 || rank.is_some_and(|rank| rank > 0),
                        );
                        if let Some(count) = count {
                            let ranges = (0..count)
                                .map(|index| {
                                    Some(PackedRange::new(
                                        ConstExpr::Literal(
                                            context
                                                .const_env
                                                .get(&parameter_dimension_marker(
                                                    &name, index, "left",
                                                ))?
                                                .to_string(),
                                        ),
                                        ConstExpr::Literal(
                                            context
                                                .const_env
                                                .get(&parameter_dimension_marker(
                                                    &name, index, "right",
                                                ))?
                                                .to_string(),
                                        ),
                                    ))
                                })
                                .collect::<Option<Vec<_>>>();
                            if let Some(ranges) = ranges
                                && (count > 0 || rank == Some(0))
                            {
                                shape.packed = signal_packed_dimension_widths(&ranges);
                            }
                        }
                        values.push(shape);
                        continue;
                    }
                }
                match &native.expression(id).kind {
                    ExprKind::Group(inner) => jobs.push(Job::Enter(*inner)),
                    ExprKind::Type(ty) => {
                        let Some(ty) = type_binding::bind(*ty, tree, context)? else {
                            return Ok(None);
                        };
                        values.push(procedural::dimensions_from_type(&ty));
                    }
                    ExprKind::Select {
                        expression,
                        dimension,
                    } => {
                        match dimension.kind {
                            DimensionKind::Index(index) => jobs.push(Job::Index(index)),
                            DimensionKind::Range { left, right } => {
                                jobs.push(Job::Range(left, right, false))
                            }
                            DimensionKind::Indexed { base, width, .. } => {
                                jobs.push(Job::Range(base, width, true))
                            }
                            _ => return Err(unsupported("query selection")),
                        }
                        jobs.push(Job::Enter(*expression));
                    }
                    ExprKind::Member {
                        expression,
                        separator,
                        name,
                    } if separator == "." => {
                        jobs.push(Job::Member(instances::normalize_identifier(&name.spelling)));
                        jobs.push(Job::Enter(*expression));
                    }
                    ExprKind::Unary {
                        operator,
                        expression,
                        ..
                    } if matches!(operator.as_str(), "+" | "-" | "~") => {
                        jobs.push(Job::Enter(*expression))
                    }
                    ExprKind::Binary {
                        operator,
                        left,
                        right,
                    } if matches!(operator.as_str(), "<<" | ">>" | "<<<" | ">>>") => {
                        expression_binding::bind(*right, tree, context)?;
                        jobs.push(Job::Enter(*left));
                    }
                    ExprKind::Cast { target, .. } => {
                        expression_binding::bind(id, tree, context)?;
                        let ty = match native.expression(*target).kind {
                            ExprKind::Type(ty) => type_binding::bind(ty, tree, context)?,
                            _ => type_binding::argument_alias(*target, native)
                                .filter(|(_, dimensions)| dimensions.is_empty())
                                .and_then(|(name, _)| context.type_aliases.get(&name).cloned()),
                        };
                        if let Some(ty) = ty {
                            values.push(procedural::dimensions_from_type(&ty));
                        } else {
                            let value = expression_binding::bind(id, tree, context)?;
                            let Some(ty) = bound_bits_type(&tree.state, &value, context) else {
                                return Ok(None);
                            };
                            values.push(scalar(ty.width, ty.signed, true));
                        }
                    }
                    _ => {
                        if special_dimension_count(id, tree, SystemFunction::Dimensions).is_some() {
                            return Ok(None);
                        }
                        let value = expression_binding::bind(id, tree, context)?;
                        let Some(ty) = bound_bits_type(&tree.state, &value, context) else {
                            return Ok(None);
                        };
                        let mut shape = scalar(
                            ty.width,
                            ty.signed,
                            ty.width > 1
                                || matches!(
                                    value,
                                    Expr::Literal(_)
                                        | Expr::Concat(_)
                                        | Expr::RepeatConcat { .. }
                                        | Expr::Resize { .. }
                                ),
                        );
                        if let Expr::Call {
                            callee: Callee::Subroutine(name),
                            ..
                        } = &value
                            && let Some(metadata) = context.function_return_types.get(name)
                            && let Some(count) = metadata.dimensions
                        {
                            if count == 0 {
                                shape.packed.clear();
                            }
                        }
                        values.push(shape);
                    }
                }
            }
            Job::Index(index) => {
                validate_index(index, tree, context)?;
                let shape = values
                    .last_mut()
                    .ok_or_else(|| unsupported("query shape"))?;
                if !shape.unpacked.is_empty() {
                    shape.unpacked.remove(0);
                } else if !shape.packed.is_empty() {
                    shape.packed.remove(0);
                    if shape.packed.is_empty() {
                        shape.members.clear();
                    }
                } else {
                    return Err(unsupported("query selection of scalar"));
                }
            }
            Job::Range(left, right, indexed) => {
                let left = if indexed {
                    validate_index(left, tree, context)?;
                    None
                } else {
                    constant_binding::bind(left, tree, context)?
                };
                let right = constant_binding::bind(right, tree, context)?
                    .and_then(|v| eval_ast_const_expr(&tree.state, &v, &context.const_env));
                let width = if indexed {
                    right
                } else {
                    left.and_then(|v| eval_ast_const_expr(&tree.state, &v, &context.const_env))
                        .zip(right)
                        .and_then(|(l, r)| i128::try_from(l.abs_diff(r)).ok()?.checked_add(1))
                };
                let Some(width) = width
                    .and_then(|v| usize::try_from(v).ok())
                    .filter(|v| *v > 0)
                else {
                    return Err(unsupported("query part-select width"));
                };
                let shape = values
                    .last_mut()
                    .ok_or_else(|| unsupported("query shape"))?;
                if !shape.unpacked.is_empty() {
                    shape.unpacked[0] = UnpackedDimension {
                        left: ConstExpr::Literal((width - 1).to_string()),
                        right: ConstExpr::Literal("0".into()),
                        width: ConstExpr::Literal(width.to_string()),
                    };
                    continue;
                }
                if shape.packed.is_empty() {
                    return Err(unsupported("query part-select"));
                }
                // Preserve the remaining declared rank; the selected dimension
                // has its authored range width, with lower dimensions intact.
                shape.packed[0] = scalar(width, false, true).packed.remove(0);
                shape.signed = false;
                shape.members.clear();
            }
            Job::Member(name) => {
                let shape = values
                    .last_mut()
                    .ok_or_else(|| unsupported("query member shape"))?;
                if !shape.unpacked.is_empty() {
                    return Err(unsupported("query array member"));
                }
                let Some(member) = shape.members.iter().find(|member| member.name() == name) else {
                    return Ok(None);
                };
                *shape = procedural::dimensions_from_type(member.r#type());
            }
        }
    }
    Ok(values.pop())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_queries_preserve_declared_shape_without_running_operands() {
        let source = "module M; logic[1:0][3:0] a; logic[7:0] memory[2:5]; int counter;function automatic logic[1:0][3:0] f();counter++;return '0;endfunction assign y=$bits(a)+$size(a)+$dimensions(a)+$left(a)+$right(a)+$low(a)+$high(a)+$increment(a)+$bits(memory)+$size(memory)+$dimensions(memory)+$unpacked_dimensions(memory)+$bits(memory[2])+$size(memory[2])+$dimensions(memory[2])+$unpacked_dimensions(memory[2])+$dimensions(1'b1)+$dimensions(1.25)+$unpacked_dimensions(memory[2+:1])+$bits(a[1:0])+$dimensions(a[1:0])+$size(a[1:0])+$bits(f())+$size(f())+$dimensions(f())+$bits(type(logic[3:0]))+$bits(type(int'(0)));endmodule";
        let tree = crate::source_tree::parse_source(
            source,
            std::path::Path::new("native-query-shapes.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let mut context = PackedDimensions {
            scope_types_complete: true,
            ..PackedDimensions::default()
        };
        context.function_return_types.insert(
            "f".into(),
            FunctionReturnMetadata {
                arguments: Some(Arc::from([])),
                width: Some(8),
                first_packed_dimension_width: Some(2),
                dimensions: Some(2),
                signed: false,
                is_2state: false,
                string: false,
                pattern_type: None,
            },
        );
        for declaration in &tree.declarations().data_declarations {
            let celox_sv_syntax::DataDeclaration::Variables(declaration) = declaration else {
                continue;
            };
            let base = type_binding::bind(declaration.data_type, &tree, &context)
                .unwrap()
                .unwrap();
            for variable in &declaration.variables {
                let ty = types::type_with_unpacked_ranges(
                    base.clone(),
                    constant_binding::bind_unpacked_ranges(&variable.dimensions, &tree, &context)
                        .unwrap(),
                );
                context.insert(
                    instances::normalize_identifier(&variable.name.spelling),
                    procedural::dimensions_from_type(&ty),
                );
            }
        }
        let mut calls = tree
            .declarations()
            .expression_nodes()
            .filter_map(|(id, expression)| {
                let ExprKind::Call {
                    expression: callee, ..
                } = &expression.kind
                else {
                    return None;
                };
                let ExprKind::Name(name) = &tree.declarations().expression(*callee).kind else {
                    return None;
                };
                name.spelling
                    .starts_with('$')
                    .then_some((id, expression.tokens))
            })
            .collect::<Vec<_>>();
        calls.sort_by_key(|(_, range)| range.start);
        let actual = calls
            .into_iter()
            .map(|(root, _)| {
                let value = query_value(root, &tree, &context).unwrap().unwrap();
                expr_to_const(&tree.state, value)
                    .and_then(|value| eval_ast_const_expr(&tree.state, &value, &context.const_env))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            [
                8, 2, 2, 1, 0, 0, 1, 1, 32, 4, 2, 1, 8, 8, 1, 0, 1, 0, 1, 8, 2, 2, 8, 2, 2, 4, 32
            ]
        );
    }
}
