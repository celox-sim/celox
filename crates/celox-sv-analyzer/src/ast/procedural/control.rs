//! Native statement-identity driven control, declarations and value lowering.
use super::*;
use celox_sv_syntax::{LoopSyntax, StatementId, StatementKind};

fn disagreement() -> AnalyzerError {
    AnalyzerError::Parse("native body control disagrees with validated source_tree coverage".into())
}
struct IfState {
    id: StatementId,
    index: usize,
    branches: Vec<(Expr, Vec<Stmt>)>,
}
struct CaseState {
    id: StatementId,
    index: usize,
    kind: CaseKind,
    selector: Expr,
    /// Whether the statement compares chandles (6.14).
    chandles: bool,
    items: Vec<CaseItemBase<Expr, LValue>>,
    default: Option<Vec<Stmt>>,
}
struct RandcaseState {
    id: StatementId,
    index: usize,
    /// The hidden variables holding the evaluated weights, and the width
    /// they and the pick share (18.16).
    weights: Vec<String>,
    width: usize,
    items: Vec<Vec<Stmt>>,
}
enum Job {
    Enter(StatementId),
    EndScope,
    /// Wrap the statements built since `start` as the named block `id`
    /// when a `disable` in them named it; they stay flattened otherwise.
    BlockEnd {
        id: u64,
        start: usize,
    },
    ForkNext {
        items: Vec<StatementId>,
        index: usize,
        join: celox_design::ForkJoin,
        branches: Vec<Vec<Stmt>>,
        /// Per item, the process-handle and armed variables of a named
        /// branch block a sibling `disable` can kill (9.6.2).
        handles: Vec<Option<(String, String)>>,
    },
    ForkBranch {
        items: Vec<StatementId>,
        index: usize,
        join: celox_design::ForkJoin,
        branches: Vec<Vec<Stmt>>,
        handles: Vec<Option<(String, String)>>,
        start: usize,
        sensitivity: usize,
        parent_body: system_functions::Body,
    },
    IfNext(IfState),
    IfBranch {
        state: IfState,
        condition: Expr,
        start: usize,
    },
    IfFinish {
        branches: Vec<(Expr, Vec<Stmt>)>,
        start: usize,
    },
    CaseNext(CaseState),
    CaseBranch {
        state: CaseState,
        labels: Option<Vec<CaseLabel<Expr>>>,
        start: usize,
    },
    RandcaseNext(RandcaseState),
    RandcaseBranch {
        state: RandcaseState,
        start: usize,
    },
    Loop {
        kind: LoopKind<Expr>,
        init: Vec<Stmt>,
        condition: Option<Expr>,
        /// Statements that start each iteration (see
        /// [`BodyBuilder::loop_condition`]).
        prologue: Vec<Stmt>,
        step: Vec<Stmt>,
        scope: bool,
        start: usize,
    },
    DoWhile {
        native: celox_sv_syntax::ExprId,
        start: usize,
    },
    Foreach {
        loops: Vec<ForeachLoop>,
        start: usize,
    },
    /// The end of the statement an `@*` controls, which starts at `start`
    /// of the output; `sensitivity` is where its constant-folded branches
    /// start in the builder's sensitivity body.
    ImplicitEvent {
        start: usize,
        sensitivity: usize,
    },
    AssertionThen {
        concurrent: Option<Option<Expr>>,
        condition: Expr,
        stages: Option<assertions::SequenceMonitor>,
        expect: Option<ExpectAttempt>,
        keyword: String,
        assertion: celox_design::AssertionSite,
        span: Option<crate::procedural::SourceSpan>,
        failure: Option<StatementId>,
        start: usize,
    },
    AssertionFinish {
        concurrent: Option<Option<Expr>>,
        condition: Expr,
        stages: Option<assertions::SequenceMonitor>,
        expect: Option<ExpectAttempt>,
        then_body: Vec<Stmt>,
        assertion: celox_design::AssertionSite,
        default_failure: Option<(String, Option<crate::procedural::SourceSpan>)>,
        start: usize,
    },
}
/// The one evaluation attempt of an `expect` statement (IEEE 1800-2023
/// 16.17), which the executing process makes itself: from the next tick of
/// `clock`, each step at its offset in ticks, until a step misses or the
/// last one matches.
struct ExpectAttempt {
    clock: Stmt,
    /// The steps by offset: whether each matches at its tick, sampled, and
    /// whether it is an antecedent step, whose miss succeeds vacuously.
    steps: Vec<(u32, Expr, bool)>,
}

