//! Symbolic value of a function output or return value.

use super::*;

impl<'a> FfParser<'a> {
    pub(super) fn extract_function_target_expr(
        &self,
        body: &veryl_analyzer::ir::FunctionBody,
        target_id: VarId,
        defs: &HashMap<VarId, Expression>,
    ) -> Result<Expression, ParserError> {
        fn merge_branch_state(
            parser: &FfParser,
            cond: &Expression,
            mut then_state: HashMap<VarId, Expression>,
            else_state: HashMap<VarId, Expression>,
        ) -> HashMap<VarId, Expression> {
            // A variable written on only one side keeps its uninitialized
            // value on the other.
            let mut merged = HashMap::default();
            let else_only: Vec<_> = else_state
                .keys()
                .filter(|id| !then_state.contains_key(id))
                .copied()
                .collect();
            for id in else_only {
                then_state.insert(id, parser.state_value_expr(id, &HashMap::default()));
            }
            for (id, then_expr) in then_state.drain() {
                let else_expr = parser.state_value_expr(id, &else_state);
                merged.insert(
                    id,
                    Expression::Ternary(
                        Box::new(FfParser::normalize_function_control_condition(cond.clone())),
                        Box::new(then_expr),
                        Box::new(else_expr),
                        Box::new(parser.function_state_merge_comptime(id)),
                    ),
                );
            }
            merged
        }

        fn build_state_from_statement(
            parser: &FfParser,
            stmt: &Statement,
            state: &HashMap<VarId, Expression>,
            substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
        ) -> Result<HashMap<VarId, Expression>, ParserError> {
            match stmt {
                Statement::Assign(assign) => {
                    let rhs = substitute(&assign.expr, state);
                    parser.assign_function_state(&assign.dst, rhs, state)
                }
                Statement::If(if_stmt) => {
                    let mut condition_state = state.clone();
                    let (cond, _) = parser.capture_nested_function_outputs_with_states(
                        &if_stmt.cond,
                        &mut condition_state,
                    )?;
                    let then_state = build_state_from_statements(
                        parser,
                        &if_stmt.true_side,
                        &condition_state,
                        substitute,
                    )?;
                    let else_state = build_state_from_statements(
                        parser,
                        &if_stmt.false_side,
                        &condition_state,
                        substitute,
                    )?;
                    let cond = substitute(&cond, &condition_state);
                    Ok(merge_branch_state(parser, &cond, then_state, else_state))
                }
                Statement::Case(case_stmt) => {
                    build_state_from_case(parser, case_stmt, 0, state, substitute)
                }
                Statement::Null => Ok(state.clone()),
                Statement::IfReset(ir) => Err(ParserError::illegal_context(
                    "statement in function body",
                    format!("{stmt}"),
                    Some(&ir.token),
                )),
                Statement::SystemFunctionCall(call) => {
                    let (_, next_state, _) = parser.prepare_system_function_call(call, state)?;
                    Ok(next_state)
                }
                Statement::FunctionCall(call) => {
                    parser.apply_statement_function_call_to_state(call, state)
                }
                Statement::For(f) => Err(ParserError::internal(
                    "for loop in function body (calls needing it are lowered inline)",
                    format!("{stmt}"),
                    Some(&f.token),
                )),
                Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                    Err(ParserError::illegal_context(
                        "statement in function body",
                        format!("{stmt}"),
                        None,
                    ))
                }
            }
        }

        fn build_state_from_case(
            parser: &FfParser,
            case_stmt: &CaseStatement,
            arm_index: usize,
            state: &HashMap<VarId, Expression>,
            substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
        ) -> Result<HashMap<VarId, Expression>, ParserError> {
            let Some(arm) = case_stmt.arms.get(arm_index) else {
                return build_state_from_statements(parser, &case_stmt.default, state, substitute);
            };

            let then_state = build_state_from_statements(parser, &arm.body, state, substitute)?;
            let else_state =
                build_state_from_case(parser, case_stmt, arm_index + 1, state, substitute)?;
            let cond = substitute(
                &case_arm_condition_expr(&case_stmt.case_target, &arm.patterns),
                state,
            );
            Ok(merge_branch_state(parser, &cond, then_state, else_state))
        }

        let body = crate::lowering::function_return::implicit_return_body(body);
        fn build_state_from_statements(
            parser: &FfParser,
            statements: &[Statement],
            initial: &HashMap<VarId, Expression>,
            substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
        ) -> Result<HashMap<VarId, Expression>, ParserError> {
            let mut state = initial.clone();
            for stmt in statements {
                state = build_state_from_statement(parser, stmt, &state, substitute)?;
            }
            Ok(state)
        }

