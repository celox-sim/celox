//! Parameter environments, enum constants, and typed parameter substitution.

use super::*;

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
            parameter.value = Some(value.clone());
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

pub(super) fn parameters_from_ref_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    parameters: &mut Vec<Parameter>,
    is_local: bool,
    base_const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Result<(), AnalyzerError> {
    // Restrict declaration-type queries to the header. Walking the complete
    // declaration also visits data types and ranges nested in initializers,
    // including the target of a size-function cast.
    let declaration_node = match node.clone() {
        RefNode::ParameterPortDeclaration(
            sv_parser::ParameterPortDeclaration::ParameterDeclaration(declaration),
        ) => RefNode::ParameterDeclaration(declaration),
        RefNode::ParameterPortDeclaration(
            sv_parser::ParameterPortDeclaration::LocalParameterDeclaration(declaration),
        ) => RefNode::LocalParameterDeclaration(declaration),
        RefNode::ParameterPortDeclaration(sv_parser::ParameterPortDeclaration::ParamList(
            declaration,
        )) => RefNode::DataType(&declaration.nodes.0),
        node => node,
    };
    let type_node = match declaration_node {
        RefNode::ParameterDeclaration(sv_parser::ParameterDeclaration::Param(declaration)) => {
            RefNode::DataTypeOrImplicit(&declaration.nodes.1)
        }
        RefNode::LocalParameterDeclaration(sv_parser::LocalParameterDeclaration::Param(
            declaration,
        )) => RefNode::DataTypeOrImplicit(&declaration.nodes.1),
        node => node,
    };
    if type_node.clone().into_iter().any(|child| {
        matches!(
            child,
            RefNode::DataTypeOrImplicit(sv_parser::DataTypeOrImplicit::DataType(data_type))
                if !matches!(
                    &**data_type,
                    sv_parser::DataType::Vector(_)
                        | sv_parser::DataType::Atom(_)
                        | sv_parser::DataType::Type(_)
                        | sv_parser::DataType::ClassType(_)
                )
        )
    }) {
        return Err(AnalyzerError::Unsupported(
            "unsupported parameter data type".to_string(),
        ));
    }
    let declared_alias = type_alias_from_ref_node(type_node.clone(), syntax_tree, type_aliases);
    let parameter_width = parameter_declared_width(
        type_node.clone(),
        syntax_tree,
        base_const_env,
        parameters,
        type_aliases,
        parameter_overrides,
    );
    let has_declared_type = type_node.clone().into_iter().any(|child| {
        matches!(
            child,
            RefNode::DataTypeOrImplicit(sv_parser::DataTypeOrImplicit::DataType(_))
                | RefNode::DataType(_)
        )
    });
    // IEEE 1800-2023 7.4.1: a packed array is signed only when declared so,
    // whatever its elements; a type name takes no signing of its own.
    let alias_has_use_site_dimensions = declared_alias.is_some()
        && type_node
            .clone()
            .into_iter()
            .any(|child| matches!(child, RefNode::PackedDimension(_)));
    let parameter_signed = parameter_width.map(|_| {
        declared_alias
            .as_ref()
            .map(|alias| alias.is_signed() && !alias_has_use_site_dimensions)
            .unwrap_or_else(|| {
                integer_atom_expr_type(type_node.clone())
                    .map(|r#type| r#type.signed)
                    .unwrap_or_else(|| is_signed_from_ref_node(type_node.clone()).unwrap_or(false))
            })
    });
    let parameter_signed_element_depth =
        signed_element_depth_from_ref_node(type_node.clone(), syntax_tree, type_aliases);
    let parameter_ranges =
        function_type_from_ref_node(type_node.clone(), syntax_tree, base_const_env, type_aliases)
            .map(|ty| ty.packed_ranges().to_vec())
            .unwrap_or_default();
    let parameter_is_2state = declared_alias
        .or_else(|| type_from_ref_node(type_node, syntax_tree))
        .is_some_and(|r#type| r#type.kind() == TypeKind::Bit);
    for child in node {
        if let RefNode::ParamAssignment(param) = child {
            // An unpacked array parameter is a constant variable, not a value.
            if array_parameters::is_array_parameter(param) {
                continue;
            }
            let name = parameter_name(RefNode::ParameterIdentifier(&param.nodes.0), syntax_tree)?;
            let mut const_env = base_const_env.clone();
            const_env.extend(const_env_from_parameters(parameters));
            let mut value = if let Some((_, expr)) = &param.nodes.2 {
                if expr.into_iter().any(|node| {
                    matches!(
                        node,
                        RefNode::ConstantIndexedRange(_) | RefNode::IndexedRange(_)
                    )
                }) {
                    let mut dimensions = PackedDimensions::new(
                        parameter_packed_dimensions(parameters),
                        &const_env,
                        type_aliases,
                    );
                    dimensions.parameter_values = parameter_value_env(parameters, &const_env);
                    // Enum constants are unavailable during preliminary collection.
                    // Leave unresolved values for the subsequent lowering pass;
                    // final validation rejects anything that still cannot be lowered.
                    selects::indexed_parameter_initializer(
                        expr,
                        syntax_tree,
                        &dimensions,
                        parameter_width,
                    )
                    .ok()
                } else {
                    const_expr_from_constant_param_with_env(
                        expr,
                        syntax_tree,
                        &const_env,
                        type_aliases,
                    )?
                }
            } else {
                None
            };
            value =
                normalize_unbased_unsized_parameter_value(value, parameter_width, parameter_signed);
            // Apply overrides as each declaration is collected so later
            // parameter initializers (including casts) are evaluated from the
            // specialized values rather than defaults that will be replaced
            // only after collection has finished.
            if !is_local && let Some(override_value) = parameter_overrides.get(&name) {
                value = Some(override_value.clone());
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
            parameter.packed_ranges = parameter_ranges.clone();
            parameter.signed_element_depth = parameter_signed_element_depth;
            parameters.push(parameter);
        }
    }
    Ok(())
}

fn parameter_declared_width(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    base_const_env: &HashMap<String, i128>,
    parameters: &[Parameter],
    type_aliases: &HashMap<String, Type>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Option<usize> {
    let declared_alias = type_alias_from_ref_node(node.clone(), syntax_tree, type_aliases);
    let mut range_env = base_const_env.clone();
    range_env.extend(const_env_from_parameters(parameters));
    // Numeric-size casts in a later parameter declaration can refer to an
    // earlier assignment in the same parameter-port list. Seed range lowering
    // from those assignments while retaining the separate environment below
    // for evaluating the resulting symbolic ranges without stale self-values.
    for child in node.clone() {
        let RefNode::ParamAssignment(parameter) = child else {
            continue;
        };
        let Ok(name) = parameter_name(
            RefNode::ParameterIdentifier(&parameter.nodes.0),
            syntax_tree,
        ) else {
            continue;
        };
        let value = parameter_overrides
            .get(&name)
            .cloned()
            .or_else(|| {
                parameter.nodes.2.as_ref().and_then(|(_, expr)| {
                    const_expr_from_constant_param_with_env(
                        expr,
                        syntax_tree,
                        &range_env,
                        type_aliases,
                    )
                    .ok()
                    .flatten()
                })
            })
            .and_then(|value| eval_ast_const_expr(&value, &range_env));
        if let Some(value) = value {
            range_env.insert(name, value);
        }
    }
    let mut env = base_const_env.clone();
    // A second lowering pass receives values from the first pass in the base
    // environment. Do not let stale values for parameters declared by this
    // same syntax node resolve its declaration ranges; only constants from
    // outside the declaration (such as enum members) belong here.
    for child in node.clone() {
        if let RefNode::ParamAssignment(parameter) = child
            && let Ok(name) = parameter_name(
                RefNode::ParameterIdentifier(&parameter.nodes.0),
                syntax_tree,
            )
        {
            env.remove(&name);
        }
    }
    env.extend(const_env_from_parameters(parameters));
    let mut ranges =
        packed_ranges_from_ref_node_with_env(node.clone(), syntax_tree, &range_env, type_aliases);
    if let Some(alias) = &declared_alias {
        // Use-site dimensions enclose the aliased packed type, just as they
        // do for ports, signals, and function types.
        ranges.extend(alias.packed_ranges.iter().cloned());
    }
    if ranges.is_empty() {
        if declared_alias.is_some() {
            return Some(1);
        }
        if let Some(r#type) = integer_atom_expr_type(node.clone()) {
            return Some(r#type.width);
        }
        return unwrap_node!(node, IntegerVectorType).is_some().then_some(1);
    }
    ranges.iter().try_fold(1usize, |acc, range| {
        let left = eval_ast_const_expr(range.left(), &env)?;
        let right = eval_ast_const_expr(range.right(), &env)?;
        acc.checked_mul(left.abs_diff(right) as usize + 1)
    })
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

pub(super) fn const_env_from_parameters(parameters: &[Parameter]) -> HashMap<String, i128> {
    let mut env = HashMap::default();
    extend_const_env_with_parameters(&mut env, parameters);
    env
}

pub(super) fn extend_const_env_with_parameters(
    env: &mut HashMap<String, i128>,
    parameters: &[Parameter],
) {
    let mut parameter_types = parameter_types_from_const_env(env);
    for parameter in parameters {
        let Some(value) = parameter.resolved_value(env, &parameter_types) else {
            continue;
        };
        if let Some(r#type) = parameter.resolved_type(&parameter_types) {
            parameter_types.insert(parameter.name().to_string(), r#type);
            insert_parameter_type_markers(env, parameter.name(), r#type);
        }
        env.insert(parameter.name().to_string(), value);
        env.insert(parameter_marker(parameter.name()), value);
        if parameter.is_local {
            env.insert(local_parameter_marker(parameter.name()), value);
        }
        insert_parameter_dimension_markers(env, parameter);
    }
}

/// Record the packed dimensions of a parameter whose selects need them: an
/// array of several dimensions, or of a signed named type.
fn insert_parameter_dimension_markers(env: &mut HashMap<String, i128>, parameter: &Parameter) {
    let name = parameter.name();
    if parameter.packed_ranges.len() < 2 && parameter.signed_element_depth.is_none() {
        return;
    }
    let Some(bounds) = parameter
        .packed_ranges
        .iter()
        .map(|range| {
            Some((
                eval_ast_const_expr(range.left(), env)?,
                eval_ast_const_expr(range.right(), env)?,
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
    if total_width > 127 {
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
    let value = *env.get(name)? as u128 & ((1u128 << total_width) - 1);
    let element = (value >> offset) & ((1u128 << element_width) - 1);
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

fn enum_initializer_fits_base_type(value: i128, value_type: ExprType, base_type: ExprType) -> bool {
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
/// declarations. Members must carry explicit values, matching what Veryl
/// emits; each initializer may reference previously declared members of the
/// same module scope.
pub(super) fn enum_member_constants_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    base_const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Result<EnumMemberConstants, AnalyzerError> {
    let mut constants = EnumMemberConstants::default();
    let mut eval_env = base_const_env.clone();
    let mut resolved_type_aliases = type_aliases.clone();
    let mut parameters = Vec::new();
    for item in module_non_port_items(node.clone()) {
        let Some(declaration) = package_or_generate_declaration_from_non_port_item(item) else {
            continue;
        };
        let data = match declaration {
            sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(localparam) => {
                parameters_from_ref_node(
                    RefNode::LocalParameterDeclaration(&localparam.0),
                    syntax_tree,
                    &mut parameters,
                    true,
                    &eval_env,
                    &resolved_type_aliases,
                    &HashMap::default(),
                )?;
                extend_const_env_with_parameters(&mut eval_env, &parameters);
                resolved_type_aliases =
                    type_aliases_from_module_node_with_env(node.clone(), syntax_tree, &eval_env)?;
                continue;
            }
            sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(parameter) => {
                parameters_from_ref_node(
                    RefNode::ParameterDeclaration(&parameter.0),
                    syntax_tree,
                    &mut parameters,
                    false,
                    &eval_env,
                    &resolved_type_aliases,
                    parameter_overrides,
                )?;
                extend_const_env_with_parameters(&mut eval_env, &parameters);
                resolved_type_aliases =
                    type_aliases_from_module_node_with_env(node.clone(), syntax_tree, &eval_env)?;
                continue;
            }
            sv_parser::PackageOrGenerateItemDeclaration::DataDeclaration(data) => data,
            _ => continue,
        };
        let sv_parser::DataDeclaration::TypeDeclaration(type_declaration) = &**data else {
            continue;
        };
        let sv_parser::TypeDeclaration::DataType(type_declaration) = &**type_declaration else {
            continue;
        };
        let sv_parser::DataType::Enum(r#enum) = &type_declaration.nodes.1 else {
            continue;
        };
        let member_type = match &r#enum.nodes.1 {
            Some(base) => type_from_ref_node_with_env(
                RefNode::EnumBaseType(base),
                syntax_tree,
                &eval_env,
                &resolved_type_aliases,
            )
            .or_else(|| {
                type_alias_from_ref_node(
                    RefNode::EnumBaseType(base),
                    syntax_tree,
                    &resolved_type_aliases,
                )
            })
            .and_then(|r#type| expr_type_from_type(&r#type, &eval_env)),
            None => Some(ExprType {
                width: 32,
                signed: true,
            }),
        }
        .ok_or_else(|| AnalyzerError::Unsupported("enum base type".to_string()))?;
        let mut next_enum_value = 0i128;
        for member in r#enum.nodes.2.nodes.1.contents() {
            let name = identifier_text(RefNode::Identifier(&member.nodes.0.nodes.0), syntax_tree)
                .ok_or_else(|| {
                AnalyzerError::Unsupported("enum member identifier".to_string())
            })?;
            if member.nodes.1.is_some() {
                return Err(AnalyzerError::Unsupported(format!(
                    "ranged enum member `{name}`"
                )));
            }
            let value = match &member.nodes.2 {
                Some((_, value)) => const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(value),
                    syntax_tree,
                    &eval_env,
                    &resolved_type_aliases,
                )?
                .ok_or_else(|| AnalyzerError::Unsupported(format!("enum member `{name}` value")))?,
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
            let number = eval_ast_const_expr(&value, &eval_env).ok_or_else(|| {
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
        resolved_type_aliases =
            type_aliases_from_module_node_with_env(node.clone(), syntax_tree, &eval_env)?;
    }
    Ok(constants)
}

pub(super) fn parameter_value_env(
    parameters: &[Parameter],
    const_env: &HashMap<String, i128>,
) -> HashMap<String, Expr> {
    let mut values = HashMap::default();
    let mut parameter_types = HashMap::default();
    for parameter in parameters {
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
            let value = replace_oob_const_selects_with_unknown(value, const_env);
            let value = substitute_expr_constants_with_parameter_literals(
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
        values.insert(parameter.name().to_string(), value);
    }
    values
}

fn replace_oob_const_selects_with_unknown(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
) -> ConstExpr {
    match expr {
        ConstExpr::Select { expr, bit } => {
            let expr = replace_oob_const_selects_with_unknown(*expr, const_env);
            let bit = replace_oob_const_selects_with_unknown(*bit, const_env);
            if let ConstExpr::Literal(literal) = &expr
                && let Some(width) =
                    typecheck::parse_integral_literal(literal).map(|literal| literal.width)
                && match typecheck::eval_const_expr(&bit.clone().into(), const_env) {
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
        ConstExpr::Function { name, args } => ConstExpr::Function {
            name,
            args: args
                .into_iter()
                .map(|arg| replace_oob_const_selects_with_unknown(arg, const_env))
                .collect(),
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op,
            expr: Box::new(replace_oob_const_selects_with_unknown(*expr, const_env)),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: Box::new(replace_oob_const_selects_with_unknown(*left, const_env)),
            op,
            right: Box::new(replace_oob_const_selects_with_unknown(*right, const_env)),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: Box::new(replace_oob_const_selects_with_unknown(
                *condition, const_env,
            )),
            then_expr: Box::new(replace_oob_const_selects_with_unknown(
                *then_expr, const_env,
            )),
            else_expr: Box::new(replace_oob_const_selects_with_unknown(
                *else_expr, const_env,
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
        ConstExpr::Select { expr, bit } => {
            const_expr_contains_unknown_literal(expr) || const_expr_contains_unknown_literal(bit)
        }
        ConstExpr::Function { args, .. } => args.iter().any(const_expr_contains_unknown_literal),
        ConstExpr::Unary { expr, .. } => const_expr_contains_unknown_literal(expr),
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
        ConstExpr::Select { expr, bit } => Expr::Select {
            expr: Box::new(const_expr_to_expr(*expr)),
            msb: (*bit).clone(),
            lsb: *bit,
            signed: false,
        },
        ConstExpr::Function { name, args } => Expr::Call {
            name,
            args: args.into_iter().map(const_expr_to_expr).collect(),
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
    match expr {
        ConstExpr::Ident(name) => match (constants.get(&name), parameter_types.get(&name)) {
            (Some(value), Some(r#type)) => ConstExpr::Literal(format_typed_parameter_literal(
                *value,
                r#type.width,
                r#type.signed,
            )),
            _ => ConstExpr::Ident(name),
        },
        ConstExpr::Literal(value) => ConstExpr::Literal(value),
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: Box::new(substitute_typed_parameter_literals(
                *expr,
                constants,
                parameter_types,
            )),
            bit: Box::new(substitute_typed_parameter_literals(
                *bit,
                constants,
                parameter_types,
            )),
        },
        ConstExpr::Function { name, args } => ConstExpr::Function {
            name,
            args: args
                .into_iter()
                .map(|arg| substitute_typed_parameter_literals(arg, constants, parameter_types))
                .collect(),
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op,
            expr: Box::new(substitute_typed_parameter_literals(
                *expr,
                constants,
                parameter_types,
            )),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: Box::new(substitute_typed_parameter_literals(
                *left,
                constants,
                parameter_types,
            )),
            op,
            right: Box::new(substitute_typed_parameter_literals(
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
            condition: Box::new(substitute_typed_parameter_literals(
                *condition,
                constants,
                parameter_types,
            )),
            then_expr: Box::new(substitute_typed_parameter_literals(
                *then_expr,
                constants,
                parameter_types,
            )),
            else_expr: Box::new(substitute_typed_parameter_literals(
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
    match expr {
        ConstExpr::Literal(literal) => {
            let literal = typecheck::parse_integral_literal(literal)?;
            Some(ExprType {
                width: literal.width,
                signed: literal.signed,
            })
        }
        ConstExpr::Ident(name) => parameter_types.get(name).copied(),
        ConstExpr::Select { .. } => Some(ExprType {
            width: 1,
            signed: false,
        }),
        ConstExpr::Function { name, .. } => match name.as_str() {
            "$clog2" | "$countones" => Some(ExprType {
                width: 32,
                signed: true,
            }),
            "$onehot" | "$onehot0" | "$isunknown" => Some(ExprType {
                width: 1,
                signed: false,
            }),
            _ => None,
        },
        ConstExpr::Unary { op, expr } => {
            let operand = infer_const_expr_type(expr, parameter_types)?;
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
            let left = infer_const_expr_type(left, parameter_types)?;
            let right = infer_const_expr_type(right, parameter_types)?;
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
            let then_type = infer_const_expr_type(then_expr, parameter_types)?;
            let else_type = infer_const_expr_type(else_expr, parameter_types)?;
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