impl BodyBuilder<'_, '_, '_> {
    pub(in crate::ast) fn native_body(
        &mut self,
        ids: &[StatementId],
        authored: &[StatementId],
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let mut members = HashSet::default();
        for id in authored {
            if !members.insert(*id) {
                return Err(disagreement());
            }
        }
        let mut pending = ids.to_vec();
        let mut seen = HashSet::default();
        while let Some(id) = pending.pop() {
            if !members.contains(&id) || !seen.insert(id) {
                return Err(disagreement());
            }
            let kind = &self.tree.declarations().statement(id).kind;
            if !matches!(kind, StatementKind::Unconverted { .. }) {
                pending.extend(kind.children());
            }
        }
        let depth = self.names.scopes.len();
        let result = self
            .native_body_machine(ids)
            .and_then(|stmts| reject_escaped_places(&stmts).map(|()| stmts));
        // Restore lexical bindings on every diagnostic path, just as the old
        // recursive block/for wrappers did; no native failure is retried.
        while self.names.scopes.len() > depth {
            self.pop_scope();
        }
        result
    }
    fn native_body_machine(&mut self, ids: &[StatementId]) -> Result<Vec<Stmt>, AnalyzerError> {
        let native = self.tree.declarations();
        let mut jobs = Vec::new();
        jobs.extend(ids.iter().rev().map(|id| Job::Enter(*id)));
        // Authored block sequences append directly. Only semantic control
        // branches split off a body, so nested blocks/timing prefixes never
        // repeatedly copy a growing flattened statement vector.
        let mut output = Vec::new();
        while let Some(job) = jobs.pop() {
            match job {
                Job::ForkNext {
                    items,
                    index,
                    join,
                    mut branches,
                    handles,
                } => {
                    if let Some(&id) = items.get(index) {
                        if let StatementKind::Declaration(declaration) = &native.statement(id).kind
                        {
                            output.extend(self.block_item_declaration(declaration)?);
                            jobs.push(Job::ForkNext {
                                items,
                                index: index + 1,
                                join,
                                branches,
                                handles,
                            });
                        } else {
                            jobs.push(Job::ForkBranch {
                                items,
                                index,
                                join,
                                branches,
                                handles,
                                start: output.len(),
                                sensitivity: self.sensitivity_body.len(),
                                parent_body: self.body,
                            });
                            // Detached children obey task rules, independently
                            // of the function/final procedure spawning them.
                            if join == celox_design::ForkJoin::None {
                                self.body = system_functions::Body::Initial;
                            }
                            jobs.push(Job::Enter(id));
                        }
                    } else {
                        // The branches' names leave scope with the fork.
                        let registered = handles.iter().flatten().count();
                        let named = self
                            .fork_blocks
                            .split_off(self.fork_blocks.len() - registered);
                        // A branch a sibling `disable` names registers its
                        // process as its first act (9.6.2). Other named
                        // branches take no handle: an exposed process keeps
                        // its record for as long as the simulation runs.
                        let mut branch = 0;
                        let mut named = named.into_iter();
                        for (item, handle) in items.iter().zip(&handles) {
                            if matches!(native.statement(*item).kind, StatementKind::Declaration(_))
                            {
                                continue;
                            }
                            if handle.is_some()
                                && let Some((_, handle, armed, true)) = named.next()
                            {
                                let assign = |name: String, rhs: Expr| Stmt::Assign {
                                    lhs: LValue::Ident(name),
                                    rhs,
                                    nonblocking: false,
                                };
                                branches[branch].splice(
                                    0..0,
                                    [
                                        assign(
                                            handle,
                                            events::call(
                                                celox_design::ProcessOp::SelfHandle,
                                                Vec::new(),
                                            ),
                                        ),
                                        assign(armed, Expr::Literal("1'b1".into())),
                                    ],
                                );
                            }
                            branch += 1;
                        }
                        self.pop_scope();
                        output.push(Stmt::Fork { join, branches });
                    }
                }
                Job::ForkBranch {
                    items,
                    index,
                    join,
                    mut branches,
                    handles,
                    start,
                    sensitivity,
                    parent_body,
                } => {
                    self.body = parent_body;
                    let branch = output.split_off(start);
                    // Folded-away statements still carry syntactic effects,
                    // but belong to the child, not its function/final parent.
                    // Preserve that boundary just as for executable branches.
                    let discarded = self.sensitivity_body.split_off(sensitivity);
                    if !discarded.is_empty() {
                        self.sensitivity_body.push(Stmt::Fork {
                            join,
                            branches: vec![discarded],
                        });
                    }
                    let mut returns = false;
                    for stmt in &branch {
                        stmt.walk(&mut |s| returns |= matches!(s, Stmt::Return(_)));
                    }
                    if returns {
                        return Err(unsupported("return from a fork-join block"));
                    }
                    branches.push(branch);
                    jobs.push(Job::ForkNext {
                        items,
                        index: index + 1,
                        join,
                        branches,
                        handles,
                    });
                }
                Job::Enter(id) => match &native.statement(id).kind {
                    StatementKind::Empty => {}
                    StatementKind::Declaration(declaration) => {
                        output.extend(self.block_item_declaration(declaration)?)
                    }
                    StatementKind::Parallel { items, join, .. } => {
                        if join != "join_none"
                            || matches!(self.body, system_functions::Body::Always)
                        {
                            self.allow_timing("fork-join block")?;
                        }
                        self.push_scope();
                        let join = match join.as_str() {
                            "join" => celox_design::ForkJoin::All,
                            "join_any" => celox_design::ForkJoin::Any,
                            "join_none" => celox_design::ForkJoin::None,
                            _ => return Err(disagreement()),
                        };
                        // A branch that is a named block can be disabled
                        // by name from a sibling branch (9.6.2): register
                        // a process handle for each such branch.
                        let mut handles = Vec::new();
                        for item in items.iter() {
                            let named = match &native.statement(*item).kind {
                                StatementKind::Block {
                                    label: Some(label), ..
                                } => Some(instances::normalize_identifier(&label.spelling)),
                                _ => None,
                            };
                            handles.push(named.map(|label| {
                                let handle = self.hidden(classes::handle(processes::CLASS)).1;
                                let armed = self.hidden(unsigned_type(1)).1;
                                self.fork_blocks.push((
                                    label,
                                    handle.clone(),
                                    armed.clone(),
                                    false,
                                ));
                                (handle, armed)
                            }));
                        }
                        jobs.push(Job::ForkNext {
                            items: items.clone(),
                            index: 0,
                            join,
                            branches: Vec::new(),
                            handles,
                        });
                    }
                    StatementKind::Block { label, items, .. } => {
                        self.push_scope();
                        // A named block is a candidate to stay a block in
                        // the IR when a `disable` in its body can leave
                        // it: no fork in the body, whose children a
                        // disable would also have to kill. Others flatten
                        // as before, and a `disable` naming them stays
                        // unsupported. A candidate no `disable` names
                        // flattens too (see `Job::BlockEnd`).
                        let named = label.as_ref().filter(|_| {
                            !native
                                .tokens
                                .get(
                                    native.statement(id).tokens.start
                                        ..native.statement(id).tokens.end,
                                )
                                .into_iter()
                                .flatten()
                                .any(|token| !token.is_trivia() && token.text == "fork")
                        });
                        if let Some(label) = named {
                            let block_id = self.block_id(native.statement(id).tokens.start);
                            self.named_blocks.push((
                                instances::normalize_identifier(&label.spelling),
                                block_id,
                                false,
                            ));
                            jobs.push(Job::BlockEnd {
                                id: block_id,
                                start: output.len(),
                            });
                        }
                        jobs.push(Job::EndScope);
                        jobs.extend(items.iter().rev().map(|id| Job::Enter(*id)));
                    }
                    StatementKind::Conditional { .. } => jobs.push(Job::IfNext(IfState {
                        id,
                        index: 0,
                        branches: Vec::new(),
                    })),
                    StatementKind::Loop { kind, body } => {
                        let mut prologue = Vec::new();
                        let (kind, condition, scope) = match kind {
                            LoopSyntax::While(value) => match self.loop_condition(*value)? {
                                Some(stmts) => {
                                    prologue = stmts;
                                    (LoopKind::For, None, false)
                                }
                                None => (
                                    LoopKind::While,
                                    Some(self.native_chandle_value(*value)?),
                                    false,
                                ),
                            },
                            LoopSyntax::Repeat(value) => {
                                // The count is evaluated once (12.7.2).
                                output.extend(self.effects(&[*value])?);
                                (LoopKind::Repeat(self.native_value(*value)?), None, false)
                            }
                            LoopSyntax::Forever => (LoopKind::Forever, None, false),
                            LoopSyntax::DoWhile(value) => {
                                jobs.push(Job::DoWhile {
                                    native: *value,
                                    start: output.len(),
                                });
                                jobs.push(Job::Enter(*body));
                                continue;
                            }
                            LoopSyntax::For(header) => {
                                self.push_scope();
                                let header = self.for_header(header)?;
                                jobs.push(Job::Loop {
                                    kind: LoopKind::For,
                                    init: header.init,
                                    condition: header.condition,
                                    prologue: header.prologue,
                                    step: header.steps,
                                    scope: true,
                                    start: output.len(),
                                });
                                jobs.push(Job::Enter(*body));
                                continue;
                            }
                            LoopSyntax::Foreach(header) => {
                                self.push_scope();
                                let loops = self.native_foreach_indices(header)?;
                                jobs.push(Job::Foreach {
                                    loops,
                                    start: output.len(),
                                });
                                jobs.push(Job::Enter(*body));
                                continue;
                            }
                        };
                        jobs.push(Job::Loop {
                            kind,
                            init: Vec::new(),
                            condition,
                            prologue,
                            step: Vec::new(),
                            scope,
                            start: output.len(),
                        });
                        jobs.push(Job::Enter(*body));
                    }
                    // A randcase evaluates each weight once, as an
                    // unsigned value at the widest weight's precision,
                    // then picks a branch by a random number below their
                    // sum (IEEE 1800-2023 18.16).
                    StatementKind::Case {
                        keyword,
                        expression: None,
                        arms,
                        ..
                    } if keyword == "randcase" => {
                        let mut weight_ids = Vec::new();
                        for arm in arms {
                            match (arm.default, arm.matches.as_slice()) {
                                (false, [celox_sv_syntax::CaseMatch::Expression(weight)]) => {
                                    weight_ids.push(*weight);
                                }
                                _ => return Err(unsupported("randcase item")),
                            }
                        }
                        if weight_ids.is_empty() {
                            return Err(unsupported("randcase without items"));
                        }
                        let mut width = 1usize;
                        for weight in &weight_ids {
                            let ty = expression_types::type_of(
                                self.tree,
                                self.session,
                                *weight,
                                native,
                                &self.dims,
                            )?;
                            let semantic_types::TyData::Packed(packed) =
                                self.session.types.data(ty).clone()
                            else {
                                return Err(unsupported("non-integral randcase weight"));
                            };
                            width = width.max(
                                usize::try_from(packed.bits())
                                    .map_err(|_| unsupported("randcase weight width"))?,
                            );
                        }
                        if width > 64 {
                            return Err(unsupported("randcase weights wider than 64 bits"));
                        }
                        output.extend(self.effects(&weight_ids)?);
                        let mut weights = Vec::new();
                        for weight in &weight_ids {
                            let value = self.native_value(*weight)?;
                            let hidden = self.hidden(unsigned_type(width)).1;
                            output.push(Stmt::Assign {
                                lhs: LValue::Ident(hidden.clone()),
                                rhs: Expr::Resize {
                                    expr: Box::new(value),
                                    width,
                                    signed: false,
                                },
                                nonblocking: false,
                            });
                            weights.push(hidden);
                        }
                        jobs.push(Job::RandcaseNext(RandcaseState {
                            id,
                            index: 0,
                            weights,
                            width,
                            items: Vec::new(),
                        }));
                    }
                    // A case statement on a type reference selects its arm
                    // at elaboration: the first whose type matches, else the
                    // default one (IEEE 1800-2023 6.23).
                    StatementKind::Case {
                        keyword,
                        mode: None,
                        expression: Some(value),
                        arms,
                        ..
                    } if keyword == "case"
                        && expression_binding::type_references::reference(*value, native)
                            .is_some() =>
                    {
                        let selected = self.type_case_arm(*value, arms)?;
                        if let Some(body) = selected {
                            jobs.push(Job::Enter(body));
                        }
                    }
                    StatementKind::Case {
                        keyword,
                        mode,
                        expression: Some(value),
                        arms,
                        ..
                    } if keyword != "randcase" => {
                        if mode.as_deref() == Some("matches") {
                            return Err(unsupported("pattern-matching case statement"));
                        }
                        // A case statement compares with `===` (IEEE 1800-2023 12.5).
                        let operator = match (mode.as_deref(), keyword.as_str()) {
                            (Some("inside"), _) => "inside",
                            (_, "case") => "===",
                            (_, keyword) => keyword,
                        };
                        // Only `case` compares chandles, with chandle or
                        // `null` items (6.14); the binder rejects them
                        // elsewhere. A selector is a chandle also when only
                        // a default arm compares nothing with it.
                        let mut chandles = operator == "==="
                            && expression_binding::native_chandle(
                                self.tree,
                                *value,
                                native,
                                &self.dims,
                                self.session,
                            );
                        for case_match in arms.iter().flat_map(|arm| &arm.matches) {
                            if let celox_sv_syntax::CaseMatch::Expression(label) = case_match {
                                chandles |= expression_binding::check_chandle_equality(
                                    operator,
                                    *value,
                                    *label,
                                    self.tree,
                                    native,
                                    &self.dims,
                                    self.session,
                                )?;
                            }
                        }
                        let kind = if mode.as_deref() == Some("inside") {
                            CaseKind::Inside
                        } else {
                            match keyword.as_str() {
                                "case" => CaseKind::Exact,
                                "casez" => CaseKind::Z,
                                "casex" => CaseKind::X,
                                _ => return Err(disagreement()),
                            }
                        };
                        output.extend(self.effects(&[*value])?);
                        let selector = if chandles {
                            self.native_chandle_value(*value)?
                        } else {
                            self.native_value(*value)?
                        };
                        jobs.push(Job::CaseNext(CaseState {
                            id,
                            index: 0,
                            kind,
                            selector,
                            chandles,
                            items: Vec::new(),
                            default: None,
                        }));
                    }
                    StatementKind::Wait { condition, body } => {
                        self.allow_timing("wait statement")?;
                        // `wait(e.triggered)` (IEEE 1800-2023 15.5.3).
                        if let Some(event) = events::triggered(
                            self.tree,
                            self.session,
                            *condition,
                            self.tree.declarations(),
                            &self.dims,
                        )? {
                            let event = self.native_value(event)?;
                            // A level wait also checks the condition again when
                            // a variable of the design names another event.
                            // Locals and class properties cannot wake a wait, so
                            // a wait on them keeps the event it started with.
                            if self.watched_handle(&event) {
                                output.extend(self.wait_statements(events::call(
                                    celox_design::ProcessOp::Triggered,
                                    vec![event],
                                ))?);
                            } else {
                                output.push(Stmt::Eval(events::call(
                                    celox_design::ProcessOp::WaitTriggered,
                                    vec![event],
                                )));
                            }
                        } else {
                            let condition = self.native_value(*condition)?;
                            if events::reads_triggered(&condition) {
                                return Err(unsupported(
                                    "`triggered` in a wait condition with other operands",
                                ));
                            }
                            output.extend(self.wait_statements(condition)?);
                        }
                        jobs.push(Job::Enter(*body));
                    }
                    StatementKind::Timing { control, body } => {
                        if self.implicit_event_control(*control)? {
                            jobs.push(Job::ImplicitEvent {
                                start: output.len(),
                                sensitivity: self.sensitivity_body.len(),
                            });
                        } else if let Some(stmts) = self.virtual_event_control(*control)? {
                            output.extend(stmts);
                        } else {
                            let timing = self.native_timing_control(*control)?;
                            output.push(timing);
                        }
                        jobs.push(Job::Enter(*body));
                    }
                    // Explicit typed assignments/calls/assertions/unsupported
                    // forms retain their established binders and diagnostics.
                    StatementKind::Assertion {
                        keyword,
                        header:
                            header @ (celox_sv_syntax::AssertionHeader::Immediate { .. }
                            | celox_sv_syntax::AssertionHeader::Concurrent { .. }),
                        success,
                        failure,
                    } => {
                        let mut stages = None;
                        let mut expect = None;
                        let (condition, deferred, concurrent) = match header {
                            celox_sv_syntax::AssertionHeader::Immediate { deferred, .. } => (
                                self.native_assertion_condition(keyword, header)?,
                                deferred,
                                None,
                            ),
                            celox_sv_syntax::AssertionHeader::Concurrent { property, .. }
                                if keyword == "expect" =>
                            {
                                expect = Some(self.expect_attempt(property)?);
                                (Expr::Literal("1'b1".into()), &None, None)
                            }
                            celox_sv_syntax::AssertionHeader::Concurrent {
                                property:
                                    celox_sv_syntax::PropertySpec {
                                        clock,
                                        disable,
                                        antecedent,
                                        condition,
                                        equivalent,
                                        sequence,
                                    },
                                module_item,
                                ..
                            } => {
                                if !module_item {
                                    return Err(unsupported("procedural concurrent assertion"));
                                }
                                // Its clock is explicit or that of the named
                                // sequence or property it instantiates (the
                                // parser waits for it); default clocking
                                // (IEEE 1800-2023 16.15) is not supported.
                                if clock.is_none() {
                                    return Err(unsupported(
                                        "concurrent assertion without a clocking event",
                                    ));
                                }
                                if !matches!(keyword.as_str(), "assert" | "assume" | "cover") {
                                    return Err(unsupported("concurrent assertion directive"));
                                }
                                if let Some(sequence) = sequence {
                                    let monitor = self.sequence_stages(*sequence)?;
                                    // An attempt of one tick is disabled at that tick; a
                                    // longer one would need the condition at each tick.
                                    if disable.is_some() && monitor.ticks.is_some() {
                                        return Err(unsupported(
                                            "disable iff of a sequence over cycles",
                                        ));
                                    }
                                    stages = Some(monitor);
                                    let disable = disable
                                        .map(|id| {
                                            self.native_value(id).map(procedural_truth_condition)
                                        })
                                        .transpose()?;
                                    if let Some(disable) = &disable {
                                        assertions::concurrent_operand(disable)?;
                                        assertions::concurrent_disable(disable)?;
                                    }
                                    (Expr::Literal("1'b1".into()), &None, Some(disable))
                                } else {
                                    let mut operand = self.native_value(*condition)?;
                                    assertions::concurrent_operand(&operand)?;
                                    // `p iff q` holds when both are true or
                                    // both false; X/Z is false (16.12.8).
                                    if let Some(equivalent) = equivalent {
                                        let equivalent = self.native_value(*equivalent)?;
                                        assertions::concurrent_operand(&equivalent)?;
                                        operand = Expr::Binary {
                                            left: Box::new(procedural_truth_condition(operand)),
                                            op: BinaryOp::Eq,
                                            right: Box::new(procedural_truth_condition(equivalent)),
                                        };
                                    }
                                    if let Some(antecedent) = antecedent {
                                        let antecedent = self.native_value(*antecedent)?;
                                        assertions::concurrent_operand(&antecedent)?;
                                        // A Boolean sequence matches only when true.
                                        // X/Z antecedents have no match (16.12.7).
                                        let matched = procedural_truth_condition(antecedent);
                                        operand = Expr::Binary {
                                            left: Box::new(Expr::Unary {
                                                op: UnaryOp::LogicNot,
                                                expr: Box::new(matched),
                                            }),
                                            op: BinaryOp::LogicOr,
                                            right: Box::new(operand),
                                        };
                                    }
                                    let condition = procedural_truth_condition(Expr::Call {
                                        callee: Callee::System(SystemFunction::Sampled),
                                        args: vec![Some(operand)],
                                        arg_order: Vec::new(),
                                    });
                                    let disable = disable
                                        .map(|id| {
                                            self.native_value(id).map(procedural_truth_condition)
                                        })
                                        .transpose()?;
                                    if let Some(disable) = &disable {
                                        assertions::concurrent_operand(disable)?;
                                        assertions::concurrent_disable(disable)?;
                                    }
                                    (condition, &None, Some(disable))
                                }
                            }
                            _ => unreachable!(),
                        };
                        let statement = native.statement(id);
                        // The action of a deferred assertion is one
                        // subroutine call (IEEE 1800-2023 16.4), not a block.
                        if deferred.is_some()
                            && success.iter().chain(failure).any(|&action| {
                                !matches!(
                                    native.statement(action).kind,
                                    StatementKind::Empty | StatementKind::Expression(_)
                                )
                            })
                        {
                            return Err(unsupported(
                                "deferred assertion action other than one display or severity task call",
                            ));
                        }
                        jobs.push(Job::AssertionThen {
                            concurrent,
                            condition,
                            stages,
                            expect,
                            keyword: keyword.clone(),
                            assertion: celox_design::AssertionSite {
                                label: statement
                                    .label
                                    .as_ref()
                                    .map(|label| label.value().to_owned()),
                                directive: match keyword.as_str() {
                                    "assume" => celox_design::AssertionDirective::Assume,
                                    "cover" => celox_design::AssertionDirective::Cover,
                                    _ => celox_design::AssertionDirective::Assert,
                                },
                                deferral: match deferred {
                                    None => celox_design::AssertionDeferral::None,
                                    Some(celox_sv_syntax::AssertionDeferral::Zero) => {
                                        celox_design::AssertionDeferral::Observed
                                    }
                                    Some(celox_sv_syntax::AssertionDeferral::Final) => {
                                        celox_design::AssertionDeferral::Final
                                    }
                                },
                                outcome: celox_design::AssertionOutcome::Fail,
                            },
                            span: super::source_span(statement.tokens, self.tree),
                            failure: *failure,
                            start: output.len(),
                        });
                        if let Some(success) = success {
                            jobs.push(Job::Enter(*success));
                        }
                    }
                    StatementKind::Break => output.push(Stmt::Break),
                    StatementKind::Continue => output.push(Stmt::Continue),
                    StatementKind::Return(value) => {
                        if let Some(id) = value {
                            output.extend(self.effects(&[*id])?);
                            // A result of an unpacked array type is assigned
                            // like any array (IEEE 1800-2023 7.6, 13.4.1).
                            if let Some(target) = self
                                .return_shape
                                .as_ref()
                                .filter(|shape| !shape.unpacked.is_empty())
                            {
                                array_compatibility::check_native_unpacked_assignment(
                                    self.tree,
                                    self.session,
                                    *id,
                                    target,
                                    || "return value".into(),
                                    &self.dims,
                                )?;
                            }
                            // A class handle result takes a handle its class
                            // takes (8.16, 13.4.1).
                            if let Some(class) = self
                                .return_shape
                                .as_ref()
                                .filter(|shape| shape.unpacked.is_empty())
                                .and_then(|shape| shape.class.clone())
                            {
                                expression_binding::check_handle_assignment(
                                    self.tree,
                                    self.session,
                                    *id,
                                    native,
                                    &self.dims,
                                    &class,
                                    || "return value".into(),
                                )?;
                            }
                        }
                        let value = value
                            .map(|id| {
                                if let Some(chandle) = self.return_chandle {
                                    expression_binding::check_chandle_assignment(
                                        chandle,
                                        id,
                                        self.tree,
                                        native,
                                        &self.dims,
                                        self.session,
                                    )?;
                                }
                                let bind = if self.return_chandle == Some(true) {
                                    expression_binding::bind_chandle_value
                                } else {
                                    expression_binding::bind_value
                                };
                                let mut value = bind(
                                    id,
                                    native,
                                    self.tree,
                                    self.session,
                                    &self.dims,
                                    self.return_shape.as_ref(),
                                )?;
                                self.rename_expr(&mut value);
                                Ok(value)
                            })
                            .transpose()?;
                        output.push(Stmt::Return(value));
                    }
                    _ => output.extend(self.native_statement_leaf(&native.statement(id).kind)?),
                },
                Job::EndScope => self.pop_scope(),
                Job::BlockEnd { id, start } => {
                    let Some((_, registered, targeted)) = self.named_blocks.pop() else {
                        return Err(disagreement());
                    };
                    debug_assert_eq!(registered, id);
                    // A block no `disable` names is ordinary sequential
                    // code: keeping it flat leaves every analysis of the
                    // enclosing statement list as it is without the label.
                    if targeted {
                        let body = output.split_off(start);
                        output.push(Stmt::Block { id, body });
                    }
                }
                Job::ImplicitEvent { start, sensitivity } => {
                    let wait = timing::implicit_event(
                        &output[start..],
                        &self.sensitivity_body[sensitivity..],
                    );
                    output.insert(start, wait);
                }
                Job::AssertionThen {
                    concurrent,
                    condition,
                    stages,
                    expect,
                    keyword,
                    assertion,
                    span,
                    failure,
                    start,
                } => {
                    let then_body = output.split_off(start);
                    jobs.push(Job::AssertionFinish {
                        concurrent,
                        condition,
                        stages,
                        expect,
                        then_body,
                        assertion,
                        default_failure: if failure.is_none() && keyword != "cover" {
                            Some((keyword, span))
                        } else {
                            None
                        },
                        start: output.len(),
                    });
                    if let Some(failure) = failure {
                        jobs.push(Job::Enter(failure));
                    }
                }
                Job::AssertionFinish {
                    concurrent,
                    condition,
                    stages,
                    expect,
                    mut then_body,
                    assertion,
                    default_failure,
                    start,
                } => {
                    let mut else_body = output.split_off(start);
                    if let Some((keyword, span)) = default_failure {
                        else_body = assertion_failure(&keyword, span);
                    }
                    if assertion.deferral != celox_design::AssertionDeferral::None {
                        deferred_action(&then_body)?;
                        deferred_action(&else_body)?;
                    }
                    attribute_reports(
                        &mut then_body,
                        &assertion,
                        celox_design::AssertionOutcome::Pass,
                    );
                    attribute_reports(
                        &mut else_body,
                        &assertion,
                        celox_design::AssertionOutcome::Fail,
                    );
                    // An `expect` statement is not an assertion the tasks
                    // control (20.11).
                    if let Some(attempt) = expect {
                        output.extend(self.expect_statements(attempt, then_body, else_body));
                        continue;
                    }
                    let controlled = self.controls_assertions();
                    if concurrent.is_some() {
                        // `timed_tasks::classify` checks the actions once
                        // every subroutine is known.
                        // Evaluate every predicate in Observed; neither action can run
                        // until all the monitors have made that decision.
                        then_body.insert(0, Stmt::AssertionRegion { reactive: true });
                        else_body.insert(0, Stmt::AssertionRegion { reactive: true });
                        output.push(Stmt::AssertionRegion { reactive: false });
                    }
                    // A sequence over cycles decides each step that an
                    // attempt reaches at this tick, the oldest attempt first.
                    if let Some(monitor) = stages {
                        // Whether assertions are on for the attempt that
                        // starts at this tick, read in the Observed region
                        // before any action (4.4.2.5, 20.11; see
                        // `sequence_stages`).
                        output.extend(monitor.start);
                        let mut decisions = Vec::new();
                        // Older attempts first; steps at one offset in their
                        // order, a `##0` sequence's calls before its action.
                        let mut ordered = monitor.stages;
                        ordered.sort_by_key(|stage| std::cmp::Reverse(stage.offset));
                        for stage in ordered {
                            // Match-item calls and the pass action share one
                            // Reactive region.
                            let mut on_match = Vec::new();
                            if !stage.calls.is_empty() || stage.last {
                                on_match.push(Stmt::AssertionRegion { reactive: true });
                            }
                            on_match.extend(stage.calls);
                            if stage.last {
                                on_match.extend(then_body.iter().skip(1).cloned());
                            }
                            // A missed antecedent step succeeds vacuously.
                            let on_miss = if stage.antecedent {
                                then_body.clone()
                            } else {
                                else_body.clone()
                            };
                            let decision = Stmt::If {
                                condition: stage.matched,
                                then_body: on_match,
                                else_body: on_miss,
                            };
                            decisions.push(match stage.alive {
                                Some(alive) => Stmt::If {
                                    condition: alive,
                                    then_body: vec![decision],
                                    else_body: Vec::new(),
                                },
                                None => decision,
                            });
                        }
                        if let Some(Some(disable)) = &concurrent {
                            output.push(Stmt::If {
                                condition: disable.clone(),
                                then_body: Vec::new(),
                                else_body: decisions,
                            });
                        } else {
                            output.extend(decisions);
                        }
                        // The values later ticks read, once this tick
                        // decided its steps.
                        output.extend(monitor.updates);
                        if let Some((ticks, length)) = monitor.ticks {
                            let count = Expr::Ident(ticks.clone());
                            output.push(Stmt::If {
                                condition: Expr::Binary {
                                    left: Box::new(count.clone()),
                                    op: BinaryOp::Lt,
                                    right: Box::new(Expr::Literal(length.to_string())),
                                },
                                then_body: vec![Stmt::Assign {
                                    lhs: LValue::Ident(ticks),
                                    rhs: Expr::Binary {
                                        left: Box::new(count),
                                        op: BinaryOp::Add,
                                        right: Box::new(Expr::Literal("1".into())),
                                    },
                                    nonblocking: false,
                                }],
                                else_body: Vec::new(),
                            });
                        }
                        continue;
                    }
                    let decision = Stmt::If {
                        condition,
                        then_body,
                        else_body,
                    };
                    let decision = if let Some(Some(disable)) = concurrent {
                        Stmt::If {
                            condition: disable,
                            then_body: Vec::new(),
                            else_body: vec![decision],
                        }
                    } else {
                        decision
                    };
                    output.push(if controlled {
                        assertion_control::checked(vec![decision])
                    } else {
                        decision
                    });
                }
                Job::IfNext(state) => {
                    let StatementKind::Conditional {
                        branches,
                        otherwise,
                    } = &native.statement(state.id).kind
                    else {
                        return Err(disagreement());
                    };
                    if state.index == branches.len() {
                        jobs.push(Job::IfFinish {
                            branches: state.branches,
                            start: output.len(),
                        });
                        if let Some(id) = otherwise {
                            jobs.push(Job::Enter(*id));
                        }
                    } else {
                        let branch = &branches[state.index];
                        let effects = match branch.predicate.items.as_slice() {
                            [item] if item.pattern.is_none() => self.effects(&[item.expression])?,
                            _ => Vec::new(),
                        };
                        // The condition of an `else if` is evaluated only
                        // when the previous ones fail: the rest of the chain
                        // becomes the else branch.
                        let state = if effects.is_empty() || state.index == 0 {
                            state
                        } else {
                            jobs.push(Job::IfFinish {
                                branches: state.branches,
                                start: output.len(),
                            });
                            IfState {
                                id: state.id,
                                index: state.index,
                                branches: Vec::new(),
                            }
                        };
                        output.extend(effects);
                        let condition = self.cond_predicate(&branch.predicate)?;
                        jobs.push(Job::IfBranch {
                            state,
                            condition,
                            start: output.len(),
                        });
                        jobs.push(Job::Enter(branch.body));
                    }
                }
                Job::IfBranch {
                    mut state,
                    condition,
                    start,
                } => {
                    state.branches.push((condition, output.split_off(start)));
                    state.index += 1;
                    jobs.push(Job::IfNext(state));
                }
                Job::IfFinish { branches, start } => {
                    let otherwise = output.split_off(start);
                    output.extend(self.fold_conditional(branches, otherwise));
                }
                Job::CaseNext(state) => {
                    let StatementKind::Case { arms, .. } = &native.statement(state.id).kind else {
                        return Err(disagreement());
                    };
                    if state.index == arms.len() {
                        output.push(Stmt::Case {
                            kind: state.kind,
                            selector: state.selector,
                            items: state.items,
                            default: state.default,
                        });
                    } else {
                        let arm = &arms[state.index];
                        let labels = if arm.default {
                            None
                        } else {
                            Some(self.native_case_labels(
                                &arm.matches,
                                state.kind == CaseKind::Inside,
                                state.chandles,
                            )?)
                        };
                        jobs.push(Job::CaseBranch {
                            state,
                            labels,
                            start: output.len(),
                        });
                        jobs.push(Job::Enter(arm.body));
                    }
                }
                Job::CaseBranch {
                    mut state,
                    labels,
                    start,
                } => {
                    let body = output.split_off(start);
                    if let Some(labels) = labels {
                        state.items.push(CaseItemBase { labels, body });
                    } else {
                        state.default = Some(body);
                    }
                    state.index += 1;
                    jobs.push(Job::CaseNext(state));
                }
                Job::RandcaseNext(state) => {
                    let StatementKind::Case { arms, .. } = &native.statement(state.id).kind else {
                        return Err(disagreement());
                    };
                    if state.index == arms.len() {
                        output.extend(self.randcase_finish(state));
                    } else {
                        let body = arms[state.index].body;
                        jobs.push(Job::RandcaseBranch {
                            state,
                            start: output.len(),
                        });
                        jobs.push(Job::Enter(body));
                    }
                }
                Job::RandcaseBranch { mut state, start } => {
                    state.items.push(output.split_off(start));
                    state.index += 1;
                    jobs.push(Job::RandcaseNext(state));
                }
                Job::Loop {
                    kind,
                    init,
                    condition,
                    mut prologue,
                    step,
                    scope,
                    start,
                } => {
                    prologue.extend(output.split_off(start));
                    let body = prologue;
                    if scope {
                        self.pop_scope();
                    }
                    output.push(Stmt::Loop {
                        kind,
                        init,
                        condition,
                        step,
                        body,
                    });
                }
                Job::DoWhile { native, start } => {
                    let mut body = output.split_off(start);
                    if let Some(prologue) = self.loop_condition(native)? {
                        // `continue` evaluates the condition too (12.8): it
                        // starts every iteration but the first.
                        let first = self.hidden(Type::new(TypeKind::Logic)).1;
                        output.push(Stmt::Assign {
                            lhs: LValue::Ident(first.clone()),
                            rhs: Expr::Literal("1'b1".into()),
                            nonblocking: false,
                        });
                        body.insert(
                            0,
                            Stmt::If {
                                condition: Expr::Ident(first.clone()),
                                then_body: vec![Stmt::Assign {
                                    lhs: LValue::Ident(first),
                                    rhs: Expr::Literal("1'b0".into()),
                                    nonblocking: false,
                                }],
                                else_body: prologue,
                            },
                        );
                        output.push(Stmt::Loop {
                            kind: LoopKind::For,
                            init: Vec::new(),
                            condition: None,
                            step: Vec::new(),
                            body,
                        });
                        continue;
                    }
                    let condition = self.native_chandle_value(native)?;
                    output.push(Stmt::Loop {
                        kind: LoopKind::DoWhile,
                        init: Vec::new(),
                        condition: Some(condition),
                        step: Vec::new(),
                        body,
                    });
                }
                Job::Foreach { loops, start } => {
                    let body = output.split_off(start);
                    // A `break` leaves the whole foreach, not only the loop
                    // of its innermost dimension (IEEE 1800-2023 12.8).
                    let stop = (loops.len() > 1 && breaks(&body))
                        .then(|| self.declare("$celox_foreach_break", int_type()));
                    self.pop_scope();
                    output.extend(Self::fold_foreach(loops, body, stop));
                }
            }
        }
        Ok(output)
    }
    fn native_case_labels(
        &mut self,
        labels: &[celox_sv_syntax::CaseMatch],
        inside: bool,
        chandles: bool,
    ) -> Result<Vec<CaseLabel<Expr>>, AnalyzerError> {
        if labels.is_empty() {
            return Err(AnalyzerError::Parse("native case arm has no labels".into()));
        }
        labels
            .iter()
            .map(|label| match label {
                celox_sv_syntax::CaseMatch::Expression(id) if chandles => {
                    self.native_chandle_value(*id).map(CaseLabel::Value)
                }
                celox_sv_syntax::CaseMatch::Expression(id) => {
                    self.native_value(*id).map(CaseLabel::Value)
                }
                celox_sv_syntax::CaseMatch::Range { left, right } if inside => {
                    Ok(CaseLabel::Range {
                        low: self.native_value(*left)?,
                        high: self.native_value(*right)?,
                    })
                }
                _ => Err(unsupported("pattern-matching case label")),
            })
            .collect()
    }
}

