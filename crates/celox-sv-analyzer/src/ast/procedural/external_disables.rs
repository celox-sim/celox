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
            let mut target_asks = false;
            let mut target_calls = Vec::new();
            stmt.visit(
                &mut |expr| expr_effects(expr, &mut asks_for_process, &mut calls),
                &mut |lvalue| lvalue_effects(lvalue, &mut target_asks, &mut target_calls),
                &mut |_| {},
            );
            asks_for_process |= target_asks;
            calls.extend(target_calls);
            stmt.walk(&mut |stmt| match stmt {
                Stmt::Block { id, .. } => {
                    defined.insert(*id);
                }
                Stmt::WaitFork | Stmt::DisableFork => children = true,
                // A fork its children outlive leaves them with the
                // branch instead of the process that runs the block.
                Stmt::Fork { join, .. } if *join != celox_design::ForkJoin::All => children = true,
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
                events::call(celox_design::ProcessOp::SelfHandleInternal, Vec::new()),
            ),
            assign(&armed, Expr::Literal("1'b1".into())),
        ];
        // The branches of blocks inside this one: killing this branch
        // cancels theirs without their disarming statements, so the
        // process that continues after the block disarms them itself
        // before another process can name one of them.
        let mut nested = Vec::new();
        for stmt in &body {
            stmt.walk(&mut |stmt| {
                if let Some(inner) = branch_label(stmt, &self.state.disables)
                    && inner != label
                    && !nested.contains(&inner)
                {
                    nested.push(inner);
                }
            });
        }
        branch.extend(body);
        branch.push(assign(&armed, Expr::Literal("1'b0".into())));
        // Release the handle, so the heap collects it and its activation
        // may be reused: it names the branch only while it runs.
        branch.push(assign(&process, Expr::Literal("32'b0".into())));
        let mut output = vec![Stmt::Fork {
            join: celox_design::ForkJoin::All,
            branches: vec![branch],
        }];
        for inner in nested {
            let (process, armed) = variables(&inner);
            output.push(assign(&armed, Expr::Literal("1'b0".into())));
            output.push(assign(&process, Expr::Literal("32'b0".into())));
        }
        output
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
                    vec![Expr::Ident(process.clone())],
                )),
                Stmt::Assign {
                    lhs: LValue::Ident(armed),
                    rhs: Expr::Literal("1'b0".into()),
                    nonblocking: false,
                },
                Stmt::Assign {
                    lhs: LValue::Ident(process),
                    rhs: Expr::Literal("32'b0".into()),
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

/// What an expression does to the process that evaluates it: whether it
/// asks for that process, and the subroutines it calls.
fn expr_effects(expr: &Expr, asks: &mut bool, calls: &mut Vec<String>) {
    *asks |= calls_self_handle(expr);
    const_functions::visit_expr_calls(expr, &mut |callee, _| {
        if let Callee::Subroutine(name) = callee {
            calls.push(name.clone());
        }
    });
}

/// The same for a constant expression, which holds the calls of an
/// assignment target's runtime selectors.
fn const_effects(expr: &ConstExpr, asks: &mut bool, calls: &mut Vec<String>) {
    const_functions::visit_const_calls(expr, &mut |callee, _| match callee {
        Callee::System(SystemFunction::Process(celox_design::ProcessOp::SelfHandle)) => {
            *asks = true
        }
        Callee::Subroutine(name) => calls.push(name.clone()),
        _ => {}
    });
}

/// Whether a call draws on or observes the calling process's random
/// stream (18.13, 20.14, 9.7): a branch of a synthetic fork has a stream
/// of its own, seeded from its parent, so such a call in the branch or
/// in the process around it does not continue the original stream.
fn uses_random(callee: &Callee) -> bool {
    match callee {
        Callee::System(SystemFunction::Process(op)) => matches!(
            op,
            celox_design::ProcessOp::Srandom
                | celox_design::ProcessOp::GetRandstate
                | celox_design::ProcessOp::SetRandstate
        ),
        Callee::System(function) => matches!(
            function,
            SystemFunction::Random
                | SystemFunction::DistUniform
                | SystemFunction::DistNormal
                | SystemFunction::DistExponential
                | SystemFunction::DistPoisson
                | SystemFunction::DistChiSquare
                | SystemFunction::DistT
                | SystemFunction::DistErlang
                | SystemFunction::Urandom
                | SystemFunction::UrandomRange
                | SystemFunction::ScopeRandomize
                | SystemFunction::ScopeRandomizeEnum
        ),
        _ => false,
    }
}

/// Whether an expression or an assignment target uses the process's
/// random stream, through any of its calls.
fn expr_randomizes(expr: &Expr) -> bool {
    let mut found = false;
    const_functions::visit_expr_calls(expr, &mut |callee, _| found |= uses_random(callee));
    found
}

fn const_randomizes(expr: &ConstExpr) -> bool {
    let mut found = false;
    const_functions::visit_const_calls(expr, &mut |callee, _| found |= uses_random(callee));
    found
}

fn lvalue_randomizes(lvalue: &LValue) -> bool {
    match lvalue {
        LValue::Ident(_) => false,
        LValue::Select {
            msb,
            lsb,
            array_slice_width,
            ..
        } => {
            const_randomizes(msb)
                || const_randomizes(lsb)
                || array_slice_width.as_ref().is_some_and(const_randomizes)
        }
        LValue::Object {
            place,
            select,
            guards,
            ..
        } => {
            guards.iter().any(expr_randomizes)
                || place_randomizes(place)
                || lvalue_randomizes(select)
        }
    }
}

fn place_randomizes(place: &ObjectPlace) -> bool {
    match place {
        ObjectPlace::Property { handle, .. } => expr_randomizes(handle),
        ObjectPlace::Element {
            container, index, ..
        } => expr_randomizes(container) || expr_randomizes(index),
        ObjectPlace::Member { outer, .. } => place_randomizes(outer),
    }
}

/// Whether some process that runs a block as a branch uses its random
/// stream, in the branch or around it, directly or through a subroutine
/// it calls: the branch would continue a different stream (9.6.2).
pub(in crate::ast) fn random_state_may_change<'a>(
    disables: &ExternalDisables,
    bodies: impl Iterator<Item = &'a [Stmt]>,
    randomizes: &HashSet<String>,
) -> bool {
    bodies.into_iter().any(|body| {
        let mut present = false;
        let mut random = false;
        for stmt in body {
            stmt.walk(&mut |stmt| {
                present |= branch_label(stmt, disables)
                    .is_some_and(|label| disables.targets.contains(&label));
            });
            let mut named = Vec::new();
            let mut called = Vec::new();
            let mut target_random = false;
            stmt.visit(
                &mut |expr| {
                    random |= expr_randomizes(expr);
                    const_functions::visit_expr_calls(expr, &mut |callee, _| {
                        if let Callee::Subroutine(name) = callee {
                            named.push(name.clone());
                        }
                    });
                },
                &mut |lvalue| target_random |= lvalue_randomizes(lvalue),
                &mut |name| called.push(name.to_string()),
            );
            named.extend(called);
            random |= target_random || named.iter().any(|name| randomizes.contains(name));
        }
        present && random
    })
}

