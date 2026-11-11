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

/// One step of a sequence over cycles as each tick of the clock decides it
/// (IEEE 1800-2023 16.7, 16.12.7): the attempt that started as many ticks
/// ago as the step's offset reaches it if every earlier step matched.
pub(super) struct SequenceStage {
    /// Whether that attempt reaches the step; the first step always.
    pub alive: Option<Expr>,
    /// Whether the step matches at this tick.
    pub matched: Expr,
    /// An antecedent step, whose miss makes the attempt vacuous.
    pub antecedent: bool,
    /// The last step, whose match makes the attempt succeed.
    pub last: bool,
    /// The subroutine calls of its match items (16.11).
    pub calls: Vec<Stmt>,
    /// The step's offset in ticks from the start of its attempt.
    pub offset: u32,
}

/// A sequence over cycles as its monitor decides it at each tick.
pub(super) struct SequenceMonitor {
    pub stages: Vec<SequenceStage>,
    /// The monitor's count of the ticks it has seen, saturating at the
    /// sequence's length: an attempt `k` ticks old exists only once `k`
    /// ticks have passed.
    pub ticks: Option<(String, u32)>,
    /// The statements that end each tick: they keep the values that later
    /// ticks read.
    pub updates: Vec<Stmt>,
    /// The statements that start each tick in the Observed region, before
    /// any action runs: they read whether assertions are on (20.11).
    pub start: Vec<Stmt>,
}

/// A value of an attempt that a later tick reads, a step's match or a
/// local variable's assignment: computed at each tick from that tick's
/// sampled values and kept for as many ticks as the latest read needs.
pub(super) struct SequenceRegister {
    /// Its value at this tick, sampled.
    pub now: Expr,
    pub ty: Type,
    /// The values of the previous ticks, the latest first.
    pub chain: Vec<String>,
}

/// An assignment to a local variable (16.10): how many assignments of the
/// sequence precede it, the offset of its step, the value with the local
/// reads it makes replaced but not yet sampled, and its register.
pub(super) struct SequenceAssignment {
    pub order: usize,
    pub offset: u32,
    pub value: Expr,
    pub register: usize,
}

/// The local variables of a sequence by unique name, with their
/// assignments in order.
pub(super) type SequenceLocals = HashMap<String, Vec<SequenceAssignment>>;

/// A point of an attempt: the offset of a step and how many local variable
/// assignments of the sequence precede it, so a match item sees the
/// assignments listed before it in its own step (16.10, 16.11).
#[derive(Clone, Copy)]
pub(super) struct SequencePoint {
    pub at: u32,
    pub before: usize,
}

/// `expr` evaluated over sampled values (16.5.1, 16.6): every variable it
/// reads, also in a select's bounds and a function's arguments, takes its
/// sampled value, and a function is evaluated on its sampled arguments.
pub(super) fn sampled(expr: Expr) -> Expr {
    Expr::Call {
        callee: Callee::System(SystemFunction::Sampled),
        args: vec![Some(expr)],
        arg_order: Vec::new(),
    }
}

/// Whether `expr` names one of `locals`.
fn reads_local(expr: &Expr, locals: &SequenceLocals) -> bool {
    let mut reads = false;
    rename_expr_idents(&mut expr.clone(), &mut |name| {
        reads |= locals.contains_key(name.as_str());
        None
    });
    reads
}

