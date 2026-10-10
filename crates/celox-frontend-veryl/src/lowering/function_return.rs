use std::borrow::Cow;

use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignDestination, CasePattern, Expression, Factor, ForBound, ForRange,
    FunctionBody, FunctionCall, Statement, SystemFunctionCall, SystemFunctionKind, VarId, VarIndex,
    VarSelect,
};

/// Recover implicit returns for Celox's return-aware function evaluators.
///
/// Veryl now follows return-value assignments with writes to a synthetic
/// `return.active` flag, guards the remaining statements, and adds loop breaks.
/// Celox already tracks function exits separately from loop breaks. Keeping both
/// representations would skip the flag writes and leave stale guards/loop state.
/// Only remove this analyzer-generated control; source breaks remain intact.
pub(super) fn implicit_return_body(body: &FunctionBody) -> Cow<'_, FunctionBody> {
    let Some(Statement::Assign(init)) = body.statements.first() else {
        return Cow::Borrowed(body);
    };
    let [active] = init.dst.as_slice() else {
        return Cow::Borrowed(body);
    };
    // The dot is part of a single interned identifier, which cannot occur in a
    // source identifier (including a user field access named `return.active`).
    if body.ret.is_none()
        || !active
            .path
            .0
            .last()
            .is_some_and(|name| name.to_string() == "return.active")
        || !active.index.indices.is_empty()
        || !active.select.0.is_empty()
        || active.select.1.is_some()
    {
        return Cow::Borrowed(body);
    }

    let mut normalized = body.clone();
    remove_return_control(&mut normalized.statements, active.id);
    Cow::Owned(normalized)
}

fn remove_return_control(statements: &mut Vec<Statement>, active: VarId) {
    let mut normalized = Vec::with_capacity(statements.len());
    for mut statement in std::mem::take(statements) {
        match &mut statement {
            Statement::Assign(assign) if assign.dst.iter().any(|dst| dst.id == active) => continue,
            Statement::If(branch) => {
                if matches!(&branch.cond, Expression::Term(factor)
                    if matches!(factor.as_ref(), Factor::Variable(id, ..) if *id == active))
                {
                    // A guarded suffix has an empty else; an unwind check has
                    // an empty then and a synthetic break in its else.
                    remove_return_control(&mut branch.true_side, active);
                    normalized.append(&mut branch.true_side);
                    continue;
                }
                remove_return_control(&mut branch.true_side, active);
                remove_return_control(&mut branch.false_side, active);
            }
            Statement::Case(case) => {
                for arm in &mut case.arms {
                    remove_return_control(&mut arm.body, active);
                }
                remove_return_control(&mut case.default, active);
            }
            Statement::For(loop_stmt) => remove_return_control(&mut loop_stmt.body, active),
            _ => {}
        }
        normalized.push(statement);
    }
    *statements = normalized;
}

/// Give every expression in `body` a token id of its own.
///
/// The analyzer unrolls constant-bound loops by cloning the loop body, so the
/// copies share source tokens. FF function lowering memoizes materialized
/// values by expression token; without distinct ids, a later iteration would
/// reuse a register defined in an earlier iteration's branch. Only the ids
/// change, so source locations in diagnostics are preserved.
pub(super) fn refresh_expression_tokens(body: &mut FunctionBody) {
    refresh_statements(&mut body.statements);
}

fn refresh_token(token: &mut veryl_parser::token_range::TokenRange) {
    token.beg.id = veryl_parser::resource_table::new_token_id();
}

