//! Symbolic execution of function bodies: statements, nested calls and their outputs.

use super::*;

fn function_call_arg_mut<'a, T>(
    args: &'a mut [(veryl_analyzer::ir::VarPath, T)],
    path: &veryl_analyzer::ir::VarPath,
) -> Option<&'a mut T> {
    args.iter_mut()
        .find_map(|(candidate, value)| (candidate == path).then_some(value))
}

impl<'a> FfParser<'a> {
    pub(super) fn statement_is_function_return(
        statement: &Statement,
        ret_id: Option<VarId>,
    ) -> bool {
        let Some(ret_id) = ret_id else {
            return false;
        };
        let Statement::Assign(assign) = statement else {
            return false;
        };
        assign.dst.len() == 1
            && assign.dst[0].id == ret_id
            && assign.dst[0].index.0.is_empty()
            && assign.dst[0].select.0.is_empty()
            && assign.dst[0].select.1.is_none()
    }

    pub(super) fn capture_nested_function_outputs_with_states(
        &self,
        expr: &Expression,
        state: &mut HashMap<VarId, Expression>,
    ) -> Result<(Expression, HashMap<TokenRange, HashMap<VarId, Expression>>), ParserError> {
        let mut expression_states = HashMap::default();
        let expr =
            self.capture_nested_function_outputs_inner(expr, state, &mut expression_states)?;
        Ok((expr, expression_states))
    }

