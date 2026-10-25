//! Parameter environments, enum constants, and typed parameter substitution.

use super::*;
mod declaration_types;
pub(super) mod ownership;
pub(super) mod skipped;

/// Presence also distinguishes a bounded value parameter from a variable or
/// enum constant; the payload is 1 for `$` and 0 for a bounded value.
pub(crate) fn unbounded_parameter_marker(name: &str) -> String {
    format!("__parameter::unbounded::{name}")
}

/// All constant/type projections of one binding. Include the old and new
/// packed bounds so scope exit can remove newly introduced dimension keys.
pub(super) fn binding_keys(
    name: &str,
    env: &HashMap<String, i128>,
    packed_count: usize,
) -> Vec<String> {
    let count = env
        .get(&parameter_dimensions_marker(name))
        .and_then(|count| usize::try_from(*count).ok())
        .unwrap_or(0)
        .max(packed_count);
    let mut keys = vec![
        name.to_string(),
        parameter_marker(name),
        local_parameter_marker(name),
        unbounded_parameter_marker(name),
        parameter_width_marker(name),
        parameter_signed_marker(name),
        parameter_rank_marker(name),
        parameter_dimensions_marker(name),
        parameter_signed_element_marker(name),
        enum_marker(name),
        variable_bits_marker(name),
        variable_size_marker(name),
        variable_signed_marker(name),
        variable_dimensions_marker(name),
        variable_unpacked_dimensions_marker(name),
    ];
    for index in 0..count {
        keys.push(parameter_dimension_marker(name, index, "left"));
        keys.push(parameter_dimension_marker(name, index, "right"));
    }
    if let Some(count) = env.get(&variable_dimensions_marker(name)) {
        for index in 0..*count {
            if let Ok(index) = usize::try_from(index) {
                keys.push(dimensions::variable_dimension_marker(name, index, "left"));
                keys.push(dimensions::variable_dimension_marker(name, index, "right"));
            }
        }
    }
    keys
}

pub(super) fn is_unbounded(value: &ConstExpr, env: &HashMap<String, i128>) -> bool {
    match value {
        ConstExpr::Literal(value) => value == "$",
        ConstExpr::Ident(name) => env.get(&unbounded_parameter_marker(name)) == Some(&1),
        _ => false,
    }
}

pub(super) fn contains_unbounded_operand(value: &ConstExpr, env: &HashMap<String, i128>) -> bool {
    match value {
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => is_unbounded(value, env),
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            contains_unbounded_operand(expr, env)
                || contains_unbounded_operand(msb, env)
                || contains_unbounded_operand(lsb, env)
        }
        ConstExpr::Select { expr, bit } => {
            contains_unbounded_operand(expr, env) || contains_unbounded_operand(bit, env)
        }
        ConstExpr::Function {
            callee: Callee::System(SystemFunction::Isunbounded),
            ..
        } => false,
        ConstExpr::Function { args, .. } => {
            args.iter().any(|arg| contains_unbounded_operand(arg, env))
        }
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
            contains_unbounded_operand(expr, env)
        }
        ConstExpr::Binary { left, right, .. } => {
            contains_unbounded_operand(left, env) || contains_unbounded_operand(right, env)
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            contains_unbounded_operand(condition, env)
                || contains_unbounded_operand(then_expr, env)
                || contains_unbounded_operand(else_expr, env)
        }
    }
}

/// Apply explicit parameter overrides by declaration name.
pub(super) fn apply_parameter_overrides(
    parameters: &mut [Parameter],
    overrides: &HashMap<String, ConstExpr>,
) -> Result<(), AnalyzerError> {
    if let Some(name) = overrides
        .keys()
        .find(|name| !parameters.iter().any(|parameter| parameter.name() == *name))
    {
        return Err(AnalyzerError::UnknownParameterOverride { name: name.clone() });
    }
    if let Some(name) = overrides.keys().find(|name| {
        parameters
            .iter()
            .any(|parameter| parameter.name() == *name && parameter.is_local)
    }) {
        return Err(AnalyzerError::LocalParameterOverride { name: name.clone() });
    }
    for parameter in parameters {
        if let Some(value) = overrides.get(parameter.name()) {
            parameter.value = normalize_unbased_unsized_parameter_value(
                Some(value.clone()),
                parameter.declared_width(),
                parameter.declared_signed(),
            );
        }
    }
    Ok(())
}

pub(super) fn const_expr_from_i128(value: i128) -> ConstExpr {
    if value < 0 {
        ConstExpr::Unary {
            op: UnaryOp::Minus,
            expr: Box::new(ConstExpr::Literal(value.unsigned_abs().to_string())),
        }
    } else {
        ConstExpr::Literal(value.to_string())
    }
}

pub(super) fn parameters_from_native(
    authored: &[&celox_sv_syntax::Parameter],
    syntax_tree: &AnalysisContext<'_>,
    parameters: &mut Vec<Parameter>,
    is_local: bool,
    base_const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Result<(), AnalyzerError> {
    let mut environment = ParameterEnvironment::new(
        &syntax_tree.state,
        parameters,
        base_const_env,
        &authored
            .iter()
            .map(|parameter| instances::normalize_identifier(&parameter.name.spelling))
            .collect::<Vec<_>>(),
    );
    parameters_from_native_with_environment(
        authored,
        syntax_tree,
        parameters,
        is_local,
        base_const_env,
        type_aliases,
        parameter_overrides,
        &mut environment,
    )
}

/// A declaration-order prefix, independent of the inherited environment.
/// Keep that prefix separate: its values and type markers override inherited
/// bindings, but inherited values must not resolve a forward prefix reference.
pub(super) struct ParameterEnvironment<'a> {
    base: &'a HashMap<String, i128>,
    values: HashMap<String, i128>,
    // Evaluation sees inherited constants except declarations of this scope.
    // Keep it incremental, while `values` remains the local prefix projection.
    bindings: HashMap<String, i128>,
    types: HashMap<String, ExprType>,
    constants: HashMap<String, i128>,
    // Only a nonnumeric prefix binding with an inherited numeric value makes
    // the range-lowering and width-evaluation projections differ. Track that
    // conservatively as the prefix grows, without rescanning its literals.
    has_inherited_literal_shadow: bool,
    // Numeric environments cannot represent X/Z. Keep resolved literals for
    // evaluating later declarations without dropping their state bits.
    literals: HashMap<String, Expr>,
}

