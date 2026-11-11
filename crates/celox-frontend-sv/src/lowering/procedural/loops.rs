//! Loop shapes: the counted loops that lower to a fold, and the jumps that
//! leave a loop body early.

use super::*;

/// The `for` loop shapes that lower to a counted SLT fold:
/// `for (v = start; v < end; v += step)` and its relatives.
#[derive(Debug, Clone)]
pub(in crate::lowering) struct CanonicalLoop {
    pub var: String,
    pub start: sv::ir::Expr,
    pub end: sv::ir::Expr,
    pub compare: sv::ir::BinaryOp,
    pub step: usize,
    pub step_op: celox_slt::SLTStepOp,
    pub decreasing: bool,
}

pub(in crate::lowering) fn literal_usize(expr: &sv::ir::Expr) -> Option<usize> {
    let sv::ir::Expr::Literal(text) = expr else {
        return None;
    };
    let literal = sv::typecheck::parse_integral_literal(text)?;
    if !literal.mask.is_zero() {
        return None;
    }
    literal.value.to_usize()
}

pub(in crate::lowering) fn canonical_for_loop(
    init: &[sv::ir::Stmt],
    condition: Option<&sv::ir::Expr>,
    step: &[sv::ir::Stmt],
) -> Option<CanonicalLoop> {
    let (var, start) = match init {
        [
            sv::ir::Stmt::Local {
                name,
                init: Some(start),
            },
        ] => (name.clone(), start.clone()),
        [
            sv::ir::Stmt::Assign {
                lhs: sv::ir::LValue::Ident(name),
                rhs,
                nonblocking: false,
            },
        ] => (name.clone(), rhs.clone()),
        _ => return None,
    };
    let [
        sv::ir::Stmt::Assign {
            lhs: sv::ir::LValue::Ident(step_target),
            rhs:
                sv::ir::Expr::Binary {
                    left: step_left,
                    op: step_binary,
                    right: step_right,
                },
            nonblocking: false,
        },
    ] = step
    else {
        return None;
    };
    if *step_target != var || !matches!(&**step_left, sv::ir::Expr::Ident(name) if *name == var) {
        return None;
    }
    let step = literal_usize(step_right)?;
    let (step_op, decreasing) = match step_binary {
        sv::ir::BinaryOp::Add => (celox_slt::SLTStepOp::Add, false),
        sv::ir::BinaryOp::Sub => (celox_slt::SLTStepOp::Add, true),
        sv::ir::BinaryOp::Mul => (celox_slt::SLTStepOp::Mul, false),
        sv::ir::BinaryOp::Shl => (celox_slt::SLTStepOp::Shl, false),
        sv::ir::BinaryOp::BitOr => (celox_slt::SLTStepOp::BitOr, false),
        sv::ir::BinaryOp::BitXor => (celox_slt::SLTStepOp::BitXor, false),
        _ => return None,
    };
    if step == 0 && step_op == celox_slt::SLTStepOp::Add {
        return None;
    }
    let sv::ir::Expr::Binary { left, op, right } = condition? else {
        return None;
    };
    let is_var = |expr: &sv::ir::Expr| matches!(expr, sv::ir::Expr::Ident(name) if *name == var);
    let (compare, end) = if is_var(left) {
        (*op, (**right).clone())
    } else if is_var(right) {
        let flipped = match op {
            sv::ir::BinaryOp::Lt => sv::ir::BinaryOp::Gt,
            sv::ir::BinaryOp::Le => sv::ir::BinaryOp::Ge,
            sv::ir::BinaryOp::Gt => sv::ir::BinaryOp::Lt,
            sv::ir::BinaryOp::Ge => sv::ir::BinaryOp::Le,
            _ => return None,
        };
        (flipped, (**left).clone())
    } else {
        return None;
    };
    let valid = match compare {
        sv::ir::BinaryOp::Lt | sv::ir::BinaryOp::Le => !decreasing,
        sv::ir::BinaryOp::Gt | sv::ir::BinaryOp::Ge => {
            decreasing && step_op == celox_slt::SLTStepOp::Add
        }
        _ => false,
    };
    valid.then_some(CanonicalLoop {
        var,
        start,
        end,
        compare,
        step,
        step_op,
        decreasing,
    })
}

/// The counted loop a `while` or `do`-`while` loop is when its condition
/// compares a variable with a bound and its last statement steps that
/// variable, as the step of a [`canonical_for_loop`] would; the loop starts
/// from the variable's current value. Also returns the statements before
/// the step, which must not write the variable or the bound.
pub(in crate::lowering) fn counted_while_loop<'s>(
    condition: Option<&sv::ir::Expr>,
    body: &'s [sv::ir::Stmt],
) -> Option<(CanonicalLoop, &'s [sv::ir::Stmt])> {
    let (step, rest) = body.split_last()?;
    let sv::ir::Stmt::Assign {
        lhs: sv::ir::LValue::Ident(var),
        ..
    } = step
    else {
        return None;
    };
    let init = [sv::ir::Stmt::Assign {
        lhs: sv::ir::LValue::Ident(var.clone()),
        rhs: sv::ir::Expr::Ident(var.clone()),
        nonblocking: false,
    }];
    let canonical = canonical_for_loop(&init, condition, std::slice::from_ref(step))?;
    let mut written = HashSet::default();
    written_names(rest, &mut written);
    let mut bound = HashSet::default();
    expr_idents(&canonical.end, &mut bound);
    (!written.contains(&canonical.var) && written.is_disjoint(&bound)).then_some((canonical, rest))
}

/// Whether a statement list may leave its enclosing block early: a `break`
/// or `continue` of the innermost loop, or a `return`.
pub(in crate::lowering) fn stmts_may_jump(stmts: &[sv::ir::Stmt], in_loop: bool) -> bool {
    stmts.iter().any(|stmt| stmt_may_jump(stmt, in_loop))
}

pub(in crate::lowering) fn stmt_may_jump(stmt: &sv::ir::Stmt, in_loop: bool) -> bool {
    match stmt {
        sv::ir::Stmt::Break | sv::ir::Stmt::Continue => !in_loop,
        sv::ir::Stmt::Return(_) | sv::ir::Stmt::DisableBlock(_) => true,
        sv::ir::Stmt::Block { body, .. } => stmts_may_jump(body, in_loop),
        sv::ir::Stmt::If {
            then_body,
            else_body,
            ..
        } => stmts_may_jump(then_body, in_loop) || stmts_may_jump(else_body, in_loop),
        sv::ir::Stmt::Case { items, default, .. } => {
            items.iter().any(|item| stmts_may_jump(&item.body, in_loop))
                || default
                    .as_ref()
                    .is_some_and(|body| stmts_may_jump(body, in_loop))
        }
        // A `break` or `continue` inside a nested loop belongs to it.
        sv::ir::Stmt::Loop { body, .. } => stmts_may_jump(body, true),
        _ => false,
    }
}