/// Replace each identifier of `expr` that `values` gives a value for.
/// `None` when such an identifier is a select's bound, which takes no
/// expression.
fn substitute(expr: &mut Expr, values: &HashMap<String, Expr>) -> Option<()> {
    let mut bound = false;
    let mut check = |bounds: &[&ConstExpr]| {
        for bound_expr in bounds {
            rename_const_idents(&mut (*bound_expr).clone(), &mut |name| {
                bound |= values.contains_key(name.as_str());
                None
            });
        }
    };
    match expr {
        Expr::Ident(name) => {
            if let Some(value) = values.get(name.as_str()) {
                *expr = value.clone();
            }
        }
        Expr::Literal(_) => {}
        Expr::Select { expr, msb, lsb, .. } => {
            check(&[msb, lsb]);
            substitute(expr, values)?;
        }
        Expr::Concat(parts) => {
            for part in parts {
                substitute(part, values)?;
            }
        }
        Expr::RepeatConcat { count, parts } => {
            check(&[count]);
            for part in parts {
                substitute(part, values)?;
            }
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => substitute(expr, values)?,
        Expr::Binary { left, right, .. } => {
            substitute(left, values)?;
            substitute(right, values)?;
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            substitute(condition, values)?;
            substitute(then_expr, values)?;
            substitute(else_expr, values)?;
        }
        Expr::Call { args, .. } => {
            for arg in args.iter_mut().flatten() {
                substitute(arg, values)?;
            }
        }
        Expr::Inside { expr, items } => {
            substitute(expr, values)?;
            for item in items {
                match item {
                    InsideItem::Value(value) => substitute(value, values)?,
                    InsideItem::Range { low, high } => {
                        substitute(low, values)?;
                        substitute(high, values)?;
                    }
                }
            }
        }
    }
    (!bound).then_some(())
}

impl BodyBuilder<'_, '_, '_> {
    /// `expr` at `point` of an attempt with each read of a local variable
    /// replaced by the value of its last earlier assignment: the value
    /// itself when that assignment is at the same tick, else the register
    /// that keeps it. The result is not yet sampled; [`sampled`] samples
    /// it whole, the registers included, which change only at the end of
    /// a tick.
    pub(super) fn sequence_value(
        &mut self,
        expr: &Expr,
        point: SequencePoint,
        locals: &SequenceLocals,
        registers: &mut [SequenceRegister],
    ) -> Converted<Expr> {
        if !reads_local(expr, locals) {
            return Ok(expr.clone());
        }
        // A sampled-value function keeps the history of what it names,
        // which a local variable has none of.
        let mut history = false;
        const_functions::visit_expr_calls(expr, &mut |callee, args| {
            if let Callee::System(function) = callee
                && function.reads_sampled_values()
            {
                match args {
                    const_functions::CallArguments::Expressions(args) => {
                        history |= args.iter().flatten().any(|arg| reads_local(arg, locals));
                    }
                    const_functions::CallArguments::Constants(args) => {
                        for arg in args {
                            rename_const_idents(&mut arg.clone(), &mut |name| {
                                history |= locals.contains_key(name.as_str());
                                None
                            });
                        }
                    }
                }
            }
        });
        if history {
            return Err(unsupported(
                "sampled-value function of a local variable of a sequence",
            ));
        }
        // The assignment each local read sees.
        let mut reads = Vec::new();
        let mut missing = false;
        rename_expr_idents(&mut expr.clone(), &mut |name| {
            if let Some(assignments) = locals.get(name.as_str()) {
                match assignments
                    .iter()
                    .rev()
                    .find(|assignment| assignment.order < point.before)
                {
                    Some(assignment) => reads.push((name.clone(), assignment)),
                    None => missing = true,
                }
            }
            None
        });
        if missing {
            return Err(unsupported(
                "local variable of a sequence read before a step assigns it",
            ));
        }
        // A register is a variable, which also a select's bound may name.
        let mut values = HashMap::default();
        let mut renames = HashMap::default();
        for (name, assignment) in reads {
            let ago = point.at - assignment.offset;
            if ago == 0 {
                values.insert(name, assignment.value.clone());
            } else {
                let register = self.sequence_register(registers, assignment.register, ago);
                renames.insert(name, register);
            }
        }
        let mut result = expr.clone();
        rename_expr_idents(&mut result, &mut |name| {
            if let Some(register) = renames.get(name.as_str()) {
                *name = register.clone();
            }
            None
        });
        substitute(&mut result, &values).ok_or_else(|| {
            unsupported("select bound reading a local variable of a sequence assigned in its step")
        })?;
        Ok(result)
    }

    /// The variable of `registers[index]` that holds its value of `ago`
    /// ticks before this one.
    pub(super) fn sequence_register(
        &mut self,
        registers: &mut [SequenceRegister],
        index: usize,
        ago: u32,
    ) -> String {
        let register = &mut registers[index];
        while register.chain.len() < ago as usize {
            let delay = register.chain.len() + 1;
            let name = self.declare(
                &format!("past{index}@{delay}@sequence"),
                register.ty.clone(),
            );
            register.chain.push(name);
        }
        register.chain[ago as usize - 1].clone()
    }
}

/// The statements that end a tick: each register's chain moves one tick
/// back and takes the value of this tick, all computed before any moves.
pub(super) fn register_updates(
    registers: Vec<SequenceRegister>,
    mut temporary: impl FnMut(&Type) -> String,
) -> Vec<Stmt> {
    let assign = |name: String, rhs: Expr| Stmt::Assign {
        lhs: LValue::Ident(name),
        rhs,
        nonblocking: false,
    };
    let mut computed = Vec::new();
    let mut moves = Vec::new();
    for register in registers {
        if register.chain.is_empty() {
            continue;
        }
        let now = temporary(&register.ty);
        computed.push(assign(now.clone(), register.now));
        for delay in (1..register.chain.len()).rev() {
            moves.push(assign(
                register.chain[delay].clone(),
                Expr::Ident(register.chain[delay - 1].clone()),
            ));
        }
        moves.push(assign(register.chain[0].clone(), Expr::Ident(now)));
    }
    computed.extend(moves);
    computed
}

