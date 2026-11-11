//! Recognition of module-level concurrent assertion monitors: the clock one
//! samples on, and the monitors that cannot fail in two-state mode.

use super::*;

/// The single edge clock of a module-level Boolean concurrent monitor.
/// General timed procedures and multi-event/derived clocks keep their existing
/// restrictions; a monitor's history advances once per completed iteration.
pub(super) fn concurrent_monitor_clock(body: &[sv::ir::Stmt]) -> Option<Clock> {
    use sv::ir::{EventEdge, Expr, LoopKind, Stmt};
    let [
        Stmt::Loop {
            kind: LoopKind::Forever,
            init,
            condition: None,
            step,
            body,
        },
    ] = body
    else {
        return None;
    };
    if !init.is_empty() || !step.is_empty() {
        return None;
    }
    let [
        Stmt::WaitEvent(items),
        Stmt::AssertionRegion { reactive: false },
        ..,
    ] = body.as_slice()
    else {
        return None;
    };
    let [item] = items.as_slice() else {
        return None;
    };
    // An iff expression is itself a clock expression: temporal calls there
    // require a separate explicit clock, which this subset does not support.
    if item.iff.is_some() {
        return None;
    }
    let Expr::Ident(signal) = &item.expr else {
        return None;
    };
    let rising = match item.edge {
        EventEdge::Pos => true,
        EventEdge::Neg => false,
        _ => return None,
    };
    Some(Clock {
        signal: signal.clone(),
        rising,
        alone: true,
    })
}

/// Whether `body` is a module-level concurrent assertion that can never
/// fail in two-state mode and whose pass action does nothing observable:
/// its predicate is true on every tick, so the monitor process, its region
/// transitions and its sampled-value readers can all be dropped. Anything
/// not in exactly that shape keeps its process.
pub(super) fn vacuous_two_state_monitor(body: &[sv::ir::Stmt]) -> bool {
    use sv::ir::{LoopKind, Stmt};
    let [
        Stmt::Loop {
            kind: LoopKind::Forever,
            init,
            condition: None,
            body: inner,
            ..
        },
    ] = body
    else {
        return false;
    };
    if !init.is_empty() {
        return false;
    }
    let [
        Stmt::WaitEvent(_),
        Stmt::AssertionRegion { reactive: false },
        rest @ ..,
    ] = inner.as_slice()
    else {
        return false;
    };
    let decision = match rest {
        // `disable iff` wraps the decision; it can only suppress attempts,
        // never make a true predicate fail. The guard itself is evaluated
        // on every tick, so it may only be discarded when it calls
        // nothing: an effectful guard (a state-updating function,
        // `$urandom`, ...) must keep its evaluation.
        [
            Stmt::If {
                condition: guard,
                then_body,
                else_body,
            },
        ] if then_body.is_empty() && else_body.len() == 1 && !expr_calls(guard, &mut |_| true) => {
            &else_body[0]
        }
        [decision] => decision,
        _ => return false,
    };
    let Stmt::If {
        condition,
        then_body,
        ..
    } = decision
    else {
        return false;
    };
    then_body
        .iter()
        .all(|stmt| matches!(stmt, Stmt::AssertionRegion { .. }))
        && two_state_condition_always_true(condition)
}

