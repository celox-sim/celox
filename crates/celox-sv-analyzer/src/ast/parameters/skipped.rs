//! Validate unevaluated parameter operands by native ownership. Shape checks
//! visit authored items once, never materializing a replication or target array.
use super::*;
use celox_sv_syntax::{DimensionKind, ExprId, ExprKind};

fn integer(root: ExprId, tree: &AnalysisContext<'_>, dims: &PackedDimensions) -> Converted<i128> {
    constant_binding::bind(root, tree, dims)?
        .and_then(|value| eval_ast_const_expr(&tree.state, &value, &dims.const_env))
        .ok_or_else(|| unsupported("parameter shape expression that is not a known constant"))
}

fn bound(
    root: ExprId,
    unknown: bool,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<()> {
    let value = constant_binding::bind(root, tree, dims)?
        .ok_or_else(|| unsupported("parameter selection bound that is not constant"))?;
    if eval_ast_const_expr(&tree.state, &value, &dims.const_env).is_none()
        && !(unknown && selects::const_index_has_unknown_value(&value, dims))
    {
        return Err(unsupported(
            "parameter selection bound that is not constant",
        ));
    }
    Ok(())
}

fn dimension(
    kind: &DimensionKind,
    stream: bool,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<()> {
    match kind {
        DimensionKind::Index(index) => bound(*index, !stream, tree, dims),
        DimensionKind::Range { left, right } => {
            bound(*left, false, tree, dims)?;
            bound(*right, false, tree, dims)
        }
        DimensionKind::Indexed {
            base,
            width,
            operator,
        } => {
            bound(*base, !stream, tree, dims)?;
            let width = integer(*width, tree, dims)?;
            if width <= 0 {
                return Err(unsupported("indexed selection width must be positive"));
            }
            if stream {
                let start = integer(*base, tree, dims)?;
                let end = if operator == "+:" {
                    start.checked_add(width - 1)
                } else {
                    start.checked_sub(width - 1)
                };
                end.ok_or_else(|| unsupported("streaming with range bounds overflow"))?;
            }
            Ok(())
        }
        _ => Err(unsupported("parameter selection dimension")),
    }
}

fn value_name(
    state: &AnalysisState,
    name: &str,
    query: bool,
    dims: &PackedDimensions,
    unavailable: &HashSet<String>,
) -> Converted<()> {
    if unavailable.contains(name) {
        return Err(unsupported(format!(
            "parameter `{name}` is not in the declaration prefix"
        )));
    }
    let constant = dims.const_env.contains_key(name)
        || dims.parameter_values.contains_key(name)
        || dims
            .const_env
            .contains_key(&unbounded_parameter_marker(name));
    if constant
        || query
            && (dims.get(name).is_some()
                || dims.type_aliases.contains_key(name)
                || dims.const_env.contains_key(&variable_bits_marker(name)))
    {
        return Ok(());
    }
    // A zero-argument constant call may omit parentheses. A visible value
    // with the same spelling still shadows that callable, as in ordinary binding.
    if dims.get(name).is_none()
        && const_functions::is_constant_function(&state.const_functions, name)
    {
        let (actuals, _) = expression_binding::arguments(name, &[], dims)?;
        return const_functions::validate_argument_bindings(
            &state.const_functions,
            name,
            &actuals.iter().map(Option::is_some).collect::<Vec<_>>(),
        );
    }
    Err(unsupported(format!(
        "unresolved or nonconstant name `{name}` in short-circuited parameter operand"
    )))
}

/// `names` is enabled for a folded value; class-header selection validation
/// retains the original policy of checking shapes without requiring values.
pub(in crate::ast) fn validate(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
    unavailable: &HashSet<String>,
    names: bool,
) -> Converted<()> {
    let native = tree.declarations();
    let mut context = dimensions::query_context_for_operand(root, tree, dims)?;
    context.constant_indexed_base = true;
    let dims = &context;
    // Receiving shapes propagate only through groups, casts and function actuals.
    // Every other pattern requires its own explicit type.
    let mut jobs = vec![(root, false, None::<VariableDimensions>)];
    while let Some((id, query, receiving)) = jobs.pop() {
        let push = |jobs: &mut Vec<_>, id| jobs.push((id, query, None));
        match &native.expression(id).kind {
            ExprKind::Literal(_) | ExprKind::Type(_) => {}
            ExprKind::Name(name) => {
                let name = instances::normalize_identifier(&name.spelling);
                if names {
                    value_name(&tree.state, &name, query, dims, unavailable)?;
                }
            }
            ExprKind::Group(inner) => jobs.push((*inner, query, receiving)),
            ExprKind::Unary { expression, .. } => push(&mut jobs, *expression),
            ExprKind::Binary { left, right, .. } | ExprKind::Assignment { left, right, .. } => {
                push(&mut jobs, *right);
                push(&mut jobs, *left);
            }
            ExprKind::Conditional {
                condition,
                then_expression,
                else_expression,
            } => {
                push(&mut jobs, *else_expression);
                push(&mut jobs, *then_expression);
                push(&mut jobs, *condition);
            }
            ExprKind::Member {
                expression,
                separator,
                ..
            } => {
                if separator == "::"
                    && let Some(name) = expression_binding::native_name(&tree.state, id, native)
                {
                    if names {
                        value_name(&tree.state, &name, query, dims, unavailable)?;
                    }
                } else {
                    push(&mut jobs, *expression);
                }
            }
            ExprKind::Call {
                expression,
                arguments,
            } => {
                let name = expression_binding::callable_name(&tree.state, *expression, native)
                    .ok_or_else(|| unsupported("function name in parameter operand"))?;
                if let Some(function) = SystemFunction::from_name(&name) {
                    let args = arguments
                        .iter()
                        .map(|arg| {
                            if arg.value.is_some() {
                                system_functions::Arg::Given
                            } else {
                                system_functions::Arg::Omitted
                            }
                        })
                        .collect::<Vec<_>>();
                    let positional = arguments.iter().all(|arg| arg.name.is_none());
                    system_functions::check_call(
                        &name,
                        positional.then_some(args.as_slice()),
                        system_functions::CallSite::Expression,
                    )?;
                    for (position, argument) in arguments.iter().enumerate() {
                        if let Some(value) = argument.value {
                            jobs.push((
                                value,
                                if function.is_data_query() {
                                    position == 0
                                } else {
                                    query
                                },
                                None,
                            ));
                        }
                    }
                } else if name.starts_with('$') {
                    return Err(AnalyzerError::UnknownSystemTf { name });
                } else {
                    if unavailable.contains(&name) {
                        return Err(unsupported(format!(
                            "parameter `{name}` is not in the declaration prefix"
                        )));
                    }
                    let (actuals, _) = expression_binding::arguments(&name, arguments, dims)?;
                    const_functions::validate_argument_bindings(
                        &tree.state.const_functions,
                        &name,
                        &actuals.iter().map(Option::is_some).collect::<Vec<_>>(),
                    )?;
                    for (position, value) in actuals.into_iter().enumerate() {
                        if let Some(value) = value {
                            let shape = dims
                                .subroutine_param_shapes
                                .get(&name)
                                .and_then(|shapes| shapes.get(position))
                                .cloned();
                            jobs.push((value, query, shape));
                        }
                    }
                }
            }
            ExprKind::Select {
                expression,
                dimension: selection,
            } => {
                dimension(&selection.kind, false, tree, dims)?;
                // Named aggregate selectors need declared rank/member checks. A
                // concatenation's huge payload remains unevaluated.
                let mut base = *expression;
                loop {
                    base = expression_binding::ungroup(base, native);
                    match native.expression(base).kind {
                        ExprKind::Select { expression, .. }
                        | ExprKind::Member { expression, .. } => base = expression,
                        _ => break,
                    }
                }
                if matches!(native.expression(base).kind, ExprKind::Name(_)) {
                    dimensions::operand_shape(id, tree, dims)?
                        .ok_or_else(|| unsupported("parameter selection shape"))?;
                }
                push(&mut jobs, *expression);
                dimension_jobs(&selection.kind, &mut jobs, query);
            }
            ExprKind::StreamSelect {
                expression,
                dimension: selection,
            } => {
                dimension(&selection.kind, true, tree, dims)?;
                let shape = dimensions::operand_shape(*expression, tree, dims)?
                    .ok_or_else(|| unsupported("streaming with operand shape"))?;
                if shape.unpacked.len() != 1 {
                    return Err(unsupported(
                        "streaming with requires a one-dimensional unpacked array",
                    ));
                }
                push(&mut jobs, *expression);
                dimension_jobs(&selection.kind, &mut jobs, query);
            }
            ExprKind::Cast { target, expression } => {
                let cast = expression_binding::cast_target_native(
                    *target,
                    *expression,
                    native,
                    tree,
                    dims,
                )?;
                let shape = if let Some(name) =
                    expression_binding::native_name(&tree.state, *target, native)
                    && let Some(ty) = dims.type_aliases.get(&name)
                {
                    Some(procedural::dimensions_from_type(ty))
                } else {
                    let width = match cast {
                        casts::RuntimeCastTarget::Integral { target, .. }
                        | casts::RuntimeCastTarget::Zero(target) => Some(target.width),
                        casts::RuntimeCastTarget::Signing(_) | casts::RuntimeCastTarget::String => {
                            None
                        }
                        casts::RuntimeCastTarget::Real { short } => {
                            Some(if short { 32 } else { 64 })
                        }
                    };
                    width.map(|width| {
                        let mut ty = Type::new(TypeKind::Logic);
                        ty.packed_ranges.push(PackedRange::new(
                            ConstExpr::Literal((width - 1).to_string()),
                            ConstExpr::Literal("0".into()),
                        ));
                        procedural::dimensions_from_type(&ty)
                    })
                };
                jobs.push((*expression, query, shape));
            }
            ExprKind::Concatenation(parts) => {
                for part in parts {
                    push(&mut jobs, *part);
                }
            }
            ExprKind::Replication { count, expressions } => {
                if integer(*count, tree, dims)? < 0 {
                    return Err(unsupported(
                        "parameter repetition count must be a nonnegative known constant",
                    ));
                }
                jobs.push((*count, false, None));
                for part in expressions {
                    push(&mut jobs, *part);
                }
            }
            ExprKind::Streaming {
                slice, expressions, ..
            } => {
                if let Some(slice) = slice {
                    let valid = if let ExprKind::Type(ty) = native.expression(*slice).kind {
                        type_binding::bind(ty, tree, dims)?
                            .and_then(|ty| {
                                casts::expr_type_from_type(&tree.state, &ty, &dims.const_env)
                            })
                            .is_some_and(|ty| ty.width > 0)
                    } else {
                        type_binding::argument_alias(*slice, native)
                            .and_then(|(name, dimensions)| dimensions.is_empty().then_some(name))
                            .and_then(|name| dims.type_aliases.get(&name))
                            .and_then(|ty| {
                                casts::expr_type_from_type(&tree.state, ty, &dims.const_env)
                            })
                            .is_some_and(|ty| ty.width > 0)
                            || integer(*slice, tree, dims).is_ok_and(|size| size > 0)
                    };
                    if !valid {
                        return Err(unsupported(
                            "streaming slice size must be a positive known constant or a sized type",
                        ));
                    }
                }
                for part in expressions {
                    push(&mut jobs, *part);
                }
            }
            ExprKind::Pattern { .. } => {
                let shape = patterns::typed_shape(id, tree, dims)?
                    .or(receiving)
                    .ok_or_else(|| unsupported("assignment pattern without a target type"))?;
                for (value, shape) in pattern_items(id, &shape, tree, dims)? {
                    jobs.push((value, query, Some(shape)));
                }
            }
            ExprKind::Set(parts) => {
                for part in parts {
                    dimension_jobs(&part.kind, &mut jobs, query);
                }
            }
            ExprKind::MinTypMax {
                minimum,
                typical,
                maximum,
            } => {
                for part in [minimum, typical, maximum] {
                    push(&mut jobs, *part);
                }
            }
            ExprKind::Tagged { value, .. } => {
                if let Some(value) = value {
                    push(&mut jobs, *value);
                }
            }
            ExprKind::Copy(_) => {
                return Err(unsupported("shallow copy in skipped operand"));
            }
            ExprKind::ArrayMethodWith { .. } | ExprKind::RandomizeWith { .. } => {
                return Err(unsupported("array method with expression"));
            }
            ExprKind::Parameterized { .. } => {
                return Err(unsupported("parameterized value in skipped operand"));
            }
        }
    }
    Ok(())
}

fn dimension_jobs(
    kind: &DimensionKind,
    jobs: &mut Vec<(ExprId, bool, Option<VariableDimensions>)>,
    query: bool,
) {
    match kind {
        DimensionKind::Index(id) => jobs.push((*id, query, None)),
        DimensionKind::Range { left, right } => {
            jobs.extend([(*left, false, None), (*right, false, None)])
        }
        DimensionKind::Indexed { base, width, .. } => {
            jobs.extend([(*base, query, None), (*width, false, None)])
        }
        _ => {}
    }
}

/// Sparse shape validation: each authored item is checked once, including a
/// repeated item and a default covering a huge target. No target-sized vector.
fn pattern_items(
    root: ExprId,
    shape: &VariableDimensions,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Vec<(ExprId, VariableDimensions)>> {
    use bound_patterns::{Key, NativeKind};
    let plan = bound_patterns::prepare_native(root, tree, dims, Some(shape))?;
    let wrong = || unsupported("assignment pattern items do not match the target shape");
    if shape.unpacked.is_empty() && shape.packed.len() <= 1 && !shape.members.is_empty() {
        if shape
            .members
            .iter()
            .any(packed_structs::PackedMember::is_tagged)
            || (shape.members.len() > 1 && shape.members.iter().all(|member| member.offset() == 0))
        {
            return Err(unsupported("assignment pattern for a packed union"));
        }
        let mut values = Vec::new();
        match plan.kind {
            NativeKind::List => {
                if plan.bindings.len() != shape.members.len() {
                    return Err(wrong());
                }
                for (value, member) in plan.bindings.into_iter().zip(&shape.members) {
                    values.push((value, procedural::dimensions_from_type(member.r#type())));
                }
            }
            NativeKind::Keyed(keys) => {
                let mut named = HashMap::default();
                let mut default = None;
                for (key, value) in keys.into_iter().zip(plan.bindings) {
                    match key {
                        Key::Member(name) => {
                            if !shape.members.iter().any(|member| member.name() == name) {
                                return Err(unsupported("assignment pattern structure key"));
                            }
                            named.insert(name, value);
                        }
                        Key::Default => default = Some(value),
                        Key::Index(_) | Key::Name { .. } => {
                            return Err(unsupported("structure assignment pattern form"));
                        }
                    }
                }
                for member in &shape.members {
                    let value = named
                        .get(member.name())
                        .copied()
                        .or(default)
                        .ok_or_else(wrong)?;
                    values.push((value, procedural::dimensions_from_type(member.r#type())));
                }
            }
            NativeKind::Repeat(_) => return Err(unsupported("structure assignment pattern form")),
        }
        return Ok(values);
    }
    let (count, low, high, element) = if let Some((dim, rest)) = shape.unpacked.split_first() {
        let left =
            eval_ast_const_expr(&tree.state, &dim.left, &dims.const_env).ok_or_else(wrong)?;
        let right =
            eval_ast_const_expr(&tree.state, &dim.right, &dims.const_env).ok_or_else(wrong)?;
        let count = usize::try_from(left.abs_diff(right))
            .ok()
            .and_then(|v| v.checked_add(1))
            .ok_or_else(wrong)?;
        (
            count,
            left.min(right),
            left.max(right),
            VariableDimensions {
                container: None,
                real: false,
                string: false,
                class: None,
                unpacked: rest.to_vec(),
                ..shape.clone()
            },
        )
    } else if let Some((dim, rest)) = shape.packed.split_first()
        && !rest.is_empty()
    {
        let left =
            eval_ast_const_expr(&tree.state, &dim.left, &dims.const_env).ok_or_else(wrong)?;
        let right =
            eval_ast_const_expr(&tree.state, &dim.right, &dims.const_env).ok_or_else(wrong)?;
        let count = usize::try_from(left.abs_diff(right))
            .ok()
            .and_then(|v| v.checked_add(1))
            .ok_or_else(wrong)?;
        (
            count,
            left.min(right),
            left.max(right),
            VariableDimensions {
                container: None,
                real: false,
                string: false,
                class: None,
                packed: rest.to_vec(),
                ..shape.clone()
            },
        )
    } else {
        let count = patterns::shape_width(&tree.state, shape, dims).ok_or_else(wrong)?;
        (
            count,
            0,
            count as i128 - 1,
            VariableDimensions {
                container: None,
                randomizable: true,
                enum_domain: None,
                packed_enum: false,
                real: false,
                string: false,
                class: None,
                packed: Vec::new(),
                unpacked: Vec::new(),
                members: Vec::new(),
                signed: false,
                is_2state: shape.is_2state,
                signed_element_depth: None,
                unpacked_struct: None,
            },
        )
    };
    match plan.kind {
        NativeKind::List => {
            if plan.bindings.len() != count {
                return Err(wrong());
            }
        }
        NativeKind::Repeat(times) => {
            if times.checked_mul(plan.bindings.len()) != Some(count) {
                return Err(wrong());
            }
        }
        NativeKind::Keyed(keys) => {
            let mut indices = HashSet::default();
            let mut default = false;
            for key in keys {
                match key {
                    Key::Index(index) => {
                        if !(low..=high).contains(&index) {
                            return Err(unsupported("assignment pattern index outside the target"));
                        }
                        indices.insert(index);
                    }
                    Key::Default => default = true,
                    Key::Member(_) | Key::Name { .. } => {
                        return Err(unsupported(
                            "member or type key in an array assignment pattern",
                        ));
                    }
                }
            }
            if !default && indices.len() != count {
                return Err(wrong());
            }
        }
    }
    Ok(plan
        .bindings
        .into_iter()
        .map(|value| (value, element.clone()))
        .collect())
}

/// Inspect only literal prefixes and shape roles. Huge value operands may be
/// proved irrelevant by logical short circuiting; huge shape operands may not.
pub(in crate::ast) fn literal_widths(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<ConstExpr>> {
    let native = tree.declarations();
    let mut jobs = vec![(root, true, false)];
    let mut oversized = None;
    let mut replication = false;
    let mut visited = Vec::new();
    while let Some((id, evaluated, shape)) = jobs.pop() {
        visited.push(id);
        let add = |jobs: &mut Vec<_>, id| jobs.push((id, evaluated, shape));
        match &native.expression(id).kind {
            ExprKind::ArrayMethodWith { .. } | ExprKind::RandomizeWith { .. } => {
                return Err(unsupported("array method with expression"));
            }
            ExprKind::Literal(text) if evaluated => {
                if let Some((prefix, _)) = text.split_once('\'')
                    && !prefix.is_empty()
                    && prefix.chars().all(|c| c.is_ascii_digit() || c == '_')
                {
                    let width = prefix
                        .replace('_', "")
                        .parse::<usize>()
                        .map_err(|_| unsupported("parameter literal width"))?;
                    if width > constant_folding::MAX_CONSTANT_CONCAT_BITS {
                        if shape {
                            validate_parameter_materialization_width(Some(width))?;
                        }
                        oversized.get_or_insert(width);
                    }
                }
            }
            ExprKind::Group(inner)
            | ExprKind::Unary {
                expression: inner, ..
            }
            | ExprKind::Member {
                expression: inner, ..
            } => add(&mut jobs, *inner),
            ExprKind::Binary { left, right, .. } | ExprKind::Assignment { left, right, .. } => {
                add(&mut jobs, *left);
                add(&mut jobs, *right);
            }
            ExprKind::Conditional {
                condition,
                then_expression,
                else_expression,
            } => {
                for id in [condition, then_expression, else_expression] {
                    add(&mut jobs, *id);
                }
            }
            ExprKind::Call {
                expression,
                arguments,
            }
            | ExprKind::Parameterized {
                expression,
                arguments,
            } => {
                let query = expression_binding::native_name(&tree.state, *expression, native)
                    .and_then(|name| SystemFunction::from_name(&name))
                    .is_some_and(SystemFunction::is_data_query);
                for (index, arg) in arguments.iter().enumerate() {
                    if let Some(value) = arg.value {
                        if query && index == 0 {
                            jobs.push((value, false, false));
                        } else if query {
                            jobs.push((value, true, true));
                        } else {
                            add(&mut jobs, value);
                        }
                    }
                }
            }
            ExprKind::Cast { target, expression } => {
                jobs.push((*target, true, true));
                add(&mut jobs, *expression);
            }
            ExprKind::Select {
                expression,
                dimension,
            }
            | ExprKind::StreamSelect {
                expression,
                dimension,
            } => {
                add(&mut jobs, *expression);
                let mut children = Vec::new();
                dimension_jobs(&dimension.kind, &mut children, false);
                jobs.extend(children.into_iter().map(|(id, _, _)| (id, true, true)));
            }
            ExprKind::Replication { count, expressions } => {
                replication |= evaluated && !shape;
                jobs.push((*count, true, true));
                for value in expressions {
                    add(&mut jobs, *value);
                }
            }
            ExprKind::Concatenation(values) => {
                for value in values {
                    add(&mut jobs, *value);
                }
            }
            ExprKind::Streaming {
                slice, expressions, ..
            } => {
                if let Some(slice) = slice {
                    jobs.push((*slice, true, true));
                }
                for value in expressions {
                    add(&mut jobs, *value);
                }
            }
            ExprKind::Pattern { target, items } => {
                if let Some(target) = target {
                    jobs.push((*target, true, true));
                }
                for item in items {
                    if let Some(key) = item.key {
                        jobs.push((key, true, true));
                    }
                    add(&mut jobs, item.value);
                }
            }
            ExprKind::Type(id) => {
                for (id, type_shape) in type_expressions(*id, native) {
                    jobs.push(if type_shape {
                        (id, true, true)
                    } else {
                        (id, evaluated, shape)
                    });
                }
            }
            ExprKind::Set(parts) => {
                for part in parts {
                    let mut children = Vec::new();
                    dimension_jobs(&part.kind, &mut children, false);
                    jobs.extend(children.into_iter().map(|(id, _, _)| (id, true, true)));
                }
            }
            ExprKind::MinTypMax {
                minimum,
                typical,
                maximum,
            } => {
                for id in [minimum, typical, maximum] {
                    add(&mut jobs, *id);
                }
            }
            ExprKind::Tagged {
                value: Some(value), ..
            } => add(&mut jobs, *value),
            _ => {}
        }
    }
    if oversized.is_none() && !replication {
        return Ok(None);
    }
    let proof = proof(root, tree, dims)?;
    if let Some(proof) = expr_to_const(&tree.state, proof)
        && let Some(ty) = infer_const_expr_type_inner(
            &proof,
            &parameter_types_from_const_env(&dims.const_env),
            true,
        )
        && ty.width <= constant_folding::MAX_CONSTANT_CONCAT_BITS
        && let Some(value) = eval_ast_const_expr(&tree.state, &proof, &dims.const_env)
    {
        for id in visited {
            match &native.expression(id).kind {
                ExprKind::Call {
                    expression,
                    arguments,
                } => {
                    if let Some(name) =
                        expression_binding::native_name(&tree.state, *expression, native)
                        && name.starts_with('$')
                    {
                        let args = arguments
                            .iter()
                            .map(|arg| {
                                if arg.value.is_some() {
                                    system_functions::Arg::Given
                                } else {
                                    system_functions::Arg::Omitted
                                }
                            })
                            .collect::<Vec<_>>();
                        system_functions::check_call(
                            &name,
                            arguments
                                .iter()
                                .all(|arg| arg.name.is_none())
                                .then_some(args.as_slice()),
                            system_functions::CallSite::Expression,
                        )?;
                    }
                }
                ExprKind::Cast { target, expression } => {
                    expression_binding::cast_target_native(
                        *target,
                        *expression,
                        native,
                        tree,
                        dims,
                    )?;
                }
                _ => {}
            }
        }
        return Ok(Some(ConstExpr::Literal(format_typed_parameter_literal(
            value, ty.width, ty.signed,
        ))));
    }
    validate_parameter_materialization_width(oversized)?;
    Ok(None)
}

fn proof(root: ExprId, tree: &AnalysisContext<'_>, dims: &PackedDimensions) -> Converted<Expr> {
    enum Job {
        Enter(ExprId),
        Unary(String),
        Binary(String),
    }
    let native = tree.declarations();
    let mut jobs = vec![Job::Enter(root)];
    let mut values = Vec::new();
    let unknown = || Expr::Ident("\0unmaterialized-parameter-operand".into());
    while let Some(job) = jobs.pop() {
        match job {
            Job::Enter(id) => match &native.expression(id).kind {
                ExprKind::Literal(text) => values.push(
                    if typecheck::integral_literal_type_metadata(text).is_some_and(|(width, _)| {
                        width <= constant_folding::MAX_CONSTANT_CONCAT_BITS
                    }) {
                        Expr::Literal(text.clone())
                    } else {
                        unknown()
                    },
                ),
                ExprKind::Group(inner) => jobs.push(Job::Enter(*inner)),
                ExprKind::Name(_) | ExprKind::Member { .. }
                    if expression_binding::native_name(&tree.state, id, native).is_some() =>
                {
                    let name = expression_binding::native_name(&tree.state, id, native).unwrap();
                    values.push(
                        dims.parameter_values
                            .get(&name)
                            .filter(|value| {
                                expr_static_width(&tree.state, value, dims).is_some_and(|width| {
                                    width <= constant_folding::MAX_CONSTANT_CONCAT_BITS
                                })
                            })
                            .cloned()
                            .unwrap_or(Expr::Ident(name)),
                    );
                }
                ExprKind::Unary {
                    operator,
                    expression,
                    postfix: false,
                } => {
                    jobs.push(Job::Unary(operator.clone()));
                    jobs.push(Job::Enter(*expression));
                }
                ExprKind::Binary {
                    operator,
                    left,
                    right,
                } => {
                    jobs.push(Job::Binary(operator.clone()));
                    jobs.push(Job::Enter(*right));
                    jobs.push(Job::Enter(*left));
                }
                ExprKind::Select { .. } => {
                    let mut base = id;
                    while let ExprKind::Select { expression, .. } = native.expression(base).kind {
                        base = expression;
                    }
                    let safe = expression_binding::native_name(&tree.state, base, native)
                        .is_some_and(|name| {
                            dims.parameter_values
                                .get(&name)
                                .and_then(|value| expr_static_width(&tree.state, value, dims))
                                .is_some_and(|width| {
                                    width <= constant_folding::MAX_CONSTANT_CONCAT_BITS
                                })
                        });
                    values.push(if safe {
                        let mut context = dims.clone();
                        context.constant_indexed_base = true;
                        expression_binding::bind(id, tree, &context)
                            .ok()
                            .and_then(|value| {
                                selects::indexed_expression_value(&tree.state, value, &context)
                            })
                            .map(const_expr_to_expr)
                            .unwrap_or_else(unknown)
                    } else {
                        unknown()
                    });
                }
                _ => values.push(unknown()),
            },
            Job::Unary(operator) => {
                let value = values
                    .pop()
                    .ok_or_else(|| unsupported("native bounded proof stack"))?;
                values.push(
                    constants::unary_expr_from_text(&operator, value).unwrap_or_else(unknown),
                );
            }
            Job::Binary(operator) => {
                let right = values
                    .pop()
                    .ok_or_else(|| unsupported("native bounded proof stack"))?;
                let left = values
                    .pop()
                    .ok_or_else(|| unsupported("native bounded proof stack"))?;
                values.push(
                    if let Some(op) = constants::binary_op_from_text(&operator) {
                        Expr::Binary {
                            left: Box::new(left),
                            op,
                            right: Box::new(right),
                        }
                    } else {
                        unknown()
                    },
                );
            }
        }
    }
    values
        .pop()
        .ok_or_else(|| unsupported("native bounded proof result"))
}

/// Native declaration ownership supplies every default, including captured
/// class headers. No original-node pairing, offset index, or name lookup.
pub(in crate::ast) fn validate_scope(
    owner: Option<celox_sv_syntax::BodyItemId>,
    is_module: bool,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    use celox_sv_syntax::{BodyItemKind, LocalDeclaration, ParameterKind, StatementKind};
    let native = tree.declarations();
    let mut values = Vec::<(celox_sv_syntax::Parameter, SyntaxTree, bool)>::new();
    if owner.is_none()
        && !matches!(
            tree.scope_handle(),
            crate::source_tree::ScopeHandle::Compilation
        )
    {
        for parameter in tree.root_unit()?.parameters.iter().flatten() {
            values.push((parameter.clone(), tree.syntax_tree.clone(), true));
        }
    }
    let mut bodies = owner
        .map_or_else(|| tree.body_membership(), |id| Ok(vec![id]))?
        .into_iter()
        .rev()
        .map(|id| (id, tree.syntax_tree.clone(), true))
        .collect::<Vec<_>>();
    let mut statements = Vec::new();
    let mut seen = HashSet::default();
    while let Some((id, scope, prefix)) = bodies.pop() {
        if !seen.insert(id) {
            return Err(AnalyzerError::Parse(
                "duplicate native parameter validation owner".into(),
            ));
        }
        match &native.body_item(id).kind {
            BodyItemKind::Parameter(id) => {
                for parameter in &native.parameter_declaration(*id).parameters {
                    values.push((parameter.clone(), scope.clone(), prefix));
                }
            }
            BodyItemKind::Region(items) => {
                bodies.extend(items.iter().rev().map(|id| (*id, scope.clone(), prefix)))
            }
            BodyItemKind::Block { items, .. } | BodyItemKind::Unconverted { items, .. } => {
                bodies.extend(items.iter().rev().map(|id| (*id, scope.clone(), false)))
            }
            BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } if !is_module => {
                bodies.extend(otherwise.iter().map(|id| (*id, scope.clone(), false)));
                bodies.push((*then_body, scope, false));
            }
            BodyItemKind::Case { arms, .. } if !is_module => bodies.extend(
                arms.iter()
                    .rev()
                    .map(|arm| (arm.body, scope.clone(), false)),
            ),
            BodyItemKind::Loop { body, .. } if !is_module => bodies.push((*body, scope, false)),
            BodyItemKind::Procedure(id) => statements.extend(
                native
                    .procedure(*id)
                    .authored_statements
                    .iter()
                    .rev()
                    .map(|id| (*id, scope.clone())),
            ),
            BodyItemKind::Subroutine(id) => statements.extend(
                native
                    .subroutine(*id)
                    .authored_statements
                    .iter()
                    .rev()
                    .map(|id| (*id, scope.clone())),
            ),
            BodyItemKind::Unit(id) => {
                let scope = scope.scoped(crate::source_tree::ScopeHandle::Unit(*id))?;
                let unit = native.unit(*id);
                for parameter in unit.parameters.iter().flatten() {
                    values.push((parameter.clone(), scope.clone(), false));
                }
                bodies.extend(
                    unit.items
                        .iter()
                        .rev()
                        .map(|id| (*id, scope.clone(), false)),
                );
            }
            _ => {}
        }
    }
    let mut seen = HashSet::default();
    while let Some((id, scope)) = statements.pop() {
        if !seen.insert(id) {
            continue;
        }
        let kind = &native.statement(id).kind;
        if let StatementKind::Declaration(LocalDeclaration::Parameters(id)) = kind {
            for parameter in &native.parameter_declaration(*id).parameters {
                values.push((parameter.clone(), scope.clone(), false));
            }
        }
        statements.extend(kind.children().map(|id| (id, scope.clone())));
    }
    values.sort_by_key(|(parameter, _, _)| parameter.tokens.start);
    let prefix = values
        .iter()
        .filter(|(_, _, prefix)| *prefix)
        .map(|(parameter, _, _)| parameter)
        .collect::<Vec<_>>();
    for (parameter, scope, is_prefix) in &values {
        let scoped_context = tree.child(scope, &tree.parameter_overrides);
        let scope = &scoped_context;
        let ParameterKind::Value {
            default: Some(root),
            ..
        } = parameter.kind
        else {
            continue;
        };
        let mut unavailable = HashSet::default();
        unavailable.insert(instances::normalize_identifier(&parameter.name.spelling));
        if *is_prefix {
            unavailable.extend(
                prefix
                    .iter()
                    .filter(|other| other.tokens.start >= parameter.tokens.start)
                    .map(|other| instances::normalize_identifier(&other.name.spelling)),
            );
        }
        validate_operators(&tree.state, root, native)?;
        if literal_widths(root, scope, dimensions)?.is_some() {
            validate(root, scope, dimensions, &unavailable, true)?;
        } else if needs_selected(&tree.state, root, native) {
            let context = dimensions::query_context_for_operand(root, scope, dimensions)?;
            selects::selected_parameter_initializer(root, scope, &context, None)?;
        }
    }
    Ok(())
}

pub(in crate::ast) fn needs_selected(
    state: &AnalysisState,
    root: ExprId,
    native: &celox_sv_syntax::Declarations,
) -> bool {
    let mut jobs = vec![root];
    while let Some(id) = jobs.pop() {
        // Data query operands contribute shape, not constant values. Their
        // operators are validated by the query binder in its type context.
        if let ExprKind::Call { expression, .. } = native.expression(id).kind
            && expression_binding::native_name(state, expression, native)
                .and_then(|name| SystemFunction::from_name(&name))
                .is_some_and(SystemFunction::is_data_query)
        {
            continue;
        }
        match &native.expression(id).kind {
            ExprKind::Cast { .. } => return true,
            ExprKind::Select {
                expression,
                dimension,
            } if !matches!(dimension.kind, DimensionKind::Index(_))
                || matches!(
                    native
                        .expression(expression_binding::ungroup(*expression, native))
                        .kind,
                    ExprKind::Select { .. }
                ) =>
            {
                return true;
            }
            _ => jobs.extend(children(id, native)),
        }
    }
    false
}

fn validate_operators(
    state: &AnalysisState,
    root: ExprId,
    native: &celox_sv_syntax::Declarations,
) -> Converted<()> {
    let mut jobs = vec![root];
    while let Some(id) = jobs.pop() {
        // Data query operands contribute shape, not constant values. Their
        // operators are validated by the query binder in its type context.
        if let ExprKind::Call { expression, .. } = native.expression(id).kind
            && expression_binding::native_name(state, expression, native)
                .and_then(|name| SystemFunction::from_name(&name))
                .is_some_and(SystemFunction::is_data_query)
        {
            continue;
        }
        if matches!(&native.expression(id).kind,
            ExprKind::Concatenation(parts) | ExprKind::Replication { expressions: parts, .. } if parts.is_empty())
        {
            return Err(unsupported("empty parameter concatenation"));
        }
        if let ExprKind::Unary {
            operator,
            expression,
            ..
        } = &native.expression(id).kind
        {
            if matches!(
                operator.as_str(),
                "&" | "|" | "^" | "~&" | "~|" | "~^" | "^~"
            ) {
                return Err(unsupported("reduction operator in parameter expression"));
            }
            if operator == "~"
                && expression_binding::native_name(state, *expression, native).is_some()
            {
                return Err(unsupported(
                    "width-dependent complement in parameter expression",
                ));
            }
        }
        jobs.extend(children(id, native));
    }
    Ok(())
}

pub(in crate::ast) fn children(
    root: ExprId,
    native: &celox_sv_syntax::Declarations,
) -> Vec<ExprId> {
    let mut children = Vec::new();
    match &native.expression(root).kind {
        ExprKind::Group(inner)
        | ExprKind::Unary {
            expression: inner, ..
        }
        | ExprKind::Member {
            expression: inner, ..
        } => children.push(*inner),
        ExprKind::Binary { left, right, .. }
        | ExprKind::Assignment { left, right, .. }
        | ExprKind::Cast {
            target: left,
            expression: right,
        } => children.extend([*left, *right]),
        ExprKind::Conditional {
            condition,
            then_expression,
            else_expression,
        } => children.extend([*condition, *then_expression, *else_expression]),
        ExprKind::RandomizeWith {
            expression,
            constraints,
        } => {
            children.push(*expression);
            children.extend(constraints.iter().flatten());
        }
        ExprKind::ArrayMethodWith {
            expression,
            predicate,
        } => {
            children.extend([*expression, *predicate]);
        }
        ExprKind::Call {
            expression,
            arguments,
        }
        | ExprKind::Parameterized {
            expression,
            arguments,
        } => {
            children.push(*expression);
            children.extend(arguments.iter().filter_map(|arg| arg.value));
        }
        ExprKind::Select {
            expression,
            dimension,
        }
        | ExprKind::StreamSelect {
            expression,
            dimension,
        } => {
            children.push(*expression);
            let mut jobs = Vec::new();
            dimension_jobs(&dimension.kind, &mut jobs, false);
            children.extend(jobs.into_iter().map(|(id, _, _)| id));
        }
        ExprKind::Concatenation(parts) => children.extend(parts),
        ExprKind::Streaming {
            slice, expressions, ..
        } => {
            children.extend(slice);
            children.extend(expressions);
        }
        ExprKind::Replication { count, expressions } => {
            children.push(*count);
            children.extend(expressions);
        }
        ExprKind::Pattern { target, items } => {
            children.extend(target);
            for item in items {
                children.extend(item.key);
                children.push(item.value);
            }
        }
        ExprKind::Set(parts) => {
            for part in parts {
                let mut jobs = Vec::new();
                dimension_jobs(&part.kind, &mut jobs, false);
                children.extend(jobs.into_iter().map(|(id, _, _)| id));
            }
        }
        ExprKind::MinTypMax {
            minimum,
            typical,
            maximum,
        } => children.extend([*minimum, *typical, *maximum]),
        ExprKind::Tagged { value, .. } => children.extend(value),
        ExprKind::Type(id) => {
            children.extend(type_expressions(*id, native).into_iter().map(|(id, _)| id))
        }
        _ => {}
    }
    children
}

fn type_expressions(
    root: celox_sv_syntax::TypeId,
    native: &celox_sv_syntax::Declarations,
) -> Vec<(ExprId, bool)> {
    use celox_sv_syntax::DataTypeKind;
    let mut types = vec![root];
    let mut result = Vec::new();
    let dimensions = |dimensions: &[celox_sv_syntax::Dimension], result: &mut Vec<_>| {
        for dimension in dimensions {
            let mut jobs = Vec::new();
            dimension_jobs(&dimension.kind, &mut jobs, false);
            result.extend(jobs.into_iter().map(|(id, _, _)| (id, true)));
        }
    };
    while let Some(id) = types.pop() {
        let data = native.data_type(id);
        dimensions(&data.packed_dimensions, &mut result);
        match &data.kind {
            DataTypeKind::TypeOf(id) => result.push((*id, false)),
            DataTypeKind::Struct { fields, .. } => {
                for field in fields {
                    types.push(field.data_type);
                    dimensions(&field.dimensions, &mut result);
                    result.extend(field.default.map(|id| (id, false)));
                }
            }
            DataTypeKind::Enum { base, items } => {
                types.extend(base);
                for item in items {
                    dimensions(&item.dimensions, &mut result);
                    result.extend(item.value.map(|id| (id, false)));
                }
            }
            DataTypeKind::Named(path) => {
                for part in path {
                    result.extend(
                        part.arguments
                            .iter()
                            .flatten()
                            .filter_map(|arg| arg.value.map(|id| (id, false))),
                    );
                }
            }
            DataTypeKind::VirtualInterface { arguments, .. } => result.extend(
                arguments
                    .iter()
                    .flatten()
                    .filter_map(|arg| arg.value.map(|id| (id, false))),
            ),
            _ => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(source: &str) -> SyntaxTree {
        crate::source_tree::parse_scope_fixture(source, Path::new("native-skipped.sv")).unwrap()
    }
    #[test]
    fn skipped_streaming_with_validates_ranges_without_lowering_arrays() {
        for (range, valid) in [
            ("0", true),
            ("0:1", true),
            ("0 +: 2", true),
            ("1 -: 2", true),
            ("-1 +: 4", true),
            ("0 +: (2 - 2)", false),
            ("0 -: -1", false),
            ("0 +: 1'bx", false),
            ("0 -: 1'bz", false),
            ("missing", false),
            ("0:missing", false),
            ("missing:0", false),
            ("1.5", false),
            ("170141183460469231731687303715884105727 +: 2", false),
        ] {
            let source = format!(
                "module M(input logic[7:0]data[0:1]);initial $display({{<<1{{data with[{range}]}}}});endmodule"
            );
            let tree = parse(&source);
            let tree = AnalysisContext::unspecialized(&tree);
            let root = tree
                .declarations()
                .expression_nodes()
                .find_map(|(id, node)| {
                    matches!(node.kind, ExprKind::StreamSelect { .. }).then_some(id)
                })
                .unwrap();
            let result = validate(
                root,
                &tree,
                &PackedDimensions::default(),
                &HashSet::default(),
                false,
            );
            assert_eq!(result.is_ok(), valid, "{range}: {result:?}");
        }
    }
    #[test]
    fn skipped_patterns_validate_billion_element_targets_without_expanding_them() {
        for (items, valid) in [
            ("default:0", true),
            ("0:1, default:0", true),
            ("1000000000:1, default:0", false),
            ("1,2", false),
        ] {
            let tree = parse(&format!(
                "module M;typedef logic[7:0] T[0:999999999];localparam N=0&&T'{{{items}}};endmodule"
            ));
            let tree = AnalysisContext::unspecialized(&tree);
            let root = tree
                .declarations()
                .expression_nodes()
                .find_map(|(id, node)| matches!(node.kind, ExprKind::Pattern { .. }).then_some(id))
                .unwrap();
            let result = validate(
                root,
                &tree,
                &PackedDimensions::default(),
                &HashSet::default(),
                false,
            );
            assert_eq!(result.is_ok(), valid, "{items}: {result:?}");
        }
    }
    #[test]
    fn native_bounded_proof_retains_precedence_without_parsing_huge_payloads() {
        for (value, expected) in [
            ("0&&1000000000'd0", 0),
            ("1||1000000000'bx&&0", 1),
            ("(0&&1000000000'd0)||1", 1),
        ] {
            let tree = parse(&format!("module M;localparam N={value};endmodule"));
            let tree = AnalysisContext::unspecialized(&tree);
            let parameter = &tree.declarations().parameter_declarations[0].parameters[0];
            let root = ownership::default(parameter).unwrap();
            let value = literal_widths(root, &tree, &PackedDimensions::default())
                .unwrap()
                .unwrap();
            assert_eq!(
                eval_ast_const_expr(&tree.state, &value, &HashMap::default()),
                Some(expected)
            );
        }
    }
    #[test]
    fn nested_query_type_ranges_keep_materialization_guards() {
        let tree = parse(
            "module M;localparam N=0&&$bits(struct packed{logic[1000000000'd1:0]x;});endmodule",
        );
        let tree = AnalysisContext::unspecialized(&tree);
        let root = ownership::default(&tree.declarations().parameter_declarations[0].parameters[0])
            .unwrap();
        let error = literal_widths(root, &tree, &PackedDimensions::default()).unwrap_err();
        assert!(
            error.to_string().contains("constant materialization limit"),
            "{error}"
        );
    }
}
