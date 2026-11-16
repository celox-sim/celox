//! Rewrites of expressions before they are lowered: renamed identifiers,
//! the values of the variables of unrolled loops, and forced reads.

use super::*;

/// `expr` with the identifiers in `renames` replaced.
pub(in crate::lowering) fn rename_const_idents(
    expr: &sv::ir::ConstExpr,
    renames: &HashMap<String, String>,
) -> sv::ir::ConstExpr {
    rename_const_idents_with_sampling(expr, renames, false)
}

pub(super) fn rename_const_idents_with_sampling(
    expr: &sv::ir::ConstExpr,
    renames: &HashMap<String, String>,
    sampling: bool,
) -> sv::ir::ConstExpr {
    if sampling
        && matches!(expr, sv::ir::ConstExpr::Function { callee: sv::ir::Callee::System(function), .. } if function.reads_sampled_values())
    {
        return expr.clone();
    }
    use sv::ir::ConstExpr;
    let go =
        |expr: &ConstExpr| Box::new(rename_const_idents_with_sampling(expr, renames, sampling));
    match expr {
        ConstExpr::Ident(name) => ConstExpr::Ident(renames.get(name).unwrap_or(name).clone()),
        ConstExpr::Literal(_) => expr.clone(),
        ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => ConstExpr::SelectRange {
            expr: go(expr),
            msb: go(msb),
            lsb: go(lsb),
            signed: *signed,
        },
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: go(expr),
            bit: go(bit),
        },
        ConstExpr::Function { callee, args, site } => ConstExpr::Function {
            callee: callee.clone(),
            site: *site,
            args: args
                .iter()
                .map(|arg| rename_const_idents_with_sampling(arg, renames, sampling))
                .collect(),
        },
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => ConstExpr::Resize {
            expr: go(expr),
            width: *width,
            signed: *signed,
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op: *op,
            expr: go(expr),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: go(left),
            op: *op,
            right: go(right),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: go(condition),
            then_expr: go(then_expr),
            else_expr: go(else_expr),
        },
    }
}

/// `expr` with the identifiers in `renames` replaced.
pub(in crate::lowering) fn rename_idents(
    expr: &sv::ir::Expr,
    renames: &HashMap<String, String>,
) -> sv::ir::Expr {
    rename_idents_with_sampling(expr, renames, false)
}

/// When sampling, leave sampled-value calls to resolve their own operands.
/// Ordinary renames (notably history reads) still descend through those calls.
pub(super) fn rename_idents_with_sampling(
    expr: &sv::ir::Expr,
    renames: &HashMap<String, String>,
    sampling: bool,
) -> sv::ir::Expr {
    if sampling
        && matches!(expr, sv::ir::Expr::Call { callee: sv::ir::Callee::System(function), .. } if function.reads_sampled_values())
    {
        return expr.clone();
    }
    use sv::ir::Expr;
    let go = |expr: &Expr| Box::new(rename_idents_with_sampling(expr, renames, sampling));
    match expr {
        Expr::Ident(name) => Expr::Ident(renames.get(name).unwrap_or(name).clone()),
        Expr::Literal(_) => expr.clone(),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: go(expr),
            msb: rename_const_idents_with_sampling(msb, renames, sampling),
            lsb: rename_const_idents_with_sampling(lsb, renames, sampling),
            signed: *signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .iter()
                .map(|part| rename_idents_with_sampling(part, renames, sampling))
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count: rename_const_idents_with_sampling(count, renames, sampling),
            parts: parts
                .iter()
                .map(|part| rename_idents_with_sampling(part, renames, sampling))
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: go(expr),
            width: *width,
            signed: *signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op: *op,
            expr: go(expr),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: go(left),
            op: *op,
            right: go(right),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: go(condition),
            then_expr: go(then_expr),
            else_expr: go(else_expr),
        },
        Expr::Call {
            callee,
            args,
            arg_order,
        } => Expr::Call {
            callee: callee.clone(),
            args: args
                .iter()
                .map(|arg| {
                    arg.as_ref()
                        .map(|arg| rename_idents_with_sampling(arg, renames, sampling))
                })
                .collect(),
            arg_order: arg_order.clone(),
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: go(expr),
            items: items
                .iter()
                .map(|item| {
                    item.map(&mut |operand| rename_idents_with_sampling(operand, renames, sampling))
                })
                .collect(),
        },
    }
}

