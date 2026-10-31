//! Assignment helpers: right-hand side coercion, operator assignments, and procedural conditions.

use super::*;

pub(super) fn coerce_procedural_assignment_rhs(
    tree: &AnalysisContext<'_>,
    rhs: Expr,
    lhs: &LValue,
    packed_dimensions: &PackedDimensions,
) -> Expr {
    let functions = &tree.functions;
    let Some(target_type) = lvalue_expr_type(functions, lhs, packed_dimensions) else {
        return rhs;
    };
    let Some(source_signed) = expr_signedness_with_return_types(
        &rhs,
        packed_dimensions,
        &HashMap::default(),
        &packed_dimensions.function_return_types,
    ) else {
        return rhs;
    };
    let assigned = if source_signed == target_type.signed
        && expr_static_width(tree, &rhs, packed_dimensions) == Some(target_type.width)
    {
        rhs
    } else {
        let assigned = Expr::Resize {
            expr: Box::new(rhs),
            width: target_type.width,
            signed: source_signed,
        };
        Expr::Resize {
            expr: Box::new(assigned),
            width: target_type.width,
            signed: target_type.signed,
        }
    };
    let name = match lhs {
        LValue::Ident(name) | LValue::Select { name, .. } => name,
    };
    if matches!(
        lhs,
        LValue::Select {
            is_2state: true,
            ..
        }
    ) || packed_dimensions
        .get(name)
        .is_some_and(|dimensions| dimensions.is_2state)
    {
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(assigned),
        }
    } else {
        assigned
    }
}

pub(super) fn lvalue_expr_type(
    functions: &const_functions::ConstantFunctions,
    value: &LValue,
    packed_dimensions: &PackedDimensions,
) -> Option<ExprType> {
    match value {
        LValue::Ident(name) => {
            let dimensions = packed_dimensions.get(name)?;
            let width = dimensions
                .packed
                .iter()
                .map(|dimension| &dimension.width)
                .chain(dimensions.unpacked.iter().map(|dimension| &dimension.width))
                .try_fold(1usize, |width, dimension| {
                    let dimension_width =
                        eval_ast_const_expr(functions, dimension, &packed_dimensions.const_env)?;
                    width.checked_mul(usize::try_from(dimension_width).ok()?)
                })?;
            Some(ExprType {
                width: width.max(1),
                signed: dimensions.signed,
            })
        }
        LValue::Select {
            msb, lsb, signed, ..
        } => {
            let (msb_base, msb) = offset_from_base(functions, msb, packed_dimensions)?;
            let (lsb_base, lsb) = offset_from_base(functions, lsb, packed_dimensions)?;
            if msb_base != lsb_base {
                return None;
            }
            Some(ExprType {
                width: usize::try_from(msb.abs_diff(lsb)).ok()?.checked_add(1)?,
                signed: *signed,
            })
        }
    }
}

/// A bound of a selection as a run-time base and a constant offset from it:
/// the bounds of a constant range in an element selected at run time, such
/// as `m[i].y`, are the element's position plus constants, so the selection
/// still has a constant width. A constant bound has no base.
fn offset_from_base<'e>(
    functions: &const_functions::ConstantFunctions,
    bound: &'e ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> Option<(Option<&'e ConstExpr>, i128)> {
    let constant = |expr| eval_ast_const_expr(functions, expr, &packed_dimensions.const_env);
    if let Some(value) = constant(bound) {
        return Some((None, value));
    }
    match bound {
        ConstExpr::Binary {
            left,
            op: BinaryOp::Add,
            right,
        } if let Some(offset) = constant(right) => Some((Some(left), offset)),
        _ => Some((Some(bound), 0)),
    }
}

pub(super) fn simplify_condition(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    expression: Expr,
    packed_dimensions: &PackedDimensions,
) -> Expr {
    // Prove this before call expansion, while scoped return metadata can tell
    // two-state functions apart from four-state functions of the same name.
    if let Expr::Binary {
        left,
        op: BinaryOp::LogicOr,
        right,
    } = &expression
        && two_state_conditions_are_complements(tree, session, left, right, packed_dimensions)
    {
        return Expr::Literal("1'b1".to_string());
    }
    expression
}

