use std::borrow::Cow;

use veryl_analyzer::ir::{Expression, Factor, FunctionBody, Statement, VarId};

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
        || !active.index.0.is_empty()
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