fn refresh_statements(statements: &mut [Statement]) {
    for statement in statements {
        match statement {
            Statement::Assign(assign) => {
                for dst in &mut assign.dst {
                    refresh_destination(dst);
                }
                refresh_expression(&mut assign.expr);
            }
            Statement::If(statement) => {
                refresh_expression(&mut statement.cond);
                refresh_statements(&mut statement.true_side);
                refresh_statements(&mut statement.false_side);
            }
            Statement::IfReset(statement) => {
                refresh_statements(&mut statement.true_side);
                refresh_statements(&mut statement.false_side);
            }
            Statement::Case(statement) => {
                refresh_expression(&mut statement.case_target);
                for arm in &mut statement.arms {
                    for pattern in &mut arm.patterns {
                        match pattern {
                            CasePattern::Eq(expr) => refresh_expression(expr),
                            CasePattern::Range { lo, hi, .. } => {
                                refresh_expression(lo);
                                refresh_expression(hi);
                            }
                        }
                    }
                    refresh_statements(&mut arm.body);
                }
                refresh_statements(&mut statement.default);
            }
            Statement::For(statement) => {
                let (start, end) = match &mut statement.range {
                    ForRange::Forward { start, end, .. }
                    | ForRange::Reverse { start, end, .. }
                    | ForRange::Stepped { start, end, .. } => (start, end),
                };
                for bound in [start, end] {
                    if let ForBound::Expression(expr) = bound {
                        refresh_expression(expr);
                    }
                }
                refresh_statements(&mut statement.body);
            }
            Statement::SystemFunctionCall(call) => refresh_system_call(call),
            Statement::FunctionCall(call) => refresh_call(call),
            Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => {}
        }
    }
}

fn refresh_destination(dst: &mut AssignDestination) {
    refresh_selects(&mut dst.index, &mut dst.select);
}

fn refresh_selects(index: &mut VarIndex, select: &mut VarSelect) {
    for expr in index.expressions_mut().chain(select.0.iter_mut()) {
        refresh_expression(expr);
    }
    if let Some((_, expr)) = &mut select.1 {
        refresh_expression(expr);
    }
}

fn refresh_call(call: &mut FunctionCall) {
    refresh_token(&mut call.comptime.token);
    for expr in call.inputs.values_mut() {
        refresh_expression(expr);
    }
    for dst in call.outputs.values_mut().flatten() {
        refresh_destination(dst);
    }
}

fn refresh_system_call(call: &mut SystemFunctionCall) {
    match &mut call.kind {
        SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
            for arg in args {
                refresh_expression(&mut arg.0);
            }
        }
        SystemFunctionKind::Assert { cond, args, .. } => {
            refresh_expression(&mut cond.0);
            for arg in args {
                refresh_expression(&mut arg.0);
            }
        }
        SystemFunctionKind::Bits(arg)
        | SystemFunctionKind::Size(arg, _)
        | SystemFunctionKind::Clog2(arg)
        | SystemFunctionKind::Onehot(arg)
        | SystemFunctionKind::Signed(arg)
        | SystemFunctionKind::Unsigned(arg)
        | SystemFunctionKind::Readmemh(arg, _) => refresh_expression(&mut arg.0),
        SystemFunctionKind::Finish => {}
    }
}

fn refresh_expression(expr: &mut Expression) {
    refresh_token(&mut expr.comptime_mut().token);
    match expr {
        Expression::Term(factor) => match factor.as_mut() {
            Factor::Variable(_, index, select, _) => refresh_selects(index, select),
            Factor::HierVariable(reference) => {
                refresh_selects(&mut reference.index, &mut reference.select)
            }
            Factor::FunctionCall(call) => refresh_call(call),
            Factor::SystemFunctionCall(call) => refresh_system_call(call),
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => {}
        },
        Expression::Binary(lhs, _, rhs, _) => {
            refresh_expression(lhs);
            refresh_expression(rhs);
        }
        Expression::Unary(_, inner, _) => refresh_expression(inner),
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            refresh_expression(cond);
            refresh_expression(then_expr);
            refresh_expression(else_expr);
        }
        Expression::Concatenation(items, _) => {
            for (expr, repeat) in items {
                refresh_expression(expr);
                if let Some(repeat) = repeat {
                    refresh_expression(repeat);
                }
            }
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expr, repeat) => {
                        refresh_expression(expr);
                        if let Some(repeat) = repeat {
                            refresh_expression(repeat);
                        }
                    }
                    ArrayLiteralItem::Defaul(expr) => refresh_expression(expr),
                }
            }
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, expr) in fields {
                refresh_expression(expr);
            }
        }
    }
}
