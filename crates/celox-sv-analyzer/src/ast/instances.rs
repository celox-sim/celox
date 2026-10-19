//! Module instantiations, parameter overrides, and port connections.

use super::*;
mod native;

pub(super) fn instances_from_scope(
    syntax_tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    interfaces: &InterfaceLookup<'_>,
) -> Result<Vec<Instance>, AnalyzerError> {
    let type_aliases = type_aliases_from_scope(syntax_tree)?;
    let active = generate::items(syntax_tree, const_env, &type_aliases)?;
    let mut instances = Vec::new();
    let mut views = generate::ScopeViews::new(packed_dimensions);
    for item in &active {
        let _interface_scope = item.interface_scope();
        let celox_sv_syntax::BodyItemKind::Instance(native) =
            syntax_tree.declarations().body_item(item.id).kind
        else {
            continue;
        };
        if scope::imported(&syntax_tree.state)
            .interface_declarations
            .contains(&item.id)
        {
            continue;
        }
        let native = syntax_tree.declarations().instantiation(native);
        let _constant_functions = item.enter();
        let start = instances.len();
        let dimensions = views.dimensions(item);
        instances_from_native_declaration(
            item.id,
            native,
            None,
            syntax_tree,
            &item.env,
            dimensions,
            interfaces,
            &mut instances,
        )?;
        for instance in &mut instances[start..] {
            instance.name = item.instance_name(&instance.name);
            for connection in &mut instance.port_connections {
                if let Some(expr) = &mut connection.actual_expr {
                    *expr = substitute_expr_constants_with_parameter_literals(
                        &syntax_tree.state,
                        expr.clone(),
                        &item.env,
                        &item.literals,
                    );
                    item.expr(expr);
                }
            }
        }
    }
    instances.retain(|instance| !type_aliases.contains_key(instance.module_name()));
    Ok(instances)
}

fn instances_from_native_declaration(
    id: celox_sv_syntax::BodyItemId,
    native: &celox_sv_syntax::Instantiation,
    condition: Option<ConstExpr>,
    syntax_tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
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
        interface,
        packed_dimensions,
        false,
    )?;
    for override_ in &mut parameter_overrides {
        if let Some(value) = override_.value.take() {
            let value = substitute_typed_parameter_literals(
                value,
                const_env,
                &parameter_types_from_const_env(const_env),
            );
            let value = expr_to_const(
                &syntax_tree.state,
                substitute_expr_idents(
                    const_expr_to_expr(value),
                    &packed_dimensions.parameter_values,
                ),
            )
            .ok_or_else(|| {
                AnalyzerError::Unsupported(format!(
                    "constant module parameter override `{}`",
                    override_.name
                ))
            })?;
            override_.value = Some(substitute_const_expr_constants_preserving_enum_types(
                value, const_env,
            ));
        }
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
            packed_dimensions,
            interface,
        )?;
        for connection in &mut port_connections {
            connection.actual_expr = connection.actual_expr.take().map(|expr| {
                substitute_expr_constants_with_parameter_literals(
                    &syntax_tree.state,
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
                const_env,
                &packed_dimensions.type_aliases,
            )?;
            let [range] = ranges.as_slice() else {
                return Err(AnalyzerError::Unsupported(
                    "multidimensional module instance array".to_string(),
                ));
            };
            let left = eval_ast_const_expr(&syntax_tree.state, range.left(), const_env);
            let right = eval_ast_const_expr(&syntax_tree.state, range.right(), const_env);
            let bounds = left
                .zip(right)
                .filter(|(left, right)| left.abs_diff(*right) < 4096)
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("module instance array bounds".to_string())
                })?;
            Some(bounds)
        };
        let imported = scope::imported(&syntax_tree.state);
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
            packed_dimensions,
            bind_values,
        )?);
    }
    Ok(overrides)
}

