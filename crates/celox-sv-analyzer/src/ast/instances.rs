//! Module instantiations, parameter overrides, and port connections.

use super::*;
mod connections;

pub(super) fn instances_from_scope(
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    packed_dimensions: &PackedDimensions,
    interfaces: &InterfaceLookup<'_>,
) -> Result<Vec<Instance>, AnalyzerError> {
    // The types of the scope with its constants: a variable of a generate
    // block may have a type whose shape a parameter gives.
    let type_aliases = packed_dimensions.type_aliases.clone();
    let active = generate::items(syntax_tree, session, const_env, &type_aliases)?;
    let mut instances = Vec::new();
    let mut views = generate::ScopeViews::new(packed_dimensions);
    let mut defparams = Vec::new();
    for item in &active {
        if let celox_sv_syntax::BodyItemKind::Defparam(assignments) =
            &syntax_tree.declarations().body_item(item.id).kind
        {
            defparams.extend(assignments.iter().map(|assignment| (item, assignment)));
            continue;
        }
        let syntax_tree = &item.scoped(syntax_tree);
        let celox_sv_syntax::BodyItemKind::Instance(native) =
            syntax_tree.declarations().body_item(item.id).kind
        else {
            continue;
        };
        if scope::imported(syntax_tree)
            .interface_declarations
            .contains(&item.id)
        {
            continue;
        }
        let native = syntax_tree.declarations().instantiation(native);
        let syntax_tree = &item.enter(syntax_tree);
        let start = instances.len();
        let dimensions = views.dimensions(item);
        instances_from_native_declaration(
            item.id,
            native,
            None,
            syntax_tree,
            session,
            &item.env,
            dimensions,
            interfaces,
            &mut instances,
        )?;
        for instance in &mut instances[start..] {
            instance.name = item.instance_name(&instance.name);
            for connection in &mut instance.port_connections {
                connection.map_expressions(|expr| {
                    let mut expr = substitute_expr_constants_with_parameter_literals(
                        syntax_tree,
                        session,
                        expr,
                        &item.env,
                        &item.literals,
                    );
                    item.expr(&mut expr);
                    expr
                });
            }
        }
    }
    instances.retain(|instance| !type_aliases.contains_key(instance.module_name()));
    // The last `defparam` of a parameter in the source text gives its value
    // (IEEE 1800-2023 23.10.1).
    defparams.sort_by_key(|(_, assignment)| assignment.tokens.start);
    for (item, assignment) in defparams {
        apply_defparam(
            item,
            assignment,
            syntax_tree,
            session,
            &mut views,
            &mut instances,
        )?;
    }
    Ok(instances)
}