impl<'a> ParameterEnvironment<'a> {
    pub(super) fn new(
        state: &AnalysisState,
        parameters: &[Parameter],
        base: &'a HashMap<String, i128>,
        shadowed: &[String],
    ) -> Self {
        let shadowed: HashSet<&str> = shadowed
            .iter()
            .map(String::as_str)
            .chain(parameters.iter().map(Parameter::name))
            .collect();
        let mut bindings = base.clone();
        for name in &shadowed {
            for key in binding_keys(name, &bindings, 0) {
                bindings.remove(&key);
            }
        }
        let mut values = HashMap::default();
        let mut types = parameter_types_from_const_env(&bindings);
        let mut literals = scope::imported(state)
            .parameter_values
            .iter()
            .filter(|(name, value)| {
                let mut literal = *value;
                while let Expr::Resize { expr, .. } = literal {
                    literal = expr;
                }
                matches!(literal, Expr::Literal(_))
                    && !bindings.contains_key(*name)
                    && !shadowed.contains(name.as_str())
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        for parameter in parameters {
            bind_parameter(state, &mut bindings, &mut types, &mut literals, parameter);
            for key in binding_keys(parameter.name(), &bindings, parameter.packed_ranges.len()) {
                if let Some(value) = bindings.get(&key) {
                    values.insert(key, *value);
                }
            }
        }
        let mut constants = base.clone();
        let has_inherited_literal_shadow = parameters.iter().any(|parameter| {
            !values.contains_key(parameter.name()) && base.contains_key(parameter.name())
        });
        for parameter in parameters {
            for key in binding_keys(parameter.name(), &constants, parameter.packed_ranges.len()) {
                constants.remove(&key);
            }
        }
        constants.extend(values.iter().map(|(name, value)| (name.clone(), *value)));
        Self {
            base,
            values,
            bindings,
            types,
            constants,
            has_inherited_literal_shadow,
            literals,
        }
    }

    fn append(&mut self, state: &AnalysisState, parameter: &Parameter) {
        let bound = bind_parameter(
            state,
            &mut self.bindings,
            &mut self.types,
            &mut self.literals,
            parameter,
        );
        for key in binding_keys(
            parameter.name(),
            &self.values,
            parameter.packed_ranges.len(),
        ) {
            self.values.remove(&key);
        }
        for key in binding_keys(
            parameter.name(),
            &self.bindings,
            parameter.packed_ranges.len(),
        ) {
            if let Some(value) = self.bindings.get(&key) {
                self.values.insert(key, *value);
            }
        }
        for key in binding_keys(
            parameter.name(),
            &self.constants,
            parameter.packed_ranges.len(),
        ) {
            self.constants.remove(&key);
        }
        self.has_inherited_literal_shadow |=
            !self.values.contains_key(parameter.name()) && self.base.contains_key(parameter.name());
        if !bound {
            // A type query does not evaluate its operand. Preserve a known
            // rank for following declarations even when an imported alias's
            // value is deferred; do not publish an inherited numeric value.
            if let Some(ty) = parameter.resolved_type(&self.types) {
                let rank = if parameter.floating.is_some() {
                    0
                } else if parameter.packed_ranges.is_empty() {
                    usize::from(!parameter.has_declared_type || ty.width > 1)
                } else {
                    parameter.packed_ranges.len()
                };
                self.constants
                    .insert(parameter_rank_marker(parameter.name()), rank as i128);
            }
            return;
        }
        let name = parameter.name();
        if self.literals.contains_key(name) && !self.values.contains_key(name) {
            self.constants.remove(name);
            self.has_inherited_literal_shadow |= self.base.contains_key(name);
        }
        let mut keys = vec![
            name.to_string(),
            parameter_marker(name),
            unbounded_parameter_marker(name),
            local_parameter_marker(name),
            parameter_width_marker(name),
            parameter_signed_marker(name),
            parameter_dimensions_marker(name),
            parameter_rank_marker(name),
            parameter_signed_element_marker(name),
        ];
        for index in 0..parameter.packed_ranges.len() {
            keys.push(parameter_dimension_marker(name, index, "left"));
            keys.push(parameter_dimension_marker(name, index, "right"));
        }
        for key in keys {
            if let Some(value) = self.values.get(&key) {
                self.constants.insert(key, *value);
            }
        }
    }
}

pub(super) fn parameters_from_native_with_environment(
    authored: &[&celox_sv_syntax::Parameter],
    syntax_tree: &AnalysisContext<'_>,
    parameters: &mut Vec<Parameter>,
    is_local: bool,
    base_const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_overrides: &HashMap<String, ConstExpr>,
    environment: &mut ParameterEnvironment<'_>,
) -> Result<(), AnalyzerError> {
    if authored.is_empty() {
        return Ok(());
    }
    let native_prefix =
        declaration_types::prefix(authored.first().copied(), syntax_tree, type_aliases)?
            .ok_or_else(|| AnalyzerError::Parse("native parameter type is missing".into()))?;
    let declared_alias = native_prefix.alias.clone();
    let parameter_width = parameter_declared_width(
        authored,
        syntax_tree,
        base_const_env,
        environment,
        type_aliases,
        parameter_overrides,
        &native_prefix,
    )?;
    // Check the source width before normalizing fill literals or building
    // declaration-order value environments, even for a narrow later select.
    validate_parameter_materialization_width(parameter_width)?;
    let has_declared_type = native_prefix.has_declared_type();
    let alias_has_use_site_dimensions =
        declared_alias.is_some() && !native_prefix.data.packed_dimensions.is_empty();
    let parameter_signed = parameter_width.map(|_| {
        declared_alias
            .as_ref()
            .map(|alias| alias.is_signed() && !alias_has_use_site_dimensions)
            .unwrap_or_else(|| native_prefix.signed())
    });
    let parameter_signed_element_depth = native_prefix.signed_element_depth();
    let parameter_ranges = native_prefix.ranges(syntax_tree, base_const_env, type_aliases)?;
    let simple_bit_vector = parameter_ranges.len() <= 1
        && declared_alias
            .as_ref()
            .is_none_or(|alias| alias.members.is_empty() && alias.unpacked_ranges.is_empty());
    let parameter_members = declared_alias
        .as_ref()
        .map(|ty| ty.members.clone())
        .unwrap_or_default();
    let parameter_is_2state = native_prefix.is_2state();
    for &native in authored {
        // An unpacked array parameter is a constant variable, not a value.
        if array_parameters::uses_signal_representation(
            native,
            syntax_tree,
            &environment.constants,
            type_aliases,
        )? || scope::imported(&syntax_tree.state)
            .pattern_parameter_bindings
            .contains_key(&native.id)
        {
            let name = instances::normalize_identifier(&native.name.spelling);
            let marker = unbounded_parameter_marker(&name);
            environment.values.insert(marker.clone(), 0);
            environment.constants.insert(marker, 0);
            continue;
        }
        let name = instances::normalize_identifier(&native.name.spelling);
        // The declaration already binds its name while its initializer is
        // analyzed. Neither an imported value nor an earlier projection
        // of this binding may resolve a self-reference.
        for key in binding_keys(&name, &environment.constants, parameter_ranges.len()) {
            environment.constants.remove(&key);
        }
        environment.literals.remove(&name);
        let const_env = &environment.constants;
        let replaced_pattern = !is_local
            && parameter_overrides.contains_key(&name)
            && array_parameters::is_array_parameter(native, syntax_tree.declarations());
        let mut value = if replaced_pattern {
            parameter_overrides.get(&name).cloned()
        } else if ownership::default(native).is_some() {
            let imported = scope::imported(&syntax_tree.state);
            let mut proof_context = PackedDimensions::new(
                parameter_packed_dimensions_with_imported(parameters, &imported),
                const_env,
                type_aliases,
            );
            // Preliminary collection must not expand unresolved initializer
            // chains merely to validate a shape. Only resolved prefix values
            // are available here; imported bindings remain shadowed by locals.
            proof_context.parameter_values = imported.parameter_values.clone().into();
            for parameter in parameters.iter() {
                proof_context.parameter_values.remove(parameter.name());
            }
            proof_context.parameter_values.remove(&name);
            proof_context.remove(&name);
            let resolved_numeric = parameters
                .iter()
                .filter(|parameter| environment.values.contains_key(parameter.name()))
                .map(|parameter| parameter.name().to_string())
                .collect();
            proof_context
                .parameter_values
                .extend(parameter_value_env_filtered(
                    &syntax_tree.state,
                    parameters,
                    const_env,
                    Some(&resolved_numeric),
                ));
            proof_context.parameter_values.extend(
                environment
                    .literals
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone())),
            );
            if let Some(value) = skipped::literal_widths(
                ownership::default(native).ok_or_else(|| {
                    AnalyzerError::Parse("native parameter default is missing".into())
                })?,
                syntax_tree,
                &proof_context,
            )? {
                Some(value)
            } else if native_prefix.floating().is_none()
                && skipped::needs_selected(
                    &syntax_tree.state,
                    ownership::default(native)
                        .ok_or_else(|| unsupported("native parameter default is missing"))?,
                    syntax_tree.declarations(),
                )
            {
                let mut dimensions = PackedDimensions::new(
                    parameter_packed_dimensions_with_imported(parameters, &imported),
                    const_env,
                    type_aliases,
                );
                dimensions.parameter_values = parameter_value_env_with_imported(
                    &syntax_tree.state,
                    parameters,
                    const_env,
                    &imported,
                )
                .into();
                // Enum constants are unavailable during preliminary collection.
                // Leave unresolved values for the subsequent lowering pass;
                // final validation rejects anything that still cannot be lowered.
                match selects::selected_parameter_initializer(
                    ownership::default(native).ok_or_else(|| {
                        AnalyzerError::Parse("native parameter default is missing".into())
                    })?,
                    syntax_tree,
                    &dimensions,
                    parameter_width,
                ) {
                    Ok(value) => Some(value),
                    // Deferred name/type resolution may be completed by a
                    // later collection pass. Native shape/identity errors
                    // are final and must not disappear with that deferral.
                    Err(error @ AnalyzerError::Parse(_)) => return Err(error),
                    Err(_) => None,
                }
            } else {
                constant_binding::bind(
                    ownership::default(native).ok_or_else(|| {
                        AnalyzerError::Parse("native parameter default is missing".into())
                    })?,
                    syntax_tree,
                    &proof_context,
                )?
            }
        } else {
            None
        };
        // Apply overrides as each declaration is collected so later
        // parameter initializers (including casts) are evaluated from the
        // specialized values rather than defaults that will be replaced
        // only after collection has finished.
        if !is_local && let Some(override_value) = parameter_overrides.get(&name) {
            value = Some(override_value.clone());
        }
        value = normalize_unbased_unsized_parameter_value(value, parameter_width, parameter_signed);
        if value
            .as_ref()
            .is_some_and(|expression| is_unbounded(expression, const_env))
            && !simple_bit_vector
        {
            return Err(AnalyzerError::Unsupported(format!(
                "unbounded parameter `{name}` requires a simple bit vector type"
            )));
        }
        if let Some(expression) = &value
            && !is_unbounded(expression, const_env)
            && contains_unbounded_operand(expression, const_env)
        {
            let lowered = substitute_expr_idents(
                const_expr_to_expr(expression.clone()),
                &environment.literals,
            );
            if !matches!(
                fold_const_integral_expr_preserving_mask(&syntax_tree.state, lowered, const_env),
                Expr::Literal(_)
            ) {
                return Err(AnalyzerError::Unsupported(format!(
                    "numeric use of unbounded parameter in `{name}`"
                )));
            }
        }
        if value
            .as_ref()
            .is_some_and(|value| is_unbounded(value, const_env))
        {
            value = Some(ConstExpr::Literal("$".into()));
        }
        let mut parameter = Parameter::new(
            name,
            value,
            parameter_width,
            parameter_signed,
            parameter_is_2state,
            has_declared_type,
            is_local,
        );
        parameter.floating = native_prefix.floating().or_else(|| {
            if has_declared_type {
                return None;
            }
            let expanded = substitute_expr_idents(
                const_expr_to_expr(parameter.value()?.clone()),
                &environment.literals,
            );
            let expr = expr_to_const(&syntax_tree.state, expanded)?;
            typecheck::const_expr_has_real_type(&expr.into()).then_some(TypeKind::Real)
        });
        parameter.packed_ranges = parameter_ranges.clone();
        parameter.members = parameter_members.clone();
        parameter.signed_element_depth = parameter_signed_element_depth;
        validate_parameter_materialization_width(
            parameter
                .resolved_type(&environment.types)
                .map(|ty| ty.width),
        )?;
        environment.append(&syntax_tree.state, &parameter);
        parameters.push(parameter);
    }
    Ok(())
}

