use super::{Domain, FfParser};
use crate::{
    HashMap, HashSet, LoweringPhase, ParserError,
    bitaccess::{build_partial_assign_expr, is_static_access},
    case::case_arm_condition_expr,
    resolve_total_width,
};
use celox_design::VarAtomBase;
use celox_sir::{
    RegisterId, RegisterType, SIRBuilder, SIRInstruction, SIRTerminator, SIRValue, UnaryOp,
};
use num_traits::ToPrimitive;
use veryl_analyzer::ir::{
    ArrayLiteralItem, CasePattern, CaseStatement, Comptime, Expression, Factor, Op, Shape,
    Statement, SystemFunctionCall, SystemFunctionKind, Type, TypeKind, ValueVariant, VarId,
    VarIndex, VarSelect,
};
use veryl_parser::token_range::TokenRange;

#[derive(Clone)]
enum FunctionPathCondition {
    Always,
    Never,
    Conditional(Box<Expression>),
}

impl<'a> FfParser<'a> {
    pub(super) fn expression_has_runtime_effect(&self, expr: &Expression) -> bool {
        self.expression_has_runtime_effect_inner(expr, &mut HashSet::default())
    }

    fn expression_needs_eager_evaluation(&self, expr: &Expression) -> bool {
        super::expression::expression_has_side_effect(expr)
            || self.expression_has_runtime_effect(expr)
    }