/// Give the instance that `assignment` names the parameter value it assigns
/// (IEEE 1800-2023 23.10.1). The value is evaluated in the scope of the
/// `defparam` and takes precedence over a parameter assignment of the
/// instantiation (23.10); the instance is one of this module, in the
/// generate block of the `defparam` when it is in one.
fn apply_defparam<'a>(
    item: &'a generate::Item,
    assignment: &celox_sv_syntax::DefparamAssignment,
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    views: &mut generate::ScopeViews<'a>,
    instances: &mut [Instance],
) -> Converted<()> {
    let scoped = item.scoped(syntax_tree);
    let tree = &item.enter(&scoped);
    let dimensions = views.dimensions(item);
    let Some((parameter, scopes)) = assignment.path.split_last() else {
        return Err(AnalyzerError::Parse("defparam without a target".into()));
    };
    let parameter_name = normalize_identifier(&parameter.name.spelling);
    let spelled = || {
        assignment
            .path
            .iter()
            .map(|segment| segment.name.spelling.as_str())
            .collect::<Vec<_>>()
            .join(".")
    };
    if assignment.rooted {
        return Err(unsupported(format!(
            "defparam of the `$root` name `{}`",
            spelled()
        )));
    }
    if parameter.index.is_some() {
        return Err(AnalyzerError::InvalidDefparam(format!(
            "the target `{}` selects from a parameter",
            spelled()
        )));
    }
    // Keep each parsed index separate from its name: an escaped name may
    // itself contain `[` (5.6.1), which is not an index.
    let mut components = Vec::new();
    for segment in scopes {
        let component = normalize_identifier(&segment.name.spelling);
        let index = match segment.index {
            Some(index) => Some(
                constant_binding::bind(index, tree, session, dimensions)?
                    .and_then(|index| eval_ast_const_expr(&tree.functions, &index, &item.env))
                    .ok_or_else(|| {
                        unsupported(format!(
                            "index of defparam target `{}` that is not constant",
                            spelled()
                        ))
                    })?,
            ),
            None => None,
        };
        components.push((component, index));
    }
    // The name may start at the module itself (23.6).
    let module = tree
        .root_unit()
        .ok()
        .map(|unit| normalize_identifier(&unit.name.spelling));
    if components.len() > 1 && module.as_deref() == Some(components[0].0.as_str()) {
        components.remove(0);
    }
    if components.is_empty() {
        return Err(unsupported(format!(
            "defparam of `{}`, a parameter of its own scope",
            spelled()
        )));
    }
    // The scopes of the name are searched for from the scope of the
    // `defparam` outward (23.8); an instance of the name ends the search.
    let mut scope = Some(item.scope_name());
    let mut target = None;
    let mut below = false;
    let mut array_element = false;
    'search: while let Some(prefix) = scope {
        for length in (1..=components.len()).rev() {
            let name = generate::scoped_instance_name(prefix, &components[..length]);
            if let Some(index) = instances.iter().position(|instance| instance.name == name) {
                if length == components.len() {
                    target = Some(index);
                } else {
                    below = true;
                }
                break 'search;
            }
            // An indexed name whose base is an instance array selects an
            // element of that array, stored under the base name alone.
            if components[length - 1].1.is_some() {
                let mut base = components[..length].to_vec();
                base[length - 1].1 = None;
                let name = generate::scoped_instance_name(prefix, &base);
                if instances
                    .iter()
                    .any(|instance| instance.name == name && instance.array_range.is_some())
                {
                    array_element = true;
                    break 'search;
                }
            }
        }
        scope = if prefix.is_empty() {
            None
        } else {
            Some(generate::parent_scope(prefix))
        };
    }
    if array_element {
        return Err(unsupported(format!(
            "defparam of `{}`, a parameter of an instance array",
            spelled()
        )));
    }
    if below {
        return Err(unsupported(format!(
            "defparam of `{}`, a parameter below a child instance",
            spelled()
        )));
    }
    let Some(target) = target else {
        return Err(unsupported(format!(
            "defparam of `{}`, which names no module instance of this module",
            spelled()
        )));
    };
    let instance = &mut instances[target];
    if instance.array_range.is_some() {
        return Err(unsupported(format!(
            "defparam of `{}`, a parameter of an instance array",
            spelled()
        )));
    }
    // A `defparam` in a generate block changes no parameter outside the
    // block (23.10.1).
    let own = item.scope_name();
    if !own.is_empty()
        && !instance
            .name
            .strip_prefix(own)
            .is_some_and(|rest| rest.starts_with('.'))
    {
        return Err(AnalyzerError::InvalidDefparam(format!(
            "the target `{}` is outside the generate block of the statement (IEEE 1800-2023 23.10.1)",
            spelled()
        )));
    }
    let mut value = parameter_override(
        parameter_name.clone(),
        assignment.value,
        tree,
        session,
        dimensions,
        false,
    )?;
    // A type parameter is not overridden by a `defparam` (6.20.3).
    if value.bound_type.is_some() {
        return Err(AnalyzerError::InvalidDefparam(format!(
            "the target `{}` is assigned a data type (IEEE 1800-2023 6.20.3)",
            spelled()
        )));
    }
    resolve_override(&mut value, tree, session, &item.env, dimensions)?;
    match instance
        .parameter_overrides
        .iter_mut()
        .find(|existing| existing.name == parameter_name)
    {
        Some(existing) => *existing = value,
        None => {
            instance.parameter_names.push(parameter_name);
            instance.parameter_overrides.push(value);
        }
    }
    Ok(())
}

/// Make the value of an override a constant of the scope that gives it.
fn resolve_override(
    override_: &mut ParameterOverride,
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    packed_dimensions: &PackedDimensions,
) -> Converted<()> {
    let Some(value) = override_.value.take() else {
        return Ok(());
    };
    let value = substitute_typed_parameter_literals(
        value,
        const_env,
        &parameter_types_from_const_env(const_env),
    );
    let value = expr_to_index_const(
        syntax_tree,
        session,
        substitute_expr_idents(
            const_expr_to_expr(value),
            &packed_dimensions.parameter_values,
        ),
        packed_dimensions,
    )
    .ok_or_else(|| {
        AnalyzerError::Unsupported(format!(
            "constant module parameter override `{}`",
            override_.name
        ))
    })?;
    let value = substitute_const_expr_constants_preserving_enum_types(value, const_env);
    override_.value = Some(constant_calls_folded(
        value,
        &syntax_tree.functions,
        const_env,
    ));
    Ok(())
}

