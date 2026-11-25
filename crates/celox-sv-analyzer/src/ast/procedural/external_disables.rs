//! `disable` of a named block from outside it (IEEE 1800-2023 9.6.2).
//!
//! A `disable` names a block that runs in another process, or in the same
//! process outside the statement: the block's activity ends and its process
//! continues after the block. A named block that some `disable` of the scope
//! names runs as the one branch of a `fork`-`join`, which records the branch
//! process in a variable of the scope while it runs:
//!
//! ```text
//! begin : b ... end   fork begin handle = self(); armed = 1; ...; armed = 0; end join
//! disable b;          if (armed) begin kill(handle); armed = 0; end
//! ```
//!
//! Killing the branch completes the join, so the process of the block
//! continues after it.
use super::*;

/// The named blocks of a scope that a `disable` reaches from outside them.
#[derive(Default)]
pub(in crate::ast) struct ExternalDisables {
    /// The simple names that a `disable` of the scope names from outside
    /// every block of that name, found once.
    outside: Option<HashSet<String>>,
    /// The blocks that run as a branch, by name.
    pub(in crate::ast) blocks: HashSet<String>,
    /// The names of more than one such block, which no `disable` from
    /// outside may name.
    pub(in crate::ast) ambiguous: HashSet<String>,
    /// The names that a `disable` outside its block names.
    pub(in crate::ast) targets: Vec<String>,
    /// The labels whose block may run in more than one process at once:
    /// it sits in a fork that a loop, an enclosing fork or a repeating
    /// process can start again while an earlier branch still runs. One
    /// process variable cannot record every activation (9.6.2).
    concurrent: HashSet<String>,
    /// The labels whose block sits in a fork; a repeating process starts
    /// such a fork again, which `concurrent` cannot see in the tokens.
    in_fork: HashSet<String>,
    /// The subroutines each block that runs as a branch calls, by label,
    /// which the scope checks once every subroutine is known.
    pub(in crate::ast) calls: HashMap<String, Vec<String>>,
}

/// The variables of the branch of block `label`: its process and whether it
/// runs.
pub(in crate::ast) fn variables(label: &str) -> (String, String) {
    (
        format!("disable%{label}%process"),
        format!("disable%{label}%armed"),
    )
}

/// The variables of the scope that the branch of block `label` keeps.
pub(in crate::ast) fn signals(label: &str, types: &mut semantic_types::TyTable) -> [Signal; 2] {
    let (process, armed) = variables(label);
    [
        Signal::new(process, classes::handle(types, processes::CLASS)),
        Signal::new(armed, control::unsigned_type(1)),
    ]
}

