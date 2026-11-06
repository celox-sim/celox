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

    #[test]
    fn actions_do_not_call_suspending_or_scheduling_subroutines() {
        for (callee, action, expected) in [
            (
                "task automatic wait_tick(); #1; endtask task automatic run(); wait_tick(); endtask",
                "run();",
                "concurrent assertion action that may suspend (through `run`)",
            ),
            (
                "task automatic late(); x <= 1; endtask task automatic run(); late(); endtask",
                "run();",
                "concurrent assertion action that performs a nonblocking assignment (through `run`)",
            ),
            (
                "function automatic void run(); fork x = 1; join_none endfunction",
                "run();",
                "concurrent assertion action that may suspend (through `run`)",
            ),
            (
                "task automatic poke(); ->> e; endtask task automatic run(); poke(); endtask",
                "run();",
                "concurrent assertion action that triggers an event or controls a process (through `run`)",
            ),
            (
                "function automatic void run(); -> e; endfunction",
                "run();",
                "concurrent assertion action that triggers an event or controls a process (through `run`)",
            ),
            (
                "",
                "->> e;",
                "concurrent assertion action that triggers an event or controls a process",
            ),
            (
                "task automatic stop(); disable fork; endtask task automatic run(); stop(); endtask",
                "run();",
                "concurrent assertion action that triggers an event or controls a process (through `run`)",
            ),
            (
                "",
                "disable fork;",
                "concurrent assertion action that triggers an event or controls a process",
            ),
            (
                "",
                "#1 x = 1;",
                "concurrent assertion action that may suspend",
            ),
            // An omitted argument's default is evaluated at the call.
            (
                "mailbox #(int) mb = new; function automatic void note(int placed = mb.try_put(1)); endfunction",
                "note();",
                "concurrent assertion action that triggers an event or controls a process",
            ),
            (
                "",
                "{x, y} <= 0;",
                "concurrent assertion action that performs a nonblocking assignment",
            ),
        ] {
            for action in [action.to_owned(), format!("else {action}")] {
                let source = format!(
                    "module Top(input bit clk); int x, y; event e; {callee} assert property (@(posedge clk) x == 0) {action} endmodule"
                );
                let error = crate::analyze_source(&source, std::path::Path::new("action-call.sv"))
                    .unwrap_err()
                    .to_string();
                assert!(error.contains(expected), "{source}: {error}");
            }
        }
        crate::analyze_source(
            "module Top(input bit clk); int x; task automatic run(); x = 1; endtask always @(posedge clk) run(); assert property (@(posedge clk) x == 0) run(); endmodule",
            std::path::Path::new("action-untimed.sv"),
        )
        .unwrap();
        // Reading event or process state neither suspends nor schedules.
        crate::analyze_source(
            "module Top(input bit clk); int x; event e; function automatic bit fired(); return e.triggered; endfunction assert property (@(posedge clk) x == 0) x = e.triggered; else x = fired(); endmodule",
            std::path::Path::new("action-triggered.sv"),
        )
        .unwrap();
    }

    /// Actions are found in every body, also a subroutine's; a concurrent
    /// assertion there is procedural, which is rejected before.
    #[test]
    fn subroutine_assertions_are_procedural() {
        let error = crate::analyze_source(
            "module Top(input bit clk); int x; task automatic wait_tick(); #1; endtask task automatic check(); assert property (@(posedge clk) x == 0) wait_tick(); endtask initial check(); endmodule",
            std::path::Path::new("task-assertion.sv"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("procedural concurrent assertion"), "{error}");
    }
}