fn instances_from_native_declaration(
    id: celox_sv_syntax::BodyItemId,
    native: &celox_sv_syntax::Instantiation,
    condition: Option<ConstExpr>,
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    packed_dimensions: &PackedDimensions,
    interfaces: &InterfaceLookup<'_>,
    instances: &mut Vec<Instance>,
) -> Result<(), AnalyzerError> {
    validate_native_instances(native)?;
    let module_name = normalize_identifier(&native.name.spelling);
    let interface = interfaces.get(&module_name);
    let mut parameter_overrides = parameter_overrides(
        native.parameters.as_deref(),
        syntax_tree,
        session,
        interface,
        packed_dimensions,
        false,
    )?;
    for override_ in &mut parameter_overrides {
        resolve_override(
            override_,
            syntax_tree,
            session,
            const_env,
            packed_dimensions,
        )?;
    }
    let condition =
        condition.map(|condition| substitute_const_expr_constants(condition, const_env));
    let parameter_names: Vec<String> = parameter_overrides
        .iter()
        .map(|parameter| parameter.name().to_string())
        .collect();
    for (index, authored) in native.instances.iter().enumerate() {
        let name = normalize_identifier(&authored.name.spelling);
        let mut port_connections = port_connections(
            (id, index),
            authored,
            syntax_tree,
            session,
            packed_dimensions,
            interface,
        )?;
        for connection in &mut port_connections {
            connection.map_expressions(|expr| {
                substitute_expr_constants_with_parameter_literals(
                    syntax_tree,
                    session,
                    expr,
                    const_env,
                    &HashMap::default(),
                )
            });
        }
        let port_names = port_connections
            .iter()
            .map(|connection| connection.formal().to_string())
            .collect();
        // `Child c[3:0](...)` is an array of instances: one dimension with
        // constant bounds. `my_t mem [N];` parses the same way but declares a
        // variable of a typedef'd type, and is dropped below.
        let array_range = if authored.dimensions.is_empty()
            || packed_dimensions.type_aliases.contains_key(&module_name)
        {
            None
        } else {
            let ranges = instance_ranges(
                authored,
                syntax_tree,
                session,
                const_env,
                &packed_dimensions.type_aliases,
            )?;
            let [range] = ranges.as_slice() else {
                return Err(AnalyzerError::Unsupported(
                    "multidimensional module instance array".to_string(),
                ));
            };
            let left = eval_ast_const_expr(&syntax_tree.functions, range.left(), const_env);
            let right = eval_ast_const_expr(&syntax_tree.functions, range.right(), const_env);
            let bounds = left
                .zip(right)
                .filter(|(left, right)| left.abs_diff(*right) < 4096)
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("module instance array bounds".to_string())
                })?;
            Some(bounds)
        };
        let imported = scope::imported(syntax_tree);
        let binding = imported.interface_instances.get(&(id, index));
        let mut parameter_names = parameter_names.clone();
        let mut parameter_overrides = parameter_overrides.clone();
        if let Some(binding) = binding {
            parameter_names.extend(
                binding
                    .parameters
                    .iter()
                    .map(|parameter| parameter.name.clone()),
            );
            parameter_overrides.extend(binding.parameters.iter().cloned());
        }
        instances.push(Instance::new(
            binding.map_or_else(|| module_name.clone(), |binding| binding.module.clone()),
            name,
            parameter_names,
            parameter_overrides,
            condition.clone(),
            port_names,
            port_connections,
            array_range,
        ));
    }
    Ok(())
}