impl BodyBuilder<'_, '_, '_> {
    /// Whether a `disable` of the scope names block `label` from outside
    /// every block of that name: one inside such a block names that block
    /// or one within it (9.6.2, 23.8).
    fn disabled_from_outside(&mut self, label: &str) -> bool {
        if self.state.disables.outside.is_none() {
            let native = self.tree.declarations();
            let scope = match self.syntax().scope_handle().unit(native) {
                Some(unit) => unit.tokens,
                None => celox_sv_syntax::TokenRange {
                    start: 0,
                    end: native.tokens.len(),
                },
            };
            let within = |tokens: celox_sv_syntax::TokenRange| {
                scope.start <= tokens.start && tokens.end <= scope.end
            };
            let mut blocks: HashMap<String, Vec<celox_sv_syntax::TokenRange>> = HashMap::default();
            let mut disables = Vec::new();
            let scoped: Vec<_> = native
                .statements
                .iter()
                .filter(|s| within(s.tokens))
                .collect();
            // A block in a fork that a loop or an enclosing fork around the
            // fork can start again may run in two processes at once.
            for statement in &scoped {
                let celox_sv_syntax::StatementKind::Block {
                    label: Some(label), ..
                } = &statement.kind
                else {
                    continue;
                };
                let label = instances::normalize_identifier(&label.spelling);
                let range = statement.tokens;
                let contains = |outer: celox_sv_syntax::TokenRange| {
                    outer.start <= range.start && range.end <= outer.end && outer != range
                };
                let ancestors: Vec<_> = scoped
                    .iter()
                    .filter(|other| contains(other.tokens))
                    .collect();
                let Some(fork) = ancestors
                    .iter()
                    .filter(|other| {
                        matches!(other.kind, celox_sv_syntax::StatementKind::Parallel { .. })
                    })
                    .min_by_key(|other| other.tokens.end - other.tokens.start)
                else {
                    continue;
                };
                self.state.disables.in_fork.insert(label.clone());
                let restarted = ancestors.iter().any(|other| {
                    other.tokens.start <= fork.tokens.start
                        && fork.tokens.end <= other.tokens.end
                        && other.tokens != fork.tokens
                        && matches!(
                            other.kind,
                            celox_sv_syntax::StatementKind::Parallel { .. }
                                | celox_sv_syntax::StatementKind::Loop { .. }
                        )
                });
                if restarted {
                    self.state.disables.concurrent.insert(label);
                }
            }
            for statement in scoped {
                match &statement.kind {
                    celox_sv_syntax::StatementKind::Block {
                        label: Some(label), ..
                    } => blocks
                        .entry(instances::normalize_identifier(&label.spelling))
                        .or_default()
                        .push(statement.tokens),
                    celox_sv_syntax::StatementKind::Disable(tokens) => {
                        let mut words = native
                            .tokens
                            .get(tokens.start..tokens.end)
                            .into_iter()
                            .flatten()
                            .filter(|token| !token.is_trivia())
                            .map(|token| token.text.as_str())
                            .skip_while(|word| *word == "disable");
                        if let Some(name) = words.next()
                            && !matches!(words.next(), Some("." | "::"))
                        {
                            disables.push((instances::normalize_identifier(name), tokens.start));
                        }
                    }
                    _ => {}
                }
            }
            let outside = disables
                .into_iter()
                .filter(|(name, at)| {
                    !blocks.get(name).is_some_and(|ranges| {
                        ranges
                            .iter()
                            .any(|range| range.start <= *at && *at < range.end)
                    })
                })
                .map(|(name, _)| name)
                .collect();
            self.state.disables.outside = Some(outside);
        }
        self.state
            .disables
            .outside
            .as_ref()
            .is_some_and(|names| names.contains(label))
    }

    /// The statements of named block `label`, `body`, as the branch that a
    /// `disable` from outside it ends, when some `disable` of the scope
    /// outside the block names it; otherwise `body` itself. A block whose statements leave it
    /// otherwise, or in a context without processes, stays as it is, and a
    /// `disable` from outside it remains unsupported.
    pub(super) fn external_disable_block(&mut self, label: String, body: Vec<Stmt>) -> Vec<Stmt> {
        // Only a hierarchical name reaches a block of a subroutine from
        // outside it (23.6), and those stay unsupported.
        if self.owner.is_some()
            || !self.disabled_from_outside(&label)
            || self
                .allow_timing("disable of a named block from outside it")
                .is_err()
        {
            return body;
        }
        // The branch is a process of its own. A statement that would see
        // the difference keeps the block in its process, and a `disable`
        // from outside it unsupported: one that leaves the block other than
        // through its own loops, or ends a block around it; one that asks
        // for the current process; and `wait fork` or `disable fork`, which
        // concern the children of the process that runs them (9.6.1, 9.6.3).
        // One process variable records one activation: a block that a
        // restarted fork may run twice at once stays in its processes.
        // A repeating process is such a restart, though its tokens hold
        // no loop around the fork.
        if self.state.disables.concurrent.contains(&label)
            || (self.repeating && self.state.disables.in_fork.contains(&label))
        {
            return body;
        }
        let mut asks_for_process = false;
        let mut defined = HashSet::default();
        let mut outward = false;
        let mut children = false;
        let mut calls = Vec::new();
        for stmt in &body {
            stmt.visit(
                &mut |expr| {
                    asks_for_process |= calls_self_handle(expr);
                    const_functions::visit_expr_calls(expr, &mut |callee, _| {
                        if let Callee::Subroutine(name) = callee {
                            calls.push(name.clone());
                        }
                    });
                },
                &mut |_| {},
                &mut |_| {},
            );
            stmt.walk(&mut |stmt| match stmt {
                Stmt::Block { id, .. } => {
                    defined.insert(*id);
                }
                Stmt::WaitFork | Stmt::DisableFork => children = true,
                Stmt::Call { name, .. } => calls.push(name.clone()),
                _ => {}
            });
        }
        for stmt in &body {
            stmt.walk(&mut |stmt| {
                if let Stmt::DisableBlock(id) = stmt {
                    outward |= !defined.contains(id);
                }
            });
        }
        if leaves_block(&body, false) || asks_for_process || outward || children {
            return body;
        }
        if !self.state.disables.blocks.insert(label.clone()) {
            self.state.disables.ambiguous.insert(label);
            return body;
        }
        self.state.disables.calls.insert(label.clone(), calls);
        let (process, armed) = variables(&label);
        let assign = |name: &str, rhs: Expr| Stmt::Assign {
            lhs: LValue::Ident(name.to_string()),
            rhs,
            nonblocking: false,
        };
        let mut branch = vec![
            assign(
                &process,
                events::call(celox_design::ProcessOp::SelfHandle, Vec::new()),
            ),
            assign(&armed, Expr::Literal("1'b1".into())),
        ];
        branch.extend(body);
        branch.push(assign(&armed, Expr::Literal("1'b0".into())));
        vec![Stmt::Fork {
            join: celox_design::ForkJoin::All,
            branches: vec![branch],
        }]
    }

    /// `disable label` of a block outside the statement.
    pub(super) fn external_disable(&mut self, label: String) -> Result<Vec<Stmt>, AnalyzerError> {
        // Only a process can end another one (9.6.2); a combinational
        // procedure or a function has none to run the kill.
        if !matches!(
            self.body,
            system_functions::Body::Initial | system_functions::Body::Process
        ) {
            return Err(unsupported(
                "disable of a named block from outside it in a function or a combinational procedure",
            ));
        }
        let (process, armed) = variables(&label);
        self.state.disables.targets.push(label);
        Ok(vec![Stmt::If {
            condition: Expr::Ident(armed.clone()),
            then_body: vec![
                Stmt::Eval(events::call(
                    celox_design::ProcessOp::Kill,
                    vec![Expr::Ident(process)],
                )),
                Stmt::Assign {
                    lhs: LValue::Ident(armed),
                    rhs: Expr::Literal("1'b0".into()),
                    nonblocking: false,
                },
            ],
            else_body: Vec::new(),
        }])
    }
}