fn validate_parameter_materialization_width(width: Option<usize>) -> Result<(), AnalyzerError> {
    if let Some(width) = width
        && width > constant_folding::MAX_CONSTANT_CONCAT_BITS
    {
        return Err(unsupported(format!(
            "parameter width {width} exceeds the {}-bit constant materialization limit",
            constant_folding::MAX_CONSTANT_CONCAT_BITS
        )));
    }
    Ok(())
}

fn parameter_declared_width(
    authored: &[&celox_sv_syntax::Parameter],
    syntax_tree: &AnalysisContext<'_>,
    base_const_env: &HashMap<String, i128>,
    environment: &ParameterEnvironment<'_>,
    type_aliases: &HashMap<String, Type>,
    parameter_overrides: &HashMap<String, ConstExpr>,
    native: &declaration_types::Prefix<'_>,
) -> Converted<Option<usize>> {
    let declared_alias = native.alias.clone();
    if declared_alias.is_none() && native.data.packed_dimensions.is_empty() {
        return Ok(native.scalar_width(&syntax_tree.state));
    }
    // With the same immutable base and no inherited numeric value shadowed
    // by a nonnumeric literal, both former copies have exactly the contents
    // of `constants`. A pure type header has no sibling assignments to seed
    // or self-values to mask.
    if std::ptr::eq(environment.base, base_const_env) && !environment.has_inherited_literal_shadow {
        return parameter_width_from_environments(
            syntax_tree,
            type_aliases,
            &environment.constants,
            &environment.constants,
            native,
        );
    }
    #[cfg(test)]
    DECLARATION_ENV_COPIES.with(|count| count.set(count.get() + 2));
    let mut range_env = environment.constants.clone();
    // Numeric-size casts in a later parameter declaration can refer to an
    // earlier assignment in the same parameter-port list. Seed range lowering
    // from those assignments while retaining the separate environment below
    // for evaluating the resulting symbolic ranges without stale self-values.
    for &native in authored {
        let name = instances::normalize_identifier(&native.name.spelling);
        let value = parameter_overrides
            .get(&name)
            .cloned()
            .or_else(|| {
                ownership::default(native).and_then(|_| {
                    constant_binding::expression(
                        ownership::default(native)?,
                        syntax_tree.declarations(),
                        syntax_tree,
                        &range_env,
                        type_aliases,
                    )
                    .ok()
                    .flatten()
                })
            })
            .and_then(|value| eval_ast_const_expr(&syntax_tree.state, &value, &range_env));
        if let Some(value) = value {
            range_env.insert(name, value);
        }
    }
    let mut env = base_const_env.clone();
    // A second lowering pass receives values from the first pass in the base
    // environment. Do not let stale values for parameters declared by this
    // same syntax node resolve its declaration ranges; only constants from
    // outside the declaration (such as enum members) belong here.
    for native in authored {
        env.remove(&instances::normalize_identifier(&native.name.spelling));
    }
    env.extend(
        environment
            .values
            .iter()
            .map(|(name, value)| (name.clone(), *value)),
    );
    parameter_width_from_environments(syntax_tree, type_aliases, &range_env, &env, native)
}

