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
    items: Vec<CaseItemBase<Expr, LValue>>,
    default: Option<Vec<Stmt>>,
}
enum Job {
    Enter(StatementId),
    EndScope,
    ForkNext {
        items: Vec<StatementId>,
        index: usize,
        join: celox_design::ForkJoin,
        branches: Vec<Vec<Stmt>>,
    },
    ForkBranch {
        items: Vec<StatementId>,
        index: usize,
        join: celox_design::ForkJoin,
        branches: Vec<Vec<Stmt>>,
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
        keyword: String,
        assertion: celox_design::AssertionSite,
        span: Option<crate::procedural::SourceSpan>,
        failure: Option<StatementId>,
        start: usize,
    },
    AssertionFinish {
        concurrent: Option<Option<Expr>>,
        condition: Expr,
        then_body: Vec<Stmt>,
        assertion: celox_design::AssertionSite,
        default_failure: Option<(String, Option<crate::procedural::SourceSpan>)>,
        start: usize,
    },
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
        let depth = self.scopes.len();
        let result = self.native_body_machine(ids);
        // Restore lexical bindings on every diagnostic path, just as the old
        // recursive block/for wrappers did; no native failure is retried.
        while self.scopes.len() > depth {
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
                    branches,
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
                            });
                        } else {
                            jobs.push(Job::ForkBranch {
                                items,
                                index,
                                join,
                                branches,
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
                        self.pop_scope();
                        output.push(Stmt::Fork { join, branches });
                    }
                }
                Job::ForkBranch {
                    items,
                    index,
                    join,
                    mut branches,
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
                        jobs.push(Job::ForkNext {
                            items: items.clone(),
                            index: 0,
                            join,
                            branches: Vec::new(),
                        });
                    }
                    StatementKind::Block { items, .. } => {
                        self.push_scope();
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
                                None => (LoopKind::While, Some(self.native_value(*value)?), false),
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
                    StatementKind::Case {
                        keyword,
                        mode,
                        expression: Some(value),
                        ..
                    } if keyword != "randcase" => {
                        if mode.as_deref() == Some("matches") {
                            return Err(unsupported("pattern-matching case statement"));
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
                        let selector = self.native_value(*value)?;
                        jobs.push(Job::CaseNext(CaseState {
                            id,
                            index: 0,
                            kind,
                            selector,
                            items: Vec::new(),
                            default: None,
                        }));
                    }
                    StatementKind::Wait { condition, body } => {
                        self.allow_timing("wait statement")?;
                        let condition = self.native_value(*condition)?;
                        output.push(Stmt::Wait(condition));
                        jobs.push(Job::Enter(*body));
                    }
                    StatementKind::Timing { control, body } => {
                        if self.implicit_event_control(*control)? {
                            jobs.push(Job::ImplicitEvent {
                                start: output.len(),
                                sensitivity: self.sensitivity_body.len(),
                            });
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
                        let (condition, deferred, concurrent) = match header {
                            celox_sv_syntax::AssertionHeader::Immediate { deferred, .. } => (
                                self.native_assertion_condition(keyword, header)?,
                                deferred,
                                None,
                            ),
                            celox_sv_syntax::AssertionHeader::Concurrent {
                                condition,
                                disable,
                                module_item,
                                ..
                            } => {
                                if !module_item {
                                    return Err(unsupported("procedural concurrent assertion"));
                                }
                                if !matches!(keyword.as_str(), "assert" | "assume" | "cover") {
                                    return Err(unsupported("concurrent assertion directive"));
                                }
                                let operand = self.native_value(*condition)?;
                                assertions::concurrent_operand(&operand)?;
                                let condition = procedural_truth_condition(Expr::Call {
                                    callee: Callee::System(SystemFunction::Sampled),
                                    args: vec![Some(operand)],
                                    arg_order: Vec::new(),
                                });
                                let disable = disable
                                    .map(|id| self.native_value(id).map(procedural_truth_condition))
                                    .transpose()?;
                                if let Some(disable) = &disable {
                                    assertions::concurrent_operand(disable)?;
                                }
                                (condition, &None, Some(disable))
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
                        }
                        let value = value
                            .map(|id| {
                                let mut value = expression_binding::bind_value(
                                    id,
                                    native,
                                    self.tree,
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
                    if concurrent.is_some() {
                        assertions::concurrent_action(&then_body)?;
                        assertions::concurrent_action(&else_body)?;
                        // Evaluate every predicate in Observed; neither action can run
                        // until all the monitors have made that decision.
                        then_body.insert(0, Stmt::AssertionRegion { reactive: true });
                        else_body.insert(0, Stmt::AssertionRegion { reactive: true });
                        output.push(Stmt::AssertionRegion { reactive: false });
                    }
                    let decision = Stmt::If {
                        condition,
                        then_body,
                        else_body,
                    };
                    if let Some(Some(disable)) = concurrent {
                        output.push(Stmt::If {
                            condition: disable,
                            then_body: Vec::new(),
                            else_body: vec![decision],
                        });
                    } else {
                        output.push(decision);
                    }
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
                        let labels =
                            if arm.default {
                                None
                            } else {
                                Some(self.native_case_labels(
                                    &arm.matches,
                                    state.kind == CaseKind::Inside,
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
                    let condition = self.native_value(native)?;
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
                    self.pop_scope();
                    output.extend(Self::fold_foreach(loops, body));
                }
            }
        }
        Ok(output)
    }
    fn native_case_labels(
        &self,
        labels: &[celox_sv_syntax::CaseMatch],
        inside: bool,
    ) -> Result<Vec<CaseLabel<Expr>>, AnalyzerError> {
        if labels.is_empty() {
            return Err(AnalyzerError::Parse("native case arm has no labels".into()));
        }
        labels
            .iter()
            .map(|label| match label {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_scope_jobs_restore_bindings_after_a_typed_leaf_error() {
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
        assert_eq!(builder.scopes.len(), 1);
        for name in ["outer_value", "inner_value"] {
            assert!(builder.lookup(name).is_none());
            assert!(builder.dims.get(name).is_none());
            assert!(!builder.local_constants.contains_key(name));
        }
    }
}
