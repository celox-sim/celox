//! Instances of a module: parameter overrides and port connections.

use super::*;

pub(super) fn evaluated_parameter_overrides(
    parameter_overrides: &[LoweredSvParameterOverride],
) -> Result<HashMap<String, sv::ir::ConstExpr>, sv::AnalyzerError> {
    let constants = HashMap::default();
    let mut evaluated = HashMap::default();
    for parameter in parameter_overrides {
        let Some(value) = parameter.value.as_ref() else {
            continue;
        };
        if !matches!(value, sv::ir::ConstExpr::Literal(value) if value == "$") {
            let integral = sv::typecheck::eval_const_integral_literal_with_types(
                value,
                &constants,
                &HashMap::default(),
            );
            if integral.is_none()
                && sv::typecheck::eval_const_real_expr(value, &constants).is_none()
            {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "nonconstant module parameter override `{}`",
                    parameter.name
                )));
            }
        }
        evaluated.insert(parameter.name.clone(), value.clone());
    }
    Ok(evaluated)
}

/// The instances of a specialized module, one per element of an instance
/// array, without those of false conditional-generate branches.
pub(super) fn lower_instances(
    module: &sv::ir::Module,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Result<Vec<LoweredSvInstance>, sv::AnalyzerError> {
    // A string parameter longer than an `i128` has no entry in `constants`;
    // keep its literal so a `$sformatf` override can still take its value.
    // Resolve the parameters in declaration order, with the wider literals
    // already kept substituted in, so a parameter that takes its value from
    // an earlier one (`TAG = ID`) is kept as well.
    let mut literals: HashMap<String, sv::ir::ConstExpr> = HashMap::default();
    for parameter in module
        .parameters()
        .iter()
        .chain(module.imported_parameters())
    {
        let Some(value) = parameter.value() else {
            continue;
        };
        let value = substitute_parameter_literals(value.clone(), &literals);
        let Some(literal) = sv::typecheck::eval_const_integral_literal_with_types(
            &value,
            constants,
            parameter_types,
        ) else {
            continue;
        };
        if literal.width > 128 {
            literals.insert(
                parameter.name().to_string(),
                sv::ir::ConstExpr::Literal(sv::typecheck::format_integral_literal_binary(&literal)),
            );
        }
    }
    let mut instances = Vec::new();
    for instance in module.instances() {
        if let Some(condition) = instance.condition() {
            let condition =
                sv::typecheck::eval_const_expr_with_types(condition, constants, parameter_types)
                    .ok_or_else(|| {
                        sv::AnalyzerError::Unsupported(
                            "unknown conditional-generate condition".to_string(),
                        )
                    })?;
            if condition == 0 {
                continue;
            }
        }
        // An instance array is one instance per element, all under one name.
        // Elements are listed from the lowest index up and numbered from the
        // lower bound, so the hierarchy uses the declared indices; each keeps
        // its position in declaration order, which decides its connections.
        let array_index_base = match instance.array_range() {
            Some((left, right)) => usize::try_from(left.min(right)).map_err(|_| {
                sv::AnalyzerError::Unsupported(format!(
                    "module instance array `{}` with a negative index",
                    instance.name()
                ))
            })?,
            None => 0,
        };
        let elements = instance.array_len().map_or(vec![None], |len| {
            let descending = instance
                .array_range()
                .is_some_and(|(left, right)| left >= right);
            (0..len)
                .map(|offset| {
                    let position = if descending { len - 1 - offset } else { offset };
                    Some((position, len))
                })
                .collect()
        });
        for array_element in elements {
            instances.push(LoweredSvInstance {
                module_name: instance.module_name().to_string(),
                instance_name: instance.name().to_string(),
                force_signals: Vec::new(),
                parameter_overrides: lower_parameter_overrides(
                    instance,
                    constants,
                    parameter_types,
                    &literals,
                ),
                port_connections: instance
                    .port_connections()
                    .iter()
                    .map(|connection| LoweredSvPortConnection {
                        formal: connection.formal().to_string(),
                        actual: connection.actual().to_string(),
                        actual_expr: connection.actual_expr().cloned(),
                        bound_pattern: connection.bound_pattern().cloned(),
                    })
                    .collect(),
                array_element,
                array_index_base,
            });
        }
    }
    Ok(instances)
}

/// `expr` with each identifier that `literals` holds replaced by its
/// literal, as `substitute_typed_constants` does for the values that fit
/// an `i128`.
fn substitute_parameter_literals(
    expr: sv::ir::ConstExpr,
    literals: &HashMap<String, sv::ir::ConstExpr>,
) -> sv::ir::ConstExpr {
    let sub = |expr: sv::ir::ConstExpr| substitute_parameter_literals(expr, literals);
    match expr {
        sv::ir::ConstExpr::Ident(name) => literals
            .get(&name)
            .cloned()
            .unwrap_or(sv::ir::ConstExpr::Ident(name)),
        sv::ir::ConstExpr::Literal(_) => expr,
        sv::ir::ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => sv::ir::ConstExpr::SelectRange {
            expr: Box::new(sub(*expr)),
            msb: Box::new(sub(*msb)),
            lsb: Box::new(sub(*lsb)),
            signed,
        },
        sv::ir::ConstExpr::Select { expr, bit } => sv::ir::ConstExpr::Select {
            expr: Box::new(sub(*expr)),
            bit: Box::new(sub(*bit)),
        },
        sv::ir::ConstExpr::Function { callee, args, site } => sv::ir::ConstExpr::Function {
            callee,
            args: args.into_iter().map(sub).collect(),
            site,
        },
        sv::ir::ConstExpr::Resize {
            expr,
            width,
            signed,
        } => sv::ir::ConstExpr::Resize {
            expr: Box::new(sub(*expr)),
            width,
            signed,
        },
        sv::ir::ConstExpr::Unary { op, expr } => sv::ir::ConstExpr::Unary {
            op,
            expr: Box::new(sub(*expr)),
        },
        sv::ir::ConstExpr::Binary { left, op, right } => sv::ir::ConstExpr::Binary {
            left: Box::new(sub(*left)),
            op,
            right: Box::new(sub(*right)),
        },
        sv::ir::ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => sv::ir::ConstExpr::Mux {
            condition: Box::new(sub(*condition)),
            then_expr: Box::new(sub(*then_expr)),
            else_expr: Box::new(sub(*else_expr)),
        },
    }
}

fn lower_parameter_overrides(
    instance: &sv::ir::Instance,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    literals: &HashMap<String, sv::ir::ConstExpr>,
) -> Vec<LoweredSvParameterOverride> {
    instance
        .parameter_overrides()
        .iter()
        .map(|parameter| {
            let value = parameter.value().cloned().map(|value| {
                let value = substitute_parameter_literals(value, literals);
                let value =
                    sv::typecheck::substitute_typed_constants(value, constants, parameter_types);
                // `$sformatf` of constants is the string it makes (IEEE
                // 1800-2023 11.2.1), which specializes the instance.
                if let sv::ir::ConstExpr::Function {
                    callee: sv::ir::Callee::String(sv::ir::StringOp::Format { .. }),
                    ..
                } = &value
                    && let Some(literal) = sv::typecheck::eval_const_integral_literal_with_types(
                        &value,
                        constants,
                        parameter_types,
                    )
                {
                    return sv::ir::ConstExpr::Literal(
                        sv::typecheck::format_integral_literal_binary(&literal),
                    );
                }
                if const_expr_references_identifier(&value) {
                    sv::typecheck::eval_const_expr_with_types(&value, constants, parameter_types)
                        .map(const_expr_from_i128)
                        .unwrap_or(value)
                } else {
                    value
                }
            });
            LoweredSvParameterOverride {
                name: parameter.name().to_string(),
                value,
                bound_type: parameter.bound_type().cloned(),
                bound_pattern: parameter.bound_pattern().cloned(),
            }
        })
        .collect()
}

pub(super) fn const_expr_references_identifier(expr: &sv::ir::ConstExpr) -> bool {
    match expr {
        sv::ir::ConstExpr::Ident(_) => true,
        sv::ir::ConstExpr::Literal(_) => false,
        sv::ir::ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            const_expr_references_identifier(expr)
                || const_expr_references_identifier(msb)
                || const_expr_references_identifier(lsb)
        }
        sv::ir::ConstExpr::Select { expr, bit } => {
            const_expr_references_identifier(expr) || const_expr_references_identifier(bit)
        }
        sv::ir::ConstExpr::Function { args, .. } => {
            args.iter().any(const_expr_references_identifier)
        }
        sv::ir::ConstExpr::Resize { expr, .. } | sv::ir::ConstExpr::Unary { expr, .. } => {
            const_expr_references_identifier(expr)
        }
        sv::ir::ConstExpr::Binary { left, right, .. } => {
            const_expr_references_identifier(left) || const_expr_references_identifier(right)
        }
        sv::ir::ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            const_expr_references_identifier(condition)
                || const_expr_references_identifier(then_expr)
                || const_expr_references_identifier(else_expr)
        }
    }
}

