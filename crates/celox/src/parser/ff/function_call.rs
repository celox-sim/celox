use super::{Domain, FfParser};
use crate::{
    HashMap,
    ir::{SIRBuilder, VarAtomBase},
    parser::ParserError,
};
use veryl_analyzer::ir::{ArrayLiteralItem, Expression, Factor, Statement, VarId};

impl<'a> FfParser<'a> {
    fn apply_function_call_to_state(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let Some(function) = self.module.functions.get(&call.id) else {
            return Err(ParserError::UnsupportedFFLowering {
                feature: "function call",
                detail: format!("unknown function id: {:?}", call.id),
            });
        };

        let Some(function_body) = (if let Some(index) = &call.index {
            function.get_function(index)
        } else {
            function.get_function(&[])
        }) else {
            return Err(ParserError::UnsupportedFFLowering {
                feature: "function call specialization",
                detail: format!("{call}"),
            });
        };

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = call.inputs.get(arg_path) {
                bindings.insert(*arg_id, Self::substitute_function_expr(arg_expr, state));
            }
        }

        let mut next = state.clone();
        for (arg_path, dsts) in &call.outputs {
            let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "function call missing argument",
                    detail: format!("{call}"),
                });
            };

            if dsts.len() != 1 {
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "function body call output assignment shape",
                    detail: format!("{call}"),
                });
            }

            let dst = &dsts[0];
            let is_whole_var =
                dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();
            if !is_whole_var {
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "function body call output non-whole assignment",
                    detail: format!("{call}"),
                });
            }

            let expr = self.extract_function_target_expr(&function_body, *arg_id, &bindings)?;
            let expr = Self::substitute_function_expr(&expr, &next);
            next.insert(dst.id, expr);
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
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(var_id, index, select, _, _)
                    if index.0.is_empty() && select.0.is_empty() && select.1.is_none() =>
                {
                    if let Some(bound) = defs.get(var_id) {
                        return Self::substitute_function_expr(bound, defs);
                    }
                    expr.clone()
                }
                Factor::FunctionCall(call, token) => {
                    let mut call = call.clone();
                    for input_expr in call.inputs.values_mut() {
                        *input_expr = Self::substitute_function_expr(input_expr, defs);
                    }
                    Expression::Term(Box::new(Factor::FunctionCall(call, *token)))
                }
                _ => expr.clone(),
            },
            Expression::Binary(lhs, op, rhs) => Expression::Binary(
                Box::new(Self::substitute_function_expr(lhs, defs)),
                *op,
                Box::new(Self::substitute_function_expr(rhs, defs)),
            ),
            Expression::Unary(op, inner) => {
                Expression::Unary(*op, Box::new(Self::substitute_function_expr(inner, defs)))
            }
            Expression::Ternary(cond, then_expr, else_expr) => Expression::Ternary(
                Box::new(Self::substitute_function_expr(cond, defs)),
                Box::new(Self::substitute_function_expr(then_expr, defs)),
                Box::new(Self::substitute_function_expr(else_expr, defs)),
            ),
            Expression::Concatenation(parts) => Expression::Concatenation(
                parts
                    .iter()
                    .map(|(x, rep)| {
                        (
                            Self::substitute_function_expr(x, defs),
                            rep.as_ref()
                                .map(|r| Self::substitute_function_expr(r, defs)),
                        )
                    })
                    .collect(),
            ),
            Expression::ArrayLiteral(items) => Expression::ArrayLiteral(
                items
                    .iter()
                    .map(|item| match item {
                        ArrayLiteralItem::Value(x, rep) => ArrayLiteralItem::Value(
                            Self::substitute_function_expr(x, defs),
                            rep.as_ref()
                                .map(|r| Self::substitute_function_expr(r, defs)),
                        ),
                        ArrayLiteralItem::Defaul(x) => {
                            ArrayLiteralItem::Defaul(Self::substitute_function_expr(x, defs))
                        }
                    })
                    .collect(),
            ),
            Expression::StructConstructor(ty, fields) => Expression::StructConstructor(
                ty.clone(),
                fields
                    .iter()
                    .map(|(name, x)| (*name, Self::substitute_function_expr(x, defs)))
                    .collect(),
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
                        return Err(ParserError::UnsupportedFFLowering {
                            feature: "function body assignment shape",
                            detail: format!("{stmt}"),
                        });
                    }

                    let dst = &assign.dst[0];
                    let is_whole_var =
                        dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();

                    if !is_whole_var {
                        return Err(ParserError::UnsupportedFFLowering {
                            feature: "function body non-whole assignment",
                            detail: format!("{stmt}"),
                        });
                    }

                    let mut next = state.clone();
                    let rhs = substitute(&assign.expr, &next);
                    next.insert(dst.id, rhs);
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
                Statement::IfReset(_) | Statement::SystemFunctionCall(_) => {
                    Err(ParserError::UnsupportedFFLowering {
                        feature: "function body control flow",
                        detail: format!("{stmt}"),
                    })
                }
                Statement::FunctionCall(call) => parser.apply_function_call_to_state(call, state),
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
        state
            .get(&target_id)
            .cloned()
            .ok_or_else(|| ParserError::UnsupportedFFLowering {
                feature: "function return expression",
                detail: format!("function target var id: {:?}", target_id),
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
                        return Err(ParserError::UnsupportedFFLowering {
                            feature: "function body assignment shape",
                            detail: format!("{stmt}"),
                        });
                    }

                    let dst = &assign.dst[0];
                    let is_whole_var =
                        dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();

                    if !is_whole_var {
                        return Err(ParserError::UnsupportedFFLowering {
                            feature: "function body non-whole assignment",
                            detail: format!("{stmt}"),
                        });
                    }

                    let rhs = substitute(&assign.expr, defs);

                    if dst.id == ret_id {
                        // Assignment to return variable corresponds to `return` and terminates
                        // this path.
                        return Ok(Some(rhs));
                    }

                    let mut next_defs = defs.clone();
                    next_defs.insert(dst.id, rhs);
                    resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
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
                        ))),
                        _ => Ok(None),
                    }
                }
                Statement::Null => resolve_return_expr(parser, rest, ret_id, defs, substitute),
                Statement::IfReset(_) | Statement::SystemFunctionCall(_) => {
                    Err(ParserError::UnsupportedFFLowering {
                        feature: "function body control flow",
                        detail: format!("{stmt}"),
                    })
                }
                Statement::FunctionCall(call) => {
                    let next_defs = parser.apply_function_call_to_state(call, defs)?;
                    resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
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
        .ok_or_else(|| ParserError::UnsupportedFFLowering {
            feature: "function return expression",
            detail: format!("function call to id {:?}", ret_id),
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
            return Err(ParserError::UnsupportedFFLowering {
                feature: "function call",
                detail: format!("unknown function id: {:?}", call.id),
            });
        };

        let Some(function_body) = (if let Some(index) = &call.index {
            function.get_function(index)
        } else {
            function.get_function(&[])
        }) else {
            return Err(ParserError::UnsupportedFFLowering {
                feature: "function call specialization",
                detail: format!("{call}"),
            });
        };

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = call.inputs.get(arg_path) {
                bindings.insert(*arg_id, arg_expr.clone());
            }
        }

        for (arg_path, dsts) in &call.outputs {
            let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "function call missing argument",
                    detail: format!("{call}"),
                });
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
            return Err(ParserError::UnsupportedFFLowering {
                feature: "void function call in expression",
                detail: format!("{call}"),
            });
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
            return Err(ParserError::UnsupportedFFLowering {
                feature: "function call",
                detail: format!("unknown function id: {:?}", call.id),
            });
        };

        let Some(function_body) = (if let Some(index) = &call.index {
            function.get_function(index)
        } else {
            function.get_function(&[])
        }) else {
            return Err(ParserError::UnsupportedFFLowering {
                feature: "function call specialization",
                detail: format!("{call}"),
            });
        };

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
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "function call missing argument",
                    detail: format!("{call}"),
                });
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