pub(super) fn parameter_overrides(
    native: Option<&[celox_sv_syntax::Argument]>,
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    interface: Option<&ModuleInterface>,
    packed_dimensions: &PackedDimensions,
    bind_values: bool,
) -> Result<Vec<ParameterOverride>, AnalyzerError> {
    let Some(native) = native else {
        return Ok(Vec::new());
    };
    if native.is_empty() {
        return Ok(Vec::new());
    }
    let ordered = native.iter().all(|argument| argument.name.is_none());
    if !ordered && native.iter().any(|argument| argument.name.is_none()) {
        return Err(unsupported("mixed named and ordered parameter assignment"));
    }
    let parameters = if ordered {
        Some(
            interface
                .map(|interface| interface.parameters.as_slice())
                .ok_or_else(|| unsupported("ordered parameter assignment"))?,
        )
    } else {
        None
    };
    let mut overrides = Vec::new();
    let mut names = HashSet::default();
    for (position, argument) in native.iter().enumerate() {
        let name = if let Some(parameters) = parameters {
            parameters
                .get(position)
                .cloned()
                .ok_or_else(|| unsupported("ordered parameter assignment"))?
        } else {
            normalize_identifier(&argument.name.as_ref().expect("named argument").spelling)
        };
        if !names.insert(name.clone()) {
            return Err(unsupported(format!(
                "duplicate parameter override `{name}`"
            )));
        }
        let value = argument
            .value
            .ok_or_else(|| unsupported(format!("empty parameter override `{name}`")))?;
        overrides.push(parameter_override(
            name,
            value,
            syntax_tree,
            session,
            packed_dimensions,
            bind_values,
        )?);
    }
    Ok(overrides)
}

pub(super) fn port_connections(
    key: interface_binding::InstanceKey,
    native: &celox_sv_syntax::InstanceDeclarator,
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    packed_dimensions: &PackedDimensions,
    interface: Option<&ModuleInterface>,
) -> Result<Vec<PortConnection>, AnalyzerError> {
    connections::connections(
        key,
        native,
        syntax_tree,
        session,
        packed_dimensions,
        interface,
    )
}

pub(super) fn declaration_from_item<'a>(
    id: celox_sv_syntax::BodyItemId,
    tree: &'a AnalysisContext<'_>,
) -> Converted<&'a celox_sv_syntax::Instantiation> {
    let celox_sv_syntax::BodyItemKind::Instance(id) = tree.declarations().body_item(id).kind else {
        return Err(AnalyzerError::Parse(
            "native instantiation body item is missing".into(),
        ));
    };
    Ok(tree.declarations().instantiation(id))
}

fn validate_native_instances(native: &celox_sv_syntax::Instantiation) -> Converted<()> {
    let mut ranges = native
        .instances
        .iter()
        .map(|instance| instance.tokens)
        .collect::<Vec<_>>();
    ranges.sort_unstable_by_key(|range| range.start);
    if ranges.iter().any(|range| {
        range.start >= range.end
            || range.start < native.tokens.start
            || range.end > native.tokens.end
    }) || ranges.windows(2).any(|pair| pair[0].end > pair[1].start)
    {
        return Err(AnalyzerError::Parse(
            "native instance membership is invalid".into(),
        ));
    }
    Ok(())
}

pub(super) fn instance_ranges(
    native: &celox_sv_syntax::InstanceDeclarator,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Result<Vec<UnpackedRange>, AnalyzerError> {
    constant_binding::unpacked_ranges(&native.dimensions, tree, session, env, aliases)
}

pub(super) fn expr_ident_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(name) => Some(name.clone()),
        _ => None,
    }
}

/// An escaped identifier is the same identifier as its text without the
/// backslash (IEEE 1800-2023 5.6.1): `\a` names `a`. One that cannot be written
/// without escaping, such as `\a.b`, keeps its backslash.
pub(crate) fn normalize_identifier(text: &str) -> String {
    match text.strip_prefix('\\') {
        Some(name)
            if name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') =>
        {
            name.to_string()
        }
        _ => text.to_string(),
    }
}