/// The default failure action of a native immediate assertion.
/// The default fail action of an `assert` or `assume`: a call to `$error`
/// at the assertion (IEEE 1800-2023 20.10).
pub(super) fn assertion_failure(
    keyword: &str,
    site: Option<crate::procedural::SourceSpan>,
) -> Vec<Stmt> {
    vec![Stmt::SystemTask {
        task: SystemTask::Severity(system_functions::Severity::Error),
        args: vec![SystemTaskArg::Str(format!("{keyword} failed"))],
        site,
        assertion: None,
    }]
}

/// Check the action of a deferred assertion: one subroutine call (IEEE
/// 1800-2023 16.4). Only display and severity tasks are supported, since
/// a user subroutine would run in the Reactive region and could wake
/// processes there.
pub(super) fn deferred_action(body: &[Stmt]) -> Result<(), AnalyzerError> {
    match body {
        [] => Ok(()),
        [
            Stmt::SystemTask {
                task:
                    SystemTask::Display(_)
                    | SystemTask::Write(_)
                    | SystemTask::Severity(_)
                    | SystemTask::Fatal,
                ..
            },
        ] => Ok(()),
        _ => Err(unsupported(
            "deferred assertion action other than one display or severity task call",
        )),
    }
}

/// Attribute the severity tasks of an assertion's action block to it, and
/// for a deferred assertion its display tasks, which wait in the same
/// queue. Tasks in subroutines the block calls are not attributed, and the
/// tasks of a nested assertion keep its own attribution.
pub(super) fn attribute_reports(
    body: &mut [Stmt],
    assertion: &celox_design::AssertionSite,
    outcome: celox_design::AssertionOutcome,
) {
    for stmt in body {
        stmt.walk_mut(&mut |stmt| {
            let deferred = assertion.deferral != celox_design::AssertionDeferral::None;
            if let Stmt::SystemTask {
                task,
                assertion: attributed @ None,
                ..
            } = stmt
                && (matches!(task, SystemTask::Severity(_) | SystemTask::Fatal)
                    || deferred && matches!(task, SystemTask::Display(_) | SystemTask::Write(_)))
            {
                *attributed = Some(celox_design::AssertionSite {
                    outcome,
                    ..assertion.clone()
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    fn analyze(locals: &str, property: &str) -> Result<(), String> {
        let source = format!(
            "module Top(input bit clk, b, input bit [7:0] bus, input int i); \
             int g; function automatic int reads_g(); return g; endfunction \
             function automatic int twice(int v); return v * 2; endfunction \
             function automatic void note(int v); endfunction bit [7:0] mem [0:3]; \
             function automatic void fill(output int v); v = 1; endfunction \
             property p; {locals} @(posedge clk) {property}; endproperty \
             assert property (p); endmodule"
        );
        crate::analyze_source(&source, std::path::Path::new("steps.sv"))
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Every value of a sequence is evaluated over the sampled values of
    /// the tick that decides it and kept for later ticks, so sampled-value
    /// functions, `$time`, other system functions and variable selects in any
    /// step are supported (IEEE 1800-2023 16.6, 16.10).
    #[test]
    fn values_of_earlier_steps_are_kept_from_their_ticks() {
        for (locals, property) in [
            ("", "$rose(b) ##1 b"),
            ("", "($time == 1) |-> ##1 b"),
            ("int idx;", "(1, idx = i) ##1 bus[idx]"),
            ("int x;", "(b, x = $countones(bus)) ##1 x == i"),
            ("", "b ##1 bus[i] == $countones(bus)"),
            ("", "(b, $readmemh(\"m.hex\", mem)) ##1 b"),
        ] {
            if let Err(error) = analyze(locals, property) {
                panic!("{property}: {error}");
            }
        }
    }

    /// What a sequence cannot keep is rejected: a select of a local as an
    /// assignment target, a sampled-value function of a local, a select
    /// bound reading a local assigned in its own step, and a user function,
    /// whose reads of module variables would not be sampled.
    #[test]
    fn values_a_sequence_cannot_keep_are_rejected() {
        for (locals, property, expected) in [
            (
                "int x;",
                "(b, x[0] = 1) ##1 x",
                "select of a local variable",
            ),
            (
                "int x;",
                "(b, x = i) ##1 $rose(x)",
                "sampled-value function of a local",
            ),
            (
                "int x;",
                "(b, x = i, x = bus[x]) ##1 x",
                "select bound reading a local",
            ),
            ("int x;", "(b, x = twice(i)) ##1 x == i", "call `twice`"),
            ("", "(b, note(reads_g())) ##1 b", "call `reads_g`"),
            (
                "int x;",
                "(b, x = i, fill(x)) ##1 b",
                "passed to an output, inout or ref",
            ),
            ("", "b ##1025 b", "sequence longer than 1024 ticks"),
            ("", "b ##2147483647 b", "sequence longer than 1024 ticks"),
        ] {
            let Err(error) = analyze(locals, property) else {
                panic!("{property} is accepted");
            };
            assert!(error.contains(expected), "{property}: {error}");
        }
    }

    /// `p iff q` of two Booleans is supported as a property and as the
    /// consequent of an implication, which binds looser (IEEE 1800-2023
    /// 16.12.8, Table 16-3); an operand over cycles, a chain and a named
    /// operand are rejected.
    #[test]
    fn iff_of_booleans() {
        for property in ["b iff bus[0]", "b |-> b iff bus[0] == g"] {
            if let Err(error) = analyze("", property) {
                panic!("{property}: {error}");
            }
        }
        for property in ["b ##1 b iff b", "b iff b iff b"] {
            let source = format!(
                "module Top(input bit clk, b); assert property (@(posedge clk) {property}); endmodule"
            );
            let Err(error) = crate::analyze_source(&source, std::path::Path::new("iff.sv")) else {
                panic!("{property} is accepted");
            };
            let error = error.to_string();
            assert!(
                error.contains("iff of properties over cycles"),
                "{property}: {error}"
            );
            // A named property with it is captured unconverted.
            if let Ok(()) = analyze("", property) {
                panic!("{property} is accepted in a named property");
            }
        }
        let source = "module Top(input bit clk, a, b); sequence s; a; endsequence \
                      assert property (@(posedge clk) s iff b); endmodule";
        let Err(error) = crate::analyze_source(source, std::path::Path::new("iff.sv")) else {
            panic!("a named operand of iff is accepted");
        };
        assert!(error.to_string().contains("operand of iff"), "{error}");
    }

    /// `expect` (IEEE 1800-2023 16.17) takes a property with an explicit
    /// clock, also a named one, and rejects what its one attempt cannot
    /// decide yet.
    #[test]
    fn expect_statements() {
        let analyze = |body: &str| {
            let source = format!(
                "module Top(input bit clk, b); sequence s; b ##1 b; endsequence \
                 sequence kept; int x; (b, x = 1) ##1 x == 1; endsequence \
                 property p; @(posedge clk) b |=> b; endproperty {body} endmodule"
            );
            crate::analyze_source(&source, std::path::Path::new("expect.sv"))
                .map(|_| ())
                .map_err(|error| error.to_string())
        };
        for body in [
            "initial expect (@(posedge clk) b ##2 b);",
            "initial expect (@(posedge clk) s) else $display(\"miss\");",
            "task automatic t(int v); expect (@(posedge clk) b |=> v == 1); endtask",
            "initial expect (p);",
            "initial expect (@(posedge clk) b |-> b iff b);",
        ] {
            if let Err(error) = analyze(body) {
                panic!("{body}: {error}");
            }
        }
        for (body, expected) in [
            (
                "initial expect (b);",
                "expect statement without a clocking event",
            ),
            (
                "initial expect (@(posedge clk) disable iff (b) b);",
                "disable iff in an expect statement",
            ),
            (
                "initial expect (@(posedge clk) kept);",
                "local variable of a sequence in an expect statement",
            ),
            (
                "initial expect (@(posedge clk) (b, $display(\"m\")) ##1 b);",
                "match item in an expect statement",
            ),
            (
                "initial expect (@(posedge clk) b ##1025 b);",
                "sequence longer than 1024 ticks",
            ),
        ] {
            let Err(error) = analyze(body) else {
                panic!("{body} is accepted");
            };
            assert!(error.contains(expected), "{body}: {error}");
        }
    }

    /// A declaration whose body starts with a cast of a built-in type
    /// declares no local variable.
    #[test]
    fn leading_casts_are_no_local_declarations() {
        let source = "module Top(input bit clk, b); sequence s; int'(b) ##1 b; endsequence \
                      assert property (@(posedge clk) s); endmodule";
        if let Err(error) = crate::analyze_source(source, std::path::Path::new("cast.sv")) {
            panic!("{error}");
        }
    }

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
