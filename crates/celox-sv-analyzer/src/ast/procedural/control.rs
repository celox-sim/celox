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
        step: Vec<Stmt>,
        scope: bool,
        start: usize,
    },
    DoWhile {
        native: celox_sv_syntax::ExprId,
        start: usize,
    },
    Foreach {
        loops: Vec<(String, i128, i128)>,
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
        condition: Expr,
        keyword: String,
        assertion: celox_design::AssertionSite,
        span: Option<crate::procedural::SourceSpan>,
        failure: Option<StatementId>,
        start: usize,
    },
    AssertionFinish {
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
                Job::Enter(id) => match &native.statement(id).kind {
                    StatementKind::Empty => {}
                    StatementKind::Declaration(declaration) => {
                        output.extend(self.block_item_declaration(declaration)?)
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
                        let (kind, condition, scope) = match kind {
                            LoopSyntax::While(value) => {
                                (LoopKind::While, Some(self.native_value(*value)?), false)
                            }
                            LoopSyntax::Repeat(value) => {
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
                                let (init, condition, step) = self.for_header(header)?;
                                jobs.push(Job::Loop {
                                    kind: LoopKind::For,
                                    init,
                                    condition,
                                    step,
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
                        header: header @ celox_sv_syntax::AssertionHeader::Immediate { .. },
                        success,
                        failure,
                    } => {
                        let condition = self.native_assertion_condition(keyword, header)?;
                        let celox_sv_syntax::AssertionHeader::Immediate { deferred, .. } = header
                        else {
                            unreachable!("matched an immediate header");
                        };
                        let statement = native.statement(id);
                        jobs.push(Job::AssertionThen {
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
                    condition,
                    keyword,
                    assertion,
                    span,
                    failure,
                    start,
                } => {
                    let then_body = output.split_off(start);
                    jobs.push(Job::AssertionFinish {
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
                    output.push(Stmt::If {
                        condition,
                        then_body,
                        else_body,
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
                    step,
                    scope,
                    start,
                } => {
                    let body = output.split_off(start);
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
                    let body = output.split_off(start);
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
