//! Native immediate predicates feed the shared procedural truth conversion.
use super::*;
use celox_sv_syntax::AssertionHeader;

impl BodyBuilder<'_, '_, '_> {
    pub(super) fn native_assertion_condition(
        &self,
        keyword: &str,
        header: &AssertionHeader,
    ) -> Result<Expr, AnalyzerError> {
        let AssertionHeader::Immediate { condition, .. } = header else {
            return Err(unsupported("concurrent assertion"));
        };
        if !matches!(keyword, "assert" | "assume" | "cover") {
            return Err(unsupported("immediate assertion keyword"));
        }
        self.native_value(*condition)
            .map(procedural_truth_condition)
    }
}

/// Until property calls have their own sampled formal bindings, reject
/// calls that could read/write live state behind an operand's sampled copy.
pub(super) fn concurrent_operand(expr: &Expr) -> Result<(), AnalyzerError> {
    let mut unsupported_call = None;
    const_functions::visit_expr_calls(expr, &mut |callee, _| {
        if callee.is_effectful() {
            unsupported_call = Some(callee.to_string());
        }
    });
    if let Some(name) = unsupported_call {
        return Err(unsupported(format!(
            "call `{name}` in concurrent assertion operand"
        )));
    }
    Ok(())
}

pub(super) fn concurrent_action(body: &[Stmt]) -> Result<(), AnalyzerError> {
    let mut unsupported_action = false;
    for stmt in body {
        stmt.walk(&mut |stmt| {
            unsupported_action |= events::blocks(stmt)
                || matches!(
                    stmt,
                    Stmt::Delay { .. }
                        | Stmt::WaitEvent(_)
                        | Stmt::WaitImplicit(_)
                        | Stmt::Wait(_)
                        | Stmt::WaitFork
                        | Stmt::DisableFork
                        | Stmt::Fork { .. }
                        | Stmt::Call { .. }
                        | Stmt::Assign {
                            nonblocking: true,
                            ..
                        }
                        | Stmt::AssertionRegion { .. }
                );
        });
    }
    if unsupported_action {
        return Err(unsupported(
            "timed, nonblocking, or subroutine concurrent assertion action",
        ));
    }
    Ok(())
}
