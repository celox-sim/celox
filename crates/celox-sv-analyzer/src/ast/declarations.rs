//! Module declarations, ports, signals, and syntax-node lookup.

use super::*;

pub(super) fn ports_from_scope(
    syntax_tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    selected: Option<&port_owners::Selection<'_>>,
) -> Result<Vec<Port>, AnalyzerError> {
    let mut ports = Vec::new();
    let mut inherited_direction = PortDirection::Unspecified;
    let mut inherited_type = Type::implicit();
    let mut inherited_variable = false;
    for native in port_owners::scope(selected, syntax_tree)? {
        let direction = native
            .direction
            .map(type_binding::port_direction)
            .unwrap_or(inherited_direction);
        let explicit = matches!(native.kind, celox_sv_syntax::PortKind::Explicit { .. });
        // An output with an explicit data type defaults to a variable;
        // input/inout and implicitly typed outputs default to nets unless
        // `var` is present (IEEE 1800-2023 23.2.2.3).
        let typed_output = direction == PortDirection::Output
            && native.net_type.is_none()
            && native.data_type.is_some_and(|id| {
                !matches!(
                    syntax_tree.declarations().data_type(id).kind,
                    celox_sv_syntax::DataTypeKind::Implicit
                )
            });
        let base = match type_binding::port_type(native, syntax_tree, const_env, type_aliases)? {
            Some(ty) => ty,
            None if native.data_type.is_none() => inherited_type.clone(),
            None => return Err(unsupported("unsupported port data type")),
        };
        inherited_direction = direction;
        inherited_type = base.clone();
        let ty = type_with_unpacked_ranges(
            base,
            constant_binding::unpacked_ranges(
                &native.dimensions,
                syntax_tree,
                const_env,
                type_aliases,
            )?,
        );
        let default = native
            .default
            .map(|value| {
                if direction != PortDirection::Input {
                    return Err(unsupported("default value on a non-input port"));
                }
                if !ty.unpacked_ranges().is_empty() {
                    let name = native_port_name(native)?;
                    let mut dimensions =
                        PackedDimensions::new(HashMap::default(), const_env, type_aliases);
                    dimensions.insert(name.clone(), procedural::dimensions_from_type(&ty));
                    let expression = expression_binding::expression(
                        value,
                        syntax_tree.declarations(),
                        &LValue::Ident(name),
                        syntax_tree,
                        &dimensions,
                    )?;
                    let value = fold_const_integral_expr_preserving_mask(
                        &syntax_tree.state,
                        expression,
                        const_env,
                    );
                    return expr_to_const(&syntax_tree.state, value)
                        .ok_or_else(|| unsupported("nonconstant unpacked input port default"));
                }
                constant_binding::expression(
                    value,
                    syntax_tree.declarations(),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )?
                .ok_or_else(|| unsupported("nonconstant input port default"))
            })
            .transpose()?;
        let variable = native.variable
            || typed_output
            || (direction == PortDirection::Output
                && native.net_type.is_none()
                && native.direction.is_none()
                && native.data_type.is_none()
                && inherited_variable);
        inherited_variable = variable;
        let mut port = Port::new(
            native_port_name(native)?,
            direction,
            ty,
            !explicit && !variable,
        )
        .with_default(default);
        port.randomizable = variable && direction != PortDirection::Input;
        ports.push(port);
    }
    Ok(ports)
}

fn native_port_name(port: &celox_sv_syntax::Port) -> Converted<String> {
    match &port.kind {
        celox_sv_syntax::PortKind::Named(name)
        | celox_sv_syntax::PortKind::Explicit { name, .. } => Ok(name.spelling.clone()),
        _ => Err(AnalyzerError::Parse(
            "native ANSI port has a non-ANSI name".into(),
        )),
    }
}

