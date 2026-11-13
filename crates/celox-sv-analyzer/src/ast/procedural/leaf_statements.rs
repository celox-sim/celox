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

    /// `assign target = value;` in a procedure (IEEE 1800-2023 10.6.1), kept
    /// as one statement for `procedural_assigns::lower`.
    pub(super) fn procedural_assign(
        &mut self,
        id: celox_sv_syntax::AssignId,
    ) -> Converted<Vec<Stmt>> {
        let assign = self.tree.declarations().assign_statement(id);
        // A procedural continuous assignment is one `variable_assignment`
        // without a strength or a delay (A.6.2).
        let [assignment] = assign.assignments.as_slice() else {
            return Err(AnalyzerError::Parse(
                "a procedural continuous assignment assigns one variable".into(),
            ));
        };
        if assign.strength.is_some() || assign.delay.is_some() {
            return Err(AnalyzerError::Parse(
                "a procedural continuous assignment has no drive strength or delay".into(),
            ));
        }
        let (left, right) = (assignment.left, assignment.right);
        self.procedural_assign_target(left)?;
        if !self.effects(&[left, right])?.is_empty() {
            return Err(unsupported(
                "procedural continuous assignment of an expression with side effects",
            ));
        }
        let stmts = self.assignment_value("=", false, (left, right))?;
        let [
            Stmt::Assign {
                lhs: lhs @ LValue::Ident(_),
                rhs,
                nonblocking: false,
            },
        ] = stmts.as_slice()
        else {
            return Err(unsupported(
                "procedural continuous assignment to a variable that is not a packed integral variable",
            ));
        };
        // The driver waits on the expression, which no call may be part of
        // but one that only reads a string, an object or a container.
        if inlining::effectful(rhs) {
            return Err(unsupported(
                "procedural continuous assignment of an expression that calls a subroutine or a system function",
            ));
        }
        // The value follows the variables of the expression after the
        // statement has executed, which a variable of a block or of a
        // subroutine call does not outlive; an automatic one is illegal
        // here (6.21).
        let mut local = false;
        procedural_assigns::reads(rhs, &mut |name| local |= self.is_local(name));
        if local {
            return Err(unsupported(
                "procedural continuous assignment that reads a variable of a procedural block",
            ));
        }
        Ok(vec![Stmt::ProceduralAssign {
            lhs: lhs.clone(),
            rhs: rhs.clone(),
        }])
    }

    /// The variable `assign` or `deassign` names: a whole variable of the
    /// module (IEEE 1800-2023 10.6.1).
    pub(super) fn procedural_assign_target(
        &mut self,
        target: celox_sv_syntax::ExprId,
    ) -> Converted<LValue> {
        // An `always` procedure with one runs as a process (see
        // `validation::always_kind`), whose body is a process body. A
        // detached fork converts its branches as initial bodies, also in a
        // subroutine, whose assigns stay unsupported there.
        if !matches!(
            self.body,
            system_functions::Body::Initial | system_functions::Body::Process
        ) || self.subroutine
        {
            return Err(unsupported(
                "procedural continuous assignment in a subroutine, a final procedure, or an `always_comb`, `always_latch` or `always_ff` procedure",
            ));
        }
        let native = self.tree.declarations();
        let id = expression_binding::ungroup(target, native);
        match &native.expression(id).kind {
            // A bit-select or part-select of a variable is not a target (10.6.1).
            celox_sv_syntax::ExprKind::Select { .. } => {
                return Err(AnalyzerError::Parse(
                    "a procedural continuous assignment assigns a whole variable, not a bit-select or part-select".into(),
                ));
            }
            celox_sv_syntax::ExprKind::Concatenation(_) => {
                return Err(unsupported(
                    "procedural continuous assignment to a concatenation",
                ));
            }
            _ => {}
        }
        let (_, renamed) = self.native_lvalue(target, true)?;
        let LValue::Ident(name) = &renamed else {
            return Err(unsupported(
                "procedural continuous assignment to a member or an element of a variable",
            ));
        };
        if self.is_local(name) {
            return Err(unsupported(
                "procedural continuous assignment to a variable of a procedural block",
            ));
        }
        Ok(renamed)
    }

    /// `force target = value;` (IEEE 1800-2023 10.6.2), kept as one
    /// statement for `procedural_forces::lower`.
    pub(super) fn procedural_force(
        &mut self,
        id: celox_sv_syntax::AssignId,
    ) -> Converted<Vec<Stmt>> {
        let assign = self.tree.declarations().assign_statement(id);
        // A force is one `variable_assignment` or `net_assignment`
        // without a strength or a delay (A.6.2).
        let [assignment] = assign.assignments.as_slice() else {
            return Err(AnalyzerError::Parse(
                "a force assigns one variable or net".into(),
            ));
        };
        if assign.strength.is_some() || assign.delay.is_some() {
            return Err(AnalyzerError::Parse(
                "a force has no drive strength or delay".into(),
            ));
        }
        let (left, right) = (assignment.left, assignment.right);
        let lhs = self.force_target(left)?;
        if !self.effects(&[left, right])?.is_empty() {
            return Err(unsupported("force of an expression with side effects"));
        }
        // The value converts to the target's type as in an assignment.
        let stmts = self.assignment_value("=", false, (left, right))?;
        let [
            Stmt::Assign {
                lhs: assigned,
                rhs,
                nonblocking: false,
            },
        ] = stmts.as_slice()
        else {
            return Err(unsupported(
                "force of a variable or net that is not of a packed integral type",
            ));
        };
        if assigned.name() != lhs.name() {
            return Err(unsupported(
                "force of a variable or net that is not of a packed integral type",
            ));
        }
        let rhs = rhs.clone();
        // The forced value follows the variables of the expression, which
        // no call may be part of but one that only reads a string, an
        // object or a container, or that draws a random number with
        // constant arguments: the statement draws it, and so does each
        // evaluation of the expression when one of its variables changes.
        let mut calls = 0usize;
        let mut randoms = 0usize;
        lifetimes::visit_calls(&rhs, &mut |callee, args| {
            calls += 1;
            let constant = args.iter().flatten().all(|arg| {
                let mut reads = false;
                procedural_assigns::reads(arg, &mut |_| reads = true);
                !reads
            });
            if constant
                && matches!(
                    callee,
                    Callee::System(SystemFunction::Urandom | SystemFunction::UrandomRange)
                )
            {
                randoms += 1;
            }
        });
        if (calls == 0 || calls != randoms) && inlining::effectful(&rhs) {
            return Err(unsupported(
                "force of an expression that calls a subroutine or a system function",
            ));
        }
        // The value follows the variables of the expression after the
        // statement has executed, which a variable of a block or of a
        // subroutine call does not outlive; an automatic one is illegal
        // here (6.21).
        // A static variable of a task, such as its argument, is read when
        // the force executes (see `procedural_forces`).
        let mut local = false;
        procedural_assigns::reads(&rhs, &mut |name| local |= self.is_local(name));
        if local && !(self.subroutine && self.task_timing && !self.automatic) {
            return Err(unsupported(
                "force of an expression that reads a variable of a procedural block or of an automatic task",
            ));
        }
        Ok(vec![Stmt::Force { lhs, rhs }])
    }

    /// The variable or net `force` or `release` names: a whole variable or
    /// net of the module (IEEE 1800-2023 10.6.2). Constant selects of a
    /// vector net, members, and names in other modules are not supported.
    pub(super) fn force_target(&mut self, target: celox_sv_syntax::ExprId) -> Converted<LValue> {
        // An `always` procedure with one runs as a process (see
        // `validation::always_kind`), whose body is a process body.
        // A task may force too: the statements of its calls execute in the
        // processes that call it.
        if !(matches!(
            self.body,
            system_functions::Body::Initial | system_functions::Body::Process
        ) && !self.subroutine
            || self.subroutine && self.task_timing)
        {
            return Err(unsupported(
                "force or release in a function, a final procedure, or an `always_comb`, `always_latch` or `always_ff` procedure",
            ));
        }
        let native = self.tree.declarations();
        let id = expression_binding::ungroup(target, native);
        if let celox_sv_syntax::ExprKind::Concatenation(_) = &native.expression(id).kind {
            return Err(unsupported("force or release of a concatenation"));
        }
        let (_, renamed) = self.native_lvalue(target, false)?;
        // A constant bit-select or part-select of a vector net, and as an
        // extension a constant select or a member of a variable, and an
        // element of an unpacked array at constant indices or a constant
        // select of one, names a constant range of bits of the target
        // (10.6.2), whose elements are laid out one after the other.
        let renamed = match renamed {
            LValue::Ident(_) => renamed,
            LValue::Select {
                array_slice_width: Some(_),
                ..
            } => {
                return Err(unsupported(
                    "force or release of a slice of an unpacked array",
                ));
            }
            LValue::Select {
                name,
                msb,
                lsb,
                signed,
                array_slice_width: None,
                array_slice_reversed: false,
                is_2state,
            } => {
                let bound = |bound: &ConstExpr| {
                    eval_ast_const_expr(&self.tree.functions, bound, &self.dims.const_env)
                };
                let (Some(msb), Some(lsb)) = (bound(&msb), bound(&lsb)) else {
                    return Err(unsupported(
                        "force or release of a select whose bounds are not constant",
                    ));
                };
                LValue::Select {
                    name,
                    msb: ConstExpr::Literal(msb.to_string()),
                    lsb: ConstExpr::Literal(lsb.to_string()),
                    signed,
                    array_slice_width: None,
                    array_slice_reversed: false,
                    is_2state,
                }
            }
            _ => {
                return Err(unsupported(
                    "force or release of a property or an element of an object",
                ));
            }
        };
        if self.is_local(renamed.name()) {
            return Err(unsupported(
                "force or release of a variable of a procedural block",
            ));
        }
        Ok(renamed)
    }

    /// Whether the assertions of this body check the assertion control
    /// state of the design (see [`assertion_control`]): those of a module of
    /// a design that calls an assertion control task. A constant function
    /// has no state to read.
    pub(super) fn controls_assertions(&self) -> bool {
        self.session.controls_assertions
            && self.tree.package.is_none()
            && !self.state.constant_evaluation
            && !self.dims.constant_function_bodies
    }

    /// Whether `name` is a variable a procedural block or a subroutine
    /// declares, by its module-unique name.
    pub(super) fn is_local(&self, name: &str) -> bool {
        self.state.locals.iter().any(|local| local.name == name)
            || self.state.statics.iter().any(|(local, _)| local == name)
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
            StatementKind::ProceduralAssign(id) => self.procedural_assign(*id),
            StatementKind::ProceduralDeassign(target) => {
                let target = self.procedural_assign_target(*target)?;
                Ok(vec![Stmt::Deassign { lhs: target }])
            }
            StatementKind::ProceduralForce(id) => self.procedural_force(*id),
            StatementKind::ProceduralRelease(target) => {
                let target = self.force_target(*target)?;
                Ok(vec![Stmt::Release { lhs: target }])
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