/// Whether `stmts` leave the block they are in other than through a loop
/// among them: a `return`, or a `break` or `continue` outside such a loop.
fn leaves_block(stmts: &[Stmt], in_loop: bool) -> bool {
    stmts.iter().any(|stmt| match stmt {
        Stmt::Return(_) => true,
        Stmt::Break | Stmt::Continue => !in_loop,
        Stmt::If {
            then_body,
            else_body,
            ..
        } => leaves_block(then_body, in_loop) || leaves_block(else_body, in_loop),
        Stmt::Case { items, default, .. } => {
            items.iter().any(|item| leaves_block(&item.body, in_loop))
                || default
                    .as_deref()
                    .is_some_and(|body| leaves_block(body, in_loop))
        }
        Stmt::Loop {
            init, step, body, ..
        } => leaves_block(init, in_loop) || leaves_block(step, in_loop) || leaves_block(body, true),
        Stmt::Block { body, .. } => leaves_block(body, in_loop),
        Stmt::Fork { branches, .. } => branches.iter().any(|branch| leaves_block(branch, false)),
        _ => false,
    })
}

/// The subroutines that would see a different process as a branch of a
/// `fork`: one whose body, or a subroutine it calls, asks for the current
/// process or waits for or disables the caller's forked children
/// (9.6.1, 9.6.3). Their callers cannot tell from the call alone, so a
/// block that calls one stays in its process (see
/// `BodyBuilder::external_disable_block`).
pub(in crate::ast) fn subroutines_that_need_their_process<'a>(
    subroutines: impl Iterator<Item = &'a Subroutine>,
) -> HashSet<String> {
    let subroutines: Vec<_> = subroutines.collect();
    let mut needs = HashSet::default();
    loop {
        let previous = needs.len();
        for subroutine in &subroutines {
            if needs.contains(&subroutine.name) {
                continue;
            }
            let mut direct = false;
            let mut callees = Vec::new();
            for stmt in subroutine.body.iter().chain(&subroutine.sensitivity_body) {
                stmt.visit(
                    &mut |expr| {
                        direct |= calls_self_handle(expr);
                        const_functions::visit_expr_calls(expr, &mut |callee, _| {
                            if let Callee::Subroutine(name) = callee {
                                callees.push(name.as_str());
                            }
                        });
                    },
                    &mut |_| {},
                    &mut |_| {},
                );
                stmt.walk(&mut |stmt| match stmt {
                    Stmt::WaitFork | Stmt::DisableFork => direct = true,
                    Stmt::Call { name, .. } => callees.push(name.as_str()),
                    _ => {}
                });
            }
            if direct || callees.iter().any(|name| needs.contains(*name)) {
                needs.insert(subroutine.name.clone());
            }
        }
        if needs.len() == previous {
            return needs;
        }
    }
}

/// Whether a block that runs as a branch calls a subroutine that needs
/// its caller's process, which the scope knows only once every
/// subroutine of it is lowered.
pub(in crate::ast) fn calls_subroutine_that_needs_its_process(
    disables: &ExternalDisables,
    needs: &HashSet<String>,
) -> bool {
    disables.targets.iter().any(|target| {
        disables
            .calls
            .get(target)
            .is_some_and(|calls| calls.iter().any(|name| needs.contains(name)))
    })
}