/// The shape of the unpacked array that a call of a function returns (IEEE
/// 1800-2023 13.4.1), from the declared result type of the function. A
/// parameter argument is bound before the functions of the scope are
/// collected, when the declaration of a function of the module gives it.
fn call_result_shape(
    id: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Option<VariableDimensions> {
    use celox_sv_syntax::{ExprKind, SubroutineKind};
    let native = tree.declarations();
    let ExprKind::Call { expression, .. } = native.expression(id).kind else {
        return None;
    };
    let ExprKind::Name(name) = &native
        .expression(expression_binding::ungroup(expression, native))
        .kind
    else {
        return None;
    };
    let name = normalize_identifier(&name.spelling);
    let ty = match dims
        .function_return_types
        .get(&name)
        .and_then(|metadata| metadata.pattern_type.clone())
    {
        Some(ty) => ty,
        None => {
            let subroutine = tree
                .root_unit()
                .ok()?
                .subroutines
                .iter()
                .map(|id| native.subroutine(*id))
                .find(|subroutine| {
                    subroutine.kind == SubroutineKind::Function
                        && subroutine.scope.is_empty()
                        && normalize_identifier(&subroutine.name.spelling) == name
                })?;
            type_binding::bind(subroutine.return_type?, tree, session, dims)
                .ok()
                .flatten()?
        }
    };
    let shape = procedural::dimensions_from_type(&ty);
    (!shape.unpacked.is_empty()).then_some(shape)
}

/// Keep whole-array conditional arguments out of the scalar constant path.
/// Traverse branch types iteratively, including branches a constant condition
/// does not select. Array sizes and integral element types must agree.
pub(super) fn array_argument_shape(
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Option<VariableDimensions> {
    use celox_sv_syntax::{ExprId, ExprKind};
    enum Job {
        Enter(ExprId),
        Conditional,
    }
    let native = tree.declarations();
    let mut jobs = vec![Job::Enter(root)];
    let mut values = Vec::<VariableDimensions>::new();
    while let Some(job) = jobs.pop() {
        match job {
            Job::Enter(id) => {
                let id = expression_binding::ungroup(id, native);
                if let ExprKind::Conditional {
                    then_expression,
                    else_expression,
                    ..
                } = native.expression(id).kind
                {
                    jobs.push(Job::Conditional);
                    jobs.push(Job::Enter(else_expression));
                    jobs.push(Job::Enter(then_expression));
                } else {
                    values.push(
                        array_compatibility::native_unpacked_shape(id, tree, session, dims)
                            .or_else(|| call_result_shape(id, tree, session, dims))?,
                    );
                }
            }
            Job::Conditional => {
                let right = values.pop()?;
                let left = values.pop()?;
                let left_type =
                    array_compatibility::unpacked_array_type(&tree.functions, &left, dims)?;
                let right_type =
                    array_compatibility::unpacked_array_type(&tree.functions, &right, dims)?;
                if left_type != right_type
                    || left.packed != right.packed
                    || left.members != right.members
                {
                    return None;
                }
                values.push(left);
            }
        }
    }
    values.pop()
}

/// One `.name(expr)` binding: a `parameter type` when `expr` is a data type or
/// the name of a type, a value otherwise.
fn parameter_override(
    name: String,
    native: celox_sv_syntax::ExprId,
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    packed_dimensions: &PackedDimensions,
    bind_values: bool,
) -> Result<ParameterOverride, AnalyzerError> {
    use celox_sv_syntax::{DimensionKind, ExprKind};
    let declarations = syntax_tree.declarations();
    let root = expression_binding::ungroup(native, declarations);
    if matches!(declarations.expression(root).kind, ExprKind::Type(_)) {
        let ty = type_binding::argument_type(
            native,
            syntax_tree,
            session,
            &packed_dimensions.const_env,
            &packed_dimensions.type_aliases,
        )?
        .and_then(|ty| {
            type_parameters::resolve(&syntax_tree.functions, ty, &packed_dimensions.const_env)
        })
        .ok_or_else(|| unsupported(format!("parameter type override `{name}`")))?;
        return Ok(ParameterOverride::type_override(name, ty));
    }
    let mut pending = vec![native];
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        if matches!(declarations.expression(id).kind, ExprKind::Pattern { .. }) {
            let pattern =
                bound_patterns::from_param(native, syntax_tree, session, packed_dimensions)?;
            let mut bound = ParameterOverride::new(name, None);
            bound.bound_pattern = Some(pattern);
            return Ok(bound);
        }
        pending.extend(parameters::skipped::children(id, declarations));
    }
    if let Some(shape) = array_argument_shape(native, syntax_tree, session, packed_dimensions) {
        let selected =
            array_parameters::selected_branch(root, syntax_tree, session, packed_dimensions)?;
        let pattern = bound_patterns::from_array_parameter(
            selected,
            syntax_tree,
            session,
            packed_dimensions,
            &shape,
        )?;
        let mut bound = ParameterOverride::new(name, None);
        bound.bound_pattern = Some(pattern);
        return Ok(bound);
    }
    if let Some((type_name, dimensions)) = type_binding::argument_alias(native, declarations)
        && packed_dimensions.type_aliases.contains_key(&type_name)
        && !dimensions.is_empty()
    {
        if dimensions
            .iter()
            .any(|dimension| !matches!(dimension.kind, DimensionKind::Range { .. }))
        {
            return Err(unsupported("indexed range on type parameter argument"));
        }
        let ty = type_binding::argument_type(
            native,
            syntax_tree,
            session,
            &packed_dimensions.const_env,
            &packed_dimensions.type_aliases,
        )?
        .and_then(|ty| {
            type_parameters::resolve(&syntax_tree.functions, ty, &packed_dimensions.const_env)
        })
        .ok_or_else(|| unsupported("type parameter packed bounds"))?;
        return Ok(ParameterOverride::type_override(name, ty));
    }
    if matches!(
        declarations.expression(root).kind,
        ExprKind::MinTypMax { .. }
    ) {
        return Err(unsupported("parameter override expression"));
    }
    let value = if matches!(&declarations.expression(root).kind, ExprKind::Name(name) if name.spelling == "$")
    {
        ConstExpr::Literal("$".into())
    } else if bind_values {
        if let Some((type_name, dimensions)) = type_binding::argument_alias(native, declarations)
            && dimensions.is_empty()
            && let Some(ty) = packed_dimensions.type_aliases.get(&type_name)
        {
            let ty = type_parameters::resolve(
                &syntax_tree.functions,
                ty.clone(),
                &packed_dimensions.const_env,
            )
            .ok_or_else(|| unsupported(format!("parameter type override `{name}`")))?;
            return Ok(ParameterOverride::type_override(name, ty));
        }
        let value = expression_binding::value(
            native,
            declarations,
            syntax_tree,
            session,
            packed_dimensions,
        )?;
        match expr_to_const(session, value) {
            Some(value) => value,
            // A value the binder gives as a concatenation or resize, such as
            // one that selects a field of a wide constant, is bound as a
            // constant expression, as a module parameter override is.
            None => constant_binding::value_expression(
                native,
                declarations,
                syntax_tree,
                session,
                packed_dimensions,
            )?
            .ok_or_else(|| unsupported("constant interface parameter override"))?,
        }
    } else {
        constant_binding::value_expression(
            native,
            declarations,
            syntax_tree,
            session,
            packed_dimensions,
        )?
        .ok_or_else(|| unsupported("parameter override expression"))?
    };
    // A bare name that denotes a type (a typedef, or the instantiating
    // module's own `parameter type`) is passed on as that type.
    if let ConstExpr::Ident(type_name) = &value
        && let Some(r#type) = packed_dimensions.type_aliases.get(type_name)
    {
        let ty = type_parameters::resolve(
            &syntax_tree.functions,
            r#type.clone(),
            &packed_dimensions.const_env,
        )
        .ok_or_else(|| AnalyzerError::Unsupported(format!("parameter type override `{name}`")))?;
        return Ok(ParameterOverride::type_override(name, ty));
    }
    Ok(ParameterOverride::new(name, Some(value)))
}

/// `value` with each call of a constant function (IEEE 1800-2023 13.4.3)
/// whose arguments are constants replaced by its result, of the function's
/// return type. The instance is specialized by the value the call returns
/// in the scope that gives the override; a call that does not evaluate
/// stays.
fn constant_calls_folded(
    value: ConstExpr,
    functions: &const_functions::ConstantFunctions,
    constants: &ConstEnv,
) -> ConstExpr {
    let fold =
        |value: Box<ConstExpr>| Box::new(constant_calls_folded(*value, functions, constants));
    match value {
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => value,
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: fold(expr),
            bit: fold(bit),
        },
        ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => ConstExpr::SelectRange {
            expr: fold(expr),
            msb: fold(msb),
            lsb: fold(lsb),
            signed,
        },
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => ConstExpr::Resize {
            expr: fold(expr),
            width,
            signed,
        },
        ConstExpr::Function { callee, args, site } => {
            let args: Vec<_> = args
                .into_iter()
                .map(|arg| constant_calls_folded(arg, functions, constants))
                .collect();
            if let Callee::Subroutine(name) = &callee {
                let ir_args: Vec<crate::ir::ConstExpr> =
                    args.iter().cloned().map(Into::into).collect();
                if let Some(literal) =
                    const_functions::eval_call_literal(functions, name, &ir_args, constants)
                {
                    return ConstExpr::Literal(typecheck::format_integral_literal_binary(&literal));
                }
            }
            ConstExpr::Function { callee, args, site }
        }
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op,
            expr: fold(expr),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: fold(left),
            op,
            right: fold(right),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: fold(condition),
            then_expr: fold(then_expr),
            else_expr: fold(else_expr),
        },
    }
}
