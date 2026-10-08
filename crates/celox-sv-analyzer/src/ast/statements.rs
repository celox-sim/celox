//! Assignment helpers: right-hand side coercion, operator assignments, and procedural conditions.

use super::*;

pub(super) fn coerce_procedural_assignment_rhs(
    rhs: Expr,
    lhs: &LValue,
    packed_dimensions: &PackedDimensions,
) -> Expr {
    let Some(target_type) = lvalue_expr_type(lhs, packed_dimensions) else {
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
        && expr_static_width(&rhs, packed_dimensions) == Some(target_type.width)
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
                        eval_ast_const_expr(dimension, &packed_dimensions.const_env)?;
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
            let msb = eval_ast_const_expr(msb, &packed_dimensions.const_env)?;
            let lsb = eval_ast_const_expr(lsb, &packed_dimensions.const_env)?;
            Some(ExprType {
                width: usize::try_from(msb.abs_diff(lsb)).ok()?.checked_add(1)?,
                signed: *signed,
            })
        }
    }
}

pub(super) fn expr_from_cond_predicate(
    predicate: &sv_parser::CondPredicate,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Converted<Expr> {
    if cond_predicate_has_conjunction_operator(predicate, syntax_tree) {
        return Err(unsupported("`&&&` in a condition"));
    }
    let entries = predicate.nodes.0.contents();
    let [sv_parser::ExpressionOrCondPattern::Expression(expr)] = entries.as_slice() else {
        return Err(unsupported("pattern matching condition"));
    };
    let expression = expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)?;
    // Prove this before call expansion, while scoped return metadata can tell
    // two-state functions apart from four-state functions of the same name.
    if let Expr::Binary {
        left,
        op: BinaryOp::LogicOr,
        right,
    } = &expression
        && two_state_conditions_are_complements(left, right, packed_dimensions)
    {
        return Ok(Expr::Literal("1'b1".to_string()));
    }
    Ok(expression)
}

fn cond_predicate_has_conjunction_operator(
    predicate: &sv_parser::CondPredicate,
    syntax_tree: &SyntaxTree,
) -> bool {
    let mut previous = None;
    for node in RefNode::CondPredicate(predicate) {
        let RefNode::Symbol(symbol) = node else {
            continue;
        };
        let locate = symbol.nodes.0;
        if previous.is_some_and(|previous: sv_parser::Locate| {
            previous.offset + previous.len == locate.offset
                && syntax_tree.get_str(&previous) == Some("&&")
                && syntax_tree.get_str(&locate) == Some("&")
        }) {
            return true;
        }
        previous = Some(locate);
    }
    false
}

pub(super) fn assignment_op_expr(
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
    Ok(guard_zero_divisions(Expr::Binary {
        left: Box::new(expr_from_lvalue(lhs, packed_dimensions)),
        op,
        right: Box::new(rhs),
    }))
}

pub(super) fn expr_from_lvalue(lhs: &LValue, packed_dimensions: &PackedDimensions) -> Expr {
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
        let Some(r#type) = lvalue_expr_type(lhs, packed_dimensions) else {
            return base;
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
    let Some(element_width) = eval_ast_const_expr(array_slice_width, &packed_dimensions.const_env)
        .and_then(|width| usize::try_from(width).ok())
        .filter(|width| *width != 0)
    else {
        return base;
    };
    let Expr::Select { msb, lsb, .. } = &base else {
        return base;
    };
    let Some(total_width) = eval_ast_const_expr(msb, &packed_dimensions.const_env)
        .and_then(|msb| {
            eval_ast_const_expr(lsb, &packed_dimensions.const_env)
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
        parts.push(Expr::Select {
            expr: Box::new(base.clone()),
            msb: ConstExpr::Literal(part_msb.to_string()),
            lsb: ConstExpr::Literal(offset.to_string()),
            signed,
        });
    }
    Expr::Concat(parts)
}