fn parameter_width_from_environments(
    syntax_tree: &AnalysisContext<'_>,
    type_aliases: &HashMap<String, Type>,
    range_env: &HashMap<String, i128>,
    env: &HashMap<String, i128>,
    native: &declaration_types::Prefix<'_>,
) -> Converted<Option<usize>> {
    let prefix = native;
    let ranges = prefix.ranges(syntax_tree, range_env, type_aliases)?;
    if ranges.is_empty() {
        return Ok(if prefix.alias.is_some() {
            Some(1)
        } else {
            prefix.scalar_width(&syntax_tree.state)
        });
    }
    let mut width = 1usize;
    for range in ranges {
        let (Some(left), Some(right)) = (
            eval_ast_const_expr(&syntax_tree.state, range.left(), env),
            eval_ast_const_expr(&syntax_tree.state, range.right(), env),
        ) else {
            return Ok(None);
        };
        width = usize::try_from(left.abs_diff(right))
            .ok()
            .and_then(|width| width.checked_add(1))
            .and_then(|part| width.checked_mul(part))
            .ok_or_else(|| unsupported("parameter width exceeds constant materialization limit"))?;
    }
    Ok(Some(width))
}

fn normalize_unbased_unsized_parameter_value(
    value: Option<ConstExpr>,
    width: Option<usize>,
    signed: Option<bool>,
) -> Option<ConstExpr> {
    let signing = if signed.unwrap_or(false) { "s" } else { "" };
    match (value, width) {
        (Some(ConstExpr::Literal(value)), Some(width)) if value == "'1" => {
            let literal = if width <= 128 {
                let bits = if width == 128 {
                    u128::MAX
                } else {
                    (1u128 << width) - 1
                };
                format!("{width}'{signing}d{bits}")
            } else {
                format!("{width}'{signing}b{}", "1".repeat(width))
            };
            Some(ConstExpr::Literal(literal))
        }
        (Some(ConstExpr::Literal(value)), Some(width)) if value == "'0" => {
            Some(ConstExpr::Literal(format!("{width}'{signing}d0")))
        }
        (Some(ConstExpr::Literal(value)), Some(width))
            if matches!(value.as_str(), "'x" | "'X" | "'z" | "'Z" | "'?") =>
        {
            let fill = value.chars().nth(1)?;
            Some(ConstExpr::Literal(format!("{width}'{signing}b{fill}")))
        }
        (value, _) => value,
    }
}

pub(super) fn const_env_from_parameters(
    state: &AnalysisState,
    parameters: &[Parameter],
) -> HashMap<String, i128> {
    let mut env = HashMap::default();
    extend_const_env_with_parameters(state, &mut env, parameters);
    env
}

pub(super) fn extend_const_env_with_parameters(
    state: &AnalysisState,
    env: &mut HashMap<String, i128>,
    parameters: &[Parameter],
) {
    let mut parameter_types = parameter_types_from_const_env(env);
    let mut literals = HashMap::default();
    for parameter in parameters {
        bind_parameter(state, env, &mut parameter_types, &mut literals, parameter);
    }
}

pub(super) fn bind_parameter(
    state: &AnalysisState,
    env: &mut HashMap<String, i128>,
    types: &mut HashMap<String, ExprType>,
    literals: &mut HashMap<String, Expr>,
    parameter: &Parameter,
) -> bool {
    #[cfg(test)]
    PARAMETER_BINDINGS.with(|count| count.set(count.get() + 1));
    for key in binding_keys(parameter.name(), env, parameter.packed_ranges.len()) {
        env.remove(&key);
    }
    types.remove(parameter.name());
    literals.remove(parameter.name());
    // `$` is an elaboration value, never an integer sentinel. Preserve it
    // separately so typed parameters, aliases and overrides cannot turn it
    // into an ordinary number (IEEE 1800-2023 6.20.7, 20.6.3).
    if parameter
        .value()
        .is_some_and(|value| is_unbounded(value, env))
    {
        env.remove(parameter.name());
        env.remove(&parameter_marker(parameter.name()));
        env.remove(&local_parameter_marker(parameter.name()));
        env.insert(unbounded_parameter_marker(parameter.name()), 1);
        literals.insert(parameter.name().to_string(), Expr::Literal("$".into()));
        if let Some(ty) = parameter.resolved_type(types) {
            types.insert(parameter.name().to_string(), ty);
            insert_parameter_type_markers(env, parameter.name(), ty);
            let rank = if parameter.floating.is_some() {
                0
            } else if parameter.packed_ranges.is_empty() {
                usize::from(!parameter.has_declared_type || ty.width > 1)
            } else {
                parameter.packed_ranges.len()
            };
            env.insert(parameter_rank_marker(parameter.name()), rank as i128);
        }
        insert_parameter_dimension_markers(state, env, parameter);
        return true;
    }
    let (value, literal) = parameter.resolved_value_and_literal(state, env, types, literals);
    if value.is_none() && literal.is_none() {
        return false;
    }
    env.insert(unbounded_parameter_marker(parameter.name()), 0);
    if let Some(ty) = parameter.resolved_type(types) {
        types.insert(parameter.name().to_string(), ty);
        insert_parameter_type_markers(env, parameter.name(), ty);
        let rank = if parameter.floating.is_some() {
            0
        } else if parameter.packed_ranges.is_empty() {
            usize::from(!parameter.has_declared_type || ty.width > 1)
        } else {
            parameter.packed_ranges.len()
        };
        env.insert(parameter_rank_marker(parameter.name()), rank as i128);
    }
    if let Some(literal) = literal {
        // Shadow a numeric inherited binding with the four-state declaration.
        env.remove(parameter.name());
        literals.insert(parameter.name().to_string(), literal);
        insert_parameter_dimension_markers(state, env, parameter);
        return true;
    }
    let value = value.expect("a known value or a resolved literal was checked above");
    env.insert(parameter.name().to_string(), value);
    env.insert(parameter_marker(parameter.name()), value);
    if parameter.is_local {
        env.insert(local_parameter_marker(parameter.name()), value);
    }
    insert_parameter_dimension_markers(state, env, parameter);
    true
}

/// Record packed bounds for parameter selects and array queries.
fn insert_parameter_dimension_markers(
    state: &AnalysisState,
    env: &mut HashMap<String, i128>,
    parameter: &Parameter,
) {
    let name = parameter.name();
    if parameter.packed_ranges.is_empty() {
        return;
    }
    let Some(bounds) = parameter
        .packed_ranges
        .iter()
        .map(|range| {
            Some((
                eval_ast_const_expr(state, range.left(), env)?,
                eval_ast_const_expr(state, range.right(), env)?,
            ))
        })
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let Ok(count) = i128::try_from(bounds.len()) else {
        return;
    };
    env.insert(parameter_dimensions_marker(name), count);
    for (index, (left, right)) in bounds.into_iter().enumerate() {
        env.insert(parameter_dimension_marker(name, index, "left"), left);
        env.insert(parameter_dimension_marker(name, index, "right"), right);
    }
    if let Some(depth) = parameter
        .signed_element_depth
        .and_then(|depth| i128::try_from(depth).ok())
    {
        env.insert(parameter_signed_element_marker(name), depth);
    }
}

