use super::{Domain, FfParser};
use crate::{HashMap, HashSet, ParserError, context_width::expression_signed, function_call_arg};
use celox_design::VarAtomBase;
use celox_sir::SIRBuilder;
use veryl_analyzer::ir::{Expression, VarId};

impl<'a> FfParser<'a> {
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
        let ordered_arg_paths: Vec<_> = function
            .args
            .iter()
            .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path.clone()))
            .collect();
        if self.call_requires_inline(call)
            || self.call_has_unpacked_input_alias(call, &function_body, &ordered_arg_paths)
        {
            return self.emit_inline_function_call(
                call,
                &function_body,
                &ordered_arg_paths,
                true,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            );
        }

        let mut function_body =
            crate::lowering::function_return::implicit_return_body(&function_body).into_owned();
        crate::lowering::function_return::refresh_expression_tokens(&mut function_body);
        self.validate_function_call_bindings(call, &function_body)?;
        self.flush_captured_nonlocal_state_before_call(
            call, targets, domain, convert, sources, ir_builder,
        )?;

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = function_call_arg(&call.inputs, arg_path) {
                let arg_expr = self.coerce_function_input_expression(arg_expr.clone(), *arg_id);
                bindings.insert(*arg_id, arg_expr);
            }
        }
        self.function_expression_value_stack
            .push(HashMap::default());
        let materialized = match self.materialize_function_inputs(
            call,
            &function_body,
            &ordered_arg_paths,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        ) {
            Ok(materialized) => materialized,
            Err(err) => {
                self.function_expression_value_stack.pop();
                return Err(err);
            }
        };
        let mut symbolic_bindings = bindings.clone();
        for arg_id in materialized.keys() {
            symbolic_bindings.remove(arg_id);
        }
        symbolic_bindings.retain(|var_id, _| self.module.variables[var_id].r#type.array.is_empty());

        self.function_arg_stack.push(bindings);
        self.function_arg_value_stack.push(materialized);
        self.function_array_view_stack.push(HashMap::default());
        self.function_array_view_enabled_stack.push(true);
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

            let mut pending_outputs = Vec::new();
            for arg_path in &ordered_arg_paths {
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
                let rhs_reg = self.coerce_register_to_variable_type(
                    rhs_reg,
                    *arg_id,
                    expression_signed(&expr),
                    ir_builder,
                )?;
                let dsts = if let Some(state) = runtime_state.as_ref() {
                    dsts.iter()
                        .map(|dst| self.substitute_assignment_destination(dst, state))
                        .collect()
                } else {
                    dsts.clone()
                };
                let rhs_reg =
                    self.coerce_function_output_to_actual(rhs_reg, *arg_id, &dsts, ir_builder)?;
                pending_outputs.push((rhs_reg, dsts));
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
            )?;
            let ret_reg = self
                .stack
                .pop_back()
                .expect("Function return expression evaluation failed");
            let ret_reg = self.coerce_register_to_variable_type(
                ret_reg,
                ret_id,
                expression_signed(&ret_expr),
                ir_builder,
            )?;
            if let Some(state) = runtime_state.as_ref() {
                self.emit_nonlocal_function_state_writes(
                    state, targets, domain, convert, sources, ir_builder,
                )?;
            }
            for (rhs_reg, dsts) in pending_outputs {
                self.emit_multi_dst_assign(
                    rhs_reg, &dsts, targets, domain, convert, sources, ir_builder,
                )?;
            }
            self.stack.push_back(ret_reg);
            Ok(())
        })();
        self.function_expression_value_stack.pop();
        self.function_arg_value_stack.pop();
        self.function_array_view_enabled_stack.pop();
        let finished_views = self.function_array_view_stack.pop().unwrap();
        self.function_arg_stack.pop();
        self.restore_active_function_array_views(&finished_views, convert, ir_builder)?;
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
        let ordered_arg_paths: Vec<_> = function
            .args
            .iter()
            .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path.clone()))
            .collect();
        if self.call_requires_inline(call)
            || self.call_has_unpacked_input_alias(call, &function_body, &ordered_arg_paths)
        {
            return self.emit_inline_function_call(
                call,
                &function_body,
                &ordered_arg_paths,
                false,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            );
        }

        let mut function_body =
            crate::lowering::function_return::implicit_return_body(&function_body).into_owned();
        crate::lowering::function_return::refresh_expression_tokens(&mut function_body);
        self.validate_function_call_bindings(call, &function_body)?;

        let has_runtime_effect =
            self.statements_have_runtime_effect(&function_body.statements, &mut HashSet::default());
        let has_effectful_input = call
            .inputs
            .values()
            .any(|expr| self.expression_needs_eager_evaluation(expr));
        if call.outputs.is_empty() && !has_runtime_effect && !has_effectful_input {
            // A pure statement-form function call has no observable result.
            return Ok(());
        }

        // Statement-form call ignores return value, if present.

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = function_call_arg(&call.inputs, arg_path) {
                let arg_expr = self.coerce_function_input_expression(arg_expr.clone(), *arg_id);
                bindings.insert(*arg_id, arg_expr);
            }
        }
        self.function_expression_value_stack
            .push(HashMap::default());
        let materialized = match self.materialize_function_inputs(
            call,
            &function_body,
            &ordered_arg_paths,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        ) {
            Ok(materialized) => materialized,
            Err(err) => {
                self.function_expression_value_stack.pop();
                return Err(err);
            }
        };
        let mut symbolic_bindings = bindings.clone();
        for arg_id in materialized.keys() {
            symbolic_bindings.remove(arg_id);
        }
        symbolic_bindings.retain(|var_id, _| self.module.variables[var_id].r#type.array.is_empty());

        self.function_arg_stack.push(bindings);
        self.function_arg_value_stack.push(materialized);
        self.function_array_view_stack.push(HashMap::default());
        self.function_array_view_enabled_stack.push(true);
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

            let mut pending_outputs = Vec::new();
            for arg_path in &ordered_arg_paths {
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
                let rhs_reg = self.coerce_register_to_variable_type(
                    rhs_reg,
                    *arg_id,
                    expression_signed(&expr),
                    ir_builder,
                )?;
                let dsts = if let Some(state) = runtime_state.as_ref() {
                    dsts.iter()
                        .map(|dst| self.substitute_assignment_destination(dst, state))
                        .collect()
                } else {
                    dsts.clone()
                };
                let rhs_reg =
                    self.coerce_function_output_to_actual(rhs_reg, *arg_id, &dsts, ir_builder)?;
                pending_outputs.push((rhs_reg, dsts));
            }

            if let Some(state) = runtime_state.as_ref() {
                self.emit_nonlocal_function_state_writes(
                    state, targets, domain, convert, sources, ir_builder,
                )?;
            }
            for (rhs_reg, dsts) in pending_outputs {
                self.emit_multi_dst_assign(
                    rhs_reg, &dsts, targets, domain, convert, sources, ir_builder,
                )?;
            }

            Ok(())
        })();
        self.function_expression_value_stack.pop();
        self.function_arg_value_stack.pop();
        self.function_array_view_enabled_stack.pop();
        let finished_views = self.function_array_view_stack.pop().unwrap();
        self.function_arg_stack.pop();
        self.restore_active_function_array_views(&finished_views, convert, ir_builder)?;
        result
    }
}
