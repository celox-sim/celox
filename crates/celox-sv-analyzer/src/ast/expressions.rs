//! Runtime expression semantics and call-schema validation.

use super::*;

pub(super) fn guard_zero_divisions(state: &AnalysisState, expr: Expr) -> Expr {
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => expr,
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(guard_zero_divisions(state, *expr)),
            msb,
            lsb,
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|expr| guard_zero_divisions(state, expr))
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts
                .into_iter()
                .map(|expr| guard_zero_divisions(state, expr))
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(guard_zero_divisions(state, *expr)),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(guard_zero_divisions(state, *expr)),
        },
        Expr::Binary { left, op, right } => {
            let left = Box::new(guard_zero_divisions(state, *left));
            let right = Box::new(guard_zero_divisions(state, *right));
            let operation = Expr::Binary {
                left,
                op,
                right: right.clone(),
            };
            let divisor_is_nonzero = expr_to_const(state, (*right).clone())
                .and_then(|right| eval_ast_const_expr(state, &right, &HashMap::default()))
                .is_some_and(|right| right != 0);
            if matches!(op, BinaryOp::Div | BinaryOp::Mod) && !divisor_is_nonzero {
                Expr::Mux {
                    condition: Box::new(Expr::Binary {
                        left: right,
                        op: BinaryOp::EqCase,
                        right: Box::new(Expr::Literal("0".to_string())),
                    }),
                    then_expr: Box::new(Expr::Literal(crate::DIV_ZERO_UNKNOWN_LITERAL.to_string())),
                    else_expr: Box::new(operation),
                }
            } else {
                operation
            }
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(guard_zero_divisions(state, *condition)),
            then_expr: Box::new(guard_zero_divisions(state, *then_expr)),
            else_expr: Box::new(guard_zero_divisions(state, *else_expr)),
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: Box::new(guard_zero_divisions(state, *expr)),
            items: items
                .into_iter()
                .map(|item| item.map(&mut |operand| guard_zero_divisions(state, operand)))
                .collect(),
        },
        Expr::Call {
            callee,
            args,
            arg_order,
        } => Expr::Call {
            arg_order: arg_order.clone(),
            callee,
            args: args
                .into_iter()
                .map(|arg| arg.map(|expr| guard_zero_divisions(state, expr)))
                .collect(),
        },
    }
}

pub(super) fn identifier_expression(
    state: &AnalysisState,
    name: String,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    if scope::imported(state).interface_types.contains_key(&name) && dimensions.get(&name).is_none()
    {
        return Err(unsupported(format!(
            "use of interface `{name}` other than as a port connection or through a member"
        )));
    }
    Ok(Expr::Ident(name))
}

/// Numeric system-function semantics for already-bound native arguments.
pub(super) fn numeric_system_function(
    state: &AnalysisState,
    function: SystemFunction,
    mut arguments: Vec<Expr>,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let operand_error = || unsupported(format!("operand of `{}`", function.name()));
    match function {
        SystemFunction::Signed | SystemFunction::Unsigned => {
            let argument = arguments.pop().ok_or_else(operand_error)?;
            let width =
                expr_static_width(state, &argument, dimensions).ok_or_else(operand_error)?;
            Ok(Expr::Resize {
                expr: Box::new(argument),
                width,
                signed: function == SystemFunction::Signed,
            })
        }
        SystemFunction::Rtoi
        | SystemFunction::Itor
        | SystemFunction::Realtobits
        | SystemFunction::Bitstoreal
        | SystemFunction::Shortrealtobits
        | SystemFunction::Bitstoshortreal
        | SystemFunction::Clog2
        | SystemFunction::Countones
        | SystemFunction::Onehot
        | SystemFunction::Onehot0
        | SystemFunction::Isunknown
        | SystemFunction::Countbits => {
            if function == SystemFunction::Clog2
                && let [argument] = arguments.as_slice()
                && let Some(argument) = expr_to_const(state, argument.clone())
                && let Some(value) = eval_ast_const_expr(
                    state,
                    &ConstExpr::Function {
                        callee: Callee::System(function),
                        args: vec![argument],
                        site: None,
                    },
                    &dimensions.const_env,
                )
            {
                return Ok(Expr::Literal(value.to_string()));
            }
            Ok(Expr::Call {
                callee: Callee::System(function),
                args: arguments.into_iter().map(Some).collect(),
                arg_order: Vec::new(),
            })
        }
        _ => Err(operand_error()),
    }
}