/// The calls in an assignment target: its selectors and, for a class or
/// container place, the expressions that name the place. `Stmt::visit`
/// passes a target to its lvalue callback, not its expression one.
fn lvalue_effects(lvalue: &LValue, asks: &mut bool, calls: &mut Vec<String>) {
    match lvalue {
        LValue::Ident(_) => {}
        LValue::Select {
            msb,
            lsb,
            array_slice_width,
            ..
        } => {
            const_effects(msb, asks, calls);
            const_effects(lsb, asks, calls);
            if let Some(width) = array_slice_width {
                const_effects(width, asks, calls);
            }
        }
        LValue::Object {
            place,
            select,
            guards,
            ..
        } => {
            for guard in guards {
                expr_effects(guard, asks, calls);
            }
            place_effects(place, asks, calls);
            lvalue_effects(select, asks, calls);
        }
    }
}

/// The expressions that name a class or container place.
fn place_effects(place: &ObjectPlace, asks: &mut bool, calls: &mut Vec<String>) {
    match place {
        ObjectPlace::Property { handle, .. } => expr_effects(handle, asks, calls),
        ObjectPlace::Element {
            container, index, ..
        } => {
            expr_effects(container, asks, calls);
            expr_effects(index, asks, calls);
        }
        ObjectPlace::Member { outer, .. } => place_effects(outer, asks, calls),
    }
}