/// The element of a packed parameter selected by constant `indices`, as a
/// sized literal: the remaining dimensions, signed when the element is of a
/// signed named type (IEEE 1800-2023 7.4.1). An index out of range gives X
/// bits (11.5.1).
pub(super) fn parameter_element_literal(
    name: &str,
    indices: &[i128],
    env: &HashMap<String, i128>,
) -> Option<String> {
    let count = usize::try_from(*env.get(&parameter_dimensions_marker(name))?).ok()?;
    if indices.is_empty() || indices.len() > count {
        return None;
    }
    let bounds = (0..count)
        .map(|index| {
            Some((
                *env.get(&parameter_dimension_marker(name, index, "left"))?,
                *env.get(&parameter_dimension_marker(name, index, "right"))?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let size =
        |(left, right): (i128, i128)| usize::try_from(left.abs_diff(right)).ok()?.checked_add(1);
    let width_from = |start: usize| {
        bounds[start..]
            .iter()
            .try_fold(1usize, |acc, bound| acc.checked_mul(size(*bound)?))
    };
    let element_width = width_from(indices.len())?;
    let total_width = width_from(0)?;
    if total_width > 128 {
        return None;
    }
    let signed = env
        .get(&parameter_signed_element_marker(name))
        .is_some_and(|depth| usize::try_from(*depth).ok() == Some(indices.len()));
    let signing = if signed { "s" } else { "" };
    let mut offset = 0usize;
    for (position, index) in indices.iter().enumerate() {
        let (left, right) = bounds[position];
        if *index < left.min(right) || *index > left.max(right) {
            return Some(format!(
                "{element_width}'{signing}b{}",
                "x".repeat(element_width)
            ));
        }
        // The right bound is the least significant element.
        let from_right = usize::try_from(index.abs_diff(right)).ok()?;
        offset = offset.checked_add(from_right.checked_mul(width_from(position + 1)?)?)?;
    }
    let mask = |width| {
        if width == 128 {
            u128::MAX
        } else {
            (1u128 << width) - 1
        }
    };
    let value = *env.get(name)? as u128 & mask(total_width);
    let element = (value >> offset) & mask(element_width);
    Some(format!(
        "{element_width}'{signing}b{element:0element_width$b}"
    ))
}

pub(super) fn coerce_const_parameter_value(value: i128, width: usize, signed: bool) -> i128 {
    if width >= 128 {
        return value;
    }
    if width == 0 {
        return 0;
    }
    let mask = (1u128 << width) - 1;
    let bits = (value as u128) & mask;
    if signed && bits & (1u128 << (width - 1)) != 0 {
        (bits | !mask) as i128
    } else {
        bits as i128
    }
}

pub(super) fn enum_initializer_fits_base_type(
    value: i128,
    value_type: ExprType,
    base_type: ExprType,
) -> bool {
    // A same-width initializer may change its numeric interpretation when
    // the enum base has different signedness, but it does not lose any bits.
    if value_type.width <= base_type.width || base_type.width >= 128 {
        return true;
    }
    if base_type.width == 0 {
        return false;
    }
    if base_type.signed {
        let magnitude = 1i128 << (base_type.width - 1);
        (-magnitude..magnitude).contains(&value)
    } else if value < 0 {
        false
    } else if base_type.width >= 127 {
        true
    } else {
        value < (1i128 << base_type.width)
    }
}

/// Collect enum member constants declared by module-level `typedef enum`
/// declarations. Native initializer nodes may reference preceding members;
/// implicit successors and type coercion retain the existing semantic kernel.
pub(super) fn enum_member_constants_from_scope(
    syntax_tree: &AnalysisContext<'_>,
    base_const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Result<EnumMemberConstants, AnalyzerError> {
    let items = syntax_tree.body_membership()?;
    enum_member_constants_from_items(
        syntax_tree,
        &items,
        base_const_env,
        type_aliases,
        parameter_overrides,
    )
}

pub(super) fn enum_member_constants_from_items(
    syntax_tree: &AnalysisContext<'_>,
    declarations: &[celox_sv_syntax::BodyItemId],
    base_const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Result<EnumMemberConstants, AnalyzerError> {
    use celox_sv_syntax::{BodyItemKind, DataTypeKind, TypeAliasTarget};
    let native = syntax_tree.declarations();
    let enum_type = |id| {
        let BodyItemKind::TypeAlias(alias) = native.body_item(id).kind else {
            return None;
        };
        let TypeAliasTarget::DataType(id) = native.type_alias(alias).target else {
            return None;
        };
        matches!(native.data_type(id).kind, DataTypeKind::Enum { .. }).then_some(id)
    };
    if !declarations.iter().any(|id| enum_type(*id).is_some()) {
        return Ok(EnumMemberConstants::default());
    }
    let mut constants = EnumMemberConstants::default();
    let mut eval_env = base_const_env.clone();
    let mut resolved_type_aliases = type_aliases.clone();
    let mut parameters = Vec::new();
    let no_overrides = HashMap::default();
    for &id in declarations {
        if let BodyItemKind::Parameter(_) = native.body_item(id).kind {
            let authored = ownership::body(id, syntax_tree)?;
            let local = authored.first().is_some_and(|parameter| parameter.local);
            parameters_from_native(
                &authored,
                syntax_tree,
                &mut parameters,
                local,
                &eval_env,
                &resolved_type_aliases,
                if local {
                    &no_overrides
                } else {
                    parameter_overrides
                },
            )?;
            extend_const_env_with_parameters(&syntax_tree.state, &mut eval_env, &parameters);
            resolved_type_aliases = types::type_aliases_from_body_with_env(
                syntax_tree,
                declarations,
                &eval_env,
                &resolved_type_aliases,
            )?;
            continue;
        }
        let Some(id) = enum_type(id) else {
            continue;
        };
        let DataTypeKind::Enum { base, items } = &native.data_type(id).kind else {
            unreachable!();
        };
        let member_type = match base {
            Some(id) => type_binding::enum_constant_base(
                *id,
                syntax_tree,
                &eval_env,
                &resolved_type_aliases,
            )?
            .and_then(|ty| expr_type_from_type(&syntax_tree.state, &ty, &eval_env)),
            None => Some(ExprType {
                width: 32,
                signed: true,
            }),
        }
        .ok_or_else(|| unsupported("enum base type"))?;
        let mut next_enum_value = 0i128;
        for new in items {
            let name = instances::normalize_identifier(&new.name.spelling);
            if !new.dimensions.is_empty() {
                return Err(unsupported(format!("ranged enum member `{name}`")));
            }
            let value = match new.value {
                Some(id) => constant_binding::expression(
                    id,
                    syntax_tree.declarations(),
                    syntax_tree,
                    &eval_env,
                    &resolved_type_aliases,
                )?
                .ok_or_else(|| unsupported(format!("enum member `{name}` value")))?,
                // An unvalued member follows its predecessor (the first is 0).
                None => ConstExpr::Literal(format_typed_parameter_literal(
                    next_enum_value,
                    member_type.width,
                    member_type.signed,
                )),
            };
            let value = match value {
                ConstExpr::Literal(literal) => ConstExpr::Literal(
                    resize_unbased_fill_literal_for_cast(
                        &literal,
                        member_type.width,
                        member_type.signed,
                    )
                    .unwrap_or(literal),
                ),
                value => value,
            };
            let value_type =
                infer_const_expr_type(&value, &parameter_types_from_const_env(&eval_env))
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported(format!("enum member `{name}` value type"))
                    })?;
            let number =
                eval_ast_const_expr(&syntax_tree.state, &value, &eval_env).ok_or_else(|| {
                    AnalyzerError::Unsupported(format!("unresolved enum member `{name}` value"))
                })?;
            if !enum_initializer_fits_base_type(number, value_type, member_type) {
                return Err(AnalyzerError::Unsupported(format!(
                    "enum member `{name}` value does not fit its base type"
                )));
            }
            let number =
                coerce_const_parameter_value(number, member_type.width, member_type.signed);
            next_enum_value = number.wrapping_add(1);
            constants.numbers.insert(name.clone(), number);
            eval_env.insert(name.clone(), number);
            insert_parameter_type_markers(&mut eval_env, &name, member_type);
            constants.types.insert(name.clone(), member_type);
            constants.exprs.insert(
                name,
                Expr::Literal(format_typed_parameter_literal(
                    number,
                    member_type.width,
                    member_type.signed,
                )),
            );
        }
        // A later typedef may use a cast or range that depends on this
        // enum's members. Rebuild aliases from the enriched environment
        // before resolving a subsequent enum base through that typedef.
        resolved_type_aliases = types::type_aliases_from_body_with_env(
            syntax_tree,
            declarations,
            &eval_env,
            &resolved_type_aliases,
        )?;
    }
    Ok(constants)
}

pub(super) fn parameter_value_env(
    state: &AnalysisState,
    parameters: &[Parameter],
    const_env: &HashMap<String, i128>,
) -> HashMap<String, Expr> {
    parameter_value_env_filtered(state, parameters, const_env, None)
}

/// Materialize only referenced values and their initializer dependencies.
/// Preserve the full declaration-order type prefix, including unused parameters.
pub(super) fn parameter_value_env_for_references(
    state: &AnalysisState,
    parameters: &[Parameter],
    const_env: &HashMap<String, i128>,
    references: &HashSet<String>,
) -> HashMap<String, Expr> {
    let mut needed: HashSet<String> = parameters
        .iter()
        .filter(|parameter| references.contains(parameter.name()))
        .map(|parameter| parameter.name().to_string())
        .collect();
    if needed.is_empty() {
        return HashMap::default();
    }
    if needed.iter().all(|name| const_env.contains_key(name)) {
        return parameter_value_env_filtered(state, parameters, const_env, Some(&needed));
    }
    // Include all declarations of a name so the projection preserves even
    // repeated-name prefixes used during preliminary collection.
    let mut by_name: HashMap<&str, Vec<&Parameter>> = HashMap::default();
    for parameter in parameters {
        by_name.entry(parameter.name()).or_default().push(parameter);
    }
    let mut pending: Vec<_> = needed.iter().cloned().collect();
    while let Some(name) = pending.pop() {
        // The original value environment uses an inherited number directly.
        // Its initializer's values are not needed; types are handled below.
        if const_env.contains_key(&name) {
            continue;
        }
        let Some(declarations) = by_name.get(name.as_str()) else {
            continue;
        };
        for parameter in declarations {
            let mut expressions: Vec<_> = parameter.value().into_iter().collect();
            while let Some(expr) = expressions.pop() {
                match expr {
                    ConstExpr::Ident(name) => {
                        if by_name.contains_key(name.as_str()) && needed.insert(name.clone()) {
                            pending.push(name.clone());
                        }
                    }
                    ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                        expressions.extend([&**expr, &**msb, &**lsb]);
                    }
                    ConstExpr::Select { expr, bit } => {
                        expressions.push(expr);
                        expressions.push(bit);
                    }
                    ConstExpr::Function { args, .. } => expressions.extend(args),
                    ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
                        expressions.push(expr)
                    }
                    ConstExpr::Binary { left, right, .. } => {
                        expressions.push(left);
                        expressions.push(right);
                    }
                    ConstExpr::Mux {
                        condition,
                        then_expr,
                        else_expr,
                    } => {
                        expressions.push(condition);
                        expressions.push(then_expr);
                        expressions.push(else_expr);
                    }
                    ConstExpr::Literal(_) => {}
                }
            }
        }
    }
    parameter_value_env_filtered(state, parameters, const_env, Some(&needed))
}

fn parameter_value_env_filtered(
    state: &AnalysisState,
    parameters: &[Parameter],
    const_env: &HashMap<String, i128>,
    needed: Option<&HashSet<String>>,
) -> HashMap<String, Expr> {
    let mut values = HashMap::default();
    let mut parameter_types = HashMap::default();
    for parameter in parameters {
        if let Some(kind) = parameter.floating {
            if let Some(value) = parameter
                .value()
                .and_then(|value| real_literal(value, kind, const_env, &values))
            {
                values.insert(parameter.name.clone(), value);
            }
            continue;
        }
        if parameter.value().is_none() {
            continue;
        }
        if parameter
            .value()
            .is_some_and(|value| is_unbounded(value, const_env))
        {
            if needed.is_none_or(|needed| needed.contains(parameter.name())) {
                #[cfg(test)]
                PARAMETER_VALUE_MATERIALIZATIONS.with(|count| count.set(count.get() + 1));
                values.insert(parameter.name().to_string(), Expr::Literal("$".into()));
            }
            continue;
        }
        let inferred_type = parameter.value().and_then(|value| {
            infer_parameter_value_type(value, parameter.has_declared_type, &parameter_types)
        });
        let width = parameter
            .declared_width
            .or(inferred_type.map(|r#type| r#type.width));
        let signed = parameter
            .declared_signed
            .or(inferred_type.map(|r#type| r#type.signed))
            .unwrap_or(false);
        if let Some(width) = width {
            parameter_types.insert(parameter.name().to_string(), ExprType { width, signed });
        }

        if needed.is_some_and(|needed| !needed.contains(parameter.name())) {
            continue;
        }
        #[cfg(test)]
        PARAMETER_VALUE_MATERIALIZATIONS.with(|count| count.set(count.get() + 1));

        let mut value = if let Some(value) = const_env.get(parameter.name()).copied() {
            if let Some(width) = width {
                Expr::Literal(format_typed_parameter_literal(value, width, signed))
            } else if value.is_negative() {
                let width = (128 - (!value as u128).leading_zeros() as usize + 1).max(32);
                let mask = if width == 128 {
                    u128::MAX
                } else {
                    (1u128 << width) - 1
                };
                Expr::Literal(format!("{width}'sd{}", (value as u128) & mask))
            } else if let Some(ConstExpr::Literal(literal)) = parameter.value() {
                Expr::Literal(literal.clone())
            } else {
                Expr::Literal(value.to_string())
            }
        } else if let Some(value) = parameter.value().cloned() {
            let value = substitute_typed_parameter_literals(value, const_env, &parameter_types);
            let value = replace_oob_const_selects_with_unknown(state, value, const_env);
            let value = substitute_expr_constants_with_parameter_literals(
                state,
                const_expr_to_expr(value),
                const_env,
                &values,
            );
            if let Some(width) = width {
                match value {
                    Expr::Literal(literal) => {
                        let resized = resize_unbased_fill_literal_for_cast(&literal, width, signed)
                            .or_else(|| {
                                typecheck::parse_integral_literal(&literal).map(|literal| {
                                    resize_integral_literal_for_cast(literal, width, signed)
                                })
                            });
                        resized.map_or_else(
                            || Expr::Resize {
                                expr: Box::new(Expr::Literal(literal)),
                                width,
                                signed,
                            },
                            Expr::Literal,
                        )
                    }
                    value => Expr::Resize {
                        expr: Box::new(value),
                        width,
                        signed,
                    },
                }
            } else {
                value
            }
        } else {
            continue;
        };
        if parameter.declared_is_2state {
            value = Expr::Unary {
                op: UnaryOp::ToTwoState,
                expr: Box::new(value),
            };
        }
        value = fold_const_integral_expr_preserving_mask(state, value, const_env);
        values.insert(parameter.name().to_string(), value);
    }
    values
}

fn replace_oob_const_selects_with_unknown(
    state: &AnalysisState,
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
) -> ConstExpr {
    match expr {
        ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => ConstExpr::SelectRange {
            expr: Box::new(replace_oob_const_selects_with_unknown(
                state, *expr, const_env,
            )),
            msb: Box::new(replace_oob_const_selects_with_unknown(
                state, *msb, const_env,
            )),
            lsb: Box::new(replace_oob_const_selects_with_unknown(
                state, *lsb, const_env,
            )),
            signed,
        },
        ConstExpr::Select { expr, bit } => {
            let expr = replace_oob_const_selects_with_unknown(state, *expr, const_env);
            let bit = replace_oob_const_selects_with_unknown(state, *bit, const_env);
            if let ConstExpr::Literal(literal) = &expr
                && let Some(width) =
                    typecheck::parse_integral_literal(literal).map(|literal| literal.width)
                && match eval_ast_const_expr(state, &bit, const_env) {
                    Some(bit_index) => {
                        usize::try_from(bit_index).map_or(true, |bit_index| bit_index >= width)
                    }
                    None => const_expr_contains_unknown_literal(&bit),
                }
            {
                ConstExpr::Literal("1'bx".to_string())
            } else {
                ConstExpr::Select {
                    expr: Box::new(expr),
                    bit: Box::new(bit),
                }
            }
        }
        ConstExpr::Function { callee, args, site } => ConstExpr::Function {
            callee,
            site,
            args: args
                .into_iter()
                .map(|arg| replace_oob_const_selects_with_unknown(state, arg, const_env))
                .collect(),
        },
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => ConstExpr::Resize {
            expr: Box::new(replace_oob_const_selects_with_unknown(
                state, *expr, const_env,
            )),
            width,
            signed,
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op,
            expr: Box::new(replace_oob_const_selects_with_unknown(
                state, *expr, const_env,
            )),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: Box::new(replace_oob_const_selects_with_unknown(
                state, *left, const_env,
            )),
            op,
            right: Box::new(replace_oob_const_selects_with_unknown(
                state, *right, const_env,
            )),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: Box::new(replace_oob_const_selects_with_unknown(
                state, *condition, const_env,
            )),
            then_expr: Box::new(replace_oob_const_selects_with_unknown(
                state, *then_expr, const_env,
            )),
            else_expr: Box::new(replace_oob_const_selects_with_unknown(
                state, *else_expr, const_env,
            )),
        },
        ConstExpr::Ident(name) => ConstExpr::Ident(name),
        ConstExpr::Literal(value) => ConstExpr::Literal(value),
    }
}

fn const_expr_contains_unknown_literal(expr: &ConstExpr) -> bool {
    match expr {
        ConstExpr::Literal(literal) => typecheck::parse_integral_literal(literal)
            .is_some_and(|literal| literal.mask != Default::default()),
        ConstExpr::Ident(_) => false,
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            const_expr_contains_unknown_literal(expr)
                || const_expr_contains_unknown_literal(msb)
                || const_expr_contains_unknown_literal(lsb)
        }
        ConstExpr::Select { expr, bit } => {
            const_expr_contains_unknown_literal(expr) || const_expr_contains_unknown_literal(bit)
        }
        ConstExpr::Function { args, .. } => args.iter().any(const_expr_contains_unknown_literal),
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
            const_expr_contains_unknown_literal(expr)
        }
        ConstExpr::Binary { left, right, .. } => {
            const_expr_contains_unknown_literal(left) || const_expr_contains_unknown_literal(right)
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            const_expr_contains_unknown_literal(condition)
                || const_expr_contains_unknown_literal(then_expr)
                || const_expr_contains_unknown_literal(else_expr)
        }
    }
}

pub(super) fn const_expr_to_expr(expr: ConstExpr) -> Expr {
    match expr {
        ConstExpr::Literal(value) => Expr::Literal(value),
        ConstExpr::Ident(name) => Expr::Ident(name),
        ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(const_expr_to_expr(*expr)),
            msb: *msb,
            lsb: *lsb,
            signed,
        },
        ConstExpr::Select { expr, bit } => Expr::Select {
            expr: Box::new(const_expr_to_expr(*expr)),
            msb: (*bit).clone(),
            lsb: *bit,
            signed: false,
        },
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(const_expr_to_expr(*expr)),
            width,
            signed,
        },
        ConstExpr::Function { callee, args, .. } => Expr::Call {
            arg_order: Vec::new(),
            callee,
            args: args
                .into_iter()
                .map(|arg| Some(const_expr_to_expr(arg)))
                .collect(),
        },
        ConstExpr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(const_expr_to_expr(*expr)),
        },
        ConstExpr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(const_expr_to_expr(*left)),
            op,
            right: Box::new(const_expr_to_expr(*right)),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(const_expr_to_expr(*condition)),
            then_expr: Box::new(const_expr_to_expr(*then_expr)),
            else_expr: Box::new(const_expr_to_expr(*else_expr)),
        },
    }
}

