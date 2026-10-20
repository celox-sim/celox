//! Constant expression parsing, substitution, and generate-iteration helpers.

use super::*;

pub(super) fn bind_generate_parameter(
    state: &AnalysisState,
    parameter: Parameter,
    const_env: &mut HashMap<String, i128>,
    parameter_literals: &mut HashMap<String, Expr>,
) {
    let mut parameter_types = parameter_types_from_const_env(const_env);
    bind_generate_parameter_with_types(
        state,
        parameter,
        const_env,
        parameter_literals,
        &mut parameter_types,
    );
}

pub(super) fn bind_generate_parameter_with_types(
    state: &AnalysisState,
    parameter: Parameter,
    const_env: &mut HashMap<String, i128>,
    parameter_literals: &mut HashMap<String, Expr>,
    parameter_types: &mut HashMap<String, ExprType>,
) {
    let unbounded = parameter
        .value()
        .is_some_and(|value| parameters::is_unbounded(value, const_env));
    if unbounded {
        const_env.remove(parameter.name());
        const_env.remove(&parameter_marker(parameter.name()));
        const_env.remove(&local_parameter_marker(parameter.name()));
    }
    const_env.insert(
        parameters::unbounded_parameter_marker(parameter.name()),
        i128::from(unbounded),
    );
    let resolved_type = parameter.resolved_type(parameter_types);
    let (resolved, resolved_literal) =
        parameter.resolved_value_and_literal(state, const_env, parameter_types, parameter_literals);
    let literal = if let Some(value) = resolved {
        const_env.insert(parameter.name().to_string(), value);
        Some(Expr::Literal(if let Some(ty) = resolved_type {
            format_typed_parameter_literal(value, ty.width, ty.signed)
        } else {
            value.to_string()
        }))
    } else {
        resolved_literal.or_else(|| {
            parameter_value_env(state, std::slice::from_ref(&parameter), const_env)
                .remove(parameter.name())
                .map(|value| substitute_expr_idents(value, parameter_literals))
        })
    };
    if let Some(ty) = resolved_type {
        insert_parameter_type_markers(const_env, parameter.name(), ty);
        parameter_types.insert(parameter.name().to_string(), ty);
    }
    if let Some(literal) = literal {
        parameter_literals.insert(parameter.name().to_string(), literal);
    }
}

pub(super) fn eval_ast_const_expr(
    state: &AnalysisState,
    expr: &ConstExpr,
    const_env: &HashMap<String, i128>,
) -> Option<i128> {
    // Look up only the identifiers in this expression, rather than scanning
    // every visible signal/parameter marker for each declaration bound.
    let expr = parameters::substitute_typed_parameter_literals_with_lookup(
        expr.clone(),
        const_env,
        &|name| parameter_type_from_const_env(const_env, name),
    );
    typecheck::eval_const_expr_in_env(
        &expr.into(),
        &const_functions::Environment {
            constants: const_env,
            functions: &state.const_functions,
        },
    )
}

pub(super) fn substitute_process_constants(
    state: &AnalysisState,
    process: CombProcess,
    const_env: &HashMap<String, i128>,
) -> CombProcess {
    substitute_process_constants_with_parameter_literals(
        state,
        process,
        const_env,
        &HashMap::default(),
    )
}

pub(super) fn substitute_process_constants_with_parameter_literals(
    state: &AnalysisState,
    process: CombProcess,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
) -> CombProcess {
    let mut sensitivity_body = process.sensitivity_body;
    for stmt in &mut sensitivity_body {
        procedural::substitute_stmt_constants(state, stmt, const_env, parameter_literals);
    }
    let condition = process
        .condition
        .map(|condition| substitute_const_expr_constants(condition, const_env));
    if process.assignments.is_empty() {
        let mut body = process.body;
        for stmt in &mut body {
            procedural::substitute_stmt_constants(state, stmt, const_env, parameter_literals);
        }
        return CombProcess {
            creation_order: process.creation_order,
            sensitivity_body,
            ..CombProcess::procedural(process.kind, condition, body)
        };
    }
    let mut result = CombProcess::new(
        process.kind,
        condition,
        process
            .assignments
            .into_iter()
            .map(|assignment| {
                substitute_assignment_constants_with_parameter_literals(
                    state,
                    assignment,
                    const_env,
                    parameter_literals,
                )
            })
            .collect(),
    );
    result.creation_order = process.creation_order;
    result.sensitivity_body = sensitivity_body;
    result
}