pub(super) fn substitute_overlay_const(
    expr: &sv::ir::ConstExpr,
    overlay: &HashMap<String, (String, i128)>,
) -> sv::ir::ConstExpr {
    use sv::ir::ConstExpr;
    let go = |expr: &ConstExpr| substitute_overlay_const(expr, overlay);
    match expr {
        ConstExpr::Ident(name) => match overlay.get(name) {
            Some((_, value)) => ConstExpr::Literal(value.to_string()),
            None => expr.clone(),
        },
        ConstExpr::Literal(_) => expr.clone(),
        ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => ConstExpr::SelectRange {
            expr: Box::new(
                if matches!(&**expr, ConstExpr::Ident(_))
                    && (const_expr_references_identifier(msb)
                        || const_expr_references_identifier(lsb))
                {
                    (**expr).clone()
                } else {
                    go(expr)
                },
            ),
            msb: Box::new(go(msb)),
            lsb: Box::new(go(lsb)),
            signed: *signed,
        },
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: Box::new(go(expr)),
            bit: Box::new(go(bit)),
        },
        ConstExpr::Function { callee, args, site } => ConstExpr::Function {
            callee: callee.clone(),
            site: *site,
            args: args.iter().map(go).collect(),
        },
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => ConstExpr::Resize {
            expr: Box::new(go(expr)),
            width: *width,
            signed: *signed,
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op: *op,
            expr: Box::new(go(expr)),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: Box::new(go(left)),
            op: *op,
            right: Box::new(go(right)),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: Box::new(go(condition)),
            then_expr: Box::new(go(then_expr)),
            else_expr: Box::new(go(else_expr)),
        },
    }
}

pub(super) fn substitute_overlay(
    expr: &sv::ir::Expr,
    overlay: &HashMap<String, (String, i128)>,
) -> sv::ir::Expr {
    use sv::ir::Expr;
    let go = |expr: &Expr| substitute_overlay(expr, overlay);
    match expr {
        Expr::Ident(name) => match overlay.get(name) {
            Some((literal, _)) => Expr::Literal(literal.clone()),
            None => expr.clone(),
        },
        Expr::Literal(_) => expr.clone(),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(go(expr)),
            msb: substitute_overlay_const(msb, overlay),
            lsb: substitute_overlay_const(lsb, overlay),
            signed: *signed,
        },
        Expr::Concat(parts) => Expr::Concat(parts.iter().map(go).collect()),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count: substitute_overlay_const(count, overlay),
            parts: parts.iter().map(go).collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(go(expr)),
            width: *width,
            signed: *signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op: *op,
            expr: Box::new(go(expr)),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(go(left)),
            op: *op,
            right: Box::new(go(right)),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(go(condition)),
            then_expr: Box::new(go(then_expr)),
            else_expr: Box::new(go(else_expr)),
        },
        Expr::Call {
            callee,
            args,
            arg_order,
        } => Expr::Call {
            arg_order: arg_order.clone(),
            callee: callee.clone(),
            args: args.iter().map(|arg| arg.as_ref().map(go)).collect(),
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: Box::new(go(expr)),
            items: items
                .iter()
                .map(|item| item.map(&mut |operand| go(operand)))
                .collect(),
        },
    }
}

pub(super) fn force_reads(expr: &sv::ir::Expr, m: &ProcModule<'_>) -> sv::ir::Expr {
    if !m.forces_reads() {
        return expr.clone();
    }
    use sv::ir::Expr;
    let go = |expr: &Expr| force_reads(expr, m);
    match expr {
        Expr::Ident(name) => {
            if let Some(id) = m.id(name)
                && let Some((enabled, forced)) = m.force_read_controls(id)
            {
                Expr::Resize {
                    width: m.var(id).width,
                    signed: m.var(id).signed,
                    expr: Box::new(Expr::Mux {
                        condition: Box::new(Expr::Ident(m.var(enabled).path[0].clone())),
                        then_expr: Box::new(Expr::Ident(m.var(forced).path[0].clone())),
                        else_expr: Box::new(expr.clone()),
                    }),
                }
            } else {
                expr.clone()
            }
        }
        Expr::Literal(_) => expr.clone(),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(go(expr)),
            msb: msb.clone(),
            lsb: lsb.clone(),
            signed: *signed,
        },
        Expr::Concat(parts) => Expr::Concat(parts.iter().map(go).collect()),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count: count.clone(),
            parts: parts.iter().map(go).collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(go(expr)),
            width: *width,
            signed: *signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op: *op,
            expr: Box::new(go(expr)),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(go(left)),
            op: *op,
            right: Box::new(go(right)),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(go(condition)),
            then_expr: Box::new(go(then_expr)),
            else_expr: Box::new(go(else_expr)),
        },
        Expr::Call {
            callee,
            args,
            arg_order,
        } => Expr::Call {
            arg_order: arg_order.clone(),
            callee: callee.clone(),
            args: args.iter().map(|arg| arg.as_ref().map(go)).collect(),
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: Box::new(go(expr)),
            items: items
                .iter()
                .map(|item| item.map(&mut |operand| go(operand)))
                .collect(),
        },
    }
}