/// The subroutines that would see a different process as a branch of a
/// `fork`, and those that leave detached children with the caller: one
/// whose body, or a subroutine it calls, asks for the current process,
/// waits for or disables the caller's forked children, or starts a
/// `fork` its children outlive (9.6.1, 9.6.2, 9.6.3). Their callers
/// cannot tell from the call alone, so a block that calls one stays in
/// its process (see `BodyBuilder::external_disable_block`).
pub(in crate::ast) fn subroutine_process_effects<'a>(
    subroutines: impl Iterator<Item = &'a Subroutine>,
) -> (HashSet<String>, HashSet<String>, HashSet<String>) {
    let mut scanned = Vec::new();
    for subroutine in subroutines {
        let mut needs = false;
        let mut spawns = false;
        let mut randomizes = false;
        let mut callees = Vec::new();
        for stmt in subroutine.body.iter().chain(&subroutine.sensitivity_body) {
            let mut target_asks = false;
            let mut target_calls = Vec::new();
            let mut target_randomizes = false;
            stmt.visit(
                &mut |expr| {
                    expr_effects(expr, &mut needs, &mut callees);
                    randomizes |= expr_randomizes(expr);
                },
                &mut |lvalue| {
                    lvalue_effects(lvalue, &mut target_asks, &mut target_calls);
                    target_randomizes |= lvalue_randomizes(lvalue);
                },
                &mut |_| {},
            );
            needs |= target_asks;
            randomizes |= target_randomizes;
            callees.extend(target_calls);
            stmt.walk(&mut |stmt| match stmt {
                Stmt::WaitFork | Stmt::DisableFork => needs = true,
                Stmt::Fork { join, .. } if *join != celox_design::ForkJoin::All => {
                    needs = true;
                    spawns = true;
                }
                Stmt::Call { name, .. } => callees.push(name.clone()),
                _ => {}
            });
        }
        scanned.push((subroutine.name.clone(), needs, spawns, randomizes, callees));
    }
    let (mut needs, mut spawns, mut randomizes) =
        (HashSet::default(), HashSet::default(), HashSet::default());
    loop {
        let previous = (needs.len(), spawns.len(), randomizes.len());
        for (name, direct_needs, direct_spawns, direct_randomizes, callees) in &scanned {
            if (*direct_needs || callees.iter().any(|callee| needs.contains(callee)))
                && !needs.contains(name)
            {
                needs.insert(name.clone());
            }
            if (*direct_spawns || callees.iter().any(|callee| spawns.contains(callee)))
                && !spawns.contains(name)
            {
                spawns.insert(name.clone());
            }
            if (*direct_randomizes || callees.iter().any(|callee| randomizes.contains(callee)))
                && !randomizes.contains(name)
            {
                randomizes.insert(name.clone());
            }
        }
        if (needs.len(), spawns.len(), randomizes.len()) == previous {
            return (needs, spawns, randomizes);
        }
    }
}

/// Whether some process that runs a block as a branch may hold forked
/// children the branch would start early: the synthetic `fork`-`join`
/// suspends its process, which starts the children pending since its
/// last suspension, where the block itself suspends nowhere (9.6.2).
/// A detached fork of the process, or a call of a subroutine that
/// starts one, outside the branch counts; the fork the branch sits in
/// does not, as its process holds no children when it starts.
pub(in crate::ast) fn pending_children_may_precede<'a>(
    disables: &ExternalDisables,
    bodies: impl Iterator<Item = &'a [Stmt]>,
    spawns: &HashSet<String>,
) -> bool {
    bodies.into_iter().any(|body| {
        let mut found = false;
        scan_pending(body, disables, spawns, &mut found);
        found
    })
}

/// Scan one process for a branch it runs that its detached children may
/// precede, and record it in `found`.
fn scan_pending(
    stmts: &[Stmt],
    disables: &ExternalDisables,
    spawns: &HashSet<String>,
    found: &mut bool,
) {
    let mut present = HashSet::default();
    let mut pending = false;
    scan_context(stmts, disables, spawns, &mut present, &mut pending, found);
    *found |= pending && present.iter().any(|label| disables.targets.contains(label));
}