/// The positional interface of a module: its ports in declaration order and
/// its overridable value parameters: in the header when present, otherwise body.
pub(super) fn module_interface_from_scope(
    syntax_tree: &AnalysisContext<'_>,
) -> Result<ModuleInterface, AnalyzerError> {
    let ports = port_owners::scope(None, syntax_tree)?
        .into_iter()
        .filter(|port| matches!(port.kind, celox_sv_syntax::PortKind::Named(_)))
        .map(native_port_name)
        .collect::<Converted<Vec<_>>>()?;
    let mut parameters = Vec::new();
    if let Some(header) = syntax_tree
        .scope_handle()
        .unit(syntax_tree.declarations())
        .and_then(|unit| unit.parameters.as_ref())
    {
        parameters.extend(
            header
                .iter()
                .filter(|parameter| !parameter.local)
                .map(|parameter| instances::normalize_identifier(&parameter.name.spelling)),
        );
    } else {
        for id in syntax_tree.body_membership()? {
            if let celox_sv_syntax::BodyItemKind::Parameter(id) =
                syntax_tree.declarations().body_item(id).kind
            {
                parameters.extend(
                    syntax_tree
                        .declarations()
                        .parameter_declaration(id)
                        .parameters
                        .iter()
                        .filter(|parameter| {
                            !parameter.local
                                && matches!(
                                    parameter.kind,
                                    celox_sv_syntax::ParameterKind::Value { .. }
                                )
                        })
                        .map(|parameter| instances::normalize_identifier(&parameter.name.spelling)),
                );
            }
        }
    }
    Ok(ModuleInterface { ports, parameters })
}