/// Formal names available to a user function call.
pub(super) fn call_signatures<'a>(
    name: &str,
    dimensions: &'a PackedDimensions,
) -> std::borrow::Cow<'a, HashMap<String, Vec<String>>> {
    if let Some(formals) = dimensions
        .type_query
        .then(|| {
            dimensions
                .function_return_types
                .get(name)
                .and_then(|metadata| metadata.arguments.as_deref())
        })
        .flatten()
    {
        std::borrow::Cow::Owned(HashMap::from_iter([(
            name.to_string(),
            formals.iter().map(|formal| formal.name.clone()).collect(),
        )]))
    } else {
        std::borrow::Cow::Borrowed(&dimensions.subroutine_param_names)
    }
}
pub(super) fn check_query_omissions<T>(
    name: &str,
    bindings: &[Option<T>],
    dimensions: &PackedDimensions,
) -> Converted<()> {
    if let Some(formals) = dimensions
        .type_query
        .then(|| {
            dimensions
                .function_return_types
                .get(name)
                .and_then(|metadata| metadata.arguments.as_deref())
        })
        .flatten()
    {
        if bindings.len() > formals.len() {
            return Err(unsupported(format!("extra arguments of `{name}`")));
        }
        for (position, formal) in formals.iter().enumerate() {
            if bindings.get(position).is_none_or(Option::is_none) && !formal.has_default {
                return Err(unsupported(format!(
                    "omitted argument {} of `{name}`",
                    position + 1
                )));
            }
        }
    }
    Ok(())
}
pub(super) fn validate_call_argument(
    name: &str,
    position: usize,
    arg: &Expr,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    let formals = dimensions
        .type_query
        .then(|| {
            dimensions
                .function_return_types
                .get(name)
                .and_then(|metadata| metadata.arguments.as_deref())
        })
        .flatten();
    if formals.is_some_and(|formals| formals[position].direction.is_written())
        && !query_actual_is_writable(arg, dimensions)
    {
        return Err(unsupported(format!(
            "non-writable argument {} of `{name}`",
            position + 1
        )));
    }
    Ok(())
}

pub(super) fn query_actual_is_writable(expression: &Expr, dimensions: &PackedDimensions) -> bool {
    match expression {
        Expr::Ident(name) => {
            dimensions.get(name).is_some()
                && !dimensions.const_env.contains_key(name)
                && !dimensions.parameter_values.contains_key(name)
                && !dimensions
                    .const_env
                    .contains_key(&parameters::unbounded_parameter_marker(name))
        }
        Expr::Select { expr, .. } => query_actual_is_writable(expr, dimensions),
        Expr::Concat(parts) => {
            !parts.is_empty()
                && parts
                    .iter()
                    .all(|part| query_actual_is_writable(part, dimensions))
        }
        _ => false,
    }
}

/// Shared declaration-order/source-order binding for authored argument nodes
/// (IEEE1800-2023 13.5.3 defaults and13.5.4 named arguments).
/// Syntax adapters provide normalized names and optional values, not source text.
pub(super) fn bind_normalized_call_arguments<T: Clone>(
    name: &str,
    mut positional: Vec<Option<T>>,
    named: Vec<(String, Option<T>)>,
    signatures: &HashMap<String, Vec<String>>,
) -> Converted<(Vec<Option<T>>, Vec<usize>)> {
    if named.is_empty() {
        return Ok((positional, Vec::new()));
    }
    let params = signatures
        .get(name)
        .ok_or_else(|| unsupported(format!("named arguments of `{name}`")))?;
    if positional.len() > params.len() {
        return Err(unsupported(format!("too many arguments of `{name}`")));
    }
    let mut order: Vec<_> = (0..positional.len()).collect();
    let mut bound: HashSet<_> = order.iter().copied().collect();
    positional.resize(params.len(), None);
    for (formal, expr) in named {
        let position = params
            .iter()
            .position(|param| *param == formal)
            .ok_or_else(|| unsupported(format!("argument `{formal}` of `{name}`")))?;
        if !bound.insert(position) {
            return Err(unsupported(format!(
                "duplicate argument `{formal}` of `{name}`"
            )));
        }
        positional[position] = expr;
        order.push(position);
    }
    // Defaults for unbound formals are resolved by the call lowerer.
    order.extend((0..params.len()).filter(|position| !bound.contains(position)));
    Ok((positional, order))
}