        let state = build_state_from_statements(self, &body.statements, defs, &|expr, defs| {
            self.substitute_function_expr(expr, defs)
        })?;
        Ok(self.state_value_expr(target_id, &state))
    }

    pub(super) fn extract_function_return_expr(
        &self,
        body: &veryl_analyzer::ir::FunctionBody,
        ret_id: VarId,
    ) -> Result<Expression, ParserError> {
        let body = crate::lowering::function_return::implicit_return_body(body);
        fn resolve_return_expr(
            parser: &FfParser,
            statements: &[Statement],
            ret_id: VarId,
            defs: &HashMap<VarId, Expression>,
            substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
        ) -> Result<Option<Expression>, ParserError> {
            if statements.is_empty() {
                // Falling off the end returns the return variable's current
                // value, which is its uninitialized value if never assigned.
                return Ok(Some(parser.state_value_expr(ret_id, defs)));
            }

            let stmt = &statements[0];
            let rest = &statements[1..];

            match stmt {
                Statement::Assign(assign) => {
                    if let [dst] = assign.dst.as_slice()
                        && dst.id == ret_id
                        && dst.index.0.is_empty()
                        && dst.select.0.is_empty()
                        && dst.select.1.is_none()
                    {
                        // Assignment to the return variable corresponds to
                        // `return` and terminates this path.
                        let rhs = substitute(&assign.expr, defs);
                        return Ok(Some(parser.coerce_function_state_assignment(rhs, dst)?));
                    }
                    // A partial write to the return variable does not
                    // terminate the path; later writes may fill other bits.
                    let rhs = substitute(&assign.expr, defs);
                    let next_defs = parser.assign_function_state(&assign.dst, rhs, defs)?;
                    resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
                }
                Statement::If(if_stmt) => {
                    let mut condition_defs = defs.clone();
                    let (cond, _) = parser.capture_nested_function_outputs_with_states(
                        &if_stmt.cond,
                        &mut condition_defs,
                    )?;
                    let cond = substitute(&cond, &condition_defs);

                    let mut then_stmts = if_stmt.true_side.clone();
                    then_stmts.extend_from_slice(rest);
                    let then_expr = resolve_return_expr(
                        parser,
                        &then_stmts,
                        ret_id,
                        &condition_defs,
                        substitute,
                    )?;

                    let mut else_stmts = if_stmt.false_side.clone();
                    else_stmts.extend_from_slice(rest);
                    let else_expr = resolve_return_expr(
                        parser,
                        &else_stmts,
                        ret_id,
                        &condition_defs,
                        substitute,
                    )?;

                    match (then_expr, else_expr) {
                        (Some(then_expr), Some(else_expr)) => Ok(Some(Expression::Ternary(
                            Box::new(FfParser::normalize_function_control_condition(cond)),
                            Box::new(then_expr),
                            Box::new(else_expr),
                            Box::new(Comptime::create_unknown(TokenRange::default())),
                        ))),
                        _ => Ok(None),
                    }
                }
                Statement::Case(case_stmt) => {
                    resolve_return_expr_case(parser, case_stmt, rest, ret_id, defs, substitute)
                }
                Statement::Null => resolve_return_expr(parser, rest, ret_id, defs, substitute),
                Statement::IfReset(ir) => Err(ParserError::illegal_context(
                    "statement in function body",
                    format!("{stmt}"),
                    Some(&ir.token),
                )),
                Statement::SystemFunctionCall(call) => {
                    let (_, next_defs, _) = parser.prepare_system_function_call(call, defs)?;
                    resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
                }
                Statement::FunctionCall(call) => {
                    let next_defs = parser.apply_statement_function_call_to_state(call, defs)?;
                    resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
                }
                Statement::For(f) => Err(ParserError::internal(
                    "for loop in function body (calls needing it are lowered inline)",
                    format!("{stmt}"),
                    Some(&f.token),
                )),
                Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                    Err(ParserError::illegal_context(
                        "statement in function body",
                        format!("{stmt}"),
                        None,
                    ))
                }
            }
        }

        fn resolve_return_expr_case(
            parser: &FfParser,
            case_stmt: &CaseStatement,
            rest: &[Statement],
            ret_id: VarId,
            defs: &HashMap<VarId, Expression>,
            substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
        ) -> Result<Option<Expression>, ParserError> {
            fn resolve_from_arm(
                parser: &FfParser,
                case_stmt: &CaseStatement,
                arm_index: usize,
                rest: &[Statement],
                ret_id: VarId,
                defs: &HashMap<VarId, Expression>,
                substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
            ) -> Result<Option<Expression>, ParserError> {
                let Some(arm) = case_stmt.arms.get(arm_index) else {
                    let mut default_stmts = case_stmt.default.clone();
                    default_stmts.extend_from_slice(rest);
                    return resolve_return_expr(parser, &default_stmts, ret_id, defs, substitute);
                };

                let cond = substitute(
                    &case_arm_condition_expr(&case_stmt.case_target, &arm.patterns),
                    defs,
                );

                let mut then_stmts = arm.body.clone();
                then_stmts.extend_from_slice(rest);
                let then_expr = resolve_return_expr(parser, &then_stmts, ret_id, defs, substitute)?;
                let else_expr = resolve_from_arm(
                    parser,
                    case_stmt,
                    arm_index + 1,
                    rest,
                    ret_id,
                    defs,
                    substitute,
                )?;

                match (then_expr, else_expr) {
                    (Some(then_expr), Some(else_expr)) => Ok(Some(Expression::Ternary(
                        Box::new(FfParser::normalize_function_control_condition(cond)),
                        Box::new(then_expr),
                        Box::new(else_expr),
                        Box::new(Comptime::create_unknown(TokenRange::default())),
                    ))),
                    _ => Ok(None),
                }
            }

            resolve_from_arm(parser, case_stmt, 0, rest, ret_id, defs, substitute)
        }

        resolve_return_expr(
            self,
            &body.statements,
            ret_id,
            &HashMap::default(),
            &|expr, defs| self.substitute_function_expr(expr, defs),
        )?
        .ok_or_else(|| {
            ParserError::internal(
                "function return expression",
                format!("no return value resolved for function return var {ret_id:?}"),
                None,
            )
        })
    }
}