pub(super) fn parameters_from_scope(
    syntax_tree: &AnalysisContext<'_>,
    type_aliases: &HashMap<String, Type>,
    base_const_env: &HashMap<String, i128>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Result<Vec<Parameter>, AnalyzerError> {
    let mut parameters = Vec::new();
    let mut environment =
        parameters::ParameterEnvironment::new(&syntax_tree.state, &parameters, base_const_env);
    let header = syntax_tree
        .scope_handle()
        .unit(syntax_tree.declarations())
        .and_then(|unit| unit.parameters.as_ref());
    let body_parameters_are_local = header.is_some();
    if let Some(header) = header {
        let values = header
            .iter()
            .filter(|parameter| {
                matches!(parameter.kind, celox_sv_syntax::ParameterKind::Value { .. })
            })
            .collect::<Vec<_>>();
        for values in values.chunk_by(|a, b| {
            let celox_sv_syntax::ParameterKind::Value {
                data_type: left, ..
            } = a.kind
            else {
                unreachable!()
            };
            let celox_sv_syntax::ParameterKind::Value {
                data_type: right, ..
            } = b.kind
            else {
                unreachable!()
            };
            left == right && a.local == b.local
        }) {
            parameters::parameters_from_native_with_environment(
                values,
                syntax_tree,
                &mut parameters,
                values[0].local,
                base_const_env,
                type_aliases,
                parameter_overrides,
                &mut environment,
            )?;
        }
    }
    for id in syntax_tree.body_membership()? {
        let celox_sv_syntax::BodyItemKind::Parameter(id) =
            syntax_tree.declarations().body_item(id).kind
        else {
            continue;
        };
        let values = syntax_tree
            .declarations()
            .parameter_declaration(id)
            .parameters
            .iter()
            .filter(|parameter| {
                matches!(parameter.kind, celox_sv_syntax::ParameterKind::Value { .. })
            })
            .collect::<Vec<_>>();
        let local =
            body_parameters_are_local || values.first().is_some_and(|parameter| parameter.local);
        parameters::parameters_from_native_with_environment(
            &values,
            syntax_tree,
            &mut parameters,
            local,
            base_const_env,
            type_aliases,
            parameter_overrides,
            &mut environment,
        )?;
    }

    Ok(parameters)
}

pub(super) fn signals_from_scope(
    syntax_tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<Vec<Signal>, AnalyzerError> {
    signals_from_scope_with_declaration_ranges(syntax_tree, const_env, type_aliases, false)
}

pub(super) fn signals_from_scope_with_declaration_ranges(
    syntax_tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    preserve_parameters: bool,
) -> Result<Vec<Signal>, AnalyzerError> {
    let mut signals = Vec::new();
    for mut item in generate::items(syntax_tree, const_env, type_aliases)? {
        let _interface_scope = item.interface_scope();
        if preserve_parameters {
            let names = item.env.keys().cloned().collect::<Vec<_>>();
            for name in names {
                item.env.remove(&local_parameter_marker(&name));
            }
        }
        let _constant_functions = item.enter();
        let start = signals.len();
        signals_from_body_item(
            item.id,
            syntax_tree,
            &item.type_aliases(type_aliases),
            &item.env,
            None,
            &mut signals,
        )?;
        for signal in &mut signals[start..] {
            signal.name = item.name(&signal.name);
        }
    }
    signals.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(name) = signals
        .windows(2)
        .find(|pair| pair[0].name == pair[1].name)
        .map(|pair| pair[0].name.clone())
    {
        return Err(AnalyzerError::Unsupported(format!(
            "duplicate internal signal `{name}`"
        )));
    }
    Ok(signals)
}

pub(super) fn signals_from_body_item(
    id: celox_sv_syntax::BodyItemId,
    tree: &AnalysisContext<'_>,
    aliases: &HashMap<String, Type>,
    env: &HashMap<String, i128>,
    selected_name: Option<&str>,
    signals: &mut Vec<Signal>,
) -> Converted<()> {
    use celox_sv_syntax::{BodyItemKind, DataDeclaration};
    let native = tree.declarations();
    let owner = native.body_item(id);
    let payload = match owner.kind {
        BodyItemKind::Data(id) => match native.data_declaration(id) {
            DataDeclaration::Variables(value) | DataDeclaration::Constructed(value) => {
                Some(value.tokens)
            }
            DataDeclaration::Unconverted(tokens) => Some(*tokens),
        },
        BodyItemKind::Net(id) => Some(native.net_declaration(id).tokens),
        BodyItemKind::Instance(id) => Some(native.instantiation(id).tokens),
        _ => None,
    };
    if payload
        .is_some_and(|tokens| tokens.start < owner.tokens.start || tokens.end > owner.tokens.end)
    {
        return Err(AnalyzerError::Parse(
            "native declaration payload is outside its body owner".into(),
        ));
    }
    let mut declared = match owner.kind {
        BodyItemKind::Data(id) => match tree.declarations().data_declaration(id) {
            DataDeclaration::Variables(value) | DataDeclaration::Constructed(value) => {
                if !value.var
                    && !value.constant
                    && matches!(
                        tree.declarations().data_type(value.data_type).kind,
                        celox_sv_syntax::DataTypeKind::Named(_)
                    )
                {
                    type_binding::alias_signals(value, tree, env, aliases, selected_name)?
                } else {
                    type_binding::local_signals(value, tree, env, aliases, selected_name)?
                }
            }
            DataDeclaration::Unconverted(_) => {
                return Err(AnalyzerError::Parse(
                    "ordinary native data declaration is unexpectedly unconverted".into(),
                ));
            }
        },
        BodyItemKind::Net(id) => type_binding::net_signals(
            tree.declarations().net_declaration(id),
            tree,
            env,
            aliases,
            selected_name,
        )?,
        BodyItemKind::Instance(id) => {
            let declaration = tree.declarations().instantiation(id);
            let name = instances::normalize_identifier(&declaration.name.spelling);
            let mut values = Vec::new();
            if let Some(base) = aliases.get(&name) {
                let context = PackedDimensions::new(HashMap::default(), env, aliases);
                for variable in &declaration.instances {
                    let name = instances::normalize_identifier(&variable.name.spelling);
                    if selected_name.is_some_and(|selected| selected != name) {
                        continue;
                    }
                    let ty = type_with_unpacked_ranges(
                        base.clone(),
                        constant_binding::bind_unpacked_ranges(
                            &variable.dimensions,
                            tree,
                            &context,
                        )?,
                    );
                    values.push(Signal::new(name, ty));
                }
            }
            values
        }
        _ => Vec::new(),
    };
    substitute_signal_local_constants(&mut declared, env);
    signals.extend(declared);
    Ok(())
}

fn substitute_signal_local_constants(signals: &mut [Signal], const_env: &HashMap<String, i128>) {
    for signal in signals {
        for range in &mut signal.r#type.packed_ranges {
            range.left = substitute_dimension_constants(range.left.clone(), const_env);
            range.right = substitute_dimension_constants(range.right.clone(), const_env);
        }
        for range in &mut signal.r#type.unpacked_ranges {
            range.left = substitute_dimension_constants(range.left.clone(), const_env);
            range.right = substitute_dimension_constants(range.right.clone(), const_env);
            range.size = range
                .size
                .take()
                .map(|size| substitute_dimension_constants(size, const_env));
        }
    }
}