    fn expression_has_runtime_effect_inner(
        &self,
        expr: &Expression,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(_, index, select, _) => {
                    index
                        .0
                        .iter()
                        .any(|expr| self.expression_has_runtime_effect_inner(expr, visiting))
                        || select
                            .0
                            .iter()
                            .any(|expr| self.expression_has_runtime_effect_inner(expr, visiting))
                        || select.1.as_ref().is_some_and(|(_, expr)| {
                            self.expression_has_runtime_effect_inner(expr, visiting)
                        })
                }
                Factor::FunctionCall(call) => {
                    call.inputs
                        .values()
                        .any(|expr| self.expression_has_runtime_effect_inner(expr, visiting))
                        || self.function_call_has_runtime_effect(call, visiting)
                }
                Factor::SystemFunctionCall(call) => match &call.kind {
                    veryl_analyzer::ir::SystemFunctionKind::Bits(_)
                    | veryl_analyzer::ir::SystemFunctionKind::Size(_) => false,
                    veryl_analyzer::ir::SystemFunctionKind::Clog2(input)
                    | veryl_analyzer::ir::SystemFunctionKind::Onehot(input)
                    | veryl_analyzer::ir::SystemFunctionKind::Signed(input)
                    | veryl_analyzer::ir::SystemFunctionKind::Unsigned(input) => {
                        self.expression_has_runtime_effect_inner(&input.0, visiting)
                    }
                    _ => true,
                },
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => false,
            },
            Expression::Binary(lhs, _, rhs, _) => {
                self.expression_has_runtime_effect_inner(lhs, visiting)
                    || self.expression_has_runtime_effect_inner(rhs, visiting)
            }
            Expression::Unary(_, inner, _) => {
                self.expression_has_runtime_effect_inner(inner, visiting)
            }
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                self.expression_has_runtime_effect_inner(cond, visiting)
                    || self.expression_has_runtime_effect_inner(then_expr, visiting)
                    || self.expression_has_runtime_effect_inner(else_expr, visiting)
            }
            Expression::Concatenation(items, _) => items.iter().any(|(expr, repeat)| {
                self.expression_has_runtime_effect_inner(expr, visiting)
                    || repeat.as_ref().is_some_and(|repeat| {
                        self.expression_has_runtime_effect_inner(repeat, visiting)
                    })
            }),
            Expression::ArrayLiteral(items, _) => items.iter().any(|item| match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    self.expression_has_runtime_effect_inner(expr, visiting)
                        || repeat.as_ref().is_some_and(|repeat| {
                            self.expression_has_runtime_effect_inner(repeat, visiting)
                        })
                }
                ArrayLiteralItem::Defaul(expr) => {
                    self.expression_has_runtime_effect_inner(expr, visiting)
                }
            }),
            Expression::StructConstructor(_, fields, _) => fields
                .iter()
                .any(|(_, expr)| self.expression_has_runtime_effect_inner(expr, visiting)),
        }
    }

    fn function_call_has_runtime_effect(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        if !visiting.insert(call.id) {
            return false;
        }
        let result = self
            .module
            .functions
            .get(&call.id)
            .and_then(|function| {
                if let Some(index) = &call.index {
                    function.get_function(index)
                } else {
                    function.get_function(&[])
                }
            })
            .is_some_and(|body| self.statements_have_runtime_effect(&body.statements, visiting));
        visiting.remove(&call.id);
        result
    }

    fn statements_have_runtime_effect(
        &self,
        statements: &[Statement],
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        statements.iter().any(|statement| match statement {
            Statement::SystemFunctionCall(_) => true,
            Statement::Assign(assign) => {
                self.expression_has_runtime_effect_inner(&assign.expr, visiting)
            }
            Statement::If(statement) => {
                self.expression_has_runtime_effect_inner(&statement.cond, visiting)
                    || self.statements_have_runtime_effect(&statement.true_side, visiting)
                    || self.statements_have_runtime_effect(&statement.false_side, visiting)
            }
            Statement::Case(statement) => {
                let pattern_effect = statement.arms.iter().any(|arm| {
                    arm.patterns.iter().any(|pattern| match pattern {
                        CasePattern::Eq(expr) => {
                            self.expression_has_runtime_effect_inner(expr, visiting)
                        }
                        CasePattern::Range { lo, hi, .. } => {
                            self.expression_has_runtime_effect_inner(lo, visiting)
                                || self.expression_has_runtime_effect_inner(hi, visiting)
                        }
                    })
                });
                self.expression_has_runtime_effect_inner(&statement.case_target, visiting)
                    || pattern_effect
                    || statement
                        .arms
                        .iter()
                        .any(|arm| self.statements_have_runtime_effect(&arm.body, visiting))
                    || self.statements_have_runtime_effect(&statement.default, visiting)
            }
            Statement::For(statement) => {
                self.statements_have_runtime_effect(&statement.body, visiting)
            }
            Statement::FunctionCall(call) => {
                call.inputs
                    .values()
                    .any(|expr| self.expression_has_runtime_effect_inner(expr, visiting))
                    || self.function_call_has_runtime_effect(call, visiting)
            }
            Statement::IfReset(statement) => {
                self.statements_have_runtime_effect(&statement.true_side, visiting)
                    || self.statements_have_runtime_effect(&statement.false_side, visiting)
            }
            Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => false,
        })
    }

    fn statement_is_function_return(statement: &Statement, ret_id: Option<VarId>) -> bool {
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

    fn state_value_expr(id: VarId, state: &HashMap<VarId, Expression>) -> Expression {
        state.get(&id).cloned().unwrap_or_else(|| {
            Expression::Term(Box::new(Factor::Variable(
                id,
                VarIndex::default(),
                VarSelect::default(),
                Comptime::create_unknown(TokenRange::default()),
            )))
        })
    }

    fn merge_expression_states(
        condition: &Expression,
        base: &HashMap<VarId, Expression>,
        then_state: &HashMap<VarId, Expression>,
        else_state: &HashMap<VarId, Expression>,
    ) -> HashMap<VarId, Expression> {
        let ids: HashSet<VarId> = base
            .keys()
            .chain(then_state.keys())
            .chain(else_state.keys())
            .copied()
            .collect();
        ids.into_iter()
            .map(|id| {
                let then_expr = then_state
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| Self::state_value_expr(id, base));
                let else_expr = else_state
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| Self::state_value_expr(id, base));
                (
                    id,
                    Expression::Ternary(
                        Box::new(Self::normalize_function_control_condition(
                            condition.clone(),
                        )),
                        Box::new(then_expr),
                        Box::new(else_expr),
                        Box::new(Comptime::create_unknown(TokenRange::default())),
                    ),
                )
            })
            .collect()
    }

    fn capture_nested_function_outputs_with_states(
        &self,
        expr: &Expression,
        state: &mut HashMap<VarId, Expression>,
    ) -> Result<(Expression, HashMap<TokenRange, HashMap<VarId, Expression>>), ParserError> {
        let mut expression_states = HashMap::default();
        let expr =
            self.capture_nested_function_outputs_inner(expr, state, &mut expression_states)?;
        Ok((expr, expression_states))
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
                Factor::Variable(_, _, _, _) => expr.clone(),
                Factor::FunctionCall(call) => {
                    let mut call = call.clone();
                    for input in call.inputs.values_mut() {
                        *input = self.capture_nested_function_outputs_inner(
                            input,
                            state,
                            expression_states,
                        )?;
                    }
                    *state = self.apply_function_call_to_state(&call, state)?;
                    call.outputs.clear();
                    Expression::Term(Box::new(Factor::FunctionCall(call)))
                }
                Factor::SystemFunctionCall(call) => {
                    let (call, next, nested_states) =
                        self.prepare_system_function_call(call, state)?;
                    expression_states.extend(nested_states);
                    *state = next;
                    Expression::Term(Box::new(Factor::SystemFunctionCall(call)))
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
                    Op::LogicAnd => Self::merge_expression_states(&lhs, &base, &rhs_state, &base),
                    Op::LogicOr => Self::merge_expression_states(&lhs, &base, &base, &rhs_state),
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
                let mut else_state = base.clone();
                let else_expr = self.capture_nested_function_outputs_inner(
                    else_expr,
                    &mut else_state,
                    expression_states,
                )?;
                *state = Self::merge_expression_states(&condition, &base, &then_state, &else_state);
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

    fn prepare_system_function_call(
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
            SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_) => {}
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

    fn apply_statement_to_function_state(
        &self,
        statement: &Statement,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        match statement {
            Statement::Assign(assign) => {
                if assign.dst.len() != 1 {
                    return Err(ParserError::unsupported(
                        43,
                        LoweringPhase::FfLowering,
                        "function body assignment shape",
                        format!("{statement}"),
                        Some(&assign.token),
                    ));
                }
                let dst = &assign.dst[0];
                let rhs = Self::substitute_function_expr(&assign.expr, state);
                let mut next = state.clone();
                let is_whole_var =
                    dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();
                if is_whole_var {
                    next.insert(dst.id, rhs);
                } else if is_static_access(&dst.index, &dst.select) {
                    let old_value = Self::state_value_expr(dst.id, state);
                    next.insert(
                        dst.id,
                        build_partial_assign_expr(self.module, dst, rhs, old_value)?,
                    );
                } else {
                    return Err(ParserError::unsupported(
                        66,
                        LoweringPhase::FfLowering,
                        "dynamic assignment before runtime effect in function body",
                        format!("{statement}"),
                        Some(&assign.token),
                    ));
                }
                Ok(next)
            }
            Statement::If(statement) => {
                let condition = Self::substitute_function_expr(&statement.cond, state);
                let then_state =
                    self.apply_statements_to_function_state(&statement.true_side, state)?;
                let else_state =
                    self.apply_statements_to_function_state(&statement.false_side, state)?;
                Ok(Self::merge_expression_states(
                    &condition,
                    state,
                    &then_state,
                    &else_state,
                ))
            }
            Statement::Case(statement) => self.apply_case_to_function_state(statement, 0, state),
            Statement::FunctionCall(call) => self.apply_function_call_to_state(call, state),
            Statement::SystemFunctionCall(call) => {
                let (_, next, _) = self.prepare_system_function_call(call, state)?;
                Ok(next)
            }
            Statement::Null => Ok(state.clone()),
            Statement::For(statement) => Err(ParserError::unsupported(
                66,
                LoweringPhase::FfLowering,
                "for loop before runtime effect in function body",
                "for loop".to_string(),
                Some(&statement.token),
            )),
            Statement::IfReset(statement) => Err(ParserError::unsupported(
                66,
                LoweringPhase::FfLowering,
                "if_reset before runtime effect in function body",
                format!("{statement}"),
                Some(&statement.token),
            )),
            Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                Err(ParserError::unsupported(
                    66,
                    LoweringPhase::FfLowering,
                    "statement before runtime effect in function body",
                    format!("{statement}"),
                    None,
                ))
            }
        }
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
        let condition = Self::substitute_function_expr(
            &case_arm_condition_expr(&statement.case_target, &arm.patterns),
            state,
        );
        let then_state = self.apply_statements_to_function_state(&arm.body, state)?;
        let else_state = self.apply_case_to_function_state(statement, arm_index + 1, state)?;
        Ok(Self::merge_expression_states(
            &condition,
            state,
            &then_state,
            &else_state,
        ))
    }

    fn function_path_and(
        path: FunctionPathCondition,
        condition: Expression,
    ) -> FunctionPathCondition {
        let condition = Self::normalize_function_control_condition(condition);
        match path {
            FunctionPathCondition::Always => {
                FunctionPathCondition::Conditional(Box::new(condition))
            }
            FunctionPathCondition::Never => FunctionPathCondition::Never,
            FunctionPathCondition::Conditional(path) => {
                FunctionPathCondition::Conditional(Box::new(Expression::Binary(
                    path,
                    Op::LogicAnd,
                    Box::new(condition),
                    Box::new(Comptime::create_unknown(TokenRange::default())),
                )))
            }
        }
    }

    fn function_path_and_not(
        path: FunctionPathCondition,
        condition: Expression,
    ) -> FunctionPathCondition {
        let condition = Expression::Unary(
            Op::LogicNot,
            Box::new(Self::normalize_function_control_condition(condition)),
            Box::new(Comptime::create_unknown(TokenRange::default())),
        );
        Self::function_path_and(path, condition)
    }

    fn function_path_or(
        lhs: FunctionPathCondition,
        rhs: FunctionPathCondition,
    ) -> FunctionPathCondition {
        match (lhs, rhs) {
            (FunctionPathCondition::Always, _) | (_, FunctionPathCondition::Always) => {
                FunctionPathCondition::Always
            }
            (FunctionPathCondition::Never, rhs) => rhs,
            (lhs, FunctionPathCondition::Never) => lhs,
            (FunctionPathCondition::Conditional(lhs), FunctionPathCondition::Conditional(rhs)) => {
                FunctionPathCondition::Conditional(Box::new(Expression::Binary(
                    lhs,
                    Op::LogicOr,
                    rhs,
                    Box::new(Comptime::create_unknown(TokenRange::default())),
                )))
            }
        }
    }

    fn parse_function_path_condition<A>(
        &mut self,
        condition: &Expression,
        state: &HashMap<VarId, Expression>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        self.function_arg_stack.push(state.clone());
        let result = self.parse_expression(
            condition, targets, domain, convert, sources, ir_builder, None,
        );
        self.function_arg_stack.pop();
        result?;
        let condition = self
            .stack
            .pop_back()
            .expect("Function path condition evaluation failed");
        Ok(self.lower_procedural_condition(condition, ir_builder))
    }

    fn coerce_register_to_type<A>(
        &self,
        reg: RegisterId,
        ty: &RegisterType,
        signed: bool,
        ir_builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        match ty {
            RegisterType::Bit {
                width,
                signed: result_signed,
            } => self.coerce_register_to_formal(
                ir_builder,
                reg,
                *width,
                signed,
                *result_signed,
                true,
            ),
            RegisterType::Logic { width } => {
                let widened = self.cast_reg_width_ext(ir_builder, reg, *width, signed);
                if matches!(ir_builder.register(&widened), RegisterType::Logic { .. }) {
                    widened
                } else {
                    let logic = ir_builder.alloc_logic(*width);
                    ir_builder.emit(SIRInstruction::Unary(logic, UnaryOp::Ident, widened));
                    logic
                }
            }
        }
    }

    fn materialize_function_runtime_expression<A>(
        &mut self,
        expr: &Expression,
        state: &mut HashMap<VarId, Expression>,
        active: &FunctionPathCondition,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Expression, ParserError> {
        let guard_state = state.clone();
        let mut evaluated_state = state.clone();
        let (expr, expression_states) =
            self.capture_nested_function_outputs_with_states(expr, &mut evaluated_state)?;
        let value = match active {
            FunctionPathCondition::Never => return Ok(expr),
            FunctionPathCondition::Always => {
                *state = evaluated_state;
                self.function_arg_stack.push(state.clone());
                self.function_event_arg_state_stack
                    .push(expression_states.clone());
                let result = self
                    .parse_expression(&expr, targets, domain, convert, sources, ir_builder, None);
                self.function_event_arg_state_stack.pop();
                self.function_arg_stack.pop();
                result?;
                self.stack
                    .pop_back()
                    .expect("Function runtime expression evaluation failed")
            }
            FunctionPathCondition::Conditional(condition) => {
                *state = Self::merge_expression_states(
                    condition,
                    &guard_state,
                    &evaluated_state,
                    &guard_state,
                );
                let condition = self.parse_function_path_condition(
                    condition,
                    &guard_state,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                let width = self.get_expression_width(&expr);
                let signed = expr.comptime().expr_context.signed;
                let result = if expr.comptime().r#type.is_2state() {
                    ir_builder.alloc_bit(width, signed)
                } else {
                    ir_builder.alloc_logic(width)
                };
                let result_type = ir_builder.register(&result).clone();
                let effect_block = ir_builder.new_block();
                let skip_block = ir_builder.new_block();
                let merge_block = ir_builder.new_block_with(vec![result]);
                ir_builder.seal_block(SIRTerminator::Branch {
                    cond: condition,
                    true_block: (effect_block, vec![]),
                    false_block: (skip_block, vec![]),
                });

                ir_builder.switch_to_block(effect_block);
                self.function_arg_stack.push(evaluated_state.clone());
                self.function_event_arg_state_stack
                    .push(expression_states.clone());
                let parse_result = self
                    .parse_expression(&expr, targets, domain, convert, sources, ir_builder, None);
                self.function_event_arg_state_stack.pop();
                self.function_arg_stack.pop();
                parse_result?;
                let effect_value = self
                    .stack
                    .pop_back()
                    .expect("Guarded function runtime expression evaluation failed");
                let effect_value =
                    self.coerce_register_to_type(effect_value, &result_type, signed, ir_builder);
                ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![effect_value]));

                ir_builder.switch_to_block(skip_block);
                let dummy = ir_builder.alloc_reg(result_type);
                ir_builder.emit(SIRInstruction::Imm(dummy, SIRValue::new(0u8)));
                ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![dummy]));
                ir_builder.switch_to_block(merge_block);
                result
            }
        };
        self.function_expression_value_stack
            .last_mut()
            .expect("Function expression value scope is active")
            .insert(expr.token_range(), value);
        Ok(expr)
    }

    fn emit_function_system_task<A>(
        &mut self,
        call: &SystemFunctionCall,
        state: &mut HashMap<VarId, Expression>,
        active: &FunctionPathCondition,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        if matches!(active, FunctionPathCondition::Never) {
            return Ok(());
        }
        let guard_state = state.clone();
        let (call, next_state, arg_states) = self.prepare_system_function_call(call, state)?;
        *state = next_state;
        let merge_block = if let FunctionPathCondition::Conditional(condition) = active {
            let condition = self.parse_function_path_condition(
                condition,
                &guard_state,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            let effect_block = ir_builder.new_block();
            let merge_block = ir_builder.new_block();
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: condition,
                true_block: (effect_block, vec![]),
                false_block: (merge_block, vec![]),
            });
            ir_builder.switch_to_block(effect_block);
            Some(merge_block)
        } else {
            None
        };

        self.function_event_arg_state_stack.push(arg_states);
        let result =
            self.parse_system_task_statement(&call, targets, domain, convert, sources, ir_builder);
        self.function_event_arg_state_stack.pop();
        result?;

        if let Some(merge_block) = merge_block {
            ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
            ir_builder.switch_to_block(merge_block);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_function_runtime_effects_in_path<A>(
        &mut self,
        statements: &[Statement],
        ret_id: Option<VarId>,
        bindings: &HashMap<VarId, Expression>,
        mut active: FunctionPathCondition,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(HashMap<VarId, Expression>, FunctionPathCondition), ParserError> {
        let mut state = bindings.clone();
        for statement in statements {
            if matches!(active, FunctionPathCondition::Never) {
                break;
            }
            let is_return = Self::statement_is_function_return(statement, ret_id);
            match statement {
                Statement::Assign(assign) => {
                    if self.expression_needs_eager_evaluation(&assign.expr) {
                        self.materialize_function_runtime_expression(
                            &assign.expr,
                            &mut state,
                            &active,
                            targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                    }
                    state = self.apply_statement_to_function_state(statement, &state)?;
                    if is_return {
                        active = FunctionPathCondition::Never;
                    }
                }
                Statement::SystemFunctionCall(call) => {
                    self.emit_function_system_task(
                        call, &mut state, &active, targets, domain, convert, sources, ir_builder,
                    )?;
                }
                Statement::Null => {}
                Statement::If(statement) => {
                    let condition = if self.expression_needs_eager_evaluation(&statement.cond) {
                        self.materialize_function_runtime_expression(
                            &statement.cond,
                            &mut state,
                            &active,
                            targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?
                    } else {
                        Self::substitute_function_expr(&statement.cond, &state)
                    };
                    let base = state.clone();
                    let true_active = Self::function_path_and(active.clone(), condition.clone());
                    let false_active =
                        Self::function_path_and_not(active.clone(), condition.clone());
                    let (then_state, then_active) = self.emit_function_runtime_effects_in_path(
                        &statement.true_side,
                        ret_id,
                        &base,
                        true_active,
                        targets,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    let (else_state, else_active) = self.emit_function_runtime_effects_in_path(
                        &statement.false_side,
                        ret_id,
                        &base,
                        false_active,
                        targets,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    state =
                        Self::merge_expression_states(&condition, &base, &then_state, &else_state);
                    active = Self::function_path_or(then_active, else_active);
                }
                Statement::Case(statement) => {
                    if self.expression_needs_eager_evaluation(&statement.case_target) {
                        self.materialize_function_runtime_expression(
                            &statement.case_target,
                            &mut state,
                            &active,
                            targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                    }
                    let mut remaining = active.clone();
                    let mut live_paths = FunctionPathCondition::Never;
                    for arm in &statement.arms {
                        for pattern in &arm.patterns {
                            match pattern {
                                CasePattern::Eq(expr) => {
                                    if self.expression_needs_eager_evaluation(expr) {
                                        self.materialize_function_runtime_expression(
                                            expr, &mut state, &remaining, targets, domain, convert,
                                            sources, ir_builder,
                                        )?;
                                    }
                                }
                                CasePattern::Range { lo, hi, .. } => {
                                    for expr in [lo, hi] {
                                        if self.expression_needs_eager_evaluation(expr) {
                                            self.materialize_function_runtime_expression(
                                                expr, &mut state, &remaining, targets, domain,
                                                convert, sources, ir_builder,
                                            )?;
                                        }
                                    }
                                }
                            }
                        }
                        let base = state.clone();
                        let condition = Self::substitute_function_expr(
                            &case_arm_condition_expr(&statement.case_target, &arm.patterns),
                            &base,
                        );
                        let arm_active =
                            Self::function_path_and(remaining.clone(), condition.clone());
                        let (_, arm_live) = self.emit_function_runtime_effects_in_path(
                            &arm.body, ret_id, &base, arm_active, targets, domain, convert,
                            sources, ir_builder,
                        )?;
                        live_paths = Self::function_path_or(live_paths, arm_live);
                        remaining = Self::function_path_and_not(remaining, condition);
                    }
                    let base = state.clone();
                    let (_, default_live) = self.emit_function_runtime_effects_in_path(
                        &statement.default,
                        ret_id,
                        &base,
                        remaining,
                        targets,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    active = Self::function_path_or(live_paths, default_live);
                    state = self.apply_case_to_function_state(statement, 0, &base)?;
                }
                Statement::For(statement) => {
                    return Err(ParserError::unsupported(
                        66,
                        LoweringPhase::FfLowering,
                        "control flow around runtime effect in function body",
                        "for loop".to_string(),
                        Some(&statement.token),
                    ));
                }
                Statement::FunctionCall(call) => {
                    if self.function_call_has_runtime_effect(call, &mut HashSet::default()) {
                        return Err(ParserError::unsupported(
                            66,
                            LoweringPhase::FfLowering,
                            "nested runtime effect in function body",
                            format!("{statement}"),
                            Some(&call.comptime.token),
                        ));
                    }
                    let Some(function) = self.module.functions.get(&call.id) else {
                        return Err(ParserError::unsupported(
                            43,
                            LoweringPhase::FfLowering,
                            "function call",
                            format!("unknown function id: {:?}", call.id),
                            Some(&call.comptime.token),
                        ));
                    };
                    let ordered_inputs: Vec<_> = function
                        .args
                        .iter()
                        .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path))
                        .filter_map(|path| call.inputs.get(path).cloned())
                        .collect();
                    let last_effectful = ordered_inputs
                        .iter()
                        .rposition(|expr| self.expression_needs_eager_evaluation(expr));
                    if let Some(last_effectful) = last_effectful {
                        for expr in &ordered_inputs[..=last_effectful] {
                            self.materialize_function_runtime_expression(
                                expr, &mut state, &active, targets, domain, convert, sources,
                                ir_builder,
                            )?;
                        }
                    }
                    state = self.apply_function_call_to_state(call, &state)?;
                }
                Statement::IfReset(statement) => {
                    return Err(ParserError::unsupported(
                        66,
                        LoweringPhase::FfLowering,
                        "control flow around runtime effect in function body",
                        format!("{statement}"),
                        Some(&statement.token),
                    ));
                }
                Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                    return Err(ParserError::unsupported(
                        66,
                        LoweringPhase::FfLowering,
                        "runtime effect in function body",
                        format!("{statement}"),
                        None,
                    ));
                }
            }
        }
        Ok((state, active))
    }

    fn emit_function_runtime_effects<A>(
        &mut self,
        statements: &[Statement],
        ret_id: Option<VarId>,
        bindings: &HashMap<VarId, Expression>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let (state, _) = self.emit_function_runtime_effects_in_path(
            statements,
            ret_id,
            bindings,
            FunctionPathCondition::Always,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        Ok(state)
    }

    fn normalize_function_control_condition(condition: Expression) -> Expression {
        let token = TokenRange::default();
        let already_one_bit = condition.comptime().r#type.total_width() == Some(1)
            || matches!(
                &condition,
                Expression::Binary(
                    _,
                    Op::Eq
                        | Op::EqWildcard
                        | Op::Ne
                        | Op::NeWildcard
                        | Op::Less
                        | Op::LessEq
                        | Op::Greater
                        | Op::GreaterEq
                        | Op::LogicAnd
                        | Op::LogicOr,
                    _,
                    _,
                ) | Expression::Unary(
                    Op::BitAnd
                        | Op::BitOr
                        | Op::BitXor
                        | Op::BitNand
                        | Op::BitNor
                        | Op::BitXnor
                        | Op::LogicNot,
                    _,
                    _,
                )
            );
        let truth = if already_one_bit {
            condition
        } else {
            Expression::Unary(
                Op::BitOr,
                Box::new(condition),
                Box::new(Comptime::create_unknown(token)),
            )
        };
        let mut bit_type = Type::new(TypeKind::Bit);
        bit_type.set_concrete_width(Shape::new(vec![Some(1)]));
        let cast_target = Expression::Term(Box::new(Factor::Value(Comptime {
            value: ValueVariant::Type(bit_type),
            r#type: Type::new(TypeKind::Type),
            is_const: true,
            is_global: true,
            token,
            ..Default::default()
        })));
        Expression::Binary(
            Box::new(truth),
            Op::As,
            Box::new(cast_target),
            Box::new(Comptime::create_unknown(token)),
        )
    }

    fn default_expr_matches_formal(expr: &Expression, formal_shape: &[usize]) -> bool {
        Self::expr_shape_matches_formal(expr, formal_shape)
            || (!formal_shape.is_empty() && expr.comptime().r#type.array.is_empty())
    }

    fn expr_shape_matches_formal(expr: &Expression, formal_shape: &[usize]) -> bool {
        match expr {
            Expression::ArrayLiteral(items, _) => {
                let Some((&formal_len, formal_tail)) = formal_shape.split_first() else {
                    return false;
                };
                let mut explicit_len = 0usize;
                let mut saw_default = false;

                for item in items {
                    match item {
                        ArrayLiteralItem::Value(inner, repeat) => {
                            let rep_count = if let Some(rep_expr) = repeat {
                                match crate::bitaccess::eval_constexpr(rep_expr)
                                    .and_then(|v| v.to_u64())
                                {
                                    Some(v) => v as usize,
                                    None => return false,
                                }
                            } else {
                                1
                            };
                            explicit_len += rep_count;
                            if explicit_len > formal_len {
                                return false;
                            }
                            if !Self::expr_shape_matches_formal(inner, formal_tail) {
                                return false;
                            }
                        }
                        ArrayLiteralItem::Defaul(inner) => {
                            if saw_default {
                                return false;
                            }
                            saw_default = true;
                            if !Self::default_expr_matches_formal(inner, formal_tail) {
                                return false;
                            }
                        }
                    }
                }

                if saw_default {
                    explicit_len <= formal_len
                } else {
                    explicit_len == formal_len
                }
            }
            _ => {
                let shape: Option<Vec<usize>> =
                    expr.comptime().r#type.array.iter().copied().collect();
                shape.unwrap_or_default() == formal_shape
            }
        }
    }

    fn actual_matches_formal_shape(
        &self,
        formal: &veryl_analyzer::ir::Variable,
        expr: &Expression,
    ) -> bool {
        let formal_shape: Option<Vec<usize>> = formal.r#type.array.iter().copied().collect();
        let formal_shape = formal_shape.unwrap_or_default();
        if formal_shape.is_empty() {
            return true;
        }
        Self::expr_shape_matches_formal(expr, &formal_shape)
    }

    fn validate_function_call_bindings(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        function_body: &veryl_analyzer::ir::FunctionBody,
    ) -> Result<(), ParserError> {
        for (arg_path, arg_id) in &function_body.arg_map {
            let Some(arg_expr) = call.inputs.get(arg_path) else {
                continue;
            };
            let formal = &self.module.variables[arg_id];
            if !self.actual_matches_formal_shape(formal, arg_expr) {
                return Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "function call argument shape",
                    format!(
                        "actual expression shape does not match unpacked array formal `{}`",
                        formal.path
                    ),
                    Some(&call.comptime.token),
                ));
            }
        }
        Ok(())
    }

    fn apply_function_call_to_state(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let Some(function) = self.module.functions.get(&call.id) else {
            return Err(ParserError::unsupported(
                43,
                LoweringPhase::FfLowering,
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
            return Err(ParserError::unsupported(
                62,
                LoweringPhase::FfLowering,
                "function call specialization",
                format!("{call}"),
                Some(&call.comptime.token),
            ));
        };

        self.validate_function_call_bindings(call, &function_body)?;

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = call.inputs.get(arg_path) {
                bindings.insert(*arg_id, Self::substitute_function_expr(arg_expr, state));
            }
        }

        let mut next = state.clone();
        for (arg_path, dsts) in &call.outputs {
            let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                return Err(ParserError::unsupported(
                    61,
                    LoweringPhase::FfLowering,
                    "function call missing argument",
                    format!("{call}"),
                    Some(&call.comptime.token),
                ));
            };

            if dsts.len() != 1 {
                return Err(ParserError::unsupported(
                    60,
                    LoweringPhase::FfLowering,
                    "function body call output assignment shape",
                    format!("{call}"),
                    Some(&call.comptime.token),
                ));
            }

            let dst = &dsts[0];
            let is_whole_var =
                dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();

            let expr = self.extract_function_target_expr(&function_body, *arg_id, &bindings)?;
            let expr = Self::substitute_function_expr(&expr, &next);

            if is_whole_var {
                next.insert(dst.id, expr);
            } else if is_static_access(&dst.index, &dst.select) {
                let old_value = next.get(&dst.id).cloned().unwrap_or_else(|| {
                    Expression::Term(Box::new(Factor::Variable(
                        dst.id,
                        VarIndex::default(),
                        VarSelect::default(),
                        dst.comptime.clone(),
                    )))
                });
                let merged = build_partial_assign_expr(self.module, dst, expr, old_value)?;
                next.insert(dst.id, merged);
            } else {
                return Err(ParserError::unsupported(
                    60,
                    LoweringPhase::FfLowering,
                    "function body call output non-whole assignment (dynamic index)",
                    format!("{call}"),
                    Some(&call.comptime.token),
                ));
            }
        }

        Ok(next)
    }

    pub(super) fn get_bound_function_arg_expr(&self, var_id: VarId) -> Option<&Expression> {
        self.function_arg_stack
            .iter()
            .rev()
            .find_map(|bindings| bindings.get(&var_id))
    }

    pub(super) fn get_bound_function_arg_value(&self, var_id: VarId) -> Option<&RegisterId> {
        self.function_arg_value_stack
            .iter()
            .rev()
            .find_map(|bindings| bindings.get(&var_id))
    }

    pub(super) fn get_bound_function_expression_value(
        &self,
        token: TokenRange,
    ) -> Option<&RegisterId> {
        self.function_expression_value_stack
            .iter()
            .rev()
            .find_map(|bindings| bindings.get(&token))
    }

    pub(super) fn get_bound_function_event_arg_state(
        &self,
        token: TokenRange,
    ) -> Option<&HashMap<VarId, Expression>> {
        self.function_event_arg_state_stack
            .iter()
            .rev()
            .find_map(|states| states.get(&token))
    }

    fn expression_references_any(expr: &Expression, candidates: &HashSet<VarId>) -> bool {
        let input_references = |input: &veryl_analyzer::ir::SystemFunctionInput| {
            Self::expression_references_any(&input.0, candidates)
        };
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(id, index, select, _) => {
                    candidates.contains(id)
                        || index
                            .0
                            .iter()
                            .any(|expr| Self::expression_references_any(expr, candidates))
                        || select
                            .0
                            .iter()
                            .any(|expr| Self::expression_references_any(expr, candidates))
                        || select.1.as_ref().is_some_and(|(_, expr)| {
                            Self::expression_references_any(expr, candidates)
                        })
                }
                Factor::FunctionCall(call) => call
                    .inputs
                    .values()
                    .any(|expr| Self::expression_references_any(expr, candidates)),
                Factor::SystemFunctionCall(call) => match &call.kind {
                    SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                        args.iter().any(input_references)
                    }
                    SystemFunctionKind::Assert { cond, args, .. } => {
                        input_references(cond) || args.iter().any(input_references)
                    }
                    SystemFunctionKind::Bits(input)
                    | SystemFunctionKind::Size(input)
                    | SystemFunctionKind::Clog2(input)
                    | SystemFunctionKind::Onehot(input)
                    | SystemFunctionKind::Signed(input)
                    | SystemFunctionKind::Unsigned(input) => input_references(input),
                    SystemFunctionKind::Readmemh(_, _) | SystemFunctionKind::Finish => false,
                },
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => false,
            },
            Expression::Binary(lhs, _, rhs, _) => {
                Self::expression_references_any(lhs, candidates)
                    || Self::expression_references_any(rhs, candidates)
            }
            Expression::Unary(_, inner, _) => Self::expression_references_any(inner, candidates),
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                Self::expression_references_any(cond, candidates)
                    || Self::expression_references_any(then_expr, candidates)
                    || Self::expression_references_any(else_expr, candidates)
            }
            Expression::Concatenation(items, _) => items.iter().any(|(expr, repeat)| {
                Self::expression_references_any(expr, candidates)
                    || repeat
                        .as_ref()
                        .is_some_and(|repeat| Self::expression_references_any(repeat, candidates))
            }),
            Expression::ArrayLiteral(items, _) => items.iter().any(|item| match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    Self::expression_references_any(expr, candidates)
                        || repeat.as_ref().is_some_and(|repeat| {
                            Self::expression_references_any(repeat, candidates)
                        })
                }
                ArrayLiteralItem::Defaul(expr) => Self::expression_references_any(expr, candidates),
            }),
            Expression::StructConstructor(_, fields, _) => fields
                .iter()
                .any(|(_, expr)| Self::expression_references_any(expr, candidates)),
        }
    }

    fn materialize_function_inputs<A>(
        &mut self,
        call: &veryl_analyzer::ir::FunctionCall,
        function_body: &veryl_analyzer::ir::FunctionBody,
        ordered_arg_paths: &[veryl_analyzer::ir::VarPath],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<HashMap<VarId, RegisterId>, ParserError> {
        let mut values = HashMap::default();
        let output_ids: HashSet<VarId> = call
            .outputs
            .values()
            .flat_map(|destinations| destinations.iter().map(|dst| dst.id))
            .collect();
        let last_effectful_arg = ordered_arg_paths.iter().rposition(|arg_path| {
            call.inputs.get(arg_path).is_some_and(|actual| {
                super::expression::expression_has_side_effect(actual)
                    || self.expression_has_runtime_effect(actual)
            })
        });
        for (arg_index, arg_path) in ordered_arg_paths.iter().enumerate() {
            let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                continue;
            };
            let Some(actual) = call.inputs.get(arg_path) else {
                continue;
            };
            let must_snapshot_for_order = last_effectful_arg.is_some_and(|last| arg_index <= last);
            if !must_snapshot_for_order && !Self::expression_references_any(actual, &output_ids) {
                continue;
            }

            let formal = &self.module.variables[arg_id];
            if !formal.r#type.array.is_empty() {
                return Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "materialized unpacked function argument",
                    format!("{actual}"),
                    Some(&call.comptime.token),
                ));
            }
            let formal_width = resolve_total_width(self.module, formal)?;
            self.parse_expression(
                actual,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                Some(formal_width),
            )?;
            let actual_reg = self.stack.pop_back().unwrap();
            let formal_reg = self.coerce_register_to_formal(
                ir_builder,
                actual_reg,
                formal_width,
                actual.comptime().r#type.signed,
                formal.r#type.signed,
                formal.r#type.is_2state(),
            );
            values.insert(*arg_id, formal_reg);
        }
        Ok(values)
    }

    pub(super) fn substitute_function_expr(
        expr: &Expression,
        defs: &HashMap<VarId, Expression>,
    ) -> Expression {
        Self::substitute_function_expr_inner(expr, defs, &mut HashSet::default())
    }

    fn substitute_function_expr_inner(
        expr: &Expression,
        defs: &HashMap<VarId, Expression>,
        expanding: &mut HashSet<VarId>,
    ) -> Expression {
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(var_id, index, select, _)
                    if index.0.is_empty() && select.0.is_empty() && select.1.is_none() =>
                {
                    if let Some(bound) = defs.get(var_id) {
                        if expanding.insert(*var_id) {
                            let result =
                                Self::substitute_function_expr_inner(bound, defs, expanding);
                            expanding.remove(var_id);
                            return result;
                        }
                    }
                    expr.clone()
                }
                Factor::FunctionCall(call) => {
                    let mut call = call.clone();
                    for input_expr in call.inputs.values_mut() {
                        *input_expr =
                            Self::substitute_function_expr_inner(input_expr, defs, expanding);
                    }
                    Expression::Term(Box::new(Factor::FunctionCall(call)))
                }
                _ => expr.clone(),
            },
            Expression::Binary(lhs, op, rhs, comptime) => Expression::Binary(
                Box::new(Self::substitute_function_expr_inner(lhs, defs, expanding)),
                *op,
                Box::new(Self::substitute_function_expr_inner(rhs, defs, expanding)),
                comptime.clone(),
            ),
            Expression::Unary(op, inner, comptime) => Expression::Unary(
                *op,
                Box::new(Self::substitute_function_expr_inner(inner, defs, expanding)),
                comptime.clone(),
            ),
            Expression::Ternary(cond, then_expr, else_expr, comptime) => Expression::Ternary(
                Box::new(Self::substitute_function_expr_inner(cond, defs, expanding)),
                Box::new(Self::substitute_function_expr_inner(
                    then_expr, defs, expanding,
                )),
                Box::new(Self::substitute_function_expr_inner(
                    else_expr, defs, expanding,
                )),
                comptime.clone(),
            ),
            Expression::Concatenation(parts, comptime) => Expression::Concatenation(
                parts
                    .iter()
                    .map(|(x, rep)| {
                        (
                            Self::substitute_function_expr_inner(x, defs, expanding),
                            rep.as_ref()
                                .map(|r| Self::substitute_function_expr_inner(r, defs, expanding)),
                        )
                    })
                    .collect(),
                comptime.clone(),
            ),
            Expression::ArrayLiteral(items, comptime) => Expression::ArrayLiteral(
                items
                    .iter()
                    .map(|item| match item {
                        ArrayLiteralItem::Value(x, rep) => ArrayLiteralItem::Value(
                            Box::new(Self::substitute_function_expr_inner(x, defs, expanding)),
                            rep.as_ref().map(|r| {
                                Box::new(Self::substitute_function_expr_inner(r, defs, expanding))
                            }),
                        ),
                        ArrayLiteralItem::Defaul(x) => ArrayLiteralItem::Defaul(Box::new(
                            Self::substitute_function_expr_inner(x, defs, expanding),
                        )),
                    })
                    .collect(),
                comptime.clone(),
            ),
            Expression::StructConstructor(ty, fields, comptime) => Expression::StructConstructor(
                ty.clone(),
                fields
                    .iter()
                    .map(|(name, x)| {
                        (
                            *name,
                            Self::substitute_function_expr_inner(x, defs, expanding),
                        )
                    })
                    .collect(),
                comptime.clone(),
            ),
        }
    }

    pub(super) fn extract_function_target_expr(
        &self,
        body: &veryl_analyzer::ir::FunctionBody,
        target_id: VarId,
        defs: &HashMap<VarId, Expression>,
    ) -> Result<Expression, ParserError> {
        fn merge_branch_state(
            cond: &Expression,
            mut then_state: HashMap<VarId, Expression>,
            else_state: HashMap<VarId, Expression>,
        ) -> HashMap<VarId, Expression> {
            let mut merged = HashMap::default();
            for (id, then_expr) in then_state.drain() {
                if let Some(else_expr) = else_state.get(&id) {
                    merged.insert(
                        id,
                        Expression::Ternary(
                            Box::new(FfParser::normalize_function_control_condition(cond.clone())),
                            Box::new(then_expr),
                            Box::new(else_expr.clone()),
                            Box::new(Comptime::create_unknown(TokenRange::default())),
                        ),
                    );
                }
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
                    if assign.dst.len() != 1 {
                        return Err(ParserError::unsupported(
                            43,
                            LoweringPhase::FfLowering,
                            "function body assignment shape",
                            format!("{stmt}"),
                            Some(&assign.token),
                        ));
                    }

                    let dst = &assign.dst[0];
                    let is_whole_var =
                        dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();

                    let mut next = state.clone();
                    let rhs = substitute(&assign.expr, &next);

                    if is_whole_var {
                        next.insert(dst.id, rhs);
                    } else if is_static_access(&dst.index, &dst.select) {
                        let old_value = next.get(&dst.id).cloned().unwrap_or_else(|| {
                            Expression::Term(Box::new(Factor::Variable(
                                dst.id,
                                VarIndex::default(),
                                VarSelect::default(),
                                dst.comptime.clone(),
                            )))
                        });
                        let merged = build_partial_assign_expr(parser.module, dst, rhs, old_value)?;
                        next.insert(dst.id, merged);
                    } else {
                        return Err(ParserError::unsupported(
                            43,
                            LoweringPhase::FfLowering,
                            "function body non-whole assignment (dynamic index)",
                            format!("{stmt}"),
                            Some(&assign.token),
                        ));
                    }
                    Ok(next)
                }
                Statement::If(if_stmt) => {
                    let then_state =
                        build_state_from_statements(parser, &if_stmt.true_side, state, substitute)?;
                    let else_state = build_state_from_statements(
                        parser,
                        &if_stmt.false_side,
                        state,
                        substitute,
                    )?;
                    let cond = substitute(&if_stmt.cond, state);
                    Ok(merge_branch_state(&cond, then_state, else_state))
                }
                Statement::Case(case_stmt) => {
                    build_state_from_case(parser, case_stmt, 0, state, substitute)
                }
                Statement::Null => Ok(state.clone()),
                Statement::IfReset(ir) => Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "function body control flow",
                    format!("{stmt}"),
                    Some(&ir.token),
                )),
                Statement::SystemFunctionCall(call) => {
                    let (_, next_state, _) = parser.prepare_system_function_call(call, state)?;
                    Ok(next_state)
                }
                Statement::FunctionCall(call) => parser.apply_function_call_to_state(call, state),
                Statement::For(f) => Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "for loop in function body",
                    format!("{stmt}"),
                    Some(&f.token),
                )),
                Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                    Err(ParserError::unsupported(
                        43,
                        LoweringPhase::FfLowering,
                        "function body control flow",
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
            Ok(merge_branch_state(&cond, then_state, else_state))
        }

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
            Self::substitute_function_expr(expr, defs)
        })?;
        state.get(&target_id).cloned().ok_or_else(|| {
            ParserError::unsupported(
                43,
                LoweringPhase::FfLowering,
                "function return expression",
                format!("function target var id: {:?}", target_id),
                None,
            )
        })
    }

    pub(super) fn extract_function_return_expr(
        &self,
        body: &veryl_analyzer::ir::FunctionBody,
        ret_id: VarId,
    ) -> Result<Expression, ParserError> {
        fn resolve_return_expr(
            parser: &FfParser,
            statements: &[Statement],
            ret_id: VarId,
            defs: &HashMap<VarId, Expression>,
            substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
        ) -> Result<Option<Expression>, ParserError> {
            if statements.is_empty() {
                return Ok(None);
            }

            let stmt = &statements[0];
            let rest = &statements[1..];

            match stmt {
                Statement::Assign(assign) => {
                    if assign.dst.len() != 1 {
                        return Err(ParserError::unsupported(
                            43,
                            LoweringPhase::FfLowering,
                            "function body assignment shape",
                            format!("{stmt}"),
                            Some(&assign.token),
                        ));
                    }

                    let dst = &assign.dst[0];
                    let is_whole_var =
                        dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();

                    let rhs = substitute(&assign.expr, defs);

                    if is_whole_var {
                        if dst.id == ret_id {
                            // Assignment to return variable corresponds to `return` and terminates
                            // this path.
                            return Ok(Some(rhs));
                        }

                        let mut next_defs = defs.clone();
                        next_defs.insert(dst.id, rhs);
                        resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
                    } else if is_static_access(&dst.index, &dst.select) {
                        let old_value = defs.get(&dst.id).cloned().unwrap_or_else(|| {
                            Expression::Term(Box::new(Factor::Variable(
                                dst.id,
                                VarIndex::default(),
                                VarSelect::default(),
                                dst.comptime.clone(),
                            )))
                        });
                        let merged = build_partial_assign_expr(parser.module, dst, rhs, old_value)?;

                        // Partial write to return var does NOT terminate the path —
                        // additional writes may fill in other bits.
                        let mut next_defs = defs.clone();
                        next_defs.insert(dst.id, merged);
                        resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
                    } else {
                        Err(ParserError::unsupported(
                            43,
                            LoweringPhase::FfLowering,
                            "function body non-whole assignment (dynamic index)",
                            format!("{stmt}"),
                            Some(&assign.token),
                        ))
                    }
                }
                Statement::If(if_stmt) => {
                    let cond = substitute(&if_stmt.cond, defs);

                    let mut then_stmts = if_stmt.true_side.clone();
                    then_stmts.extend_from_slice(rest);
                    let then_expr =
                        resolve_return_expr(parser, &then_stmts, ret_id, defs, substitute)?;

                    let mut else_stmts = if_stmt.false_side.clone();
                    else_stmts.extend_from_slice(rest);
                    let else_expr =
                        resolve_return_expr(parser, &else_stmts, ret_id, defs, substitute)?;

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
                Statement::IfReset(ir) => Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "function body control flow",
                    format!("{stmt}"),
                    Some(&ir.token),
                )),
                Statement::SystemFunctionCall(call) => {
                    let (_, next_defs, _) = parser.prepare_system_function_call(call, defs)?;
                    resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
                }
                Statement::FunctionCall(call) => {
                    let next_defs = parser.apply_function_call_to_state(call, defs)?;
                    resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
                }
                Statement::For(f) => Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "for loop in function body",
                    format!("{stmt}"),
                    Some(&f.token),
                )),
                Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                    Err(ParserError::unsupported(
                        43,
                        LoweringPhase::FfLowering,
                        "function body control flow",
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
            &|expr, defs| Self::substitute_function_expr(expr, defs),
        )?
        .ok_or_else(|| {
            ParserError::unsupported(
                43,
                LoweringPhase::FfLowering,
                "function return expression",
                format!("function call to id {:?}", ret_id),
                None,
            )
        })
    }

    pub(super) fn parse_function_call_expr<A>(
        &mut self,
        call: &veryl_analyzer::ir::FunctionCall,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let Some(function) = self.module.functions.get(&call.id) else {
            return Err(ParserError::unsupported(
                43,
                LoweringPhase::FfLowering,
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
            return Err(ParserError::unsupported(
                62,
                LoweringPhase::FfLowering,
                "function call specialization",
                format!("{call}"),
                Some(&call.comptime.token),
            ));
        };
        let ordered_arg_paths: Vec<_> = function
            .args
            .iter()
            .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path.clone()))
            .collect();

        self.validate_function_call_bindings(call, &function_body)?;

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = call.inputs.get(arg_path) {
                bindings.insert(*arg_id, arg_expr.clone());
            }
        }
        let materialized = self.materialize_function_inputs(
            call,
            &function_body,
            &ordered_arg_paths,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        let mut symbolic_bindings = bindings.clone();
        for arg_id in materialized.keys() {
            symbolic_bindings.remove(arg_id);
        }

        self.function_arg_stack.push(bindings);
        self.function_arg_value_stack.push(materialized);
        self.function_expression_value_stack
            .push(HashMap::default());
        let result = (|| {
            let runtime_state = if self
                .statements_have_runtime_effect(&function_body.statements, &mut HashSet::default())
            {
                Some(self.emit_function_runtime_effects(
                    &function_body.statements,
                    function_body.ret,
                    &symbolic_bindings,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?)
            } else {
                None
            };

            for (arg_path, dsts) in &call.outputs {
                let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                    return Err(ParserError::unsupported(
                        61,
                        LoweringPhase::FfLowering,
                        "function call missing argument",
                        format!("{call}"),
                        Some(&call.comptime.token),
                    ));
                };

                let expr = if let Some(expr) =
                    runtime_state.as_ref().and_then(|state| state.get(arg_id))
                {
                    expr.clone()
                } else {
                    self.extract_function_target_expr(&function_body, *arg_id, &symbolic_bindings)?
                };
                self.parse_expression(&expr, targets, domain, convert, sources, ir_builder, None)?;

                let rhs_reg = self
                    .stack
                    .pop_back()
                    .expect("Function output expression evaluation failed");
                self.emit_multi_dst_assign(
                    rhs_reg, dsts, targets, domain, convert, sources, ir_builder,
                )?;
            }

            let Some(ret_id) = function_body.ret else {
                return Err(ParserError::illegal_context(
                    "void function call in expression",
                    format!("{call}"),
                    Some(&call.comptime.token),
                ));
            };
            let ret_expr =
                if let Some(expr) = runtime_state.as_ref().and_then(|state| state.get(&ret_id)) {
                    expr.clone()
                } else {
                    self.extract_function_return_expr(&function_body, ret_id)?
                };
            self.parse_expression(
                &ret_expr, targets, domain, convert, sources, ir_builder, None,
            )
        })();
        self.function_expression_value_stack.pop();
        self.function_arg_value_stack.pop();
        self.function_arg_stack.pop();
        result
    }

    pub(super) fn parse_function_call_statement<A>(
        &mut self,
        call: &veryl_analyzer::ir::FunctionCall,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let Some(function) = self.module.functions.get(&call.id) else {
            return Err(ParserError::unsupported(
                43,
                LoweringPhase::FfLowering,
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
            return Err(ParserError::unsupported(
                62,
                LoweringPhase::FfLowering,
                "function call specialization",
                format!("{call}"),
                Some(&call.comptime.token),
            ));
        };
        let ordered_arg_paths: Vec<_> = function
            .args
            .iter()
            .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path.clone()))
            .collect();

        self.validate_function_call_bindings(call, &function_body)?;

        let has_runtime_effect =
            self.statements_have_runtime_effect(&function_body.statements, &mut HashSet::default());
        if call.outputs.is_empty() && !has_runtime_effect {
            // A pure statement-form function call has no observable result.
            return Ok(());
        }

        // Statement-form call ignores return value, if present.

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = call.inputs.get(arg_path) {
                bindings.insert(*arg_id, arg_expr.clone());
            }
        }
        let materialized = self.materialize_function_inputs(
            call,
            &function_body,
            &ordered_arg_paths,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        let mut symbolic_bindings = bindings.clone();
        for arg_id in materialized.keys() {
            symbolic_bindings.remove(arg_id);
        }

        self.function_arg_stack.push(bindings);
        self.function_arg_value_stack.push(materialized);
        self.function_expression_value_stack
            .push(HashMap::default());
        let result = (|| {
            let runtime_state = if has_runtime_effect {
                Some(self.emit_function_runtime_effects(
                    &function_body.statements,
                    function_body.ret,
                    &symbolic_bindings,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?)
            } else {
                None
            };

            for (arg_path, dsts) in &call.outputs {
                let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                    return Err(ParserError::unsupported(
                        61,
                        LoweringPhase::FfLowering,
                        "function call missing argument",
                        format!("{call}"),
                        Some(&call.comptime.token),
                    ));
                };

                let expr = if let Some(expr) =
                    runtime_state.as_ref().and_then(|state| state.get(arg_id))
                {
                    expr.clone()
                } else {
                    self.extract_function_target_expr(&function_body, *arg_id, &symbolic_bindings)?
                };
                self.parse_expression(&expr, targets, domain, convert, sources, ir_builder, None)?;

                let rhs_reg = self
                    .stack
                    .pop_back()
                    .expect("Function output expression evaluation failed");
                self.emit_multi_dst_assign(
                    rhs_reg, dsts, targets, domain, convert, sources, ir_builder,
                )?;
            }

            Ok(())
        })();
        self.function_expression_value_stack.pop();
        self.function_arg_value_stack.pop();
        self.function_arg_stack.pop();
        result
    }
}