pub(super) fn const_expr_from_i128(value: i128) -> sv::ir::ConstExpr {
    if value < 0 {
        sv::ir::ConstExpr::Unary {
            op: sv::ir::UnaryOp::Minus,
            expr: Box::new(sv::ir::ConstExpr::Literal(value.unsigned_abs().to_string())),
        }
    } else {
        sv::ir::ConstExpr::Literal(value.to_string())
    }
}

/// The port connections of element `position` of an array of `count`
/// instances. A connection as wide as the port is shared by every element; one
/// `count` times as wide is divided between them, the first element taking the
/// most significant slice.
pub(super) fn array_element_connections(
    connections: &[LoweredSvPortConnection],
    child: &LoweredSvModule,
    position: usize,
    count: usize,
    parent_variables: &HashMap<SourceVarId, SvVariable>,
    parent_signal_names: &HashMap<String, SourceVarId>,
    parent_constants: &HashMap<String, i128>,
    parent_parameter_types: &HashMap<String, (usize, bool)>,
) -> Result<Vec<LoweredSvPortConnection>, ParserError> {
    let unsupported = |detail: String| {
        ParserError::unsupported(
            sv::SV_FRONTEND_TRACKING_ISSUE,
            LoweringPhase::SimulatorParser,
            "systemverilog module instance array",
            detail,
            None,
        )
    };
    connections
        .iter()
        .map(|connection| {
            let Some(actual_expr) = connection.actual_expr.as_ref() else {
                return Ok(connection.clone());
            };
            let Some(port_width) = child
                .signal_names
                .get(&connection.formal)
                .and_then(|id| child.variables.get(id))
                .map(|variable| variable.width)
            else {
                return Ok(connection.clone());
            };
            let Some(actual_width) = sv_expr_natural_width(
                actual_expr,
                parent_variables,
                parent_signal_names,
                parent_constants,
                parent_parameter_types,
            ) else {
                return Ok(connection.clone());
            };
            if actual_width == port_width {
                return Ok(connection.clone());
            }
            // Any other width is an error (IEEE 1800-2023 23.3.3.5), including
            // a fill literal ('0 is one bit) or an unsized constant (32 bits).
            let mismatch = || {
                ParserError::illegal_context(
                    "systemverilog module instance array connection",
                    format!(
                        "`{}` is {actual_width} bits wide; a {port_width}-bit port of {count} elements needs {port_width} or {} bits",
                        connection.formal,
                        port_width * count
                    ),
                    None,
                )
            };
            if actual_width != port_width * count {
                return Err(mismatch());
            }
            let sv::ir::Expr::Ident(name) = actual_expr else {
                return Err(unsupported(format!(
                    "`{}` is split between the elements only from a named vector or array",
                    connection.formal
                )));
            };
            let unpacked = parent_signal_names
                .get(name)
                .and_then(|id| parent_variables.get(id))
                .is_some_and(|variable| variable.array_dims == [count]);
            if unpacked && actual_width == port_width * count {
                // An unpacked array of `count` elements: element `i` of the
                // instance array takes array element `i`.
                let lsb = position * port_width;
                return Ok(LoweredSvPortConnection {
                    formal: connection.formal.clone(),
                    actual: connection.actual.clone(),
                    bound_pattern: None,
                    actual_expr: Some(sv::ir::Expr::Select {
                        expr: Box::new(actual_expr.clone()),
                        msb: sv::ir::ConstExpr::Literal((lsb + port_width - 1).to_string()),
                        lsb: sv::ir::ConstExpr::Literal(lsb.to_string()),
                        signed: false,
                    }),
                });
            }
            let zero_based = parent_signal_names
                .get(name)
                .and_then(|id| parent_variables.get(id))
                .is_some_and(|variable| {
                    variable.array_dims.is_empty()
                        && matches!(
                            variable.packed_ranges.as_slice(),
                            [] | [(_, 0)]
                        )
                });
            if !zero_based {
                return Err(unsupported(format!(
                    "`{}` is split between the elements only from a vector declared [N-1:0]",
                    connection.formal
                )));
            }
            let lsb = (count - 1 - position) * port_width;
            let slice = sv::ir::Expr::Select {
                expr: Box::new(actual_expr.clone()),
                msb: sv::ir::ConstExpr::Literal((lsb + port_width - 1).to_string()),
                lsb: sv::ir::ConstExpr::Literal(lsb.to_string()),
                signed: false,
            };
            Ok(LoweredSvPortConnection {
                formal: connection.formal.clone(),
                actual: connection.actual.clone(),
                actual_expr: Some(slice),
                bound_pattern: None,
            })
        })
        .collect()
}