pub(super) fn substitute_assignment_constants_with_parameter_literals(
    state: &AnalysisState,
    assignment: Assignment,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
) -> Assignment {
    Assignment::new(
        substitute_lvalue_constants(assignment.lhs, const_env),
        substitute_expr_constants_with_parameter_literals(
            state,
            assignment.rhs,
            const_env,
            parameter_literals,
        ),
    )
}

pub(super) fn substitute_lvalue_constants(
    lvalue: LValue,
    const_env: &HashMap<String, i128>,
) -> LValue {
    match lvalue {
        LValue::Ident(name) => LValue::Ident(name),
        LValue::Select {
            name,
            msb,
            lsb,
            signed,
            array_slice_width,
            array_slice_reversed,
            is_2state,
        } => LValue::Select {
            name,
            msb: substitute_const_expr_constants(msb, const_env),
            lsb: substitute_const_expr_constants(lsb, const_env),
            signed,
            array_slice_width: array_slice_width
                .map(|width| substitute_const_expr_constants(width, const_env)),
            array_slice_reversed,
            is_2state,
        },
    }
}

pub(super) fn substitute_expr_constants_with_parameter_literals(
    state: &AnalysisState,
    expr: Expr,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
) -> Expr {
    match expr {
        Expr::Ident(name) => parameter_literals
            .get(&name)
            .cloned()
            .or_else(|| {
                let value = *const_env.get(&name)?;
                // Generate indices carry the same width/signing markers as
                // enum values. Preserve them instead of creating a literal
                // such as "-2147483648", whose minus is an expression operator.
                if const_env.contains_key(&enum_marker(&name))
                    || (!const_env.contains_key(&parameter_marker(&name))
                        && const_env.contains_key(&parameter_width_marker(&name)))
                {
                    let width = const_env
                        .get(&parameter_width_marker(&name))
                        .and_then(|width| usize::try_from(*width).ok())?;
                    let signed = const_env
                        .get(&parameter_signed_marker(&name))
                        .is_some_and(|signed| *signed != 0);
                    Some(Expr::Literal(format_typed_parameter_literal(
                        value, width, signed,
                    )))
                } else if !const_env.contains_key(&parameter_marker(&name)) {
                    Some(Expr::Literal(value.to_string()))
                } else {
                    None
                }
            })
            .unwrap_or(Expr::Ident(name)),
        Expr::Literal(value) => Expr::Literal(value),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(substitute_expr_constants_with_parameter_literals(
                state,
                *expr,
                const_env,
                parameter_literals,
            )),
            msb: substitute_const_expr_constants(msb, const_env),
            lsb: substitute_const_expr_constants(lsb, const_env),
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| {
                    substitute_expr_constants_with_parameter_literals(
                        state,
                        part,
                        const_env,
                        parameter_literals,
                    )
                })
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count: substitute_const_expr_constants(count, const_env),
            parts: parts
                .into_iter()
                .map(|part| {
                    substitute_expr_constants_with_parameter_literals(
                        state,
                        part,
                        const_env,
                        parameter_literals,
                    )
                })
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(substitute_expr_constants_with_parameter_literals(
                state,
                *expr,
                const_env,
                parameter_literals,
            )),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(substitute_expr_constants_with_parameter_literals(
                state,
                *expr,
                const_env,
                parameter_literals,
            )),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(substitute_expr_constants_with_parameter_literals(
                state,
                *left,
                const_env,
                parameter_literals,
            )),
            op,
            right: Box::new(substitute_expr_constants_with_parameter_literals(
                state,
                *right,
                const_env,
                parameter_literals,
            )),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(substitute_expr_constants_with_parameter_literals(
                state,
                *condition,
                const_env,
                parameter_literals,
            )),
            then_expr: Box::new(substitute_expr_constants_with_parameter_literals(
                state,
                *then_expr,
                const_env,
                parameter_literals,
            )),
            else_expr: Box::new(substitute_expr_constants_with_parameter_literals(
                state,
                *else_expr,
                const_env,
                parameter_literals,
            )),
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: Box::new(substitute_expr_constants_with_parameter_literals(
                state,
                *expr,
                const_env,
                parameter_literals,
            )),
            items: items
                .into_iter()
                .map(|item| {
                    item.map(&mut |operand| {
                        substitute_expr_constants_with_parameter_literals(
                            state,
                            operand,
                            const_env,
                            parameter_literals,
                        )
                    })
                })
                .collect(),
        },
        Expr::Call {
            callee,
            args,
            arg_order,
        } => {
            let args: Vec<_> = args
                .into_iter()
                .map(|arg| {
                    arg.map(|arg| {
                        substitute_expr_constants_with_parameter_literals(
                            state,
                            arg,
                            const_env,
                            parameter_literals,
                        )
                    })
                })
                .collect();
            let function = callee.system();
            if let Some(function @ (SystemFunction::Signed | SystemFunction::Unsigned)) = function
                && let [Some(arg)] = args.as_slice()
                && let Some(constant) = expr_to_const(state, arg.clone())
                && let Some(ty) =
                    infer_const_expr_type(&constant, &parameter_types_from_const_env(const_env))
            {
                Expr::Resize {
                    expr: Box::new(arg.clone()),
                    width: ty.width,
                    signed: function == SystemFunction::Signed,
                }
            } else if function == Some(SystemFunction::Countbits) {
                // Resolve constant calls before SLT lowering while retaining X/Z masks.
                fold_const_integral_expr_preserving_mask(
                    state,
                    Expr::Call {
                        callee,
                        args,
                        arg_order,
                    },
                    const_env,
                )
            } else {
                Expr::Call {
                    callee,
                    args,
                    arg_order,
                }
            }
        }
    }
}