/// Whether `expr` is true on every evaluation in two-state mode, where no
/// stored value carries unknown bits. Only the shapes proved here count;
/// everything else is left to runtime evaluation.
pub(super) fn two_state_condition_always_true(expr: &sv::ir::Expr) -> bool {
    use sv::ir::{BinaryOp, Callee, Expr, SystemFunction, UnaryOp};
    if let Some((value, mask)) = two_state_literal(expr) {
        return mask.is_zero() && !value.is_zero();
    }
    match expr {
        // Truthiness-preserving wrappers the assertion transform puts
        // around a Boolean predicate: sampled in Preponed, converted to
        // two-state and reduced to one bit.
        Expr::Unary {
            op: UnaryOp::RedOr | UnaryOp::Plus | UnaryOp::ToTwoState,
            expr,
        } => two_state_condition_always_true(expr),
        Expr::Call {
            callee: Callee::System(SystemFunction::Sampled),
            args,
            ..
        } if args.len() == 1 => args[0]
            .as_ref()
            .is_some_and(two_state_condition_always_true),
        // `known !== <literal with X/Z>` (or mirrored) can never be an
        // exact match for the literal, whatever the known side holds
        // (IEEE 1800-2023 11.4.10): extending either side to the common
        // width keeps the literal's unknown bits and adds none to the
        // known side.
        Expr::Binary {
            left,
            op: BinaryOp::NeCase,
            right,
        } => {
            (two_state_literal(left).is_some_and(|(_, mask)| !mask.is_zero())
                && two_state_mask_zero(right))
                || (two_state_literal(right).is_some_and(|(_, mask)| !mask.is_zero())
                    && two_state_mask_zero(left))
        }
        Expr::Binary {
            left,
            op: BinaryOp::LogicAnd,
            right,
        } => two_state_condition_always_true(left) && two_state_condition_always_true(right),
        _ => false,
    }
}

/// Whether `expr` provably carries no unknown bits in two-state mode.
/// Stored values are two-state there, so unknown bits can only come from
/// a literal that carries them or from an operation that produces them
/// out of known operands; both are refused here, as is anything whose
/// result this function cannot bound.
pub(super) fn two_state_mask_zero(expr: &sv::ir::Expr) -> bool {
    use sv::ir::{BinaryOp, Callee, Expr, SystemFunction, UnaryOp};
    match expr {
        Expr::Literal(_) => two_state_literal(expr).is_some_and(|(_, mask)| mask.is_zero()),
        Expr::Ident(_) => true,
        // A sampled or past value is its first operand's value, so a
        // literal operand that carries unknown bits carries them through.
        Expr::Call {
            callee: Callee::System(SystemFunction::Sampled | SystemFunction::Past),
            args,
            ..
        } => args
            .first()
            .and_then(Option::as_ref)
            .is_some_and(two_state_mask_zero),
        Expr::Call { .. } => false,
        // `inside` compares with `==?` and the relational operators, which
        // are unknown when an operand is (11.4.13).
        Expr::Inside { expr, items } => {
            two_state_mask_zero(expr)
                && items
                    .iter()
                    .flat_map(sv::ir::InsideItem::exprs)
                    .all(two_state_mask_zero)
        }
        Expr::Unary { op, expr } => matches!(op, UnaryOp::ToTwoState) || two_state_mask_zero(expr),
        Expr::Binary { left, op, right } => match op {
            BinaryOp::EqCase | BinaryOp::NeCase => true,
            // A zero divisor, or zero raised to a negative power, gives X
            // from known operands (11.4.3).
            BinaryOp::Div | BinaryOp::Mod | BinaryOp::Pow => false,
            _ => two_state_mask_zero(left) && two_state_mask_zero(right),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            two_state_mask_zero(condition)
                && two_state_mask_zero(then_expr)
                && two_state_mask_zero(else_expr)
        }
        Expr::Concat(parts) => parts.iter().all(two_state_mask_zero),
        Expr::RepeatConcat { parts, .. } => parts.iter().all(two_state_mask_zero),
        Expr::Resize { expr, .. } => two_state_mask_zero(expr),
        // A select outside its operand's bounds, or at an unknown
        // position, reads X whatever the operand holds (11.5.1, 7.4.5),
        // and the bounds are not known here.
        Expr::Select { .. } => false,
    }
}

/// The `(value, unknown mask)` of a sized literal; `None` for anything
/// else. Constant operators are deliberately not folded here: their
/// operands take their width and signedness from the enclosing
/// expression (11.6, 11.8), which only the lowering tracks. An unbased
/// unsized literal is sized by that context too (5.7.1).
pub(super) fn two_state_literal(expr: &sv::ir::Expr) -> Option<(BigUint, BigUint)> {
    let sv::ir::Expr::Literal(text) = expr else {
        return None;
    };
    let text = text.trim();
    if text.len() == 2 && text.starts_with('\'') {
        return None;
    }
    let literal = sv::typecheck::parse_integral_literal(text)?;
    Some((literal.value, literal.mask))
}