/// Scan `stmts` of one process for the branches of `disables` and for
/// detached children it may hold, without entering a branch itself. A
/// fork's branches are processes of their own, which hold no children
/// when they start; the fork itself leaves its children with this
/// process when it does not join them all.
fn scan_context(
    stmts: &[Stmt],
    disables: &ExternalDisables,
    spawns: &HashSet<String>,
    present: &mut HashSet<String>,
    pending: &mut bool,
    found: &mut bool,
) {
    for stmt in stmts {
        if let Some(label) = branch_label(stmt, disables) {
            present.insert(label);
            continue;
        }
        match stmt {
            Stmt::Fork { join, branches } => {
                for branch in branches {
                    scan_pending(branch, disables, spawns, found);
                }
                *pending |= *join != celox_design::ForkJoin::All;
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                *pending |= expr_spawns(condition, spawns);
                scan_context(then_body, disables, spawns, present, pending, found);
                scan_context(else_body, disables, spawns, present, pending, found);
            }
            Stmt::Case {
                selector,
                items,
                default,
                ..
            } => {
                *pending |= expr_spawns(selector, spawns);
                for item in items {
                    for label in &item.labels {
                        match label {
                            crate::procedural::CaseLabel::Value(value) => {
                                *pending |= expr_spawns(value, spawns)
                            }
                            crate::procedural::CaseLabel::Range { low, high } => {
                                *pending |= expr_spawns(low, spawns) || expr_spawns(high, spawns)
                            }
                        }
                    }
                    scan_context(&item.body, disables, spawns, present, pending, found);
                }
                if let Some(default) = default {
                    scan_context(default, disables, spawns, present, pending, found);
                }
            }
            Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => {
                if let crate::procedural::LoopKind::Repeat(count) = kind {
                    *pending |= expr_spawns(count, spawns);
                }
                if let Some(condition) = condition {
                    *pending |= expr_spawns(condition, spawns);
                }
                scan_context(init, disables, spawns, present, pending, found);
                scan_context(step, disables, spawns, present, pending, found);
                scan_context(body, disables, spawns, present, pending, found);
            }
            Stmt::Block { body, .. } => {
                scan_context(body, disables, spawns, present, pending, found)
            }
            Stmt::DelayedNonblocking {
                control,
                assignment,
            } => {
                scan_context(
                    std::slice::from_ref(&**control),
                    disables,
                    spawns,
                    present,
                    pending,
                    found,
                );
                scan_context(
                    std::slice::from_ref(&**assignment),
                    disables,
                    spawns,
                    present,
                    pending,
                    found,
                );
            }
            _ => {
                let mut calls = Vec::new();
                let mut named = Vec::new();
                let mut asks = false;
                stmt.visit(
                    &mut |expr| expr_effects(expr, &mut asks, &mut calls),
                    &mut |_| {},
                    &mut |name| named.push(name.to_string()),
                );
                calls.extend(named);
                *pending |= calls.iter().any(|name| spawns.contains(name));
            }
        }
    }
}

/// Take the branch off every block no elaborated `disable` names from
/// outside it: token discovery also sees `disable` statements of
/// unselected generate branches, which elaborate to nothing, and a
/// wrapper without a target only adds a suspension (9.6.2). The
/// branch's first two statements record its process, so the rest of
/// the branch is the block again.
pub(in crate::ast) fn unwrap_without_elaborated_target<'a>(
    disables: &mut ExternalDisables,
    bodies: impl Iterator<Item = &'a mut Vec<Stmt>>,
) {
    let targets: HashSet<&str> = disables.targets.iter().map(String::as_str).collect();
    let unwanted: HashSet<String> = disables
        .blocks
        .iter()
        .filter(|label| !targets.contains(label.as_str()))
        .cloned()
        .collect();
    if unwanted.is_empty() {
        return;
    }
    for body in bodies {
        *body = unwrap_stmts(std::mem::take(body), &unwanted, disables);
    }
    disables.blocks.retain(|label| !unwanted.contains(label));
    disables.calls.retain(|label, _| !unwanted.contains(label));
}