pub(super) fn assignment_op_expr(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    lhs: &LValue,
    op: &str,
    rhs: Expr,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let operator = || unsupported(format!("assignment operator `{op}`"));
    let op = match op.strip_suffix('=').ok_or_else(operator)? {
        "+" => BinaryOp::Add,
        "-" => BinaryOp::Sub,
        "*" => BinaryOp::Mul,
        "/" => BinaryOp::Div,
        "%" => BinaryOp::Mod,
        "<<" => BinaryOp::Shl,
        "<<<" => BinaryOp::Shl,
        ">>" => BinaryOp::Shr,
        ">>>" => BinaryOp::Sar,
        "&" => BinaryOp::BitAnd,
        "|" => BinaryOp::BitOr,
        "^" => BinaryOp::BitXor,
        _ => return Err(operator()),
    };
    let left = expr_from_lvalue(tree, lhs, packed_dimensions);
    if (expression_binding::real_value(&left, packed_dimensions)
        || expression_binding::real_value(&rhs, packed_dimensions))
        && !matches!(
            op,
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div
        )
    {
        return Err(unsupported("assignment operator on a real operand"));
    }
    Ok(expression_binding::remove_real_division_guards(
        guard_zero_divisions(
            tree,
            session,
            Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(rhs),
            },
        ),
        packed_dimensions,
    ))
}

pub(super) fn expr_from_lvalue(
    tree: &AnalysisContext<'_>,
    lhs: &LValue,
    packed_dimensions: &PackedDimensions,
) -> Expr {
    let functions = &tree.functions;
    if let Some(value) = selects::invalid_unpacked_read(tree, lhs, packed_dimensions) {
        return value;
    }
    let (base, array_slice_width, array_slice_reversed, signed) = match lhs {
        LValue::Ident(name) => return Expr::Ident(name.clone()),
        LValue::Select {
            name,
            msb,
            lsb,
            signed,
            array_slice_width,
            array_slice_reversed,
            ..
        } => (
            Expr::Select {
                expr: Box::new(Expr::Ident(name.clone())),
                msb: msb.clone(),
                lsb: lsb.clone(),
                signed: *signed,
            },
            array_slice_width,
            *array_slice_reversed,
            *signed,
        ),
    };
    let base = if matches!(
        lhs,
        LValue::Select {
            is_2state: true,
            ..
        }
    ) {
        // A selection at run-time positions, such as a member of an element
        // `m[i].b`, has no constant bounds; the conversion keeps its width.
        let Some(r#type) = lvalue_expr_type(&tree.functions, lhs, packed_dimensions) else {
            return Expr::Unary {
                op: UnaryOp::ToTwoState,
                expr: Box::new(base),
            };
        };
        Expr::Resize {
            expr: Box::new(Expr::Unary {
                op: UnaryOp::ToTwoState,
                expr: Box::new(base),
            }),
            width: r#type.width,
            signed: r#type.signed,
        }
    } else {
        base
    };
    if !array_slice_reversed {
        return base;
    }
    let Some(array_slice_width) = array_slice_width else {
        return base;
    };
    let Some(element_width) =
        eval_ast_const_expr(functions, array_slice_width, &packed_dimensions.const_env)
            .and_then(|width| usize::try_from(width).ok())
            .filter(|width| *width != 0)
    else {
        return base;
    };
    let Expr::Select { msb, lsb, .. } = &base else {
        return base;
    };
    let Some(total_width) = eval_ast_const_expr(functions, msb, &packed_dimensions.const_env)
        .and_then(|msb| {
            eval_ast_const_expr(functions, lsb, &packed_dimensions.const_env)
                .and_then(|lsb| usize::try_from(msb.abs_diff(lsb)).ok())
        })
        .and_then(|width| width.checked_add(1))
    else {
        return base;
    };
    if total_width % element_width != 0 || total_width == element_width {
        return base;
    }
    let mut parts = Vec::new();
    for offset in (0..total_width).step_by(element_width) {
        let Some(part_msb) = offset
            .checked_add(element_width)
            .and_then(|value| value.checked_sub(1))
        else {
            return base;
        };
        if let Expr::Select { expr, lsb, .. } = &base {
            let bound = |offset: usize| ConstExpr::Binary {
                left: Box::new(lsb.clone()),
                op: BinaryOp::Add,
                right: Box::new(ConstExpr::Literal(offset.to_string())),
            };
            parts.push(Expr::Select {
                expr: expr.clone(),
                msb: bound(part_msb),
                lsb: bound(offset),
                signed,
            });
        } else {
            parts.push(Expr::Select {
                expr: Box::new(base.clone()),
                msb: ConstExpr::Literal(part_msb.to_string()),
                lsb: ConstExpr::Literal(offset.to_string()),
                signed,
            });
        }
    }
    Expr::Concat(parts)
}
