//! Native immediate predicates feed the shared procedural truth conversion.
use super::*;
use celox_sv_syntax::AssertionHeader;

impl BodyBuilder<'_, '_, '_> {
    pub(super) fn native_assertion_condition(
        &mut self,
        keyword: &str,
        header: &AssertionHeader,
    ) -> Result<Expr, AnalyzerError> {
        let AssertionHeader::Immediate { condition, .. } = header else {
            return Err(unsupported("concurrent assertion"));
        };
        if !matches!(keyword, "assert" | "assume" | "cover") {
            return Err(unsupported("immediate assertion keyword"));
        }
        self.native_chandle_value(*condition)
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

/// Disable conditions do not infer the assertion clock (16.9.3). Only
/// `$sampled`, which needs no clock, is supported here without an explicit one.
pub(super) fn concurrent_disable(expr: &Expr) -> Result<(), AnalyzerError> {
    let mut temporal = false;
    const_functions::visit_expr_calls(expr, &mut |callee, _| {
        temporal |= matches!(callee, Callee::System(function) if function.reads_sampled_values() && *function != SystemFunction::Sampled);
    });
    if temporal {
        return Err(unsupported(
            "sampled history function in disable iff requires an explicit clock",
        ));
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

#[cfg(test)]
mod tests {
    #[test]
    fn disable_conditions_do_not_infer_the_property_clock() {
        for condition in [
            "$past(a)",
            "$rose(a)",
            "$fell(a)",
            "$stable(a)",
            "$changed(a)",
            "$sampled($past(a))",
        ] {
            let source = format!(
                "module Top(input bit clk,a); assert property (@(posedge clk) disable iff ({condition}) a); endmodule"
            );
            let error = crate::analyze_source(&source, std::path::Path::new("disable-clock.sv"))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("disable iff requires an explicit clock"),
                "{error}"
            );
        }
        crate::analyze_source("module Top(input bit clk,a); assert property (@(posedge clk) disable iff ($sampled(a)) a); endmodule", std::path::Path::new("disable-sampled.sv")).unwrap();
    }
}
