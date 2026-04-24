use super::{Domain, FfParser};
use crate::{
    HashMap, HashSet,
    ir::{SIRBuilder, VarAtomBase},
    parser::{
        LoweringPhase, ParserError,
        bitaccess::{build_partial_assign_expr, is_static_access},
    },
};
use num_traits::ToPrimitive;
use veryl_analyzer::ir::{
    ArrayLiteralItem, Comptime, Expression, Factor, Statement, VarId, VarIndex, VarSelect,
};
use veryl_parser::token_range::TokenRange;

impl<'a> FfParser<'a> {
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
                                match crate::parser::bitaccess::eval_constexpr(rep_expr)
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
            Expression::Binary(lhs, op, rhs, _) => Expression::Binary(
                Box::new(Self::substitute_function_expr_inner(lhs, defs, expanding)),
                *op,
                Box::new(Self::substitute_function_expr_inner(rhs, defs, expanding)),
                Box::new(Comptime::create_unknown(TokenRange::default())),
            ),
            Expression::Unary(op, inner, _) => Expression::Unary(
                *op,
                Box::new(Self::substitute_function_expr_inner(inner, defs, expanding)),
                Box::new(Comptime::create_unknown(TokenRange::default())),
            ),
            Expression::Ternary(cond, then_expr, else_expr, _) => Expression::Ternary(
                Box::new(Self::substitute_function_expr_inner(cond, defs, expanding)),
                Box::new(Self::substitute_function_expr_inner(
                    then_expr, defs, expanding,
                )),
                Box::new(Self::substitute_function_expr_inner(
                    else_expr, defs, expanding,
                )),
                Box::new(Comptime::create_unknown(TokenRange::default())),
            ),
            Expression::Concatenation(parts, _) => Expression::Concatenation(
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
                Box::new(Comptime::create_unknown(TokenRange::default())),
            ),
            Expression::ArrayLiteral(items, _) => Expression::ArrayLiteral(
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
                Box::new(Comptime::create_unknown(TokenRange::default())),
            ),
            Expression::StructConstructor(ty, fields, _) => Expression::StructConstructor(
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
                Box::new(Comptime::create_unknown(TokenRange::default())),
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
                            Box::new(cond.clone()),
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
                Statement::Null => Ok(state.clone()),
                Statement::IfReset(ir) => Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "function body control flow",
                    format!("{stmt}"),
                    Some(&ir.token),
                )),
                Statement::SystemFunctionCall(sc) => Err(ParserError::unsupported(
                    66,
                    LoweringPhase::FfLowering,
                    "function body control flow",
                    format!("{stmt}"),
                    Some(&sc.comptime.token),
                )),
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
                            Box::new(cond),
                            Box::new(then_expr),
                            Box::new(else_expr),
                            Box::new(Comptime::create_unknown(TokenRange::default())),
                        ))),
                        _ => Ok(None),
                    }
                }
                Statement::Null => resolve_return_expr(parser, rest, ret_id, defs, substitute),
                Statement::IfReset(ir) => Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "function body control flow",
                    format!("{stmt}"),
                    Some(&ir.token),
                )),
                Statement::SystemFunctionCall(sc) => Err(ParserError::unsupported(
                    66,
                    LoweringPhase::FfLowering,
                    "function body control flow",
                    format!("{stmt}"),
                    Some(&sc.comptime.token),
                )),
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

        self.validate_function_call_bindings(call, &function_body)?;

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = call.inputs.get(arg_path) {
                bindings.insert(*arg_id, arg_expr.clone());
            }
        }

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

            let expr = self.extract_function_target_expr(&function_body, *arg_id, &bindings)?;
            self.function_arg_stack.push(bindings.clone());
            self.parse_expression(&expr, targets, domain, convert, sources, ir_builder, None)?;
            self.function_arg_stack.pop();

            let rhs_reg = self
                .stack
                .pop_back()
                .expect("Function output expression evaluation failed");
            self.emit_multi_dst_assign(
                rhs_reg, dsts, targets, domain, convert, sources, ir_builder,
            )?;
        }

        let Some(ret_id) = function_body.ret else {
            return Err(ParserError::unsupported(
                63,
                LoweringPhase::FfLowering,
                "void function call in expression",
                format!("{call}"),
                Some(&call.comptime.token),
            ));
        };

        let ret_expr = self.extract_function_return_expr(&function_body, ret_id)?;

        self.function_arg_stack.push(bindings);
        let result = self.parse_expression(
            &ret_expr, targets, domain, convert, sources, ir_builder, None,
        );
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

        self.validate_function_call_bindings(call, &function_body)?;

        if call.outputs.is_empty() {
            // No side effect through output arguments: statement-form function call
            // has no effect in FF lowering.
            return Ok(());
        }

        // Statement-form call ignores return value, if present.

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = call.inputs.get(arg_path) {
                bindings.insert(*arg_id, arg_expr.clone());
            }
        }

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

            let expr = self.extract_function_target_expr(&function_body, *arg_id, &bindings)?;
            self.function_arg_stack.push(bindings.clone());
            self.parse_expression(&expr, targets, domain, convert, sources, ir_builder, None)?;
            self.function_arg_stack.pop();

            let rhs_reg = self
                .stack
                .pop_back()
                .expect("Function output expression evaluation failed");
            self.emit_multi_dst_assign(
                rhs_reg, dsts, targets, domain, convert, sources, ir_builder,
            )?;
        }

        Ok(())
    }
}