pub(super) fn format_typed_parameter_literal(value: i128, width: usize, signed: bool) -> String {
    let signing = if signed { "s" } else { "" };
    if width <= 128 {
        let mask = if width == 128 {
            u128::MAX
        } else {
            (1u128 << width) - 1
        };
        let bits = (value as u128) & mask;
        format!("{width}'{signing}d{bits}")
    } else {
        let extension = if value.is_negative() { '1' } else { '0' };
        let high_bits = extension.to_string().repeat(width - 128);
        let low_bits = value as u128;
        format!("{width}'{signing}b{high_bits}{low_bits:0128b}")
    }
}

pub(super) fn substitute_typed_parameter_literals(
    expr: ConstExpr,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, ExprType>,
) -> ConstExpr {
    substitute_typed_parameter_literals_with_lookup(expr, constants, &|name| {
        parameter_types.get(name).copied()
    })
}

pub(super) fn substitute_typed_parameter_literals_with_lookup(
    expr: ConstExpr,
    constants: &HashMap<String, i128>,
    parameter_types: &impl Fn(&str) -> Option<ExprType>,
) -> ConstExpr {
    match expr {
        ConstExpr::Ident(name) => match (constants.get(&name), parameter_types(&name)) {
            (Some(value), Some(r#type)) => ConstExpr::Literal(format_typed_parameter_literal(
                *value,
                r#type.width,
                r#type.signed,
            )),
            _ => ConstExpr::Ident(name),
        },
        ConstExpr::Literal(value) => ConstExpr::Literal(value),
        ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => ConstExpr::SelectRange {
            expr: Box::new(substitute_typed_parameter_literals_with_lookup(
                *expr,
                constants,
                parameter_types,
            )),
            msb: Box::new(substitute_typed_parameter_literals_with_lookup(
                *msb,
                constants,
                parameter_types,
            )),
            lsb: Box::new(substitute_typed_parameter_literals_with_lookup(
                *lsb,
                constants,
                parameter_types,
            )),
            signed,
        },
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: Box::new(substitute_typed_parameter_literals_with_lookup(
                *expr,
                constants,
                parameter_types,
            )),
            bit: Box::new(substitute_typed_parameter_literals_with_lookup(
                *bit,
                constants,
                parameter_types,
            )),
        },
        ConstExpr::Function {
            callee: callee @ Callee::System(SystemFunction::Isunbounded),
            args,
            site,
        } => {
            if let [ConstExpr::Ident(parameter)] = args.as_slice()
                && let Some(value) = constants.get(&unbounded_parameter_marker(parameter))
            {
                ConstExpr::Literal(format!("1'b{}", usize::from(*value == 1)))
            } else {
                ConstExpr::Function { callee, args, site }
            }
        }
        ConstExpr::Function { callee, args, site } => ConstExpr::Function {
            callee,
            site,
            args: args
                .into_iter()
                .map(|arg| {
                    substitute_typed_parameter_literals_with_lookup(arg, constants, parameter_types)
                })
                .collect(),
        },
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => ConstExpr::Resize {
            expr: Box::new(substitute_typed_parameter_literals_with_lookup(
                *expr,
                constants,
                parameter_types,
            )),
            width,
            signed,
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op,
            expr: Box::new(substitute_typed_parameter_literals_with_lookup(
                *expr,
                constants,
                parameter_types,
            )),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: Box::new(substitute_typed_parameter_literals_with_lookup(
                *left,
                constants,
                parameter_types,
            )),
            op,
            right: Box::new(substitute_typed_parameter_literals_with_lookup(
                *right,
                constants,
                parameter_types,
            )),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: Box::new(substitute_typed_parameter_literals_with_lookup(
                *condition,
                constants,
                parameter_types,
            )),
            then_expr: Box::new(substitute_typed_parameter_literals_with_lookup(
                *then_expr,
                constants,
                parameter_types,
            )),
            else_expr: Box::new(substitute_typed_parameter_literals_with_lookup(
                *else_expr,
                constants,
                parameter_types,
            )),
        },
    }
}