/// `stmts` with the branches of the blocks in `unwanted` replaced by
/// their bodies.
fn unwrap_stmts(
    stmts: Vec<Stmt>,
    unwanted: &HashSet<String>,
    disables: &ExternalDisables,
) -> Vec<Stmt> {
    // The variables of an unwrapped block, which no statement may keep
    // assigning: the branch ends with one that disarms it.
    let compiler: HashSet<String> = unwanted
        .iter()
        .flat_map(|label| {
            let (process, armed) = variables(label);
            [process, armed]
        })
        .collect();
    let mut output = Vec::new();
    for stmt in stmts {
        if let Stmt::Assign {
            lhs: LValue::Ident(name),
            ..
        } = &stmt
            && compiler.contains(name)
        {
            continue;
        }
        if let Some(label) = branch_label(&stmt, disables)
            && unwanted.contains(&label)
        {
            let Stmt::Fork { branches, .. } = stmt else {
                unreachable!("a branch is a fork");
            };
            let mut branch = branches.into_iter().next().unwrap_or_default();
            let body = if branch.len() > 2 {
                branch.split_off(2)
            } else {
                Vec::new()
            };
            output.extend(unwrap_stmts(body, unwanted, disables));
            continue;
        }
        output.push(match stmt {
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => Stmt::If {
                condition,
                then_body: unwrap_stmts(then_body, unwanted, disables),
                else_body: unwrap_stmts(else_body, unwanted, disables),
            },
            Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => Stmt::Case {
                kind,
                selector,
                items: items
                    .into_iter()
                    .map(|item| crate::procedural::CaseItemBase {
                        labels: item.labels,
                        body: unwrap_stmts(item.body, unwanted, disables),
                    })
                    .collect(),
                default: default.map(|body| unwrap_stmts(body, unwanted, disables)),
            },
            Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => Stmt::Loop {
                kind,
                init: unwrap_stmts(init, unwanted, disables),
                condition,
                step: unwrap_stmts(step, unwanted, disables),
                body: unwrap_stmts(body, unwanted, disables),
            },
            Stmt::Fork { join, branches } => Stmt::Fork {
                join,
                branches: branches
                    .into_iter()
                    .map(|branch| unwrap_stmts(branch, unwanted, disables))
                    .collect(),
            },
            Stmt::Block { id, body } => Stmt::Block {
                id,
                body: unwrap_stmts(body, unwanted, disables),
            },
            Stmt::DelayedNonblocking {
                control,
                assignment,
            } => Stmt::DelayedNonblocking {
                control: Box::new(
                    unwrap_stmts(vec![*control], unwanted, disables)
                        .into_iter()
                        .next()
                        .unwrap(),
                ),
                assignment: Box::new(
                    unwrap_stmts(vec![*assignment], unwanted, disables)
                        .into_iter()
                        .next()
                        .unwrap(),
                ),
            },
            stmt => stmt,
        });
    }
    output
}

/// Whether `expr` calls a subroutine in `spawns`.
fn expr_spawns(expr: &Expr, spawns: &HashSet<String>) -> bool {
    let mut calls = Vec::new();
    let mut asks = false;
    expr_effects(expr, &mut asks, &mut calls);
    calls.iter().any(|name| spawns.contains(name))
}