/// Whether `expr` asks for the process that evaluates it (`process::self()`).
fn calls_self_handle(expr: &Expr) -> bool {
    let mut found = false;
    let mut pending = vec![expr];
    while let Some(expr) = pending.pop() {
        match expr {
            Expr::Call { callee, args, .. } => {
                found |= matches!(
                    callee,
                    Callee::System(SystemFunction::Process(celox_design::ProcessOp::SelfHandle))
                );
                pending.extend(args.iter().flatten());
            }
            Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
                pending.push(expr)
            }
            Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => pending.extend(parts),
            Expr::Binary { left, right, .. } => pending.extend([&**left, &**right]),
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => pending.extend([&**condition, &**then_expr, &**else_expr]),
            Expr::Inside { expr, .. } => pending.push(expr),
            Expr::Ident(_) | Expr::Literal(_) => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn analyze(source: &str) -> Result<crate::ir::Ir, crate::AnalyzerError> {
        crate::analyze_source(source, Path::new("external_disable.sv"))
    }

    #[test]
    fn a_disable_inside_a_block_of_the_same_name_keeps_others_flat() {
        // The `disable worker` names the first block only; the second runs
        // in its process, as before.
        let ir = analyze(
            "module Top; initial begin : a begin : worker #1; disable worker; end end \
             initial begin : b begin : worker #1; end end endmodule",
        )
        .unwrap();
        let module = &ir.modules()[0];
        assert!(
            !module
                .signals()
                .iter()
                .any(|signal| signal.name().starts_with("disable%")),
            "no block runs as a branch"
        );
    }

    #[test]
    fn combinational_procedures_cannot_disable_blocks_of_other_processes() {
        let error = analyze(
            "module Top(input bit a); initial begin : work #5; end \
             always_comb if (a) disable work; endmodule",
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("combinational procedure"),
            "{error}"
        );
    }

    #[test]
    fn blocks_that_need_their_own_process_stay_unsupported_targets() {
        for body in [
            // A condition that asks for the process.
            "begin : work if (process::self() != null) #5; end",
            // The children of the process that runs the block.
            "begin fork #1; join_none begin : work wait fork; end end",
            // A block around it.
            "begin : outer begin : work #5; disable outer; end end",
        ] {
            let source = format!("module Top; initial {body} initial #1 disable work; endmodule");
            let error = analyze(&source).unwrap_err();
            assert!(
                error.to_string().contains("disable statement"),
                "{body}: {error}"
            );
        }
    }

    #[test]
    fn blocks_whose_subroutines_need_their_process_stay_unsupported_targets() {
        for helper in [
            // The process, asked for in a condition of the callee.
            "task helper; if (process::self() != null) #1; endtask",
            // The children of the process that runs the callee.
            "task helper; wait fork; endtask",
            "task helper; disable fork; endtask",
            // Through another subroutine.
            "task inner; if (process::self() != null) #1; endtask \
             task helper; inner(); endtask",
            // A function the block evaluates.
            "function int helper; return process::self() != null; endfunction",
        ] {
            let call = if helper.contains("function int helper") {
                "if (helper()) #5;"
            } else {
                "helper(); #5;"
            };
            let source = format!(
                "module Top; {helper} initial begin : work {call} end \
                 initial #1 disable work; endmodule"
            );
            let error = analyze(&source).unwrap_err();
            assert!(
                error.to_string().contains("disable statement"),
                "{helper}: {error}"
            );
        }
    }

    #[test]
    fn blocks_may_call_subroutines_that_do_not_need_their_process() {
        let ir = analyze(
            "module Top; task helper; #1; endtask \
             initial begin : work helper(); #5; end \
             initial #2 disable work; endmodule",
        )
        .unwrap();
        let module = &ir.modules()[0];
        assert!(
            module
                .signals()
                .iter()
                .any(|signal| signal.name().starts_with("disable%")),
            "the block runs as a branch"
        );
    }

    #[test]
    fn blocks_a_restarted_fork_may_run_twice_stay_unsupported_targets() {
        for body in [
            // A loop starts the fork again while its branch still runs.
            "initial begin for (int i = 0; i < 2; i++) \
             fork begin : work #5; end join_none end",
            // A repeating process starts the fork again.
            "always begin fork begin : work #5; end join_none #1; end",
        ] {
            let source = format!("module Top; {body} initial #1 disable work; endmodule");
            let error = analyze(&source).unwrap_err();
            assert!(
                error.to_string().contains("disable statement"),
                "{body}: {error}"
            );
        }
    }

    #[test]
    fn a_single_forked_activation_still_runs_as_a_branch() {
        let ir = analyze(
            "module Top; initial fork begin : work #5; end join_none \
             initial #1 disable work; endmodule",
        )
        .unwrap();
        let module = &ir.modules()[0];
        assert!(
            module
                .signals()
                .iter()
                .any(|signal| signal.name().starts_with("disable%")),
            "the block runs as a branch"
        );
    }

    #[test]
    fn blocks_that_ask_for_their_process_stay_unsupported_targets() {
        let error = analyze(
            "module Top; process p; initial begin : work p = process::self(); #5; end \
             initial #1 disable work; endmodule",
        )
        .unwrap_err();
        assert!(error.to_string().contains("disable statement"), "{error}");
    }
}