pub(super) fn infer_const_expr_type(
    expr: &ConstExpr,
    parameter_types: &HashMap<String, ExprType>,
) -> Option<ExprType> {
    infer_const_expr_type_inner(expr, parameter_types, false)
}

// Only the bounded proof may infer a logical result without resolving its
// opaque skipped operands. Ordinary type queries must resolve both operands.
fn infer_const_expr_type_inner(
    expr: &ConstExpr,
    parameter_types: &HashMap<String, ExprType>,
    bounded_proof: bool,
) -> Option<ExprType> {
    match expr {
        ConstExpr::Literal(literal) => {
            let literal = typecheck::parse_integral_literal(literal)?;
            Some(ExprType {
                width: literal.width,
                signed: literal.signed,
            })
        }
        ConstExpr::Ident(name) => parameter_types.get(name).copied(),
        ConstExpr::SelectRange {
            msb, lsb, signed, ..
        } => Some(ExprType {
            width: typecheck::select_range_width(
                &(**msb).clone().into(),
                &(**lsb).clone().into(),
                &HashMap::default(),
            )?,
            signed: *signed,
        }),
        ConstExpr::Resize { width, signed, .. } => Some(ExprType {
            width: *width,
            signed: *signed,
        }),
        ConstExpr::Select { .. } => Some(ExprType {
            width: 1,
            signed: false,
        }),
        ConstExpr::Function {
            callee: Callee::System(function),
            args,
            ..
        } => match function {
            SystemFunction::Signed | SystemFunction::Unsigned => {
                let [arg] = args.as_slice() else { return None };
                let mut ty = infer_const_expr_type_inner(arg, parameter_types, bounded_proof)?;
                ty.signed = *function == SystemFunction::Signed;
                Some(ty)
            }
            function => function
                .result_type()
                .map(|(width, signed)| ExprType { width, signed }),
        },
        ConstExpr::Function { .. } => None,
        ConstExpr::Unary { op, expr } => {
            let operand = infer_const_expr_type_inner(expr, parameter_types, bounded_proof)?;
            if matches!(
                op,
                UnaryOp::LogicNot | UnaryOp::RedAnd | UnaryOp::RedOr | UnaryOp::RedXor
            ) {
                Some(ExprType {
                    width: 1,
                    signed: false,
                })
            } else {
                Some(operand)
            }
        }
        ConstExpr::Binary { left, op, right } => {
            if bounded_proof && matches!(op, BinaryOp::LogicAnd | BinaryOp::LogicOr) {
                return Some(ExprType {
                    width: 1,
                    signed: false,
                });
            }
            let left = infer_const_expr_type_inner(left, parameter_types, bounded_proof)?;
            let right = infer_const_expr_type_inner(right, parameter_types, bounded_proof)?;
            match op {
                BinaryOp::LogicAnd
                | BinaryOp::LogicOr
                | BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::EqCase
                | BinaryOp::NeCase
                | BinaryOp::EqWildcard
                | BinaryOp::NeWildcard
                | BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge => Some(ExprType {
                    width: 1,
                    signed: false,
                }),
                BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => Some(left),
                _ => Some(ExprType {
                    width: left.width.max(right.width),
                    signed: left.signed && right.signed,
                }),
            }
        }
        ConstExpr::Mux {
            then_expr,
            else_expr,
            ..
        } => {
            let then_type = infer_const_expr_type_inner(then_expr, parameter_types, bounded_proof)?;
            let else_type = infer_const_expr_type_inner(else_expr, parameter_types, bounded_proof)?;
            Some(ExprType {
                width: then_type.width.max(else_type.width),
                signed: then_type.signed && else_type.signed,
            })
        }
    }
}