/// The label whose branch `stmt` is, if it is one: the `fork`-`join`
/// that records the process of a block a `disable` ends from outside.
fn branch_label(stmt: &Stmt, disables: &ExternalDisables) -> Option<String> {
    let Stmt::Fork {
        join: celox_design::ForkJoin::All,
        branches,
    } = stmt
    else {
        return None;
    };
    let [branch] = branches.as_slice() else {
        return None;
    };
    let Some(Stmt::Assign {
        lhs: LValue::Ident(name),
        ..
    }) = branch.first()
    else {
        return None;
    };
    disables
        .blocks
        .iter()
        .find(|label| variables(label).0 == *name)
        .cloned()
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

/// Whether `expr` asks for the process that evaluates it
/// (`process::self()`), wherever in its subtrees it occurs: in a bit or
/// part selector, an `inside` set item, a concatenation count and the
/// like, which the call visitor already walks.
fn calls_self_handle(expr: &Expr) -> bool {
    let mut found = false;
    const_functions::visit_expr_calls(expr, &mut |callee, _| {
        found |= matches!(
            callee,
            Callee::System(SystemFunction::Process(celox_design::ProcessOp::SelfHandle))
        );
    });
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
            // Children the callee leaves with the calling process.
            "task helper; fork begin #3; end join_none endtask",
            "task helper; fork begin #3; end join_any endtask",
            // Through another subroutine.
            "task inner; fork begin #3; end join_none endtask \
             task helper; inner(); endtask",
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
    fn calls_in_assignment_target_selectors_keep_blocks_in_their_process() {
        for source in [
            // A selector of the block's own assignment.
            "module Top; bit [3:0] bits; \
             function int idx; return process::self() != null; endfunction \
             initial begin : work bits[idx()] = 1'b1; #5; end \
             initial #1 disable work; endmodule",
            // A selector in an assignment of a subroutine the block calls.
            "module Top; bit [3:0] bits; \
             function int idx; return process::self() != null; endfunction \
             task helper; bits[idx()] = 1'b1; endtask \
             initial begin : work helper(); #5; end \
             initial #1 disable work; endmodule",
            // The process itself in a selector.
            "module Top; bit [3:0] bits; \
             initial begin : work bits[process::self() != null] = 1'b1; #5; end \
             initial #1 disable work; endmodule",
        ] {
            let error = analyze(source).unwrap_err();
            assert!(
                error.to_string().contains("disable statement"),
                "{source}: {error}"
            );
        }
    }

    #[test]
    fn pending_children_keep_blocks_in_their_process() {
        for body in [
            // A detached child the process started before the block.
            "initial begin fork begin #3; end join_none \
             begin : work #5; end wait fork; end",
            // Through a subroutine the process called before the block.
            "task spawn; fork begin #3; end join_none endtask \
             initial begin spawn(); begin : work #5; end end",
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
    fn blocks_with_detached_forks_or_self_queries_stay_unsupported_targets() {
        for source in [
            // A fork its children outlive, inside the block itself.
            "module Top; bit done; initial begin : work \
             fork #5 done = 1; join_none #1; end \
             initial #10 disable work; endmodule",
            "module Top; bit done; initial begin : work \
             fork #5 done = 1; join_any #1; end \
             initial #10 disable work; endmodule",
            // The process, asked for in a bit selector of the block.
            "module Top; process p; bit [3:0] bits; int q; \
             initial begin p = process::self(); \
             begin : work q = bits[process::self() == p]; #5; end end \
             initial #1 disable work; endmodule",
            // The process, in an `inside` set item of the block.
            "module Top; process p; bit q; \
             initial begin p = process::self(); \
             begin : work q = p inside {process::self()}; #5; end end \
             initial #1 disable work; endmodule",
        ] {
            let error = analyze(source).unwrap_err();
            assert!(
                error.to_string().contains("disable statement"),
                "{source}: {error}"
            );
        }
    }

    #[test]
    fn a_disable_in_an_unselected_generate_branch_wraps_nothing() {
        let ir = analyze(
            "module Top; initial begin : work #5; end \
             if (0) begin initial disable work; end endmodule",
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
        assert!(
            !format!("{:?}", ir.modules()[0]).contains("disable%"),
            "no statement keeps the branch's variables"
        );
    }

    #[test]
    fn random_state_use_keeps_blocks_in_their_process() {
        for source in [
            // The process around the block observes its random state.
            "module Top; process p; string s; \
             initial begin p = process::self(); s = p.get_randstate(); \
             begin : work #5; end end \
             initial #1 disable work; endmodule",
            // The block itself draws on the process's stream.
            "module Top; int q; initial begin : work q = $urandom; #5; end \
             initial #1 disable work; endmodule",
            // Through a subroutine the process calls around the block.
            "module Top; int q; task draw; q = $urandom; endtask \
             initial begin draw(); begin : work #5; end end \
             initial #1 disable work; endmodule",
        ] {
            let error = analyze(source).unwrap_err();
            assert!(
                error.to_string().contains("disable statement"),
                "{source}: {error}"
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
