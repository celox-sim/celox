//! Statements without a body: assignments, expressions, event triggers and `disable`.
use super::*;

impl BodyBuilder<'_, '_, '_> {
    pub(super) fn reject_tagged_integral_target(
        &mut self,
        root: celox_sv_syntax::ExprId,
    ) -> Converted<()> {
        if expression_binding::targets_tagged_union(
            root,
            self.tree.declarations(),
            self.tree,
            self.session,
            &self.dims,
        )? {
            return Err(unsupported("integral update of a tagged union"));
        }
        Ok(())
    }

    /// The statements of `id` used as a statement, when it is an increment
    /// or decrement.
    pub(super) fn update_statement(
        &mut self,
        id: celox_sv_syntax::ExprId,
    ) -> Converted<Option<Vec<Stmt>>> {
        let celox_sv_syntax::ExprKind::Unary {
            operator,
            expression,
            ..
        } = &self.tree.declarations().expression(id).kind
        else {
            return Ok(None);
        };
        if !matches!(operator.as_str(), "++" | "--") {
            return Ok(None);
        }
        let increment = operator == "++";
        let mut stmts = self.effects(&[*expression])?;
        self.update(id, *expression, None, increment, &mut stmts)?;
        Ok(Some(stmts))
    }

    pub(super) fn native_statement_leaf(
        &mut self,
        statement: &celox_sv_syntax::StatementKind,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        use celox_sv_syntax::StatementKind;
        match statement {
            StatementKind::Assignment {
                left,
                operator,
                right,
                nonblocking,
                timing,
                timing_control,
                repeat_count,
            } => {
                if timing.is_some() {
                    return self.intra_assignment(
                        operator,
                        *nonblocking,
                        *left,
                        *right,
                        *timing_control,
                        *repeat_count,
                    );
                }
                if (operator == "<=") != *nonblocking {
                    return Err(AnalyzerError::Parse(
                        "native assignment operator mode disagrees".into(),
                    ));
                }
                let mut stmts = self.effects(&[*left, *right])?;
                if let Some(assignment) = self.stream_assignment(
                    if *nonblocking { "=" } else { operator },
                    *nonblocking,
                    *left,
                    *right,
                )? {
                    stmts.extend(assignment);
                    return Ok(stmts);
                }
                if let Some(assignment) =
                    self.aggregate_property_assignment(operator, *nonblocking, *left, *right)?
                {
                    stmts.extend(assignment);
                    return Ok(stmts);
                }
                stmts.extend(self.assignment_value(
                    if *nonblocking { "=" } else { operator },
                    *nonblocking,
                    (*left, *right),
                )?);
                Ok(stmts)
            }
            StatementKind::Expression(id) => {
                if let Some(stmts) = self.update_statement(*id)? {
                    return Ok(stmts);
                }
                if let Some(stmts) = self.ordering_with_statement(*id)? {
                    let mut effects = self.effects(&[*id])?;
                    effects.extend(stmts);
                    return Ok(effects);
                }
                let mut stmts = self.effects(&[*id])?;
                stmts.extend(self.subroutine_call_statement(*id)?);
                Ok(stmts)
            }
            StatementKind::Assertion { .. } => Err(unsupported("concurrent assertion")),
            StatementKind::WaitFork => {
                self.allow_timing("wait fork statement")?;
                Ok(vec![Stmt::WaitFork])
            }
            StatementKind::WaitOrder { .. } => Err(unsupported("wait order statement")),
            StatementKind::EventTrigger {
                nonblocking,
                control,
                target,
            } => {
                if control.is_some() {
                    return Err(unsupported("delayed nonblocking event trigger"));
                }
                if !events::is_handle(
                    self.tree,
                    self.session,
                    *target,
                    self.tree.declarations(),
                    &self.dims,
                ) {
                    return Err(unsupported(
                        "event trigger of a value that is not a named event",
                    ));
                }
                let event = self.native_value(*target)?;
                let op = if *nonblocking {
                    celox_design::ProcessOp::TriggerNonblocking
                } else {
                    celox_design::ProcessOp::Trigger
                };
                Ok(vec![Stmt::Eval(events::call(op, vec![event]))])
            }
            StatementKind::ProceduralAssign(_) | StatementKind::ProceduralRelease(_) => {
                Err(unsupported("procedural continuous assignment"))
            }
            StatementKind::Disable(tokens) => {
                let mut words = self
                    .tree
                    .declarations()
                    .tokens
                    .get(tokens.start..tokens.end)
                    .into_iter()
                    .flatten()
                    .filter(|token| !token.is_trivia())
                    .map(|token| token.text.as_str());
                // The target follows the `disable` keyword: `fork`, or a
                // block, task or hierarchical name (9.6.2). Hierarchical
                // and task targets stay unsupported.
                let target = words.by_ref().find(|word| *word != "disable");
                let hierarchical = matches!(words.next(), Some("." | "::"));
                match target {
                    Some("fork") if !hierarchical => Ok(vec![Stmt::DisableFork]),
                    Some(name) if !hierarchical => {
                        let name = instances::normalize_identifier(name);
                        if let Some((_, id, targeted)) = self
                            .named_blocks
                            .iter_mut()
                            .rev()
                            .find(|(label, _, _)| *label == name)
                        {
                            // Killing a sibling fork branch by this name
                            // while also inside a block of it is resolved
                            // for the enclosing block first.
                            *targeted = true;
                            Ok(vec![Stmt::DisableBlock(*id)])
                        } else if let Some((_, handle, armed, targeted)) = self
                            .fork_blocks
                            .iter_mut()
                            .rev()
                            .find(|(label, _, _, _)| *label == name)
                        {
                            *targeted = true;
                            let (handle, armed) = (handle.clone(), armed.clone());
                            Ok(vec![Stmt::If {
                                condition: Expr::Ident(armed.clone()),
                                then_body: vec![
                                    Stmt::Eval(events::call(
                                        celox_design::ProcessOp::Kill,
                                        vec![Expr::Ident(handle)],
                                    )),
                                    Stmt::Assign {
                                        lhs: LValue::Ident(armed),
                                        rhs: Expr::Literal("1'b0".into()),
                                        nonblocking: false,
                                    },
                                ],
                                else_body: Vec::new(),
                            }])
                        } else {
                            Err(unsupported("disable statement"))
                        }
                    }
                    _ => Err(unsupported("disable statement")),
                }
            }
            StatementKind::Parallel { .. } => Err(unsupported("fork-join block")),
            _ => Err(unsupported("procedural statement")),
        }
    }

    /// Timing controls suspend the process that executes them: an
    /// `initial` or `always` process. An `always_comb` or `always_ff` body
    /// and a function may not suspend. A task suspends its caller.
    pub(super) fn allow_timing(&self, construct: &'static str) -> Result<(), AnalyzerError> {
        // A delayed nonblocking assignment's delay does not suspend its
        // process (IEEE 1800-2023 9.4.5, 10.4.2).
        if self.delayed_nonblocking && construct == "delay control" {
            return Ok(());
        }
        match self.body {
            system_functions::Body::Initial | system_functions::Body::Process => Ok(()),
            system_functions::Body::Final => Err(AnalyzerError::TimingControlOutsideProcess {
                construct,
                context: "a final procedure",
            }),
            system_functions::Body::Always => Err(AnalyzerError::TimingControlOutsideProcess {
                construct,
                context: "an always_comb or always_ff body",
            }),
            system_functions::Body::Subroutine if self.task_timing => Ok(()),
            // IEEE 1800-2023 13.4: a function shall not contain a timing
            // control.
            system_functions::Body::Subroutine => Err(AnalyzerError::TimingControlOutsideProcess {
                construct,
                context: "a function",
            }),
        }
    }
}