    pub(super) fn apply_statement_function_call_to_state(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let Some(function) = self.module.functions.get(&call.id) else {
            return Err(ParserError::internal(
                "function call",
                format!("unknown function id: {:?}", call.id),
                Some(&call.comptime.token),
            ));
        };

        let ordered_paths: Vec<_> = function
            .args
            .iter()
            .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path.clone()))
            .collect();
        let mut call = call.clone();
        let mut next = state.clone();
        for path in ordered_paths {
            if let Some(input) = function_call_arg_mut(&mut call.inputs, &path) {
                let input_state = next.clone();
                let captured = self
                    .capture_nested_function_outputs_with_states(input, &mut next)?
                    .0;
                *input = self.substitute_function_expr(&captured, &input_state);
            }
        }
        self.apply_function_call_to_state(&call, &next)
    }

    fn capture_nested_function_outputs_inner(
        &self,
        expr: &Expression,
        state: &mut HashMap<VarId, Expression>,
        expression_states: &mut HashMap<TokenRange, HashMap<VarId, Expression>>,
    ) -> Result<Expression, ParserError> {
        expression_states.insert(expr.token_range(), state.clone());
        Ok(match expr {
            Expression::Term(factor) => match factor.as_ref() {
                // Keep the formal access intact. Runtime-event lowering binds it
                // against the state captured for this argument, which both
                // preserves left-to-right snapshots and applies the formal's
                // declared width, signedness, and state kind.
                Factor::Variable(id, index, select, comptime) => {
                    let mut index = index.clone();
                    for expr in &mut index.0 {
                        *expr = self.capture_nested_function_outputs_inner(
                            expr,
                            state,
                            expression_states,
                        )?;
                    }
                    let mut select = select.clone();
                    for expr in &mut select.0 {
                        *expr = self.capture_nested_function_outputs_inner(
                            expr,
                            state,
                            expression_states,
                        )?;
                    }
                    if let Some((_, expr)) = &mut select.1 {
                        *expr = self.capture_nested_function_outputs_inner(
                            expr,
                            state,
                            expression_states,
                        )?;
                    }
                    Expression::Term(Box::new(Factor::Variable(
                        *id,
                        index,
                        select,
                        comptime.clone(),
                    )))
                }
                Factor::FunctionCall(call) => {
                    let mut call = call.clone();
                    let emits_nonlocal_runtime_write =
                        self.function_call_emits_nonlocal_runtime_write(&call);
                    let Some(function) = self.module.functions.get(&call.id) else {
                        return Err(ParserError::internal(
                            "function call",
                            format!("unknown function id: {:?}", call.id),
                            Some(&call.comptime.token),
                        ));
                    };
                    let ordered_paths: Vec<_> = function
                        .args
                        .iter()
                        .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path.clone()))
                        .collect();
                    for path in ordered_paths {
                        if let Some(input) = function_call_arg_mut(&mut call.inputs, &path) {
                            let input_state = state.clone();
                            let captured = self.capture_nested_function_outputs_inner(
                                input,
                                state,
                                expression_states,
                            )?;
                            *input = self.substitute_function_expr(&captured, &input_state);
                        }
                    }
                    let mut state_call = call.clone();
                    self.retain_direct_runtime_copyouts(&mut state_call, false);
                    let mut transitioned = self.apply_function_call_to_state(&state_call, state)?;
                    if emits_nonlocal_runtime_write {
                        self.mark_emitted_call_state(&state_call, &mut transitioned);
                    }
                    *state = transitioned;
                    self.retain_direct_runtime_copyouts(&mut call, true);
                    Expression::Term(Box::new(Factor::FunctionCall(call)))
                }
                Factor::SystemFunctionCall(call) => {
                    let (call, next, nested_states) =
                        self.prepare_system_function_call(call, state)?;
                    expression_states.extend(nested_states);
                    *state = next;
                    Expression::Term(Box::new(Factor::SystemFunctionCall(call)))
                }
                Factor::HierVariable(reference) => {
                    let mut reference = reference.as_ref().clone();
                    for expr in &mut reference.index.0 {
                        *expr = self.capture_nested_function_outputs_inner(
                            expr,
                            state,
                            expression_states,
                        )?;
                    }
                    for expr in &mut reference.select.0 {
                        *expr = self.capture_nested_function_outputs_inner(
                            expr,
                            state,
                            expression_states,
                        )?;
                    }
                    if let Some((_, expr)) = &mut reference.select.1 {
                        *expr = self.capture_nested_function_outputs_inner(
                            expr,
                            state,
                            expression_states,
                        )?;
                    }
                    Expression::Term(Box::new(Factor::HierVariable(Box::new(reference))))
                }
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => expr.clone(),
            },
            Expression::Binary(lhs, op, rhs, comptime) => {
                let lhs =
                    self.capture_nested_function_outputs_inner(lhs, state, expression_states)?;
                let base = state.clone();
                let mut rhs_state = base.clone();
                let rhs = self.capture_nested_function_outputs_inner(
                    rhs,
                    &mut rhs_state,
                    expression_states,
                )?;
                *state = match op {
                    Op::LogicAnd => {
                        let rhs_runs = Self::function_control_may_be_true(lhs.clone());
                        self.merge_expression_states(&rhs_runs, &base, &rhs_state, &base)
                    }
                    Op::LogicOr => self.merge_expression_states(&lhs, &base, &base, &rhs_state),
                    _ => rhs_state,
                };
                Expression::Binary(Box::new(lhs), *op, Box::new(rhs), comptime.clone())
            }
            Expression::Unary(op, inner, comptime) => Expression::Unary(
                *op,
                Box::new(self.capture_nested_function_outputs_inner(
                    inner,
                    state,
                    expression_states,
                )?),
                comptime.clone(),
            ),
            Expression::Ternary(condition, then_expr, else_expr, comptime) => {
                let condition = self.capture_nested_function_outputs_inner(
                    condition,
                    state,
                    expression_states,
                )?;
                let base = state.clone();
                let mut then_state = base.clone();
                let then_expr = self.capture_nested_function_outputs_inner(
                    then_expr,
                    &mut then_state,
                    expression_states,
                )?;
                // A true condition skips the else arm, a false condition starts
                // it from `base`, and an unknown condition evaluates it after
                // the then arm. The true-path entry is irrelevant because the
                // final merge discards the hypothetical else transition there.
                let then_may_run = Self::function_control_may_be_true(condition.clone());
                let mut else_state =
                    self.merge_expression_states(&then_may_run, &base, &then_state, &base);
                let else_expr = self.capture_nested_function_outputs_inner(
                    else_expr,
                    &mut else_state,
                    expression_states,
                )?;
                *state = self.merge_expression_states(&condition, &base, &then_state, &else_state);
                Expression::Ternary(
                    Box::new(condition),
                    Box::new(then_expr),
                    Box::new(else_expr),
                    comptime.clone(),
                )
            }
            Expression::Concatenation(parts, comptime) => {
                let mut rewritten = Vec::with_capacity(parts.len());
                for (expr, repeat) in parts {
                    let expr =
                        self.capture_nested_function_outputs_inner(expr, state, expression_states)?;
                    let repeat = repeat
                        .as_ref()
                        .map(|repeat| {
                            self.capture_nested_function_outputs_inner(
                                repeat,
                                state,
                                expression_states,
                            )
                        })
                        .transpose()?;
                    rewritten.push((expr, repeat));
                }
                Expression::Concatenation(rewritten, comptime.clone())
            }
            Expression::ArrayLiteral(items, comptime) => {
                let mut rewritten = Vec::with_capacity(items.len());
                for item in items {
                    rewritten.push(match item {
                        ArrayLiteralItem::Value(expr, repeat) => ArrayLiteralItem::Value(
                            Box::new(self.capture_nested_function_outputs_inner(
                                expr,
                                state,
                                expression_states,
                            )?),
                            repeat
                                .as_ref()
                                .map(|repeat| {
                                    self.capture_nested_function_outputs_inner(
                                        repeat,
                                        state,
                                        expression_states,
                                    )
                                })
                                .transpose()?
                                .map(Box::new),
                        ),
                        ArrayLiteralItem::Defaul(expr) => ArrayLiteralItem::Defaul(Box::new(
                            self.capture_nested_function_outputs_inner(
                                expr,
                                state,
                                expression_states,
                            )?,
                        )),
                    });
                }
                Expression::ArrayLiteral(rewritten, comptime.clone())
            }
            Expression::StructConstructor(ty, fields, comptime) => {
                let mut rewritten = Vec::with_capacity(fields.len());
                for (name, expr) in fields {
                    rewritten.push((
                        *name,
                        self.capture_nested_function_outputs_inner(expr, state, expression_states)?,
                    ));
                }
                Expression::StructConstructor(ty.clone(), rewritten, comptime.clone())
            }
        })
    }

    pub(super) fn prepare_system_function_call(
        &self,
        call: &SystemFunctionCall,
        initial: &HashMap<VarId, Expression>,
    ) -> Result<
        (
            SystemFunctionCall,
            HashMap<VarId, Expression>,
            HashMap<TokenRange, HashMap<VarId, Expression>>,
        ),
        ParserError,
    > {
        let mut call = call.clone();
        let mut state = initial.clone();
        let mut arg_states = HashMap::default();
        let capture = |parser: &Self,
                       input: &mut veryl_analyzer::ir::SystemFunctionInput,
                       state: &mut HashMap<VarId, Expression>,
                       arg_states: &mut HashMap<TokenRange, HashMap<VarId, Expression>>|
         -> Result<(), ParserError> {
            input.0 = parser.capture_nested_function_outputs_inner(&input.0, state, arg_states)?;
            Ok(())
        };
        match &mut call.kind {
            SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                for input in args {
                    capture(self, input, &mut state, &mut arg_states)?;
                }
            }
            SystemFunctionKind::Assert { cond, args, .. } => {
                capture(self, cond, &mut state, &mut arg_states)?;
                for input in args {
                    capture(self, input, &mut state, &mut arg_states)?;
                }
            }
            SystemFunctionKind::Readmemh(_, _) | SystemFunctionKind::Finish => {}
            SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => {}
            SystemFunctionKind::Clog2(input)
            | SystemFunctionKind::Onehot(input)
            | SystemFunctionKind::Signed(input)
            | SystemFunctionKind::Unsigned(input) => {
                capture(self, input, &mut state, &mut arg_states)?;
            }
        }
        Ok((call, state, arg_states))
    }

    fn apply_statements_to_function_state(
        &self,
        statements: &[Statement],
        initial: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let mut state = initial.clone();
        for statement in statements {
            state = self.apply_statement_to_function_state(statement, &state)?;
        }
        Ok(state)
    }

    pub(super) fn apply_statements_to_function_state_in_path(
        &self,
        statements: &[Statement],
        ret_id: Option<VarId>,
        initial: &HashMap<VarId, Expression>,
        mut active: FunctionPathCondition,
    ) -> Result<(HashMap<VarId, Expression>, FunctionPathCondition), ParserError> {
        let mut state = initial.clone();
        for statement in statements {
            if matches!(active, FunctionPathCondition::Never) {
                break;
            }
            match statement {
                Statement::Assign(assign) => {
                    let base = state.clone();
                    let mut transitioned_base = base.clone();
                    // State-only evaluation must still apply output bindings
                    // nested in an assignment RHS.
                    let (rhs, _) = self.capture_nested_function_outputs_with_states(
                        &assign.expr,
                        &mut transitioned_base,
                    )?;
                    let transitioned =
                        self.apply_assignment_to_function_state(assign, &rhs, &transitioned_base)?;
                    state = self.apply_state_transition_on_path(&active, &base, transitioned);
                    if Self::statement_is_function_return(statement, ret_id) {
                        active = FunctionPathCondition::Never;
                    }
                }
                Statement::If(statement) => {
                    let base = state.clone();
                    let mut condition_base = base.clone();
                    // State-only evaluation must still apply output bindings
                    // nested in the predicate before deriving either branch.
                    let (condition, _) = self.capture_nested_function_outputs_with_states(
                        &statement.cond,
                        &mut condition_base,
                    )?;
                    let condition = self.substitute_function_expr(&condition, &condition_base);
                    let condition_base =
                        self.apply_state_transition_on_path(&active, &base, condition_base);
                    let true_active = Self::function_path_and(active.clone(), condition.clone());
                    let false_active =
                        Self::function_path_and_not(active.clone(), condition.clone());
                    let (then_state, then_active) = self
                        .apply_statements_to_function_state_in_path(
                            &statement.true_side,
                            ret_id,
                            &condition_base,
                            true_active,
                        )?;
                    let (else_state, else_active) = self
                        .apply_statements_to_function_state_in_path(
                            &statement.false_side,
                            ret_id,
                            &condition_base,
                            false_active,
                        )?;
                    state = self.merge_expression_states(
                        &condition,
                        &condition_base,
                        &then_state,
                        &else_state,
                    );
                    active = Self::function_path_or(then_active, else_active);
                }
                Statement::Case(statement) => {
                    let control_base = state.clone();
                    let case_target =
                        self.substitute_function_expr(&statement.case_target, &control_base);
                    let mut evaluated_state = control_base.clone();
                    let (case_target, _) = self.capture_nested_function_outputs_with_states(
                        &case_target,
                        &mut evaluated_state,
                    )?;
                    state = self.apply_state_transition_on_path(
                        &active,
                        &control_base,
                        evaluated_state,
                    );
                    let case_base = state.clone();
                    let mut remaining = active.clone();
                    let mut arm_states = Vec::with_capacity(statement.arms.len());
                    let mut live_paths = FunctionPathCondition::Never;
                    for arm in &statement.arms {
                        let mut pattern_remaining = remaining.clone();
                        let mut arm_active = FunctionPathCondition::Never;
                        let mut arm_condition = None;
                        for pattern in &arm.patterns {
                            let mut pattern = pattern.clone();
                            let expressions: Vec<&mut Box<Expression>> = match &mut pattern {
                                CasePattern::Eq(expr) => vec![expr],
                                CasePattern::Range { lo, hi, .. } => vec![lo, hi],
                            };
                            for expr in expressions {
                                let pattern_base = state.clone();
                                let frozen = self.substitute_function_expr(expr, &pattern_base);
                                let mut evaluated_state = pattern_base.clone();
                                let (captured, _) = self
                                    .capture_nested_function_outputs_with_states(
                                        &frozen,
                                        &mut evaluated_state,
                                    )?;
                                **expr = captured;
                                state = self.apply_state_transition_on_path(
                                    &pattern_remaining,
                                    &pattern_base,
                                    evaluated_state,
                                );
                            }
                            let condition = case_arm_condition_expr(
                                &case_target,
                                std::slice::from_ref(&pattern),
                            );
                            let matched = Self::function_path_and(
                                pattern_remaining.clone(),
                                condition.clone(),
                            );
                            arm_condition = Some(match arm_condition {
                                None => condition.clone(),
                                Some(previous) => Expression::Binary(
                                    Box::new(previous),
                                    Op::LogicOr,
                                    Box::new(condition.clone()),
                                    Box::new(Comptime::create_unknown(TokenRange::default())),
                                ),
                            });
                            arm_active = Self::function_path_or(arm_active, matched);
                            pattern_remaining =
                                Self::function_path_and_not(pattern_remaining, condition);
                        }
                        let base = state.clone();
                        let (arm_state, arm_live) = self
                            .apply_statements_to_function_state_in_path(
                                &arm.body, ret_id, &base, arm_active,
                            )?;
                        arm_states.push((
                            arm_condition.expect("Case arm must have at least one pattern"),
                            arm_state,
                        ));
                        live_paths = Self::function_path_or(live_paths, arm_live);
                        remaining = pattern_remaining;
                    }
                    let base = state.clone();
                    let (mut merged, default_live) = self
                        .apply_statements_to_function_state_in_path(
                            &statement.default,
                            ret_id,
                            &base,
                            remaining,
                        )?;
                    for (condition, arm_state) in arm_states.into_iter().rev() {
                        merged = self
                            .merge_expression_states(&condition, &case_base, &arm_state, &merged);
                    }
                    state = merged;
                    active = Self::function_path_or(live_paths, default_live);
                }
                Statement::SystemFunctionCall(call) => {
                    let base = state.clone();
                    let (_, transitioned, _) = self.prepare_system_function_call(call, &base)?;
                    state = self.apply_state_transition_on_path(&active, &base, transitioned);
                }
                Statement::FunctionCall(call) => {
                    let base = state.clone();
                    let transitioned = self.apply_statement_function_call_to_state(call, &base)?;
                    state = self.apply_state_transition_on_path(&active, &base, transitioned);
                }
                Statement::Null => {}
                Statement::For(statement) => {
                    return Err(ParserError::internal(
                        "for loop in function body (calls needing it are lowered inline)",
                        "for loop".to_string(),
                        Some(&statement.token),
                    ));
                }
                Statement::IfReset(statement) => {
                    return Err(ParserError::illegal_context(
                        "statement in function body",
                        format!("{statement}"),
                        Some(&statement.token),
                    ));
                }
                Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                    return Err(ParserError::illegal_context(
                        "statement in function body",
                        format!("{statement}"),
                        None,
                    ));
                }
            }
        }
        Ok((state, active))
    }

    pub(super) fn apply_statement_to_function_state(
        &self,
        statement: &Statement,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        match statement {
            Statement::Assign(assign) => {
                self.apply_assignment_to_function_state(assign, &assign.expr, state)
            }
            Statement::If(statement) => {
                let mut condition_state = state.clone();
                let (condition, _) = self.capture_nested_function_outputs_with_states(
                    &statement.cond,
                    &mut condition_state,
                )?;
                let condition = self.substitute_function_expr(&condition, &condition_state);
                let then_state = self
                    .apply_statements_to_function_state(&statement.true_side, &condition_state)?;
                let else_state = self
                    .apply_statements_to_function_state(&statement.false_side, &condition_state)?;
                Ok(self.merge_expression_states(
                    &condition,
                    &condition_state,
                    &then_state,
                    &else_state,
                ))
            }
            Statement::Case(statement) => self.apply_case_to_function_state(statement, 0, state),
            Statement::FunctionCall(call) => {
                self.apply_statement_function_call_to_state(call, state)
            }
            Statement::SystemFunctionCall(call) => {
                let (_, next, _) = self.prepare_system_function_call(call, state)?;
                Ok(next)
            }
            Statement::Null => Ok(state.clone()),
            Statement::For(statement) => Err(ParserError::internal(
                "for loop before runtime effect in function body (calls needing it are lowered inline)",
                "for loop".to_string(),
                Some(&statement.token),
            )),
            Statement::IfReset(statement) => Err(ParserError::illegal_context(
                "if_reset in function body",
                format!("{statement}"),
                Some(&statement.token),
            )),
            Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                Err(ParserError::illegal_context(
                    "statement in function body",
                    format!("{statement}"),
                    None,
                ))
            }
        }
    }

    pub(super) fn apply_assignment_to_function_state(
        &self,
        assign: &AssignStatement,
        rhs: &Expression,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let rhs = self.substitute_function_expr(rhs, state);
        self.assign_function_state(&assign.dst, rhs, state)
    }

    fn apply_case_to_function_state(
        &self,
        statement: &CaseStatement,
        arm_index: usize,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let Some(arm) = statement.arms.get(arm_index) else {
            return self.apply_statements_to_function_state(&statement.default, state);
        };
        let condition = self.substitute_function_expr(
            &case_arm_condition_expr(&statement.case_target, &arm.patterns),
            state,
        );
        let then_state = self.apply_statements_to_function_state(&arm.body, state)?;
        let else_state = self.apply_case_to_function_state(statement, arm_index + 1, state)?;
        Ok(self.merge_expression_states(&condition, state, &then_state, &else_state))
    }

    pub(super) fn apply_function_call_to_state(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let Some(function) = self.module.functions.get(&call.id) else {
            return Err(ParserError::internal(
                "function call",
                format!("unknown function id: {:?}", call.id),
                Some(&call.comptime.token),
            ));
        };

        let Some(function_body) = (if let Some(index) = &call.index {
            function.get_function(index)
        } else {
            function.get_function(&[])
        }) else {
            return Err(ParserError::internal(
                "function call specialization",
                format!("{call}"),
                Some(&call.comptime.token),
            ));
        };

        let mut function_body =
            crate::lowering::function_return::implicit_return_body(&function_body).into_owned();
        crate::lowering::function_return::refresh_expression_tokens(&mut function_body);
        self.validate_function_call_bindings(call, &function_body)?;

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = function_call_arg(&call.inputs, arg_path) {
                let arg_expr = self.substitute_function_expr(arg_expr, state);
                bindings.insert(
                    *arg_id,
                    self.coerce_function_input_expression(arg_expr, *arg_id),
                );
            }
        }
        let nonlocal_ids: HashSet<VarId> = self
            .module
            .variables
            .iter()
            .filter_map(|(id, variable)| {
                (variable.affiliation != Affiliation::Function).then_some(*id)
            })
            .collect();
        let writes_nonlocal = self.statements_write_any(
            &function_body.statements,
            &nonlocal_ids,
            &mut HashSet::default(),
        );
        let function_state = if call.outputs.is_empty() && !writes_nonlocal {
            None
        } else {
            Some(
                self.apply_statements_to_function_state_in_path(
                    &function_body.statements,
                    function_body.ret,
                    &bindings,
                    FunctionPathCondition::Always,
                )?
                .0,
            )
        };

        // Function outputs are copied out together. Resolve every formal's
        // final value against the pre-call caller state before mutating any
        // actual destination, so the result is independent of map order and
        // overlapping input/output bindings observe their call-time values.
        let ordered_paths = function
            .args
            .iter()
            .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path));
        let mut output_values = Vec::with_capacity(call.outputs.len());
        for arg_path in ordered_paths {
            let Some(dsts) = function_call_arg(&call.outputs, arg_path) else {
                continue;
            };
            if dsts.is_empty() {
                continue;
            }
            let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                return Err(ParserError::internal(
                    "function call missing argument",
                    format!("{call}"),
                    Some(&call.comptime.token),
                ));
            };

            let expr = match function_state.as_ref() {
                Some(function_state) => self.state_value_expr(*arg_id, function_state),
                None => self.state_value_expr(*arg_id, &HashMap::default()),
            };
            let expr = self.substitute_function_expr(&expr, state);
            output_values.push((dsts, expr));
        }

        let mut next = state.clone();
        if let Some(function_state) = &function_state {
            for (&id, expr) in function_state {
                if nonlocal_ids.contains(&id) {
                    next.insert(id, self.substitute_function_expr(expr, state));
                }
            }
        }
        for (dsts, expr) in output_values {
            next = self.assign_function_state_with_selects(dsts, expr, state, &next)?;
        }

        Ok(next)
    }
}