/// Tagged unions cannot acquire an arbitrary tag through an integral port connection.
pub(super) fn check_tagged_port_connections(
    parent: &sv::ir::Module,
    child: &sv::ir::Module,
    connections: &[LoweredSvPortConnection],
) -> Result<(), ParserError> {
    let type_of = |name: &str| {
        parent
            .ports()
            .iter()
            .find(|port| port.name() == name)
            .map(|port| port.r#type())
            .or_else(|| {
                parent
                    .signals()
                    .iter()
                    .find(|signal| signal.name() == name)
                    .map(|signal| signal.r#type())
            })
    };
    for connection in connections {
        let Some(port) = child
            .ports()
            .iter()
            .find(|port| port.name() == connection.formal)
        else {
            continue;
        };
        let Some(expr) = &connection.actual_expr else {
            continue;
        };
        let actual = if let sv::ir::Expr::Ident(name) = expr {
            type_of(name)
        } else {
            None
        };
        let writes_actual = matches!(
            port.direction(),
            sv::ir::PortDirection::Output
                | sv::ir::PortDirection::Inout
                | sv::ir::PortDirection::Ref
        );
        let mut writes_tagged = false;
        if writes_actual {
            let mut pending = vec![expr];
            while let Some(part) = pending.pop() {
                match part {
                    sv::ir::Expr::Ident(name) => {
                        writes_tagged |=
                            type_of(name).is_some_and(|ty| ty.tagged_identity().is_some());
                    }
                    sv::ir::Expr::Concat(parts) => pending.extend(parts),
                    sv::ir::Expr::Select { expr, .. } | sv::ir::Expr::Resize { expr, .. } => {
                        pending.push(expr)
                    }
                    _ => {}
                }
            }
        }
        let target = port.r#type();
        if target.tagged_identity().is_none() && !writes_tagged {
            continue;
        }
        if !actual.is_some_and(|ty| {
            ty.tagged_identity() == target.tagged_identity()
                && ty.resolved_width() == target.resolved_width()
                && ty.unpacked_ranges() == target.unpacked_ranges()
        }) {
            return Err(ParserError::illegal_context(
                "systemverilog tagged union port",
                format!(
                    "port `{}` requires a signal of the same tagged union type",
                    connection.formal
                ),
                None,
            ));
        }
    }
    Ok(())
}

/// Connect unpacked structures through their independently typed leaf ports.
/// Aggregate declarations remain in source metadata for nominal type checking.
pub(super) fn expand_structure_connections<'a>(
    connections: &'a [LoweredSvPortConnection],
    parent: &sv::ir::Module,
    child: &sv::ir::Module,
) -> Result<std::borrow::Cow<'a, [LoweredSvPortConnection]>, ParserError> {
    if !child
        .ports()
        .iter()
        .any(|port| port.r#type().structure_identity().is_some())
    {
        return Ok(std::borrow::Cow::Borrowed(connections));
    }
    let mut expanded = Vec::new();
    let mut connected_structures = HashSet::default();
    for connection in connections {
        let Some(port) = child.ports().iter().find(|port| {
            port.name() == connection.formal && port.r#type().structure_identity().is_some()
        }) else {
            expanded.push(connection.clone());
            continue;
        };
        let invalid = |message: String| {
            ParserError::illegal_context(
                "systemverilog unpacked structure port connection",
                message,
                None,
            )
        };
        if !connected_structures.insert(&connection.formal) {
            return Err(invalid(format!(
                "duplicate connection of port `{}`",
                connection.formal
            )));
        }
        if let Some(pattern) = &connection.bound_pattern {
            if port.direction() != sv::ir::PortDirection::Input {
                return Err(invalid(
                    "an output structure port requires a variable".into(),
                ));
            }
            let parts = port
                .bind_structure_pattern(pattern)
                .map_err(|error| invalid(error.to_string()))?;
            for ((member, _), value) in port.r#type().structure_leaves().iter().zip(parts) {
                expanded.push(LoweredSvPortConnection {
                    formal: format!("{}.{member}", connection.formal),
                    actual: connection.actual.clone(),
                    actual_expr: Some(value),
                    bound_pattern: None,
                });
            }
            continue;
        }
        if connection.actual_expr.is_none() {
            // An explicitly open port leaves all of its leaf ports open.
            continue;
        }
        let actual = parent
            .ports()
            .iter()
            .find(|p| p.name() == connection.actual)
            .map(|p| p.r#type())
            .or_else(|| {
                parent
                    .signals()
                    .iter()
                    .find(|s| s.name() == connection.actual)
                    .map(|s| s.r#type())
            });
        if actual.and_then(|ty| ty.structure_identity()) != port.r#type().structure_identity() {
            return Err(invalid(format!(
                "port `{}` requires a variable of the same unpacked structure type",
                connection.formal
            )));
        }
        let expected = sv::ir::Expr::Concat(
            port.r#type()
                .structure_leaves()
                .iter()
                .map(|(member, _)| sv::ir::Expr::Ident(format!("{}.{member}", connection.actual)))
                .collect(),
        );
        if connection.actual_expr.as_ref() != Some(&expected)
            && connection.actual_expr.as_ref()
                != Some(&sv::ir::Expr::Ident(connection.actual.clone()))
        {
            return Err(invalid("an unpacked structure port currently requires a named variable or an input assignment pattern".into()));
        }
        for (member, _) in port.r#type().structure_leaves() {
            let actual = format!("{}.{member}", connection.actual);
            expanded.push(LoweredSvPortConnection {
                formal: format!("{}.{member}", connection.formal),
                actual: actual.clone(),
                actual_expr: Some(sv::ir::Expr::Ident(actual)),
                bound_pattern: None,
            });
        }
    }
    Ok(std::borrow::Cow::Owned(expanded))
}

pub(super) fn bind_port_patterns(
    connections: &mut [LoweredSvPortConnection],
    child: &LoweredSvModule,
) -> Result<(), ParserError> {
    for connection in connections {
        let Some(pattern) = connection.bound_pattern.take() else {
            continue;
        };
        let port = child
            .source
            .ports()
            .iter()
            .find(|port| port.name() == connection.formal)
            .ok_or_else(|| {
                ParserError::illegal_context(
                    "systemverilog port assignment pattern",
                    format!("unknown port `{}`", connection.formal),
                    None,
                )
            })?;
        if port.direction() != sv::ir::PortDirection::Input {
            return Err(ParserError::illegal_context(
                "systemverilog port assignment pattern",
                "only input port assignment patterns are supported",
                None,
            ));
        }
        connection.actual_expr = Some(port.bind_pattern(&pattern).map_err(|error| {
            ParserError::illegal_context(
                "systemverilog port assignment pattern",
                error.to_string(),
                None,
            )
        })?);
    }
    Ok(())
}