pub(super) fn infer_parameter_value_type(
    value: &ConstExpr,
    has_declared_type: bool,
    parameter_types: &HashMap<String, ExprType>,
) -> Option<ExprType> {
    if !has_declared_type
        && let ConstExpr::Literal(literal) = value
        && matches!(
            literal.trim(),
            "'0" | "'1" | "'x" | "'X" | "'z" | "'Z" | "'?"
        )
    {
        Some(ExprType {
            width: 1,
            signed: false,
        })
    } else {
        infer_const_expr_type(value, parameter_types)
    }
}

#[cfg(test)]
thread_local! {
    pub(super) static PARAMETER_VALUE_MATERIALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static DECLARATION_ENV_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static PARAMETER_BINDINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static LITERAL_ENV_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static LITERAL_RESOLUTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;

/// Resolve real parameter expressions without passing IEEE payloads through integer arithmetic.
pub(super) fn real_literal(
    expr: &ConstExpr,
    kind: TypeKind,
    constants: &HashMap<String, i128>,
    literals: &HashMap<String, Expr>,
) -> Option<Expr> {
    let expanded = substitute_expr_idents(const_expr_to_expr(expr.clone()), literals);
    let expr = expr_to_const(&AnalysisState::default(), expanded)?;
    let value = typecheck::eval_real_const_expr(&expr.into(), constants)?;
    let value = if kind == TypeKind::Shortreal {
        f64::from(value as f32)
    } else {
        value
    };
    Some(if value.is_finite() {
        Expr::Literal(format!("{value:?}"))
    } else {
        Expr::Call {
            callee: Callee::System(SystemFunction::Bitstoreal),
            args: vec![Some(Expr::Literal(format!("64'h{:x}", value.to_bits())))],
            arg_order: Vec::new(),
        }
    })
}