/// Whether `body` has a `break` of the loop that holds it, outside nested
/// loops and fork branches.
fn breaks(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Break => true,
        Stmt::If {
            then_body,
            else_body,
            ..
        } => breaks(then_body) || breaks(else_body),
        Stmt::Case { items, default, .. } => {
            items.iter().any(|item| breaks(&item.body)) || default.as_deref().is_some_and(breaks)
        }
        Stmt::WaitImplicit { reads, .. } => breaks(reads),
        // A named block is no loop: a `break` in it leaves this one.
        Stmt::Block { body, .. } => breaks(body),
        _ => false,
    })
}

/// `body` with each `break` of the loop that holds it, outside nested loops
/// and fork branches, first setting `stop`.
pub(super) fn mark_breaks(body: Vec<Stmt>, stop: &str) -> Vec<Stmt> {
    let mut marked = Vec::with_capacity(body.len());
    for stmt in body {
        match stmt {
            Stmt::Break => {
                marked.push(Stmt::Assign {
                    lhs: LValue::Ident(stop.to_owned()),
                    rhs: Expr::Literal("1".into()),
                    nonblocking: false,
                });
                marked.push(Stmt::Break);
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => marked.push(Stmt::If {
                condition,
                then_body: mark_breaks(then_body, stop),
                else_body: mark_breaks(else_body, stop),
            }),
            Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => marked.push(Stmt::Case {
                kind,
                selector,
                items: items
                    .into_iter()
                    .map(|mut item| {
                        item.body = mark_breaks(item.body, stop);
                        item
                    })
                    .collect(),
                default: default.map(|body| mark_breaks(body, stop)),
            }),
            Stmt::WaitImplicit { reads, always_comb } => marked.push(Stmt::WaitImplicit {
                reads: mark_breaks(reads, stop),
                always_comb,
            }),
            Stmt::Block { id, body } => marked.push(Stmt::Block {
                id,
                body: mark_breaks(body, stop),
            }),
            stmt => marked.push(stmt),
        }
    }
    marked
}

/// An unsigned packed type of `width` bits, for hidden randcase values.
fn unsigned_type(width: usize) -> Type {
    let mut ty = super::int_type();
    ty.is_signed = false;
    ty.packed_ranges = vec![PackedRange::new(
        ConstExpr::Literal((width - 1).to_string()),
        ConstExpr::Literal("0".to_string()),
    )];
    ty
}

impl BodyBuilder<'_, '_, '_> {
    /// The statements that pick a randcase branch: when the weights sum
    /// to a nonzero total, draw a number below it with the thread RNG
    /// and take the first branch whose cumulative weight exceeds it; a
    /// zero total takes no branch (IEEE 1800-2023 18.16).
    fn randcase_finish(&mut self, state: RandcaseState) -> Vec<Stmt> {
        let width = state.width;
        let ty = unsigned_type(width);
        let literal = |value: u32| Expr::Literal(format!("{width}'d{value}"));
        let sum = |names: &[String]| {
            names
                .iter()
                .map(|name| Expr::Ident(name.clone()))
                .reduce(|left, right| Expr::Binary {
                    left: Box::new(left),
                    op: BinaryOp::Add,
                    right: Box::new(right),
                })
                .expect("a randcase has weights")
        };
        let total = self.hidden(ty.clone()).1;
        let pick = self.hidden(ty).1;
        let urandom = || Expr::Call {
            callee: Callee::System(SystemFunction::Urandom),
            args: Vec::new(),
            arg_order: Vec::new(),
        };
        let draw = if width <= 32 {
            Expr::Call {
                callee: Callee::System(SystemFunction::UrandomRange),
                args: vec![
                    Some(Expr::Binary {
                        left: Box::new(Expr::Ident(total.clone())),
                        op: BinaryOp::Sub,
                        right: Box::new(literal(1)),
                    }),
                    Some(literal(0)),
                ],
                arg_order: Vec::new(),
            }
        } else {
            // Two 32-bit draws make a 64-bit one; the high half of its
            // product with the total is uniform below the total, the
            // scaling `$urandom_range` itself uses.
            let widen = |expr: Expr| Expr::Resize {
                expr: Box::new(expr),
                width: 128,
                signed: false,
            };
            Expr::Resize {
                expr: Box::new(Expr::Binary {
                    left: Box::new(Expr::Binary {
                        left: Box::new(widen(Expr::Concat(vec![urandom(), urandom()]))),
                        op: BinaryOp::Mul,
                        right: Box::new(widen(Expr::Ident(total.clone()))),
                    }),
                    op: BinaryOp::Shr,
                    right: Box::new(Expr::Literal("128'd64".into())),
                }),
                width,
                signed: false,
            }
        };
        let mut chain: Vec<Stmt> = Vec::new();
        for (index, body) in state.items.into_iter().enumerate().rev() {
            chain = vec![Stmt::If {
                condition: Expr::Binary {
                    left: Box::new(Expr::Ident(pick.clone())),
                    op: BinaryOp::Lt,
                    right: Box::new(sum(&state.weights[..=index])),
                },
                then_body: body,
                else_body: chain,
            }];
        }
        let mut guarded = vec![Stmt::Assign {
            lhs: LValue::Ident(pick),
            rhs: draw,
            nonblocking: false,
        }];
        guarded.extend(chain);
        vec![
            Stmt::Assign {
                lhs: LValue::Ident(total.clone()),
                rhs: sum(&state.weights),
                nonblocking: false,
            },
            Stmt::If {
                condition: Expr::Binary {
                    left: Box::new(Expr::Ident(total)),
                    op: BinaryOp::Ne,
                    right: Box::new(literal(0)),
                },
                then_body: guarded,
                else_body: Vec::new(),
            },
        ]
    }

    /// The body of the arm that a case statement on type reference `value`
    /// selects: the first whose type matches, else the default one (IEEE
    /// 1800-2023 6.23).
    fn type_case_arm(
        &mut self,
        value: celox_sv_syntax::ExprId,
        arms: &[celox_sv_syntax::CaseArm],
    ) -> Converted<Option<StatementId>> {
        let native = self.tree.declarations();
        for arm in arms {
            for case_match in &arm.matches {
                let celox_sv_syntax::CaseMatch::Expression(label) = case_match else {
                    return Err(unsupported("range item of a case on a type"));
                };
                if expression_binding::type_references::matching(
                    self.tree,
                    self.session,
                    value,
                    *label,
                    native,
                    &self.dims,
                )? {
                    return Ok(Some(arm.body));
                }
            }
        }
        Ok(arms.iter().find(|arm| arm.default).map(|arm| arm.body))
    }

    /// The steps of sequence `id` as each tick of the assertion clock decides
    /// them, in the order of their offsets (IEEE 1800-2023 16.7, 16.10,
    /// 16.12.7). Each tick starts an attempt; with fixed delays, the attempt
    /// that started `k` ticks ago reaches the step at offset `k` if every
    /// earlier step matched at its own offset. Every value is evaluated
    /// over the sampled values of the tick that decides it (16.6); a later
    /// tick reads it from a register of the monitor.
    pub(super) fn sequence_stages(
        &mut self,
        id: celox_sv_syntax::SequenceId,
    ) -> Converted<assertions::SequenceMonitor> {
        let native = self.tree.declarations();
        let sequence = native.sequence(id).clone();
        // Local variables bind as locals of the monitor; their reads become
        // the values their assignments sampled.
        self.push_scope();
        let mut locals = assertions::SequenceLocals::default();
        let mut local_types = HashMap::default();
        let mut registers = Vec::new();
        for local in &sequence.locals {
            let ty = type_binding::bind(local.data_type, self.tree, self.session, &self.dims)?
                .filter(|ty| {
                    // An integral variable, which a register can keep.
                    semantic_types::of_type(
                        &mut self.session.types,
                        &self.tree.functions,
                        ty,
                        &self.dims.const_env,
                    )
                    .is_some_and(|ty| self.session.types.is_integral(ty))
                })
                .ok_or_else(|| unsupported("type of a local variable of a sequence"))?;
            for name in &local.names {
                let unique = self.declare(&normalize_identifier(&name.spelling), ty.clone());
                locals.insert(unique.clone(), Vec::new());
                local_types.insert(unique, ty.clone());
            }
        }
        let mut steps = Vec::new();
        let mut offset = 0u32;
        let mut order = 0usize;
        let parts = [
            (true, sequence.antecedent.clone().unwrap_or_default()),
            (false, sequence.consequent.clone()),
        ];
        for (antecedent, part) in parts {
            for step in part {
                offset = offset
                    .checked_add(step.delay)
                    .ok_or_else(|| unsupported("sequence longer than 2^32 ticks"))?;
                let point = assertions::SequencePoint {
                    at: offset,
                    before: order,
                };
                let condition = self.native_value(step.expression)?;
                assertions::concurrent_operand(&condition)?;
                let condition = self.sequence_value(&condition, point, &locals, &mut registers)?;
                let mut calls = Vec::new();
                for item in &step.items {
                    match item {
                        celox_sv_syntax::SequenceMatchItem::Assign {
                            target,
                            operator,
                            value,
                        } => {
                            let native = self.tree.declarations();
                            let celox_sv_syntax::ExprKind::Name(name) = &native
                                .expression(expression_binding::ungroup(*target, native))
                                .kind
                            else {
                                return Err(unsupported(
                                    "match item assigning a select of a local variable",
                                ));
                            };
                            let source = normalize_identifier(&name.spelling);
                            let lvalue = LValue::Ident(source.clone());
                            let rhs = match value {
                                Some(value) => expression_binding::value(
                                    *value,
                                    native,
                                    self.tree,
                                    self.session,
                                    &self.dims,
                                )?,
                                None => Expr::Literal("1".into()),
                            };
                            // An operator assignment, increment or decrement
                            // reads the local's previous value.
                            let value = match operator.as_str() {
                                "=" => rhs,
                                "++" | "--" => statements::assignment_op_expr(
                                    self.tree,
                                    self.session,
                                    &lvalue,
                                    if operator == "++" { "+=" } else { "-=" },
                                    rhs,
                                    &self.dims,
                                )?,
                                _ => statements::assignment_op_expr(
                                    self.tree,
                                    self.session,
                                    &lvalue,
                                    operator,
                                    rhs,
                                    &self.dims,
                                )?,
                            };
                            // The value converts to the local's type as an
                            // assignment's does (10.7).
                            let mut value = coerce_procedural_assignment_rhs(
                                self.tree, value, &lvalue, &self.dims,
                            );
                            self.rename_expr(&mut value);
                            assertions::concurrent_operand(&value)?;
                            let mut target = Expr::Ident(source);
                            self.rename_expr(&mut target);
                            let Expr::Ident(unique) = target else {
                                return Err(unsupported("match item target"));
                            };
                            if !locals.contains_key(&unique) {
                                return Err(unsupported(
                                    "match item assigning a variable that is no local variable of the sequence",
                                ));
                            }
                            // The value is that of the tick of its step, over
                            // its sampled values (16.10).
                            let here = assertions::SequencePoint {
                                at: offset,
                                before: order,
                            };
                            let value =
                                self.sequence_value(&value, here, &locals, &mut registers)?;
                            registers.push(assertions::SequenceRegister {
                                now: assertions::sampled(value.clone()),
                                ty: local_types[&unique].clone(),
                                chain: Vec::new(),
                            });
                            let register = registers.len() - 1;
                            locals.get_mut(&unique).expect("a local variable").push(
                                assertions::SequenceAssignment {
                                    order,
                                    offset,
                                    value,
                                    register,
                                },
                            );
                            order += 1;
                        }
                        celox_sv_syntax::SequenceMatchItem::Call(call) => {
                            // An argument passed by value takes the sampled
                            // value, and one reading a local variable the
                            // value its assignment sampled (16.11).
                            let here = assertions::SequencePoint {
                                at: offset,
                                before: order,
                            };
                            for mut statement in self.subroutine_call_statement(*call)? {
                                self.sample_call_arguments(
                                    &mut statement,
                                    here,
                                    &locals,
                                    &mut registers,
                                )?;
                                calls.push(statement);
                            }
                        }
                    }
                }
                steps.push((antecedent, point, condition, calls));
            }
        }
        let length = offset;
        // A value later ticks read takes a register per tick it is kept.
        if length > MAX_SEQUENCE_TICKS {
            return Err(unsupported(format!(
                "sequence longer than {MAX_SEQUENCE_TICKS} ticks"
            )));
        }
        let ticks = (length > 0).then(|| self.declare("ticks@sequence", super::int_type()));
        // Each step's match at this tick, kept for the later steps of the
        // attempt.
        let mut bit = Type::new(TypeKind::Bit);
        bit.is_signed = false;
        let mut matches = Vec::with_capacity(steps.len());
        for (_, _, condition, _) in &steps {
            let matched = procedural_truth_condition(assertions::sampled(condition.clone()));
            registers.push(assertions::SequenceRegister {
                now: matched.clone(),
                ty: bit.clone(),
                chain: Vec::new(),
            });
            matches.push((matched, registers.len() - 1));
        }
        // Whether assertions were on at the tick an attempt started, read
        // once in the Observed region before any action of the tick runs
        // (4.4.2.5): no new attempt starts while they are off, and one that
        // started before `$assertoff` goes on with its pass or fail action
        // (20.11), even when an action of this tick calls `$assertoff`.
        let mut start = Vec::new();
        let started = if self.controls_assertions() {
            let on = self.declare("on@sequence", bit.clone());
            start.push(Stmt::Assign {
                lhs: LValue::Ident(on.clone()),
                rhs: assertion_control::on(),
                nonblocking: false,
            });
            registers.push(assertions::SequenceRegister {
                now: Expr::Ident(on),
                ty: bit.clone(),
                chain: Vec::new(),
            });
            Some(registers.len() - 1)
        } else {
            None
        };
        let count = steps.len();
        let mut stages = Vec::with_capacity(count);
        for (index, (antecedent, point, _, calls)) in steps.iter().enumerate() {
            // The attempt that started `point.at` ticks ago exists.
            let mut alive = ticks
                .as_ref()
                .filter(|_| point.at > 0)
                .map(|ticks| Expr::Binary {
                    left: Box::new(Expr::Ident(ticks.clone())),
                    op: BinaryOp::Ge,
                    right: Box::new(Expr::Literal(point.at.to_string())),
                });
            for (earlier, (matched, register)) in steps[..index].iter().zip(&matches) {
                let ago = point.at - earlier.1.at;
                let matched = if ago == 0 {
                    matched.clone()
                } else {
                    Expr::Ident(self.sequence_register(&mut registers, *register, ago))
                };
                alive = Some(match alive {
                    None => matched,
                    Some(previous) => Expr::Binary {
                        left: Box::new(previous),
                        op: BinaryOp::LogicAnd,
                        right: Box::new(matched),
                    },
                });
            }
            if let Some(started) = started {
                let on = if point.at == 0 {
                    registers[started].now.clone()
                } else {
                    Expr::Ident(self.sequence_register(&mut registers, started, point.at))
                };
                alive = Some(match alive {
                    None => on,
                    Some(previous) => Expr::Binary {
                        left: Box::new(previous),
                        op: BinaryOp::LogicAnd,
                        right: Box::new(on),
                    },
                });
            }
            stages.push(assertions::SequenceStage {
                alive,
                matched: matches[index].0.clone(),
                antecedent: *antecedent,
                last: index + 1 == count,
                calls: calls.clone(),
                offset: point.at,
            });
        }
        let updates =
            assertions::register_updates(registers, |ty| self.declare("now@sequence", ty.clone()));
        self.pop_scope();
        Ok(assertions::SequenceMonitor {
            stages,
            ticks: ticks.map(|ticks| (ticks, length)),
            updates,
            start,
        })
    }

    /// Rewrite the arguments of match-item call `statement` at `here`. An
    /// argument passed by value, to an `input` formal or a system task
    /// argument the task does not write, is evaluated over sampled values;
    /// any argument reading a local variable takes the value its assignment
    /// sampled (16.11).
    fn sample_call_arguments(
        &mut self,
        statement: &mut Stmt,
        here: assertions::SequencePoint,
        locals: &assertions::SequenceLocals,
        registers: &mut [assertions::SequenceRegister],
    ) -> Converted<()> {
        let all_directions = self.state.subroutine_directions;
        let mut sampled = |expr: &mut Expr, argument: Argument| -> Converted<()> {
            // A written or referenced argument needs the storage of what it
            // names, which a local's kept value is not.
            if argument == Argument::Storage
                && locals.keys().any(|local| {
                    let mut reads = false;
                    rename_expr_idents(&mut expr.clone(), &mut |name| {
                        reads |= name == local;
                        None
                    });
                    reads
                })
            {
                return Err(unsupported(
                    "local variable of a sequence passed to an output, inout or ref argument",
                ));
            }
            let by_value = argument == Argument::Value;
            let value = self.sequence_value(expr, here, locals, registers)?;
            *expr = if by_value {
                // A value read over sampled values, as an operand is: a user
                // function would read module variables that sampling does
                // not reach (16.6).
                assertions::concurrent_operand(&value)?;
                assertions::sampled(value)
            } else {
                value
            };
            Ok(())
        };
        match statement {
            Stmt::SystemTask { task, args, .. } => {
                let task = *task;
                for (position, arg) in args.iter_mut().enumerate() {
                    if let crate::procedural::SystemTaskArg::Expr(expr) = arg {
                        // A task's destination, as `$readmemh`'s memory, is
                        // written, not read (21.4).
                        sampled(
                            expr,
                            if task.writes_argument(position) {
                                Argument::Storage
                            } else {
                                Argument::Value
                            },
                        )?;
                    }
                }
            }
            Stmt::Call { name, args, .. } => {
                let directions = all_directions.get(name.as_str());
                for (index, arg) in args.iter_mut().enumerate() {
                    if let Some(expr) = arg {
                        let argument = match directions.and_then(|directions| directions.get(index))
                        {
                            Some(crate::procedural::ParamDirection::Input) => Argument::Value,
                            Some(direction)
                                if direction.is_written() || direction.is_reference() =>
                            {
                                Argument::Storage
                            }
                            _ => Argument::Other,
                        };
                        sampled(expr, argument)?;
                    }
                }
            }
            _ => {
                let mut error = None;
                statement.visit_mut(
                    &mut |expr| {
                        if let Err(failure) = sampled(expr, Argument::Other) {
                            error = error.take().or(Some(failure));
                        }
                    },
                    &mut |_| {},
                    &mut |_| {},
                );
                if let Some(error) = error {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

/// The most ticks a sequence may span: each value a later tick reads is
/// kept in a register per tick.
const MAX_SEQUENCE_TICKS: u32 = 1024;

impl BodyBuilder<'_, '_, '_> {
    /// The attempt of `expect (property)` (IEEE 1800-2023 16.17): a
    /// Boolean property, an implication or a sequence of steps a fixed
    /// number of ticks apart, with an explicit clock, without `disable iff`,
    /// local variables or match items.
    fn expect_attempt(
        &mut self,
        property: &celox_sv_syntax::PropertySpec,
    ) -> Converted<ExpectAttempt> {
        // A named sequence or property of the module stands for its body
        // (16.8, 16.12), unless the expect's scope declares the name too.
        let named = |this: &Self, id: Option<celox_sv_syntax::ExprId>| {
            id.and_then(|id| this.expect_instance(id))
        };
        let resolved;
        let mut declared = false;
        let mut body_clock = false;
        let property = match named(self, Some(property.condition)) {
            Some(body)
                if property.sequence.is_none()
                    && property.antecedent.is_none()
                    && property.equivalent.is_none() =>
            {
                if [body.antecedent, Some(body.condition), body.equivalent]
                    .into_iter()
                    .any(|id| named(self, id).is_some())
                    || property.disable.is_some() && body.disable.is_some()
                {
                    return Err(unsupported(
                        "instance in the body of a named sequence or property in an expect statement",
                    ));
                }
                declared = true;
                body_clock = body.clock.is_some();
                resolved = celox_sv_syntax::PropertySpec {
                    clock: body.clock.or(property.clock),
                    disable: body.disable.or(property.disable),
                    ..body
                };
                &resolved
            }
            Some(_) => {
                return Err(unsupported(
                    "named sequence or property as an operand in an expect statement",
                ));
            }
            None => {
                if [property.antecedent, property.equivalent]
                    .into_iter()
                    .any(|id| named(self, id).is_some())
                {
                    return Err(unsupported(
                        "named sequence or property as an operand in an expect statement",
                    ));
                }
                property
            }
        };
        let clock = property
            .clock
            .ok_or_else(|| unsupported("expect statement without a clocking event"))?;
        if property.disable.is_some() {
            return Err(unsupported("disable iff in an expect statement"));
        }
        // The body of a named sequence or property binds in its
        // declaration's scope, the expect's own clock in the expect's.
        let steps = if declared {
            self.in_declaration_scope(|this| this.expect_steps(property))?
        } else {
            self.expect_steps(property)?
        };
        let clock = if declared && body_clock {
            self.in_declaration_scope(|this| this.native_timing_control(clock))?
        } else {
            self.native_timing_control(clock)?
        };
        Ok(ExpectAttempt { clock, steps })
    }

    /// The steps of `property` by offset, each with whether it matches,
    /// sampled, and whether it is an antecedent step (see
    /// [`ExpectAttempt::steps`]).
    fn expect_steps(
        &mut self,
        property: &celox_sv_syntax::PropertySpec,
    ) -> Converted<Vec<(u32, Expr, bool)>> {
        let mut steps = Vec::new();
        let mut step = |this: &mut Self, offset: u32, id, antecedent| -> Converted<()> {
            let value = this.native_value(id)?;
            assertions::concurrent_operand(&value)?;
            steps.push((
                offset,
                procedural_truth_condition(assertions::sampled(value)),
                antecedent,
            ));
            Ok(())
        };
        match property.sequence {
            None => {
                if let Some(antecedent) = property.antecedent {
                    step(self, 0, antecedent, true)?;
                }
                match property.equivalent {
                    // `p iff q` holds when both are true or both false
                    // (16.12.8).
                    Some(equivalent) => {
                        let left = self.native_value(property.condition)?;
                        assertions::concurrent_operand(&left)?;
                        let right = self.native_value(equivalent)?;
                        assertions::concurrent_operand(&right)?;
                        steps.push((
                            0,
                            Expr::Binary {
                                left: Box::new(procedural_truth_condition(assertions::sampled(
                                    left,
                                ))),
                                op: BinaryOp::Eq,
                                right: Box::new(procedural_truth_condition(assertions::sampled(
                                    right,
                                ))),
                            },
                            false,
                        ));
                    }
                    None => step(self, 0, property.condition, false)?,
                }
            }
            Some(id) => {
                let sequence = self.tree.declarations().sequence(id).clone();
                if !sequence.locals.is_empty() {
                    return Err(unsupported(
                        "local variable of a sequence in an expect statement",
                    ));
                }
                let mut offset = 0u32;
                let parts = [
                    (true, sequence.antecedent.unwrap_or_default()),
                    (false, sequence.consequent),
                ];
                for (antecedent, part) in parts {
                    for native in part {
                        if !native.items.is_empty() {
                            return Err(unsupported("match item in an expect statement"));
                        }
                        offset = offset
                            .checked_add(native.delay)
                            .filter(|&offset| offset <= MAX_SEQUENCE_TICKS)
                            .ok_or_else(|| {
                                unsupported(format!(
                                    "sequence longer than {MAX_SEQUENCE_TICKS} ticks"
                                ))
                            })?;
                        step(self, offset, native.expression, antecedent)?;
                    }
                }
            }
        }
        Ok(steps)
    }

    /// The body of the named sequence or property of the module that
    /// expression `id`, a bare name, instantiates, if no local or formal of
    /// the current scope has the name.
    fn expect_instance(
        &self,
        id: celox_sv_syntax::ExprId,
    ) -> Option<celox_sv_syntax::PropertySpec> {
        let native = self.tree.declarations();
        let id = expression_binding::ungroup(id, native);
        let celox_sv_syntax::ExprKind::Name(name) = &native.expression(id).kind else {
            return None;
        };
        let name = name.value();
        let shadowed = self
            .names
            .scopes
            .iter()
            .any(|scope| scope.entries.iter().any(|entry| entry.source == name));
        if shadowed {
            return None;
        }
        let unit = self.tree.root_unit().ok()?.tokens;
        native
            .assertion_declarations
            .iter()
            .find(|declaration| {
                declaration.name.value() == name
                    && unit.start <= declaration.tokens.start
                    && declaration.tokens.end <= unit.end
            })
            .map(|declaration| declaration.body)
    }

    /// The statements of an `expect`: the process waits for each tick of
    /// the attempt in turn and decides that tick's steps on their sampled
    /// values in the Observed region, until one misses or the last matches.
    /// It then runs `then_body` or `else_body` in the Reactive region and
    /// goes on (IEEE 1800-2023 16.17).
    fn expect_statements(
        &mut self,
        attempt: ExpectAttempt,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
    ) -> Vec<Stmt> {
        let alive = self.declare("$expect_alive", super::int_type());
        let passed = self.declare("$expect_passed", super::int_type());
        let assign = |name: &str, value: &str| Stmt::Assign {
            lhs: LValue::Ident(name.to_owned()),
            rhs: Expr::Literal(value.into()),
            nonblocking: false,
        };
        let is_alive = || Expr::Ident(alive.clone());
        let mut output = vec![assign(&alive, "1"), assign(&passed, "1")];
        let length = attempt.steps.last().map_or(0, |(offset, ..)| *offset);
        let mut steps = attempt.steps.into_iter().peekable();
        for tick in 0..=length {
            let mut body = vec![
                attempt.clock.clone(),
                Stmt::AssertionRegion { reactive: false },
            ];
            while let Some((_, matched, antecedent)) = steps.next_if(|(offset, ..)| *offset == tick)
            {
                // A missed antecedent step succeeds vacuously (16.12.7).
                body.push(Stmt::If {
                    condition: is_alive(),
                    then_body: vec![Stmt::If {
                        condition: matched,
                        then_body: Vec::new(),
                        else_body: vec![
                            assign(&alive, "0"),
                            assign(&passed, if antecedent { "1" } else { "0" }),
                        ],
                    }],
                    else_body: Vec::new(),
                });
            }
            output.push(Stmt::If {
                condition: is_alive(),
                then_body: body,
                else_body: Vec::new(),
            });
        }
        output.push(Stmt::AssertionRegion { reactive: true });
        output.push(Stmt::If {
            condition: Expr::Ident(passed),
            then_body,
            else_body,
        });
        output
    }
}

/// How a match-item call passes an argument (16.11).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Argument {
    /// By value: evaluated over sampled values.
    Value,
    /// To a formal or task argument that writes or refers to it.
    Storage,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_scope_jobs_restore_bindings_after_a_typed_leaf_error() {
        let session = &mut Session::default();
        let tree=crate::source_tree::parse_source("module M;initial begin automatic int outer_value=1;begin automatic int inner_value=2;disable target;end end endmodule",std::path::Path::new("scope-error.sv")).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let mut locals = Vec::new();
        let mut counter = 0;
        let params = HashMap::default();
        let directions = HashMap::default();
        let mut state = BodyState {
            locals: &mut locals,
            counter: &mut counter,
            subroutine_params: &params,
            subroutine_directions: &directions,
            automatic: false,
            statics: Vec::new(),
            explicit_statics: HashSet::default(),
            static_initializers: Vec::new(),
            constant_evaluation: false,
        };
        let mut builder = BodyBuilder::new(
            &tree,
            session,
            &PackedDimensions::default(),
            &mut state,
            system_functions::Body::Initial,
        );
        let id = tree.declarations().procedures[0].body;
        let error = builder
            .native_body(
                &[id],
                &tree.declarations().procedures[0].authored_statements,
            )
            .unwrap_err();
        assert!(error.to_string().contains("disable statement"), "{error}");
        assert_eq!(builder.names.scopes.len(), 1);
        for name in ["outer_value", "inner_value"] {
            assert!(builder.names.lookup(name).is_none());
            assert!(builder.dims.get(name).is_none());
            assert!(!builder.names.local_constants.contains_key(name));
        }
    }
}