pub(super) fn port_connections(
    key: native_interfaces::InstanceKey,
    native: &celox_sv_syntax::InstanceDeclarator,
    syntax_tree: &AnalysisContext<'_>,
    packed_dimensions: &PackedDimensions,
    interface: Option<&ModuleInterface>,
) -> Result<Vec<PortConnection>, AnalyzerError> {
    native::connections(key, native, syntax_tree, packed_dimensions, interface)
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
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Result<Vec<UnpackedRange>, AnalyzerError> {
    native_constants::unpacked_ranges(&native.dimensions, tree, env, aliases)
}

// Child port directions are resolved by the frontend adapter. Defer its net
// driver check for selected/concatenated connections as well as whole nets.
pub(super) fn collect_connected_nets<'a>(expr: &'a Expr, names: &mut HashSet<&'a str>) {
    match expr {
        Expr::Ident(actual) => {
            names.insert(actual);
        }
        Expr::Select { expr, .. }
        | Expr::Resize { expr, .. }
        | Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr,
        } => collect_connected_nets(expr, names),
        Expr::Concat(parts) => {
            for part in parts {
                collect_connected_nets(part, names);
            }
        }
        _ => {}
    }
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

/// One `.name(expr)` binding: a `parameter type` when `expr` is a data type or
/// the name of a type, a value otherwise.
fn parameter_override(
    name: String,
    native: celox_sv_syntax::ExprId,
    syntax_tree: &AnalysisContext<'_>,
    packed_dimensions: &PackedDimensions,
    bind_values: bool,
) -> Result<ParameterOverride, AnalyzerError> {
    use celox_sv_syntax::{DimensionKind, ExprKind};
    let declarations = syntax_tree.declarations();
    let root = native_expressions::ungroup(native, declarations);
    if matches!(declarations.expression(root).kind, ExprKind::Type(_)) {
        let ty = native_types::argument_type(
            native,
            syntax_tree,
            &packed_dimensions.const_env,
            &packed_dimensions.type_aliases,
        )?
        .and_then(|ty| {
            type_parameters::resolve(&syntax_tree.state, ty, &packed_dimensions.const_env)
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
            let pattern = bound_patterns::from_param(native, syntax_tree, packed_dimensions)?;
            let mut bound = ParameterOverride::new(name, None);
            bound.bound_pattern = Some(pattern);
            return Ok(bound);
        }
        pending.extend(parameters::skipped::children(id, declarations));
    }
    if let Some((type_name, dimensions)) = native_types::argument_alias(native, declarations)
        && packed_dimensions.type_aliases.contains_key(&type_name)
        && !dimensions.is_empty()
    {
        if dimensions
            .iter()
            .any(|dimension| !matches!(dimension.kind, DimensionKind::Range { .. }))
        {
            return Err(unsupported("indexed range on type parameter argument"));
        }
        let ty = native_types::argument_type(
            native,
            syntax_tree,
            &packed_dimensions.const_env,
            &packed_dimensions.type_aliases,
        )?
        .and_then(|ty| {
            type_parameters::resolve(&syntax_tree.state, ty, &packed_dimensions.const_env)
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
        if let Some((type_name, dimensions)) = native_types::argument_alias(native, declarations)
            && dimensions.is_empty()
            && let Some(ty) = packed_dimensions.type_aliases.get(&type_name)
        {
            let ty = type_parameters::resolve(
                &syntax_tree.state,
                ty.clone(),
                &packed_dimensions.const_env,
            )
            .ok_or_else(|| unsupported(format!("parameter type override `{name}`")))?;
            return Ok(ParameterOverride::type_override(name, ty));
        }
        expr_to_const(
            &syntax_tree.state,
            native_expressions::value(native, declarations, syntax_tree, packed_dimensions)?,
        )
        .ok_or_else(|| unsupported("constant interface parameter override"))?
    } else {
        native_constants::value_expression(native, declarations, syntax_tree, packed_dimensions)?
            .ok_or_else(|| unsupported("parameter override expression"))?
    };
    // A bare name that denotes a type (a typedef, or the instantiating
    // module's own `parameter type`) is passed on as that type.
    if let ConstExpr::Ident(type_name) = &value
        && let Some(r#type) = packed_dimensions.type_aliases.get(type_name)
    {
        let ty = type_parameters::resolve(
            &syntax_tree.state,
            r#type.clone(),
            &packed_dimensions.const_env,
        )
        .ok_or_else(|| AnalyzerError::Unsupported(format!("parameter type override `{name}`")))?;
        return Ok(ParameterOverride::type_override(name, ty));
    }
    Ok(ParameterOverride::new(name, Some(value)))
}