pub(super) fn substitute_const_expr_constants(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
) -> ConstExpr {
    substitute_const_expr_constants_impl(expr, const_env, false, false)
}

pub(super) fn substitute_dimension_constants(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
) -> ConstExpr {
    substitute_const_expr_constants_impl(expr, const_env, true, false)
}

pub(super) fn substitute_const_expr_constants_preserving_enum_types(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
) -> ConstExpr {
    substitute_const_expr_constants_impl(expr, const_env, false, true)
}

fn substitute_const_expr_constants_impl(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
    include_local_parameters: bool,
    preserve_enum_types: bool,
) -> ConstExpr {
    match expr {
        ConstExpr::Ident(name) => {
            let Some(value) = const_env.get(&name).filter(|_| {
                !const_env.contains_key(&parameter_marker(&name))
                    || include_local_parameters
                        && const_env.contains_key(&local_parameter_marker(&name))
            }) else {
                return ConstExpr::Ident(name);
            };
            if preserve_enum_types && const_env.contains_key(&enum_marker(&name)) {
                let width = const_env
                    .get(&parameter_width_marker(&name))
                    .and_then(|width| usize::try_from(*width).ok());
                let signed = const_env
                    .get(&parameter_signed_marker(&name))
                    .is_some_and(|signed| *signed != 0);
                if let Some(width) = width {
                    return ConstExpr::Literal(format_typed_parameter_literal(
                        *value, width, signed,
                    ));
                }
            }
            ConstExpr::Literal(value.to_string())
        }
        ConstExpr::Literal(value) => ConstExpr::Literal(value),
        ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => ConstExpr::SelectRange {
            expr: Box::new(substitute_const_expr_constants_impl(
                *expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            msb: Box::new(substitute_const_expr_constants_impl(
                *msb,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            lsb: Box::new(substitute_const_expr_constants_impl(
                *lsb,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            signed,
        },
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: Box::new(substitute_const_expr_constants_impl(
                *expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            bit: Box::new(substitute_const_expr_constants_impl(
                *bit,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
        },
        ConstExpr::Function { callee, args, site } => ConstExpr::Function {
            callee,
            site,
            args: args
                .into_iter()
                .map(|arg| {
                    substitute_const_expr_constants_impl(
                        arg,
                        const_env,
                        include_local_parameters,
                        preserve_enum_types,
                    )
                })
                .collect(),
        },
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => ConstExpr::Resize {
            expr: Box::new(substitute_const_expr_constants_impl(
                *expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            width,
            signed,
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op,
            expr: Box::new(substitute_const_expr_constants_impl(
                *expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: Box::new(substitute_const_expr_constants_impl(
                *left,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            op,
            right: Box::new(substitute_const_expr_constants_impl(
                *right,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: Box::new(substitute_const_expr_constants_impl(
                *condition,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            then_expr: Box::new(substitute_const_expr_constants_impl(
                *then_expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            else_expr: Box::new(substitute_const_expr_constants_impl(
                *else_expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
        },
    }
}

/// Convert an expression to its symbolic constant form when representable.
pub(super) fn expr_to_const(state: &AnalysisState, expr: Expr) -> Option<ConstExpr> {
    match expr {
        Expr::Ident(name) => Some(ConstExpr::Ident(name)),
        Expr::Literal(value) => Some(ConstExpr::Literal(value)),
        Expr::Unary { op, expr } => Some(ConstExpr::Unary {
            op,
            expr: Box::new(expr_to_const(state, *expr)?),
        }),
        Expr::Binary { left, op, right } => Some(ConstExpr::Binary {
            left: Box::new(expr_to_const(state, *left)?),
            op,
            right: Box::new(expr_to_const(state, *right)?),
        }),
        Expr::Select { expr, msb, lsb, .. } if msb == lsb => Some(ConstExpr::Select {
            expr: Box::new(expr_to_const(state, *expr)?),
            bit: Box::new(msb),
        }),
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Some(ConstExpr::Mux {
            condition: Box::new(expr_to_const(state, *condition)?),
            then_expr: Box::new(expr_to_const(state, *then_expr)?),
            else_expr: Box::new(expr_to_const(state, *else_expr)?),
        }),
        Expr::Call {
            callee,
            args,
            arg_order,
        } => {
            if !arg_order.is_empty() {
                return None;
            }
            Some(ConstExpr::call(
                state,
                callee,
                args.into_iter()
                    .map(|arg| expr_to_const(state, arg?))
                    .collect::<Option<_>>()?,
            ))
        }
        Expr::Select { .. }
        | Expr::Concat(_)
        | Expr::RepeatConcat { .. }
        | Expr::Resize { .. }
        | Expr::Inside { .. } => None,
    }
}

pub(super) fn expr_to_index_const(
    state: &AnalysisState,
    expr: Expr,
    dimensions: &PackedDimensions,
) -> Option<ConstExpr> {
    match expr {
        Expr::Resize {
            expr,
            width,
            signed,
        } => {
            if width == 0 {
                return Some(ConstExpr::Literal("0".into()));
            }
            Some(ConstExpr::Resize {
                expr: Box::new(expr_to_index_const(state, *expr, dimensions)?),
                width,
                signed,
            })
        }
        Expr::Ident(name) => Some(ConstExpr::Ident(name)),
        Expr::Literal(value) => Some(ConstExpr::Literal(value)),
        Expr::Unary { op, expr } => Some(ConstExpr::Unary {
            op,
            expr: Box::new(expr_to_index_const(state, *expr, dimensions)?),
        }),
        Expr::Binary { left, op, right } => Some(ConstExpr::Binary {
            left: Box::new(expr_to_index_const(state, *left, dimensions)?),
            op,
            right: Box::new(expr_to_index_const(state, *right, dimensions)?),
        }),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } if !matches!(&*expr, Expr::Concat(_) | Expr::RepeatConcat { .. }) => {
            Some(ConstExpr::SelectRange {
                expr: Box::new(expr_to_index_const(state, *expr, dimensions)?),
                msb: Box::new(msb),
                lsb: Box::new(lsb),
                signed,
            })
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Some(ConstExpr::Mux {
            condition: Box::new(expr_to_index_const(state, *condition, dimensions)?),
            then_expr: Box::new(expr_to_index_const(state, *then_expr, dimensions)?),
            else_expr: Box::new(expr_to_index_const(state, *else_expr, dimensions)?),
        }),
        Expr::Call {
            callee,
            args,
            arg_order,
        } => {
            // Pure calls were expanded before index conversion. Retaining a
            // named call here would discard the order of its procedural effects.
            if !arg_order.is_empty() {
                return None;
            }
            Some(ConstExpr::call(
                state,
                callee,
                args.into_iter()
                    .map(|arg| expr_to_index_const(state, arg?, dimensions))
                    .collect::<Option<_>>()?,
            ))
        }
        Expr::Select {
            expr,
            msb,
            lsb,
            signed: false,
        } => {
            let msb = eval_ast_const_expr(state, &msb, &dimensions.const_env)?;
            let lsb = eval_ast_const_expr(state, &lsb, &dimensions.const_env)?;
            let width = usize::try_from(msb.abs_diff(lsb).checked_add(1)?).ok()?;
            if width > constant_folding::MAX_CONSTANT_CONCAT_BITS {
                return None;
            }
            // Concatenations have [n-1:0] coordinates. Resolve just the
            // selected bits in their operands, retaining each operand's range.
            let positions = (0..width).map(|offset| {
                if msb >= lsb {
                    lsb + offset as i128
                } else {
                    lsb - offset as i128
                }
            });
            let bits = match *expr {
                expr @ (Expr::Concat(_) | Expr::RepeatConcat { .. }) => {
                    // Extracting only some operands must not discard calls
                    // with effects. Pure calls have already been expanded.
                    if inlining::calls_subroutine(&expr) {
                        return None;
                    }
                    let source = IndexBitSource::from_expr(state, expr, dimensions)?;
                    positions
                        .map(|bit| source.bit(bit))
                        .collect::<Option<_>>()?
                }
                expr => {
                    let expr = expr_to_index_const(state, expr, dimensions)?;
                    positions
                        .map(|bit| ConstExpr::Select {
                            expr: Box::new(expr.clone()),
                            bit: Box::new(const_expr_from_i128(bit)),
                        })
                        .collect()
                }
            };
            assemble_index_bits(bits)
        }
        expr @ (Expr::Concat(_) | Expr::RepeatConcat { .. }) => {
            // Bit extraction retains self-determined operand widths and X/Z.
            // Never duplicate a call whose effects have not been expanded.
            if inlining::calls_subroutine(&expr) {
                return None;
            }
            let source = IndexBitSource::from_expr(state, expr, dimensions)?;
            let width = source.width();
            if width > constant_folding::MAX_CONSTANT_CONCAT_BITS {
                return None;
            }
            assemble_index_bits(
                (0..width)
                    .map(|bit| source.bit(bit as i128))
                    .collect::<Option<_>>()?,
            )
        }
        Expr::Select { .. } | Expr::Inside { .. } => None,
    }
}

/// Assemble single bits at their own unsigned result width. A balanced tree
/// keeps the recursion bounded for wider slices.
fn assemble_index_bits(bits: Vec<ConstExpr>) -> Option<ConstExpr> {
    let width = bits.len();
    let mut bits = bits
        .into_iter()
        .enumerate()
        .map(|(offset, bit)| ConstExpr::Binary {
            left: Box::new(bit),
            op: BinaryOp::Shl,
            right: Box::new(ConstExpr::Literal(offset.to_string())),
        })
        .collect::<Vec<_>>();
    while bits.len() > 1 {
        let mut remaining = bits.into_iter();
        bits = std::iter::from_fn(|| {
            let left = remaining.next()?;
            Some(match remaining.next() {
                Some(right) => ConstExpr::Binary {
                    left: Box::new(left),
                    op: BinaryOp::BitOr,
                    right: Box::new(right),
                },
                None => left,
            })
        })
        .collect();
    }
    Some(ConstExpr::Binary {
        left: Box::new(ConstExpr::Literal(format!("{width}'b0"))),
        op: BinaryOp::BitOr,
        right: Box::new(bits.pop()?),
    })
}

/// A concatenation operand prepared once, so copied bits retain call sites
/// and replications do not expand their complete bit vectors.
enum IndexBitSource {
    Scalar {
        expr: ConstExpr,
        width: usize,
        right: i128,
        step: i128,
    },
    Concat {
        parts: Vec<Self>,
        width: usize,
    },
    Repeat {
        part: Box<Self>,
        width: usize,
    },
    Slice {
        part: Box<Self>,
        width: usize,
        right: i128,
        step: i128,
    },
    Resize {
        part: Box<Self>,
        width: usize,
        extend_signed: bool,
    },
}

impl IndexBitSource {
    fn from_expr(state: &AnalysisState, expr: Expr, dimensions: &PackedDimensions) -> Option<Self> {
        let width = expr_static_width(state, &expr, dimensions)?;
        Some(match expr {
            Expr::Concat(parts) => Self::Concat {
                parts: parts
                    .into_iter()
                    .map(|part| Self::from_expr(state, part, dimensions))
                    .collect::<Option<_>>()?,
                width,
            },
            Expr::RepeatConcat { parts, .. } => Self::Repeat {
                part: Box::new(Self::from_expr(state, Expr::Concat(parts), dimensions)?),
                width,
            },
            Expr::Resize { expr, .. } => {
                let extend_signed = expr_signedness_with_return_types(
                    &expr,
                    &dimensions.expression_signedness,
                    &dimensions.functions,
                    &dimensions.function_return_types,
                )?;
                Self::Resize {
                    part: Box::new(Self::from_expr(state, *expr, dimensions)?),
                    width,
                    extend_signed,
                }
            }
            Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } => {
                match *expr {
                    expr @ (Expr::Concat(_) | Expr::RepeatConcat { .. }) => {
                        let left = eval_ast_const_expr(state, &msb, &dimensions.const_env)?;
                        let right = eval_ast_const_expr(state, &lsb, &dimensions.const_env)?;
                        Self::Slice {
                            part: Box::new(Self::from_expr(state, expr, dimensions)?),
                            width,
                            right,
                            step: if left >= right { 1 } else { -1 },
                        }
                    }
                    expr => Self::Scalar {
                        // The selected value has [width-1:0] coordinates even
                        // when its position in an unpacked array is dynamic.
                        expr: expr_to_index_const(
                            state,
                            Expr::Select {
                                expr: Box::new(expr),
                                msb,
                                lsb,
                                signed,
                            },
                            dimensions,
                        )?,
                        width,
                        right: 0,
                        step: 1,
                    },
                }
            }
            expr => {
                let (right, step) = if let Expr::Ident(name) = &expr
                    && let Some(variable) = dimensions.get(name)
                    && variable.packed.len() == 1
                    && !variable.packed[0].normalize_single
                {
                    let range = &variable.packed[0];
                    let left = eval_ast_const_expr(state, &range.left, &dimensions.const_env)?;
                    let right = eval_ast_const_expr(state, &range.right, &dimensions.const_env)?;
                    (right, if left >= right { 1 } else { -1 })
                } else {
                    (0, 1)
                };
                Self::Scalar {
                    expr: expr_to_index_const(state, expr, dimensions)?,
                    width,
                    right,
                    step,
                }
            }
        })
    }

    fn width(&self) -> usize {
        match self {
            Self::Scalar { width, .. }
            | Self::Concat { width, .. }
            | Self::Repeat { width, .. }
            | Self::Slice { width, .. }
            | Self::Resize { width, .. } => *width,
        }
    }

    fn bit(&self, bit: i128) -> Option<ConstExpr> {
        let Some(mut offset) = usize::try_from(bit).ok().filter(|bit| *bit < self.width()) else {
            return Some(ConstExpr::Literal("1'bx".into()));
        };
        match self {
            Self::Scalar {
                expr, right, step, ..
            } => Some(ConstExpr::Select {
                expr: Box::new(expr.clone()),
                bit: Box::new(const_expr_from_i128(
                    right.checked_add(step.checked_mul(bit)?)?,
                )),
            }),
            Self::Concat { parts, .. } => {
                for part in parts.iter().rev() {
                    if offset < part.width() {
                        return part.bit(offset as i128);
                    }
                    offset -= part.width();
                }
                None
            }
            Self::Repeat { part, .. } => part.bit((offset % part.width()) as i128),
            Self::Slice {
                part, right, step, ..
            } => part.bit(right.checked_add(step.checked_mul(bit)?)?),
            Self::Resize {
                part,
                extend_signed,
                ..
            } => {
                if offset < part.width() {
                    part.bit(bit)
                } else if *extend_signed && part.width() > 0 {
                    part.bit((part.width() - 1) as i128)
                } else {
                    Some(ConstExpr::Literal("1'b0".into()))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

/// Preserve constant-call resolution/default/result-type semantics independently
/// of the syntax adapter that supplied the call and its actual values.
pub(super) fn user_call_value(
    state: &AnalysisState,
    expression: Expr,
    constants: &HashMap<String, i128>,
) -> Option<ConstExpr> {
    if let Expr::Call {
        args, arg_order, ..
    } = &expression
        && (!arg_order.is_empty() || args.iter().any(Option::is_none))
    {
        // Effectful runtime named calls keep their source order. Constant calls
        // are folded only when their bodies and actuals evaluate successfully.
        let literal = const_functions::eval_expression_literal(
            &state.const_functions,
            &expression,
            constants,
        )?;
        let Expr::Call { callee, args, .. } = &expression else {
            unreachable!()
        };
        let types = parameter_types_from_const_env(constants)
            .into_iter()
            .map(|(name, ty)| (name, (ty.width, ty.signed)))
            .collect();
        let args = const_functions::resolved_arguments(&state.const_functions, callee, args)?
            .into_iter()
            .map(|arg| {
                expr_to_const(state, arg.clone()).or_else(|| {
                    constant_folding::constant_with_folded_selections(
                        state, &arg, constants, &types,
                    )
                })
            })
            .collect::<Option<Vec<_>>>()?;
        // Keep references to parameters for consumers that re-specialize the
        // declaration contract; retain the native function's result type too.
        return Some(ConstExpr::Resize {
            expr: Box::new(ConstExpr::call(state, callee.clone(), args)),
            width: literal.width,
            signed: literal.signed,
        });
    }
    expr_to_const(state, expression.clone()).or_else(|| {
        let Expr::Call {
            callee,
            args,
            arg_order,
        } = expression
        else {
            return None;
        };
        if !arg_order.is_empty() {
            return None;
        }
        let types = parameter_types_from_const_env(constants)
            .into_iter()
            .map(|(name, ty)| (name, (ty.width, ty.signed)))
            .collect();
        let args = args
            .into_iter()
            .map(|arg| {
                let arg = arg?;
                expr_to_const(state, arg.clone()).or_else(|| {
                    constant_folding::constant_with_folded_selections(
                        state, &arg, constants, &types,
                    )
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(ConstExpr::call(state, callee, args))
    })
}

// ConstExpr represents bit selects but not concatenations or part selects.
// Counting each stream segment separately keeps parameter dependencies symbolic
// and preserves the selected width, including when zero bits are counted.
pub(super) fn countbits_constant_call(
    state: &AnalysisState,
    expression: Expr,
    const_env: &HashMap<String, i128>,
) -> Option<ConstExpr> {
    let Expr::Call { args, .. } = expression else {
        return None;
    };
    let mut args = args.into_iter().collect::<Option<Vec<_>>>()?;
    let operand = args.remove(0);
    let controls = args
        .into_iter()
        .map(|control| match control {
            Expr::Select { expr, lsb, .. } => Some(ConstExpr::Select {
                expr: Box::new(expr_to_const(state, *expr)?),
                bit: Box::new(lsb),
            }),
            control => expr_to_const(state, control),
        })
        .collect::<Option<Vec<_>>>()?;
    countbits_constant_operand(state, operand, &controls, const_env)
}

fn countbits_constant_operand(
    state: &AnalysisState,
    operand: Expr,
    controls: &[ConstExpr],
    const_env: &HashMap<String, i128>,
) -> Option<ConstExpr> {
    let call = |operand| {
        let mut args = Vec::with_capacity(controls.len() + 1);
        args.push(operand);
        args.extend_from_slice(controls);
        ConstExpr::call(state, Callee::System(SystemFunction::Countbits), args)
    };
    let sum = |mut terms: Vec<ConstExpr>| {
        // Keep large selected vectors from creating a deeply nested sum.
        while terms.len() > 1 {
            let mut next = Vec::with_capacity(terms.len().div_ceil(2));
            let mut terms_iter = terms.into_iter();
            while let Some(left) = terms_iter.next() {
                next.push(match terms_iter.next() {
                    Some(right) => ConstExpr::Binary {
                        left: Box::new(left),
                        op: BinaryOp::Add,
                        right: Box::new(right),
                    },
                    None => left,
                });
            }
            terms = next;
        }
        terms.pop()
    };
    match operand {
        Expr::Select { expr, msb, lsb, .. } if msb != lsb => {
            let msb = usize::try_from(eval_ast_const_expr(state, &msb, const_env)?).ok()?;
            let lsb = usize::try_from(eval_ast_const_expr(state, &lsb, const_env)?).ok()?;
            let width = msb.checked_sub(lsb)?.checked_add(1)?;
            if width > constant_folding::MAX_CONSTANT_CONCAT_BITS {
                return None;
            }
            let operand = expr_to_const(state, *expr)?;
            sum((lsb..=msb)
                .map(|bit| {
                    call(ConstExpr::Select {
                        expr: Box::new(operand.clone()),
                        bit: Box::new(ConstExpr::Literal(bit.to_string())),
                    })
                })
                .collect())
        }
        Expr::Concat(parts) => sum(parts
            .into_iter()
            .map(|part| countbits_constant_operand(state, part, controls, const_env))
            .collect::<Option<Vec<_>>>()?),
        Expr::RepeatConcat { count, parts } => {
            let count = eval_ast_const_expr(state, &count, const_env)?;
            if count < 0 {
                return None;
            }
            let counted =
                countbits_constant_operand(state, Expr::Concat(parts), controls, const_env)?;
            Some(ConstExpr::Binary {
                left: Box::new(counted),
                op: BinaryOp::Mul,
                right: Box::new(ConstExpr::Literal(format!("32'sd{count}"))),
            })
        }
        operand => Some(call(expr_to_const(state, operand)?)),
    }
}

pub(super) fn unary_op_from_text(text: &str) -> Option<UnaryOp> {
    match text {
        "+" => Some(UnaryOp::Plus),
        "-" => Some(UnaryOp::Minus),
        "~" => Some(UnaryOp::BitNot),
        "!" => Some(UnaryOp::LogicNot),
        "&" => Some(UnaryOp::RedAnd),
        "|" => Some(UnaryOp::RedOr),
        "^" => Some(UnaryOp::RedXor),
        _ => None,
    }
}

pub(super) fn unary_expr_from_text(text: &str, expr: Expr) -> Option<Expr> {
    let reduction = match text {
        "~&" => Some(UnaryOp::RedAnd),
        "~|" => Some(UnaryOp::RedOr),
        "~^" | "^~" => Some(UnaryOp::RedXor),
        _ => None,
    };
    if let Some(op) = reduction {
        return Some(Expr::Unary {
            op: UnaryOp::BitNot,
            expr: Box::new(Expr::Unary {
                op,
                expr: Box::new(expr),
            }),
        });
    }
    Some(Expr::Unary {
        op: unary_op_from_text(text)?,
        expr: Box::new(expr),
    })
}

pub(super) fn binary_op_from_text(text: &str) -> Option<BinaryOp> {
    match text {
        "+" => Some(BinaryOp::Add),
        "-" => Some(BinaryOp::Sub),
        "*" => Some(BinaryOp::Mul),
        "/" => Some(BinaryOp::Div),
        "%" => Some(BinaryOp::Mod),
        "**" => Some(BinaryOp::Pow),
        "<<" => Some(BinaryOp::Shl),
        "<<<" => Some(BinaryOp::Shl),
        ">>" => Some(BinaryOp::Shr),
        ">>>" => Some(BinaryOp::Sar),
        "&" => Some(BinaryOp::BitAnd),
        "|" => Some(BinaryOp::BitOr),
        "^" => Some(BinaryOp::BitXor),
        "&&" => Some(BinaryOp::LogicAnd),
        "||" => Some(BinaryOp::LogicOr),
        "==" => Some(BinaryOp::Eq),
        "!=" => Some(BinaryOp::Ne),
        "===" => Some(BinaryOp::EqCase),
        "!==" => Some(BinaryOp::NeCase),
        "==?" => Some(BinaryOp::EqWildcard),
        "!=?" => Some(BinaryOp::NeWildcard),
        "<" => Some(BinaryOp::Lt),
        "<=" => Some(BinaryOp::Le),
        ">" => Some(BinaryOp::Gt),
        ">=" => Some(BinaryOp::Ge),
        _ => None,
    }
}
