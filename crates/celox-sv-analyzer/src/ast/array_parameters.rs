//! Parameters of unpacked array type (IEEE 1800-2023 6.20.2), such as the
//! table `localparam logic [2:0] T [4] = '{...};`, and parameters whose value
//! is an assignment pattern, such as a packed structure `S'{m: 1, k: 2}`.
//! Such a parameter becomes a module variable whose initial value is the
//! pattern; nothing else writes it, so it holds that value throughout the
//! simulation.

use super::*;

/// A parameter of unpacked array type, as the variable that holds it.
pub(super) struct ArrayParameter {
    pub signal: Signal,
    native_pattern: Option<celox_sv_syntax::ExprId>,
    bound: Option<BoundPattern>,
}

/// An authored unpacked parameter or an assignment-pattern value.
pub(super) fn is_array_parameter(
    parameter: &celox_sv_syntax::Parameter,
    native: &celox_sv_syntax::Declarations,
) -> bool {
    let celox_sv_syntax::ParameterKind::Value {
        dimensions,
        default,
        ..
    } = &parameter.kind
    else {
        return false;
    };
    !dimensions.is_empty()
        || default.is_some_and(|id| {
            matches!(
                native
                    .expression(expression_binding::ungroup(id, native))
                    .kind,
                celox_sv_syntax::ExprKind::Pattern { .. }
            )
        })
}

fn declared_type(
    native: &celox_sv_syntax::Parameter,
    tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Converted<Option<Type>> {
    let celox_sv_syntax::ParameterKind::Value { data_type, .. } = native.kind else {
        return Err(unsupported(
            "type parameter used as an array value parameter",
        ));
    };
    type_binding::bind(
        data_type,
        tree,
        &PackedDimensions::new(HashMap::default(), const_env, type_aliases),
    )
}

/// A scalar value override replaces a packed pattern default. Keep unpacked
/// values and caller-bound patterns on the aggregate binding path.
pub(super) fn uses_signal_representation(
    parameter: &celox_sv_syntax::Parameter,
    tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<bool> {
    if !is_array_parameter(parameter, tree.declarations()) {
        return Ok(false);
    }
    let name = instances::normalize_identifier(&parameter.name.spelling);
    if !parameter.local
        && tree.parameter_overrides.contains_key(&name)
        && matches!(&parameter.kind, celox_sv_syntax::ParameterKind::Value { dimensions, .. } if dimensions.is_empty())
        && declared_type(parameter, tree, const_env, aliases)?
            .is_some_and(|ty| ty.unpacked_ranges.is_empty())
    {
        return Ok(false);
    }
    Ok(true)
}

/// Array parameters declared directly in the selected native unit/scope.
pub(super) fn array_parameters_from_scope(
    tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Converted<Vec<ArrayParameter>> {
    let mut parameters = Vec::new();
    if let Ok(unit) = tree.root_unit() {
        let authored = unit
            .parameters
            .iter()
            .flatten()
            .filter(|parameter| {
                matches!(parameter.kind, celox_sv_syntax::ParameterKind::Value { .. })
            })
            .collect::<Vec<_>>();
        parameters.extend(array_parameters_from_declaration(
            &authored,
            tree,
            const_env,
            type_aliases,
            &mut |_| {},
        )?);
    }
    for id in tree.body_membership()? {
        if let celox_sv_syntax::BodyItemKind::Parameter(id) = tree.declarations().body_item(id).kind
        {
            let authored = tree
                .declarations()
                .parameter_declaration(id)
                .parameters
                .iter()
                .filter(|parameter| {
                    matches!(parameter.kind, celox_sv_syntax::ParameterKind::Value { .. })
                })
                .collect::<Vec<_>>();
            parameters.extend(array_parameters_from_declaration(
                &authored,
                tree,
                const_env,
                type_aliases,
                &mut |_| {},
            )?);
        }
    }
    Ok(parameters)
}

/// A scope scan installs each array parameter declaration in source order.
fn array_parameters_from_declaration(
    authored: &[&celox_sv_syntax::Parameter],
    tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    publish: &mut impl FnMut(&ArrayParameter),
) -> Result<Vec<ArrayParameter>, AnalyzerError> {
    let mut parameters = Vec::new();
    for &native in authored {
        let bound = scope::imported(&tree.state)
            .pattern_parameter_bindings
            .get(&native.id)
            .cloned();
        if !uses_signal_representation(native, tree, const_env, type_aliases)? && bound.is_none() {
            continue;
        }
        let name = instances::normalize_identifier(&native.name.spelling);
        let element = declared_type(native, tree, const_env, type_aliases)?.ok_or_else(|| {
            AnalyzerError::Unsupported(format!("type of array parameter `{name}`"))
        })?;
        let celox_sv_syntax::ParameterKind::Value { dimensions, .. } = &native.kind else {
            return Err(AnalyzerError::Parse(
                "native array parameter kind disagrees".into(),
            ));
        };
        let ranges = dimensions
            .iter()
            .map(|dimension| {
                constant_binding::unpacked_dimension(dimension, tree, const_env, type_aliases)
            })
            .collect::<Converted<Vec<_>>>()?;
        let pattern = parameters::ownership::default(native).filter(|id| {
            matches!(
                tree.declarations()
                    .expression(expression_binding::ungroup(*id, tree.declarations()))
                    .kind,
                celox_sv_syntax::ExprKind::Pattern { .. }
            )
        });
        if pattern.is_none() && bound.is_none() {
            return Err(AnalyzerError::Unsupported(format!(
                "value of array parameter `{name}`"
            )));
        }
        // A supplied caller-bound pattern takes precedence over the authored
        // default. Only defaults actually consumed here require their native ID.
        let native_pattern = if bound.is_none() { pattern } else { None };
        let parameter = ArrayParameter {
            signal: Signal::new(name, type_with_unpacked_ranges(element, ranges)),
            native_pattern,
            bound,
        };
        // Publish before the next declarator can query this array's shape.
        publish(&parameter);
        parameters.push(parameter);
    }
    Ok(parameters)
}

impl ArrayParameter {
    /// The `initial` block that gives the variable its value.
    pub(super) fn initial_process(
        &self,
        tree: &AnalysisContext<'_>,
        dims: &PackedDimensions,
    ) -> Result<InitialProcess, AnalyzerError> {
        let name = self.signal.name().to_string();
        let shape = procedural::dimensions_from_type(self.signal.r#type());
        let value = if let Some(bound) = &self.bound {
            bound.lower(&tree.state, &shape, dims)?
        } else {
            match self.native_pattern {
                Some(root) => match patterns::native_expression(root, &shape, tree, dims)? {
                    Some(value) => value,
                    None => return Err(unsupported("array parameter pattern target")),
                },
                None => {
                    return Err(AnalyzerError::Parse(
                        "native array parameter default is missing".into(),
                    ));
                }
            }
        };
        Ok(InitialProcess {
            creation_order: 0,
            site: None,
            condition: None,
            body: vec![Stmt::Assign {
                lhs: LValue::Ident(name),
                rhs: value,
                nonblocking: false,
            }],
            initializer: false,
            synthetic: true,
            repeats: false,
        })
    }
}
